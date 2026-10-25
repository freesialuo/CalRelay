// Explicit test helper only. No production imports, runtime copy, DNS/TLS bypass or default server.
import { createServer } from "node:http";

export const logicalEndpoints = Object.freeze({
  a: "https://ics-a.system.invalid/calendar.ics",
  b: "https://ics-b.system.invalid/calendar.ics",
  bark: "https://bark.system.invalid/push",
});
const MAX_BODY = 65536;
const MAX_JOURNAL = 256;
const stamp = (time) =>
  new Date(time)
    .toISOString()
    .replaceAll(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z");
function calendar(source, state, now) {
  const title =
    source === "a" ? "First synthetic calendar" : "Second synthetic calendar";
  const start = now + (source === "a" ? 120000 : 180000) + state.shift;
  return [
    "BEGIN:VCALENDAR",
    "VERSION:2.0",
    "PRODID:-//CalRelay//System fixture//EN",
    "BEGIN:VEVENT",
    "UID:same-system-uid",
    `DTSTAMP:${stamp(now)}`,
    `DTSTART:${stamp(start)}`,
    `DTEND:${stamp(start + 60000)}`,
    `SEQUENCE:${state.revision}`,
    `SUMMARY:${title}`,
    `STATUS:${state.cancelled ? "CANCELLED" : "CONFIRMED"}`,
    "END:VEVENT",
    "END:VCALENDAR",
    "",
  ].join("\r\n");
}
function barrier() {
  let reached;
  let release;
  const started = new Promise((r) => {
    reached = r;
  });
  const waiting = new Promise((r) => {
    release = r;
  });
  return {
    started,
    release,
    async wait(signal) {
      reached();
      if (signal.aborted) return;
      await Promise.race([
        waiting,
        new Promise((resolve) =>
          signal.addEventListener("abort", resolve, { once: true }),
        ),
      ]);
    },
  };
}

export async function startSystemFixtures({ nowMs, barkCredential }) {
  if (
    !Number.isSafeInteger(nowMs) ||
    nowMs < 0 ||
    nowMs > Date.UTC(9999, 11, 23) ||
    typeof barkCredential !== "string" ||
    !barkCredential.startsWith("synthetic_") ||
    !/^[\x21-\x7e]{1,4096}$/.test(barkCredential)
  )
    throw new Error("Invalid synthetic fixture configuration");
  const states = {
    a: { revision: 1, status: 200, shift: 0, cancelled: false },
    b: { revision: 1, status: 200, shift: 0, cancelled: false },
  };
  let barkStatus = 200;
  let closed = false;
  const journal = [];
  let overflow = 0;
  const record = (item) => {
    if (journal.length === MAX_JOURNAL) {
      journal.shift();
      overflow++;
    }
    journal.push(Object.freeze(item));
  };
  const held = new Map();
  const controllers = new Set();
  const server = createServer({ connectionsCheckingInterval: 100 });
  server.on("request", async (request, response) => {
    const controller = new AbortController();
    controllers.add(controller);
    const disconnected = () => {
      if (!response.writableFinished) controller.abort();
    };
    request.once("aborted", disconnected);
    response.once("close", disconnected);
    // A total body deadline, independent of socket activity and Node's periodic sweep.
    const deadline = setTimeout(() => {
      controller.abort();
      request.destroy();
      response.destroy();
    }, 3000);
    const clearDeadline = () => clearTimeout(deadline);
    request.once("end", clearDeadline);
    request.once("aborted", clearDeadline);
    response.once("close", clearDeadline);
    if (
      request.complete ||
      (request.headers["content-length"] === undefined &&
        request.headers["transfer-encoding"] === undefined)
    )
      clearDeadline();
    response.setHeader("cache-control", "no-store");
    const path = request.url;
    try {
      if (
        (path === "/ics/a" || path === "/ics/b") &&
        request.method === "GET"
      ) {
        request.resume();
        const source = path.at(-1);
        const state = { ...states[source] };
        const tag = `"system-${source}-r${state.revision}"`;
        const conditional = request.headers["if-none-match"] === tag;
        const status = state.status === 503 ? 503 : conditional ? 304 : 200;
        // Snapshot the response before the barrier to exercise an old in-flight fetch.
        const body = calendar(source, state, nowMs);
        const pending = held.get(source);
        held.delete(source);
        if (pending) await pending.wait(controller.signal);
        if (closed || controller.signal.aborted) return;
        response.statusCode = status;
        if (status !== 503) response.setHeader("etag", tag);
        response.setHeader("content-type", "text/calendar; charset=utf-8");
        if (status === 503) response.setHeader("retry-after", "1");
        record({
          kind: "ics",
          source,
          status,
          revision: state.revision,
          conditional,
        });
        response.end(status === 200 ? body : undefined);
        return;
      }
      if (path === "/bark" && request.method === "POST") {
        let size = 0;
        const chunks = [];
        for await (const chunk of request) {
          size += chunk.length;
          if (size > MAX_BODY) {
            response.writeHead(413);
            response.end();
            return;
          }
          chunks.push(chunk);
        }
        let credentialMatched = false;
        try {
          const value = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          credentialMatched =
            !!value &&
            typeof value === "object" &&
            value.device_key === barkCredential &&
            typeof value.title === "string" &&
            typeof value.body === "string";
        } catch {
          /* Deliberately do not log input or parse errors. */
        }
        const status = credentialMatched ? barkStatus : 422;
        const pending = held.get("bark");
        held.delete("bark");
        if (pending) await pending.wait(controller.signal);
        if (closed || controller.signal.aborted) return;
        record({ kind: "bark", status, bytes: size, credentialMatched });
        response.writeHead(status, {
          "content-type": "application/json",
          ...(status === 503 ? { "retry-after": "1" } : {}),
        });
        response.end(
          JSON.stringify({
            code: status,
            message: status === 200 ? "success" : "synthetic rejection",
          }),
        );
        return;
      }
      response.writeHead(404);
      response.end();
    } catch {
      if (!response.headersSent) response.writeHead(400);
      response.end();
    } finally {
      controllers.delete(controller);
    }
  });
  // Header/total-request limits are checked every 100ms, rather than Node's 30s default.
  // The explicit body deadline above and socket inactivity bound remain independent.
  server.maxConnections = 32;
  server.maxRequestsPerSocket = 64;
  server.requestTimeout = 3000;
  server.headersTimeout = 3000;
  server.timeout = 5000;
  server.on("timeout", (socket) => socket.destroy());
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const base = `http://127.0.0.1:${server.address().port}`;
  return {
    targets: Object.freeze({
      a: `${base}/ics/a`,
      b: `${base}/ics/b`,
      bark: `${base}/bark`,
    }),
    snapshot: () => ({
      requests: journal.map((item) => ({ ...item })),
      overflow,
    }),
    source(id, change) {
      if (
        !Object.hasOwn(states, id) ||
        !change ||
        typeof change !== "object" ||
        Object.keys(change).some(
          (k) => !["status", "shift", "cancelled"].includes(k),
        ) ||
        (Object.hasOwn(change, "status") &&
          ![200, 503].includes(change.status)) ||
        (Object.hasOwn(change, "shift") &&
          (!Number.isSafeInteger(change.shift) ||
            Math.abs(change.shift) > 604800000)) ||
        (Object.hasOwn(change, "cancelled") &&
          typeof change.cancelled !== "boolean")
      )
        throw new Error("Invalid fixture source change");
      const next = { ...states[id], ...change };
      if (
        next.shift !== states[id].shift ||
        next.cancelled !== states[id].cancelled
      )
        next.revision++;
      states[id] = next;
    },
    bark(status) {
      if (![200, 503].includes(status))
        throw new Error("Invalid fixture Bark status");
      barkStatus = status;
    },
    holdNext(kind) {
      if (!["a", "b", "bark"].includes(kind) || held.has(kind))
        throw new Error("Invalid fixture response barrier");
      const pending = barrier();
      held.set(kind, pending);
      return { started: pending.started, release: pending.release };
    },
    async close() {
      if (closed) return;
      closed = true;
      for (const c of controllers) c.abort();
      for (const b of held.values()) b.release();
      held.clear();
      await new Promise((resolve) => {
        server.close(resolve);
        server.closeAllConnections();
      });
      barkCredential = "";
    },
  };
}
