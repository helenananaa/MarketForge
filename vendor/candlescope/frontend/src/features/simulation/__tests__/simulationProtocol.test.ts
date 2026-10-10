import assert from "node:assert/strict";
import test from "node:test";
import { parseLosslessJson, parseObservation, parseCandles, parseReceipt, SIMULATION_CHART_EPOCH, type SimulationSelection } from "../simulationProtocol.js";
import { SimulationClient } from "../simulationClient.js";
import { SimulationSession } from "../simulationSession.js";

const selection: SimulationSelection = { roomId: "test-room", accountId: 20, instrumentId: "BTC", intervalMs: 1000 };
function observation(room = "test-room") {
  return { api_version: "strategy.v1", observation: { version: 1, room_id: room, instrument_id: "BTC", status: "Running", step: 2, market_time_ms: 2000,
    book: { bids: [{ price_tick: 99, qty: 20 }], asks: [{ price_tick: 101, qty: 20 }] }, own_orders: [], public_trades: [],
    own_account: { Spot: { account_id: 20, cash_balance: "170141183460469231731687303715884105727", position_qty: 20, available_cash: 1000 } } } };
}
function candles(room = "test-room", intervalMs = 1000) {
  return { api_version: "http.v1", room_id: room, instrument_id: "BTC", interval_ms: intervalMs, market_time_ms: 2000,
    candles: [{ schema_version: 1, open_time_ms: 0, close_time_ms: intervalMs, open_tick: 100, high_tick: 101, low_tick: 99, close_tick: 101, volume: 3, is_final: 2000 >= intervalMs }] };
}
test("i128 and u64 tokens survive parsing; strings and escaped quotes are untouched", () => {
  assert.deepEqual(parseLosslessJson('{"cash":170141183460469231731687303715884105727,"id":18446744073709551615,"negative":-9007199254740993,"safe":9007199254740991,"text":"\\"18446744073709551615\\""}'), {
    cash: "170141183460469231731687303715884105727", id: "18446744073709551615", negative: "-9007199254740993", safe: Number.MAX_SAFE_INTEGER, text: '"18446744073709551615"',
  });
});
test("account ownership and room identity are validated without converting money", () => {
  assert.equal(parseObservation(observation(), selection).account?.cash_balance, "170141183460469231731687303715884105727");
  assert.throws(() => parseObservation(observation("other"), selection), /identity/);
  assert.throws(() => parseObservation(observation(), { ...selection, accountId: 30 }), /account identity/);
});
test("chart coordinates use simulation time; forged finality and unsafe ticks fail closed", () => {
  const wire = candles();
  assert.equal(parseCandles(wire, selection)[0]?.time, SIMULATION_CHART_EPOCH);
  wire.candles[0]!.is_final = false;
  assert.throws(() => parseCandles(wire, selection), /finality/);
  wire.candles[0]!.is_final = true;
  wire.candles[0]!.high_tick = Number.MAX_SAFE_INTEGER + 1;
  assert.throws(() => parseCandles(wire, selection), /integer range/);
});
test("an accepted actor command containing RiskRejected is shown as an order rejection", () => {
  const receipt = parseReceipt({ accepted: true, command_seq: 8, reject_reason: null, events: [{ type: "RiskRejected", order_id: 11357, reason: "InsufficientCash" }] });
  assert.equal(receipt.accepted, false);
  assert.equal(receipt.reject_reason, "InsufficientCash");
});
test("orders carry bearer identity, instrument and idempotency; u64 cancel is emitted exactly", async () => {
  const requests: { url: string; options: RequestInit }[] = [];
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "test-token", userId: "spoof-user" }, async (url, options) => {
    requests.push({ url: String(url), options: options! });
    return new Response('{"accepted":true,"command_seq":7,"reject_reason":null}');
  });
  await client.order(selection, "Buy", 2, 100, new AbortController().signal, "order-1");
  await client.cancel(selection, "18446744073709551615", new AbortController().signal, "cancel-1");
  const headers = requests[0]!.options.headers as Record<string, string>;
  assert.equal(headers.Authorization, "Bearer test-token");
  assert.equal(headers["x-user-id"], undefined);
  assert.equal(headers["Idempotency-Key"], "order-1");
  assert.match(String(requests[0]!.options.body), /"instrument_id":"BTC"/);
  assert.match(String(requests[1]!.options.body), /"order_id":18446744073709551615/);
  await assert.rejects(client.order(selection, "Buy", Number.MAX_SAFE_INTEGER + 1, null, new AbortController().signal, "bad"));
  assert.equal(requests.length, 2);
});

