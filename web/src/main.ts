import { adminSession } from "./admin-session";
import { readProbe } from "./api";
import { SourcesAPI } from "./sources-api";
import { mountSources } from "./sources-page";

let sourcesView: ReturnType<typeof mountSources> | undefined;
let renderedHash = location.hash;

import "./style.css";

function element<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  text?: string,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function panel(title: string, ...paragraphs: string[]): HTMLElement {
  const section = element("section", undefined, "panel");
  section.append(element("h2", title));
  for (const text of paragraphs) section.append(element("p", text));
  return section;
}
const pages = [
  ["sources", "来源"],
  ["capabilities", "能力与周期"],
  ["calendars", "日历与规则"],
  ["outputs", "输出与订阅"],
  ["diagnostics", "诊断"],
] as const;
const active = new Set<AbortController>();

function probe(
  title: string,
  path: Parameters<typeof readProbe>[0],
): HTMLElement {
  const section = panel(title);
  const status = element("p", "尚未检查", "result");
  status.setAttribute("role", "status");
  const run = element("button", "检查");
  const cancel = element("button", "取消检查", "secondary");
  run.type = cancel.type = "button";
  cancel.hidden = true;
  let current: AbortController | undefined;
  run.addEventListener("click", async () => {
    if (current) return;
    const controller = new AbortController();
    current = controller;
    active.add(controller);
    run.disabled = true;
    cancel.hidden = false;
    status.textContent = "检查中…";
    status.dataset.state = "pending";
    try {
      const result = await readProbe(path, controller.signal);
      if (controller.signal.aborted) return;
      status.textContent =
        result.message +
        (result.requestId ? ` · 请求 ID：${result.requestId}` : "");
      status.dataset.state = result.ok ? "success" : "error";
    } catch {
      if (controller.signal.aborted) return;
      status.textContent =
        "未取得可信结果：连接中断、超时、重定向或响应不符合契约。请手动重试；没有确认任何保存。";
      status.dataset.state = "error";
    } finally {
      active.delete(controller);
      current = undefined;
      run.disabled = false;
      cancel.hidden = true;
    }
  });
  cancel.addEventListener("click", () => {
    current?.abort();
    status.textContent = "检查已取消；未确认远端结果。";
    status.dataset.state = "cancelled";
  });
  section.append(status, run, cancel);
  return section;
}

function render(): void {
  if (
    sourcesView &&
    location.hash !== renderedHash &&
    !sourcesView.canLeave()
  ) {
    history.replaceState(null, "", renderedHash || "#sources");
    return;
  }
  sourcesView?.dispose();
  sourcesView = undefined;
  renderedHash = location.hash;
  for (const controller of active) controller.abort();
  active.clear();
  const root = document.querySelector<HTMLDivElement>("#app");
  if (!root) return;
  const route = location.hash.slice(1);
  const page = pages.find(([id]) => id === route)?.[0] ?? "sources";
  const shell = element("div", undefined, "shell");
  const header = element("header");
  header.append(
    element("span", "CALRELAY", "brand"),
    element("p", "日历与提醒中继 · 管理入口"),
  );
  const nav = element("nav");
  nav.setAttribute("aria-label", "管理旅程");
  for (const [id, title] of pages) {
    const link = element("a", title);
    link.href = `#${id}`;
    if (id === page) link.setAttribute("aria-current", "page");
    nav.append(link);
  }
  const main = element("main");
  const notice = element(
    "p",
    "来源配置已接入条件请求；写入需真实管理登录。规则、输出与订阅尚未接通。",
    "notice",
  );
  main.append(notice);
  if (page === "sources") {
    sourcesView = mountSources(main, new SourcesAPI(adminSession));
  } else if (page === "capabilities") {
    main.append(
      element("h1", "能力与周期"),
      panel(
        "来源能力与周期",
        "请在来源页面编辑声明的能力与周期。支持 pull 的来源才能配置自动刷新；保存配置不等于工作器已经运行。",
        "静态 / 手动来源不自动轮询",
        "push-only 来源仅在明确 SLA 时判断 freshness。接入器离线不等于核心服务故障。",
      ),
    );
  } else if (page === "calendars") {
    main.append(
      element("h1", "日历与规则"),
      panel(
        "日历选择与提醒规则尚未接通",
        "日历映射、规则及其 revision 必须来自真实 API。当前不能读取或保存，也不会用示例事件替代实例数据。",
        "写入发生 CAS 冲突时需要重新读取并核对；响应丢失不能当作保存成功，也不应自动重复提交。",
      ),
    );
  } else if (page === "outputs") {
    main.append(
      element("h1", "输出与订阅"),
      panel(
        "Bark 输出尚未接通",
        "Bark 接受请求（accepted）不等于设备已经展示通知。结果未知需要核对投递尝试与审计，不自动重发。",
      ),
      panel(
        "订阅配置尚未接通",
        "订阅是日历客户端的只读输出。feed token 不提供管理权限；订阅 URL 和一次性密钥需要真实权限与生命周期接口。",
      ),
    );
  } else {
    main.append(
      element("h1", "诊断"),
      element(
        "p",
        "以下仅执行同源只读检查，不会刷新来源、规划提醒或发送 Bark。",
        "intro",
      ),
      probe("服务版本", "/api/v1/version"),
      probe("HTTP 存活探针", "/health/live"),
      probe("HTTP 就绪探针", "/health/ready"),
      probe("管理会话", "/api/admin/v1/session"),
      panel(
        "来源、规划、投递与审计",
        "业务诊断 API 尚未接通。HTTP 成功不证明工作器已装配；来源陈旧、外部失败和核心健康需要分别读取。",
        "页面返回或刷新会取消当前检查并清除检查结果。管理会话依赖安全 Cookie 与服务端身份校验，前端不保存 token。",
      ),
    );
  }
  const footer = element(
    "footer",
    "配置与运行诊断以真实服务为准 · 尚未完成产品全旅程验收",
  );
  shell.append(header, nav, main, footer);
  root.replaceChildren(shell);
}
window.addEventListener("hashchange", render);
window.addEventListener("pagehide", () => {
  for (const controller of active) controller.abort();
  sourcesView?.dispose();
  adminSession.clear();
});
render();
window.addEventListener("pageshow", (event) => {
  if (event.persisted) render();
});
