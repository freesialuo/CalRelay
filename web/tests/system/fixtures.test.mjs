import assert from "node:assert/strict";
import { connect } from "node:net";
import { performance } from "node:perf_hooks";
import { test as nodeTest } from "node:test";

const test = (name, run) => nodeTest(name, { timeout: 10000 }, run);

import { logicalEndpoints, startSystemFixtures } from "./fixtures.mjs";

const nowMs = Date.UTC(2026, 9, 25, 12);
const key = "synthetic_system_fixture_key";
async function fixture(run) {
  const f = await startSystemFixtures({ nowMs, barkCredential: key });
  try {
    await run(f);
  } finally {
    await f.close();
  }
}
const post = (url, body) =>
  fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
const payload = {
  device_key: key,
  title: "synthetic title",
  body: "synthetic description",
};

test("two real HTTP ICS endpoints retain a shared UID but independent calendars and validators", () =>
  fixture(async (f) => {
    const a = await fetch(f.targets.a);
    const b = await fetch(f.targets.b);
    assert.equal(a.status, 200);
    assert.equal(b.status, 200);
    assert.match(a.headers.get("content-type"), /^text\/calendar/);
    const bodyA = await a.text();
    const bodyB = await b.text();
    assert.equal(
      bodyA.match(/UID:(.*)\r\n/)[1],
      bodyB.match(/UID:(.*)\r\n/)[1],
    );
    assert.notEqual(
      bodyA.match(/SUMMARY:(.*)/)[1],
      bodyB.match(/SUMMARY:(.*)/)[1],
    );
    assert.ok(bodyA.includes("DTSTART:20261025T120200Z\r\n"));
    assert.ok(bodyB.includes("DTSTART:20261025T120300Z\r\n"));
    const tag = a.headers.get("etag");
    const cached = await fetch(f.targets.a, {
      headers: { "if-none-match": tag },
    });
    assert.equal(cached.status, 304);
    assert.equal(await cached.text(), "");
    assert.equal(cached.headers.get("etag"), tag);
    assert.ok(
      Object.values(logicalEndpoints).every(
        (url) =>
          new URL(url).protocol === "https:" &&
          new URL(url).hostname.endsWith(".invalid"),
      ),
    );
  }));

test("one source fails independently and edits/cancellation invalidate its ETag", () =>
  fixture(async (f) => {
    const old = await fetch(f.targets.a);
    await old.text();
    const etag = old.headers.get("etag");
    f.source("b", { status: 503 });
    const failed = await fetch(f.targets.b);
    assert.equal(failed.status, 503);
    assert.equal(failed.headers.get("retry-after"), "1");
    assert.equal((await fetch(f.targets.a)).status, 200);
    f.source("a", { shift: 60000, cancelled: true });
    const changed = await fetch(f.targets.a, {
      headers: { "if-none-match": etag },
    });
    assert.equal(changed.status, 200);
    assert.notEqual(changed.headers.get("etag"), etag);
    const body = await changed.text();
    assert.ok(body.includes("DTSTART:20261025T120300Z"));
    assert.ok(body.includes("STATUS:CANCELLED"));
    assert.ok(body.includes("SEQUENCE:2"));
    f.source("b", { status: 200 });
    assert.equal((await fetch(f.targets.b)).status, 200);
  }));

test("fetch barrier returns the captured old snapshot while a later request sees the edit", () =>
  fixture(async (f) => {
    const held = f.holdNext("a");
    const pending = fetch(f.targets.a);
    await held.started;
    f.source("a", { shift: 120000 });
    held.release();
    const old = await pending;
    assert.equal(old.headers.get("etag"), '"system-a-r1"');
    assert.ok((await old.text()).includes("DTSTART:20261025T120200Z"));
    const fresh = await fetch(f.targets.a);
    assert.equal(fresh.headers.get("etag"), '"system-a-r2"');
  }));

test("Bark records bounded metadata for retry and accepted responses without retaining payloads", () =>
  fixture(async (f) => {
    f.bark(503);
    const retry = await post(f.targets.bark, payload);
    assert.equal(retry.status, 503);
    assert.equal(retry.headers.get("retry-after"), "1");
    f.bark(200);
    const accepted = await post(f.targets.bark, payload);
    assert.deepEqual(await accepted.json(), { code: 200, message: "success" });
    const secretLeak = JSON.stringify(f.snapshot());
    for (const value of [key, payload.title, payload.body, "device_key"])
      assert.ok(
        !secretLeak.includes(value),
        "journal retained a secret or event field",
      );
    assert.deepEqual(
      f.snapshot().requests.map((r) => r.status),
      [503, 200],
    );
    for (let i = 0; i < 260; i++) {
      const r = await fetch(f.targets.a);
      await r.arrayBuffer();
    }
    assert.equal(f.snapshot().requests.length, 256);
    assert.equal(f.snapshot().overflow, 6);
  }));

