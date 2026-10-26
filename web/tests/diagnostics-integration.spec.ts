import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, type Page, test } from "@playwright/test";
import { login, source } from "./support/journey";

const database = () =>
  JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../.test-admin-state.json", import.meta.url)),
      "utf8",
    ),
  ).database;
function seed(id: string, calendar: string) {
  execFileSync("python3", [
    "-c",
    `import sqlite3,sys,time
c=sqlite3.connect(sys.argv[1]); c.execute('PRAGMA foreign_keys=ON'); now=int(time.time()*1000)
c.execute('UPDATE sources SET last_verified_at=?,snapshot_revision=1 WHERE id=?',(now,sys.argv[2]))
c.execute("INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash) VALUES (?,?,?,'diag-event',1,?, 'hash')",('diag-'+sys.argv[2],sys.argv[2],sys.argv[3],'{"title":"SECRET_EVENT_BODY"}'))
c.execute("INSERT INTO outputs(id,kind,credential_id) VALUES (?,'bark','synthetic_browser_bark')",('out-'+sys.argv[2],))
c.execute('INSERT INTO reminder_rules(id) VALUES (?)',('rule-'+sys.argv[2],))
c.execute('INSERT INTO rule_outputs(rule_id,output_id) VALUES (?,?)',('rule-'+sys.argv[2],'out-'+sys.argv[2]))
for n,(state,outcome) in enumerate([('accepted','accepted'),('retry_wait','retryable'),('retry_wait','unknown'),('failed_permanent','permanent_failure'),('leased',None)]):
 lid='logical-'+sys.argv[2]+'-'+str(n); jid='job-'+sys.argv[2]+'-'+str(n)
 c.execute("INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id,accepted_at) VALUES (?,?,?,?,?,?)",(lid,'diag-'+sys.argv[2],str(n),'rule-'+sys.argv[2],'out-'+sys.argv[2],now if n==0 else None))
 c.execute("INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at) VALUES (?,?,1,1,1,1,?,?,?)",(jid,lid,now,state,now+1000))
 c.execute("INSERT INTO delivery_attempts(id,job_id,attempt_no,lease_owner,lease_token,request_id,started_at,sent_at,finished_at,outcome,disposition) VALUES (?,?,1,'owner',?,?, ?,?,?,?,?)",('attempt-'+jid,jid,'SECRET_LEASE-'+jid,'SECRET_REQUEST-'+jid,now,now,None if outcome is None else now,outcome,None if outcome is None else state))
c.commit()`,
    database(),
    id,
    calendar,
  ]);
}
const panel = (page: Page) =>
  page.locator("main > section").filter({
    has: page.getByRole("heading", {
      name: "来源同步与提醒投递",
      exact: true,
    }),
  });
