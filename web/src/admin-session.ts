import type { paths } from "./generated/api";

type SessionGrant =
  paths["/api/admin/v1/session"]["post"]["responses"][200]["content"]["application/json"];
export type FailureKind =
  | "denied"
  | "conflict"
  | "invalid"
  | "missing"
  | "unknown"
  | "busy";
export class AdminFailure extends Error {
  constructor(
    public readonly kind: FailureKind,
    message: string,
    public readonly requestId?: string,
  ) {
    super(message);
  }
}
const token = (value: unknown): value is string =>
  typeof value === "string" && /^[A-Za-z0-9_-]{1,256}$/.test(value);

export async function boundedJSON(
  response: Response,
  maximum = 131072,
): Promise<unknown> {
  if (!response.headers.get("content-type")?.includes("application/json"))
    throw new Error("Invalid JSON response");
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Missing response");
  let size = 0;
  const chunks: Uint8Array[] = [];
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > maximum) throw new Error("Response budget exceeded");
      chunks.push(value);
    }
  } finally {
    await reader.cancel();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return JSON.parse(new TextDecoder().decode(bytes));
}
export class AdminSession extends EventTarget {
  #grant?: SessionGrant;
  #busy = false;
  #generation = 0;
  #deadline?: ReturnType<typeof setTimeout>;
  #store(grant: SessionGrant): void {
    clearTimeout(this.#deadline);
    this.#grant = grant;
    this.#deadline = setTimeout(
      () => this.clear(),
      Math.max(0, grant.expires_at * 1000 - Date.now()),
    );
    if (typeof this.#deadline === "object") this.#deadline.unref();
  }
  constructor(
    private readonly transport: typeof fetch = globalThis.fetch.bind(
      globalThis,
    ),
  ) {
    super();
  }
  get writable(): boolean {
    return (
      !this.#busy && !!this.#grant && Date.now() < this.#grant.expires_at * 1000
    );
  }
  get busy(): boolean {
    return this.#busy;
  }
  clear(): void {
    clearTimeout(this.#deadline);
    this.#deadline = undefined;
    this.#grant = undefined;
    this.#generation++;
    this.dispatchEvent(new Event("change"));
  }

  // Called only by the trusted login UI's completed Turnstile challenge.
  async establish(challenge: string, signal: AbortSignal): Promise<void> {
    if (this.#busy) throw new AdminFailure("busy", "另一项管理请求仍在进行。");
    this.clear();
    if (!challenge || challenge.length > 2048)
      throw new AdminFailure("invalid", "请重新完成人机验证。");
    this.#busy = true;
    this.dispatchEvent(new Event("change"));
    const generation = this.#generation;
    try {
      const response = await this.request(
        "/api/admin/v1/session",
        "POST",
        { "content-type": "application/json" },
        JSON.stringify({ turnstile_token: challenge }),
        signal,
        false,
      );
      await requireStatus(response, [200]);
      const data = (await boundedJSON(response, 4096)) as Partial<SessionGrant>;
      if (
        generation !== this.#generation ||
        signal.aborted ||
        !token(data.csrf_token) ||
        !token(data.write_nonce) ||
        !Number.isSafeInteger(data.expires_at) ||
        (data.expires_at ?? 0) * 1000 <= Date.now()
      )
        throw new Error("Invalid session grant");
      this.#store(data as SessionGrant);
    } catch (error) {
      this.clear();
      if (error instanceof AdminFailure) throw error;
      throw new AdminFailure("unknown", "未取得可信会话，请重新登录。");
    } finally {
      this.#busy = false;
      this.dispatchEvent(new Event("change"));
    }
  }

  async read(path: string, signal: AbortSignal): Promise<Response> {
    return this.request(path, "GET", {}, undefined, signal, false);
  }
  async write(
    path: string,
    method: "PUT" | "DELETE" | "POST",
    headers: Record<string, string>,
    body: string | undefined,
    signal: AbortSignal,
  ): Promise<Response> {
    if (this.#busy)
      throw new AdminFailure("busy", "另一项管理写入仍在进行，请等待结果。");
    if (!this.writable || !this.#grant) {
      this.clear();
      throw new AdminFailure("denied", "写入授权缺失或过期，请重新登录。");
    }
    const authorization = this.#grant;
    this.#busy = true;
    this.dispatchEvent(new Event("change"));
    // Burn locally before awaiting I/O. Never reuse a nonce after an uncertain outcome.
    this.#grant = undefined;
    const generation = this.#generation;
    try {
      const response = await this.request(
        path,
        method,
        {
          ...headers,
          "x-csrf-token": authorization.csrf_token,
          "x-admin-write-nonce": authorization.write_nonce,
        },
        body,
        signal,
        true,
      );
      const next = response.headers.get("x-admin-write-nonce");
      if (
        token(next) &&
        next !== authorization.write_nonce &&
        generation === this.#generation
      )
        this.#store({ ...authorization, write_nonce: next });
      return response;
    } finally {
      this.#busy = false;
      this.dispatchEvent(new Event("change"));
    }
  }
  private async request(
    path: string,
    method: string,
    headers: Record<string, string>,
    body: string | undefined,
    signal: AbortSignal,
    unsafe: boolean,
  ): Promise<Response> {
    if (
      !/^\/api\/admin\/v1\/(?:bootstrap|session|(?:(?:sources|rules|outputs|subscriptions)(?:\?[A-Za-z0-9_=%-]+|\/[A-Za-z0-9_-]{1,128})?|subscriptions\/[A-Za-z0-9_-]{1,128}\/token|sources\/[A-Za-z0-9_-]{1,128}\/diagnostics))$/.test(
        path,
      )
    )
      throw new AdminFailure("invalid", "无效的管理路径。");
    const generation = this.#generation;
    let response: Response;
    try {
      response = await this.transport(path, {
        method,
        headers: { Accept: "application/json", ...headers },
        body,
        signal: AbortSignal.any([signal, AbortSignal.timeout(10000)]),
        credentials: "same-origin",
        cache: "no-store",
        redirect: "error",
      });
    } catch {
      if (unsafe && generation === this.#generation) this.clear();
      throw new AdminFailure(
        "unknown",
        unsafe
          ? "写入结果未知。请重新登录并读取当前来源核实，勿重复提交。"
          : "未取得可信结果，请手动重新读取。",
      );
    }
    if (!response.ok && generation === this.#generation) {
      // Preserve rotation even on business rejection; the guard consumed the nonce.
      if (unsafe && this.#grant === undefined) {
        const next = response.headers.get("x-admin-write-nonce");
        // write() handles this response's rotation before reporting its status.
        if (!token(next)) this.clear();
      }
      if ([401, 403].includes(response.status)) this.clear();
    }
    return response;
  }
}

export async function requireStatus(
  response: Response,
  expected: number[],
): Promise<void> {
  if (expected.includes(response.status)) return;
  const rawId = response.headers.get("x-request-id");
  const requestId =
    rawId && /^[A-Za-z0-9_-]{1,128}$/.test(rawId) ? rawId : undefined;
  await response.body?.cancel();
  const status = response.status;
  if (status === 401 || status === 403)
    throw new AdminFailure(
      "denied",
      "管理请求被拒绝。请重新登录并检查会话、CSRF、Origin 与权限。",
      requestId,
    );
  if (status === 409)
    throw new AdminFailure(
      "conflict",
      "配置版本冲突或资源已退休。输入已保留；请重新读取并比较，勿覆盖新版本。",
      requestId,
    );
  if (status === 404)
    throw new AdminFailure(
      "missing",
      "资源不存在、尚未托管或接口未装配；没有确认保存。",
      requestId,
    );
  if (status === 400 || status === 413 || status === 422)
    throw new AdminFailure(
      "invalid",
      "配置、条件或请求大小不符合服务契约，请核对输入。",
      requestId,
    );
  throw new AdminFailure(
    "unknown",
    "服务未确认可信结果。请重新读取核实后再决定下一步。",
    requestId,
  );
}

export const adminSession = new AdminSession();
