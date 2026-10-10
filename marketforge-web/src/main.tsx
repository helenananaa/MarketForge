import { StrictMode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Bot,
  ExternalLink,
  Play,
  Plus,
  RefreshCw,
  Send,
  Square,
} from "lucide-react";
import "./styles.css";
import { AgentTraders } from "./AgentTraders";
import backgroundMarket from "../../scripts/fixtures/background_market.json";
import behaviorMarket from "../../scripts/fixtures/behavior_market.json";
import microstructureMarket from "../../scripts/fixtures/microstructure_market.json";
import linkedMarket from "../../scripts/fixtures/linked_market.json";

type Side = "Buy" | "Sell";
type AgentStatus = {
  room_id: string;
  running: boolean;
  interval_ms: number;
  participants: string[];
  lifecycle?: string;
  last_error?: string | null;
};
type BotParameter = {
  type: "integer" | "boolean" | "string" | "object" | "array";
  required: boolean;
  default: unknown;
  minimum?: number | null;
  maximum?: number | null;
  choices: unknown[];
};
type BotDescriptor = {
  id: string;
  name: string;
  version: string;
  state_version: number;
  parameters: Record<string, BotParameter>;
};
type BotInstance = { Plugin: {
  participant: { participant_id: string; kind: "RuleAgent"; room_id: string; account_id: number; instrument_id: string };
  plugin_id: string; plugin_version: string; state_version: number; seed: number; config: Record<string, unknown>;
} };
type BookLevel = {
  price_tick: number;
  qty: number;
};
type SpotAccount = {
  account_id: number;
  cash_balance: number;
  position_qty: number;
  fees_paid: number;
  available_cash?: number;
};
type PerpAccount = SpotAccount & {
  hedge_positions?: { long: { qty: number; avg_entry_price_tick: number }; short: { qty: number; avg_entry_price_tick: number } };
  equity: number;
  realized_pnl: number;
  unrealized_pnl: number;
  initial_margin: number;
  funding_pnl?: number;
};
type AnyAccount = SpotAccount | PerpAccount;
type AccountSnapshots = { Spot: SpotAccount[] } | { Perp: PerpAccount[] };
type MarketView = {
  room_id: string;
  instrument_id: string;
  status: "Running" | "Paused" | "Closed";
  book: {
    bids: BookLevel[];
    asks: BookLevel[];
  };
  accounts: AccountSnapshots;
  instruments?: string[];
  perp_price?: {
    spot_instrument_id: string;
    index_price_tick: number | null;
    mark_price_tick: number;
    status: "awaiting_price" | "live" | "stale" | "unavailable";
    funding?: {
      market_time_ms: number;
      estimated_rate_ppm: number | null;
      next_funding_time_ms: number;
      last_settlement: { status: "settled" | "no_positions" | "skipped_prices" | "unbalanced_positions"; rate_ppm: number; covered_ms: number; interval_ms: number } | null;
    };
  };
};
type ApiEvent = {
  type: string;
  seq: number;
  order_id?: number;
  trade_id?: number;
  price_tick?: number;
  qty?: number;
  remaining_qty?: number;
  unfilled_qty?: number;
  reason?: string;
  taker_side?: Side;
};
type OrderResponse = {
  participant_id: string;
  account_id: number;
  room_id: string;
  command_seq: number;
  status: string;
  accepted: boolean;
  reject_reason: string | null;
  events: ApiEvent[];
  clearing_event_count: number;
};
type RoomExecutionSummary = {
  room_id: string;
  instrument_id?: string | null;
  market_time_ms?: number | null;
  command_seq: number;
  status: string;
  accepted: boolean;
  reject_reason: string | null;
  events: ApiEvent[];
  clearing_event_count: number;
};
type RoomEventsResponse = {
  room_id: string;
  executions: RoomExecutionSummary[];
};
type TimelineRow = {
  execution: RoomExecutionSummary;
  event?: ApiEvent;
};
type Trade = {
  key: string;
  command_seq: number;
  price_tick: number;
  qty: number;
  taker_side?: Side;
};
type RoomTradesResponse = {
  room_id: string;
  trades: {
    instrument_id: string;
    trade_id: number;
    command_seq: number;
    price_tick: number;
    qty: number;
    taker_side: "buy" | "sell";
  }[];
};
type LogEntry = {
  id: string;
  level: "ok" | "warn" | "info";
  text: string;
};

const API_DEFAULT = "http://127.0.0.1:57305";
const ROOM_DEFAULT = "demo-web";
// Served by scripts/start-candlescope-workbench.ps1; see docs/CANDLESCOPE_WORKBENCH.md.
const WORKBENCH_URL = "http://127.0.0.1:15173/simulation.html";
const BOOK_DEPTH = 8;
const STATUS_LABELS: Record<MarketView["status"], string> = { Running: "运行中", Paused: "已暂停", Closed: "已关闭" };
const LOG_LABELS: Record<LogEntry["level"], string> = { ok: "成功", warn: "失败", info: "提示" };

