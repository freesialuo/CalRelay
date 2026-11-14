import { expect, type Page, test } from "@playwright/test";
import type { SourceConfiguration, SourceRecord } from "../src/sources-api";

const fixture = "/admin/tests/fixture/sources.html";
const config = (): SourceConfiguration => ({
  version: "v1",
  name: "测试来源",
  enabled: false,
  adapter: "ics",
  mode: "pull",
  endpoint: "https://calendar.example/events.ics",
  credential_id: null,
  refresh_interval_ms: 60000,
  freshness_sla_ms: 300000,
  capabilities: {
    snapshot: true,
    delta: false,
    upsert: true,
    delete: false,
    conditional_read: false,
    pagination: false,
  },
  calendars: [
    {
      id: "calendar_a",
      external_id: "primary",
      selected: true,
      timezone: "UTC",
    },
  ],
});
function backend(page: Page, initial?: SourceRecord) {
  let record = initial;
  let nonce = 1;
  const requests: {
    method: string;
    headers: Record<string, string>;
    body: unknown;
  }[] = [];
  return {
    requests,
    get record() {
      return record;
    },
    set record(value: SourceRecord | undefined) {
      record = value;
    },
    async install() {
      await page.route("**/api/admin/v1/session", (route) =>
        route.fulfill({
          json: {
            csrf_token: "test_csrf",
            write_nonce: "test_nonce_1",
            expires_at: Math.floor(Date.now() / 1000) + 600,
          },
        }),
      );
      await page.route("**/api/admin/v1/sources**", async (route) => {
        const request = route.request();
        const method = request.method();
        const url = new URL(request.url());
        if (method === "GET") {
          if (url.pathname.endsWith("/sources"))
            return route.fulfill({
              json: {
                items: record
                  ? [
                      {
                        source_id: record.source_id,
                        name: record.configuration.name,
                        enabled: record.configuration.enabled,
                        config_revision: record.config_revision,
                        source_revision: record.source_revision,
                        retired_at: record.retired_at,
                      },
                    ]
                  : [],
                next_after: null,
              },
            });
          return record
            ? route.fulfill({
                json: record,
                headers: { etag: `"${record.config_revision}"` },
              })
            : route.fulfill({ status: 404 });
        }
        requests.push({
          method,
          headers: request.headers(),
          body: request.postDataJSON(),
        });
        const headers = { "x-admin-write-nonce": `test_nonce_${++nonce}` };
        if (request.headers()["x-csrf-token"] !== "test_csrf")
          return route.fulfill({ status: 403 });
        if (
          (record?.retired_at !== null && record !== undefined) ||
          (record &&
            request.headers()["if-match"] !== `"${record.config_revision}"`)
        )
          return route.fulfill({ status: 409, headers });
        if (method === "DELETE" && record) {
          record = {
            ...record,
            retired_at: Date.now(),
            config_revision: record.config_revision + 1,
            source_revision: record.source_revision + 1,
          };
          return route.fulfill({ status: 204, headers });
        }
        const created = !record;
        const id = url.pathname.split("/").at(-1) ?? "";
        record = {
          source_id: id,
          configuration: request.postDataJSON() as SourceConfiguration,
          config_revision: record ? record.config_revision + 1 : 1,
          source_revision: record ? record.source_revision + 1 : 1,
          retired_at: null,
        };
        return route.fulfill({
          status: created ? 201 : 200,
          json: record,
          headers: { ...headers, etag: `"${record.config_revision}"` },
        });
      });
    },
  };
}
async function createDraft(page: Page) {
  await page.getByRole("button", { name: "创建来源", exact: true }).click();
  await page.getByLabel("稳定来源 ID").fill("source_a");
  await page.getByLabel("名称", { exact: true }).fill("测试来源");
  await page
    .getByLabel("ICS HTTPS 地址", { exact: false })
    .fill("https://calendar.example/events.ics");
  await page.getByLabel("远端日历 external_id").fill("primary");
}

