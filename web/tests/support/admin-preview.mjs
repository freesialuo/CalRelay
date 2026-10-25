import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { startSystemFixtures } from "../system/fixtures.mjs";
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
const systemMode = process.argv[2] === "--system-runtime";
let child;
let fixture;
let proxy;
let stateCreated = false;
let closing = false;
const close = async () => {
  if (closing) return;
  closing = true;
  await proxy?.close();
  if (child?.pid && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.kill("SIGTERM");
    const forced = setTimeout(
      () => child.kill("SIGKILL"),
      systemMode ? 22000 : 4000,
    );
    await exited;
    clearTimeout(forced);
  }
  await fixture?.close();
  if (stateCreated) await execute("find", [statePath, "-delete"]);
  await execute("find", [directory, "-depth", "-delete"]);
};
try {
  // Only this explicit offline test process creates synthetic credential files.
  // Provision via the actual owner's CLI, never SQL seeds or a management bypass.
  const keyPath = join(directory, "synthetic-master-key");
  const secretPath = join(directory, "synthetic-bark-secret");
  const deploymentPath = join(directory, "synthetic-deployment.json");
  await writeFile(keyPath, randomBytes(32), { flag: "wx", mode: 0o600 });
  await writeFile(secretPath, "synthetic_bark_browser_secret", {
    flag: "wx",
    mode: 0o600,
  });
  await writeFile(
    deploymentPath,
    JSON.stringify({
      bind: "127.0.0.1:0",
      credentials: { current_key_version: 1, key_files: { 1: keyPath } },
    }),
    { flag: "wx", mode: 0o600 },
  );
  const fixtureFile = join(directory, "system-fixtures.json");
  if (systemMode) {
    fixture = await startSystemFixtures({
      nowMs: Date.now() - 60000,
      barkCredential: "synthetic_bark_browser_secret",
    });
    fixture.source("b", { shift: -60000 });
    const { a, b, bark } = fixture.targets;
    await writeFile(fixtureFile, JSON.stringify({ a, b, bark }), {
      flag: "wx",
      mode: 0o600,
    });
  }
  child = spawn(
    binary,
    [
      "--database",
      database,
      "--browser-origin",
      origin,
      ...(systemMode
        ? [
            "--system-fixtures",
            fixtureFile,
            "--deployment-config",
            deploymentPath,
          ]
        : []),
    ],
    { stdio: ["ignore", "pipe", "ignore"] },
  );
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
  const credentialBinary = fileURLToPath(
    new URL("../../../target/debug/calendar-server", import.meta.url),
  );
  const provision = await execute(
    credentialBinary,
    [
      "credential",
      "provision-bark",
      "--deployment-config",
      deploymentPath,
      "--credential-id",
      "synthetic_browser_bark",
      "--secret-file",
      secretPath,
    ],
    { env: { ...process.env, CALRELAY_DATABASE: database }, maxBuffer: 4096 },
  );
  const metadata = JSON.parse(provision.stdout);
  if (
    metadata.credential_id !== "synthetic_browser_bark" ||
    metadata.revision !== 0 ||
    provision.stderr
  )
    throw new Error("Invalid synthetic CLI receipt");
  proxy = await startHTTPSProxy(
    configuration.upstream,
    configuration.access_jwt,
  );
  // Only public local addresses/paths are persisted. The synthetic JWT stays in proxy memory.
  await writeFile(
    statePath,
    JSON.stringify({
      upstream: configuration.upstream,
      database,
      ...(fixture ? { fixture_status: fixture.targets.status } : {}),
    }),
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
