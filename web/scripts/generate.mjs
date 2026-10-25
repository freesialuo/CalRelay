import { readFile, writeFile } from "node:fs/promises";
import openapiTS, { astToString } from "openapi-typescript";
import { startService } from "./local-service.mjs";

const service = await startService();
try {
  const response = await fetch(`${service.origin}/api/v1/openapi.json`, {
    signal: AbortSignal.timeout(8000),
  });
  if (!response.ok) throw new Error("Local OpenAPI export failed");
  const generated =
    "// Generated from the local Rust service OpenAPI. Run npm run generate.\n" +
    astToString(await openapiTS(await response.json()));
  const target = new URL("../src/generated/api.d.ts", import.meta.url);
  if (process.argv.includes("--check")) {
    if ((await readFile(target, "utf8")) !== generated)
      throw new Error(
        "OpenAPI types drifted; rebuild Rust and run npm run generate",
      );
  } else await writeFile(target, generated);
} finally {
  await service.close();
}