test("empty instance creates disabled source, enables, edits and retires", async ({
  page,
}) => {
  const api = backend(page);
  await api.install();
  await page.goto(fixture);
  await expect(
    page.getByText("尚无托管来源。", { exact: false }),
  ).toBeVisible();
  await createDraft(page);
  await expect(page.getByLabel("启用来源", { exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "保存禁用来源", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(
    "服务端已确认保存配置版本 1",
  );
  expect(api.requests[0].headers["if-none-match"]).toBe("*");
  expect(api.record?.configuration.enabled).toBe(false);
  await page.getByLabel("启用来源", { exact: true }).check();
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("配置版本 2");
  expect(api.requests[1].headers["if-match"]).toBe('"1"');
  expect(api.requests[1].headers["x-admin-write-nonce"]).toBe("test_nonce_2");
  await page.getByLabel("名称", { exact: true }).fill("重命名来源");
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("配置版本 3");
  await page.getByRole("button", { name: "退休来源", exact: true }).click();
  await page.getByRole("button", { name: "确认永久退休", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("服务端已确认永久退休");
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
  await page.reload();
  await page.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await expect(
    page.getByText("此来源已永久退休", { exact: false }),
  ).toBeVisible();
  await expect(page.getByLabel("启用来源", { exact: true })).toBeDisabled();
  expect(
    api.requests.filter((request) => request.method === "DELETE"),
  ).toHaveLength(1);
});

test("CAS conflict preserves draft and only explicit adoption changes revision", async ({
  page,
}) => {
  const api = backend(page, {
    source_id: "source_a",
    config_revision: 1,
    source_revision: 1,
    retired_at: null,
    configuration: config(),
  });
  await api.install();
  await page.goto(fixture);
  await page.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await page.getByLabel("名称", { exact: true }).fill("本地草稿");
  if (!api.record) throw new Error("Missing test source");
  api.record = {
    ...api.record,
    config_revision: 2,
    source_revision: 2,
    configuration: { ...config(), name: "服务端新版本" },
  };
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("配置版本冲突");
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "本地草稿",
  );
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "读取最新版本（保留输入）", exact: true })
    .click();
  await expect(
    page.getByText("当前服务端：服务端新版本", { exact: false }),
  ).toBeVisible();
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "本地草稿",
  );
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("button", {
      name: "采用最新版本重新编辑（放弃原输入）",
      exact: true,
    })
    .click();
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "服务端新版本",
  );
});

test("static and push-only forms have no poll interval, malicious names stay text", async ({
  page,
}) => {
  const initial = {
    source_id: "source_a",
    config_revision: 1,
    source_revision: 1,
    retired_at: null,
    configuration: {
      ...config(),
      name: '<img src=x onerror="window.pwned=1">',
    },
  };
  const api = backend(page, initial);
  await api.install();
  await page.goto(fixture);
  await expect(page.locator("img")).toHaveCount(0);
  await page.getByRole("button", { name: "创建来源", exact: true }).click();
  await page
    .getByLabel("来源类型与更新方式")
    .selectOption("standard-calendar:push-only");
  await expect(page.getByLabel("刷新间隔（秒）", { exact: true })).toBeHidden();
  await page.getByLabel("来源类型与更新方式").selectOption("ics:manual-static");
  await expect(page.getByLabel("新鲜度 SLA", { exact: false })).toBeHidden();
});

for (const status of [403, 409, 422]) {
  test(`source write HTTP ${status} never claims save success`, async ({
    page,
  }) => {
    const api = backend(page);
    await api.install();
    await page.route(
      "**/api/admin/v1/sources/source_a",
      async (route, request) => {
        if (request.method() === "PUT")
          return route.fulfill({
            status,
            headers: { "x-admin-write-nonce": "test_nonce_rejected" },
            json: { private_details: "do-not-render-this" },
          });
        return route.fallback();
      },
    );
    await page.goto(fixture);
    await createDraft(page);
    await page
      .getByRole("button", { name: "保存禁用来源", exact: true })
      .click();
    await expect(page.getByRole("status")).toHaveAttribute(
      "data-state",
      "error",
    );
    await expect(page.getByRole("status")).not.toContainText("确认保存");
    await expect(page.getByRole("status")).not.toContainText(
      "do-not-render-this",
    );
    await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
      "测试来源",
    );
    if (status !== 422)
      await expect(
        page.getByRole("button", { name: "保存禁用来源", exact: true }),
      ).toBeDisabled();
    expect(api.record).toBeUndefined();
  });
}

