import test from "node:test";
import assert from "node:assert/strict";
import { parseRoomContext, parseConfiguration } from "../roomPortalProtocol.js";
import { SimulationClient } from "../simulationClient.js";
import { SimulationSession } from "../simulationSession.js";

const context = { room_id: "room", user_id: "viewer", role: "spectator", visible_account_ids: [20, 30], trade_account_ids: [], instruments: ["BTC"],
  capabilities: { read_all_accounts: true, trade: false, manage_bots: false, manage_members: false, control_room: false } };
test("entry rejects forged spectator trade grants, unsafe account IDs and missing permissions", () => {
  assert.deepEqual(parseRoomContext(context).trade_account_ids, []);
  assert.throws(() => parseRoomContext({ ...context, trade_account_ids: [20] }));
  assert.throws(() => parseRoomContext({ ...context, visible_account_ids: [Number.MAX_SAFE_INTEGER + 1] }));
  assert.throws(() => parseRoomContext({ ...context, capabilities: {} }));
  assert.throws(() => parseRoomContext({ ...context, role: "unknown" }));
  assert.throws(() => parseConfiguration('{"cash":9007199254740993}'));
});
test("equivalent HTTP contexts have a stable signature despite JSON field order", () => {
  const reversed = { ...context, capabilities: Object.fromEntries(Object.entries(context.capabilities).reverse()) };
  assert.equal(JSON.stringify(parseRoomContext(reversed)), JSON.stringify(parseRoomContext(context)));
});
test("revoked HTTP access clears the previously displayed private account", async () => {
  let revoked = false;
  const client = new SimulationClient({ baseUrl: "http://127.0.0.1:57306", userId: "trader", token: "" }, async (url) => {
    const path = String(url);
    if (revoked) return new Response('{"error":"membership removed"}', { status: 403 });
    if (path.endsWith("/runtime")) return new Response('{"storage":{"kind":"memory","durable":false}}');
    if (path.includes("/observe")) return new Response(JSON.stringify({ api_version: "strategy.v1", observation: { version: 1, room_id: "room", instrument_id: "BTC", status: "Running", step: 0, market_time_ms: 0,
      book: { bids: [], asks: [] }, own_orders: [], public_trades: [], own_account: { Spot: { account_id: 20, cash_balance: 1000 } } } }));
    return new Response(JSON.stringify({ api_version: "http.v1", room_id: "room", instrument_id: "BTC", interval_ms: 1000, market_time_ms: 0, candles: [] }));
  }, null);
  const session = new SimulationSession(client, 60_000);
  try {
    await session.connect({ roomId: "room", accountId: 20, instrumentId: "BTC", intervalMs: 1000 });
    assert.equal(session.getSnapshot().snapshot?.observation.account?.cash_balance, 1000);
    revoked = true; await session.refresh(); assert.equal(session.getSnapshot().snapshot, null);
    assert.equal(session.getSnapshot().status, "error");
  } finally { session.stop(); }
});
