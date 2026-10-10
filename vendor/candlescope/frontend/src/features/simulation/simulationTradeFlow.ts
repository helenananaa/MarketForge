import { useEffect, useMemo, useState } from "react";
import { createTradeFlowStore } from "../trade-flow/tradeFlowStore.js";
import { useTradeFlowPreferences } from "../trade-flow/tradeFlowPreferencesStore.js";
import type { AggregateTrade, TradeFlowRuntime } from "../trade-flow/tradeFlowTypes.js";
import { safeInteger, wireObject, SIMULATION_CHART_EPOCH, type SimulationSelection } from "./simulationProtocol.js";
import type { SimulationClient } from "./simulationClient.js";

export function simulationTrades(payload: unknown, selection: SimulationSelection): AggregateTrade[] {
  const root = wireObject(payload);
  if (root.room_id !== selection.roomId || !Array.isArray(root.ticks)) throw new Error("MarketForge trade identity mismatch");
  return root.ticks.map((entry) => {
    const trade = wireObject(entry);
    if (trade.room_id !== selection.roomId || trade.instrument_id !== selection.instrumentId) throw new Error("MarketForge trade identity mismatch");
    const price = safeInteger(trade.price_tick, 1), quantity = safeInteger(trade.qty, 1);
    const aggressorSide = trade.taker_side === "buy" ? "buy" as const : trade.taker_side === "sell" ? "sell" as const : null;
    if (!aggressorSide) throw new Error("Invalid MarketForge trade direction");
    const id = safeInteger(trade.trade_id);
    const time = SIMULATION_CHART_EPOCH * 1000 + safeInteger(trade.market_time_ms);
    const quoteQuantity = price * quantity;
    safeInteger(quoteQuantity);
    return { exchange: "marketforge", marketType: "simulation", symbol: selection.instrumentId,
      aggTradeId: id, tradeId: String(id), price, quantity, quoteQuantity, tradeTimeMs: time, eventTimeMs: time,
      receivedAtMs: Date.now(), aggressorSide, isBuyerMaker: aggressorSide === "sell", source: "marketforge.ticks",
      firstTradeId: id, lastTradeId: id, continuityMode: "observational" as const };
  }).sort((a, b) => a.aggTradeId - b.aggTradeId);
}

/** Reuse CandleScope tape/profile with an explicit bounded historical snapshot. */
export function useSimulationTradeFlow(client: SimulationClient, selection: SimulationSelection | null, interval: string, live: boolean): TradeFlowRuntime {
  const [store] = useState(() => createTradeFlowStore({ maxRecords: 500 }));
  const { preferences, actions } = useTradeFlowPreferences();
  const [retry, setRetry] = useState(0);
  const room = selection?.roomId, instrument = selection?.instrumentId;
  useEffect(() => {
    store.reset();
    if (!room || !instrument || !live) { store.publishStatus(live ? "idle" : "reconnecting"); return; }
    const selected = { roomId: room, instrumentId: instrument, accountId: 1, intervalMs: 1000 };
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      try {
        const payload = await client.request(`/rooms/${encodeURIComponent(room)}/ticks?instrument_id=${encodeURIComponent(instrument)}&limit=500`, AbortSignal.any([controller.signal, AbortSignal.timeout(10_000)]));
        if (!controller.signal.aborted) store.replaceRecent(simulationTrades(payload, selected));
      } catch (error) {
        if (!controller.signal.aborted) store.publishStatus("error", { error: error instanceof Error ? error.message : "Trade history failed" });
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(() => void poll(), 750);
      }
    };
    void poll();
    return () => { controller.abort(); if (timer) clearTimeout(timer); };
  }, [client, room, instrument, live, retry, store]);
  return useMemo(() => ({ view: {
    identity: { exchange: "marketforge", marketType: "simulation", symbol: instrument ?? "" }, interval,
    supported: Boolean(selection), supportMessage: null, continuityMode: "observational", deliveryMode: "polling_observational",
    preferences, store, markerSource: null,
  }, actions: { ...actions, retry: () => setRetry((value) => value + 1) }, status: { enabled: live } }), [actions, instrument, interval, live, preferences, selection, store]);
}