test("cancelled write is unknown, blocks duplicates and is read back explicitly", async ({
  page,
}) => {
  const api = backend(page);
  await api.install();
  let requests = 0;
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(
    "**/api/admin/v1/sources/source_a",
    async (route, request) => {
      if (request.method() !== "PUT") return route.fallback();
      requests++;
      api.record = {
        source_id: "source_a",
        config_revision: 1,
        source_revision: 1,
        retired_at: null,
        configuration: config(),
      };
      await held;
      await route
        .fulfill({
          status: 201,
          json: api.record,
          headers: { etag: '"1"', "x-admin-write-nonce": "late_nonce" },
        })
        .catch(() => {});
    },
  );
  await page.goto(fixture);
  await createDraft(page);
  const save = page.getByRole("button", { name: "保存禁用来源", exact: true });
  await save.click();
  await expect(save).toBeDisabled();
  await expect.poll(() => requests).toBe(1);
  await page
    .getByRole("button", { name: "取消等待并核实结果", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText("写入结果未知");
  release?.();
  await expect(save).toBeDisabled();
  await page
    .getByRole("button", { name: "读取最新版本（保留输入）", exact: true })
    .click();
  await expect(
    page.getByText("当前服务端：测试来源", { exact: false }),
  ).toBeVisible();
  expect(requests).toBe(1);
});

test("cancel and return asks before discarding unsaved inputs", async ({
  page,
}) => {
  const api = backend(page);
  await api.install();
  await page.goto(fixture);
  await createDraft(page);
  page.once("dialog", (dialog) => dialog.dismiss());
  await page
    .getByRole("button", { name: "取消 / 返回列表", exact: true })
    .click();
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "测试来源",
  );
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("button", { name: "取消 / 返回列表", exact: true })
    .click();
  await expect(page.getByLabel("名称", { exact: true })).toHaveCount(0);
  expect(api.requests).toHaveLength(0);
});

test("late detail response cannot replace the source selected afterwards", async ({
  page,
}) => {
  await backend(page).install();
  const a = {
    source_id: "source_a",
    config_revision: 1,
    source_revision: 1,
    retired_at: null,
    configuration: config(),
  };
  const b = {
    ...a,
    source_id: "source_b",
    configuration: { ...config(), name: "第二来源" },
  };
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/admin/v1/sources**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith("/sources"))
      return route.fulfill({
        json: {
          items: [a, b].map((record) => ({
            source_id: record.source_id,
            name: record.configuration.name,
            config_revision: 1,
            source_revision: 1,
            enabled: false,
            retired_at: null,
          })),
          next_after: null,
        },
      });
    const record = url.pathname.endsWith("source_a") ? a : b;
    if (record === a) await held;
    return route.fulfill({ json: record, headers: { etag: '"1"' } });
  });
  await page.goto(fixture);
  await page
    .locator("section")
    .filter({
      has: page.getByRole("heading", { name: "测试来源", exact: true }),
    })
    .getByRole("button", { name: "查看 / 编辑", exact: true })
    .click();
  await page
    .locator("section")
    .filter({
      has: page.getByRole("heading", { name: "第二来源", exact: true }),
    })
    .getByRole("button", { name: "查看 / 编辑", exact: true })
    .click();
  await expect(page.getByLabel("稳定来源 ID")).toHaveValue("source_b");
  release?.();
  await expect(page.getByLabel("名称", { exact: true })).toHaveValue(
    "第二来源",
  );
});

