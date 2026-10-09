export type AiConnectionMode = "off" | "observe" | "edit";
export interface AiConnectionSnapshot {
  preferences: { mode: AiConnectionMode };
  activeMode: AiConnectionMode;
  launchOverride: boolean;
  restartRequired: boolean;
  preferenceError: string | null;
  status: "disabled" | "starting" | "ready";
  instanceId: string | null;
  scopes: readonly string[];
  windows: readonly { windowId: string; ready: boolean }[];
  adapterAvailable: boolean;
  config: { mcpServers: { candlescope: { command: string; args: string[]; env: { ELECTRON_RUN_AS_NODE: "1" } } } };
}
