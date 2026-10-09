import { safeInteger, wireObject, wireText } from "./simulationProtocol.js";

export interface RoomContext {
  room_id: string; user_id: string; display_name?: string | undefined; role: "owner" | "admin" | "instructor" | "trader" | "spectator";
  capabilities: { read_all_accounts: boolean; trade: boolean; manage_bots: boolean; manage_members: boolean; control_room: boolean };
  visible_account_ids: number[]; trade_account_ids: number[]; instruments: string[];
}
export type RoomOverview = { context: RoomContext; markets: { instrument_id: string; accounts: Record<string, Record<string, unknown>[]>; orders: Record<string, unknown>[] }[];
  competition?: Record<string, unknown> | null; bots: null | { status: { lifecycle: string; running: boolean; market_running: boolean; interval_ms: number }; agents: unknown[] }; status: string };
export const roleLabel = (role: RoomContext["role"]) => ({ owner: "房主", admin: "管理员", instructor: "教练", trader: "交易员", spectator: "观众" })[role];
export function parseRoomContext(input: unknown): RoomContext {
  const value = wireObject(input); const capabilities = wireObject(value.capabilities);
  const role = wireText(value.role) as RoomContext["role"];
  if (!["owner", "admin", "instructor", "trader", "spectator"].includes(role)) throw new Error("未知房间身份");
  for (const key of ["read_all_accounts", "trade", "manage_bots", "manage_members", "control_room"]) {
    if (typeof capabilities[key] !== "boolean") throw new Error("无效房间权限");
  }
  const ids = (input: unknown) => { if (!Array.isArray(input)) throw new Error("无效账户列表"); return [...new Set(input.map((id) => safeInteger(id, 1)))]; };
  const visible = ids(value.visible_account_ids); const trade = ids(value.trade_account_ids);
  if (trade.some((id) => !visible.includes(id)) || (role === "spectator" && trade.length)) throw new Error("账户权限不一致");
  if (!Array.isArray(value.instruments) || !value.instruments.length) throw new Error("房间没有市场");
  return { room_id: wireText(value.room_id), user_id: wireText(value.user_id), display_name: typeof value.display_name === "string" ? value.display_name : wireText(value.user_id), role,
    capabilities: { read_all_accounts: capabilities.read_all_accounts as boolean, trade: capabilities.trade as boolean,
      manage_bots: capabilities.manage_bots as boolean, manage_members: capabilities.manage_members as boolean, control_room: capabilities.control_room as boolean },
    visible_account_ids: visible, trade_account_ids: trade, instruments: value.instruments.map(wireText) };
}

export function parseConfiguration(text: string): unknown {
  return JSON.parse(text, (_key, value: unknown) => {
    if (typeof value === "number" && (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value)))) throw new Error("配置中有超出安全范围的整数");
    return value;
  });
}
