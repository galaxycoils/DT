import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

test("mobile client screens & Rakazo tokens match /api/v1 contract", () => {
  const root = path.resolve(import.meta.dirname, "..");
  for (const rel of ["app/_layout.tsx", "app/index.tsx", "app/thread.tsx", "app/routines.tsx", "app/settings.tsx"]) {
    assert.ok(fs.existsSync(path.join(root, rel)), `Missing mobile screen ${rel}`);
  }
  const tokens = fs.readFileSync(path.join(root, "lib/tokens.ts"), "utf8");
  assert.ok(tokens.includes("darkTokens") && tokens.includes("lightTokens"));
  const api = fs.readFileSync(path.join(root, "lib/api.ts"), "utf8");
  assert.match(api, /\/api\/v1/);
});
