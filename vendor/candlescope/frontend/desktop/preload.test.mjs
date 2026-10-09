import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import test from "node:test";

const source = await readFile(new URL("./preload.cjs", import.meta.url), "utf8");
function load(argv, ipcRenderer = {}) {
  let bridge;
  vm.runInNewContext(source, {
    process: { argv, env: { CANDLESCOPE_DESKTOP_BACKEND_PORT: "18080" } },
    require: () => ({ contextBridge: { exposeInMainWorld: (_name, value) => { bridge = value; } }, ipcRenderer }),
  });
  return bridge;
}
test("preload uses the host's actual port instead of the inherited preferred port", () => {
  assert.equal(load(["electron", "--candlescope-backend-port=29123"]).apiBase, "http://127.0.0.1:29123/api/v1");
});
test("missing or invalid endpoint cannot silently connect to another service", () => {
  for (const value of [undefined, "0", "65536", "NaN", "1.5"]) {
    assert.throws(() => load(value === undefined ? [] : [`--candlescope-backend-port=${value}`]), /not configured/);
  }
});

test("extension diagnostics uses the readonly native IPC channel", async () => {
  const calls = [];
  const bridge = load(["--candlescope-backend-port=29123"], { invoke: async (channel) => { calls.push(channel); return { active: [] }; } });
  assert.deepEqual(await bridge.getExtensionDiagnostics(), { active: [] });
  assert.deepEqual(calls, ["candlescope:desktop:extension-diagnostics"]);
});
test("AI setup remains available when control is disabled and uses fixed host preference channels", async () => {
  const calls = [];
  const bridge = load(["--candlescope-backend-port=29123"], { invoke: async (...args) => { calls.push(args); return { status: "disabled" }; } });
  assert.equal(bridge.controlEnabled, false);
  await bridge.getAiConnection(); await bridge.saveAiConnection({ mode: "observe" });
  assert.deepEqual(calls, [["candlescope:ai-connection:get"], ["candlescope:ai-connection:save", { mode: "observe" }]]);
});