test("mobile source form fits the viewport", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 375, height: 812 });
  const api = backend(page);
  await api.install();
  await page.goto(fixture);
  await createDraft(page);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: testInfo.outputPath("source-form.png"),
    fullPage: true,
  });
});

test("the final required calendar mapping cannot be removed", async ({
  page,
}) => {
  await backend(page).install();
  await page.goto(fixture);
  await createDraft(page);
  const remove = page.getByRole("button", {
    name: "移除此未保存映射",
    exact: true,
  });
  await expect(remove).toBeDisabled();
  await page
    .getByLabel("来源类型与更新方式")
    .selectOption("standard-calendar:push-only");
  await page.getByRole("button", { name: "添加日历映射", exact: true }).click();
  await expect(remove.first()).toBeEnabled();
  await remove.last().click();
  await expect(remove).toBeDisabled();
});

for (const result of ["success", "failure"]) {
  test(`pending detail transition locks the outgoing editor and restores on ${result}`, async ({
    page,
  }) => {
    await backend(page).install();
    const a = {
      source_id: "source_a",
      config_revision: 1,
      source_revision: 1,
      retired_at: null,
      configuration: config(),
    };
    const b = {
      ...a,
      source_id: "source_b",
      configuration: { ...config(), name: "第二来源" },
    };
    let release: (() => void) | undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route("**/api/admin/v1/sources**", async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname.endsWith("/sources"))
        return route.fulfill({
          json: {
            items: [a, b].map((record) => ({
              source_id: record.source_id,
              name: record.configuration.name,
              config_revision: 1,
              source_revision: 1,
              enabled: false,
              retired_at: null,
            })),
            next_after: null,
          },
        });
      if (url.pathname.endsWith("source_b")) {
        await held;
        if (result === "failure") return route.fulfill({ status: 500 });
        return route.fulfill({ json: b, headers: { etag: '"1"' } });
      }
      return route.fulfill({ json: a, headers: { etag: '"1"' } });
    });
    await page.goto(fixture);
    const row = (name: string) =>
      page
        .locator("section")
        .filter({ has: page.getByRole("heading", { name, exact: true }) });
    await row("测试来源")
      .getByRole("button", { name: "查看 / 编辑", exact: true })
      .click();
    await expect(page.getByLabel("稳定来源 ID")).toHaveValue("source_a");
    await row("第二来源")
      .getByRole("button", { name: "查看 / 编辑", exact: true })
      .click();
    await expect(page.getByLabel("名称", { exact: true })).toBeDisabled();
    await expect(
      page.getByRole("button", {
        name: "保存配置",
        exact: true,
        includeHidden: true,
      }),
    ).toBeDisabled();
    release?.();
    await expect(page.getByLabel("名称", { exact: true })).toBeEnabled();
    await expect(page.getByLabel("稳定来源 ID")).toHaveValue(
      result === "success" ? "source_b" : "source_a",
    );
  });
}

