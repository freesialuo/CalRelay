import type { paths } from "./generated/api";

type GetPath =
  | "/api/v1/version"
  | "/health/live"
  | "/health/ready"
  | "/api/admin/v1/session";
type Version =
  paths["/api/v1/version"]["get"]["responses"][200]["content"]["application/json"];
export type ProbeResult = { message: string; requestId?: string; ok: boolean };

function isVersion(value: unknown): value is Version {
  if (typeof value !== "object" || value === null) return false;
  const data = value as Record<string, unknown>;
  return (
    data.name === "CalRelay" &&
    data.api_version === "v1" &&
    typeof data.version === "string" &&
    data.version.length <= 128
  );
}

export async function readProbe(
  path: GetPath,
  signal: AbortSignal,
): Promise<ProbeResult> {
  // Fixed same-origin read paths only. Browser sends existing HttpOnly cookies.
  const response = await fetch(path, {
    method: "GET",
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    signal: AbortSignal.any([signal, AbortSignal.timeout(8000)]),
    headers: { Accept: "application/json" },
  });
  const rawId = response.headers.get("x-request-id");
  const requestId =
    rawId && /^[a-zA-Z0-9_-]{1,128}$/.test(rawId) ? rawId : undefined;
  if (!response.ok) {
    await response.body?.cancel();
    const messages: Record<number, string> = {
      401: "管理身份未验证。请通过管理入口重新认证。",
      403: "管理请求被拒绝。请重新认证；会话、身份或安全策略可能已失效。写入还需 CSRF 与精确 Origin 校验。",
      404: "此接口未接通或未装配；不能据此判断配置为空。",
      409: "状态发生冲突。请重新读取当前状态，不要假定操作成功。",
      429: "请求过于频繁。请稍后手动重试。",
      503: "服务暂未就绪。此响应不说明所有接入器的状态。",
    };
    return {
      ok: false,
      message:
        messages[response.status] ??
        `请求失败（HTTP ${response.status}）。请核对诊断后手动重试。`,
      requestId,
    };
  }
  if (path === "/api/admin/v1/session") {
    await response.body?.cancel();
    if (response.status !== 204) throw new Error("Invalid session response");
    return {
      ok: true,
      message:
        "本次管理身份与会话检查通过；不代表已保存配置或取得长期写入授权。",
      requestId,
    };
  }
  if (response.status !== 200) {
    await response.body?.cancel();
    throw new Error("Unexpected response status");
  }
  if (!response.headers.get("content-type")?.includes("application/json"))
    throw new Error("Invalid content type");
  // Bound what we parse/render, even if an intermediary returns a bad response.
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Missing response");
  let size = 0;
  const chunks: Uint8Array[] = [];
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.length;
      if (size > 4096) throw new Error("Response too large");
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
  const data: unknown = JSON.parse(new TextDecoder().decode(bytes));
  if (path === "/api/v1/version") {
    if (!isVersion(data)) throw new Error("Invalid version response");
    return {
      ok: true,
      message: `CalRelay ${data.version} · API ${data.api_version}`,
      requestId,
    };
  }
  const expected = path === "/health/live" ? "live" : "ready";
  if (
    typeof data !== "object" ||
    data === null ||
    !("status" in data) ||
    data.status !== expected
  )
    throw new Error("Invalid probe response");
  return {
    ok: true,
    message: `HTTP 探针返回 ${expected}；实际工作器与来源 freshness 需要独立诊断。`,
    requestId,
  };
}
