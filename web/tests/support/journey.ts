import { expect, type Page } from "@playwright/test";
export const apiURL =
  "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";

export async function widget(
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

export async function login(page: Page, mode: "valid" | "duplicate" = "valid") {
  await widget(page, mode);
  const pageResponse = await page.goto("/admin/");
  expect(pageResponse?.headers()["content-security-policy"]).toContain(
    "form-action 'none'",
  );
  expect(pageResponse?.headers()["referrer-policy"]).toBe("no-referrer");
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

export const status = (page: Page) => page.locator("main > p[role=status]");

export async function source(
  page: Page,
  mode: "ics:manual-static" | "ics:pull" = "ics:manual-static",
  endpoint?: string,
  conditional = false,
) {
  await page.getByRole("link", { name: "来源", exact: true }).click();
  await page.getByRole("button", { name: "创建来源", exact: true }).click();
  const id = `source_${crypto.randomUUID().replaceAll("-", "")}`;
  await page.getByLabel("稳定来源 ID").fill(id);
  const calendar = await page
    .getByLabel("日历 ID", { exact: true })
    .inputValue();
  await page.getByLabel("名称", { exact: true }).fill("静态测试来源");
  await page.getByLabel("来源类型与更新方式").selectOption(mode);
  if (endpoint)
    await page.getByLabel("ICS HTTPS 地址", { exact: false }).fill(endpoint);
  if (mode === "ics:pull") {
    const checkbox = page.getByLabel("条件读取", { exact: false });
    if (conditional) await checkbox.check();
    else await checkbox.uncheck();
  }
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

export async function subscriptionDraft(page: Page) {
  await page.getByRole("link", { name: "汇总订阅", exact: true }).click();
  await page.getByRole("button", { name: "创建订阅", exact: true }).click();
  const id = await page.getByLabel("稳定订阅 ID").inputValue();
  await page.getByLabel("名称", { exact: true }).fill("只读汇总");
  return id;
}

export async function saveSubscription(page: Page) {
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

export async function dismissSecret(page: Page) {
  await page
    .getByRole("button", { name: "已保存，清除本页链接", exact: true })
    .click();
}

export async function output(page: Page) {
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

export async function openReminder(page: Page, id: string) {
  const row = page
    .locator("main section")
    .filter({ has: page.locator("p").filter({ hasText: `ID：${id} ·` }) });
  await row.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await expect(page.getByLabel(/^稳定.* ID$/)).toHaveValue(id);
}

export async function reauthenticate(page: Page) {
  const panel = page.getByRole("region", { name: "管理登录", exact: true });
  await panel
    .getByRole("button", { name: "验证管理身份", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "完成离线测试挑战", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("本次管理登录已完成");
}