test("unrelated edits preserve valid millisecond interval and SLA precision", async ({
  page,
}) => {
  const api = backend(page, {
    source_id: "source_a",
    config_revision: 1,
    source_revision: 1,
    retired_at: null,
    configuration: {
      ...config(),
      refresh_interval_ms: 60001,
      freshness_sla_ms: 300001,
    },
  });
  await api.install();
  await page.goto(fixture);
  await page.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  const interval = page.getByLabel("刷新间隔（秒）", { exact: true });
  await expect(interval).toHaveValue("60.001");
  expect(
    await interval.evaluate((input: HTMLInputElement) => input.validity.valid),
  ).toBe(true);
  await page.getByLabel("名称", { exact: true }).fill("仅重命名");
  await page.getByRole("button", { name: "保存配置", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("配置版本 2");
  expect(api.record?.configuration.refresh_interval_ms).toBe(60001);
  expect(api.record?.configuration.freshness_sla_ms).toBe(300001);
});

// Refresh and ICS upload error branches.
const pullConfig = (): SourceConfiguration => ({ ...config(), enabled: true });
const staticConfig = (): SourceConfiguration => ({
  ...config(),
  enabled: true,
  mode: "manual-static",
  endpoint: null,
  refresh_interval_ms: null,
  freshness_sla_ms: null,
});
const refreshSuccess = "服务端已记录刷新请求";
const uploadSuccess = "服务端已确认替换快照";
const noAuthorization = "当前没有可用的管理写入授权";
const authorizationMissing = (page: Page) =>
  page.getByText(noAuthorization, { exact: false });
const validReceipt = { source_id: "source_a", requested_at: 1 };
const snapshotReceipt = {
  source_id: "source_a",
  source_revision: 2,
  changed_events: 3,
};
const icsFile = (buffer: Buffer = Buffer.from("BEGIN:VCALENDAR\r\n")) => ({
  name: "a.ics",
  mimeType: "text/calendar",
  buffer,
});

type Answer = (
  url: URL,
  call: number,
) => Promise<{
  status: number;
  json?: unknown;
  headers?: Record<string, string>;
}>;
async function writeBackend(
  page: Page,
  configuration: SourceConfiguration,
  answer: Answer,
) {
  const base = backend(page, {
    source_id: "source_a",
    config_revision: 1,
    source_revision: 1,
    retired_at: null,
    configuration,
  });
  await base.install();
  const calls: URL[] = [];
  await page.route(
    /\/api\/admin\/v1\/sources\/source_a\/(refresh|snapshot)/,
    async (route) => {
      const url = new URL(route.request().url());
      calls.push(url);
      const reply = await answer(url, calls.length);
      await route
        .fulfill({
          ...reply,
          headers: {
            "x-admin-write-nonce": `rotated_nonce_${calls.length}`,
            ...reply.headers,
          },
        })
        .catch(() => {});
    },
  );
  await page.goto(fixture);
  await page.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("已读取配置版本 1");
  return calls;
}
const refreshButton = (page: Page) =>
  page.getByRole("button", { name: "请求立即刷新", exact: true });
const uploadButton = (page: Page) =>
  page.getByRole("button", { name: "上传并替换快照", exact: true });
const chooseFile = (page: Page, buffer?: Buffer) =>
  page.getByLabel("ICS 文件", { exact: false }).setInputFiles(icsFile(buffer));
const hold = () => {
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { held, release: () => release() };
};

test("refresh button only for enabled pull sources, upload only for enabled manual-static ICS", async ({
  page,
}) => {
  await writeBackend(page, pullConfig(), async () => ({ status: 202 }));
  await expect(refreshButton(page)).toBeVisible();
  await expect(uploadButton(page)).toHaveCount(0);
});

test("refresh and upload controls are absent for disabled sources", async ({
  page,
}) => {
  await writeBackend(page, config(), async () => ({ status: 202 }));
  await expect(refreshButton(page)).toHaveCount(0);
  await expect(uploadButton(page)).toHaveCount(0);
});

test("push-only sources have neither refresh nor upload control", async ({
  page,
}) => {
  await writeBackend(
    page,
    { ...staticConfig(), mode: "push-only" },
    async () => ({ status: 200 }),
  );
  await expect(refreshButton(page)).toHaveCount(0);
  await expect(uploadButton(page)).toHaveCount(0);
});

test("upload control only for enabled manual-static ICS sources", async ({
  page,
}) => {
  await writeBackend(page, staticConfig(), async () => ({ status: 200 }));
  await expect(uploadButton(page)).toBeVisible();
  await expect(refreshButton(page)).toHaveCount(0);
});

test("static source with a non-ICS adapter has no upload control", async ({
  page,
}) => {
  await writeBackend(
    page,
    { ...staticConfig(), adapter: "standard-calendar" },
    async () => ({ status: 200 }),
  );
  await expect(uploadButton(page)).toHaveCount(0);
});

test("refresh HTTP 409 reports conflict, never claims success and blocks the editor", async ({
  page,
}) => {
  const calls = await writeBackend(page, pullConfig(), async () => ({
    status: 409,
  }));
  await refreshButton(page).click();
  const status = page.getByRole("status");
  await expect(status).toHaveAttribute("data-state", "error");
  await expect(status).toHaveText(
    "来源当前不是已启用的可拉取来源，未记录刷新请求。请重新读取来源核实。",
  );
  await expect(status).not.toContainText(refreshSuccess);
  await expect(refreshButton(page)).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "保存配置", exact: true }),
  ).toBeDisabled();
  expect(calls).toHaveLength(1);
});

