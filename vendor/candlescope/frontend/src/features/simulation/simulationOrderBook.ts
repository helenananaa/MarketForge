import { useEffect, useMemo, useState } from "react";
import { createOrderBookStore } from "../order-book/orderBookStore.js";
import { useOrderBookPreferences } from "../order-book/orderBookPreferencesStore.js";
import type { OrderBookBook, OrderBookRuntime } from "../order-book/orderBookTypes.js";
import { SIMULATION_CHART_EPOCH, type SimulationSnapshot } from "./simulationProtocol.js";

/** Adapt the authoritative room snapshot to CandleScope's existing book dock. */
export function simulationOrderBook(snapshot: SimulationSnapshot): OrderBookBook {
  const observation = snapshot.observation;
  const bids = observation.book.bids.map((level) => [level.price_tick, level.qty] as const);
  const asks = observation.book.asks.map((level) => [level.price_tick, level.qty] as const);
  const topBid = bids[0]?.[0] ?? null, topAsk = asks[0]?.[0] ?? null;
  const midPrice = topBid !== null && topAsk !== null ? (topBid + topAsk) / 2 : null;
  const spread = topBid !== null && topAsk !== null ? topAsk - topBid : null;
  return {
    mode: "partial", identity: { exchange: "marketforge", marketType: "simulation", symbol: observation.instrument_id },
    topic: observation.room_id, eventTimeMs: SIMULATION_CHART_EPOCH * 1000 + observation.market_time_ms,
    receivedAtMs: snapshot.receivedAt, source: "marketforge.snapshot", sequence: observation.step, revision: observation.step,
    bids, asks, topBid, topAsk, midPrice, spread, spreadBps: midPrice && spread !== null ? spread / midPrice * 10_000 : null,
    notionalImbalance: null, updateIntervalMs: null, depthLevels: null, outputLimit: null,
    bookBidLevels: bids.length, bookAskLevels: asks.length, priceTickSize: 1, priceStep: 1, priceGrouping: "raw",
    aggregationApplied: false, bucketBidLevels: bids.length, bucketAskLevels: asks.length,
  };
}

export function useSimulationOrderBook(snapshot: SimulationSnapshot | null, live: boolean, retry: () => void): OrderBookRuntime {
  const [store] = useState(createOrderBookStore);
  const { preferences, actions } = useOrderBookPreferences();
  const room = snapshot?.observation.room_id, symbol = snapshot?.observation.instrument_id ?? "";
  useEffect(() => { store.reset(); }, [store, room, symbol]);
  useEffect(() => {
    if (snapshot && live) store.publishBook(simulationOrderBook(snapshot));
    else store.publishStatus(snapshot ? "stale" : "idle");
  }, [store, snapshot, live]);
  useEffect(() => () => store.reset(), [store]);
  return useMemo(() => ({
    view: { identity: { exchange: "marketforge", marketType: "simulation", symbol }, supported: Boolean(snapshot),
      supportMessage: null, fullModeSupported: false, snapshotMode: "live_snapshot", preferences: { ...preferences, mode: "partial" },
      updateIntervalMs: preferences.updateIntervalMs, updateIntervalsMs: [], store },
    actions: { ...actions, retry }, status: { enabled: live },
  }), [actions, live, preferences, retry, snapshot, store, symbol]);
}
