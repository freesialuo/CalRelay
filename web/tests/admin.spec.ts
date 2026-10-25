import { expect, test } from "@playwright/test";

test("configuration journey honestly marks disconnected writes", async ({
  page,
}) => {
  await page.goto("/admin/");
  await expect(page.getByRole("heading", { name: "从来源开始" })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "创建来源", exact: true }),
  ).toBeDisabled();
  await expect(page.getByRole("button", { name: "保存" })).toHaveCount(0);
  await page.getByRole("link", { name: "能力与周期" }).click();
  await expect(page.getByText("静态 / 手动来源不自动轮询")).toBeVisible();
});

test("real local HTTP version, health, optional admin session and refresh", async ({
  page,
}) => {
  const methods: string[] = [];
  page.on("request", (request) => {
    if (/\/(api|health)\//.test(request.url())) methods.push(request.method());
  });
  await page.goto("/admin/#diagnostics");
  // Sections are located by their accessible heading, independent of card layout.
  const card = page
    .locator("section")
    .filter({ has: page.getByRole("heading", { name: "服务版本" }) });
  await card.getByRole("button", { name: "检查", exact: true }).click();
  await expect(card.getByRole("status")).toContainText(
    "CalRelay 0.1.0 · API v1",
  );
  const live = page
    .locator("section")
    .filter({ has: page.getByRole("heading", { name: "HTTP 存活探针" }) });
  await live.getByRole("button", { name: "检查", exact: true }).click();
  await expect(live.getByRole("status")).toContainText("HTTP 探针返回 live");
  const session = page.locator("section").filter({
    has: page.getByRole("heading", { name: "管理会话", exact: true }),
  });
  await session.getByRole("button", { name: "检查", exact: true }).click();
  await expect(session.getByRole("status")).toContainText(
    "此接口未接通或未装配",
  );
  await page.reload();
  await expect(card.getByRole("status")).toHaveText("尚未检查");
  expect(methods).toEqual(["GET", "GET", "GET"]);
  expect(
    await page.evaluate(() => [localStorage.length, sessionStorage.length]),
  ).toEqual([0, 0]);
});

test("malicious text is rendered literally without executing or fetching", async ({
  page,
}) => {
  const malicious = '<img src=x onerror="window.pwned=1">';
  await page.route("**/api/v1/version", (route) =>
    route.fulfill({
      json: { name: "CalRelay", version: malicious, api_version: "v1" },
    }),
  );
  await page.goto("/admin/#diagnostics");
  const card = page.locator("section").filter({ hasText: "服务版本" });
  await card.getByRole("button", { name: "检查", exact: true }).click();
  await expect(card.getByRole("status")).toContainText(malicious);
  await expect(page.locator("img")).toHaveCount(0);
  expect(await page.evaluate(() => "pwned" in window)).toBe(false);
});

for (const status of [401, 403, 404, 409, 429, 503]) {
  test(`HTTP ${status} never appears successful`, async ({ page }) => {
    await page.route("**/api/v1/version", (route) =>
      route.fulfill({
        status,
        json: { error: { code: "untrusted SECRET", request_id: "untrusted" } },
      }),
    );
    await page.goto("/admin/#diagnostics");
    const card = page.locator("section").filter({ hasText: "服务版本" });
    await card.getByRole("button", { name: "检查", exact: true }).click();
    await expect(card.getByRole("status")).toHaveAttribute(
      "data-state",
      "error",
    );
    await expect(card).not.toContainText("SECRET");
  });
}

test("duplicate click, cancellation and navigation ignore late responses", async ({
  page,
}) => {
  let requests = 0;
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/v1/version", async (route) => {
    requests++;
    await held;
    await route
      .fulfill({
        json: { name: "CalRelay", version: "late", api_version: "v1" },
      })
      .catch(() => {});
  });
  await page.goto("/admin/#diagnostics");
  const card = page.locator("section").filter({ hasText: "服务版本" });
  const run = card.getByRole("button", { name: "检查", exact: true });
  await run.click();
  await expect(run).toBeDisabled();
  await expect.poll(() => requests).toBe(1);
  await card.getByRole("button", { name: "取消检查" }).click();
  await expect(card.getByRole("status")).toContainText("检查已取消");
  await expect(run).toBeEnabled();
  release?.();
  await expect(card.getByRole("status")).not.toContainText("late");
  await run.click();
  await page.getByRole("link", { name: "来源", exact: true }).click();
  await page.goBack();
  await expect(card.getByRole("status")).toHaveText("尚未检查");
});

for (const failure of [
  "network",
  "html",
  "malformed",
  "oversized",
  "redirect",
  "unexpected-status",
]) {
  test(`untrusted ${failure} response stays unknown`, async ({ page }) => {
    await page.route("**/api/v1/version", (route) => {
      if (failure === "unexpected-status")
        return route.fulfill({
          status: 201,
          json: { name: "CalRelay", version: "0.1.0", api_version: "v1" },
        });
      if (failure === "network") return route.abort("failed");
      if (failure === "redirect")
        return route.fulfill({ status: 302, headers: { location: "/admin/" } });
      if (failure === "html")
        return route.fulfill({
          contentType: "text/html",
          body: "<h1>login</h1>",
        });
      if (failure === "oversized")
        return route.fulfill({
          json: {
            name: "CalRelay",
            version: "a".repeat(10000),
            api_version: "v1",
          },
        });
      return route.fulfill({ json: { name: "other" } });
    });
    await page.goto("/admin/#diagnostics");
    const card = page.locator("section").filter({ hasText: "服务版本" });
    await card.getByRole("button", { name: "检查", exact: true }).click();
    await expect(card.getByRole("status")).toContainText("未取得可信结果");
  });
}

test("mobile navigation fits viewport", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await page.goto("/admin/");
  await page.getByRole("link", { name: "输出与订阅" }).click();
  await expect(
    page.getByRole("heading", { name: "输出与订阅", exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
});

test("restored browser page clears stale diagnostic evidence", async ({
  page,
}) => {
  await page.goto("/admin/#diagnostics");
  const card = page.locator("section").filter({ hasText: "服务版本" });
  await card.getByRole("button", { name: "检查", exact: true }).click();
  await expect(card.getByRole("status")).toContainText("CalRelay 0.1.0");
  await page.evaluate(() =>
    window.dispatchEvent(
      new PageTransitionEvent("pageshow", { persisted: true }),
    ),
  );
  await expect(card.getByRole("status")).toHaveText("尚未检查");
});

for (const status of [200, 204]) {
  test(`session HTTP ${status} is interpreted according to its contract`, async ({
    page,
  }) => {
    await page.route("**/api/admin/v1/session", (route) =>
      route.fulfill({ status }),
    );
    await page.goto("/admin/#diagnostics");
    const card = page.locator("section").filter({ hasText: "管理会话" });
    await card.getByRole("button", { name: "检查", exact: true }).click();
    await expect(card.getByRole("status")).toContainText(
      status === 204 ? "本次管理身份与会话检查通过" : "未取得可信结果",
    );
  });
}

test("a stalled read has a bounded unknown outcome and can be retried", async ({
  page,
}) => {
  await page.route("**/api/v1/version", () => new Promise(() => {}));
  await page.goto("/admin/#diagnostics");
  const card = page.locator("section").filter({ hasText: "服务版本" });
  await card.getByRole("button", { name: "检查", exact: true }).click();
  await expect(card.getByRole("status")).toContainText("未取得可信结果", {
    timeout: 11000,
  });
  await expect(
    card.getByRole("button", { name: "检查", exact: true }),
  ).toBeEnabled();
});
