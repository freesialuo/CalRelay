import { mountAdminLogin } from "./admin-login";
import { adminSession } from "./admin-session";
import { readProbe } from "./api";
import { ConfigurationAPI } from "./configuration-api";
import { mountConfiguration } from "./configuration-page";
import { mountDiagnostics } from "./diagnostics-page";
import { SourcesAPI } from "./sources-api";
import { mountSources } from "./sources-page";

let disposeLogin: (() => void) | undefined;
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
  ["subscriptions", "汇总订阅"],
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
  disposeLogin?.();
  disposeLogin = undefined;
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
    "按来源 → Bark 输出 → 提醒规则 → 汇总订阅配置。首次 Bark 凭据请先由部署管理员通过安全 CLI 初始化。",
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
  } else if (
    page === "calendars" ||
    page === "outputs" ||
    page === "subscriptions"
  ) {
    const kind = page === "calendars" ? "rules" : page;
    sourcesView = mountConfiguration(
      main,
      new ConfigurationAPI(kind, adminSession),
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
    );
    sourcesView = mountDiagnostics(main, adminSession);
  }
  const footer = element(
    "footer",
    "配置与运行诊断以真实服务为准 · 尚未完成产品全旅程验收",
  );
  shell.append(header, nav);
  mountLoginContainer(shell);
  shell.append(main, footer);
  root.replaceChildren(shell);
}
window.addEventListener("hashchange", render);
window.addEventListener("pagehide", () => {
  for (const controller of active) controller.abort();
  sourcesView?.dispose();
  sourcesView = undefined;
  disposeLogin?.();
  disposeLogin = undefined;
  adminSession.clear();
});
render();
window.addEventListener("pageshow", (event) => {
  if (event.persisted) render();
});

function mountLoginContainer(shell: HTMLElement): void {
  disposeLogin = mountAdminLogin(shell, adminSession);
}
