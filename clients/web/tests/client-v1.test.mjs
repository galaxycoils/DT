import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
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
