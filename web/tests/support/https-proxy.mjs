// Test-only HTTPS boundary. The upstream must be the feature-isolated Rust harness.
import { execFile } from "node:child_process";
import { mkdtemp, readFile } from "node:fs/promises";
import { createServer } from "node:https";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execute = promisify(execFile);

export async function startHTTPSProxy(upstream, syntheticJWT, port = 4175) {
  if (
    !/^http:\/\/127\.0\.0\.1:\d+$/.test(upstream) ||
    !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(syntheticJWT)
  )
    throw new Error(
      "Expected isolated loopback harness and synthetic identity",
    );
  const directory = await mkdtemp(join(tmpdir(), "calrelay-browser-tls-"));
  const key = join(directory, "key.pem");
  const certificate = join(directory, "cert.pem");
  const cleanup = () => execute("find", [directory, "-depth", "-delete"]);
  let server;
  try {
    await execute("openssl", [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      key,
      "-out",
      certificate,
      "-days",
      "1",
      "-subj",
      "/CN=localhost",
      "-addext",
      "subjectAltName=IP:127.0.0.1,DNS:localhost",
    ]);
    const dist = fileURLToPath(new URL("../../dist/", import.meta.url));
    server = createServer(
      { key: await readFile(key), cert: await readFile(certificate) },
      async (request, response) => {
        response.setHeader("cache-control", "no-store");
        try {
          const url = new URL(request.url ?? "/", `https://127.0.0.1:${port}`);
          if (
            url.pathname.startsWith("/api/") ||
            url.pathname.startsWith("/health/") ||
            url.pathname.startsWith("/feeds/aggregates/")
          ) {
            const headers = new Headers();
            for (const [name, value] of Object.entries(request.headers)) {
              if (
                ![
                  "host",
                  "connection",
                  "transfer-encoding",
                  "content-length",
                  "cf-access-jwt-assertion",
                ].includes(name) &&
                value !== undefined
              )
                headers.set(
                  name,
                  Array.isArray(value) ? value.join(", ") : value,
                );
            }
            // An explicit test proxy acts as the synthetic Access boundary. The real
            // Rust verifier still checks its signature, identity, expiry and policy.
            if (url.pathname.startsWith("/api/admin/"))
              headers.set("cf-access-jwt-assertion", syntheticJWT);
            const chunks = [];
            let size = 0;
            for await (const chunk of request) {
              size += chunk.length;
              if (size > 1048576) {
                response.writeHead(413);
                response.end();
                return;
              }
              chunks.push(chunk);
            }
            const result = await fetch(
              `${upstream}${url.pathname}${url.search}`,
              {
                method: request.method,
                headers,
                body: ["GET", "HEAD"].includes(request.method ?? "GET")
                  ? undefined
                  : Buffer.concat(chunks),
                redirect: "error",
                signal: AbortSignal.timeout(12000),
              },
            );
            response.statusCode = result.status;
            for (const [name, value] of result.headers)
              if (
                ![
                  "connection",
                  "transfer-encoding",
                  "content-length",
                  "content-encoding",
                  "set-cookie",
                ].includes(name)
              )
                response.setHeader(name, value);
            const cookies = result.headers.getSetCookie();
            if (cookies.length) response.setHeader("set-cookie", cookies);
            const body = await result.arrayBuffer();
            if (body.byteLength > 2097152)
              throw new Error("Upstream response exceeded test budget");
            response.end(Buffer.from(body));
            return;
          }
          if (request.method !== "GET") {
            response.writeHead(405);
            response.end();
            return;
          }
          const relative = decodeURIComponent(url.pathname).replace(
            /^\/admin\//,
            "",
          );
          const path = resolve(dist, relative === "" ? "index.html" : relative);
          if (
            !url.pathname.startsWith("/admin/") ||
            !path.startsWith(`${resolve(dist)}${sep}`)
          ) {
            response.writeHead(404);
            response.end();
            return;
          }
          const content = await readFile(path);
          const kind = path.endsWith(".html")
            ? "text/html; charset=utf-8"
            : path.endsWith(".js")
              ? "text/javascript"
              : path.endsWith(".css")
                ? "text/css"
                : "application/octet-stream";
          response.setHeader("content-type", kind);
          response.end(content);
        } catch {
          if (!response.headersSent) response.writeHead(502);
          response.end("Local test request failed");
        }
      },
    );
    await new Promise((done, reject) => {
      server.once("error", reject);
      server.listen(port, "127.0.0.1", done);
    });
    return {
      origin: `https://127.0.0.1:${port}`,
      close: async () => {
        server.closeAllConnections();
        await new Promise((done) => server.close(done));
        await cleanup();
      },
    };
  } catch (error) {
    server?.closeAllConnections();
    server?.close();
    await cleanup();
    throw error;
  }
}
