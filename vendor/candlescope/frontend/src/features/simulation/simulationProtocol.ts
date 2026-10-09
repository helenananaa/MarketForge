import type { KlineBarInput } from "../market-data/marketDataTypes.js";
import { parseIntervalParts, parseIntervalSeconds } from "../../utils/intervals.js";

export type WireInteger = number | string;
export type Side = "Buy" | "Sell";
export interface SimulationConnection { baseUrl: string; token: string; userId: string }
export interface SimulationSelection { roomId: string; accountId: number; instrumentId: string; intervalMs: number }
export interface BookLevel { price_tick: number; qty: number }
export interface RestingOrder { order_id: WireInteger; side: Side; price_tick: number; remaining_qty: number }
export interface PublicTrade { trade_id: WireInteger; price_tick: number; qty: number; taker_side: Side }
export interface Observation {
  room_id: string; instrument_id: string; status: "Running" | "Paused" | "Closed";
  step: number; market_time_ms: number;
  book: { bids: BookLevel[]; asks: BookLevel[] };
  own_orders: RestingOrder[]; public_trades: PublicTrade[];
  account: Record<string, WireInteger> | null; marketType: "spot" | "perp";
}
export interface SimulationSnapshot { observation: Observation; bars: KlineBarInput[]; receivedAt: number }
export interface ActionReceipt { accepted: boolean; command_seq: WireInteger; reject_reason: string | null }

// The offset is a chart coordinate only. MarketForge's simulation clock is never changed.
export const SIMULATION_CHART_EPOCH = 1_704_067_200;
export const SIMULATION_INTERVALS = [
  { label: "1s", ms: 1000 }, { label: "1m", ms: 60_000 },
  { label: "5m", ms: 300_000 }, { label: "15m", ms: 900_000 }, { label: "1h", ms: 3_600_000 },
  { label: "4h", ms: 14_400_000 }, { label: "1d", ms: 86_400_000 },
] as const;

/** Simulation intervals have fixed duration; calendar months have no simulation meaning. */
export function simulationIntervalMs(value: string): number {
  const parts = parseIntervalParts(value);
  const seconds = parseIntervalSeconds(value);
  if (!parts || parts.unit === "M" || seconds === null || seconds < 1 || seconds > 2_678_400) {
    throw new Error("Use a fixed interval from 1s to 31d (seconds, minutes, hours, days or weeks)");
  }
  return seconds * 1000;
}

/** Preserve canonical JSON integer tokens before JSON.parse can round i128/u64. */
export function parseLosslessJson(text: string): unknown {
  let normalized = "";
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    if (char === '"') {
      const start = index++;
      while (index < text.length) {
        if (text[index] === "\\") { index += 2; continue; }
        if (text[index++] === '"') break;
      }
      normalized += text.slice(start, index);
    } else if (char === "-" || (char !== undefined && /[0-9]/.test(char))) {
      const token = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(text.slice(index))?.[0];
      if (!token) throw new Error("Invalid JSON number");
      normalized += !/[.eE]/.test(token) && !Number.isSafeInteger(Number(token)) ? JSON.stringify(token) : token;
      index += token.length;
    } else { normalized += char; index++; }
  }
  return JSON.parse(normalized) as unknown;
}

