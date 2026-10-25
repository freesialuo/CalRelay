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
const sourceStatus = (page: Page) => page.locator("main").getByRole("status");
async function draft(page: Page, existingId?: string): Promise<string> {
  const id = existingId ?? `source_${crypto.randomUUID().replaceAll("-", "")}`;
  await page.getByRole("button", { name: "创建来源", exact: true }).click();
  await page.getByLabel("稳定来源 ID").fill(id);
  await page.getByLabel("名称", { exact: true }).fill(`来源 ${id}`);
  await page
    .getByLabel("ICS HTTPS 地址", { exact: false })
    .fill("https://calendar.example/events.ics");
  await page.getByLabel("远端日历 external_id").fill("primary");
  return id;
}
async function create(page: Page): Promise<string> {
  const id = await draft(page);
  await page.getByRole("button", { name: "保存禁用来源", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("服务端已确认保存配置版本 1");
  return id;
}
async function open(page: Page, id: string) {
  const row = page
    .locator("main section")
    .filter({ has: page.locator("p").filter({ hasText: `ID：${id} ·` }) });
  await row.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await expect(page.getByLabel("稳定来源 ID")).toHaveValue(id);
}
function state(): { upstream: string; database: string } {
  return JSON.parse(
    readFileSync(
      fileURLToPath(new URL("../.test-admin-state.json", import.meta.url)),
      "utf8",
    ),
  );
}
function persisted(id: string): {
  enabled: number;
  revision: number;
  retired: boolean;
} {
  const data = execFileSync(
    "python3",
    [
      "-c",
      "import sqlite3,json,sys; db=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); row=db.execute('SELECT enabled,config_revision,retired_at FROM sources WHERE id=?',(sys.argv[2],)).fetchone(); print(json.dumps({'enabled':row[0],'revision':row[1],'retired':row[2] is not None}))",
      state().database,
      id,
    ],
    { encoding: "utf8" },
  );
  return JSON.parse(data);
}

test("real guard and SQLite source lifecycle survives browser refresh", async ({
  page,
}) => {
  await login(page);
  const id = await create(page);
  expect(persisted(id)).toMatchObject({
    enabled: 0,
    revision: 1,
    retired: false,
  });
  await page.getByLabel("启用来源", { exact: true }).check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("配置版本 2");
  await page.getByLabel("名称", { exact: true }).fill("真实回读名称");
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("配置版本 3");
  await page.reload();
  await expect(
    page.getByRole("button", { name: "创建来源", exact: true }),
  ).toBeDisabled();
  await open(page, id);
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "真实回读名称",
  );
  const panel = page.getByRole("region", { name: "管理登录", exact: true });
  await panel
    .getByRole("button", { name: "验证管理身份", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "完成离线测试挑战", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeEnabled();
  await page.getByLabel("启用来源", { exact: true }).uncheck();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("配置版本 4");
  await page.getByRole("button", { name: "退休来源", exact: true }).click();
  await page.getByRole("button", { name: "确认永久退休", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("服务端已确认永久退休");
  expect(persisted(id)).toMatchObject({
    enabled: 0,
    revision: 5,
    retired: true,
  });
  await draft(page, id);
  await page.getByRole("button", { name: "保存禁用来源", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("配置版本冲突");
  expect(persisted(id)).toMatchObject({
    enabled: 0,
    revision: 5,
    retired: true,
  });
  page.once("dialog", (dialog) => dialog.accept());
  await page.reload();
  await open(page, id);
  await expect(
    page.getByText("此来源已永久退休", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
  expect(
    await page.evaluate(() => [localStorage.length, sessionStorage.length]),
  ).toEqual([0, 0]);
});

test("real CAS rejects a stale independent browser without overwriting its draft", async ({
  page,
  browser,
}) => {
  await login(page);
  const id = await create(page);
  await page.getByLabel("名称", { exact: true }).fill("本地未提交草稿");
  const context = await browser.newContext({
    ignoreHTTPSErrors: true,
    baseURL: "https://127.0.0.1:4175",
  });
  try {
    const other = await context.newPage();
    await login(other);
    await open(other, id);
    await other.getByLabel("名称", { exact: true }).fill("另一管理员版本");
    await other.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(sourceStatus(other)).toContainText("配置版本 2");
    await page.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(sourceStatus(page)).toContainText("配置版本冲突");
    await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
      "本地未提交草稿",
    );
    expect(persisted(id).revision).toBe(2);
  } finally {
    await context.close();
  }
});

for (const failure of ["csrf", "origin"]) {
  test(`real ${failure} rejection has no database side effects`, async ({
    page,
  }) => {
    await login(page);
    const id = await create(page);
    await page.route(
      `**/api/admin/v1/sources/${id}`,
      async (route, request) => {
        if (request.method() !== "PUT") return route.fallback();
        const headers = {
          ...request.headers(),
          ...(failure === "csrf"
            ? { "x-csrf-token": "invalid_fixture_csrf" }
            : { origin: "https://wrong.example" }),
        };
        // Chromium controls Origin on browser requests. Send this negative case
        // through Playwright's real HTTP transport, preserving the bad header.
        const response = await route.fetch({ headers, maxRedirects: 0 });
        expect(response.status()).toBe(403);
        return route.fulfill({ response });
      },
    );
    await page.getByLabel("名称", { exact: true }).fill("不应提交");
    await page.getByRole("button", { name: "保存配置", exact: true }).click();
    await expect(sourceStatus(page)).toContainText("管理请求被拒绝");
    expect(persisted(id).revision).toBe(1);
    await expect(
      page.getByRole("button", { name: "保存配置", exact: true }),
    ).toBeDisabled();
  });
}

test("source and feed bearer credentials alone do not grant management", async ({
  request,
}) => {
  for (const bearer of ["synthetic-source-token", "synthetic-feed-token"]) {
    const response = await request.get(
      `${state().upstream}/api/admin/v1/sources`,
      { headers: { authorization: `Bearer ${bearer}` } },
    );
    expect(response.status()).toBe(403);
  }
});

test("cancelled real write commits at most once and is explicitly read back", async ({
  page,
}) => {
  await login(page);
  const id = await draft(page);
  let writes = 0;
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(`**/api/admin/v1/sources/${id}`, async (route, request) => {
    if (request.method() !== "PUT") return route.fallback();
    writes++;
    const response = await route.fetch({ maxRedirects: 0 });
    expect(response.status()).toBe(201);
    await held;
    await route.fulfill({ response }).catch(() => {});
  });
  const save = page.getByRole("button", { name: "保存禁用来源", exact: true });
  await save.click();
  await expect(save).toBeDisabled();
  await expect
    .poll(() => {
      try {
        return persisted(id).revision;
      } catch {
        return 0;
      }
    })
    .toBe(1);
  await page
    .getByRole("button", { name: "取消等待并核实结果", exact: true })
    .click();
  await expect(sourceStatus(page)).toContainText("写入结果未知");
  release?.();
  await expect(save).toBeDisabled();
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
  await expect(loginPanel.getByRole("status")).toContainText(
    "本次管理登录已完成",
  );
  await expect(page.getByRole("alert")).toContainText("写入结果未知");
  await expect(save).toBeDisabled();
  await page
    .getByRole("button", { name: "读取最新版本（保留输入）", exact: true })
    .click();
  await expect(page.getByText("当前服务端：", { exact: false })).toBeVisible();
  expect(persisted(id).revision).toBe(1);
  expect(writes).toBe(1);
});

test("confirmed logout revokes the real session and clears write capability", async ({
  page,
}) => {
  await login(page);
  const panel = page.getByRole("region", { name: "管理登录", exact: true });
  await panel
    .getByRole("button", { name: "退出管理会话", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("服务端已确认退出");
  await expect(
    page.getByRole("button", { name: "创建来源", exact: true }),
  ).toBeDisabled();
  const response = await page.request.get("/api/admin/v1/sources");
  expect(response.status()).toBe(403);
});

test("duplicate widget callbacks create only one real session", async ({
  page,
}) => {
  let posts = 0;
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      request.url().endsWith("/api/admin/v1/session")
    )
      posts++;
  });
  await login(page, "duplicate");
  expect(posts).toBe(1);
});

for (const mode of ["expired", "error"] as const) {
  test(`challenge ${mode} stays unauthenticated until explicit retry`, async ({
    page,
  }) => {
    let posts = 0;
    page.on("request", (request) => {
      if (
        request.method() === "POST" &&
        request.url().endsWith("/api/admin/v1/session")
      )
        posts++;
    });
    await widget(page, mode);
    await page.goto("/admin/");
    const panel = page.getByRole("region", { name: "管理登录", exact: true });
    await panel
      .getByRole("button", { name: "验证管理身份", exact: true })
      .click();
    await panel
      .getByRole("button", { name: "完成离线测试挑战", exact: true })
      .click();
    await expect(panel.getByRole("status")).toContainText(
      mode === "expired" ? "人机验证已过期" : "人机验证未完成",
    );
    expect(posts).toBe(0);
    await expect(
      page.getByRole("button", { name: "创建来源", exact: true }),
    ).toBeDisabled();
    await panel
      .getByRole("button", { name: "验证管理身份", exact: true })
      .click();
    await panel
      .getByRole("button", { name: "完成离线测试挑战", exact: true })
      .click();
    await expect(panel.getByRole("status")).toContainText("本次管理登录已完成");
    expect(posts).toBe(1);
  });
}

test("real source form fits a mobile browser", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await login(page);
  await create(page);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: testInfo.outputPath("sources-real.png"),
    fullPage: true,
  });
});

test("late anonymous denial cannot replace a completed real login and list", async ({
  page,
}) => {
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let delayed = false;
  await page.route("**/api/admin/v1/sources", async (route) => {
    const response = await route.fetch({ maxRedirects: 0 });
    if (response.status() === 403 && !delayed) {
      delayed = true;
      await held;
    }
    await route.fulfill({ response });
  });
  await login(page);
  await expect.poll(() => delayed).toBe(true);
  await expect(sourceStatus(page)).toContainText("来源列表已读取");
  const oldResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/api/admin/v1/sources") &&
      response.status() === 403,
  );
  release?.();
  expect((await oldResponse).status()).toBe(403);
  await create(page);
  await expect(sourceStatus(page)).toContainText("服务端已确认保存配置版本 1");
});

test("real API preserves millisecond timings during unrelated edits", async ({
  page,
}) => {
  await login(page);
  const id = await draft(page);
  await page.getByLabel("刷新间隔（秒）", { exact: true }).fill("60.001");
  await page.getByLabel("新鲜度 SLA", { exact: false }).fill("300.001");
  await page.getByRole("button", { name: "保存禁用来源", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("配置版本 1");
  await page.getByLabel("名称", { exact: true }).fill("仅重命名毫秒配置");
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(sourceStatus(page)).toContainText("配置版本 2");
  const response = await page.request.get(`/api/admin/v1/sources/${id}`);
  expect(response.status()).toBe(200);
  const record = await response.json();
  expect(record.configuration.refresh_interval_ms).toBe(60001);
  expect(record.configuration.freshness_sla_ms).toBe(300001);
});
