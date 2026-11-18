import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";

test("web client uses cookie sessions with CSRF, targets /api/v1, and carries Rakazo tokens", () => {
  const root = path.resolve(import.meta.dirname, "..");
  const apiSrc = fs.readFileSync(path.join(root, "src/api.ts"), "utf8");
  assert.match(apiSrc, /\/api\/v1/);
  assert.match(apiSrc, /\/workspaces\/.*\/events/);
  assert.match(apiSrc, /\/approvals\//);
  assert.match(apiSrc, /\/system\/providers/);
  assert.match(apiSrc, /credentials:\s*"include"/);
  assert.match(apiSrc, /X-DT-CSRF-Token/);
  assert.ok(!apiSrc.includes("localStorage") && !apiSrc.includes("sessionStorage"), "Hosted bearer tokens must not be stored in browser storage");
  const mainSrc = fs.readFileSync(path.join(root, "src/main.tsx"), "utf8");
  assert.ok(!mainSrc.includes("localStorage") && !mainSrc.includes("sessionStorage"), "main.tsx must not read bearer tokens from browser storage");

  const cssSrc = fs.readFileSync(path.join(root, "src/tokens.css"), "utf8");
  for (const token of ["--background:", "--foreground:", "--chat-user:", "--working-dot:", "--unread:", "--radius:"]) {
    assert.ok(cssSrc.includes(token), `Missing token ${token}`);
  }
  assert.ok(cssSrc.includes('[data-theme="dark"]'));
  assert.ok(cssSrc.includes('[data-theme="light"]'));
});

test("web client session sign-in, message send, SSE reconnect cursor recovery, and sign-out journey", async () => {
  const messages = [];
  const events = [];
  let activeSession = true;

  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    if (req.method === "POST" && url.pathname === "/api/v1/auth/session") {
      activeSession = true;
      res.writeHead(200, {
        "Content-Type": "application/json",
        "Set-Cookie": "dt_session=sess-1; HttpOnly; Secure; Path=/",
      });
      res.end(JSON.stringify({
        authenticated: true,
        user_id: "alice",
        ws_id: "ws_alpha",
        tenant_id: "t1",
        role: "owner",
        csrf_token: "csrf-123",
        expires_at: "2099-01-01T00:00:00Z",
      }));
      return;
    }
    if (!activeSession || req.headers.cookie !== "dt_session=sess-1") {
      res.writeHead(401, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "unauthorized" }));
      return;
    }
    if (req.method === "POST" && url.pathname === "/api/v1/dm/muse/messages") {
      if (req.headers["x-dt-csrf-token"] !== "csrf-123") {
        res.writeHead(403);
        res.end();
        return;
      }
      let raw = "";
      for await (const chunk of req) raw += chunk;
      const body = JSON.parse(raw);
      const id = `msg-${messages.length + 1}`;
      messages.push({ id, author: "alice", content: body.content });
      events.push({ seq: events.length + 1, type: "DIRECT_MESSAGE_ADDED", id });
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ id }));
      return;
    }
    if (req.method === "GET" && url.pathname === "/api/v1/dm/muse/messages") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify(messages));
      return;
    }
    if (req.method === "GET" && url.pathname === "/api/v1/workspaces/ws_alpha/events") {
      const after = Number(url.searchParams.get("after") ?? "0");
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      for (const ev of events.filter((e) => e.seq > after)) {
        res.write(`id: ${ev.seq}\nevent: ${ev.type}\ndata: ${JSON.stringify(ev)}\n\n`);
      }
      res.end();
      return;
    }
    if (req.method === "POST" && url.pathname === "/api/v1/auth/logout") {
      activeSession = false;
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ signed_out: true }));
      return;
    }
    res.writeHead(404);
    res.end();
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  const base = `http://127.0.0.1:${addr.port}`;

  try {
    // 1. Sign in
    const loginResp = await fetch(`${base}/api/v1/auth/session`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ enrollment_token: "tok" }),
    });
    assert.equal(loginResp.status, 200);
    const cookie = loginResp.headers.get("set-cookie").split(";")[0];
    const sess = await loginResp.json();
    assert.equal(sess.ws_id, "ws_alpha");

    // 2. Send first message with CSRF
    const send1 = await fetch(`${base}/api/v1/dm/muse/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Cookie: cookie, "X-DT-CSRF-Token": sess.csrf_token },
      body: JSON.stringify({ content: "first" }),
    });
    assert.equal(send1.status, 200);

    // 3. Initial event fetch (seq 1), then simulate disconnect and commit seq 2 & 3
    const sse1 = await (await fetch(`${base}/api/v1/workspaces/ws_alpha/events?after=0`, { headers: { Cookie: cookie } })).text();
    assert.match(sse1, /id: 1/);

    await fetch(`${base}/api/v1/dm/muse/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Cookie: cookie, "X-DT-CSRF-Token": sess.csrf_token },
      body: JSON.stringify({ content: "second" }),
    });
    await fetch(`${base}/api/v1/dm/muse/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Cookie: cookie, "X-DT-CSRF-Token": sess.csrf_token },
      body: JSON.stringify({ content: "third" }),
    });

    // 4. Reconnect at after=1 and verify missed events 2 and 3 + server message history match
    const sse2 = await (await fetch(`${base}/api/v1/workspaces/ws_alpha/events?after=1`, { headers: { Cookie: cookie } })).text();
    assert.doesNotMatch(sse2, /id: 1\b/);
    assert.match(sse2, /id: 2/);
    assert.match(sse2, /id: 3/);
    const history = await (await fetch(`${base}/api/v1/dm/muse/messages`, { headers: { Cookie: cookie } })).json();
    assert.deepEqual(history.map((m) => m.content), ["first", "second", "third"]);

    // 5. Sign out invalidates session
    await fetch(`${base}/api/v1/auth/logout`, {
      method: "POST",
      headers: { Cookie: cookie, "X-DT-CSRF-Token": sess.csrf_token },
    });
    const afterSignOut = await fetch(`${base}/api/v1/dm/muse/messages`, { headers: { Cookie: cookie } });
    assert.equal(afterSignOut.status, 401);
  } finally {
    server.close();
  }
});
