import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./tests",
  testIgnore: ["**/sources.spec.ts", "**/*integration.spec.ts", "**/system/**"],
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 15000,
  use: {
    baseURL: "http://127.0.0.1:4173",
    browserName: "chromium",
    launchOptions: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH }
      : {},
  },
  webServer: {
    command: "node scripts/local-preview.mjs",
    url: "http://127.0.0.1:4173/admin/",
    reuseExistingServer: false,
    gracefulShutdown: { signal: "SIGTERM", timeout: 5000 },
    timeout: 30000,
  },
});
