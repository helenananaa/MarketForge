import assert from "node:assert/strict";
import test from "node:test";
import { simulationOrderBook } from "../simulationOrderBook.js";
import { SIMULATION_CHART_EPOCH, type SimulationSnapshot } from "../simulationProtocol.js";

function snapshot(): SimulationSnapshot {
  return { receivedAt: 12345, bars: [], observation: {
    room_id: "room", instrument_id: "BTC", status: "Paused", step: 8, market_time_ms: 8000,
    book: { bids: [{ price_tick: 99, qty: 4 }], asks: [{ price_tick: 101, qty: 6 }] },
    own_orders: [], public_trades: [], account: null, marketType: "spot",
  } };
}

test("shared book dock receives room levels and simulation time without advancing the room", () => {
  const source = snapshot(), before = structuredClone(source);
  const book = simulationOrderBook(source);
  assert.deepEqual(book.bids, [[99, 4]]);
  assert.deepEqual(book.asks, [[101, 6]]);
  assert.equal(book.midPrice, 100);
  assert.equal(book.spread, 2);
  assert.equal(book.spreadBps, 200);
  assert.equal(book.eventTimeMs, SIMULATION_CHART_EPOCH * 1000 + 8000);
  assert.equal(book.receivedAtMs, 12345);
  assert.equal(book.identity.symbol, "BTC");
  assert.deepEqual(source, before);
});

test("empty and one-sided books keep missing quotes unknown", () => {
  const source = snapshot();
  source.observation.book.asks = [];
  assert.equal(simulationOrderBook(source).topAsk, null);
  assert.equal(simulationOrderBook(source).spreadBps, null);
  source.observation.book.bids = [];
  const book = simulationOrderBook(source);
  assert.equal(book.topBid, null);
  assert.equal(book.midPrice, null);
  assert.equal(book.spread, null);
});