test("Bark rejects malformed, wrong credential and oversized payloads without echoing input", () =>
  fixture(async (f) => {
    for (const input of [
      "malformed",
      { ...payload, device_key: "different" },
    ]) {
      const r = await post(f.targets.bark, input);
      assert.equal(r.status, 422);
      assert.ok(!(await r.text()).includes("different"));
    }
    const huge = await post(f.targets.bark, "x".repeat(65537));
    assert.equal(huge.status, 413);
    assert.ok(!JSON.stringify(f.snapshot()).includes(key));
    assert.ok(
      f.snapshot().requests.every((r) => r.credentialMatched === false),
    );
    assert.equal(
      (await fetch(`${f.targets.bark}?secret=untrusted`)).status,
      404,
    );
  }));

test("in-flight Bark acceptance can be held and fixture shutdown aborts a held fetch", async () => {
  const f = await startSystemFixtures({ nowMs, barkCredential: key });
  try {
    const held = f.holdNext("bark");
    const pending = post(f.targets.bark, payload);
    await held.started;
    assert.equal(f.snapshot().requests.length, 0);
    held.release();
    assert.equal((await pending).status, 200);
    const fetchHold = f.holdNext("a");
    const reading = fetch(f.targets.a).then(
      () => "unexpected",
      () => "aborted",
    );
    await fetchHold.started;
    await f.close();
    assert.equal(await reading, "aborted");
    await f.close();
  } finally {
    await f.close();
  }
});

test("invalid test controls and non-synthetic secrets fail closed", async () => {
  await assert.rejects(
    startSystemFixtures({ nowMs, barkCredential: "real-looking-credential" }),
    /Invalid synthetic/,
  );
  await assert.rejects(
    startSystemFixtures({ nowMs: NaN, barkCredential: key }),
    /Invalid synthetic/,
  );
  await assert.rejects(
    startSystemFixtures({ nowMs: Date.UTC(10000, 0, 1), barkCredential: key }),
    /Invalid synthetic/,
  );
  await fixture(async (f) => {
    for (const change of [
      { shift: Infinity },
      { shift: 604800001 },
      { status: 302 },
      { cancelled: "yes" },
      { unknown: 1 },
    ])
      assert.throws(() => f.source("a", change), /Invalid fixture/);
    assert.throws(() => f.source("other", {}), /Invalid fixture/);
    assert.throws(() => f.bark(302), /Invalid fixture/);
    const held = f.holdNext("a");
    assert.throws(() => f.holdNext("a"), /Invalid fixture/);
    held.release();
  });
});

async function slowClient(url, headersOnly) {
  const target = new URL(url);
  const socket = connect({ host: target.hostname, port: Number(target.port) });
  const started = performance.now();
  let trickle;
  let fail;
  const closed = new Promise((resolve, reject) => {
    socket.on("error", () => {});
    socket.once("close", () => resolve(performance.now() - started));
    fail = setTimeout(
      () => reject(new Error("slow request exceeded its absolute deadline")),
      4500,
    );
    socket.once("connect", () => {
      socket.write(
        headersOnly
          ? "POST /bark HTTP/1.1\r\nHost: localhost\r\nX-Slow: "
          : "POST /bark HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n",
      );
      trickle = setInterval(() => socket.write("x"), 200);
    });
    socket.resume();
  });
  try {
    const elapsed = await closed;
    assert.ok(
      elapsed >= 2500,
      "request closed before the deadline was exercised",
    );
    assert.ok(
      elapsed < 4500,
      "request was not bounded independently of incoming bytes",
    );
  } finally {
    clearTimeout(fail);
    clearInterval(trickle);
    socket.destroy();
  }
}
for (const headersOnly of [false, true])
  test(`absolute ${headersOnly ? "headers" : "body"} deadline rejects a continuously trickling client`, () =>
    fixture(async (f) => {
      await slowClient(f.targets.bark, headersOnly);
      assert.deepEqual(f.snapshot(), { requests: [], overflow: 0 });
      const healthy = await fetch(f.targets.a);
      assert.equal(healthy.status, 200);
      await healthy.text();
    }));
test("present undefined controls are rejected without corrupting the next ICS response", () =>
  fixture(async (f) => {
    const original = await fetch(f.targets.a);
    const body = await original.text();
    const tag = original.headers.get("etag");
    for (const field of ["shift", "status", "cancelled"]) {
      assert.throws(
        () => f.source("a", { [field]: undefined }),
        /Invalid fixture/,
      );
      const response = await fetch(f.targets.a);
      assert.equal(response.status, 200);
      assert.equal(response.headers.get("etag"), tag);
      assert.equal(await response.text(), body);
    }
  }));
