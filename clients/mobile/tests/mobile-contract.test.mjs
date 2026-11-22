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
  assert.equal(pkg.main, "expo-router/entry", "Expo Router entry must be configured in package.json");
  assert.match(pkg.dependencies.expo, /58\./);
  assert.ok(pkg.dependencies["expo-secure-store"]);
  assert.ok(pkg.scripts["export:ios"] && pkg.scripts["export:android"]);

  const tokens = fs.readFileSync(path.join(root, "lib/tokens.ts"), "utf8");
  assert.ok(tokens.includes("darkTokens") && tokens.includes("lightTokens") && tokens.includes("layoutMetrics"));

  const api = fs.readFileSync(path.join(root, "lib/api.ts"), "utf8");
  assert.match(api, /\/api\/v1/);
  assert.match(api, /expo-secure-store/);
  assert.match(api, /WHEN_UNLOCKED_THIS_DEVICE_ONLY/);
  assert.match(api, /triggerRoutine/);
  assert.match(api, /respondApproval/);

  const threadSrc = fs.readFileSync(path.join(root, "app/thread.tsx"), "utf8");
  assert.ok(threadSrc.includes("Inline Approval") && threadSrc.includes("handleSend"));
  const routinesSrc = fs.readFileSync(path.join(root, "app/routines.tsx"), "utf8");
  assert.ok(routinesSrc.includes("Trigger Now") && routinesSrc.includes("/routines"));
  assert.match(api, /normalizeRoutine/);
  assert.match(api, /normalizeActivityPost/);
  assert.match(api, /parseMessageCard/);
  assert.match(api, /abortAllMobileRequests/);
  assert.match(api, /client_msg_id/);

  const routinesScreen = fs.readFileSync(path.join(root, "app/routines.tsx"), "utf8");
  assert.match(routinesScreen, /listRoutines/);
  assert.match(routinesScreen, /listRoutineRuns/);

  const threadScreen = fs.readFileSync(path.join(root, "app/thread.tsx"), "utf8");
  assert.match(threadScreen, /Message saved, reply failed/);
  assert.match(threadScreen, /Retry reply/);
  assert.match(threadScreen, /respondApproval/);
});
