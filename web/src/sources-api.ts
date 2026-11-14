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
export type SnapshotReceipt = components["schemas"]["SourceSnapshotReceipt"];
export type ProducerToken = components["schemas"]["ProducerToken"];
export type ProducerTokenPage = components["schemas"]["ProducerTokenPage"];
export type ProducerTokenGrant = components["schemas"]["ProducerTokenGrant"];
export const MAX_ICS_BYTES = 1_048_576;
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
function producerToken(value: unknown): value is ProducerToken {
  return (
    object(value) &&
    Object.keys(value).length === 6 &&
    validId(value.token_id) &&
    typeof value.can_upsert === "boolean" &&
    typeof value.can_delete === "boolean" &&
    (value.can_upsert || value.can_delete) &&
    timestamp(value.expires_at) &&
    timestamp(value.revoked_at) &&
    timestamp(value.issued_at)
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
  /** Records a refresh request only; the receipt does not mean a sync ran or succeeded. */
  async refresh(id: string, signal: AbortSignal): Promise<void> {
    const response = await this.session.write(
      `${path(id)}/refresh`,
      "POST",
      {},
      undefined,
      signal,
    );
    if (response.status === 409) {
      await response.body?.cancel();
      throw new AdminFailure(
        "conflict",
        "来源当前不是已启用的可拉取来源，未记录刷新请求。请重新读取来源核实。",
      );
    }
    await requireStatus(response, [202]);
    try {
      const data = await boundedJSON(response);
      if (
        !object(data) ||
        Object.keys(data).length !== 2 ||
        data.source_id !== id ||
        !Number.isSafeInteger(data.requested_at)
      )
        throw unknown();
    } catch {
      this.session.clear();
      throw new AdminFailure(
        "unknown",
        "响应不符合刷新回执契约。未确认请求已记录，请稍后在诊断中核实。",
      );
    }
  }
  /** Replaces the whole snapshot of a manual-static ICS source; all or nothing. */
  async uploadSnapshot(
    id: string,
    revision: number,
    ics: string,
    signal: AbortSignal,
    approveDeletions = false,
  ): Promise<SnapshotReceipt | "approval-required"> {
    if (!positive(revision))
      throw new AdminFailure("invalid", "配置版本无效，请重新读取。");
    if (new TextEncoder().encode(ics).length > MAX_ICS_BYTES)
      throw new AdminFailure("invalid", "ICS 文件超过 1 MiB，未上传。");
    const response = await this.session.write(
      `${path(id)}/snapshot${approveDeletions ? "?approve_deletions=true" : ""}`,
      "PUT",
      {
        "content-type": "text/calendar; charset=utf-8",
        "if-match": `"${revision}"`,
      },
      ics,
      signal,
    );
    if (response.status === 409) {
      await response.body?.cancel();
      throw new AdminFailure(
        "conflict",
        "来源不是已启用的 ICS 静态来源，或配置已变化；既有事件未改变。请重新读取来源核实。",
      );
    }
    if (response.status === 428 && !approveDeletions) {
      await response.body?.cancel();
      return "approval-required";
    }
    if ([413, 415, 422].includes(response.status)) {
      await response.body?.cancel();
      throw new AdminFailure("invalid", "ICS 无效或超出预算；既有事件未改变。");
    }
    await requireStatus(response, [200]);
    try {
      const data = await boundedJSON(response);
      if (
        !object(data) ||
        Object.keys(data).length !== 3 ||
        data.source_id !== id ||
        !positive(data.source_revision) ||
        !Number.isSafeInteger(data.changed_events) ||
        (data.changed_events as number) < 0
      )
        throw unknown();
      return data as SnapshotReceipt;
    } catch {
      this.session.clear();
      throw new AdminFailure(
        "unknown",
        "响应不符合快照回执契约。未确认是否已替换，请在诊断中核实，勿重复上传。",
      );
    }
  }
  async tokens(id: string, signal: AbortSignal): Promise<ProducerTokenPage> {
    const response = await this.session.read(`${path(id)}/tokens`, signal);
    await requireStatus(response, [200]);
    try {
      const data = await boundedJSON(response);
      if (
        !object(data) ||
        Object.keys(data).length !== 2 ||
        !Array.isArray(data.items) ||
        data.items.length > 100 ||
        !data.items.every(producerToken) ||
        typeof data.more !== "boolean"
      )
        throw unknown();
      return data as ProducerTokenPage;
    } catch {
      throw new AdminFailure("unknown", "令牌列表响应不符合契约，请重新读取。");
    }
  }
  /** Issues a producer token; the secret in the grant is shown once and never stored. */
  async issueToken(
    id: string,
    request: {
      can_upsert: boolean;
      can_delete: boolean;
      expires_at: number | null;
    },
    signal: AbortSignal,
  ): Promise<ProducerTokenGrant> {
    const response = await this.session.write(
      `${path(id)}/tokens`,
      "POST",
      { "content-type": "application/json" },
      JSON.stringify(request),
      signal,
    );
    if (response.status === 409) {
      await response.body?.cancel();
      throw new AdminFailure(
        "invalid",
        "未签发：来源不能持有生产者令牌，或有效令牌已达 16 个上限。请撤销不用的令牌或重新读取来源核实。",
      );
    }
    if (response.status === 422) {
      await response.body?.cancel();
      throw new AdminFailure(
        "invalid",
        "未签发：至少选择一项权限，有效期须晚于当前时间。",
      );
    }
    await requireStatus(response, [201]);
    try {
      const data = await boundedJSON(response);
      if (
        !object(data) ||
        Object.keys(data).length !== 2 ||
        !producerToken(data.token) ||
        data.token.revoked_at !== null ||
        typeof data.secret !== "string" ||
        !/^[0-9a-f]{64}$/.test(data.secret)
      )
        throw unknown();
      return data as ProducerTokenGrant;
    } catch {
      this.session.clear();
      throw new AdminFailure(
        "unknown",
        "响应不符合令牌签发契约。未确认是否已签发，请重新读取令牌列表核实，必要时撤销未知令牌。",
      );
    }
  }
  async revokeToken(
    id: string,
    tokenId: string,
    signal: AbortSignal,
  ): Promise<void> {
    if (!validId(tokenId)) throw new AdminFailure("invalid", "令牌 ID 无效。");
    const response = await this.session.write(
      `${path(id)}/tokens/${tokenId}/revoke`,
      "POST",
      {},
      undefined,
      signal,
    );
    if (response.status === 404) {
      await response.body?.cancel();
      throw new AdminFailure(
        "invalid",
        "令牌不存在或不属于此来源；请重新读取令牌列表。",
      );
    }
    await requireStatus(response, [204]);
    await response.body?.cancel();
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