test("refresh HTTP 403 is denied, never claims success and clears write authorization", async ({
  page,
}) => {
  await writeBackend(page, pullConfig(), async () => ({ status: 403 }));
  await refreshButton(page).click();
  const status = page.getByRole("status");
  await expect(status).toHaveAttribute("data-state", "error");
  await expect(status).toHaveText(
    "管理请求被拒绝。请重新登录并检查会话、CSRF、Origin 与权限。",
  );
  await expect(status).not.toContainText(refreshSuccess);
  await expect(authorizationMissing(page)).toBeVisible();
  await expect(refreshButton(page)).toBeDisabled();
});

for (const [name, body] of [
  ["wrong source_id", { ...validReceipt, source_id: "source_b" }],
  ["extra key", { ...validReceipt, extra: true }],
  ["non-integer requested_at", { ...validReceipt, requested_at: 1.5 }],
] as const) {
  test(`refresh 202 with invalid receipt (${name}) clears write authorization`, async ({
    page,
  }) => {
    await writeBackend(page, pullConfig(), async () => ({
      status: 202,
      json: body,
    }));
    await expect(authorizationMissing(page)).toHaveCount(0);
    await refreshButton(page).click();
    const status = page.getByRole("status");
    await expect(status).toHaveAttribute("data-state", "error");
    await expect(status).toHaveText(
      "响应不符合刷新回执契约。未确认请求已记录，请稍后在诊断中核实。",
    );
    await expect(status).not.toContainText(refreshSuccess);
    await expect(authorizationMissing(page)).toBeVisible();
    await expect(refreshButton(page)).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "保存配置", exact: true }),
    ).toBeDisabled();
  });
}

test("refresh 202 with a valid receipt claims only a recorded request", async ({
  page,
}) => {
  await writeBackend(page, pullConfig(), async () => ({
    status: 202,
    json: validReceipt,
  }));
  await refreshButton(page).click();
  await expect(page.getByRole("status")).toContainText(refreshSuccess);
  await expect(authorizationMissing(page)).toHaveCount(0);
});

test("upload HTTP 409 reports conflict, never claims success and blocks the editor", async ({
  page,
}) => {
  const calls = await writeBackend(page, staticConfig(), async () => ({
    status: 409,
  }));
  await chooseFile(page);
  await uploadButton(page).click();
  const status = page.getByRole("status");
  await expect(status).toHaveAttribute("data-state", "error");
  await expect(status).toHaveText(
    "来源不是已启用的 ICS 静态来源，或配置已变化；既有事件未改变。请重新读取来源核实。",
  );
  await expect(status).not.toContainText(uploadSuccess);
  await expect(uploadButton(page)).toBeDisabled();
  expect(calls).toHaveLength(1);
});

for (const code of [413, 415, 422]) {
  test(`upload HTTP ${code} is rejected without claiming success and the editor stays usable`, async ({
    page,
  }) => {
    const calls = await writeBackend(page, staticConfig(), async () => ({
      status: code,
    }));
    await chooseFile(page);
    await uploadButton(page).click();
    const status = page.getByRole("status");
    await expect(status).toHaveAttribute("data-state", "error");
    await expect(status).toHaveText("ICS 无效或超出预算；既有事件未改变。");
    await expect(status).not.toContainText(uploadSuccess);
    await expect(uploadButton(page)).toBeEnabled();
    expect(calls).toHaveLength(1);
  });
}

