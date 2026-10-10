import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";

const modes = new Set(["off", "observe", "edit"]);
export function parseAiPreferences(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== 1 || Object.keys(value)[0] !== "mode" || !modes.has(value.mode)) throw new Error("INVALID_AI_PREFERENCES");
  return { mode: value.mode };
}
export class AiPreferencesStore {
  constructor(profile) { this.filename = path.join(profile, "ai-connection.json"); }
  async load() {
    try {
      const bytes = await readFile(this.filename, "utf8");
      if (Buffer.byteLength(bytes) > 1024) throw new Error("INVALID_AI_PREFERENCES");
      return { preferences: parseAiPreferences(JSON.parse(bytes)), error: null };
    } catch (error) {
      return { preferences: { mode: "off" }, error: error.code === "ENOENT" ? null : "INVALID_AI_PREFERENCES" };
    }
  }
  async save(input) {
    const preferences = parseAiPreferences(input);
    await mkdir(path.dirname(this.filename), { recursive: true });
    await writeFile(`${this.filename}.tmp`, JSON.stringify(preferences), { mode: 0o600 });
    await rename(`${this.filename}.tmp`, this.filename);
    return preferences;
  }
}
export function resolveAiMode(preferences, argv, env) {
  if (argv.includes("--control-edit")) return { mode: "edit", overridden: true };
  if (argv.includes("--control")) return { mode: "observe", overridden: true };
  if (env.CANDLESCOPE_CONTROL_ENABLED === "1") return { mode: env.CANDLESCOPE_CONTROL_EDIT === "1" ? "edit" : "observe", overridden: true };
  return { mode: preferences.mode, overridden: false };
}
export function aiConnectionSnapshot({ preferences, effective, service, adapter, executable, connectionFile, preferenceError = null }) {
  const capabilities = service?.capabilities();
  return { preferences, activeMode: effective.mode, launchOverride: effective.overridden,
    restartRequired: preferences.mode !== effective.mode, preferenceError,
    status: service ? (capabilities.windows.some((window) => window.ready) ? "ready" : "starting") : "disabled",
    instanceId: capabilities?.instanceId ?? null, scopes: capabilities?.scopes ?? [],
    windows: capabilities?.windows ?? [], adapterAvailable: existsSync(adapter),
    config: { mcpServers: { candlescope: { command: executable,
      args: [adapter, "mcp", "--connection", connectionFile], env: { ELECTRON_RUN_AS_NODE: "1" } } } },
  };
}
