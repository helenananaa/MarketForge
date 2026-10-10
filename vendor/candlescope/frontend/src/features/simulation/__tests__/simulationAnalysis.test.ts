import assert from "node:assert/strict";
import test from "node:test";
import { parseCandles, simulationIntervalMs, SIMULATION_CHART_EPOCH, type SimulationSelection } from "../simulationProtocol.js";
import { simulationTrades } from "../simulationTradeFlow.js";
import { SimulationClient } from "../simulationClient.js";
import { resolveKlineOrderFlow } from "../../indicators/klineOrderFlowProjection.js";
import { buildTradeFlowProfile } from "../../trade-flow/tradeFlowProfile.js";
import type { KlineBar } from "../../market-data/marketDataTypes.js";

const selection: SimulationSelection = { roomId: "room", accountId: 20, instrumentId: "BTC", intervalMs: 3000 };
const candles = () => ({ api_version: "http.v1", room_id: "room", instrument_id: "BTC", interval_ms: 3000, market_time_ms: 3000,
  candles: [{ schema_version: 1, open_time_ms: 0, close_time_ms: 3000, open_tick: 100, high_tick: 100, low_tick: 99, close_tick: 99,
    volume: 5, quote_volume: 497, trades: 2, taker_buy_base: 2, taker_buy_quote: 200, is_final: true }] });

test("fixed simulation periods reuse interval parsing without inventing calendar months", () => {
  assert.equal(simulationIntervalMs("3m"), 180000);
  assert.equal(simulationIntervalMs("2h"), 7200000);
  assert.equal(simulationIntervalMs("1w"), 604800000);
  for (const value of ["0s", "1M", "32d", "1.5m", "bad"]) assert.throws(() => simulationIntervalMs(value));
});
test("authoritative aggressor volume feeds CandleScope Delta/CVD; absent fields stay unknown", () => {
  const payload = candles();
  const bar = parseCandles(payload, selection)[0] as KlineBar;
  assert.deepEqual(resolveKlineOrderFlow(bar), { buy: 2, sell: 3, delta: -1, contribution: -1 });
  assert.equal(bar.quote_volume, 497);
  payload.candles[0]!.taker_buy_base = 6;
  assert.throws(() => parseCandles(payload, selection), /taker volume/);
  const legacy = candles();
  delete (legacy.candles[0] as Partial<typeof legacy.candles[0]>)?.taker_buy_base;
  assert.equal(resolveKlineOrderFlow(parseCandles(legacy, selection)[0] as KlineBar), null);
});
test("tick adapter preserves exact side/price/quantity and chronological identity for the shared profile", () => {
  const payload = { room_id: "room", ticks: [
    { room_id: "room", instrument_id: "BTC", trade_id: 2, price_tick: 99, qty: 3, taker_side: "sell", market_time_ms: 2000 },
    { room_id: "room", instrument_id: "BTC", trade_id: 0, price_tick: 100, qty: 2, taker_side: "buy", market_time_ms: 0 },
  ] };
  const trades = simulationTrades(payload, selection);
  assert.deepEqual(trades.map((row) => row.aggTradeId), [0, 2]);
  assert.equal(trades[0]?.tradeTimeMs, SIMULATION_CHART_EPOCH * 1000);
  assert.equal(buildTradeFlowProfile(trades).trades, 2);
  assert.throws(() => simulationTrades(payload, { ...selection, instrumentId: "OTHER" }), /identity/);
  payload.ticks[0]!.trade_id = Number.MAX_SAFE_INTEGER + 1;
  assert.throws(() => simulationTrades(payload, selection), /integer range/);
});
test("room editor configuration is cloned and room identity rebound without changing user draft", async () => {
  const draft = { scenario: { room_id: "old", accounts: [] }, agents: [{ Plugin: { participant: { room_id: "old" } } }, { NoiseTrader: { participant: { room_id: "old" } } }], autostart_agents: false };
  let posted: unknown;
  const client = new SimulationClient({ baseUrl: "http://localhost:57306", token: "", userId: "local-user" }, async (_url, options) => {
    posted = JSON.parse(String(options?.body)); return new Response("{}");
  }, null);
  await client.createBackgroundRoom("new", new AbortController().signal, draft);
  assert.equal(draft.scenario.room_id, "old");
  assert.equal((posted as typeof draft).scenario.room_id, "new");
  assert.equal((posted as typeof draft).agents[0]?.Plugin?.participant.room_id, "new");
  assert.equal((posted as typeof draft).agents[1]?.NoiseTrader?.participant.room_id, "new");
  assert.equal((posted as typeof draft).autostart_agents, false);
});
test("history uses a bounded exclusive cursor and checks room/period identity", async () => {
  let url = "";
  const client = new SimulationClient({ baseUrl: "http://localhost:57306", token: "", userId: "local-user" }, async (input) => {
    url = String(input); return new Response(JSON.stringify(candles()));
  }, null);
  await client.history(selection, SIMULATION_CHART_EPOCH + 3, new AbortController().signal);
  assert.match(url, /before_open_time_ms=3000&limit=500/);
  assert.match(url, /interval_ms=3000/);
});
