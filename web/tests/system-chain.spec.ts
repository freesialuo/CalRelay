import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import {
  dismissSecret,
  login,
  output,
  saveSubscription,
  source,
  status,
  subscriptionDraft,
} from "./support/journey";
import { logicalEndpoints } from "./system/fixtures.mjs";

function state(): {
  database: string;
  fixture_status: string;
  clock_file?: string;
  initial_utc?: number;
  control_request?: string;
  control_done?: string;
  hold_started?: string;
} {
  return JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../.test-admin-state.json", import.meta.url)),
      "utf8",
    ),
  );
}
function observed() {
  return JSON.parse(
    execFileSync(
      "python3",
      [
        "-c",
        `import sqlite3,json,sys
c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)
print(json.dumps({'events':c.execute('SELECT count(*) FROM events').fetchone()[0],'watermarks':c.execute('SELECT count(*) FROM planning_watermarks').fetchone()[0],'accepted':c.execute("SELECT count(*) FROM delivery_attempts WHERE outcome='accepted'").fetchone()[0],'accepted_logical':c.execute('SELECT count(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL').fetchone()[0]}))`,
        state().database,
      ],
      { encoding: "utf8" },
    ),
  );
}
let controls = 0;
// Ask the system fixture for one control action and wait for its acknowledgement.
async function control(command: Record<string, unknown>) {
  const { control_request, control_done } = state();
  if (!control_request || !control_done)
    throw new Error("Missing fixture control");
  writeFileSync(control_request, JSON.stringify(command), {
    flag: "wx",
    mode: 0o600,
  });
  controls++;
  await expect
    .poll(
      () => {
        try {
          return readFileSync(control_done, "utf8");
        } catch {
          return "";
        }
      },
      { timeout: 30000, intervals: [100, 250, 500] },
    )
    .toBe(String(controls));
}
test("browser configuration drives actual scanner planner delivery and distinct aggregate UIDs", async ({
  page,
  request,
}) => {
  await login(page);
  const a = await source(page, "ics:pull", logicalEndpoints.a, true);
  const b = await source(page, "ics:pull", logicalEndpoints.b, false);
  // A manual-static source has no URL: its only event arrives through the browser upload.
  const c = await source(page);
  const due = (state().initial_utc ?? Date.now() - 30000) + 60000;
  const stamp = new Date(due)
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d+Z$/, "Z");
  await page.getByLabel("ICS 文件", { exact: false }).setInputFiles({
    name: "static.ics",
    mimeType: "text/calendar",
    buffer: Buffer.from(
      `BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:uploaded\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:${stamp}\r\nSUMMARY:Uploaded synthetic calendar\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n`,
    ),
  });
  await page
    .getByRole("button", { name: "上传并替换快照", exact: true })
    .click();
  await expect(status(page)).toContainText("变更 1 个事件");
  // A push-only producer writes its only event through a token issued in the browser.
  const d = await source(page, "standard-calendar:push-only");
  await page.getByRole("button", { name: "签发令牌", exact: true }).click();
  const secret = await page
    .getByLabel("新令牌密钥（只显示这一次）")
    .inputValue();
  const ingest = `/api/ingest/standard-calendar/v1/sources/${d.id}`;
  const bearer = { authorization: `Bearer ${secret}` };
  const pushState = await (
    await request.get(`${ingest}/state`, { headers: bearer })
  ).json();
  const pushed = await request.post(`${ingest}/batches`, {
    headers: { ...bearer, "idempotency-key": "system-push-1" },
    data: {
      protocol_version: "standard-calendar-source/v1",
      batch_id: "system-push-1",
      mode: "snapshot",
      base_source_revision: pushState.source_revision,
      base_config_revision: pushState.config_revision,
      base_checkpoint: null,
      proposed_checkpoint: null,
      complete: true,
      pages: 1,
      collection_tag: null,
      operations: [
        {
          op: "upsert",
          calendar_id: d.calendar,
          event: {
            schema_version: "1.0",
            external_uid: "pushed",
            title: "Pushed synthetic event",
            status: "confirmed",
            start: {
              kind: "utc",
              value: new Date(due).toISOString().replace(/\.\d+Z$/, "Z"),
            },
          },
        },
      ],
    },
  });
  expect(pushed.status()).toBe(200);
  const outputId = await output(page);
  await page
    .getByLabel("Bark HTTPS origin", { exact: false })
    .fill(logicalEndpoints.bark);
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 2");
  await page.getByRole("link", { name: "日历与规则", exact: true }).click();
  await page.getByRole("button", { name: "创建规则", exact: true }).click();
  await page.getByLabel("名称", { exact: true }).fill("系统双源规则");
  await page.getByLabel("提前提醒（秒）").fill("0");
  await page.getByRole("button", { name: "读取来源选项", exact: true }).click();
  for (const s of [a, b, c, d]) {
    await page
      .getByRole("combobox", { name: "选择来源", exact: true })
      .selectOption(s.id);
    await page
      .getByRole("button", { name: "读取此来源日历", exact: true })
      .click();
    await page
      .getByLabel(`选择日历 primary · ${s.calendar}`, { exact: true })
      .check();
  }
  await page.getByRole("button", { name: "读取输出选项", exact: true }).click();
  await page.getByLabel(`Bark 合成输出 · ${outputId}`, { exact: true }).check();
  await page.getByRole("button", { name: "保存禁用规则", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 1");
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 2");
  await subscriptionDraft(page);
  await page.getByRole("button", { name: "读取来源选项", exact: true }).click();
  for (const s of [a, b]) {
    await page
      .getByRole("combobox", { name: "选择来源", exact: true })
      .selectOption(s.id);
    await page
      .getByRole("button", { name: "读取此来源日历", exact: true })
      .click();
    await page
      .getByLabel(`选择日历 primary · ${s.calendar}`, { exact: true })
      .check();
  }
  const capability = await saveSubscription(page);
  await dismissSecret(page);
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 2");
  await expect
    .poll(
      () => {
        const value = observed();
        return { events: value.events, watermarks: value.watermarks };
      },
      { timeout: 10000, intervals: [100, 250, 500] },
    )
    .toEqual({ events: 4, watermarks: 4 });
  const clock = state();
  function frame(utc: number) {
    if (!clock.clock_file) throw new Error("Missing controlled clock");
    const next = `${clock.clock_file}.next`;
    writeFileSync(
      next,
      JSON.stringify({ utc_now_ms: utc, monotonic_offset_ms: 61000 }),
      { flag: "wx", mode: 0o600 },
    );
    renameSync(next, clock.clock_file);
  }
  if (clock.clock_file && clock.initial_utc !== undefined) {
    expect(observed().accepted).toBe(0); // Frozen before the two event starts.
    frame(clock.initial_utc + 61000);
  }
  await expect
    .poll(observed, { timeout: 75000, intervals: [100, 250, 500] })
    .toMatchObject({
      events: 4,
      watermarks: 4,
      accepted: 4,
      accepted_logical: 4,
    });
  const feed = await request.get(capability);
  expect(feed.status()).toBe(200);
  const body = await feed.text();
  const uids = [...body.matchAll(/^UID:(.+)\r?$/gm)].map((m) => m[1]);
  expect(uids.length).toBe(2);
  expect(new Set(uids).size).toBe(2);
  expect(body.includes("First synthetic calendar")).toBe(true);
  expect(body.includes("Second synthetic calendar")).toBe(true);
  expect(
    (
      await request.get(capability, {
        headers: { "if-none-match": feed.headers().etag },
      })
    ).status(),
  ).toBe(304);
  if (clock.clock_file && clock.initial_utc !== undefined) {
    // Cached conditional responses still enforce future verified_at after a UTC rewind.
    frame(clock.initial_utc - 1000);
    await expect
      .poll(
        async () =>
          (
            await request.get(capability, {
              headers: { "if-none-match": feed.headers().etag },
            })
          ).status(),
        { timeout: 3000 },
      )
      .toBe(503);
    expect(observed().accepted).toBe(4);
    // Management authentication keeps real UTC despite business time movement.
    expect((await page.request.get("/api/admin/v1/sources")).status()).toBe(
      200,
    );
    frame(clock.initial_utc + 61000);
    await expect
      .poll(async () => (await request.get(capability)).status(), {
        timeout: 3000,
      })
      .toBe(200);
  }
  const network = await (await request.get(state().fixture_status)).json();
  expect(
    network.requests.filter(
      (r: { kind: string; status: number }) =>
        r.kind === "bark" && r.status === 200,
    ),
  ).toHaveLength(4);
  for (const source of ["a", "b"])
    expect(
      network.requests.filter(
        (r: { kind: string; status: number; source?: string }) =>
          r.kind === "ics" && r.status === 200 && r.source === source,
      ).length,
    ).toBeGreaterThanOrEqual(1);
  expect((await request.get("/health/live")).status()).toBe(200);
  expect((await request.get("/health/ready")).status()).toBe(200);

  // Forced restart: SIGKILL the service and start it again on the same database and clock.
  const sources = [a, b, c, d].map((s) => s.id);
  await control({ action: "restart" });
  await expect
    .poll(async () => (await request.get("/health/ready")).status(), {
      timeout: 20000,
    })
    .toBe(200);
  expect((await request.get("/health/live")).status()).toBe(200);
  // Configuration survives; a new browser login reads it back.
  await login(page);
  const listed = await (await page.request.get("/api/admin/v1/sources")).json();
  for (const id of sources)
    expect(
      listed.items.some((item: { source_id: string }) => item.source_id === id),
    ).toBe(true);
  expect((await request.get(capability)).status()).toBe(200);
  // Already accepted reminders stay delivered exactly once across further worker rounds.
  await page.waitForTimeout(5000);
  expect(observed()).toMatchObject({
    events: 4,
    accepted: 4,
    accepted_logical: 4,
  });
  const after = await (await request.get(state().fixture_status)).json();
  expect(
    after.requests.filter(
      (r: { kind: string; status: number }) =>
        r.kind === "bark" && r.status === 200,
    ),
  ).toHaveLength(4);

  // One upstream failing is a source-level state: the other source keeps syncing and the
  // core stays live and ready.
  const diagnostic = async (id: string) =>
    (
      await (
        await page.request.get(`/api/admin/v1/sources/${id}/diagnostics`)
      ).json()
    ).sync_status;
  const fetchedB = async () =>
    (await (await request.get(state().fixture_status)).json()).requests.filter(
      (r: { kind: string; status: number; source?: string }) =>
        r.kind === "ics" && r.status !== 503 && r.source === "b",
    ).length;
  const before = await fetchedB();
  await control({ action: "source", id: "a", status: 503 });
  await page.getByRole("link", { name: "来源", exact: true }).click();
  for (const s of [a, b]) {
    await page
      .locator("main section")
      .filter({ hasText: s.id })
      .getByRole("button", { name: "查看 / 编辑", exact: true })
      .click();
    await page
      .getByRole("button", { name: "请求立即刷新", exact: true })
      .click();
    await expect(status(page)).toContainText("服务端已记录刷新请求");
  }
  await expect
    .poll(async () => (await diagnostic(a.id))?.consecutive_failures ?? 0, {
      timeout: 30000,
      intervals: [250, 500, 1000],
    })
    .toBeGreaterThan(0);
  expect(["retry_after", "adapter", "unavailable"]).toContain(
    (await diagnostic(a.id)).outcome,
  );
  await expect
    .poll(fetchedB, { timeout: 30000, intervals: [250, 500, 1000] })
    .toBeGreaterThan(before);
  expect((await diagnostic(b.id)).consecutive_failures).toBe(0);
  // Bark being down is also not a core fault: a new due reminder meets the outage, its
  // attempt is recorded as retryable and the core stays live and ready.
  await control({ action: "bark", status: 503 });
  const businessNow = clock.clock_file
    ? JSON.parse(readFileSync(clock.clock_file, "utf8")).utc_now_ms
    : Date.now() + 3000;
  const late = await (
    await request.get(`${ingest}/state`, { headers: bearer })
  ).json();
  const event = (uid: string, at: number) => ({
    op: "upsert",
    calendar_id: d.calendar,
    event: {
      schema_version: "1.0",
      external_uid: uid,
      title: "Pushed synthetic event",
      status: "confirmed",
      start: {
        kind: "utc",
        value: new Date(at).toISOString().replace(/\.\d+Z$/, "Z"),
      },
    },
  });
  const repushed = await request.post(`${ingest}/batches`, {
    headers: { ...bearer, "idempotency-key": "system-push-2" },
    data: {
      protocol_version: "standard-calendar-source/v1",
      batch_id: "system-push-2",
      mode: "snapshot",
      base_source_revision: late.source_revision,
      base_config_revision: late.config_revision,
      base_checkpoint: null,
      proposed_checkpoint: null,
      complete: true,
      pages: 1,
      collection_tag: null,
      operations: [event("pushed", due), event("pushed-late", businessNow)],
    },
  });
  expect(repushed.status()).toBe(200);
  const retryable = () =>
    Number(
      execFileSync(
        "python3",
        [
          "-c",
          "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(c.execute(\"SELECT count(*) FROM delivery_attempts WHERE outcome='retryable'\").fetchone()[0])",
          state().database,
        ],
        { encoding: "utf8" },
      ),
    );
  await expect
    .poll(retryable, { timeout: 45000, intervals: [250, 500, 1000] })
    .toBeGreaterThan(0);
  const outage = await (await request.get(state().fixture_status)).json();
  expect(
    outage.requests.some(
      (r: { kind: string; status: number }) =>
        r.kind === "bark" && r.status === 503,
    ),
  ).toBe(true);
  expect(observed().accepted).toBe(4);
  for (const path of ["/health/live", "/health/ready"])
    expect((await request.get(path)).status(), path).toBe(200);

  // An upstream that stops answering is bounded by the fetch timeout and becomes a
  // source-level failure; the core stays live and ready while it hangs.
  const failuresB = (await diagnostic(b.id))?.consecutive_failures ?? 0;
  await control({ action: "hold", id: "b" });
  await page
    .locator("main section")
    .filter({ hasText: b.id })
    .getByRole("button", { name: "查看 / 编辑", exact: true })
    .click();
  await page.getByRole("button", { name: "请求立即刷新", exact: true }).click();
  await expect(status(page)).toContainText("服务端已记录刷新请求");
  // Probe while the fetch of b is actually hanging in the fixture.
  await expect
    .poll(() => existsSync(state().hold_started ?? ""), {
      timeout: 30000,
      intervals: [50, 100, 250],
    })
    .toBe(true);
  for (const path of ["/health/live", "/health/ready"])
    expect((await request.get(path)).status(), path).toBe(200);
  // The 3 s synthetic fetch timeout has not elapsed yet, so the stall was observed live.
  expect((await diagnostic(b.id))?.consecutive_failures ?? 0).toBe(failuresB);
  await expect
    .poll(async () => (await diagnostic(b.id))?.consecutive_failures ?? 0, {
      timeout: 30000,
      intervals: [250, 500, 1000],
    })
    .toBeGreaterThan(failuresB);
  // The synthetic fetcher reports its timeout as a transport failure.
  expect((await diagnostic(b.id)).outcome).toBe("adapter");
  for (const path of ["/health/live", "/health/ready"])
    expect((await request.get(path)).status(), path).toBe(200);
  await control({ action: "release" });

  // A panicking delivery worker is a core fault: live reports it while the management API
  // keeps serving; nothing exits or restarts by itself. A forced restart recovers.
  await control({ action: "bark", status: 599 });
  const panicking = await (
    await request.get(`${ingest}/state`, { headers: bearer })
  ).json();
  const panicAt = clock.clock_file
    ? JSON.parse(readFileSync(clock.clock_file, "utf8")).utc_now_ms
    : Date.now() + 3000;
  expect(
    (
      await request.post(`${ingest}/batches`, {
        headers: { ...bearer, "idempotency-key": "system-push-3" },
        data: {
          protocol_version: "standard-calendar-source/v1",
          batch_id: "system-push-3",
          mode: "snapshot",
          base_source_revision: panicking.source_revision,
          base_config_revision: panicking.config_revision,
          base_checkpoint: null,
          proposed_checkpoint: null,
          complete: true,
          pages: 1,
          collection_tag: null,
          operations: [
            event("pushed", due),
            event("pushed-late", businessNow),
            event("pushed-panic", panicAt),
          ],
        },
      })
    ).status(),
  ).toBe(200);
  await expect
    .poll(async () => (await request.get("/health/live")).status(), {
      timeout: 45000,
      intervals: [250, 500, 1000],
    })
    .toBe(503);
  const panicked = await (await request.get(state().fixture_status)).json();
  expect(
    panicked.requests.some(
      (r: { kind: string; status: number }) =>
        r.kind === "bark" && r.status === 599,
    ),
  ).toBe(true);
  expect((await page.request.get("/api/admin/v1/sources")).status()).toBe(200);
  await control({ action: "bark", status: 200 });
  await control({ action: "restart" });
  await expect
    .poll(async () => (await request.get("/health/ready")).status(), {
      timeout: 20000,
    })
    .toBe(200);
  expect((await request.get("/health/live")).status()).toBe(200);
});
