import { PositionRiskPanel } from "./PositionRiskPanel.js";
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import CompetitionPanel from "./CompetitionPanel.js";
import { roleLabel, type RoomContext, type RoomOverview } from "./roomPortalProtocol.js";
import type { SimulationConnection } from "./simulationProtocol.js";
import { useChartSettingsRuntime } from "../settings/chartAppearanceSettings.js";
import { t, getNumberLocale } from "../../i18n/index.js";
import { useLocale } from "../../i18n/useLocale.js";
import { SimulationClient } from "./simulationClient.js";
import { SimulationSession } from "./simulationSession.js";
import { SIMULATION_INTERVALS, simulationIntervalMs, elapsedLabel, type WireInteger } from "./simulationProtocol.js";
import SimulationChart from "./SimulationChart.js";
import MarketPageFrame from "../../app/MarketPageFrame.js";
import MarketTopBarFrame from "../../app/MarketTopBarFrame.js";
import MarketStatusBar from "../../app/MarketStatusBar.js";
import MarketRightRailFrame from "../../app/MarketRightRailFrame.js";
import { AccountRailIcon, ActivityRailIcon, CapabilityRailIcon, OrderBookRailIcon, PaperRailIcon, ProfileRailIcon, TapeRailIcon } from "../../app/marketRailIcons.js";
import type { MarketRailViewDescriptor } from "../../app/marketRailTypes.js";
import IntervalSelector from "../../components/IntervalSelector.js";
import { useCustomIntervals } from "../chart-session/customIntervalStore.js";
import type { CustomIntervalRecord, CreateCustomIntervalResult } from "../chart-session/chartSessionTypes.js";
import { groupIntervalsByDuration } from "../../utils/intervals.js";
import OrderBookDock from "../order-book/OrderBookDock.js";
import TradeFlowDock from "../trade-flow/TradeFlowDock.js";
import { useSimulationTradeFlow } from "./simulationTradeFlow.js";
import { useSimulationOrderBook } from "./simulationOrderBook.js";
import { SIMULATION_CHART_EPOCH } from "./simulationProtocol.js";
import "./simulation.css";

function message(error: unknown): string { return error instanceof Error ? error.message : "MarketForge request failed"; }