test("perpetual protection preserves risk precision and refuses another account's protection", () => {
  const wire = observation() as unknown as { observation: Record<string, unknown>; api_version: string };
  wire.observation.own_account = { Perp: { account_id: 20, position_qty: 5, margin_status: "margin_call", avg_entry_price_tick: 101 } };
  wire.observation.risk = { margin_buffer: "-9007199254740993", mark_price_tick: 100 };
  wire.observation.position_protections = [{ account_id: 20, instrument_id: "BTC", status: "armed" }];
  const parsed = parseObservation(wire, selection);
  assert.equal(parsed.risk?.margin_buffer, "-9007199254740993");
  assert.equal(parsed.margin_status, "margin_call");
  wire.observation.position_protections = [{ account_id: 30, instrument_id: "BTC", status: "armed" }];
  assert.throws(() => parseObservation(wire, selection), /ownership/);
});

test("entry and position protection use the scoped gateway and stable retry keys", async () => {
  const requests: RequestInit[] = [];
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "test-token", userId: "" }, async (_url, options) => {
    requests.push(options!); return new Response('{"accepted":true,"command_seq":7,"reject_reason":null}');
  });
  const spec = { take_profit_tick: 110, stop_loss_tick: 90, trigger: "Mark" as const };
  const signal = new AbortController().signal;
  await client.order(selection, "Buy", 5, null, signal, "bracket", spec);
  await client.protect(selection, "Both", spec, signal, "replace");
  await client.protect(selection, "Both", null, signal, "clear");
  const body = requests.map((r) => JSON.parse(String(r.body)));
  assert.equal(body[0].action.PlaceBracket.price_tick, null);
  assert.equal(body[0].account_id, 20);
  assert.deepEqual(body[1].action.SetPositionProtection.protection, spec);
  assert.equal(body[2].action.SetPositionProtection.protection, null);
  await assert.rejects(client.protect(selection, "Both", { ...spec, stop_loss_tick: Number.MAX_SAFE_INTEGER + 1 }, signal, "invalid"));
  assert.equal(requests.length, 3);
});
test("slow old-room reads cannot overwrite a newly connected room", async () => {
  let resolveOld!: (response: Response) => void;
  const pending = new Promise<Response>((resolve) => { resolveOld = resolve; });
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "", userId: "" }, async (url) => {
    const value = String(url);
    if (value.includes("test-room/observe")) return pending;
    const room = value.includes("other") ? "other" : "test-room";
    return new Response(JSON.stringify(value.includes("/observe") ? observation(room) : candles(room)));
  });
  const session = new SimulationSession(client, 60_000);
  const old = session.connect(selection);
  await session.connect({ ...selection, roomId: "other" });
  resolveOld(new Response(JSON.stringify(observation())));
  await old;
  assert.equal(session.getSnapshot().snapshot?.observation.room_id, "other");
  session.stop();
});
test("single step advances only the server clock for an explicitly unscheduled room", async () => {
  const requests: { url: string; options: RequestInit }[] = [];
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "", userId: "" }, async (url, options) => {
    requests.push({ url: String(url), options: options! });
    return String(url).endsWith("/clock/step")
      ? new Response('{"error":"room test-room has no scheduler to step"}', { status: 409 })
      : new Response('{}');
  });
  await client.control("test-room", "clock/step", new AbortController().signal, "step-key");
  assert.equal(requests.length, 2);
  assert.match(requests[1]!.url, /clock\/advance$/);
  assert.equal(requests[1]!.options.body, '{"steps":1}');
  const denied = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "", userId: "" }, async () => new Response('{"error":"Forbidden"}', { status: 403 }));
  await assert.rejects(denied.control("test-room", "clock/step", new AbortController().signal, "step-key"), /Forbidden/);
});
test("one pending write blocks duplicate submissions; state is refreshed after acknowledgement", async () => {
  let release!: (response: Response) => void;
  let writes = 0;
  let reads = 0;
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "", userId: "" }, async (url, options) => {
    if (options?.method === "POST") { writes++; return new Promise<Response>((resolve) => { release = resolve; }); }
    if (String(url).includes("/observe")) reads++;
    return new Response(JSON.stringify(String(url).includes("/observe") ? observation() : candles()));
  });
  const session = new SimulationSession(client, 60_000);
  await session.connect(selection);
  const pending = session.order("Buy", 1, null);
  await assert.rejects(session.order("Buy", 1, null), /current MarketForge/);
  release(new Response('{"accepted":false,"command_seq":8,"reject_reason":"InsufficientCash"}'));
  await pending;
  assert.equal(writes, 1);
  assert.equal(reads, 2);
  assert.equal(session.getSnapshot().receipt?.accepted, false);
  assert.equal(session.getSnapshot().receipt?.reject_reason, "InsufficientCash");
  session.stop();
});
test("unauthorized reads clear trade eligibility and private snapshots", async () => {
  let denied = false;
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57305", token: "", userId: "" }, async (url) => denied
    ? new Response('{"error":"Forbidden"}', { status: 403 })
    : new Response(JSON.stringify(String(url).includes("/observe") ? observation() : candles())));
  const session = new SimulationSession(client, 60_000);
  await session.connect(selection);
  denied = true;
  await session.refresh();
  assert.equal(session.getSnapshot().status, "error");
  assert.equal(session.getSnapshot().snapshot, null);
  await assert.rejects(session.order("Buy", 1, null));
  session.stop();
});
