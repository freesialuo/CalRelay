import { execFileSync } from "node:child_process";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
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
test("browser configuration drives actual scanner planner delivery and distinct aggregate UIDs", async ({
  page,
  request,
}) => {
  await login(page);
  const a = await source(page, "ics:pull", logicalEndpoints.a, true);
  const b = await source(page, "ics:pull", logicalEndpoints.b, false);
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
    .toEqual({ events: 2, watermarks: 2 });
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
      events: 2,
      watermarks: 2,
      accepted: 2,
      accepted_logical: 2,
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
    expect(observed().accepted).toBe(2);
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
  ).toHaveLength(2);
  for (const source of ["a", "b"])
    expect(
      network.requests.filter(
        (r: { kind: string; status: number; source?: string }) =>
          r.kind === "ics" && r.status === 200 && r.source === source,
      ).length,
    ).toBeGreaterThanOrEqual(1);
  expect((await request.get("/health/live")).status()).toBe(200);
  expect((await request.get("/health/ready")).status()).toBe(200);
});