test("upload HTTP 200 with an invalid receipt clears write authorization", async ({
  page,
}) => {
  await writeBackend(page, staticConfig(), async () => ({
    status: 200,
    json: { ...snapshotReceipt, source_id: "source_b" },
  }));
  await chooseFile(page);
  await uploadButton(page).click();
  const status = page.getByRole("status");
  await expect(status).toHaveAttribute("data-state", "error");
  await expect(status).toHaveText(
    "响应不符合快照回执契约。未确认是否已替换，请在诊断中核实，勿重复上传。",
  );
  await expect(status).not.toContainText(uploadSuccess);
  await expect(authorizationMissing(page)).toBeVisible();
  await expect(uploadButton(page)).toBeDisabled();
});

test("upload HTTP 200 with a valid receipt reports the replacement", async ({
  page,
}) => {
  await writeBackend(page, staticConfig(), async () => ({
    status: 200,
    json: snapshotReceipt,
  }));
  await chooseFile(page);
  await uploadButton(page).click();
  await expect(page.getByRole("status")).toContainText(
    "来源修订 2，变更 3 个事件",
  );
});

test("a file larger than 1 MiB is refused locally without any request", async ({
  page,
}) => {
  const calls = await writeBackend(page, staticConfig(), async () => ({
    status: 200,
    json: snapshotReceipt,
  }));
  await chooseFile(page, Buffer.alloc(1_048_577, "a"));
  await uploadButton(page).click();
  const status = page.getByRole("status");
  await expect(status).toHaveAttribute("data-state", "error");
  await expect(status).toHaveText("ICS 文件超过 1 MiB，未上传。");
  await expect(status).not.toContainText(uploadSuccess);
  expect(calls).toHaveLength(0);
});

test("an invalid UTF-8 file is refused locally without any request", async ({
  page,
}) => {
  const calls = await writeBackend(page, staticConfig(), async () => ({
    status: 200,
    json: snapshotReceipt,
  }));
  await chooseFile(page, Buffer.from([0x42, 0xff, 0xfe, 0x41]));
  await uploadButton(page).click();
  const status = page.getByRole("status");
  await expect(status).toHaveAttribute("data-state", "error");
  await expect(status).toHaveText(
    "文件不是有效的 UTF-8 文本，未上传；既有事件未改变。",
  );
  await expect(status).not.toContainText(uploadSuccess);
  expect(calls).toHaveLength(0);
});

test("upload without a chosen file asks for one and sends nothing", async ({
  page,
}) => {
  const calls = await writeBackend(page, staticConfig(), async () => ({
    status: 200,
  }));
  await uploadButton(page).click();
  await expect(page.getByRole("status")).toHaveText("请先选择 ICS 文件。");
  expect(calls).toHaveLength(0);
});

test("upload 428 then 保留既有事件 sends no second request and hides the prompt", async ({
  page,
}) => {
  const calls = await writeBackend(page, staticConfig(), async () => ({
    status: 428,
  }));
  await chooseFile(page);
  await uploadButton(page).click();
  const approve = page.getByRole("button", {
    name: "确认删除并替换",
    exact: true,
  });
  await expect(approve).toBeVisible();
  await expect(page.getByRole("status")).toHaveText(
    "需要确认：此文件会删除超过 100 个既有事件；既有事件未改变。",
  );
  await page.getByRole("button", { name: "保留既有事件", exact: true }).click();
  await expect(approve).toBeHidden();
  await expect(page.getByRole("status")).toHaveText("已取消；既有事件未改变。");
  await expect(page.getByRole("status")).not.toContainText(uploadSuccess);
  expect(calls).toHaveLength(1);
  expect(calls[0].search).toBe("");
});

test("upload 428 then confirm sends the approved request, which may fail with 409", async ({
  page,
}) => {
  const calls = await writeBackend(
    page,
    staticConfig(),
    async (_url, call) => ({ status: call === 1 ? 428 : 409 }),
  );
  await chooseFile(page);
  await uploadButton(page).click();
  await page
    .getByRole("button", { name: "确认删除并替换", exact: true })
    .click();
  const status = page.getByRole("status");
  await expect(status).toHaveText(
    "来源不是已启用的 ICS 静态来源，或配置已变化；既有事件未改变。请重新读取来源核实。",
  );
  await expect(status).not.toContainText(uploadSuccess);
  await expect(
    page.getByRole("button", { name: "确认删除并替换", exact: true }),
  ).toBeHidden();
  await expect(uploadButton(page)).toBeDisabled();
  expect(calls.map((url) => url.search)).toEqual([
    "",
    "?approve_deletions=true",
  ]);
});

