import {
  AdminFailure,
  type AdminSession,
  boundedJSON,
  requireStatus,
} from "./admin-session";
import type { components } from "./generated/api";
import { validId } from "./sources-api";

type S = components["schemas"];
export type OutputConfig = S["BarkOutputConfigurationV1"];
export type RuleConfig = S["RuleConfigurationV1"];
export type SubscriptionConfig = S["AggregateConfigurationV1"];
export type Configurations = {
  outputs: OutputConfig;
  rules: RuleConfig;
  subscriptions: SubscriptionConfig;
};
export type Records = {
  outputs: S["BarkOutputConfigurationRecord"];
  rules: S["RuleConfigurationRecord"];
  subscriptions: S["AggregateRecord"];
};
export type Kind = keyof Configurations;
export type Summary = S["ReminderConfigurationSummary"] | S["AggregateSummary"];
export const labels = {
  outputs: "Bark 输出",
  rules: "提醒规则",
  subscriptions: "汇总订阅",
} as const;
export const object = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
const integer = (
  v: unknown,
  min: number,
  max = Number.MAX_SAFE_INTEGER,
): v is number =>
  Number.isSafeInteger(v) && (v as number) >= min && (v as number) <= max;
const text = (v: unknown, max = 256): v is string =>
  typeof v === "string" &&
  v.trim().length > 0 &&
  new TextEncoder().encode(v).length <= max &&
  ![...v].some((c) => c.charCodeAt(0) < 32 || c.charCodeAt(0) === 127);
const stamp = (v: unknown) => v === null || integer(v, 0);
const ids = (v: unknown, min: number, max: number): v is string[] =>
  Array.isArray(v) &&
  v.length >= min &&
  v.length <= max &&
  v.every(validId) &&
  new Set(v).size === v.length;
export const visibleFields: SubscriptionConfig["visible_fields"] = [
  "summary",
  "description",
  "location",
  "url",
  "categories",
];
export function validConfiguration<K extends Kind>(
  kind: K,
  c: unknown,
): c is Configurations[K] {
  if (!object(c) || !text(c.name) || typeof c.enabled !== "boolean")
    return false;
  if (kind === "outputs") {
    if (
      c.version !== "v1" ||
      c.kind !== "bark" ||
      !validId(c.credential_id) ||
      !integer(c.request_timeout_ms, 1, 120000) ||
      !text(c.origin, 8192)
    )
      return false;
    try {
      const url = new URL(c.origin);
      return (
        url.protocol === "https:" &&
        !url.username &&
        !url.password &&
        !url.search &&
        !url.hash &&
        ["/", "/push"].includes(url.pathname)
      );
    } catch {
      return false;
    }
  }
  if (kind === "rules")
    return (
      c.version === "v1" &&
      ids(c.output_ids, 1, 32) &&
      object(c.policy) &&
      ids(c.policy.calendar_ids, 1, 256) &&
      integer(c.policy.lead_ms, 0, 604800000) &&
      typeof c.policy.all_day_time === "string" &&
      /^(?:[01][0-9]|2[0-3]):[0-5][0-9](?::[0-5][0-9])?$/.test(
        c.policy.all_day_time,
      )
    );
  return (
    c.version === 1 &&
    stamp(c.expires_at) &&
    c.stale_policy === "reject" &&
    Array.isArray(c.sources) &&
    c.sources.length <= 32 &&
    c.sources.every(
      (s) => object(s) && validId(s.source_id) && ids(s.calendar_ids, 0, 256),
    ) &&
    new Set(c.sources.map((s) => (s as Record<string, unknown>).source_id))
      .size === c.sources.length &&
    c.sources.reduce(
      (n, s) => n + (s as { calendar_ids: string[] }).calendar_ids.length,
      0,
    ) <= 256 &&
    Array.isArray(c.visible_fields) &&
    c.visible_fields.every((f) =>
      visibleFields.includes(f as SubscriptionConfig["visible_fields"][number]),
    ) &&
    new Set(c.visible_fields).size === c.visible_fields.length
  );
}
export function recordId(record: Records[Kind]): string {
  return "output_id" in record
    ? record.output_id
    : "rule_id" in record
      ? record.rule_id
      : record.subscription_id;
}
export function revision(record: Records[Kind]): number {
  return "generation" in record ? record.generation : record.revision;
}
export function retired(record: Records[Kind]): number | null {
  return "retired_at" in record ? record.retired_at : record.revoked_at;
}
export const summaryId = (s: Summary) => ("id" in s ? s.id : s.subscription_id);
export const summaryRevision = (s: Summary) =>
  "generation" in s ? s.generation : s.revision;
export const summaryRetired = (s: Summary) =>
  "retired_at" in s ? s.retired_at : s.revoked_at;
const unknown = () =>
  new AdminFailure(
    "unknown",
    "响应不符合配置契约，结果未知；请重新读取核实。一次性链接不能从 GET 恢复。",
  );
