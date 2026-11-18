import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

test("mobile client screens, Expo SDK 58 config, SecureStore, and tokens match contract", () => {
  const root = path.resolve(import.meta.dirname, "..");
  for (const rel of [
    "app/_layout.tsx",
    "app/index.tsx",
    "app/thread.tsx",
    "app/routines.tsx",
    "app/settings.tsx",
    "app.json",
    "eas.json",
  ]) {
    assert.ok(fs.existsSync(path.join(root, rel)), `Missing mobile artifact ${rel}`);
  }
  const pkg = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8"));
  assert.match(pkg.dependencies.expo, /58\./);
  assert.ok(pkg.dependencies["expo-secure-store"]);
  const tokens = fs.readFileSync(path.join(root, "lib/tokens.ts"), "utf8");
  assert.ok(tokens.includes("darkTokens") && tokens.includes("lightTokens") && tokens.includes("layoutMetrics"));
  const api = fs.readFileSync(path.join(root, "lib/api.ts"), "utf8");
  assert.match(api, /\/api\/v1/);
  assert.match(api, /expo-secure-store/);
});
