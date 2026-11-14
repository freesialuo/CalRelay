import { expect, test } from "@playwright/test";
import { login, source, status } from "./support/journey";

test("a producer token issued in the browser writes a snapshot until it is revoked", async ({
  page,
}) => {
  await login(page);
  const s = await source(page, "standard-calendar:push-only");
  // The saved, enabled push-only source shows its producer tokens.
  await expect(page.getByRole("heading", { name: "生产者令牌" })).toBeVisible();
  await page.getByRole("button", { name: "签发令牌", exact: true }).click();
  await expect(status(page)).toContainText("密钥只显示这一次");
  const secret = await page
    .getByLabel("新令牌密钥（只显示这一次）")
    .inputValue();
  expect(secret).toMatch(/^[0-9a-f]{64}$/);
  const list = page.getByLabel("生产者令牌列表");
  await expect(list).toContainText("upsert + delete");
  await expect(list).not.toContainText(secret);

  const base = `/api/ingest/standard-calendar/v1/sources/${s.id}`;
  const bearer = { authorization: `Bearer ${secret}` };
  const state = await page.request.get(`${base}/state`, { headers: bearer });
  expect(state.status()).toBe(200);
  const before = await state.json();
  expect(before.has_snapshot).toBe(false);
  const envelope = {
    protocol_version: "standard-calendar-source/v1",
    batch_id: "browser-1",
    mode: "snapshot",
    base_source_revision: before.source_revision,
    base_config_revision: before.config_revision,
    base_checkpoint: null,
    proposed_checkpoint: "cp-1",
    complete: true,
    pages: 1,
    collection_tag: null,
    operations: [
      {
        op: "upsert",
        calendar_id: s.calendar,
        event: {
          schema_version: "1.0",
          external_uid: "producer-event",
          title: "生产者事件",
          status: "confirmed",
          start: { kind: "utc", value: "2030-01-01T09:00:00Z" },
        },
      },
    ],
  };
  const send = (key: string) =>
    page.request.post(`${base}/batches`, {
      headers: { ...bearer, "idempotency-key": key },
      data: envelope,
    });
  const committed = await send("browser-key-1");
  expect(committed.status()).toBe(200);
  expect(await committed.json()).toEqual({
    source_id: s.id,
    batch_id: "browser-1",
    source_revision: before.source_revision + 1,
    changed_events: 1,
  });
  const diagnostic = await (
    await page.request.get(`/api/admin/v1/sources/${s.id}/diagnostics`)
  ).json();
  expect(diagnostic.snapshot_revision).toBe(before.source_revision + 1);
  expect(JSON.stringify(diagnostic)).not.toContain(secret);

  // Revoking in the browser rejects the same secret at once.
  await page.getByRole("button", { name: "读取令牌", exact: true }).click();
  await expect(page.getByLabel("新令牌密钥（只显示这一次）")).toHaveCount(0);
  const revoke = list.getByRole("button", { name: /^撤销 pt_/ });
  await revoke.click();
  await list.getByRole("button", { name: /^确认撤销 pt_/ }).click();
  await expect(status(page)).toContainText("服务端已撤销令牌");
  await expect(list).toContainText("已撤销");
  expect((await send("browser-key-2")).status()).toBe(401);
});
