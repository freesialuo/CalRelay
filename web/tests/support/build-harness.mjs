import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const child = spawn(
  "cargo",
  [
    "build",
    "-p",
    "calendar-server",
    "--features",
    "test-harness",
    "--bin",
    "calendar-admin-harness",
    "--bin",
    "calendar-server",
    "--locked",
  ],
  {
    cwd: fileURLToPath(new URL("../../../", import.meta.url)),
    stdio: "inherit",
  },
);
child.once("error", () => {
  console.error("Could not build isolated test harness");
  process.exitCode = 1;
});
child.once("exit", (code) => {
  process.exitCode = code ?? 1;
});
