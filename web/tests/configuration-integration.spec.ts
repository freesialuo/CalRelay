import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, type Page, test } from "@playwright/test";

const apiURL =
  "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";
async function widget(
  page: Page,
  mode: "valid" | "duplicate" | "expired" | "error" = "valid",
) {
  await page.route(apiURL, (route) =>
    route.fulfill({
      contentType: "text/javascript",
      body: `let first=true;window.turnstile={render(root,options){if(options['response-field']!==false)throw new Error('Unexpected DOM challenge field');const button=document.createElement('button');button.textContent='完成离线测试挑战';button.type='button';button.onclick=()=>{const mode=${JSON.stringify(mode)};if(first&&mode==='expired'){first=false;options['expired-callback']();return;}if(first&&mode==='error'){first=false;options['error-callback']();return;}const challenge='synthetic-challenge:'+crypto.randomUUID();options.callback(challenge);if(mode==='duplicate')options.callback(challenge);};root.append(button);return 'offline-widget';},remove(){document.querySelectorAll('button').forEach(button=>{if(button.textContent==='完成离线测试挑战')button.remove();});}};`,
    }),
  );
  // Block all other non-local requests. No real Cloudflare/ICS/Bark request is permitted.
  await page.route("**/*", (route) => {
    const url = new URL(route.request().url());
    if (url.hostname === "127.0.0.1" || url.href === apiURL)
      return route.fallback();
    return route.abort();
  });
}
async function login(page: Page, mode: "valid" | "duplicate" = "valid") {
  await widget(page, mode);
  await page.goto("/admin/");
  const panel = page.getByRole("region", { name: "管理登录", exact: true });
  await panel
    .getByRole("button", { name: "验证管理身份", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "完成离线测试挑战", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("本次管理登录已完成");
  await expect(
    page.getByRole("button", { name: "创建来源", exact: true }),
  ).toBeEnabled();
}
test("subscription management is available after real login", async ({
  page,
}) => {
  await login(page);
  await page.getByRole("link", { name: "汇总订阅", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "汇总订阅", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "创建订阅", exact: true }),
  ).toBeEnabled();
});
const status = (page: Page) => page.locator("main > p[role=status]");
function database(): string {
  return JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../.test-admin-state.json", import.meta.url)),
      "utf8",
    ),
  ).database;
}
function persisted(id: string) {
  return JSON.parse(
    execFileSync(
      "python3",
      [
        "-c",
        "import sqlite3,json,sys; db=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); r=db.execute('SELECT revision,revoked_at,configuration_json FROM aggregate_subscriptions WHERE id=?',(sys.argv[2],)).fetchone(); print(json.dumps(None if r is None else {'revision':r[0],'retired':r[1] is not None,'configuration':json.loads(r[2])}))",
        database(),
        id,
      ],
      { encoding: "utf8" },
    ),
  );
}
async function source(page: Page) {
  await page.getByRole("link", { name: "来源", exact: true }).click();
  await page.getByRole("button", { name: "创建来源", exact: true }).click();
  const id = `source_${crypto.randomUUID().replaceAll("-", "")}`;
  await page.getByLabel("稳定来源 ID").fill(id);
  const calendar = await page
    .getByLabel("日历 ID", { exact: true })
    .inputValue();
  await page.getByLabel("名称", { exact: true }).fill("静态测试来源");
  await page.getByLabel("来源类型与更新方式").selectOption("ics:manual-static");
  await page.getByLabel("远端日历 external_id").fill("primary");
  await page.getByRole("button", { name: "保存禁用来源", exact: true }).click();
  await expect(page.locator("main > p[role=status]")).toContainText(
    "配置版本 1",
  );
  await page.getByLabel("启用来源", { exact: true }).check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(page.locator("main > p[role=status]")).toContainText(
    "配置版本 2",
  );
  return { id, calendar };
}
async function subscriptionDraft(page: Page) {
  await page.getByRole("link", { name: "汇总订阅", exact: true }).click();
  await page.getByRole("button", { name: "创建订阅", exact: true }).click();
  const id = await page.getByLabel("稳定订阅 ID").inputValue();
  await page.getByLabel("名称", { exact: true }).fill("只读汇总");
  return id;
}
async function saveSubscription(page: Page) {
  const id = await page.getByLabel("稳定订阅 ID").inputValue();
  const url = `**/api/admin/v1/subscriptions/${id}`;
  let capability = "";
  await page.route(url, async (route, request) => {
    if (request.method() !== "PUT") return route.fallback();
    const response = await route.fetch();
    expect(response.status()).toBe(201);
    capability = (await response.json()).capability_url;
    return route.fulfill({ response });
  });
  await page.getByRole("button", { name: "保存禁用订阅", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 1");
  await page.unroute(url);
  return capability;
}
async function dismissSecret(page: Page) {
  await page
    .getByRole("button", { name: "已保存，清除本页链接", exact: true })
    .click();
}
async function openSubscription(page: Page, id: string) {
  const row = page
    .locator("main section")
    .filter({ has: page.locator("p").filter({ hasText: `ID：${id} ·` }) });
  await row.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await expect(page.getByLabel("稳定订阅 ID")).toHaveValue(id);
}

test("real subscription selections, one-time capability, edit retention, rotation and retirement", async ({
  page,
  context,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await login(page);
  const s = await source(page);
  const id = await subscriptionDraft(page);
  await page.getByRole("button", { name: "读取来源选项", exact: true }).click();
  await page.getByLabel("选择来源", { exact: true }).selectOption(s.id);
  await page
    .getByRole("button", { name: "读取此来源日历", exact: true })
    .click();
  await page
    .getByLabel(`选择日历 primary · ${s.calendar}`, { exact: true })
    .check();
  const capability = await saveSubscription(page);
  expect(persisted(id)).toMatchObject({
    revision: 1,
    configuration: {
      enabled: false,
      sources: [{ source_id: s.id, calendar_ids: [s.calendar] }],
    },
  });
  const secret = page.getByLabel("一次性链接（默认隐藏）");
  await expect(secret).toHaveValue("");
  await page
    .getByRole("button", { name: "复制一次性链接", exact: true })
    .click();
  expect(
    (await page.evaluate(() => navigator.clipboard.readText())) ===
      new URL(capability, page.url()).href,
  ).toBe(true);
  await page
    .getByRole("button", { name: "显示一次性链接", exact: true })
    .click();
  expect((await secret.inputValue()).endsWith(capability)).toBe(true);
  await page
    .getByRole("button", { name: "隐藏一次性链接", exact: true })
    .click();
  await expect(secret).toHaveAttribute("type", "password");
  expect(new URL(page.url()).pathname).toBe("/admin/");
  expect(new URL(page.url()).hash).toBe("#subscriptions");
  await dismissSecret(page);
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 2");
  expect((await page.request.get(capability)).status()).toBe(200);
  await expect(page.getByText("一次性订阅链接", { exact: true })).toHaveCount(
    0,
  );
  await page
    .getByLabel("名称", { exact: true })
    .fill('<img src=x onerror="window.pwned=1">');
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 3");
  expect((await page.request.get(capability)).status()).toBe(200);
  await expect(page.locator("img")).toHaveCount(0);
  await page.reload();
  await openSubscription(page, id);
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    '<img src=x onerror="window.pwned=1">',
  );
  await expect(page.getByText("一次性订阅链接", { exact: true })).toHaveCount(
    0,
  );
  const loginPanel = page.getByRole("region", {
    name: "管理登录",
    exact: true,
  });
  await loginPanel
    .getByRole("button", { name: "验证管理身份", exact: true })
    .click();
  await loginPanel
    .getByRole("button", { name: "完成离线测试挑战", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "轮换订阅链接", exact: true }),
  ).toBeEnabled();
  page.once("dialog", (d) => d.accept());
  let next = "";
  const tokenPath = `**/api/admin/v1/subscriptions/${id}/token`;
  await page.route(tokenPath, async (route) => {
    const response = await route.fetch();
    expect(response.status()).toBe(200);
    next = (await response.json()).capability_url;
    return route.fulfill({ response });
  });
  await page.getByRole("button", { name: "轮换订阅链接", exact: true }).click();
  await expect(status(page)).toContainText("轮换至版本 4");
  await page.unroute(tokenPath);
  expect(next !== capability).toBe(true);
  expect((await page.request.get(capability)).status()).toBe(404);
  expect((await page.request.get(next)).status()).toBe(200);
  await dismissSecret(page);
  await page.getByLabel("启用配置").uncheck();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 5");
  expect((await page.request.get(next)).status()).toBe(404);
  await page.getByRole("button", { name: "退休订阅", exact: true }).click();
  await page.getByRole("button", { name: "确认永久退休", exact: true }).click();
  await expect(status(page)).toContainText("服务端已确认永久退休");
  expect(persisted(id)).toMatchObject({ revision: 6, retired: true });
  await page.reload();
  await openSubscription(page, id);
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
  expect(
    await page.evaluate(() => [localStorage.length, sessionStorage.length]),
  ).toEqual([0, 0]);
});

test("real subscription CAS keeps input and requires explicit read and adoption", async ({
  page,
  browser,
}) => {
  await login(page);
  const id = await subscriptionDraft(page);
  await saveSubscription(page);
  await dismissSecret(page);
  await page.getByLabel("名称", { exact: true }).fill("本地草稿");
  const context = await browser.newContext({
    ignoreHTTPSErrors: true,
    baseURL: "https://127.0.0.1:4175",
  });
  try {
    const other = await context.newPage();
    await login(other);
    await other.getByRole("link", { name: "汇总订阅", exact: true }).click();
    await openSubscription(other, id);
    await other.getByLabel("名称", { exact: true }).fill("独立会话版本");
    await other.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(status(other)).toContainText("配置版本 2");
    await page.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(status(page)).toContainText("配置版本冲突");
    await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
      "本地草稿",
    );
    expect(persisted(id).revision).toBe(2);
    await page
      .getByRole("button", { name: "读取最新版本（保留输入）", exact: true })
      .click();
    await expect(
      page.getByText("当前服务端：独立会话版本", { exact: false }),
    ).toBeVisible();
    await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
      "本地草稿",
    );
    page.once("dialog", (d) => d.accept());
    await page
      .getByRole("button", {
        name: "采用最新版本重新编辑（放弃原输入）",
        exact: true,
      })
      .click();
    await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
      "独立会话版本",
    );
  } finally {
    await context.close();
  }
});