function App() {
  const [apiBase, setApiBase] = useState(API_DEFAULT);
  const [roomId, setRoomId] = useState(ROOM_DEFAULT);
  const [activeRoom, setActiveRoom] = useState("");
  const [view, setView] = useState<MarketView | null>(null);
  const [activeInstrument, setActiveInstrument] = useState("");
  const [linkedMarkets, setLinkedMarkets] = useState(true);
  const [enhancedBehaviors, setEnhancedBehaviors] = useState(true);
  const [microstructureBehaviors, setMicrostructureBehaviors] = useState(false);
  const refreshRequest = useRef(0);
  const [agentStatus, setAgentStatus] = useState<AgentStatus | null>(null);
  const [roomEvents, setRoomEvents] = useState<RoomExecutionSummary[]>([]);
  const [trades, setTrades] = useState<Trade[]>([]);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [side, setSide] = useState<Side>("Buy");
  const [positionSide, setPositionSide] = useState<"Long" | "Short">("Long");
  const [orderKind, setOrderKind] = useState<"Limit" | "Market">("Limit");
  const [price, setPrice] = useState(100);
  const [qty, setQty] = useState(2);
  const [accountId, setAccountId] = useState(20);
  const [bots, setBots] = useState<BotDescriptor[]>([]);
  const [botId, setBotId] = useState("DcaTrader");
  const [botParams, setBotParams] = useState<Record<string, string>>({});
  const [botAccount, setBotAccount] = useState(30);
  const [botSeed, setBotSeed] = useState(1);
  const [botInstanceId, setBotInstanceId] = useState("bot-1");
  const [botInstances, setBotInstances] = useState<BotInstance[]>([]);
  const selectedBot = bots.find((bot) => bot.id === botId);


  const accounts = useMemo(() => flattenAccounts(view?.accounts), [view]);
  const selectedAccount = accounts.find((account) => account.account_id === accountId);
  const hedgePositions = selectedAccount && isPerpAccount(selectedAccount) ? selectedAccount.hedge_positions : undefined;
  const closingLeg = !!hedgePositions && ((positionSide === "Long" && side === "Sell") || (positionSide === "Short" && side === "Buy"));
  const quantityUnit = view && "Perp" in view.accounts ? "张" : "单位";
  const funding = view?.perp_price?.funding;
  const bestBid = view?.book.bids[0]?.price_tick;
  const bestAsk = view?.book.asks[0]?.price_tick;
  const lastPrice = trades[0]?.price_tick;
  const priceTone = trades.length > 1 ? priceDirection(trades[0].price_tick, trades[1].price_tick) : undefined;
  const spread = bestAsk !== undefined && bestBid !== undefined ? bestAsk - bestBid : undefined;
  const latestLog = logs[0];

  const pushLog = useCallback((entry: Omit<LogEntry, "id">) => {
    setLogs((current) => [
      { ...entry, id: `${Date.now()}-${Math.random()}` },
      ...current.slice(0, 31),
    ]);
  }, []);

  const api = useCallback(
    async <T,>(path: string, init?: RequestInit): Promise<T> => {
      const response = await fetch(`${apiBase}${path}`, {
        ...init,
        headers: {
          "content-type": "application/json",
          ...(init?.headers ?? {}),
        },
      });
      if (!response.ok) {
        const body = await response
          .json()
          .catch(() => ({ error: response.statusText }));
        throw new Error(body.error ?? response.statusText);
      }
      return response.json() as Promise<T>;
    },
    [apiBase],
  );

  useEffect(() => {
    let cancelled = false;
    api<BotDescriptor[]>("/bots").then((descriptors) => {
      if (!cancelled) setBots(descriptors);
    }).catch((error: Error) => {
      if (!cancelled) { setBots([]); pushLog({ level: "warn", text: error.message }); }
    });
    return () => { cancelled = true; };
  }, [api, pushLog]);

  const configureBot = (id: string) => {
    setBotId(id);
    setBotParams({});
  };

  const makeBotInstance = (): BotInstance => {
    if (!selectedBot || !activeRoom) throw new Error("请先载入房间并选择交易 bot");
    if (!botInstanceId.trim()) throw new Error("请填写 bot 实例名称");
    if (!Number.isSafeInteger(botAccount) || botAccount <= 0 || !Number.isSafeInteger(botSeed) || botSeed < 0) {
      throw new Error("账户和种子必须是有效整数");
    }
    const config: Record<string, unknown> = {};
    for (const [name, parameter] of Object.entries(selectedBot.parameters)) {
      const raw = botParams[name];
      if (raw === undefined || raw === "") {
        if (parameter.default !== null && parameter.default !== undefined) config[name] = parameter.default;
        else if (parameter.required) throw new Error(`请填写参数 ${name}`);
        continue;
      }
      const value = parameter.type === "string" ? raw : JSON.parse(raw);
      if (parameter.type === "integer" && !Number.isSafeInteger(value)) throw new Error(`${name} 必须是整数`);
      config[name] = value;
    }
    return { Plugin: {
      participant: { participant_id: botInstanceId.trim(), kind: "RuleAgent", room_id: activeRoom, account_id: botAccount, instrument_id: view?.instrument_id ?? "V-BTC-SPOT" },
      plugin_id: selectedBot.id, plugin_version: selectedBot.version, state_version: selectedBot.state_version, seed: botSeed, config,
    } };
  };

  const addBotInstance = () => {
    try {
      const instance = makeBotInstance();
      if (botInstances.some((bot) => bot.Plugin.participant.participant_id === instance.Plugin.participant.participant_id)) {
        throw new Error("bot 实例名称重复");
      }
      setBotInstances([...botInstances, instance]);
      setBotInstanceId(`bot-${botInstances.length + 2}`);
    } catch (error) { pushLog({ level: "warn", text: error instanceof Error ? error.message : "添加失败" }); }
  };

  const refresh = useCallback(
    async (room = activeRoom, instrument = activeInstrument) => {
      if (!room) {
        return;
      }
      const request = ++refreshRequest.current;
      const viewRequest = api<MarketView>(instrument ? `/rooms/${room}/instruments/${encodeURIComponent(instrument)}/view` : `/rooms/${room}/view`);
      const [nextView, nextAgents, nextEvents, nextTrades] = await Promise.all([
        viewRequest,
        api<AgentStatus>(`/rooms/${room}/agents`),
        api<RoomEventsResponse>(`/rooms/${room}/events?limit=80`),
        // Query the selected instrument's latest trades independently of the room command window.
        viewRequest.then((nextView) => api<RoomTradesResponse>(
          `/rooms/${room}/instruments/${encodeURIComponent(nextView.instrument_id)}/trades?limit=80`,
        )),
      ]);
      if (request !== refreshRequest.current) return;
      setView(nextView);
      setActiveInstrument(nextView.instrument_id);
      setAgentStatus(nextAgents);
      setRoomEvents(nextEvents.executions);
      setTrades(nextTrades.trades.map((trade) => ({
        key: `${trade.instrument_id}-${trade.trade_id}`,
        command_seq: trade.command_seq,
        price_tick: trade.price_tick,
        qty: trade.qty,
        taker_side: trade.taker_side === "buy" ? "Buy" : "Sell",
      })));
    },
    [activeRoom, activeInstrument, api],
  );

  useEffect(() => {
    if (!activeRoom || !autoRefresh || busy) {
      return;
    }
    let cancelled = false;
    let handle: ReturnType<typeof window.setTimeout>;
    const poll = async () => {
      try {
        await refresh();
      } catch (error) {
        if (!cancelled) pushLog({ level: "warn", text: error instanceof Error ? error.message : String(error) });
      } finally {
        if (!cancelled) handle = window.setTimeout(poll, 900);
      }
    };
    handle = window.setTimeout(poll, 900);
    return () => { cancelled = true; window.clearTimeout(handle); };
  }, [activeRoom, autoRefresh, busy, pushLog, refresh]);

  const loadSavedBots = async (room: string) => {
    const saved = await api<{ agents: { template: BotInstance | Record<string, unknown> }[] } | null>(`/rooms/${room}/bots`);
    setBotInstances((saved?.agents ?? []).flatMap(({ template }) => "Plugin" in template ? [template as BotInstance] : []));
  };

  const createRoom = async () => {
    const nextRoom = roomId.trim() || ROOM_DEFAULT;
    setBusy(true);
    try {
      const payload = sampleRoomPayload(nextRoom, linkedMarkets, enhancedBehaviors, microstructureBehaviors);
      await api("/rooms", {
        method: "POST",
        body: JSON.stringify(payload),
      });
      setActiveRoom(nextRoom);
      await loadSavedBots(nextRoom);
      pushLog({ level: "ok", text: `房间 ${nextRoom} 已创建，${payload.agents.length} 个背景交易者已启动${linkedMarkets ? "，现货与永续已绑定" : ""}` });
      await refresh(nextRoom, "");
    } catch (error) {
      const message = error instanceof Error ? error.message : "create room failed";
      if (message.includes("RoomAlreadyExists")) {
        setActiveRoom(nextRoom);
        pushLog({ level: "info", text: `房间 ${nextRoom} 已载入` });
        await refresh(nextRoom, "");
        await loadSavedBots(nextRoom);
      } else {
        pushLog({ level: "warn", text: message });
      }
    } finally {
      setBusy(false);
    }
  };

  const loadRoom = async () => {
    const nextRoom = roomId.trim();
    if (!nextRoom) return;
    setBusy(true);
    try {
      await refresh(nextRoom, "");
      await loadSavedBots(nextRoom);
      setActiveRoom(nextRoom);
      pushLog({ level: "ok", text: `房间 ${nextRoom} 已载入` });
    } catch (error) {
      pushLog({ level: "warn", text: error instanceof Error ? error.message : "载入失败" });
    } finally { setBusy(false); }
  };

  const submitOrder = async () => {
    if (!activeRoom) {
      pushLog({ level: "warn", text: "请先创建或载入房间" });
      return;
    }
    setBusy(true);
    try {
      const action = hedgePositions
        ? orderKind === "Market"
          ? { PlaceUnboundedMarket: { side, qty, position_side: positionSide, reduce_only: closingLeg } }
          : { PlaceProtected: { side, qty, position_side: positionSide, price_tick: price, order_type: closingLeg ? "ImmediateOrCancel" : "Limit", reduce_only: closingLeg } }
        : orderKind === "Market"
          ? { PlaceMarket: { side, qty } }
          : { PlaceLimit: { side, price_tick: price, qty } };
      const response = await api<OrderResponse>(`/rooms/${activeRoom}/orders`, {
        method: "POST",
        body: JSON.stringify({
          participant_id: "human-web",
          instrument_id: activeInstrument,
          account_id: accountId,
          action,
        }),
      });
      pushLog({
        level: response.accepted ? "ok" : "warn",
        text: summarizeOrder(response),
      });
      await refresh();
    } catch (error) {
      pushLog({
        level: "warn",
        text: error instanceof Error ? error.message : "order failed",
      });
    } finally {
      setBusy(false);
    }
  };

  const startAi = async () => {
    if (!activeRoom) {
      pushLog({ level: "warn", text: "请先创建或载入房间" });
      return;
    }
    setBusy(true);
    try {
      const status = await api<AgentStatus>(`/rooms/${activeRoom}/agents`, {
        method: "POST",
        body: JSON.stringify({
          agents: botInstances.length ? botInstances : [makeBotInstance()],
          interval_ms: 700,
        }),
      });
      setAgentStatus(status);
      pushLog({ level: "ok", text: "交易 bot 已启动" });
      await refresh();
    } catch (error) {
      pushLog({
        level: "warn",
        text: error instanceof Error ? error.message : "start AI failed",
      });
    } finally {
      setBusy(false);
    }
  };

  const stopAi = async () => {
    if (!activeRoom) {
      return;
    }
    setBusy(true);
    try {
      const status = await api<AgentStatus>(`/rooms/${activeRoom}/agents/stop`, {
        method: "POST",
        body: "{}",
      });
      setAgentStatus(status);
      pushLog({ level: "info", text: "交易 bot 已停止" });
    } catch (error) {
      pushLog({
        level: "warn",
        text: error instanceof Error ? error.message : "stop AI failed",
      });
    } finally {
      setBusy(false);
    }
  };

  const switchInstrument = async (instrument: string) => {
    setBusy(true);
    try { await refresh(activeRoom, instrument); }
    catch (error) { pushLog({ level: "warn", text: error instanceof Error ? error.message : "切换失败" }); }
    finally { setBusy(false); }
  };

  return (
    <main className={`terminal${view?.perp_price ? " has-perp-price" : ""}`}>
      <div className="dev-banner" role="note">
        <span>
          这是开发调试页面，只显示后端原始数据。完整的看盘与交易请使用 CandleScope 仿真工作台。
        </span>
        <a href={WORKBENCH_URL} target="_blank" rel="noreferrer">
          打开仿真工作台
          <ExternalLink size={14} aria-hidden="true" />
        </a>
      </div>
      <header className="global-nav">
        <div className="brand">
          <span className="brand-mark">MF</span>
          <strong>MarketForge</strong>
          <span className="brand-tag">调试台</span>
        </div>
        <label className="api-field">
          <span>API 地址</span>
          <input
            value={apiBase}
            onChange={(event) => setApiBase(event.target.value)}
          />
        </label>
      </header>

      <AgentTraders room={activeRoom} instrument={view?.instrument_id ?? "V-BTC-SPOT"} />
      <section className="market-strip">
        <div className="symbol-block">
          <span className="coin">V</span>
          <div>
            <div className="symbol-line">
              <select aria-label="交易品种" value={activeInstrument} disabled={busy || !view} onChange={(event) => switchInstrument(event.target.value)}>
                {!view && <option value="">选择交易品种</option>}
                {(view?.instruments ?? (view ? [view.instrument_id] : [])).map((id) => <option key={id} value={id}>{id}</option>)}
              </select>
            </div>
            <span>{activeRoom || "未载入房间"}</span>
          </div>
        </div>
        <MarketStat label="最新成交价" value={formatNumber(lastPrice)} tone={priceTone} />
        <MarketStat label="最优买价" value={formatNumber(bestBid)} tone="up" />
        <MarketStat label="最优卖价" value={formatNumber(bestAsk)} tone="down" />
        <MarketStat label="价差" value={formatNumber(spread)} className="optional-stat" />
        <MarketStat label="状态" value={view ? STATUS_LABELS[view.status] : "-"} className="optional-stat" />
        <div className="room-controls">
          <input
            value={roomId}
            onChange={(event) => setRoomId(event.target.value)}
            aria-label="room id"
          />
          <button onClick={createRoom} disabled={busy}>
            <Plus size={16} aria-hidden="true" />
            创建仿真市场
          </button>
          <label className="auto-toggle">
            <input type="checkbox" checked={linkedMarkets} onChange={(event) => setLinkedMarkets(event.target.checked)} />
            绑定永续
          </label>
          <label className="auto-toggle">
            <input type="checkbox" checked={enhancedBehaviors} disabled={!linkedMarkets} onChange={(event) => setEnhancedBehaviors(event.target.checked)} />
            增强行为
          </label>
          <label className="auto-toggle">
            <input type="checkbox" checked={microstructureBehaviors} disabled={!linkedMarkets || !enhancedBehaviors} onChange={(event) => setMicrostructureBehaviors(event.target.checked)} />
            盘口反馈与杠杆
          </label>
          <button onClick={loadRoom} disabled={busy || !roomId.trim()}>载入</button>
          <button onClick={() => refresh()} disabled={!activeRoom || busy} aria-label="刷新" title="刷新">
            <RefreshCw size={16} aria-hidden="true" />
          </button>
          <label className="auto-toggle">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(event) => setAutoRefresh(event.target.checked)}
            />
            自动
          </label>
        </div>
      </section>

      {view?.perp_price && <section className="linked-price-strip" aria-label="现货永续联动">
        <MarketStat label="现货指数价" value={formatNumber(view.perp_price.index_price_tick ?? undefined)} />
        <MarketStat label="标记价" value={formatNumber(view.perp_price.mark_price_tick)} />
        <MarketStat label={`关联 ${view.perp_price.spot_instrument_id}`} value={{ live: "已联动", stale: "行情过期", unavailable: "行情不足", awaiting_price: "等待行情" }[view.perp_price.status]} />
        {funding && <>
          <MarketStat label="资金费率（预估）" value={funding.estimated_rate_ppm === null ? "等待行情" : `${funding.estimated_rate_ppm >= 0 ? "+" : ""}${(funding.estimated_rate_ppm / 10000).toFixed(4)}%`} />
          <MarketStat label="距结算（仿真时间）" value={`${Math.max(0, Math.ceil((funding.next_funding_time_ms - funding.market_time_ms) / 1000))} 秒`} />
          <MarketStat label="上次资金费结算" value={funding.last_settlement ? `${{ settled: "已结算", no_positions: "无持仓", skipped_prices: "行情不足，已跳过", unbalanced_positions: "持仓不平衡，已跳过" }[funding.last_settlement.status]} · 行情覆盖 ${(funding.last_settlement.covered_ms / funding.last_settlement.interval_ms * 100).toFixed(0)}%` : "尚未结算"} />
        </>}
      </section>}

      <div className="trade-grid">
        <section className="trades-pane" aria-labelledby="trades-title">
          <div className="pane-header">
            <h2 id="trades-title">最近成交</h2>
            <span className="pane-note">当前品种最近 80 笔成交 · K 线请在仿真工作台查看</span>
          </div>
          <TradeTape trades={trades} />
        </section>

        <section className="orderbook-pane" aria-labelledby="book-title">
          <div className="pane-header">
            <h2 id="book-title">订单簿</h2>
            <span className="pane-note">前 {BOOK_DEPTH} 档</span>
          </div>
          <OrderBook bids={view?.book.bids ?? []} asks={view?.book.asks ?? []} lastPrice={lastPrice} priceTone={priceTone} />
        </section>

        <aside className="ticket-pane" aria-label="下单">
          <div className="buy-sell-tabs">
            <button className={side === "Buy" ? "active" : ""} onClick={() => setSide("Buy")}>
              {hedgePositions ? positionSide === "Long" ? "开多" : "平空" : "买入"}
            </button>
            <button className={side === "Sell" ? "active sell" : ""} onClick={() => setSide("Sell")}>
              {hedgePositions ? positionSide === "Long" ? "平多" : "开空" : "卖出"}
            </button>
          </div>
          {hedgePositions && <div className="order-type-row" aria-label="选择持仓方向">
            <button className={positionSide === "Long" ? "active" : ""} onClick={() => setPositionSide("Long")}>多仓</button>
            <button className={positionSide === "Short" ? "active" : ""} onClick={() => setPositionSide("Short")}>空仓</button>
          </div>}
          <div className="order-type-row">
            <button className={orderKind === "Limit" ? "active" : ""} onClick={() => setOrderKind("Limit")}>
              {closingLeg ? "限价平仓（IOC）" : "限价委托"}
            </button>
            <button className={orderKind === "Market" ? "active" : ""} onClick={() => setOrderKind("Market")}>
              市价委托
            </button>
          </div>
          <TicketInput
            label="账户"
            value={accountId}
            onChange={setAccountId}
            suffix="ID"
          />
          <TicketInput
            label="价格"
            value={price}
            onChange={setPrice}
            suffix="TICK"
            disabled={orderKind === "Market"}
          />
          <TicketInput label="数量" value={qty} onChange={setQty} suffix={quantityUnit} />
          <div className="balance-lines">
            <span>可用现金</span>
            <strong>{selectedAccount?.available_cash ?? selectedAccount?.cash_balance ?? "-"}</strong>
            <span>{hedgePositions ? "净持仓数量" : "持仓数量"}</span>
            <strong>{selectedAccount?.position_qty ?? "-"} {quantityUnit}</strong>
            {hedgePositions && <>
              <span>多仓 / 开仓均价</span><strong>{hedgePositions.long.qty} / {hedgePositions.long.avg_entry_price_tick}</strong>
              <span>空仓 / 开仓均价</span><strong>{hedgePositions.short.qty} / {hedgePositions.short.avg_entry_price_tick}</strong>
            </>}
            {selectedAccount && "funding_pnl" in selectedAccount && <>
              <span>累计资金费收付</span>
              <strong>{selectedAccount.funding_pnl ?? 0}</strong>
            </>}
          </div>
          <button className={side === "Buy" ? "submit buy" : "submit sell"} onClick={submitOrder} disabled={busy || !activeRoom}>
            <Send size={17} aria-hidden="true" />
            {hedgePositions ? positionSide === "Long" ? side === "Buy" ? "开多" : "平多" : side === "Buy" ? "平空" : "开空" : side === "Buy" ? "买入" : "卖出"}
          </button>
          {latestLog && (
            <p className={`ticket-feedback ${latestLog.level}`} role={latestLog.level === "warn" ? "alert" : "status"}>
              {latestLog.text}
            </p>
          )}
          <div className="ai-box">
            <div className="ai-state">
              <Bot size={16} aria-hidden="true" />
              <strong>{agentStatus?.running ? "Bot 运行中" : "Bot 已停止"}</strong>
              <span>{agentStatus?.interval_ms ? `${agentStatus.interval_ms}ms` : "-"}</span>
            </div>
            <label className="bot-field">交易 bot
              <select aria-label="交易 bot" value={botId} onChange={(event) => configureBot(event.target.value)} disabled={busy || !bots.length}>
                {bots.map((bot) => <option key={bot.id} value={bot.id}>{bot.name} · {bot.version}</option>)}
              </select>
            </label>
            <label className="bot-field">实例名称
              <input aria-label="实例名称" value={botInstanceId} onChange={(event) => setBotInstanceId(event.target.value)} />
            </label>
            <TicketInput label="Bot 账户" value={botAccount} onChange={setBotAccount} suffix="ID" />
            <TicketInput label="随机种子" value={botSeed} onChange={setBotSeed} suffix="" />
            {Object.entries(selectedBot?.parameters ?? {}).map(([name, parameter]) => {
              const fallback = parameter.default === null || parameter.default === undefined ? "" : parameter.type === "string" ? String(parameter.default) : JSON.stringify(parameter.default);
              const value = botParams[name] ?? fallback;
              return <label className="bot-field" key={name}>{name}{parameter.required ? " *" : ""}
                {parameter.type === "boolean" || parameter.choices.length ?
                  <select aria-label={name} value={value} onChange={(event) => setBotParams({ ...botParams, [name]: event.target.value })}>
                    {(!value || parameter.default == null) && <option value="">请选择</option>}
                    {(parameter.type === "boolean" ? [true, false] : parameter.choices).map((choice) => {
                      const text = parameter.type === "string" ? String(choice) : JSON.stringify(choice);
                      return <option key={text} value={text}>{text}</option>;
                    })}
                  </select> : <input aria-label={name} value={value} type={parameter.type === "integer" ? "number" : "text"}
                    min={parameter.minimum ?? undefined} max={parameter.maximum ?? undefined}
                    onChange={(event) => setBotParams({ ...botParams, [name]: event.target.value })} />}
              </label>;
            })}
            <button className="bot-add" onClick={addBotInstance} disabled={busy || !activeRoom || !selectedBot}>添加到启动列表</button>
            {botInstances.map((bot) => <div className="bot-instance" key={bot.Plugin.participant.participant_id}>
              <span>{bot.Plugin.participant.participant_id} · {bot.Plugin.plugin_id} · 账户 {bot.Plugin.participant.account_id}</span>
              <button aria-label={`移除 ${bot.Plugin.participant.participant_id}`} onClick={() => setBotInstances(botInstances.filter((item) => item !== bot))}>移除</button>
            </div>)}
            {!!botInstances.length && <small>启动将应用整个列表；列表为空时启动当前配置。</small>}
            {agentStatus?.last_error && <p role="alert" className="bot-error">{agentStatus.last_error}</p>}
            <div className="ai-buttons">
              <button onClick={startAi} disabled={busy || !activeRoom || !selectedBot}>
                <Play size={16} aria-hidden="true" />
                启动
              </button>
              <button onClick={stopAi} disabled={busy || !activeRoom}>
                <Square size={16} aria-hidden="true" />
                停止
              </button>
            </div>
          </div>
        </aside>

        <section className="bottom-pane">
          <div className="data-section" aria-labelledby="events-title">
            <h2 id="events-title" className="section-title">房间事件与本地日志</h2>
            <ActivityTable executions={roomEvents} logs={logs} />
          </div>
          <div className="data-section" aria-labelledby="accounts-title">
            <h2 id="accounts-title" className="section-title">账户</h2>
            <AccountTable accounts={accounts} />
          </div>
        </section>
      </div>
    </main>
  );
}

