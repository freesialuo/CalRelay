import type { SourceConfiguration } from "./sources-api";
export function node<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  text?: string,
): HTMLElementTagNameMap[K] {
  const result = document.createElement(tag);
  if (text !== undefined) result.textContent = text;
  return result;
}
function field(title: string, input: HTMLElement): HTMLLabelElement {
  const label = node("label", title);
  label.append(input);
  return label;
}
function input(
  title: string,
  value: string,
  required = false,
): [HTMLLabelElement, HTMLInputElement] {
  const control = node("input");
  control.value = value;
  control.required = required;
  return [field(title, control), control];
}
function checkbox(
  title: string,
  checked: boolean,
): [HTMLLabelElement, HTMLInputElement] {
  const control = node("input");
  control.type = "checkbox";
  control.checked = checked;
  return [field(title, control), control];
}
export function sourceForm(configuration?: SourceConfiguration): {
  form: HTMLFormElement;
  read: () => SourceConfiguration;
} {
  const form = node("form");
  form.className = "source-form";
  const [nameLabel, name] = input("名称", configuration?.name ?? "", true);
  name.maxLength = 256;
  const mode = node("select");
  for (const [value, title] of [
    ["ics:pull", "ICS URL（定期读取）"],
    ["ics:manual-static", "ICS 静态 / 手动（此页面暂不提供文件上传）"],
    ["standard-calendar:push-only", "外部 producer 推送"],
    ["standard-calendar:manual-static", "标准日历手动来源"],
  ]) {
    const option = node("option", title);
    option.value = value;
    mode.append(option);
  }
  mode.value = configuration
    ? `${configuration.adapter}:${configuration.mode}`
    : "ics:pull";
  const [enabledLabel, enabled] = checkbox(
    "启用来源",
    configuration?.enabled ?? false,
  );
  enabled.disabled = !configuration;
  const [urlLabel, url] = input(
    "ICS HTTPS 地址（不含凭据、query 或 fragment）",
    configuration?.endpoint ?? "",
  );
  url.type = "url";
  const [credentialLabel, credential] = input(
    "已有凭据引用 ID（可选；不要输入密钥）",
    configuration?.credential_id ?? "",
  );
  credential.maxLength = 128;
  const [intervalLabel, interval] = input(
    "刷新间隔（秒）",
    String((configuration?.refresh_interval_ms ?? 60000) / 1000),
  );
  interval.type = "number";
  interval.min = "60";
  interval.max = "604800";
  interval.step = "1";
  const [slaLabel, sla] = input(
    "新鲜度 SLA（秒；推送来源可留空）",
    configuration?.freshness_sla_ms
      ? String(configuration.freshness_sla_ms / 1000)
      : configuration
        ? ""
        : "300",
  );
  sla.type = "number";
  sla.min = "60";
  sla.max = "2592000";
  sla.step = "1";
  const capabilities = node("fieldset");
  capabilities.append(node("legend", "来源声明的能力（不代表工作器已执行）"));
  const controls = new Map<
    keyof SourceConfiguration["capabilities"],
    HTMLInputElement
  >();
  for (const [key, title] of [
    ["snapshot", "完整快照"],
    ["delta", "增量更新"],
    ["upsert", "写入事件（必需）"],
    ["delete", "显式删除（需要增量）"],
    ["conditional_read", "条件读取"],
    ["pagination", "分页（本批模式未支持）"],
  ] as const) {
    const [label, control] = checkbox(
      title,
      configuration?.capabilities[key] ?? ["snapshot", "upsert"].includes(key),
    );
    controls.set(key, control);
    capabilities.append(label);
  }
  const calendars = node("fieldset");
  calendars.append(
    node("legend", "日历映射"),
    node(
      "p",
      "已保存的 ID 与 external_id 不可改写或移除；取消选择会保留历史事件。",
    ),
  );
  const rows: {
    element: HTMLElement;
    read: () => SourceConfiguration["calendars"][number];
  }[] = [];
  const addRow = (calendar?: SourceConfiguration["calendars"][number]) => {
    const row = node("div");
    row.className = "calendar-row";
    const [idLabel, id] = input(
      "日历 ID",
      calendar?.id ?? `cal_${crypto.randomUUID().replaceAll("-", "")}`,
      true,
    );
    id.pattern = "[A-Za-z0-9_-]{1,128}";
    id.maxLength = 128;
    id.readOnly = !!calendar;
    const [externalLabel, external] = input(
      "远端日历 external_id",
      calendar?.external_id ?? "",
      true,
    );
    external.maxLength = 256;
    external.readOnly = !!calendar;
    const [selectedLabel, selected] = checkbox(
      "选择此日历",
      calendar?.selected ?? true,
    );
    const [zoneLabel, zone] = input(
      "IANA 时区（可选）",
      calendar?.timezone ?? "",
    );
    zone.maxLength = 256;
    row.append(idLabel, externalLabel, selectedLabel, zoneLabel);
    const entry = {
      element: row,
      read: () => ({
        id: id.value,
        external_id: external.value,
        selected: selected.checked,
        timezone: zone.value || null,
      }),
    };
    rows.push(entry);
    if (!calendar) {
      const remove = node("button", "移除此未保存映射");
      remove.type = "button";
      remove.addEventListener("click", () => {
        if (rows.length > 1) {
          rows.splice(rows.indexOf(entry), 1);
          row.remove();
          form.dispatchEvent(new Event("input", { bubbles: true }));
        }
      });
      row.append(remove);
    }
    calendars.append(row);
  };
  for (const calendar of configuration?.calendars ?? [undefined])
    addRow(calendar);
  const add = node("button", "添加日历映射");
  add.type = "button";
  add.addEventListener("click", () => {
    if (rows.length < 256) {
      addRow();
      form.dispatchEvent(new Event("input", { bubbles: true }));
    }
  });
  const explanation = node("p");
  explanation.className = "notice";
  const modeChanged = (reset: boolean) => {
    const pull = mode.value === "ics:pull";
    const ics = mode.value.startsWith("ics:");
    const manual = mode.value.endsWith("manual-static");
    urlLabel.hidden = credentialLabel.hidden = intervalLabel.hidden = !pull;
    url.required = interval.required = pull;
    url.disabled = credential.disabled = interval.disabled = !pull;
    slaLabel.hidden = manual;
    sla.disabled = manual;
    sla.required = pull;
    explanation.textContent = pull
      ? "只有支持 pull 的来源使用刷新周期。保存 URL 不会立即抓取或证明来源可用。"
      : "静态 / 推送来源不轮询；接入器离线不等于核心服务故障。文件上传与 producer 执行需各自真实接入接口。";
    for (const [key, control] of controls) {
      control.disabled =
        key === "upsert" ||
        key === "pagination" ||
        (ics && ["delta", "delete", "snapshot"].includes(key)) ||
        (!pull && key === "conditional_read");
      if (reset) control.checked = key === "snapshot" || key === "upsert";
    }
  };
  mode.addEventListener("change", () => modeChanged(true));
  modeChanged(false);
  form.append(
    nameLabel,
    field("来源类型与更新方式", mode),
    enabledLabel,
    node("p", "新来源必须先以禁用状态创建；确认配置后再显式启用。"),
    explanation,
    urlLabel,
    credentialLabel,
    intervalLabel,
    slaLabel,
    capabilities,
    calendars,
    add,
  );
  return {
    form,
    read: () => {
      const [adapter, sourceMode] = mode.value.split(":") as [
        SourceConfiguration["adapter"],
        SourceConfiguration["mode"],
      ];
      const pull = sourceMode === "pull";
      const mapped = rows.map((row) => row.read());
      if (
        new Set(mapped.map((row) => row.id)).size !== mapped.length ||
        new Set(mapped.map((row) => row.external_id)).size !== mapped.length
      )
        throw new Error("日历 ID 或 external_id 重复。");
      return {
        version: "v1",
        name: name.value,
        adapter,
        mode: sourceMode,
        enabled: enabled.checked,
        endpoint: pull ? url.value : null,
        credential_id: pull ? credential.value || null : null,
        refresh_interval_ms: pull ? Number(interval.value) * 1000 : null,
        freshness_sla_ms:
          sourceMode === "manual-static" || !sla.value
            ? null
            : Number(sla.value) * 1000,
        capabilities: Object.fromEntries(
          [...controls].map(([key, control]) => [key, control.checked]),
        ) as SourceConfiguration["capabilities"],
        calendars: mapped,
      };
    },
  };
}
