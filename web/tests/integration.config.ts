import { fileURLToPath } from "node:url";
import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: ".",
  testMatch: "*integration.spec.ts",
  workers: 1,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  timeout: 25000,
  use: {
    baseURL: "https://127.0.0.1:4175",
    ignoreHTTPSErrors: true,
    browserName: "chromium",
    launchOptions: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH }
      : {},
  },
  webServer: {
    cwd: fileURLToPath(new URL("../", import.meta.url)),
    command: "node tests/support/admin-preview.mjs",
    url: "https://127.0.0.1:4175/admin/",
    ignoreHTTPSErrors: true,
    reuseExistingServer: false,
    timeout: 30000,
    gracefulShutdown: { signal: "SIGTERM", timeout: 6000 },
  },
});
