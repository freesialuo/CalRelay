import {
  AdminFailure,
  type AdminSession,
  boundedJSON,
  requireStatus,
} from "./admin-session";
import type { paths } from "./generated/api";
import { node } from "./source-form";
import { mountChallenge } from "./turnstile";

type Bootstrap =
  paths["/api/admin/v1/bootstrap"]["get"]["responses"][200]["content"]["application/json"];
function bootstrap(value: unknown): value is Bootstrap {
  if (typeof value !== "object" || value === null || !("enabled" in value))
    return false;
  if (value.enabled === false) return Object.keys(value).length === 1;
  if (
    value.enabled !== true ||
    !("turnstile" in value) ||
    typeof value.turnstile !== "object" ||
    value.turnstile === null ||
    Object.keys(value).length !== 2
  )
    return false;
  const data = value.turnstile as Record<string, unknown>;
  return (
    Object.keys(data).length === 3 &&
    typeof data.site_key === "string" &&
    /^[A-Za-z0-9_-]{1,256}$/.test(data.site_key) &&
    typeof data.action === "string" &&
    /^[A-Za-z0-9_-]{1,32}$/.test(data.action) &&
    typeof data.hostname === "string" &&
    data.hostname.length >= 1 &&
    data.hostname.length <= 253
  );
}
export function mountAdminLogin(
  root: HTMLElement,
  session: AdminSession,
): () => void {
  const panel = node("section");
  panel.className = "panel login";
  panel.setAttribute("aria-label", "管理登录");
  const status = node("p", "正在读取管理登录配置…");
  status.setAttribute("role", "status");
  const read = node("button", "重新读取登录配置");
  read.type = "button";
  const start = node("button", "验证管理身份");
  start.type = "button";
  start.hidden = true;
  const cancel = node("button", "取消验证");
  cancel.type = "button";
  cancel.hidden = true;
  const logout = node("button", "退出管理会话");
  logout.type = "button";
  logout.hidden = true;
  const challenge = node("div");
  panel.append(
    node("h2", "管理登录"),
    status,
    read,
    start,
    cancel,
    logout,
    challenge,
  );
  root.append(panel);
  let disposed = false;
  let metadata: Bootstrap | undefined;
  let epoch = 0;
  let current: AbortController | undefined;
  let remove: (() => void) | undefined;
  let verifying = false;
  let previouslyWritable = session.writable;
  const update = () => {
    if (disposed) return;
    const writable = session.writable;
    logout.hidden = !writable && !session.busy;
    logout.disabled = session.busy || verifying;
    start.hidden = !metadata?.enabled || writable;
    start.disabled = session.busy || verifying;
    read.disabled = session.busy || verifying;
    if (writable)
      status.textContent =
        "本次管理登录已完成。写入授权仅在内存，刷新后需重新验证。";
    else if (previouslyWritable && !session.busy)
      status.textContent = "管理写入授权已失效，请重新验证身份。";
    if (!session.busy) previouslyWritable = writable;
  };
  const stop = () => {
    epoch++;
    current?.abort();
    current = undefined;
    remove?.();
    remove = undefined;
    challenge.replaceChildren();
    verifying = false;
    cancel.hidden = true;
  };
  const message = (text: string) => {
    if (!disposed) status.textContent = text;
  };
  const load = async () => {
    stop();
    const id = epoch;
    const controller = new AbortController();
    current = controller;
    read.disabled = true;
    start.hidden = true;
    message("正在读取管理登录配置…");
    try {
      const response = await session.read(
        "/api/admin/v1/bootstrap",
        controller.signal,
      );
      await requireStatus(response, [200]);
      const data = await boundedJSON(response, 4096);
      if (!bootstrap(data)) throw new Error("Invalid bootstrap");
      if (disposed || id !== epoch || controller.signal.aborted) return;
      metadata = data;
      if (!data.enabled)
        message(
          "管理登录未启用。请由部署者完成管理认证装配；此页面不能授予权限。",
        );
      else if (location.protocol !== "https:") {
        metadata = undefined;
        message("管理登录需要 HTTPS，请通过配置的安全入口访问。");
      } else if (data.turnstile.hostname !== location.hostname) {
        metadata = undefined;
        message("挑战配置的 hostname 与当前页面不匹配，请检查部署配置。");
      } else
        message(
          "请验证管理身份以取得短期写入授权。来源与 feed token 均不是管理身份。",
        );
    } catch {
      if (!disposed && !controller.signal.aborted) {
        metadata = undefined;
        message(
          "未取得可信登录配置，可能未装配、暂不可用或响应不符合契约。请手动重试。",
        );
      }
    } finally {
      if (id === epoch) {
        current = undefined;
        read.disabled = false;
        update();
      }
    }
  };
  start.addEventListener("click", () => {
    if (!metadata?.enabled || session.busy || verifying) return;
    stop();
    session.clear();
    const id = epoch;
    const controller = new AbortController();
    current = controller;
    verifying = true;
    cancel.hidden = false;
    message("请完成人机验证。验证结果只提交到当前同源管理接口。");
    update();
    const failed = (text: string) => {
      if (disposed || id !== epoch) return;
      stop();
      session.clear();
      message(text);
      update();
    };
    remove = mountChallenge(
      challenge,
      metadata.turnstile.site_key,
      metadata.turnstile.action,
      {
        complete: (token) => {
          if (disposed || id !== epoch || controller.signal.aborted) return;
          message("正在验证管理身份与会话…");
          void session
            .establish(token, controller.signal)
            .then(() => {
              if (!disposed && id === epoch && !controller.signal.aborted) {
                stop();
                update();
              }
            })
            .catch((error) => {
              if (!disposed && id === epoch && !controller.signal.aborted) {
                stop();
                message(
                  error instanceof AdminFailure
                    ? error.message
                    : "登录未确认，请重新验证。",
                );
                update();
              }
            });
        },
        expired: () => failed("人机验证已过期或超时，请重新验证。"),
        failed: () => failed("人机验证未完成或连接失败，请重新验证。"),
      },
    );
  });
  cancel.addEventListener("click", () => {
    stop();
    session.clear();
    message("验证已取消，未确认会话结果；当前内存写入授权已清除。");
    update();
  });
  logout.addEventListener("click", () => {
    if (session.busy || verifying) return;
    stop();
    const id = epoch;
    const controller = new AbortController();
    current = controller;
    void session
      .write(
        "/api/admin/v1/session",
        "DELETE",
        {},
        undefined,
        controller.signal,
      )
      .then(async (response) => {
        await requireStatus(response, [204]);
        await response.body?.cancel();
        if (!disposed && id === epoch) message("服务端已确认退出管理会话。");
      })
      .catch(() => {
        if (!disposed && id === epoch)
          message("服务端未确认退出；客户端写入授权已清除，请重新验证。");
      })
      .finally(() => {
        previouslyWritable = false;
        session.clear();
        if (!disposed && id === epoch) {
          current = undefined;
          update();
        }
      });
  });
  read.addEventListener("click", () => {
    void load();
  });
  session.addEventListener("change", update);
  void load();
  return () => {
    disposed = true;
    stop();
    session.removeEventListener("change", update);
    panel.remove();
  };
}