export default function SimulationApp({ connection, context, overview, onLeave, onManage }: { connection: SimulationConnection; context: RoomContext; overview: RoomOverview | null; onLeave: () => void; onManage: () => void }) {
  useLocale();
  const appearance = useChartSettingsRuntime();
  const roomId = context.room_id;
  const [accountId, setAccountId] = useState(String(context.trade_account_ids[0] ?? context.visible_account_ids[0] ?? 0));
  const [instrumentId, setInstrumentId] = useState(context.instruments[0]!);
  const [intervalMs, setIntervalMs] = useState(1000);

  const [session] = useState(() => new SimulationSession(new SimulationClient(connection)));
  const state = useSyncExternalStore(session.subscribe, session.getSnapshot, session.getSnapshot);
  const [uiError, setUiError] = useState<string | null>(null);
  const [entryTp, setEntryTp] = useState(""); const [entrySl,setEntrySl] = useState(""); const [entryLeg,setEntryLeg] = useState<"Both" | "Long" | "Short">("Both");
  const [orderKind, setOrderKind] = useState<"market" | "limit">("market");
  const [price, setPrice] = useState("100");
  const [qty, setQty] = useState("1");
  useEffect(() => () => session.stop(), [session]);
  const observation = state.snapshot?.observation;
  const disabled = state.busy;
  const allowedTrade = context.trade_account_ids.includes(Number(accountId));
  const canTrade = allowedTrade && state.status === "live" && observation?.status === "Running" && observation.account !== null && !disabled;
  const number = (value: WireInteger | undefined) => value === undefined ? "—" : new Intl.NumberFormat(getNumberLocale(), { maximumFractionDigits: 0 }).format(BigInt(value));
  const run = async (action: () => Promise<void>) => {
    setUiError(null);
    try { await action(); } catch (error) { setUiError(message(error)); }
  };
  useEffect(() => {
    void session.connect({ roomId, accountId: Number(accountId), instrumentId, intervalMs: 1000 });
    return () => session.stop();
  }, [session, roomId, accountId, instrumentId]);
  const changeInterval = async (ms: number) => {
    setIntervalMs(ms);
    if (state.selection) await run(() => session.connect({ ...state.selection!, intervalMs: ms }));
  };
  useEffect(() => { setEntryTp(""); setEntrySl(""); setEntryLeg("Both"); }, [roomId, accountId, instrumentId]);
  const account = observation?.account;
  const accountFields = [
    ["cash_balance", "simulation.cash"], ["available_cash", "simulation.available"], ["position_qty", "simulation.position"],
    ["reserved_cash", "simulation.reserved"], ["fees_paid", "simulation.fees"], ["equity", "simulation.equity"],
    ["initial_margin", "simulation.margin"], ["maintenance_margin", "simulation.maintenance"], ["unrealized_pnl", "simulation.unrealized"],
  ] as const;
  const customs = useCustomIntervals();
  const interval = SIMULATION_INTERVALS.find((entry) => entry.ms === intervalMs)?.label
    ?? customs.savedCustomIntervals.find((value) => { try { return simulationIntervalMs(value) === intervalMs; } catch { return false; } })
    ?? `${intervalMs / 1000}s`;
  const datasetKey = JSON.stringify([session.client.baseUrl, context.user_id, observation?.room_id ?? roomId, observation?.instrument_id ?? instrumentId, interval]);
  const [removedInterval, setRemovedInterval] = useState<CustomIntervalRecord | null>(null);
  const nativeIntervals = useMemo(() => SIMULATION_INTERVALS.map((entry) => ({ value: entry.label, label: entry.label, seconds: entry.ms / 1000 })), []);
  const intervalGroups = groupIntervalsByDuration([
    ...nativeIntervals.map((entry) => ({ ...entry, isCustom: false })),
    ...customs.savedCustomIntervals.filter((value) => { try { simulationIntervalMs(value); return true; } catch { return false; } }).map((value) => ({ value, label: value, seconds: simulationIntervalMs(value) / 1000, isCustom: true })),
  ]);
  const createInterval = (value: string): CreateCustomIntervalResult => {
    try {
      simulationIntervalMs(value);
      const result = customs.addCustomInterval(value, { markUsed: true });
      if (!result.ok) return { ok: false, message: t("simulation.customInterval") };
      void changeInterval(simulationIntervalMs(value));
      return { ok: true, added: result.added };
    } catch (error) { return { ok: false, message: message(error) }; }
  };
  const [openViews, setOpenViews] = useState<string[]>(context.capabilities.read_all_accounts ? ["competition", "overview", "ticket", "book"] : ["competition", "ticket", "book"]);
  const [panelCollapsed, setPanelCollapsed] = useState(false);
  const [railWidth, setRailWidth] = useState(320);
  const [viewHeights, setViewHeights] = useState<Record<string, number>>({});
  const toggleView = (id: string) => {
    if (panelCollapsed) { setPanelCollapsed(false); setOpenViews((previous) => previous.includes(id) ? previous : [...previous, id]); }
    else setOpenViews((previous) => previous.includes(id) ? previous.filter((value) => value !== id) : [...previous, id]);
  };
  const closeView = (id: string) => setOpenViews((previous) => previous.filter((value) => value !== id));
  const tradeFlow = useSimulationTradeFlow(session.client, state.selection, interval, state.status === "live" && !panelCollapsed && (openViews.includes("tape") || openViews.includes("profile")));
  const orderBook = useSimulationOrderBook(state.snapshot, state.status === "live", () => { void run(session.refresh); });
  const views: MarketRailViewDescriptor[] = [
    ...(overview?.competition ? [{ id: "competition", title: "比赛状态与排名", icon: <ActivityRailIcon />, order: 5, sizing: "fixed" as const, defaultHeight: 220, minHeight: 140, maxHeight: 800 }] : []),
    ...(context.capabilities.read_all_accounts ? [{ id: "overview", title: "房间总览（只读）", icon: <CapabilityRailIcon />, order: 10, sizing: "fixed" as const, defaultHeight: 280, minHeight: 180, maxHeight: 800 }] : []),
    { id: "ticket", title: allowedTrade ? t("simulation.ticket") : "账户（只读）", icon: <PaperRailIcon />, order: 20, sizing: "fixed", defaultHeight: 400, minHeight: 220, maxHeight: 800, collapsedSummary: t("simulation.assets"), badge: state.selection?.accountId ?? accountId },
    { id: "book", title: t("simulation.book"), icon: <OrderBookRailIcon />, order: 30, sizing: "fixed", defaultHeight: 360, minHeight: 220, maxHeight: 800 },
    { id: "orders", title: t("simulation.orders"), icon: <AccountRailIcon />, order: 40, sizing: "fixed", defaultHeight: 220, minHeight: 150, maxHeight: 800, badge: observation?.own_orders.length ?? 0 },
    { id: "tape", title: t("simulation.tape"), icon: <TapeRailIcon />, order: 50, sizing: "fixed", defaultHeight: 360, minHeight: 220, maxHeight: 800 },
    { id: "profile", title: t("simulation.profile"), icon: <ProfileRailIcon />, order: 60, sizing: "fixed", defaultHeight: 420, minHeight: 260, maxHeight: 1000 },
    { id: "market", title: "连接状态", icon: <ActivityRailIcon />, order: 70, sizing: "fixed", defaultHeight: 180, minHeight: 140, maxHeight: 500 },
  ];
  const overviewPanel = <div className="simulation-rail-content" data-testid="room-overview">
    <p>{roleLabel(context.role)} · 全局只读信息</p>
    {overview?.markets.map((market) => <section key={market.instrument_id}><h3>{market.instrument_id}</h3>
      <table><thead><tr><th>账户</th><th>资金</th><th>持仓</th></tr></thead><tbody>{Object.values(market.accounts).flat().map((a) => <tr key={String(a.account_id)}><td>{String(a.account_id)}</td><td>{String(a.cash_balance ?? a.equity ?? "—")}</td><td>{String(a.position_qty ?? "—")}</td></tr>)}</tbody></table>
      <details><summary>当前委托：{market.orders.length}（只读）</summary><table><thead><tr><th>账户</th><th>方向</th><th>价格 / 数量</th></tr></thead><tbody>{market.orders.map((order) => <tr key={String(order.order_id)}><td>{String(order.account_id)}</td><td>{order.side === "Buy" ? "买" : "卖"}</td><td>{String(order.price_tick)} / {String(order.remaining_qty)}</td></tr>)}</tbody></table></details>
    </section>)}
    {overview?.bots && <p>Bot：{overview.bots.agents.length} · {overview.bots.status.lifecycle} · {overview.bots.status.running ? "运行中" : "已停止"}</p>}
    {overview?.bots && <details><summary>Bot 策略参数（只读）</summary><pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", fontSize: 11 }}>{JSON.stringify(overview.bots.agents, null, 2)}</pre></details>}
    {context.capabilities.manage_bots && <button onClick={onManage}>进入房间管理</button>}
  </div>;
  const ticketPanel = <div className="simulation-rail-content">
        <section className="simulation-panel simulation-ticket"><label>账户<select aria-label="选择账户" value={accountId} disabled={disabled} onChange={(event) => { setIntervalMs(1000); setAccountId(event.target.value); }}>{!context.visible_account_ids.length && <option value="0">公共行情</option>}{context.visible_account_ids.map((id) => <option value={id} key={id}>#{id}{context.trade_account_ids.includes(id) ? " · 可交易" : " · 只读"}</option>)}</select></label><h2>{allowedTrade ? t("simulation.ticket") : "账户查看"} <small>{state.selection?.accountId ?? accountId}</small></h2>{allowedTrade ? <><div className="simulation-order-kind"><button aria-pressed={orderKind === "market"} onClick={() => setOrderKind("market")}>{t("simulation.market")}</button><button aria-pressed={orderKind === "limit"} onClick={() => setOrderKind("limit")}>{t("simulation.limit")}</button></div>
          {orderKind === "limit" && <label>{t("simulation.price")}<input aria-label={t("simulation.price")} type="number" min="1" step="1" value={price} disabled={disabled} onChange={(event) => setPrice(event.target.value)} /></label>}
          <label>{t("simulation.quantity")}<input aria-label={t("simulation.quantity")} type="number" min="1" step="1" value={qty} disabled={disabled} onChange={(event) => setQty(event.target.value)} /></label>
          {observation?.marketType === "perp" && <><label>开仓方向<select aria-label="开仓方向" value={entryLeg} onChange={(e) => setEntryLeg(e.target.value as "Both" | "Long" | "Short")}><option value="Both">单向持仓</option><option value="Long">多头仓位</option><option value="Short">空头仓位</option></select></label><label>开仓止盈价<input aria-label="开仓止盈价" type="number" min="1" step="1" value={entryTp} onChange={(e) => setEntryTp(e.target.value)} /></label><label>开仓止损价<input aria-label="开仓止损价" type="number" min="1" step="1" value={entrySl} onChange={(e) => setEntrySl(e.target.value)} /></label></>}
          <div className="simulation-trade-actions"><button className="simulation-buy" disabled={!canTrade} onClick={() => run(() => session.order("Buy", Number(qty), orderKind === "market" ? null : Number(price), observation?.marketType === "perp" && (entryTp || entrySl) ? { take_profit_tick: entryTp ? Number(entryTp) : null, stop_loss_tick: entrySl ? Number(entrySl) : null, trigger: "Mark" } : undefined, observation?.marketType === "perp" ? entryLeg : "Both"))}>{t("simulation.buy")}</button><button className="simulation-sell" disabled={!canTrade} onClick={() => run(() => session.order("Sell", Number(qty), orderKind === "market" ? null : Number(price), observation?.marketType === "perp" && (entryTp || entrySl) ? { take_profit_tick: entryTp ? Number(entryTp) : null, stop_loss_tick: entrySl ? Number(entrySl) : null, trigger: "Mark" } : undefined, observation?.marketType === "perp" ? entryLeg : "Both"))}>{t("simulation.sell")}</button></div>
          {state.busy && <p role="status">{t("simulation.pending")}</p>}
          {state.receipt && <p role="status" data-testid="simulation-receipt" data-accepted={state.receipt.accepted}>{t(state.receipt.accepted ? "simulation.accepted" : "simulation.rejected", { sequence: String(state.receipt.command_seq), reason: state.receipt.reject_reason ?? "—" })}</p>}
        </> : <p>此账户仅可查看</p>}</section>
        {observation?.marketType === "perp" && state.selection && <PositionRiskPanel key={`${state.selection.roomId}:${state.selection.instrumentId}:${state.selection.accountId}`} observation={observation} selection={state.selection} client={session.client} canTrade={allowedTrade} busy={!canTrade} onProtect={(side,spec) => { void run(() => session.protect(side,spec)); }} />}
        <section className="simulation-panel simulation-account" data-testid="simulation-account"><h2>{t("simulation.assets")}</h2>{account ? <dl>{accountFields.filter(([field]) => field in account).map(([field, label]) => <div key={field}><dt>{t(label)}</dt><dd data-field={field}>{number(account[field])}</dd></div>)}</dl> : <p>{t("simulation.noAccount")}</p>}</section>
  </div>;
  const ordersPanel = <div className="simulation-rail-content">
    <div className="simulation-bottom"><section className="simulation-panel simulation-orders"><h2>{t("simulation.orders")}</h2><div data-testid="simulation-orders">
      {observation?.own_orders.length ? observation.own_orders.map((order) => <div className="simulation-open-order" key={String(order.order_id)} data-order-id={String(order.order_id)}><span>#{String(order.order_id)}</span><span className={order.side === "Buy" ? "simulation-buy-text" : "simulation-sell-text"}>{t(order.side === "Buy" ? "simulation.buy" : "simulation.sell")}</span><span>{number(order.price_tick)} × {number(order.remaining_qty)}</span><button disabled={!allowedTrade || state.status !== "live" || disabled || observation.status === "Closed"} onClick={() => run(() => session.cancel(order.order_id))}>{t("simulation.cancel")}</button></div>) : <p>{t("simulation.noOrders")}</p>}
    </div></section><section className="simulation-panel simulation-trades"><h2>{t("simulation.trades")}</h2><div data-testid="simulation-trades">{observation?.public_trades.slice(-12).reverse().map((trade) => <div key={String(trade.trade_id)} className={trade.taker_side === "Buy" ? "simulation-buy-text" : "simulation-sell-text"}><span>#{String(trade.trade_id)}</span><span>{number(trade.price_tick)}</span><span>{number(trade.qty)}</span></div>)}</div></section></div>
  </div>;
  const marketPanel = <div className="simulation-rail-content"><div className="simulation-status">
      <span className={`simulation-dot ${state.status}`} />
      <strong>{t(state.status === "live" ? "simulation.live" : state.status === "error" ? "simulation.stale" : state.status === "connecting" ? "simulation.connecting" : "simulation.idle")}</strong>
      <span data-testid="simulation-transport">{t(`simulation.transport.${state.transport}`)}</span>
      <span data-testid="simulation-storage">{t(`simulation.storage.${state.storage}`)}</span>
      {observation && <><span>{observation.room_id} · {observation.instrument_id}</span><span>{t(`simulation.${observation.status}`)}</span><span>{t("simulation.clock")} {elapsedLabel(observation.market_time_ms / 1000)}</span><span>{t("simulation.stepNumber")} {number(observation.step)}</span></>}
  </div>
      <div className="simulation-market-actions"><button disabled={!state.selection || disabled} onClick={() => run(session.refresh)}>{t("simulation.refresh")}</button>
      </div>
  </div>;
  const rightRail = <MarketRightRailFrame source="simulation" views={views} openViewIds={openViews} panelCollapsed={panelCollapsed}
    onToggleView={toggleView} onTogglePanelCollapsed={() => setPanelCollapsed((previous) => !previous)}
    layout={{ width: railWidth, onWidthChange: setRailWidth }} viewHeights={viewHeights}
    onViewHeightChange={(id, height) => setViewHeights((previous) => ({ ...previous, [id]: height }))}
    upColor={appearance.settings.upColor} downColor={appearance.settings.downColor}
    renderView={(id, height) => {
      if (id === "competition") return <CompetitionPanel client={session.client} context={context} compact />;
      if (id === "overview") return overviewPanel;
      if (id === "ticket") return ticketPanel;
      if (id === "orders") return ordersPanel;
      if (id === "market") return marketPanel;
      if (id === "book") return <OrderBookDock runtime={orderBook} height={height} {...(allowedTrade ? { onSelectPrice: (value: number) => { if (!disabled) { setOrderKind("limit"); setPrice(String(value)); setOpenViews((previous) => previous.includes("ticket") ? previous : [...previous, "ticket"]); } } } : {})} onRequestClose={() => closeView(id)} />;
      if (id === "tape" || id === "profile") return <TradeFlowDock runtime={tradeFlow} height={height} mode={id}
        clockMs={SIMULATION_CHART_EPOCH * 1000 + (observation?.market_time_ms ?? 0)}
        timeFormatter={(ms) => elapsedLabel(ms / 1000 - SIMULATION_CHART_EPOCH)}
        notionalFormatter={(value) => `${value.toLocaleString(getNumberLocale())} tick·lot`} onRequestClose={() => closeView(id)} />;
      return null;
    }} />;
  return <div className="simulation-page">
    <MarketPageFrame
      topBar={<MarketTopBarFrame source="simulation" taskNavigation={<div className="simulation-room-navigation"><button className="simulation-theme-button" onClick={onLeave}>返回大厅</button>{context.capabilities.manage_bots && <button className="simulation-theme-button simulation-manage-button" onClick={onManage}>房间管理</button>}</div>}
        identity={<div className="simulation-identity"><strong>{observation?.instrument_id ?? "MarketForge"}</strong><span>{roomId} · {roleLabel(context.role)} · {context.display_name ?? context.user_id}</span></div>}
        trailing={<>{context.instruments.length > 1 && <select aria-label="选择市场" disabled={disabled} value={instrumentId} onChange={(event) => { setIntervalMs(1000); setInstrumentId(event.target.value); }}>{context.instruments.map((id) => <option key={id} value={id}>{id}</option>)}</select>}<button className="simulation-theme-button" onClick={() => appearance.setSettings((previous) => ({ ...previous, theme: appearance.resolvedTheme === "dark" ? "light" : "dark" }))}>{t("simulation.theme")}</button></>} />}
      intervalSelector={<IntervalSelector interval={interval} capabilityReady={!disabled} capabilityLoading={disabled}
        nativeIntervals={nativeIntervals} intervalGroups={intervalGroups} customIntervalRecords={customs.customIntervalRecords}
        savedCustomIntervals={customs.savedCustomIntervals} onSelectInterval={(value) => { customs.markIntervalUsed(value); void run(() => changeInterval(simulationIntervalMs(value))); }}
        onCreateCustomInterval={createInterval} onRemoveCustomInterval={(value) => setRemovedInterval(customs.removeCustomInterval(value))}
        onRestoreCustomInterval={() => { if (removedInterval) customs.restoreCustomInterval(removedInterval); }}
        onTogglePinCustomInterval={customs.togglePinCustomInterval} onClearCustomIntervals={() => { const removed = customs.clearCustomIntervals(); setRemovedInterval(removed.at(-1) ?? null); }}
        intervalAvailability={(value) => { try { simulationIntervalMs(value); return true; } catch { return false; } }} intervalNotice={null} />}
      workspace={<SimulationChart key={datasetKey} datasetKey={datasetKey} symbol={observation?.instrument_id ?? "MarketForge"} interval={interval}
        bars={state.snapshot?.bars ?? []} appearance={appearance} stale={state.status === "error"} client={session.client} selection={state.selection}
        rightRail={rightRail} tradeFlow={tradeFlow} />}
      featureSurfaces={<>
    {(uiError || state.error || state.actionError) && <div className="simulation-error" role="alert">{uiError ?? state.actionError ?? state.error}{state.actionError && <span>{t("simulation.unconfirmed")}</span>}</div>}
      </>}
      statusBar={<MarketStatusBar source="simulation" connectionStatus={state.status}
        dataAttributes={{ "data-testid": "simulation-status" }} left={<>
      <span className={`simulation-dot ${state.status}`} />
      <strong>{t(state.status === "live" ? "simulation.live" : state.status === "error" ? "simulation.stale" : state.status === "connecting" ? "simulation.connecting" : "simulation.idle")}</strong>
      <span data-testid="simulation-transport">{t(`simulation.transport.${state.transport}`)}</span>
      <span data-testid="simulation-storage">{t(`simulation.storage.${state.storage}`)}</span>
      {observation && <><span>{observation.room_id} · {observation.instrument_id}</span><span>{t(`simulation.${observation.status}`)}</span><span>{t("simulation.clock")} {elapsedLabel(observation.market_time_ms / 1000)}</span><span>{t("simulation.stepNumber")} {number(observation.step)}</span></>}
        </>} right={<span>{t("simulation.authority")}</span>} />}
    />
  </div>;
}