for (const kind of ["refresh", "upload"] as const) {
  test(`cancelling a pending ${kind} reports an unknown result and blocks the editor`, async ({
    page,
  }) => {
    const gate = hold();
    const calls = await writeBackend(
      page,
      kind === "refresh" ? pullConfig() : staticConfig(),
      async () => {
        await gate.held;
        return kind === "refresh"
          ? { status: 202, json: validReceipt }
          : { status: 200, json: snapshotReceipt };
      },
    );
    if (kind === "refresh") await refreshButton(page).click();
    else {
      await chooseFile(page);
      await uploadButton(page).click();
    }
    await expect.poll(() => calls.length).toBe(1);
    await page
      .getByRole("button", { name: "取消等待并核实结果", exact: true })
      .click();
    const status = page.getByRole("status");
    await expect(status).toHaveAttribute("data-state", "error");
    await expect(status).toContainText("写入结果未知");
    gate.release();
    await expect(status).toContainText("写入结果未知");
    await expect(status).not.toContainText(
      kind === "refresh" ? refreshSuccess : uploadSuccess,
    );
    await expect(authorizationMissing(page)).toBeVisible();
    await expect(
      kind === "refresh" ? refreshButton(page) : uploadButton(page),
    ).toBeDisabled();
    expect(calls).toHaveLength(1);
  });
}

test("new sources cannot pick the standard-calendar manual type, which has no write path", async ({
  page,
}) => {
  await backend(page).install();
  await page.goto(fixture);
  await page.getByRole("button", { name: "创建来源", exact: true }).click();
  const mode = page.getByLabel("来源类型与更新方式");
  await expect(mode.locator("option")).toHaveText([
    "ICS URL（定期读取）",
    "ICS 静态 / 手动（启用后在来源详情上传文件）",
    "外部 producer 推送",
  ]);
});

test("an existing standard-calendar manual source keeps its type and says it has no write path", async ({
  page,
}) => {
  await backend(page, {
    source_id: "source_a",
    config_revision: 1,
    source_revision: 1,
    retired_at: null,
    configuration: {
      ...config(),
      adapter: "standard-calendar",
      mode: "manual-static",
      endpoint: null,
      refresh_interval_ms: null,
      freshness_sla_ms: null,
    },
  }).install();
  await page.goto(fixture);
  await page.getByRole("button", { name: "查看 / 编辑", exact: true }).click();
  const mode = page.getByLabel("来源类型与更新方式");
  await expect(mode).toHaveValue("standard-calendar:manual-static");
  await expect(mode.locator("option:checked")).toHaveText(
    "标准日历手动来源（尚无写入入口，不会产生事件）",
  );
});

test("an ICS source maps exactly one calendar and a second mapping is refused before sending", async ({
  page,
}) => {
  const api = backend(page);
  await api.install();
  await page.goto(fixture);
  await createDraft(page);
  const add = page.getByRole("button", { name: "添加日历映射", exact: true });
  const mode = page.getByLabel("来源类型与更新方式");
  await expect(add).toBeHidden();
  await mode.selectOption("standard-calendar:push-only");
  await add.click();
  await page.getByLabel("远端日历 external_id").last().fill("second");
  await mode.selectOption("ics:manual-static");
  await expect(add).toBeHidden();
  await page.getByRole("button", { name: "保存禁用来源", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText(
    "ICS 来源只能映射一个日历：一个 ICS 文档对应一个日历。请移除多余映射。",
  );
  expect(api.requests.filter((r) => r.method !== "GET")).toHaveLength(0);
});
