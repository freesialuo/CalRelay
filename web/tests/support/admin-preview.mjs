import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, realpath, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { startSystemFixtures } from "../system/fixtures.mjs";
import { startHTTPSProxy } from "./https-proxy.mjs";

const execute = promisify(execFile);
const systemMode = process.argv[2] === "--system-runtime";
const controlledClock =
  systemMode && process.env.CALRELAY_TEST_CONTROLLED_CLOCK === "1";
// The controlled runtime only accepts a canonical root; macOS tmpdir is behind a symlink.
const directory = await mkdtemp(
  join(
    await realpath(tmpdir()),
    systemMode ? "calrelay-system-" : "calrelay-admin-browser-",
  ),
);
const database = join(directory, "test.sqlite");
const statePath = fileURLToPath(
  new URL("../../.test-admin-state.json", import.meta.url),
);
const binary = fileURLToPath(
  new URL("../../../target/debug/calendar-admin-harness", import.meta.url),
);
const origin = "https://127.0.0.1:4175";
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
  const clockFile = join(directory, "business-clock.json");
  const initialUtc = Date.now();
  if (controlledClock)
    await writeFile(
      clockFile,
      JSON.stringify({ utc_now_ms: initialUtc, monotonic_offset_ms: 0 }),
      { flag: "wx", mode: 0o600 },
    );
  if (systemMode) {
    fixture = await startSystemFixtures({
      nowMs: initialUtc - 60000,
      barkCredential: "synthetic_bark_browser_secret",
    });
    fixture.source("b", { shift: -60000 });
    const { a, b, bark } = fixture.targets;
    await writeFile(fixtureFile, JSON.stringify({ a, b, bark }), {
      flag: "wx",
      mode: 0o600,
    });
  }
  // Start the isolated harness on the same database, fixtures and clock; resolves with its
  // readiness contract. Used once at startup and again for each requested forced restart.
  const launch = async (reopen = false) => {
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
              ...(controlledClock ? ["--business-clock", clockFile] : []),
              ...(reopen ? ["--reopen-database"] : []),
            ]
          : []),
      ],
      { stdio: ["ignore", "pipe", "ignore"] },
    );
    return await new Promise((resolve, reject) => {
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
  };
  let configuration = await launch();
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
      ...(controlledClock
        ? { clock_file: clockFile, initial_utc: initialUtc }
        : {}),
      ...(systemMode
        ? {
            control_request: join(directory, "control-request"),
            control_done: join(directory, "control-done"),
          }
        : {}),
    }),
    { flag: "wx" },
  );
  stateCreated = true;
  if (systemMode) {
    // A system test writes one JSON request to the control file: a forced restart (SIGKILL,
    // then the same state reopened) or a synthetic source/Bark status. Each is acknowledged.
    let handled = 0;
    let busy = false;
    const request = join(directory, "control-request");
    const done = join(directory, "control-done");
    setInterval(async () => {
      if (busy || closing || !existsSync(request)) return;
      busy = true;
      try {
        const command = JSON.parse(await readFile(request, "utf8"));
        await execute("find", [request, "-delete"]);
        if (command.action === "restart") {
          const exited = new Promise((resolve) => child.once("exit", resolve));
          child.kill("SIGKILL");
          await exited;
          configuration = await launch(true);
          proxy.retarget(configuration.upstream, configuration.access_jwt);
        } else if (command.action === "source") {
          fixture.source(command.id, { status: command.status });
        } else if (command.action === "bark") {
          fixture.bark(command.status);
        } else throw new Error("Unknown control request");
        handled++;
        await writeFile(done, String(handled));
      } catch {
        await close();
        process.exitCode = 1;
      } finally {
        busy = false;
      }
    }, 100).unref();
  }
  process.once("SIGTERM", close);
  process.once("SIGINT", close);
  console.log(`Isolated HTTPS admin browser tests: ${origin}/admin/`);
} catch {
  await close();
  console.error("Offline admin browser startup failed");
  process.exitCode = 1;
}