async function open(page: Page, id?: string) {
  await page.getByRole("link", { name: "诊断", exact: true }).click();
  const card = panel(page);
  if (id) {
    await expect(card.getByRole("status")).toContainText("请选择来源读取诊断");
    await card.getByLabel("诊断来源").selectOption(id);
    await card
      .getByRole("button", { name: "读取来源诊断", exact: true })
      .click();
  }
  return card;
}
test("real guarded diagnostics deny capabilities, show empty instance and missing source", async ({
  page,
  request,
}) => {
  const denied = await request.get("/api/admin/v1/sources/none/diagnostics", {
    headers: { authorization: "Bearer feed-token-is-not-admin" },
  });
  expect(denied.status()).toBe(403);
  await login(page);
  const list = await (await page.request.get("/api/admin/v1/sources")).json();
  const card = await open(page);
  if (list.items.length === 0)
    await expect(card.getByRole("status")).toContainText("本页没有已配置来源");
  else
    await expect(card.getByLabel("诊断来源").locator("option")).toHaveCount(
      list.items.length + 1,
    );
  expect(
    (await page.request.get("/api/admin/v1/sources/none/diagnostics")).status(),
  ).toBe(404);
});
test("real SQLite success, retry, unknown and failure are safe read-only projections", async ({
  page,
}) => {
  await login(page);
  await page.getByRole("link", { name: "来源", exact: true }).click();
  await expect(page.locator("main > p[role=status]")).toContainText(
    "来源列表已读取",
  );
  const s = await source(
    page,
    "ics:pull",
    "https://ics-diagnostics.example.test/calendar.ics",
  );
  let card = await open(page, s.id);
  await expect(card.getByRole("status")).toContainText("诊断快照时间");
  await expect(card).toContainText("尚无成功校验记录");
  await expect(card).toContainText("尚无投递任务记录");
  seed(s.id, s.calendar);
  const response = await page.request.get(
    `/api/admin/v1/sources/${s.id}/diagnostics`,
  );
  expect(response.status()).toBe(200);
  expect(response.headers()["cache-control"]).toBe("no-store");
  const value = await response.json();
  expect(value.freshness).toBe("fresh");
  expect(value.deliveries).toHaveLength(5);
  expect(JSON.stringify(value)).not.toContain("SECRET_");
  expect(Object.keys(value).sort()).toEqual(
    [
      "source_id",
      "observed_at",
      "mode",
      "enabled",
      "retired_at",
      "last_verified_at",
      "snapshot_revision",
      "freshness",
      "deliveries",
      "more_deliveries",
    ].sort(),
  );
  await card.getByRole("button", { name: "读取来源诊断", exact: true }).click();
  await expect(card).toContainText("符合当前配置的时效要求");
  await expect(card).toContainText("结果未知（可能已经发送）");
  await expect(card).toContainText("永久失败");
  await expect(card).toContainText("可重试失败");
  await expect(card).toContainText("已有发送意图，结果尚未确认");
  await expect(card).toContainText("当前不可查询");
  await expect(card).toContainText("不证明设备显示");
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  execFileSync("python3", [
    "-c",
    "import sqlite3,sys,time; c=sqlite3.connect(sys.argv[1]); c.execute('UPDATE sources SET last_verified_at=? WHERE id=?',(int(time.time()*1000)+3600000,sys.argv[2])); c.commit()",
    database(),
    s.id,
  ]);
  expect(
    (
      await (
        await page.request.get(`/api/admin/v1/sources/${s.id}/diagnostics`)
      ).json()
    ).freshness,
  ).toBe("future_timestamp");
  execFileSync("python3", [
    "-c",
    "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('UPDATE sources SET last_verified_at=0 WHERE id=?',(sys.argv[2],)); c.commit()",
    database(),
    s.id,
  ]);
  await card.getByRole("button", { name: "读取来源诊断", exact: true }).click();
  await expect(card).toContainText("已超过配置的时效要求");
  await page.reload();
  card = panel(page);
  await expect(card.getByLabel("持久诊断结果")).toBeEmpty();
});
test("static sources and untrusted names, cancellation and error responses are honest", async ({
  page,
}) => {
  await login(page);
  const s = await source(page);
  const malicious = '<img src=x onerror="window.pwned=1">';
  execFileSync("python3", [
    "-c",
    "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1]); x=json.loads(c.execute('SELECT config_json FROM sources WHERE id=?',(sys.argv[2],)).fetchone()[0]); x['name']=sys.argv[3]; c.execute('UPDATE sources SET config_json=? WHERE id=?',(json.dumps(x),sys.argv[2])); c.commit()",
    database(),
    s.id,
    malicious,
  ]);
  const card = await open(page, s.id);
  await expect(card).toContainText("不按轮询时效判定");
  await expect(card.getByLabel("诊断来源")).toContainText(malicious);
  expect(await page.evaluate(() => "pwned" in window)).toBe(false);
  await expect(card.locator("img")).toHaveCount(0);
  let release = () => {};
  await page.route("**/sources/*/diagnostics", async (route) => {
    await new Promise<void>((r) => {
      release = r;
    });
    await route.continue().catch(() => {});
  });
  await card.getByRole("button", { name: "读取来源诊断", exact: true }).click();
  await expect(
    card.getByRole("button", { name: "读取来源诊断", exact: true }),
  ).toBeDisabled();
  await card.getByRole("button", { name: "取消诊断读取", exact: true }).click();
  await expect(card.getByRole("status")).toContainText("已取消");
  release();
  await page.unroute("**/sources/*/diagnostics");
  await page.route("**/sources/*/diagnostics", (route) =>
    route.fulfill({
      status: 503,
      contentType: "application/json",
      body: '{"error":"SECRET_RAW_EXTERNAL_BODY"}',
    }),
  );
  await card.getByRole("button", { name: "读取来源诊断", exact: true }).click();
  await expect(card.getByRole("status")).toContainText("服务未确认可信结果");
  await expect(card).not.toContainText("SECRET_RAW");
  await page.unroute("**/sources/*/diagnostics");
  await page.route("**/sources/*/diagnostics", (route) =>
    route.fulfill({ status: 403 }),
  );
  await card.getByRole("button", { name: "读取来源诊断", exact: true }).click();
  await expect(card.getByRole("status")).toContainText("会话授权已变化");
  await expect(card.getByLabel("持久诊断结果")).toBeEmpty();
});
