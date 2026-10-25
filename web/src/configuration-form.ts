import { AdminFailure } from "./admin-session";
import {
  ConfigurationAPI,
  type Configurations,
  type Kind,
  type OutputConfig,
  type RuleConfig,
  type SubscriptionConfig,
  summaryId,
  summaryRetired,
  visibleFields,
} from "./configuration-api";
import { node } from "./source-form";
import type { SourceRecord, SourcesAPI } from "./sources-api";

function input(
  root: HTMLElement,
  label: string,
  value: string,
  type = "text",
): HTMLInputElement {
  const wrap = node("label", label);
  const control = node("input");
  control.type = type;
  control.value = value;
  wrap.append(control);
  root.append(wrap);
  return control;
}
function check(
  root: HTMLElement,
  label: string,
  value: boolean,
): HTMLInputElement {
  const c = input(root, label, "", "checkbox");
  c.checked = value;
  return c;
}
function numeric(
  root: HTMLElement,
  label: string,
  value: number,
  min: number,
  max: number,
  step = "1",
) {
  const c = input(root, label, String(value), "number");
  c.min = String(min);
  c.max = String(max);
  c.step = step;
  c.required = true;
  return c;
}
const fieldsLabels: Record<
  SubscriptionConfig["visible_fields"][number],
  string
