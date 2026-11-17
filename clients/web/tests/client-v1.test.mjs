import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

test("web client targets /api/v1 paths and carries Rakazo dark/light CSS tokens", () => {
  const root = path.resolve(import.meta.dirname, "..");
  const apiSrc = fs.readFileSync(path.join(root, "src/api.ts"), "utf8");
  assert.match(apiSrc, /\/api\/v1/);
  assert.match(apiSrc, /\/workspaces\/.*\/events/);
  assert.match(apiSrc, /\/approvals\//);
  assert.match(apiSrc, /\/system\/providers/);

  const cssSrc = fs.readFileSync(path.join(root, "src/tokens.css"), "utf8");
  for (const token of ["--background:", "--foreground:", "--chat-user:", "--working-dot:", "--unread:", "--radius:"]) {
    assert.ok(cssSrc.includes(token), `Missing token ${token}`);
  }
  assert.ok(cssSrc.includes('[data-theme="dark"]'));
  assert.ok(cssSrc.includes('[data-theme="light"]'));
});
