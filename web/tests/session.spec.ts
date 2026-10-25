import { expect, test } from "@playwright/test";
import { AdminFailure, AdminSession } from "../src/admin-session";

const grant = {
  csrf_token: "csrf_fixture",
  write_nonce: "nonce_fixture",
  expires_at: Math.floor(Date.now() / 1000) + 600,
};
const signal = () => new AbortController().signal;
const response = (status: number, nonce?: string) =>
  new Response(null, {
    status,
    headers: nonce ? { "x-admin-write-nonce": nonce } : {},
  });

test("management nonce rotates on business conflicts and never retries", async () => {
  const calls: RequestInit[] = [];
  const session = new AdminSession(async (_path, init) => {
    calls.push(init ?? {});
    if (calls.length === 1) return Response.json(grant);
    return response(409, `nonce_${calls.length}`);
  });
  await session.establish("test_challenge", signal());
  await session.write(
    "/api/admin/v1/sources/source_a",
    "PUT",
    { "if-match": '"1"' },
    "{}",
    signal(),
  );
  expect(session.writable).toBe(true);
  await session.write(
    "/api/admin/v1/sources/source_a",
    "DELETE",
    { "if-match": '"2"' },
    undefined,
    signal(),
  );
  expect(calls).toHaveLength(3);
  expect(calls[1].headers).toMatchObject({
    "x-admin-write-nonce": "nonce_fixture",
    "if-match": '"1"',
  });
  expect(calls[2].headers).toMatchObject({ "x-admin-write-nonce": "nonce_2" });
});

for (const failure of ["lost", "missing", "same", "denied"]) {
  test(`uncertain or denied ${failure} invalidates write authorization`, async () => {
    let call = 0;
    const session = new AdminSession(async () => {
      if (++call === 1) return Response.json(grant);
      if (failure === "lost") throw new Error("private transport details");
      return response(
        failure === "denied" ? 403 : 200,
        failure === "same" ? grant.write_nonce : undefined,
      );
    });
    await session.establish("test_challenge", signal());
    await session
      .write("/api/admin/v1/sources/a", "PUT", {}, "{}", signal())
      .catch((error) => expect(error).toBeInstanceOf(AdminFailure));
    expect(session.writable).toBe(false);
    await expect(
      session.write("/api/admin/v1/sources/a", "PUT", {}, "{}", signal()),
    ).rejects.toMatchObject({ kind: "denied" });
    expect(call).toBe(2);
  });
}

test("simultaneous writes are serialized by rejecting the duplicate", async () => {
  let call = 0;
  let release: ((value: Response) => void) | undefined;
  const session = new AdminSession(async () => {
    if (++call === 1) return Response.json(grant);
    return new Promise<Response>((resolve) => {
      release = resolve;
    });
  });
  await session.establish("test_challenge", signal());
  const first = session.write(
    "/api/admin/v1/sources/a",
    "DELETE",
    {},
    undefined,
    signal(),
  );
  await expect(
    session.write("/api/admin/v1/sources/a", "DELETE", {}, undefined, signal()),
  ).rejects.toMatchObject({ kind: "busy" });
  release?.(response(204, "nonce_next"));
  await first;
  expect(call).toBe(2);
});

test("failed login reports denial rather than minting an unknown session", async () => {
  const session = new AdminSession(async () => response(403));
  await expect(
    session.establish("test_challenge", signal()),
  ).rejects.toMatchObject({ kind: "denied" });
  expect(session.writable).toBe(false);
});

test("concurrent login cannot overwrite a newer authorization", async () => {
  const pending: ((value: Response) => void)[] = [];
  const session = new AdminSession(
    async () =>
      new Promise<Response>((resolve) => {
        pending.push(resolve);
      }),
  );
  const first = session.establish("test_challenge", signal());
  const second = session
    .establish("test_challenge2", signal())
    .catch((error) => error);
  try {
    expect(pending).toHaveLength(1);
  } finally {
    session.clear();
    for (const release of pending) release(Response.json(grant));
    await Promise.allSettled([first, second]);
  }
  expect(session.writable).toBe(false);
});

test("session expiry emits a change and removes write capability", async () => {
  const session = new AdminSession(async () =>
    Response.json({ ...grant, expires_at: Math.floor(Date.now() / 1000) + 2 }),
  );
  await session.establish("test_challenge", signal());
  let changes = 0;
  session.addEventListener("change", () => {
    changes++;
  });
  await expect.poll(() => changes, { timeout: 4000 }).toBe(1);
  expect(session.writable).toBe(false);
});

test("a stale denied read cannot clear a newer login grant", async () => {
  let release: ((value: Response) => void) | undefined;
  const session = new AdminSession(async (path) => {
    if (path === "/api/admin/v1/sources")
      return new Promise<Response>((resolve) => {
        release = resolve;
      });
    return Response.json(grant);
  });
  const oldRead = session.read("/api/admin/v1/sources", signal());
  await session.establish("test_challenge", signal());
  expect(session.writable).toBe(true);
  release?.(response(403));
  await oldRead;
  expect(session.writable).toBe(true);
  const currentRead = session.read("/api/admin/v1/sources", signal());
  release?.(response(403));
  await currentRead;
  expect(session.writable).toBe(false);
});
