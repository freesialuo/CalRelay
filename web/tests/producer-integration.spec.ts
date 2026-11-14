import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import { login, source, status } from "./support/journey";

const database = () =>
  JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../.test-admin-state.json", import.meta.url)),
      "utf8",
    ),
  ).database;

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

test("producer requests that must fail are rejected without side effects or admin power", async ({
  page,
  playwright,
}) => {
  await login(page);
  // The source under test is created last, so its detail stays open.
  const other = await source(page, "standard-calendar:push-only");
  const s = await source(page, "standard-calendar:push-only");
  await page.getByRole("button", { name: "签发令牌", exact: true }).click();
  const secret = await page
    .getByLabel("新令牌密钥（只显示这一次）")
    .inputValue();
  const base = `/api/ingest/standard-calendar/v1/sources/${s.id}`;
  const bearer = (token: string) => ({ authorization: `Bearer ${token}` });
  const before = await (
    await page.request.get(`${base}/state`, { headers: bearer(secret) })
  ).json();
  const envelope = (overrides: Record<string, unknown> = {}) => ({
    protocol_version: "standard-calendar-source/v1",
    batch_id: "negative",
    mode: "snapshot",
    base_source_revision: before.source_revision,
    base_config_revision: before.config_revision,
    base_checkpoint: null,
    proposed_checkpoint: null,
    complete: true,
    pages: 1,
    collection_tag: null,
    operations: [
      {
        op: "upsert",
        calendar_id: s.calendar,
        event: {
          schema_version: "1.0",
          external_uid: "negative-event",
          title: "x",
          status: "confirmed",
          start: { kind: "utc", value: "2030-01-01T09:00:00Z" },
        },
      },
    ],
    ...overrides,
  });
  const send = (path: string, token: string, key: string, data: unknown) =>
    page.request.post(path, {
      headers: { ...bearer(token), "idempotency-key": key },
      data: typeof data === "string" ? data : JSON.stringify(data),
    });
  const batches = `${base}/batches`;
  const cases: [string, string, string, unknown, number][] = [
    ["forged token", "f".repeat(64), "n1", envelope(), 401],
    ["another source's path", secret, "n2", envelope(), 401],
    [
      "calendar of another source",
      secret,
      "n3",
      envelope({
        operations: [
          {
            op: "upsert",
            calendar_id: other.calendar,
            event: {
              schema_version: "1.0",
              external_uid: "x",
              title: "x",
              status: "confirmed",
              start: { kind: "utc", value: "2030-01-01T09:00:00Z" },
            },
          },
        ],
      }),
      422,
    ],
    ["unknown field", secret, "n4", envelope({ source_id: s.id }), 422],
    ["too many pages", secret, "n5", envelope({ pages: 33 }), 422],
    [
      "duplicate JSON key",
      secret,
      "n6",
      JSON.stringify(envelope()).replace('"pages":1', '"pages":1,"pages":1'),
      422,
    ],
  ];
  for (const [name, token, key, data, expected] of cases) {
    const path =
      name === "another source's path"
        ? `/api/ingest/standard-calendar/v1/sources/${other.id}/batches`
        : batches;
    expect((await send(path, token, key, data)).status(), name).toBe(expected);
  }
  // Replaying a key with another envelope is a conflict, never a second write.
  expect((await send(batches, secret, "replay", envelope())).status()).toBe(
    200,
  );
  const committed = await (
    await page.request.get(`${base}/state`, { headers: bearer(secret) })
  ).json();
  // The changed envelope is valid at the current revision, so only the reused key rejects it.
  const changed = envelope({
    base_source_revision: committed.source_revision,
    proposed_checkpoint: "changed",
  });
  expect((await send(batches, secret, "replay", changed)).status()).toBe(409);
  expect((await send(batches, secret, "fresh", changed)).status()).toBe(200);

  // A producer token grants no management power, with or without a browser Origin.
  const anonymous = await playwright.request.newContext({
    baseURL: "https://127.0.0.1:4175",
    ignoreHTTPSErrors: true,
  });
  for (const path of [
    "/api/admin/v1/sources",
    `/api/admin/v1/sources/${s.id}/tokens`,
  ]) {
    const denied = await anonymous.get(path, { headers: bearer(secret) });
    expect(denied.status(), path).toBe(403);
  }
  const preflight = await anonymous.fetch(batches, {
    method: "OPTIONS",
    headers: {
      origin: "https://evil.example",
      "access-control-request-method": "POST",
      "access-control-request-headers": "authorization,idempotency-key",
    },
  });
  expect(preflight.headers()["access-control-allow-origin"]).toBeUndefined();
  await anonymous.dispose();

  // An expired token is refused at once.
  execFileSync("python3", [
    "-c",
    "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('UPDATE api_tokens SET expires_at=1 WHERE source_id=?',(sys.argv[2],)); c.commit()",
    database(),
    s.id,
  ]);
  expect((await send(batches, secret, "expired", envelope())).status()).toBe(
    401,
  );
  const after = await (
    await page.request.get(`/api/admin/v1/sources/${s.id}/diagnostics`)
  ).json();
  // Only the two accepted batches changed the source.
  expect(committed.source_revision).toBe(before.source_revision + 1);
  expect(after.snapshot_revision).toBe(committed.source_revision + 1);
});