for (const failure of ["csrf", "origin"] as const) {
  test(`real subscription ${failure} denial preserves database`, async ({
    page,
  }) => {
    await login(page);
    const id = await subscriptionDraft(page);
    await saveSubscription(page);
    await dismissSecret(page);
    await page.route(
      `**/api/admin/v1/subscriptions/${id}`,
      async (route, request) => {
        if (request.method() !== "PUT") return route.fallback();
        const headers = {
          ...request.headers(),
          ...(failure === "csrf"
            ? { "x-csrf-token": "invalid_test_csrf" }
            : { origin: "https://wrong.example" }),
        };
        const response = await route.fetch({ headers, maxRedirects: 0 });
        expect(response.status()).toBe(403);
        return route.fulfill({ response });
      },
    );
    await page.getByLabel("名称", { exact: true }).fill("拒绝写入");
    await page.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(status(page)).toContainText("管理请求被拒绝");
    expect(persisted(id).revision).toBe(1);
    await expect(
      page.getByRole("button", { name: "保存配置", exact: true }),
    ).toBeDisabled();
  });
}

test("lost create receipt never repeats write and recovers only by read then explicit rotation", async ({
  page,
}) => {
  await login(page);
  const id = await subscriptionDraft(page);
  let writes = 0;
  let release = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(
    `**/api/admin/v1/subscriptions/${id}`,
    async (route, request) => {
      if (request.method() !== "PUT") return route.fallback();
      writes++;
      const response = await route.fetch();
      expect(response.status()).toBe(201);
      await held;
      await route.fulfill({ response }).catch(() => {});
    },
  );
  await page.getByRole("button", { name: "保存禁用订阅", exact: true }).click();
  await expect.poll(() => persisted(id)?.revision).toBe(1);
  await expect(
    page.getByRole("button", { name: "保存禁用订阅", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "取消等待并核实结果", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("写入结果未知");
  release();
  expect(writes).toBe(1);
  const panel = page.getByRole("region", { name: "管理登录", exact: true });
  await panel
    .getByRole("button", { name: "验证管理身份", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "完成离线测试挑战", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("写入结果未知");
  await page
    .getByRole("button", { name: "读取最新版本（保留输入）", exact: true })
    .click();
  await expect(
    page.getByText("当前服务端：只读汇总", { exact: false }),
  ).toBeVisible();
  page.once("dialog", (d) => d.accept());
  await page
    .getByRole("button", {
      name: "采用最新版本重新编辑（放弃原输入）",
      exact: true,
    })
    .click();
  await expect(page.getByText("一次性订阅链接", { exact: true })).toHaveCount(
    0,
  );
  expect(persisted(id).revision).toBe(1);
  expect(writes).toBe(1);
});

test("one-time URL clipboard failure, cancelled navigation, expiry and session revocation are explicit", async ({
  page,
}) => {
  await page.addInitScript(() =>
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: () => Promise.reject(new Error("denied")) },
    }),
  );
  await login(page);
  await subscriptionDraft(page);
  await saveSubscription(page);
  await page
    .getByRole("button", { name: "复制一次性链接", exact: true })
    .click();
  await expect(
    page.getByText("浏览器未允许复制。", { exact: false }),
  ).toBeVisible();
  page.once("dialog", (d) => d.dismiss());
  await page.getByRole("link", { name: "输出与订阅", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "汇总订阅", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("一次性订阅链接", { exact: true })).toBeVisible();
  await page
    .getByRole("region", { name: "管理登录", exact: true })
    .getByRole("button", { name: "退出管理会话", exact: true })
    .click();
  await expect(page.getByText("一次性订阅链接", { exact: true })).toHaveCount(
    0,
  );
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
});

test("expired and empty subscriptions are bounded explicit configurations and mobile layout fits", async ({
  page,
}, testInfo) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await login(page);
  const id = await subscriptionDraft(page);
  await page
    .getByLabel("到期时间", { exact: false })
    .fill("2020-01-01T00:00:00Z");
  const capability = await saveSubscription(page);
  await dismissSecret(page);
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("不符合服务契约");
  expect(persisted(id).revision).toBe(1);
  expect((await page.request.get(capability)).status()).toBe(404);
  expect(persisted(id).configuration.sources).toEqual([]);
  await expect(
    page.getByText("链接已到期（不公开密钥）", { exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: testInfo.outputPath("subscription-mobile.png"),
    fullPage: true,
  });
});

test("output requires an existing CLI handle and invalid reference cannot claim saved", async ({
  page,
}) => {
  await login(page);
  await page.getByRole("link", { name: "输出与订阅", exact: true }).click();
  await page.getByRole("button", { name: "创建输出", exact: true }).click();
  await expect(page.getByText("首次使用：", { exact: false })).toContainText(
    "credential provision-bark",
  );
  await page.getByLabel("名称", { exact: true }).fill("未初始化输出");
  await page.getByLabel("已有 Bark credential_id").fill("missing_credential");
  await page.getByRole("button", { name: "保存禁用输出", exact: true }).click();
  await expect(status(page)).toContainText("不符合服务契约");
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "未初始化输出",
  );
  await expect(
    page.getByRole("button", { name: "保存禁用输出", exact: true }),
  ).toBeEnabled();
});

test("deselecting a chosen calendar also updates its available option", async ({
  page,
}) => {
  await login(page);
  const s = await source(page);
  await subscriptionDraft(page);
  await page.getByRole("button", { name: "读取来源选项", exact: true }).click();
  await page.getByLabel("选择来源", { exact: true }).selectOption(s.id);
  await page
    .getByRole("button", { name: "读取此来源日历", exact: true })
    .click();
  const available = page.getByLabel(`选择日历 primary · ${s.calendar}`, {
    exact: true,
  });
  await available.check();
  await page.getByLabel(`日历 ${s.calendar}`, { exact: true }).uncheck();
  await expect(available).not.toBeChecked();
});

function reminderState(kind: "rules" | "outputs", id: string) {
  const table = kind === "rules" ? "reminder_rules" : "outputs";
  return JSON.parse(
    execFileSync(
      "python3",
      [
        "-c",
        `import sqlite3,json,sys;db=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True);r=db.execute('SELECT generation,enabled,retired_at FROM ${table} WHERE id=?',(sys.argv[2],)).fetchone();print(json.dumps(None if r is None else {'generation':r[0],'enabled':r[1],'retired':r[2] is not None}))`,
        database(),
        id,
      ],
      { encoding: "utf8" },
    ),
  );
}
async function output(page: Page) {
  await page.getByRole("link", { name: "输出与订阅", exact: true }).click();
  await page.getByRole("button", { name: "创建输出", exact: true }).click();
  const id = await page.getByLabel("稳定输出 ID").inputValue();
  await page.getByLabel("名称", { exact: true }).fill("Bark 合成输出");
  await page
    .getByLabel("已有 Bark credential_id")
    .fill("synthetic_browser_bark");
  await page.getByRole("button", { name: "保存禁用输出", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 1");
  return id;
}
async function openReminder(page: Page, id: string) {
  const row = page
    .locator("main section")
    .filter({ has: page.locator("p").filter({ hasText: `ID：${id} ·` }) });
  await row.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await expect(page.getByLabel(/^稳定.* ID$/)).toHaveValue(id);
}
async function reauthenticate(page: Page) {
  const panel = page.getByRole("region", { name: "管理登录", exact: true });
  await panel
    .getByRole("button", { name: "验证管理身份", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "完成离线测试挑战", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("本次管理登录已完成");
}

test("real CLI handle, output and rule lifecycle use generation CAS across pages and reload", async ({
  page,
}, testInfo) => {
  await login(page);
  const s = await source(page);
  const outputId = await output(page);
  expect(reminderState("outputs", outputId)).toMatchObject({
    generation: 1,
    enabled: 0,
    retired: false,
  });
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 2");
  await page.getByRole("link", { name: "日历与规则", exact: true }).click();
  await page.getByRole("button", { name: "创建规则", exact: true }).click();
  const ruleId = await page.getByLabel("稳定规则 ID").inputValue();
  await page.getByLabel("名称", { exact: true }).fill("提前毫秒规则");
  await page.getByLabel("提前提醒（秒）").fill("0.001");
  await page.getByRole("button", { name: "读取来源选项", exact: true }).click();
  await page.getByLabel("选择来源", { exact: true }).selectOption(s.id);
  await page
    .getByRole("button", { name: "读取此来源日历", exact: true })
    .click();
  await page
    .getByLabel(`选择日历 primary · ${s.calendar}`, { exact: true })
    .check();
  await page.getByRole("button", { name: "读取输出选项", exact: true }).click();
  await page.getByLabel(`Bark 合成输出 · ${outputId}`, { exact: true }).check();
  await page.getByRole("button", { name: "保存禁用规则", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 1");
  expect(reminderState("rules", ruleId)).toMatchObject({
    generation: 1,
    enabled: 0,
    retired: false,
  });
  await page.getByLabel("启用配置").check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 2");
  await page.reload();
  await openReminder(page, ruleId);
  await expect(page.getByLabel("提前提醒（秒）")).toHaveValue("0.001");
  await expect(
    page.getByLabel(`日历 ${s.calendar}`, { exact: true }),
  ).toBeChecked();
  await expect(
    page.getByLabel(`输出 ${outputId}`, { exact: true }),
  ).toBeChecked();
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
  await reauthenticate(page);
  await page.getByLabel("启用配置").uncheck();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(status(page)).toContainText("配置版本 3");
  expect(reminderState("rules", ruleId).enabled).toBe(0);
  await page.setViewportSize({ width: 375, height: 812 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: testInfo.outputPath("rule-mobile.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "退休规则", exact: true }).click();
  await page.getByRole("button", { name: "确认永久退休", exact: true }).click();
  await expect(status(page)).toContainText("服务端已确认永久退休");
  expect(reminderState("rules", ruleId)).toMatchObject({
    generation: 4,
    retired: true,
  });
  await page.getByRole("link", { name: "输出与订阅", exact: true }).click();
  await openReminder(page, outputId);
  await page.getByRole("button", { name: "退休输出", exact: true }).click();
  await page.getByRole("button", { name: "确认永久退休", exact: true }).click();
  await expect(status(page)).toContainText("服务端已确认永久退休");
  expect(reminderState("outputs", outputId)).toMatchObject({
    generation: 3,
    retired: true,
  });
});

test("real independent output generation conflict retains draft and nonce remains usable", async ({
  page,
  browser,
}) => {
  await login(page);
  const id = await output(page);
  await page.getByLabel("名称", { exact: true }).fill("未提交输出草稿");
  const context = await browser.newContext({
    ignoreHTTPSErrors: true,
    baseURL: "https://127.0.0.1:4175",
  });
  try {
    const other = await context.newPage();
    await login(other);
    await other.getByRole("link", { name: "输出与订阅", exact: true }).click();
    await openReminder(other, id);
    await other.getByLabel("名称", { exact: true }).fill("另一管理员输出");
    await other.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(status(other)).toContainText("配置版本 2");
    await page.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(status(page)).toContainText("配置版本冲突");
    await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
      "未提交输出草稿",
    );
    expect(reminderState("outputs", id).generation).toBe(2);
    await page
      .getByRole("button", { name: "读取最新版本（保留输入）", exact: true })
      .click();
    await expect(
      page.getByText("当前服务端：另一管理员输出", { exact: false }),
    ).toBeVisible();
    page.once("dialog", (d) => d.accept());
    await page
      .getByRole("button", {
        name: "采用最新版本重新编辑（放弃原输入）",
        exact: true,
      })
      .click();
    await page.getByLabel("名称", { exact: true }).fill("采用后重提");
    await page.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(status(page)).toContainText("配置版本 3");
  } finally {
    await context.close();
  }
});

test("one-time URL clears after its local retention deadline without changing server revision", async ({
  page,
}) => {
  await page.clock.install();
  await login(page);
  const id = await subscriptionDraft(page);
  await saveSubscription(page);
  await expect(page.getByText("一次性订阅链接", { exact: true })).toBeVisible();
  await page.clock.fastForward(120001);
  await expect(page.getByText("一次性订阅链接", { exact: true })).toHaveCount(
    0,
  );
  expect(persisted(id).revision).toBe(1);
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeEnabled();
});

test("untrusted capability receipt cannot inject an external URL or claim save confirmation", async ({
  page,
}) => {
  await login(page);
  const id = await subscriptionDraft(page);
  await page.route(
    `**/api/admin/v1/subscriptions/${id}`,
    async (route, request) => {
      if (request.method() !== "PUT") return route.fallback();
      const response = await route.fetch();
      expect(response.status()).toBe(201);
      const data = await response.json();
      data.capability_url = "https://untrusted.example/secret";
      return route.fulfill({ response, json: data });
    },
  );
  await page.getByRole("button", { name: "保存禁用订阅", exact: true }).click();
  await expect(status(page)).toContainText("结果未知");
  await expect(page.getByText("一次性订阅链接", { exact: true })).toHaveCount(
    0,
  );
  expect(persisted(id).revision).toBe(1);
  expect(new URL(page.url()).origin).toBe("https://127.0.0.1:4175");
  await expect(
    page.getByRole("button", { name: "保存禁用订阅", exact: true }),
  ).toBeDisabled();
});
