import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { AiPreferencesStore, aiConnectionSnapshot, parseAiPreferences, resolveAiMode } from "./ai-connection.mjs";

test("AI preference persistence is strict and fails closed without granting trust/live", async () => {
  const profile = await mkdtemp(path.join(os.tmpdir(), "candlescope-ai-"));
  try {
    const store = new AiPreferencesStore(profile);
    assert.deepEqual((await store.load()).preferences, { mode: "off" });
    await store.save({ mode: "edit" });
    assert.deepEqual((await new AiPreferencesStore(profile).load()).preferences, { mode: "edit" });
    for (const input of [{ mode: "live" }, { mode: "edit", trust: true }, null, []]) assert.throws(() => parseAiPreferences(input));
    await writeFile(store.filename, '{"mode":"edit","trust":true}');
    assert.deepEqual(await store.load(), { preferences: { mode: "off" }, error: "INVALID_AI_PREFERENCES" });
  } finally { assert.equal(path.dirname(profile), os.tmpdir()); await rm(profile, { recursive: true, force: true }); }
});
test("explicit readonly launch overrides a saved editing preference", () => {
  assert.deepEqual(resolveAiMode({ mode: "edit" }, ["--control"], {}), { mode: "observe", overridden: true });
  assert.deepEqual(resolveAiMode({ mode: "edit" }, [], {}), { mode: "edit", overridden: false });
  assert.deepEqual(resolveAiMode({ mode: "off" }, [], { CANDLESCOPE_CONTROL_EDIT: "1" }), { mode: "off", overridden: false });
});
test("copyable config contains paths and a bundled runtime launch, never bearer credentials", () => {
  const snapshot = aiConnectionSnapshot({ preferences: { mode: "off" }, effective: { mode: "off", overridden: false },
    executable: "C:/My App/CandleScope.exe", adapter: "/absent/cli.mjs", connectionFile: "C:/profile/control/connection.json" });
  assert.equal(snapshot.status, "disabled"); assert.equal(snapshot.adapterAvailable, false);
  assert.deepEqual(snapshot.config.mcpServers.candlescope.env, { ELECTRON_RUN_AS_NODE: "1" });
  assert.equal(JSON.stringify(snapshot).includes('"token"'), false);
  assert.equal(snapshot.restartRequired, false);
});