function validRecord<K extends Kind>(kind: K, v: unknown): v is Records[K] {
  if (!object(v) || !validConfiguration(kind, v.configuration)) return false;
  if (kind === "subscriptions")
    return (
      validId(v.subscription_id) &&
      validId(v.public_id) &&
      integer(v.revision, 1) &&
      stamp(v.revoked_at)
    );
  return (
    validId(v[kind === "outputs" ? "output_id" : "rule_id"]) &&
    integer(v.generation, 1) &&
    stamp(v.retired_at)
  );
}
function path(kind: Kind, id?: string): string {
  if (id !== undefined && !validId(id))
    throw new AdminFailure(
      "invalid",
      "稳定 ID 需为1–128个字母、数字、下划线或连字符。",
    );
  return `/api/admin/v1/${kind}${id === undefined ? "" : `/${id}`}`;
}
export class ConfigurationAPI<K extends Kind> {
  constructor(
    public readonly kind: K,
    public readonly session: AdminSession,
  ) {}
  async list(
    after: string | undefined,
    signal: AbortSignal,
  ): Promise<S["ReminderConfigurationPage"] | S["AggregatePage"]> {
    if (after !== undefined && !validId(after))
      throw new AdminFailure("invalid", "分页游标无效。");
    const r = await this.session.read(
      `${path(this.kind)}${after ? `?after=${after}` : ""}`,
      signal,
    );
    await requireStatus(r, [200]);
    const data = await boundedJSON(r);
    if (
      !object(data) ||
      !Array.isArray(data.items) ||
      data.items.length > 100 ||
      !(data.next_after === null || validId(data.next_after)) ||
      data.next_after === after ||
      !data.items.every(
        (s) =>
          object(s) &&
          text(s.name) &&
          typeof s.enabled === "boolean" &&
          (this.kind === "subscriptions"
            ? validId(s.subscription_id) &&
              validId(s.public_id) &&
              integer(s.revision, 1) &&
              stamp(s.revoked_at) &&
              stamp(s.expires_at)
            : validId(s.id) && integer(s.generation, 1) && stamp(s.retired_at)),
      )
    )
      throw unknown();
    return data as S["ReminderConfigurationPage"] | S["AggregatePage"];
  }
  private async receipt(
    r: Response,
    id: string,
    expected?: number,
  ): Promise<Records[K]> {
    const data = await boundedJSON(r);
    if (
      !validRecord(this.kind, data) ||
      recordId(data) !== id ||
      r.headers.get("etag") !== `"${revision(data)}"` ||
      (expected !== undefined && revision(data) !== expected) ||
      "capability_url" in data
    )
      throw unknown();
    return data;
  }
  async read(id: string, signal: AbortSignal): Promise<Records[K]> {
    const r = await this.session.read(path(this.kind, id), signal);
    await requireStatus(r, [200]);
    return this.receipt(r, id);
  }
  async save(
    id: string,
    config: Configurations[K],
    version: number | null,
    signal: AbortSignal,
  ): Promise<{ record: Records[K]; capability?: string }> {
    if (
      !validConfiguration(this.kind, config) ||
      (version === null ? config.enabled : !integer(version, 1))
    )
      throw new AdminFailure(
        "invalid",
        "请核对配置、引用与预算；创建必须先禁用。",
      );
    const body = JSON.stringify(config);
    if (new TextEncoder().encode(body).length > 65536)
      throw new AdminFailure("invalid", "配置超过64 KiB。");
    const r = await this.session.write(
      path(this.kind, id),
      "PUT",
      {
        "content-type": "application/json",
        ...(version === null
          ? { "if-none-match": "*" }
          : { "if-match": `"${version}"` }),
      },
      body,
      signal,
    );
    await requireStatus(r, [version === null ? 201 : 200]);
    try {
      if (this.kind === "subscriptions" && version === null)
        return await this.grant(r, id, 1);
      const record = await this.receipt(
        r,
        id,
        version === null ? 1 : version + 1,
      );
      if (retired(record) !== null) throw unknown();
      return { record };
    } catch {
      this.session.clear();
      throw unknown();
    }
  }
  private async grant(
    r: Response,
    id: string,
    expected: number,
  ): Promise<{ record: Records[K]; capability: string }> {
    const data = await boundedJSON(r);
    if (
      !object(data) ||
      !validRecord("subscriptions", data.subscription) ||
      data.subscription.subscription_id !== id ||
      data.subscription.revision !== expected ||
      data.subscription.revoked_at !== null ||
      r.headers.get("etag") !== `"${expected}"` ||
      typeof data.capability_url !== "string"
    )
      throw unknown();
    const prefix = `/feeds/aggregates/${data.subscription.public_id}/`;
    if (
      !data.capability_url.startsWith(prefix) ||
      !/^[A-Za-z0-9_-]{16,256}\.ics$/.test(
        data.capability_url.slice(prefix.length),
      )
    )
      throw unknown();
    return {
      record: data.subscription as Records[K],
      capability: new URL(data.capability_url, location.origin).href,
    };
  }
  async rotate(
    id: string,
    version: number,
    signal: AbortSignal,
  ): Promise<{ record: Records[K]; capability: string }> {
    if (this.kind !== "subscriptions" || !integer(version, 1))
      throw new AdminFailure("invalid", "轮换条件无效。");
    const r = await this.session.write(
      `${path(this.kind, id)}/token`,
      "POST",
      { "if-match": `"${version}"` },
      undefined,
      signal,
    );
    await requireStatus(r, [200]);
    try {
      return await this.grant(r, id, version + 1);
    } catch {
      this.session.clear();
      throw unknown();
    }
  }
  async retire(
    id: string,
    version: number,
    signal: AbortSignal,
  ): Promise<void> {
    if (!integer(version, 1))
      throw new AdminFailure("invalid", "版本无效，请重新读取。");
    const r = await this.session.write(
      path(this.kind, id),
      "DELETE",
      { "if-match": `"${version}"` },
      undefined,
      signal,
    );
    await requireStatus(r, [204]);
    await r.body?.cancel();
  }
}