function MarketStat({
  label,
  value,
  tone,
  className,
}: {
  label: string;
  value: string | number;
  tone?: "up" | "down";
  className?: string;
}) {
  return (
    <div className={className ? `market-stat ${className}` : "market-stat"}>
      <span>{label}</span>
      <strong className={tone ?? ""}>{value}</strong>
    </div>
  );
}

function TradeTape({ trades }: { trades: Trade[] }) {
  if (trades.length === 0) {
    return <div className="data-empty">暂无成交</div>;
  }
  return (
    <div className="trade-tape">
      <div className="tape-head">
        <span>命令</span>
        <span>价格</span>
        <span>数量</span>
        <span>主动方</span>
      </div>
      <div className="tape-rows">
        {trades.map((trade) => (
          <div className="tape-row" key={trade.key}>
            <span>#{trade.command_seq}</span>
            <strong className={trade.taker_side === "Buy" ? "up" : trade.taker_side === "Sell" ? "down" : ""}>
              {formatNumber(trade.price_tick)}
            </strong>
            <span>{trade.qty}</span>
            <span>{trade.taker_side === "Buy" ? "主动买" : trade.taker_side === "Sell" ? "主动卖" : "-"}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function OrderBook({
  bids,
  asks,
  lastPrice,
  priceTone,
}: {
  bids: BookLevel[];
  asks: BookLevel[];
  lastPrice?: number;
  priceTone?: "up" | "down";
}) {
  const bidRows = withCumulative(bids.slice(0, BOOK_DEPTH));
  // Asks are accumulated outward from the spread, then flipped so the best ask sits next to it.
  const askRows = withCumulative(asks.slice(0, BOOK_DEPTH)).reverse();
  const bidTotal = bidRows.at(-1)?.total ?? 0;
  const askTotal = askRows[0]?.total ?? 0;
  const maxTotal = Math.max(bidTotal, askTotal, 1);
  const bidShare = bidTotal + askTotal > 0 ? (bidTotal / (bidTotal + askTotal)) * 100 : undefined;
  return (
    <div className="book-table">
      <div className="book-head">
        <span>价格</span>
        <span>数量</span>
        <span>累计</span>
      </div>
      <div className="book-asks">
        {askRows.length === 0 ? (
          <div className="book-empty">暂无卖单</div>
        ) : (
          askRows.map((row) => (
            <BookRow row={row} side="ask" maxTotal={maxTotal} key={`ask-${row.price_tick}`} />
          ))
        )}
      </div>
      <div className="last-price">
        <strong className={priceTone ?? ""}>{formatNumber(lastPrice)}</strong>
        <span>{lastPrice === undefined ? "暂无成交" : "最新成交价"}</span>
      </div>
      <div className="book-bids">
        {bidRows.length === 0 ? (
          <div className="book-empty">暂无买单</div>
        ) : (
          bidRows.map((row) => (
            <BookRow row={row} side="bid" maxTotal={maxTotal} key={`bid-${row.price_tick}`} />
          ))
        )}
      </div>
      <div className="depth-ratio">
        {bidShare === undefined ? (
          <span className="muted">买卖量占比：-</span>
        ) : (
          <>
            <span>买 {bidShare.toFixed(1)}%</span>
            <span>卖 {(100 - bidShare).toFixed(1)}%</span>
          </>
        )}
      </div>
    </div>
  );
}

function BookRow({ row, side, maxTotal }: { row: BookLevel & { total: number }; side: "bid" | "ask"; maxTotal: number }) {
  return (
    <div className={`book-row ${side}`}>
      <span className="depth-bg" style={{ width: `${(row.total / maxTotal) * 100}%` }} />
      <strong>{formatNumber(row.price_tick)}</strong>
      <span>{row.qty}</span>
      <span>{row.total}</span>
    </div>
  );
}

function withCumulative(levels: BookLevel[]) {
  let total = 0;
  return levels.map((level) => ({ ...level, total: (total += level.qty) }));
}

function TicketInput({
  label,
  value,
  suffix,
  disabled,
  onChange,
}: {
  label: string;
  value: number;
  suffix: string;
  disabled?: boolean;
  onChange: (value: number) => void;
}) {
  return (
    <label className="ticket-field">
      <span>{label}</span>
      <div>
        <input
          type="number"
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(Number(event.target.value))}
        />
        <strong>{suffix}</strong>
      </div>
    </label>
  );
}

function ActivityTable({
  executions,
  logs,
}: {
  executions: RoomExecutionSummary[];
  logs: LogEntry[];
}) {
  const rows = executions
    .flatMap<TimelineRow>((execution) =>
      execution.events.length === 0
        ? [{ execution }]
        : execution.events.map((event) => ({ execution, event })),
    )
    .reverse()
    .slice(0, 40);

  return (
    <div className="data-table activity">
      <div className="data-head">
        <span>来源</span>
        <span>事件</span>
        <span>说明</span>
      </div>
      {rows.length === 0 && logs.length === 0 ? (
        <div className="data-empty">暂无房间事件</div>
      ) : (
        <>
          {logs.slice(0, 3).map((log) => (
            <div className="data-row" key={log.id}>
              <span>本地</span>
              <strong className={log.level}>{LOG_LABELS[log.level]}</strong>
              <span>{log.text}</span>
            </div>
          ))}
          {rows.map(({ execution, event }, index) => (
            <div
              className="data-row"
              key={`${execution.command_seq}-${event?.seq ?? "reject"}-${index}`}
            >
              <span>#{execution.command_seq}</span>
              <strong className={eventTone(event, execution)} title={event?.type ?? "Rejected"}>
                {eventLabel(event)}
              </strong>
              <span>{eventDescription(event, execution)}</span>
            </div>
          ))}
        </>
      )}
    </div>
  );
}

function eventTone(event: ApiEvent | undefined, execution: RoomExecutionSummary) {
  if (!execution.accepted || event?.type.includes("Rejected")) {
    return "warn";
  }
  if (event?.type === "TradePrinted" || event?.type === "OrderFilled") {
    return "ok";
  }
  return "info";
}

const EVENT_LABELS: Record<string, string> = {
  TradePrinted: "成交",
  OrderRested: "挂单",
  OrderAccepted: "已接受",
  OrderFilled: "完全成交",
  OrderPartiallyFilled: "部分成交",
  OrderCanceled: "已撤销",
  RiskRejected: "风控拒绝",
  OrderRejected: "订单拒绝",
  CancelRejected: "撤单拒绝",
  OrderExpired: "已过期",
};

function eventLabel(event: ApiEvent | undefined) {
  if (!event) {
    return "拒绝";
  }
  return EVENT_LABELS[event.type] ?? event.type;
}

function priceDirection(current: number, previous: number): "up" | "down" | undefined {
  if (current > previous) {
    return "up";
  }
  return current < previous ? "down" : undefined;
}

function eventDescription(event: ApiEvent | undefined, execution: RoomExecutionSummary) {
  if (!event) {
    return execution.reject_reason ?? "rejected";
  }
  switch (event.type) {
    case "TradePrinted":
      return `成交 ${event.qty} @ ${event.price_tick}, trade #${event.trade_id}`;
    case "OrderRested":
      return `挂单 #${event.order_id}, ${event.remaining_qty} @ ${event.price_tick}`;
    case "OrderAccepted":
      return `订单 #${event.order_id} 已接受`;
    case "OrderFilled":
      return `订单 #${event.order_id} 已完全成交`;
    case "OrderPartiallyFilled":
      return `订单 #${event.order_id} 部分成交，剩余 ${event.remaining_qty}`;
    case "OrderCanceled":
      return `订单 #${event.order_id} 已撤销，剩余 ${event.remaining_qty}`;
    case "RiskRejected":
    case "OrderRejected":
    case "CancelRejected":
      return `订单 #${event.order_id} 被拒绝：${event.reason}`;
    case "OrderExpired":
      return `订单 #${event.order_id} 过期，未成交 ${event.unfilled_qty}`;
    default:
      return event.type;
  }
}

function AccountTable({ accounts }: { accounts: AnyAccount[] }) {
  return (
    <div className="data-table accounts-table">
      <div className="data-head">
        <span>账户</span>
        <span>现金</span>
        <span>持仓</span>
        <span>权益</span>
      </div>
      {accounts.length === 0 ? (
        <div className="data-empty">暂无资产</div>
      ) : (
        accounts.map((account) => (
          <div className="data-row" key={account.account_id}>
            <strong>{account.account_id}</strong>
            <span>{account.cash_balance}</span>
            <span>{isPerpAccount(account) && account.hedge_positions ? `多 ${account.hedge_positions.long.qty} / 空 ${account.hedge_positions.short.qty}` : account.position_qty}</span>
            <span>{accountEquity(account)}</span>
          </div>
        ))
      )}
    </div>
  );
}

function flattenAccounts(accounts?: AccountSnapshots): AnyAccount[] {
  if (!accounts) {
    return [];
  }
  if ("Spot" in accounts) {
    return accounts.Spot;
  }
  return accounts.Perp;
}

function accountEquity(account: AnyAccount) {
  return isPerpAccount(account) ? account.equity : "-";
}

function isPerpAccount(account: AnyAccount): account is PerpAccount {
  return Object.prototype.hasOwnProperty.call(account, "equity");
}

function sampleRoomPayload(roomId: string, linked: boolean, enhanced: boolean, microstructure: boolean) {
  const recipe = structuredClone(linked ? (enhanced ? (microstructure ? microstructureMarket : behaviorMarket) : linkedMarket) : backgroundMarket);
  recipe.scenario.room_id = roomId;
  for (const bot of recipe.agents) bot.Plugin.participant.room_id = roomId;
  return recipe;
}

function summarizeOrder(response: OrderResponse) {
  if (!response.accepted) {
    return response.reject_reason ?? "order rejected";
  }
  const trade = response.events.find((event) => event.type === "TradePrinted");
  if (trade) {
    return `${response.participant_id} 成交 ${trade.qty} @ ${trade.price_tick}`;
  }
  const rested = response.events.find((event) => event.type === "OrderRested");
  if (rested) {
    return `${response.participant_id} 挂单 ${rested.remaining_qty} @ ${rested.price_tick}`;
  }
  return `${response.participant_id} 订单已接受`;
}

function formatNumber(value?: number) {
  if (value === undefined || Number.isNaN(value)) {
    return "-";
  }
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits: 2,
  }).format(value);
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
