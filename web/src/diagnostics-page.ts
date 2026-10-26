import {
  AdminFailure,
  type AdminSession,
  boundedJSON,
  requireStatus,
} from "./admin-session";
import type { components } from "./generated/api";
import { node } from "./source-form";
import { SourcesAPI, validId } from "./sources-api";

type Diagnostic = components["schemas"]["SourceDiagnostic"];
const freshness: Record<Diagnostic["freshness"], string> = {
  not_applicable: "不按轮询时效判定（静态来源或未声明 SLA 的推送源）",
  never_verified: "尚无成功校验记录",
  future_timestamp: "校验时间位于当前业务时间之后，不能判为新鲜",
  fresh: "符合当前配置的时效要求",
  stale: "已超过配置的时效要求；请检查来源可达性、凭据与工作器状态",
};
const states = {
  pending: "等待到期 / 处理",
  leased: "处理中；不能据此确认上游已接收",
  accepted: "上游已接收",
  retry_wait: "等待重试",
  failed_permanent: "永久失败",
  expired: "已过期",
  superseded: "已被新版本替代",
  cancelled: "已取消",
};
const outcomes = {
  accepted: "上游已接收",
  permanent_failure: "永久失败",
  retryable: "可重试失败",
  unknown: "结果未知（可能已经发送）",
  not_sent: "未发送",
};
const object = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
const time = (v: unknown) =>
  typeof v === "number" &&
  Number.isSafeInteger(v) &&
  Math.abs(v) <= 8640000000000000;
const nullableTime = (v: unknown) => v === null || time(v);
const member = (v: unknown, choices: object) =>
  typeof v === "string" && Object.hasOwn(choices, v);
function valid(value: unknown, id: string): value is Diagnostic {
  return (
    object(value) &&
    value.source_id === id &&
    time(value.observed_at) &&
    ["pull", "manual-static", "push-only"].includes(String(value.mode)) &&
    typeof value.enabled === "boolean" &&
    nullableTime(value.retired_at) &&
    nullableTime(value.last_verified_at) &&
    (value.snapshot_revision === null ||
      (Number.isSafeInteger(value.snapshot_revision) &&
        Number(value.snapshot_revision) >= 0)) &&
    member(value.freshness, freshness) &&
    typeof value.more_deliveries === "boolean" &&
    Array.isArray(value.deliveries) &&
    value.deliveries.length <= 20 &&
    value.deliveries.every(
      (d: unknown) =>
        object(d) &&
        validId(d.job_id) &&
        validId(d.rule_id) &&
        validId(d.output_id) &&
        member(d.state, states) &&
        (d.last_outcome === null || member(d.last_outcome, outcomes)) &&
        time(d.due_at) &&
        time(d.next_attempt_at) &&
        [d.accepted_at, d.started_at, d.sent_at, d.finished_at].every(
          nullableTime,
        ),
    )
  );
}
const date = (value: number | null) =>
  value === null ? "无记录" : new Date(value).toISOString();
