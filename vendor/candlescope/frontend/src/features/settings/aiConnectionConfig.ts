import type { AiConnectionSnapshot } from "./aiConnectionTypes.js";

export function formatAiConnectionConfig(config: AiConnectionSnapshot["config"], format: "json" | "toml"): string {
  if (format === "json") return JSON.stringify(config, null, 2);
  const server = config.mcpServers.candlescope;
  return `[mcp_servers.candlescope]\ncommand = ${JSON.stringify(server.command)}\nargs = ${JSON.stringify(server.args)}\n\n[mcp_servers.candlescope.env]\nELECTRON_RUN_AS_NODE = "1"\n`;
}