export function wireObject(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid MarketForge object");
  return value as Record<string, unknown>;
}
export function wireText(value: unknown): string {
  if (typeof value !== "string" || !value) throw new Error("Invalid MarketForge text");
  return value;
}
export function wireInteger(value: unknown): WireInteger {
  if (typeof value === "number" && Number.isSafeInteger(value)) return value;
  if (typeof value === "string" && /^-?(0|[1-9]\d*)$/.test(value)) return value;
  throw new Error("Invalid or unsafe MarketForge integer");
}
export function safeInteger(value: unknown, min = 0): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < min) throw new Error("MarketForge value exceeds the chart/input integer range");
  return value;
}
function rows(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error("Invalid MarketForge array");
  return value as unknown[];
}
function side(value: unknown): Side {
  if (value !== "Buy" && value !== "Sell") throw new Error("Invalid MarketForge side");
  return value;
}
export function parseObservation(payload: unknown, selection: SimulationSelection): Observation {
  const root = wireObject(payload);
  if (root.api_version !== "strategy.v1") throw new Error("Unsupported MarketForge observation protocol");
  const o = wireObject(root.observation);
  if (o.version !== 1 || o.room_id !== selection.roomId || (selection.instrumentId && o.instrument_id !== selection.instrumentId)) throw new Error("MarketForge observation identity mismatch");
  if (o.status !== "Running" && o.status !== "Paused" && o.status !== "Closed") throw new Error("Invalid MarketForge market status");
  const book = wireObject(o.book);
  const levels = (value: unknown) => rows(value).map((entry) => {
    const level = wireObject(entry);
    return { price_tick: safeInteger(level.price_tick, 1), qty: safeInteger(level.qty) };
  });
  let account: Record<string, WireInteger> | null = null;
  let marketType: "spot" | "perp" = "spot";
  if (o.own_account != null) {
    const wrapper = wireObject(o.own_account);
    marketType = "Perp" in wrapper ? "perp" : "spot";
    const source = wireObject(wrapper[marketType === "perp" ? "Perp" : "Spot"]);
    if (String(wireInteger(source.account_id)) !== String(selection.accountId)) throw new Error("MarketForge account identity mismatch");
    account = {};
    for (const field of ["account_id", "cash_balance", "position_qty", "available_cash", "available_position", "reserved_cash", "reserved_position", "fees_paid", "equity", "unrealized_pnl", "realized_pnl", "initial_margin", "maintenance_margin", "available_margin", "margin_ratio_ppm"]) {
      if (source[field] != null) account[field] = wireInteger(source[field]);
    }
  }
  return {
    room_id: selection.roomId, instrument_id: wireText(o.instrument_id), status: o.status,
    step: safeInteger(o.step), market_time_ms: safeInteger(o.market_time_ms),
    book: { bids: levels(book.bids), asks: levels(book.asks) }, account, marketType,
    own_orders: rows(o.own_orders).map((entry) => {
      const order = wireObject(entry);
      if (String(wireInteger(order.account_id)) !== String(selection.accountId)) throw new Error("MarketForge order ownership mismatch");
      return { order_id: wireInteger(order.order_id), side: side(order.side), price_tick: safeInteger(order.price_tick, 1), remaining_qty: safeInteger(order.remaining_qty) };
    }),
    public_trades: rows(o.public_trades).map((entry) => {
      const trade = wireObject(entry);
      return { trade_id: wireInteger(trade.trade_id), price_tick: safeInteger(trade.price_tick, 1), qty: safeInteger(trade.qty), taker_side: side(trade.taker_side) };
    }),
  };
}
export function parseCandles(payload: unknown, selection: SimulationSelection): KlineBarInput[] {
  const root = wireObject(payload);
  if (root.api_version !== "http.v1" || root.room_id !== selection.roomId || root.instrument_id !== selection.instrumentId || root.interval_ms !== selection.intervalMs) throw new Error("MarketForge candle identity mismatch");
  const now = safeInteger(root.market_time_ms);
  let previous = -1;
  return rows(root.candles).map((entry) => {
    const bar = wireObject(entry);
    const openTime = safeInteger(bar.open_time_ms);
    const closeTime = safeInteger(bar.close_time_ms);
    if (bar.schema_version !== 1 || openTime <= previous || openTime % selection.intervalMs !== 0 || closeTime !== openTime + selection.intervalMs || typeof bar.is_final !== "boolean" || bar.is_final !== (now >= closeTime)) throw new Error("Invalid MarketForge candle sequence/finality");
    previous = openTime;
    const open = safeInteger(bar.open_tick, 1), high = safeInteger(bar.high_tick, 1), low = safeInteger(bar.low_tick, 1), close = safeInteger(bar.close_tick, 1);
    if (low > Math.min(open, close) || high < Math.max(open, close)) throw new Error("Invalid MarketForge OHLC");
    const volume = safeInteger(bar.volume);
    const buy = bar.taker_buy_base == null ? null : safeInteger(bar.taker_buy_base);
    if (buy !== null && buy > volume) throw new Error("Invalid MarketForge taker volume");
    return { time: SIMULATION_CHART_EPOCH + openTime / 1000, open, high, low, close, volume, is_closed: bar.is_final,
      quote_volume: bar.quote_volume == null ? null : safeInteger(bar.quote_volume),
      trades: bar.trades == null ? null : safeInteger(bar.trades),
      taker_buy_base: buy,
      taker_buy_quote: bar.taker_buy_quote == null ? null : safeInteger(bar.taker_buy_quote),
      order_flow: buy === null ? null : { taker_sell_base: volume - buy, volume_delta_base: buy * 2 - volume,
        taker_buy_ratio_base: volume === 0 ? null : buy / volume, cvd_contribution_base: buy * 2 - volume },
    };
  });
}
export function parseReceipt(payload: unknown): ActionReceipt {
  const root = wireObject(payload);
  if (typeof root.accepted !== "boolean") throw new Error("Invalid MarketForge action acknowledgement");
  // `accepted` acknowledges the actor command. Its engine result can still reject the order.
  const rejection = root.events == null ? undefined : rows(root.events).map(wireObject).find((event) =>
    ["RiskRejected", "OrderRejected", "CancelRejected", "AmendRejected"].includes(String(event.type)));
  return { accepted: root.accepted && rejection === undefined, command_seq: wireInteger(root.command_seq),
    reject_reason: rejection ? wireText(rejection.reason) : root.reject_reason == null ? null : wireText(root.reject_reason) };
}
export function elapsedLabel(seconds: number): string {
  const elapsed = Math.max(0, Math.floor(seconds));
  return `${Math.floor(elapsed / 3600).toString().padStart(2, "0")}:${Math.floor(elapsed / 60 % 60).toString().padStart(2, "0")}:${(elapsed % 60).toString().padStart(2, "0")}`;
}
