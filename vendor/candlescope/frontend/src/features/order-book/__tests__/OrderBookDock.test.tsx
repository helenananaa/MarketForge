import assert from "node:assert/strict";
import test from "node:test";
import { t } from "../../../i18n/index.js";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import OrderBookDock from "../OrderBookDock.js";
import { snapshotDeliveryLabel } from "../orderBookDelivery.js";
import { createOrderBookStore } from "../orderBookStore.js";
import { DEFAULT_ORDER_BOOK_PREFERENCES } from "../orderBookPreferencesStore.js";
import type { OrderBookRuntime } from "../orderBookTypes.js";

test("dock renders stream failure detail and retry instead of generic support text", () => {
  const store = createOrderBookStore();
  const noop = () => undefined;
  const runtime: OrderBookRuntime = {
    view: {
      identity: { exchange: "okx", marketType: "futures", symbol: "BTC-USDT-SWAP" },
      supported: true, supportMessage: null, fullModeSupported: false,
      snapshotMode: "live_snapshot", preferences: DEFAULT_ORDER_BOOK_PREFERENCES,
      updateIntervalMs: 250, updateIntervalsMs: [250], store,
    },
    actions: { retry: noop, setHeight: noop, setCollapsed: noop, setMode: noop,
      setPartialDepth: noop, setUpdateIntervalMs: noop, setFullOutputLimit: noop, setPriceGrouping: noop },
    status: { enabled: true },
  };
  for (const status of ["stale", "reconnecting", "error"] as const) {
    store.publishStatus(status, { message: "检测到序列缺口，正在重新同步" });
    const html = renderToStaticMarkup(<OrderBookDock runtime={runtime} height={300} />);
    assert.match(html, /title="检测到序列缺口，正在重新同步"/);
    assert.match(html, /<span>检测到序列缺口，正在重新同步<\/span>/);
    assert.match(html, /<button type="button">/);
  }
  store.publishStatus("error", { error: "Subscription timed out" });
  assert.match(renderToStaticMarkup(<OrderBookDock runtime={runtime} height={300} />), /<span>Subscription timed out<\/span>/);
  store.destroy();
});


test("actual snapshot source takes precedence over advertised delivery capability", () => {
  assert.equal(snapshotDeliveryLabel("partial", "live_snapshot", "http"), t("orderBook.delivery.pollingSnapshot"));
  assert.equal(snapshotDeliveryLabel("partial", "polling_snapshot", "websocket"), t("orderBook.delivery.liveSnapshot"));
  assert.equal(snapshotDeliveryLabel("partial", "polling_snapshot"), t("orderBook.delivery.pollingSnapshot"));
  assert.equal(snapshotDeliveryLabel("full", "live_snapshot", "websocket"), t("orderBook.delivery.strictContinuous"));
});

const noopCancel = () => undefined;

test("price grouping menu only offers buckets up to a tenth of the price", () => {
  let flush: (() => void) | null = null;
  const store = createOrderBookStore({ request: (callback) => { flush = callback; return 1; }, cancel: noopCancel });
  const noop = () => undefined;
  const runtime: OrderBookRuntime = {
    view: {
      identity: { exchange: "marketforge", marketType: "spot", symbol: "V-BTC-SPOT" },
      supported: true, supportMessage: null, fullModeSupported: false,
      snapshotMode: "live_snapshot", preferences: DEFAULT_ORDER_BOOK_PREFERENCES,
      updateIntervalMs: 250, updateIntervalsMs: [250], store,
    },
    actions: { retry: noop, setHeight: noop, setCollapsed: noop, setMode: noop,
      setPartialDepth: noop, setUpdateIntervalMs: noop, setFullOutputLimit: noop, setPriceGrouping: noop },
    status: { enabled: true },
  };
  store.publishBook({
    mode: "partial", identity: runtime.view.identity, topic: "book", eventTimeMs: 1, receivedAtMs: 1,
    source: "websocket", sequence: 1, revision: 1, bids: [[100, 1]], asks: [[101, 1]],
    topBid: 100, topAsk: 101, midPrice: 100.5, spread: 1, spreadBps: 99.5, notionalImbalance: 0,
    updateIntervalMs: 250, depthLevels: 20, outputLimit: null, bookBidLevels: 1, bookAskLevels: 1,
    priceTickSize: 0.1, priceStep: 0.1, priceGrouping: "raw", aggregationApplied: false,
    bucketBidLevels: null, bucketAskLevels: null,
  });
  flush?.();
  const html = renderToStaticMarkup(<OrderBookDock runtime={runtime} height={300} />);
  // Tick 0.1 at price 100 allows steps up to 10, i.e. grouping 100.
  assert.match(html, /<option value="100"/);
  assert.doesNotMatch(html, /<option value="200"/);
  assert.doesNotMatch(html, /<option value="1000000000"/);
  assert.match(html, /<option value="auto"/);
  store.destroy();
});
