import {
  AdminFailure,
  type AdminSession,
  boundedJSON,
  requireStatus,
} from "./admin-session";
import type { components } from "./generated/api";
export type SourceConfiguration =
  components["schemas"]["SourceConfigurationV1"];
export type SourceRecord = components["schemas"]["SourceConfigurationRecord"];
export type SourcePage = components["schemas"]["SourceConfigurationPage"];
export type SourceSummary = components["schemas"]["SourceConfigurationSummary"];
export const validId = (value: unknown): value is string =>
  typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
const object = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
const positive = (v: unknown): v is number =>
  Number.isSafeInteger(v) && (v as number) > 0;
const timestamp = (v: unknown) =>
  v === null || (Number.isSafeInteger(v) && (v as number) >= 0);
const text = (v: unknown, limit: number): v is string =>
  typeof v === "string" &&
  v.length > 0 &&
  new TextEncoder().encode(v).length <= limit;
const optionalText = (v: unknown, limit: number) => v == null || text(v, limit);
const optionalNumber = (v: unknown) => v == null || positive(v);
function summary(value: unknown): value is SourceSummary {
  return (
    object(value) &&
    validId(value.source_id) &&
    text(value.name, 256) &&
    typeof value.enabled === "boolean" &&
    positive(value.config_revision) &&
    positive(value.source_revision) &&
    timestamp(value.retired_at)
  );
}
function configuration(value: unknown): value is SourceConfiguration {
  if (
    !object(value) ||
    value.version !== "v1" ||
    !text(value.name, 256) ||
    typeof value.enabled !== "boolean" ||
    !["ics", "standard-calendar"].includes(String(value.adapter)) ||
    !["pull", "manual-static", "push-only"].includes(String(value.mode)) ||
    !optionalText(value.endpoint, 4096) ||
    !optionalText(value.credential_id, 128) ||
    !optionalNumber(value.refresh_interval_ms) ||
    !optionalNumber(value.freshness_sla_ms) ||
    !object(value.capabilities)
  )
    return false;
  if (
    ![
      "snapshot",
      "delta",
      "upsert",
      "delete",
      "conditional_read",
      "pagination",
    ].every(
      (key) =>
        typeof (value.capabilities as Record<string, unknown>)[key] ===
        "boolean",
    )
  )
    return false;
  return (
    Array.isArray(value.calendars) &&
    value.calendars.length >= 1 &&
    value.calendars.length <= 256 &&
    value.calendars.every(
      (calendar) =>
        object(calendar) &&
        validId(calendar.id) &&
        text(calendar.external_id, 256) &&
        typeof calendar.selected === "boolean" &&
        optionalText(calendar.timezone, 256),
    )
  );
}
function record(value: unknown): value is SourceRecord {
  return (
    object(value) &&
    validId(value.source_id) &&
    positive(value.config_revision) &&
    positive(value.source_revision) &&
    timestamp(value.retired_at) &&
    configuration(value.configuration)
  );
}
const unknown = () =>
  new AdminFailure(
    "unknown",
    "响应不符合来源契约。未确认保存，请重新读取核实。",
  );
const path = (id: string) => {
  if (!validId(id))
    throw new AdminFailure(
      "invalid",
      "来源 ID 只能使用1–128个字母、数字、下划线或连字符。",
    );
  return `/api/admin/v1/sources/${id}`;
};
export class SourcesAPI {
  constructor(public readonly session: AdminSession) {}
  async list(
    after: string | undefined,
    signal: AbortSignal,
  ): Promise<SourcePage> {
    if (after !== undefined && !validId(after))
      throw new AdminFailure("invalid", "来源分页游标无效。");
    const response = await this.session.read(
      `/api/admin/v1/sources${after ? `?after=${after}` : ""}`,
      signal,
    );
    await requireStatus(response, [200]);
    try {
      const data = await boundedJSON(response);
      if (
        !object(data) ||
        !Array.isArray(data.items) ||
        data.items.length > 100 ||
        !data.items.every(summary) ||
        !(data.next_after === null || validId(data.next_after)) ||
        data.next_after === after
      )
        throw unknown();
      return data as SourcePage;
    } catch {
      throw unknown();
    }
  }
  async read(id: string, signal: AbortSignal): Promise<SourceRecord> {
    const response = await this.session.read(path(id), signal);
    await requireStatus(response, [200]);
    try {
      const data = await boundedJSON(response);
      if (
        !record(data) ||
        data.source_id !== id ||
        response.headers.get("etag") !== `"${data.config_revision}"`
      )
        throw unknown();
      return data;
    } catch {
      throw unknown();
    }
  }
  async save(
    id: string,
    config: SourceConfiguration,
    revision: number | null,
    signal: AbortSignal,
  ): Promise<SourceRecord> {
    if (revision !== null && !positive(revision))
      throw new AdminFailure("invalid", "配置版本无效，请重新读取。");
    const body = JSON.stringify(config);
    if (
      !configuration(config) ||
      (revision === null && config.enabled) ||
      new TextEncoder().encode(body).length > 65536
    )
      throw new AdminFailure(
        "invalid",
        "来源配置无效或超出64 KiB；新来源必须先保存为禁用状态。",
      );
    const response = await this.session.write(
      path(id),
      "PUT",
      {
        "content-type": "application/json",
        ...(revision === null
          ? { "if-none-match": "*" }
          : { "if-match": `"${revision}"` }),
      },
      body,
      signal,
    );
    await requireStatus(response, [revision === null ? 201 : 200]);
    try {
      const data = await boundedJSON(response);
      if (
        !record(data) ||
        data.source_id !== id ||
        data.config_revision !== (revision === null ? 1 : revision + 1) ||
        data.retired_at !== null ||
        response.headers.get("etag") !== `"${data.config_revision}"`
      )
        throw unknown();
      return data;
    } catch {
      this.session.clear();
      throw unknown();
    }
  }
  async retire(
    id: string,
    revision: number,
    signal: AbortSignal,
  ): Promise<void> {
    if (!positive(revision))
      throw new AdminFailure("invalid", "配置版本无效，请重新读取。");
    const response = await this.session.write(
      path(id),
      "DELETE",
      { "if-match": `"${revision}"` },
      undefined,
      signal,
    );
    await requireStatus(response, [204]);
    await response.body?.cancel();
  }
}
