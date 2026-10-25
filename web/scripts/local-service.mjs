// Development/test harness only. Never connects to an existing database or service.
import { execFile, spawn } from "node:child_process";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

export async function startService() {
  const directory = await mkdtemp(join(tmpdir(), "calrelay-web-"));
  const binary = fileURLToPath(
    new URL("../../target/debug/calendar-server", import.meta.url),
  );
  const child = spawn(binary, [], {
    env: {
      CALRELAY_BIND: "127.0.0.1:0",
      CALRELAY_DATABASE: join(directory, "test.sqlite"),
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  const close = async () => {
    if (child.pid && child.exitCode === null && child.signalCode === null) {
      const exited = new Promise((resolve) => child.once("exit", resolve));
      child.kill("SIGTERM");
      await exited;
    }
    await promisify(execFile)("find", [directory, "-depth", "-delete"]);
  };
  try {
    const origin = await new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error("Local service startup timed out")),
        15000,
      );
      let output = "";
      child.stderr.on("data", (chunk) => {
        output += chunk.toString();
        const match = output.match(/CalRelay listening on (127\.0\.0\.1:\d+)/);
        if (match) {
          clearTimeout(timer);
          resolve(`http://${match[1]}`);
        }
      });
      child.once("error", () => {
        clearTimeout(timer);
        reject(
          new Error("Build calendar-server before running the web harness"),
        );
      });
      child.once("exit", () => {
        clearTimeout(timer);
        reject(new Error("Local service exited during startup"));
      });
    });
    return { origin, close };
  } catch (error) {
    await close();
    throw error;
  }
}
