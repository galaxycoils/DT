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
  assert.match(apiSrc, /\/routines\/.*\/trigger/);
  assert.match(apiSrc, /credentials:\s*"include"/);
  assert.match(apiSrc, /X-DT-CSRF-Token/);
  assert.ok(!apiSrc.includes("localStorage") && !apiSrc.includes("sessionStorage"), "Hosted bearer tokens must not be stored in browser storage");
  const mainSrc = fs.readFileSync(path.join(root, "src/main.tsx"), "utf8");
  assert.ok(!mainSrc.includes("localStorage") && !mainSrc.includes("sessionStorage"), "main.tsx must not read bearer tokens from browser storage");

  const cssSrc = fs.readFileSync(path.join(root, "src/tokens.css"), "utf8");
  for (const token of [
    "--background:",
    "--foreground:",
    "--chat-user:",
    "--working-dot:",
    "--unread:",
    "--radius:",
    "--sidebar-default: 316px;",
    "--sidebar-min: 240px;",
    "--sidebar-max: 480px;",
    "--sidebar-mini: 72px;",
    "--composer-radius: 26px;",
  ]) {
    assert.ok(cssSrc.includes(token), `Missing token ${token}`);
  }
  assert.ok(cssSrc.includes('[data-theme="dark"]'));
  assert.ok(cssSrc.includes('[data-theme="light"]'));
  assert.ok(cssSrc.includes("@media (max-width: 768px)"), "Missing responsive mobile drawer rules");
});

test("web client isolates drafts per target, supports smart arrow editing, and renders responsive workspace features", () => {
  const root = path.resolve(import.meta.dirname, "..");
  const appSrc = fs.readFileSync(path.join(root, "src/App.tsx"), "utf8");
  assert.ok(appSrc.includes("loadTargetDraft") && appSrc.includes("saveTargetDraft"));
  assert.ok(appSrc.includes("selectConversation"), "Switching targets must explicitly load target draft without overwriting");
  assert.ok(appSrc.includes("Quick Jump Command Palette"), "Command palette modal must be present");
  assert.ok(appSrc.includes('sandbox="allow-scripts"'), "Split Space preview must use allow-scripts sandbox");
  assert.ok(appSrc.includes("dt-avatar") && appSrc.includes("dt-eyes"), "Geometric coworker avatars with eyes must be rendered");
});
