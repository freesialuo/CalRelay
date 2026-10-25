import { execFile, spawn } from "node:child_process";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { startHTTPSProxy } from "./https-proxy.mjs";

const execute = promisify(execFile);
const directory = await mkdtemp(join(tmpdir(), "calrelay-admin-browser-"));
const database = join(directory, "test.sqlite");
const statePath = fileURLToPath(
  new URL("../../.test-admin-state.json", import.meta.url),
);
const binary = fileURLToPath(
  new URL("../../../target/debug/calendar-admin-harness", import.meta.url),
);
const origin = "https://127.0.0.1:4175";
const child = spawn(
  binary,
  ["--database", database, "--browser-origin", origin],
  { stdio: ["ignore", "pipe", "ignore"] },
);
let proxy;
let stateCreated = false;
let closing = false;
const close = async () => {
  if (closing) return;
  closing = true;
  await proxy?.close();
  if (child.pid && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill("SIGTERM");
    const forced = setTimeout(() => child.kill("SIGKILL"), 4000);
    await exited;
    clearTimeout(forced);
  }
  if (stateCreated) await execute("find", [statePath, "-delete"]);
  await execute("find", [directory, "-depth", "-delete"]);
};
try {
  const configuration = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("Offline harness startup timed out")),
      15000,
    );
    let buffer = "";
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      if (buffer.length > 32768) {
        clearTimeout(timer);
        reject(new Error("Invalid harness startup"));
        return;
      }
      const line = buffer.split("\n")[0];
      if (!buffer.includes("\n")) return;
      clearTimeout(timer);
      try {
        const data = JSON.parse(line);
        if (
          data.synthetic_only !== true ||
          data.browser_origin !== origin ||
          data.challenge_prefix !== "synthetic-challenge:"
        )
          throw new Error();
        resolve(data);
      } catch {
        reject(new Error("Invalid offline harness contract"));
      }
    });
    child.once("error", () => {
      clearTimeout(timer);
      reject(new Error("Build the explicit test-harness binary first"));
    });
    child.once("exit", () => {
      clearTimeout(timer);
      reject(new Error("Offline harness exited before readiness"));
    });
  });
  proxy = await startHTTPSProxy(
    configuration.upstream,
    configuration.access_jwt,
  );
  // Only public local addresses/paths are persisted. The synthetic JWT stays in proxy memory.
  await writeFile(
    statePath,
    JSON.stringify({ upstream: configuration.upstream, database }),
    { flag: "wx" },
  );
  stateCreated = true;
  process.once("SIGTERM", close);
  process.once("SIGINT", close);
  console.log(`Isolated HTTPS admin browser tests: ${origin}/admin/`);
} catch {
  await close();
  console.error("Offline admin browser startup failed");
  process.exitCode = 1;
}