> = {
  summary: "标题",
  description: "描述",
  location: "地点",
  url: "事件 URL",
  categories: "分类",
};
export function configurationForm<K extends Kind>(
  root: HTMLFieldSetElement,
  kind: K,
  config: Configurations[K] | undefined,
  sources: SourcesAPI,
  signal: AbortSignal,
): () => Configurations[K] {
  const name = input(root, "名称", config?.name ?? "");
  name.required = true;
  name.maxLength = 256;
  const enabled = check(root, "启用配置", config?.enabled ?? false);
  enabled.disabled = !config;
  root.append(
    node(
      "p",
      config
        ? "保存仅确认配置；运行与外部结果需另查诊断。"
        : "新配置必须先以禁用状态保存，再显式启用。",
    ),
  );
  if (kind === "outputs") {
    const c = config as OutputConfig | undefined;
    root.append(
      node(
        "p",
        "首次使用：请部署管理员先运行安全 CLI credential provision-bark，从受控 secret-file 创建凭据；将返回的 credential_id 填在下方。主密钥与 Bark secret 不交给浏览器，不需要手改数据库。详见仓库 docs/credential-bootstrap.md。",
      ),
    );
    const origin = input(
      root,
      "Bark HTTPS origin（可含 /push）",
      c?.origin ?? "https://api.day.app",
      "url",
    );
    origin.required = true;
    origin.maxLength = 8192;
    const credential = input(
      root,
      "已有 Bark credential_id",
      c?.credential_id ?? "",
    );
    credential.required = true;
    credential.pattern = "[A-Za-z0-9_-]{1,128}";
    const timeout = numeric(
      root,
      "请求超时（毫秒）",
      c?.request_timeout_ms ?? 10000,
      1,
      120000,
    );
    root.append(
      node(
        "p",
        "这里只引用既有 kind=bark 凭据句柄。保存不发送通知；Bark accepted 不等于设备已显示，结果未知不自动重发。",
      ),
    );
    return () =>
      ({
        version: "v1",
        kind: "bark",
        name: name.value,
        enabled: enabled.checked,
        origin: origin.value,
        credential_id: credential.value,
        request_timeout_ms: timeout.valueAsNumber,
      }) as Configurations[K];
  }
  const r = config as RuleConfig | undefined;
  const sub = config as SubscriptionConfig | undefined;
  const selectedCalendars = new Set(
    kind === "rules" ? r?.policy.calendar_ids : [],
  );
  const members = new Map<string, Set<string>>(
    kind === "subscriptions"
      ? sub?.sources.map((s) => [s.source_id, new Set(s.calendar_ids)])
      : [],
  );
  const picked = node("fieldset");
  picked.append(
    node("legend", kind === "rules" ? "选定提醒日历" : "选定来源与日历"),
  );
  root.append(picked);
  let syncCalendarOptions = () => {};
  const renderPicked = () => {
    picked.replaceChildren(
      node("legend", kind === "rules" ? "选定提醒日历" : "选定来源与日历"),
    );
    if (kind === "rules") {
      if (!selectedCalendars.size)
        picked.append(node("p", "尚未选择日历；规则至少选择一个日历。"));
      for (const id of selectedCalendars) {
        const c = check(picked, `日历 ${id}`, true);
        c.onchange = () => {
          if (c.checked) selectedCalendars.add(id);
          else selectedCalendars.delete(id);
          syncCalendarOptions();
        };
      }
    } else {
      if (!members.size)
        picked.append(node("p", "空选择仅产生空日历，不授权全部来源。"));
      for (const [sourceId, calendars] of members) {
        const group = node("fieldset");
        group.append(node("legend", `来源 ${sourceId}`));
        const include = check(group, `包含来源 ${sourceId}`, true);
        include.onchange = () => {
          if (!include.checked) {
            members.delete(sourceId);
            renderPicked();
            syncCalendarOptions();
          }
        };
        for (const id of calendars) {
          const c = check(group, `日历 ${id}`, true);
          c.onchange = () => {
            if (c.checked) calendars.add(id);
            else calendars.delete(id);
            syncCalendarOptions();
          };
        }
        picked.append(group);
      }
    }
  };
  renderPicked();
  const chooser = node("fieldset");
  chooser.append(node("legend", "从真实来源读取日历选项"));
  root.append(chooser);
  const referenceStatus = node(
    "p",
    "点击读取来源；已有选中 ID 保留，不因选项读取失败而丢失。",
  );
  referenceStatus.setAttribute("role", "status");
  chooser.append(referenceStatus);
  const sourceSelect = node("select");
  const sourceLabel = node("label", "选择来源");
  sourceLabel.append(sourceSelect);
  chooser.append(sourceLabel);
  const readSources = node("button", "读取来源选项");
  readSources.type = "button";
  const readCalendars = node("button", "读取此来源日历");
  readCalendars.type = "button";
  readCalendars.disabled = true;
  const options = node("div");
  const calendarOptions = new Map<string, HTMLInputElement>();
  let memberOption: HTMLInputElement | undefined;
  let optionSource = "";
  syncCalendarOptions = () => {
    if (memberOption) memberOption.checked = members.has(optionSource);
    for (const [id, c] of calendarOptions)
      c.checked =
        kind === "rules"
          ? selectedCalendars.has(id)
          : (members.get(optionSource)?.has(id) ?? false);
  };
  chooser.append(readSources, readCalendars, options);
  let after: string | undefined;
  let sourceBusy = false;
  let detailGeneration = 0;
  const fail = (e: unknown) => {
    if (!signal.aborted)
      referenceStatus.textContent =
        e instanceof AdminFailure
          ? e.message
          : "选项读取失败；没有改变已选配置。";
  };
  readSources.onclick = async () => {
    if (sourceBusy || signal.aborted) return;
    sourceBusy = true;
    readSources.disabled = true;
    try {
      if (!after) {
        sourceSelect.replaceChildren();
        options.replaceChildren();
        detailGeneration++;
      }
      const page = await sources.list(after, signal);
      if (signal.aborted) return;
      for (const s of page.items) {
        if (s.retired_at !== null) continue;
        const option = node(
          "option",
          `${s.name} · ${s.source_id}${s.enabled ? "" : "（禁用）"}`,
        );
        option.value = s.source_id;
        sourceSelect.append(option);
      }
      after = page.next_after ?? undefined;
      readSources.textContent = after ? "读取更多来源" : "重新读取来源选项";
      referenceStatus.textContent = sourceSelect.options.length
        ? "可读取所选来源的当前日历；禁用来源的配置可引用，实际运行须启用。"
        : "此页无可用来源；先在来源页面创建，或继续读取更多。";
      readCalendars.disabled = !sourceSelect.options.length;
    } catch (e) {
      fail(e);
    } finally {
      sourceBusy = false;
      if (!signal.aborted) readSources.disabled = false;
    }
  };
  sourceSelect.onchange = () => {
    detailGeneration++;
    options.replaceChildren();
    readCalendars.disabled = !sourceSelect.value;
  };
  readCalendars.onclick = async () => {
    if (signal.aborted || !sourceSelect.value) return;
    const generation = ++detailGeneration;
    const id = sourceSelect.value;
    calendarOptions.clear();
    memberOption = undefined;
    optionSource = id;
    readCalendars.disabled = true;
    options.replaceChildren();
    try {
      const source: SourceRecord = await sources.read(id, signal);
      if (signal.aborted || generation !== detailGeneration) return;
      if (source.retired_at !== null)
        throw new AdminFailure("invalid", "该来源已退休，请选择其他来源。");
      options.append(
        node(
          "p",
          `来源配置版本 ${source.config_revision}；仅显示当前 selected 日历。后端保存时再次校验引用。`,
        ),
      );
      if (kind === "subscriptions") {
        const member = check(
          options,
          `选择来源 ${id}（可为空日历）`,
          members.has(id),
        );
        memberOption = member;
        member.onchange = () => {
          if (member.checked) {
            if (!members.has(id)) members.set(id, new Set());
          } else members.delete(id);
          renderPicked();
          syncCalendarOptions();
          root.dispatchEvent(new Event("input", { bubbles: true }));
        };
      }
      for (const calendar of source.configuration.calendars.filter(
        (c) => c.selected,
      )) {
        const c = check(
          options,
          `选择日历 ${calendar.external_id} · ${calendar.id}`,
          kind === "rules"
            ? selectedCalendars.has(calendar.id)
            : (members.get(id)?.has(calendar.id) ?? false),
        );
        calendarOptions.set(calendar.id, c);
        c.onchange = () => {
          if (kind === "rules") {
            if (c.checked) selectedCalendars.add(calendar.id);
            else selectedCalendars.delete(calendar.id);
          } else {
            if (c.checked) {
              if (!members.has(id)) members.set(id, new Set());
              members.get(id)?.add(calendar.id);
            } else members.get(id)?.delete(calendar.id);
          }
          renderPicked();
          syncCalendarOptions();
          root.dispatchEvent(new Event("input", { bubbles: true }));
        };
      }
    } catch (e) {
      fail(e);
    } finally {
      if (!signal.aborted && generation === detailGeneration)
        readCalendars.disabled = false;
    }
  };
  if (kind === "rules") {
    const lead = numeric(
      root,
      "提前提醒（秒）",
      (r?.policy.lead_ms ?? 900000) / 1000,
      0,
      604800,
      "0.001",
    );
    const allDay = input(
      root,
      "全天提醒时间（HH:MM[:SS]）",
      r?.policy.all_day_time ?? "09:00",
    );
    allDay.required = true;
    allDay.pattern = "(?:[01][0-9]|2[0-3]):[0-5][0-9](?::[0-5][0-9])?";
    const outputIds = new Set(r?.output_ids ?? []);
    const outputBox = node("fieldset");
    outputBox.append(node("legend", "提醒输出"));
    root.append(outputBox);
    const outputOptions = node("div");
    const outputInputs = new Map<string, HTMLInputElement>();
    const selected = node("div");
    const draw = () => {
      selected.replaceChildren();
      for (const id of outputIds) {
        const c = check(selected, `输出 ${id}`, true);
        c.onchange = () => {
          if (c.checked) outputIds.add(id);
          else outputIds.delete(id);
          const option = outputInputs.get(id);
          if (option) option.checked = outputIds.has(id);
        };
      }
    };
    draw();
    outputBox.append(selected);
    const readOutputs = node("button", "读取输出选项");
    readOutputs.type = "button";
    const outputStatus = node(
      "p",
      "至少选择一个未退休输出；可引用禁用输出，运行时须启用。",
    );
    outputStatus.setAttribute("role", "status");
    outputBox.append(readOutputs, outputStatus, outputOptions);
    let outputAfter: string | undefined;
    let outputBusy = false;
    const seen = new Set<string>();
    readOutputs.onclick = async () => {
      if (outputBusy) return;
      outputBusy = true;
      readOutputs.disabled = true;
      try {
        const page = await new ConfigurationAPI(
          "outputs",
          sources.session,
        ).list(outputAfter, signal);
        if (signal.aborted) return;
        for (const s of page.items) {
          const id = summaryId(s);
          if (summaryRetired(s) !== null || seen.has(id)) continue;
          seen.add(id);
          const c = check(
            outputOptions,
            `${s.name} · ${id}${s.enabled ? "" : "（禁用）"}`,
            outputIds.has(id),
          );
          outputInputs.set(id, c);
          c.onchange = () => {
            if (c.checked) outputIds.add(id);
            else outputIds.delete(id);
            draw();
            root.dispatchEvent(new Event("input", { bubbles: true }));
          };
        }
        outputAfter = page.next_after ?? undefined;
        readOutputs.textContent = outputAfter
          ? "读取更多输出"
          : "已读取输出选项";
        outputStatus.textContent = seen.size
          ? "选择输出后保存；没有执行投递。"
          : "尚无可用输出，请先创建 Bark 输出。";
        readOutputs.disabled = !outputAfter;
      } catch (e) {
        outputStatus.textContent =
          e instanceof AdminFailure ? e.message : "读取输出失败。";
        readOutputs.disabled = false;
      } finally {
        outputBusy = false;
      }
    };
    return () =>
      ({
        version: "v1",
        name: name.value,
        enabled: enabled.checked,
        policy: {
          calendar_ids: [...selectedCalendars],
          lead_ms: Math.round(lead.valueAsNumber * 1000),
          all_day_time: allDay.value,
        },
        output_ids: [...outputIds],
      }) as Configurations[K];
  }
  const projection = node("fieldset");
  projection.append(node("legend", "公开字段"));
  root.append(projection);
  const fieldInputs = visibleFields.map(
    (f) =>
      [
        f,
        check(
          projection,
          fieldsLabels[f],
          sub?.visible_fields.includes(f) ?? f === "summary",
        ),
      ] as const,
  );
  const expiry = input(
    root,
    "到期时间（UTC，可空；ISO 8601 含 Z）",
    sub?.expires_at === null || sub?.expires_at === undefined
      ? ""
      : new Date(sub.expires_at).toISOString(),
  );
  expiry.placeholder = "2030-01-01T00:00:00Z";
  root.append(
    node(
      "p",
      "陈旧策略：reject。任何所选来源不可用/失权时整份订阅拒绝；静态与 push-only 不套假轮询。空字段投影仍公开协议必要的 UID、时间与状态，不等于隐藏事件。到期后链接不可用；GET 不返回密钥，修改配置保留原链接，显式轮换立即使旧链接失效。日历客户端何时刷新由客户端决定，不承诺强制更新；不同来源同 UID 不合并。",
    ),
  );
  return () => {
    const time = expiry.value ? Date.parse(expiry.value) : null;
    if (
      expiry.value &&
      (!/^\d{4}-\d\d-\d\dT\d\d:\d\d(?::\d\d(?:\.\d{1,3})?)?Z$/.test(
        expiry.value,
      ) ||
        !Number.isSafeInteger(time))
    )
      throw new AdminFailure(
        "invalid",
        "到期时间须为有效 UTC ISO 8601 时间（含 Z），或留空。",
      );
    return {
      version: 1,
      name: name.value,
      enabled: enabled.checked,
      sources: [...members].map(([source_id, calendar_ids]) => ({
        source_id,
        calendar_ids: [...calendar_ids],
      })),
      visible_fields: fieldInputs.filter(([, c]) => c.checked).map(([f]) => f),
      stale_policy: "reject",
      expires_at: time,
    } as Configurations[K];
  };
}