export function mountDiagnostics(
  root: HTMLElement,
  session: AdminSession,
): { dispose: () => void; canLeave: () => boolean } {
  const sources = new SourcesAPI(session);
  const section = node("section");
  section.className = "panel";
  section.append(
    node("h2", "来源同步与提醒投递"),
    node(
      "p",
      "仅读取持久记录，不刷新来源、不重试任务、不发送通知。外部来源离线不等于核心故障；Bark accepted 只表示上游接收，不证明设备显示。",
    ),
  );
  const status = node("p", "请选择来源读取诊断。");
  status.setAttribute("role", "status");
  const label = node("label", "诊断来源");
  label.className = "source-form";
  const select = node("select");
  label.append(select);
  const refresh = node("button", "读取来源选项");
  const next = node("button", "下一页来源");
  next.hidden = true;
  const read = node("button", "读取来源诊断");
  read.disabled = true;
  const cancel = node("button", "取消诊断读取");
  cancel.hidden = true;
  const result = node("div");
  result.setAttribute("aria-label", "持久诊断结果");
  for (const b of [refresh, next, read, cancel]) b.type = "button";
  section.append(status, refresh, next, label, read, cancel, result);
  root.append(section);
  let current: AbortController | undefined;
  let after: string | undefined;
  let disposed = false;
  const controls = () => {
    refresh.disabled = next.disabled = select.disabled = !!current;
    read.disabled = !!current || !select.value;
    cancel.hidden = !current;
  };
  async function run(work: (signal: AbortSignal) => Promise<void>) {
    if (current || disposed) return;
    const controller = new AbortController();
    current = controller;
    result.replaceChildren();
    status.textContent = "正在读取诊断…";
    controls();
    try {
      await work(controller.signal);
    } catch (error) {
      if (!controller.signal.aborted && !disposed)
        status.textContent =
          error instanceof AdminFailure
            ? error.message
            : "未取得可信诊断；请手动重新读取。";
    } finally {
      if (current === controller) current = undefined;
      if (!disposed) controls();
    }
  }
  async function list(cursor?: string) {
    await run(async (signal) => {
      const page = await sources.list(cursor, signal);
      if (signal.aborted || disposed) return;
      select.replaceChildren(node("option", "请选择来源"));
      select.options[0].value = "";
      for (const s of page.items) {
        const option = node("option", `${s.name} · ${s.source_id}`);
        option.value = s.source_id;
        select.append(option);
      }
      after = page.next_after ?? undefined;
      next.hidden = !after;
      status.textContent = page.items.length
        ? "请选择来源读取诊断；选项最多100条，可继续下一页。"
        : "本页没有已配置来源；空实例请先到来源页面创建。";
    });
  }
  refresh.onclick = () => {
    void list();
  };
  next.onclick = () => {
    void list(after);
  };
  select.onchange = () => {
    result.replaceChildren();
    status.textContent = "来源已切换，请重新读取诊断。";
    controls();
  };
  cancel.onclick = () => {
    current?.abort();
    status.textContent = "诊断读取已取消；没有确认远端结果。";
  };
  read.onclick = () => {
    const id = select.value;
    if (!validId(id)) return;
    void run(async (signal) => {
      const response = await session.read(
        `/api/admin/v1/sources/${id}/diagnostics`,
        signal,
      );
      await requireStatus(response, [200]);
      const value = await boundedJSON(response, 32768);
      if (!valid(value, id)) throw new Error("Invalid diagnostic response");
      if (signal.aborted || disposed) return;
      status.textContent = `诊断快照时间：${date(value.observed_at)}（不会自动刷新）`;
      result.append(
        node(
          "p",
          `来源状态：${value.retired_at !== null ? "已退休" : value.enabled ? "已启用" : "已禁用"} · 模式：${value.mode}`,
        ),
        node(
          "p",
          `最近成功同步 / 条件校验：${date(value.last_verified_at)} · 快照版本：${value.snapshot_revision ?? "无快照"}`,
        ),
        node("p", `来源时效：${freshness[value.freshness]}`),
        node(
          "p",
          "最近同步错误原因：未持久化，当前不可查询。成功校验时间不证明最近一次尝试成功；无投递记录也不证明规则已完成规划。",
        ),
        node("h3", "最近创建的投递任务（最多20条）"),
      );
      if (!value.deliveries.length)
        result.append(
          node(
            "p",
            "尚无投递任务记录。请核对来源同步、所选日历、启用规则与输出，以及事件到期时间。",
          ),
        );
      for (const d of value.deliveries) {
        const item = node("section");
        item.className = "panel";
        item.append(
          node(
            "p",
            `任务 ${d.job_id} · 规则 ${d.rule_id} · 输出 ${d.output_id}`,
          ),
          node(
            "p",
            `当前任务状态：${states[d.state]} · 最近尝试：${d.last_outcome === null ? (d.sent_at === null ? "无已完成尝试" : "已有发送意图，结果尚未确认") : outcomes[d.last_outcome]}`,
          ),
          node(
            "p",
            `到期：${date(d.due_at)} · ${d.state === "retry_wait" ? "下次重试" : "调度时间记录"}：${date(d.next_attempt_at)}`,
          ),
          node(
            "p",
            `尝试开始：${date(d.started_at)} · 完成：${date(d.finished_at)} · 逻辑提醒接收：${date(d.accepted_at)}`,
          ),
        );
        result.append(item);
      }
      if (value.more_deliveries)
        result.append(node("p", "还有更早任务，本页不是完整审计记录。"));
    });
  };
  const authorization = () => {
    if (!session.writable) {
      current?.abort();
      result.replaceChildren();
      status.textContent =
        "会话授权已变化；请重新登录并读取诊断，旧结果已清除。";
    }
  };
  session.addEventListener("change", authorization);
  void list();
  return {
    canLeave: () => true,
    dispose: () => {
      disposed = true;
      current?.abort();
      result.replaceChildren();
      session.removeEventListener("change", authorization);
    },
  };
}
