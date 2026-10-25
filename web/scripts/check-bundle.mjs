import { execFile } from "node:child_process";
import { readdir, readFile } from "node:fs/promises";
import { promisify } from "node:util";

const markers = [
  "CALRELAY_PRIVATE_CANARY_79cc3812",
  "CALRELAY_PUBLIC_CANARY_10b72dfe",
  "synthetic_browser_test_challenge",
  "synthetic-challenge:",
  "offline-widget",
];
await promisify(execFile)(
  process.execPath,
  ["node_modules/vite/bin/vite.js", "build"],
  {
    env: {
      ...process.env,
      CALRELAY_MASTER_KEY: markers[0],
      VITE_SECRET: markers[1],
    },
  },
);
async function inspect(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = `${directory}/${entry.name}`;
    if (entry.isDirectory()) await inspect(path);
    else {
      const bytes = await readFile(path, "utf8");
      if (markers.some((marker) => bytes.includes(marker)))
        throw new Error("Build env canary leaked into bundle");
      if (path.endsWith(".map"))
        throw new Error("Unexpected source map in bundle");
    }
  }
}
await inspect("dist");
async function inspectSource(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = `${directory}/${entry.name}`;
    if (entry.name === "generated") continue;
    if (entry.isDirectory()) await inspectSource(path);
    else if (entry.name.endsWith(".ts")) {
      const source = await readFile(path, "utf8");
      if (
        /innerHTML|outerHTML|insertAdjacentHTML|document\.write|localStorage|sessionStorage|console\.|import\.meta\.env|process\.env/.test(
          source,
        )
      )
        throw new Error(`Unsafe DOM/storage/log/env surface in ${path}`);
    }
  }
}
await inspectSource("src");

console.log(
  "Bundle env canaries absent; no source maps or credential persistence/logging/unsafe HTML sinks in app source.",
);
