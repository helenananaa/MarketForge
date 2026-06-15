import { StrictMode, useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Bot,
  ChevronDown,
  CircleHelp,
  Globe2,
  Play,
  Plus,
  RefreshCw,
  Search,
  Send,
  Settings,
  Square,
} from "lucide-react";
import "./styles.css";

type Side = "Buy" | "Sell";
type AgentStatus = {
  room_id: string;
  running: boolean;
  interval_ms: number;
  participants: string[];
};
type BookLevel = {
  price_tick: number;
  qty: number;
};
type SpotAccount = {
  account_id: number;
  cash_balance: number;
  position_qty: number;
  fees_paid: number;
};
type PerpAccount = SpotAccount & {
  equity: number;
  realized_pnl: number;
  unrealized_pnl: number;
  initial_margin: number;
};
type AnyAccount = SpotAccount | PerpAccount;
type AccountSnapshots = { Spot: SpotAccount[] } | { Perp: PerpAccount[] };
type MarketView = {
  room_id: string;
  status: "Running" | "Paused" | "Closed";
  book: {
    bids: BookLevel[];
    asks: BookLevel[];
  };
  accounts: AccountSnapshots;
};
type ApiEvent = {
  type: string;
  seq: number;
  order_id?: number;
  trade_id?: number;
  price_tick?: number;
  qty?: number;
  remaining_qty?: number;
  reason?: string;
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
type LogEntry = {
  id: string;
  level: "ok" | "warn" | "info";
  text: string;
};

const API_DEFAULT = "http://127.0.0.1:3000";
const ROOM_DEFAULT = "demo-web";

function App() {
  const [apiBase, setApiBase] = useState(API_DEFAULT);
  const [roomId, setRoomId] = useState(ROOM_DEFAULT);
  const [activeRoom, setActiveRoom] = useState("");
  const [view, setView] = useState<MarketView | null>(null);
  const [agentStatus, setAgentStatus] = useState<AgentStatus | null>(null);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [side, setSide] = useState<Side>("Buy");
  const [orderKind, setOrderKind] = useState<"Limit" | "Market">("Limit");
  const [price, setPrice] = useState(100);
  const [qty, setQty] = useState(2);
  const [accountId, setAccountId] = useState(20);

  const accounts = useMemo(() => flattenAccounts(view?.accounts), [view]);
  const selectedAccount = accounts.find((account) => account.account_id === accountId);
  const bestBid = view?.book.bids[0]?.price_tick;
  const bestAsk = view?.book.asks[0]?.price_tick;
  const lastPrice = bestAsk ?? bestBid ?? price;
  const spread = bestAsk !== undefined && bestBid !== undefined ? bestAsk - bestBid : 0;

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

  const refresh = useCallback(
    async (room = activeRoom) => {
      if (!room) {
        return;
      }
      const [nextView, nextAgents] = await Promise.all([
        api<MarketView>(`/rooms/${room}/view`),
        api<AgentStatus>(`/rooms/${room}/agents`),
      ]);
      setView(nextView);
      setAgentStatus(nextAgents);
    },
    [activeRoom, api],
  );

  useEffect(() => {
    if (!activeRoom || !autoRefresh) {
      return;
    }
    const handle = window.setInterval(() => {
      refresh().catch((error: Error) =>
        pushLog({ level: "warn", text: error.message }),
      );
    }, 900);
    return () => window.clearInterval(handle);
  }, [activeRoom, autoRefresh, pushLog, refresh]);

  const createRoom = async () => {
    const nextRoom = roomId.trim() || ROOM_DEFAULT;
    setBusy(true);
    try {
      await api("/rooms", {
        method: "POST",
        body: JSON.stringify(sampleRoomPayload(nextRoom)),
      });
      setActiveRoom(nextRoom);
      pushLog({ level: "ok", text: `房间 ${nextRoom} 已创建` });
      await refresh(nextRoom);
    } catch (error) {
      const message = error instanceof Error ? error.message : "create room failed";
      if (message.includes("RoomAlreadyExists")) {
        setActiveRoom(nextRoom);
        pushLog({ level: "info", text: `房间 ${nextRoom} 已载入` });
        await refresh(nextRoom);
      } else {
        pushLog({ level: "warn", text: message });
      }
    } finally {
      setBusy(false);
    }
  };

  const submitOrder = async () => {
    if (!activeRoom) {
      pushLog({ level: "warn", text: "请先创建或载入房间" });
      return;
    }
    setBusy(true);
    try {
      const action =
        orderKind === "Market"
          ? { PlaceMarket: { side, qty } }
          : { PlaceLimit: { side, price_tick: price, qty } };
      const response = await api<OrderResponse>(`/rooms/${activeRoom}/orders`, {
        method: "POST",
        body: JSON.stringify({
          participant_id: "human-web",
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
          agents: [dcaAgent(activeRoom)],
          interval_ms: 700,
        }),
      });
      setAgentStatus(status);
      pushLog({ level: "ok", text: "AI 交易员已启动" });
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
      pushLog({ level: "info", text: "AI 交易员已停止" });
    } catch (error) {
      pushLog({
        level: "warn",
        text: error instanceof Error ? error.message : "stop AI failed",
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="terminal">
      <header className="global-nav">
        <div className="brand">
          <span className="brand-mark">MF</span>
          <strong>MarketForge</strong>
        </div>
        <nav aria-label="main navigation">
          <button>市场</button>
          <button>交易</button>
          <button>策略</button>
          <button>房间</button>
        </nav>
        <div className="search">
          <Search size={16} aria-hidden="true" />
          <input
            value={apiBase}
            onChange={(event) => setApiBase(event.target.value)}
            aria-label="API base URL"
          />
        </div>
        <div className="nav-actions">
          <button aria-label="settings">
            <Settings size={18} aria-hidden="true" />
          </button>
          <button aria-label="help">
            <CircleHelp size={18} aria-hidden="true" />
          </button>
          <button aria-label="language">
            <Globe2 size={18} aria-hidden="true" />
          </button>
        </div>
      </header>

      <section className="market-strip">
        <div className="symbol-block">
          <span className="coin">V</span>
          <div>
            <div className="symbol-line">
              <strong>V-BTC/SPOT</strong>
              <ChevronDown size={16} aria-hidden="true" />
            </div>
            <span>{activeRoom || "未载入房间"}</span>
          </div>
        </div>
        <MarketStat label="最新价" value={formatNumber(lastPrice)} tone="up" />
        <MarketStat label="价差" value={formatNumber(spread)} />
        <MarketStat label="最优买价" value={formatNumber(bestBid)} tone="up" />
        <MarketStat label="最优卖价" value={formatNumber(bestAsk)} tone="down" />
        <MarketStat label="状态" value={view?.status ?? "-"} />
        <div className="room-controls">
          <input
            value={roomId}
            onChange={(event) => setRoomId(event.target.value)}
            aria-label="room id"
          />
          <button onClick={createRoom} disabled={busy}>
            <Plus size={16} aria-hidden="true" />
            创建
          </button>
          <button onClick={() => refresh()} disabled={!activeRoom || busy}>
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

      <div className="trade-grid">
        <section className="chart-pane">
          <div className="pane-tabs">
            <button className="tab active">图表</button>
            <button className="tab">信息</button>
            <button className="tab">交易数据</button>
            <button className="tab">动态</button>
          </div>
          <div className="timeframe-row">
            {["1秒", "1分", "5分", "15分", "1小时", "4小时", "1日"].map((item) => (
              <button className={item === "1分" ? "active" : ""} key={item}>
                {item}
              </button>
            ))}
          </div>
          <ChartPlaceholder price={lastPrice} />
        </section>

        <section className="orderbook-pane">
          <div className="pane-header">
            <div>
              <button className="tab active">订单表</button>
              <button className="tab">最新成交</button>
            </div>
            <button aria-label="book settings">
              <Settings size={16} aria-hidden="true" />
            </button>
          </div>
          <OrderBook bids={view?.book.bids ?? []} asks={view?.book.asks ?? []} lastPrice={lastPrice} />
        </section>

        <aside className="ticket-pane">
          <div className="pane-tabs compact">
            <button className="tab active">交易</button>
            <button className="tab">工具</button>
          </div>
          <div className="buy-sell-tabs">
            <button className={side === "Buy" ? "active" : ""} onClick={() => setSide("Buy")}>
              买入
            </button>
            <button className={side === "Sell" ? "active sell" : ""} onClick={() => setSide("Sell")}>
              卖出
            </button>
          </div>
          <div className="order-type-row">
            <button className={orderKind === "Limit" ? "active" : ""} onClick={() => setOrderKind("Limit")}>
              限价委托
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
          <TicketInput label="数量" value={qty} onChange={setQty} suffix="BTC" />
          <div className="percent-row">
            {[0, 25, 50, 75, 100].map((item) => (
              <button key={item}>{item}%</button>
            ))}
          </div>
          <div className="balance-lines">
            <span>可用现金</span>
            <strong>{selectedAccount?.cash_balance ?? "-"} EUR</strong>
            <span>持仓数量</span>
            <strong>{selectedAccount?.position_qty ?? "-"} BTC</strong>
          </div>
          <button className={side === "Buy" ? "submit buy" : "submit sell"} onClick={submitOrder} disabled={busy || !activeRoom}>
            <Send size={17} aria-hidden="true" />
            {side === "Buy" ? "买入 BTC" : "卖出 BTC"}
          </button>
          <div className="ai-box">
            <div className="ai-state">
              <Bot size={16} aria-hidden="true" />
              <strong>{agentStatus?.running ? "AI 运行中" : "AI 已停止"}</strong>
              <span>{agentStatus?.interval_ms ? `${agentStatus.interval_ms}ms` : "-"}</span>
            </div>
            <div className="ai-buttons">
              <button onClick={startAi} disabled={busy || !activeRoom}>
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
          <div className="pane-tabs">
            <button className="tab active">当前委托</button>
            <button className="tab">历史委托</button>
            <button className="tab">当前仓位</button>
            <button className="tab">资产</button>
            <button className="tab">策略</button>
          </div>
          <div className="bottom-content">
            <ActivityTable logs={logs} />
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
}: {
  label: string;
  value: string | number;
  tone?: "up" | "down";
}) {
  return (
    <div className="market-stat">
      <span>{label}</span>
      <strong className={tone ?? ""}>{value}</strong>
    </div>
  );
}

function ChartPlaceholder({ price }: { price: number }) {
  const candles = [42, 48, 45, 51, 49, 54, 52, 58, 61, 57, 64, 68, 65, 71, 75, 73, 78, 76, 82, 80];
  return (
    <div className="chart-surface">
      <div className="chart-title">
        <span>V-BTC/SPOT - 1 - MarketForge</span>
        <strong>{formatNumber(price)}</strong>
      </div>
      <div className="chart-grid">
        {candles.map((height, index) => (
          <span
            className={index % 4 === 1 ? "candle down" : "candle"}
            key={`${height}-${index}`}
            style={{ height: `${height}%` }}
          />
        ))}
      </div>
      <div className="volume-row">
        {candles.map((height, index) => (
          <span
            className={index % 4 === 1 ? "volume down" : "volume"}
            key={`v-${height}-${index}`}
            style={{ height: `${Math.max(10, 100 - height)}%` }}
          />
        ))}
      </div>
      <div className="chart-footer">
        <span>1日</span>
        <span>5日</span>
        <span>1月</span>
        <span>3月</span>
        <span>自动</span>
      </div>
    </div>
  );
}

function OrderBook({
  bids,
  asks,
  lastPrice,
}: {
  bids: BookLevel[];
  asks: BookLevel[];
  lastPrice: number;
}) {
  const askRows = asks.slice(0, 8).reverse();
  const bidRows = bids.slice(0, 8);
  return (
    <div className="book-table">
      <div className="book-head">
        <span>价格</span>
        <span>数量</span>
        <span>合计</span>
      </div>
      <div className="book-asks">
        {askRows.length === 0 ? (
          <EmptyRows side="ask" />
        ) : (
          askRows.map((level, index) => (
            <BookRow level={level} side="ask" key={`ask-${level.price_tick}`} rank={index + 1} />
          ))
        )}
      </div>
      <div className="last-price">
        <strong>{formatNumber(lastPrice)}</strong>
        <span>最新价</span>
      </div>
      <div className="book-bids">
        {bidRows.length === 0 ? (
          <EmptyRows side="bid" />
        ) : (
          bidRows.map((level, index) => (
            <BookRow level={level} side="bid" key={`bid-${level.price_tick}`} rank={index + 1} />
          ))
        )}
      </div>
      <div className="depth-ratio">
        <span>B 50.25%</span>
        <span>S 49.75%</span>
      </div>
    </div>
  );
}

function BookRow({ level, side, rank }: { level: BookLevel; side: "bid" | "ask"; rank: number }) {
  const width = Math.min(100, Math.max(12, level.qty * 12 + rank * 5));
  return (
    <div className={`book-row ${side}`}>
      <span className="depth-bg" style={{ width: `${width}%` }} />
      <strong>{formatNumber(level.price_tick)}</strong>
      <span>{level.qty}</span>
      <span>{level.qty}</span>
    </div>
  );
}

function EmptyRows({ side }: { side: "bid" | "ask" }) {
  return (
    <>
      {Array.from({ length: 4 }).map((_, index) => (
        <div className={`book-row ${side} empty-row`} key={index}>
          <strong>-</strong>
          <span>-</span>
          <span>-</span>
        </div>
      ))}
    </>
  );
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

function ActivityTable({ logs }: { logs: LogEntry[] }) {
  return (
    <div className="data-table activity">
      <div className="data-head">
        <span>时间</span>
        <span>类型</span>
        <span>说明</span>
      </div>
      {logs.length === 0 ? (
        <div className="data-empty">暂无委托</div>
      ) : (
        logs.slice(0, 7).map((log) => (
          <div className="data-row" key={log.id}>
            <span>{new Date(Number(log.id.split("-")[0])).toLocaleTimeString()}</span>
            <strong className={log.level}>{log.level}</strong>
            <span>{log.text}</span>
          </div>
        ))
      )}
    </div>
  );
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
            <span>{account.position_qty}</span>
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

function sampleRoomPayload(roomId: string) {
  return {
    scenario: {
      room_id: roomId,
      market: {
        Spot: {
          instrument: { symbol: "V-BTC-SPOT", tick_size: 1, lot_size: 1 },
          clearing: { maker_fee_ppm: 0, taker_fee_ppm: 0 },
          risk: {
            price_tick_size: null,
            lot_size: null,
            max_order_qty: null,
            max_order_notional: null,
            allow_short: false,
          },
        },
      },
      accounts: [
        { Spot: { account_id: 10, cash_balance: 10_000, position_qty: 120 } },
        { Basic: { account_id: 20, cash_balance: 10_000 } },
        { Basic: { account_id: 30, cash_balance: 10_000 } },
      ],
      seed_orders: [
        {
          NewOrder: {
            order_id: 10_000,
            account_id: 10,
            side: "Sell",
            kind: { Limit: { price_tick: 104 } },
            qty: 8,
          },
        },
      ],
    },
    agents: [dcaAgent(roomId)],
    agent_interval_ms: 900,
    autostart_agents: false,
  };
}

function dcaAgent(roomId: string) {
  return {
    DcaTrader: {
      participant: {
        participant_id: "dca-worker",
        kind: "RuleAgent",
        room_id: roomId,
        account_id: 30,
      },
      interval_steps: 1,
      order_qty: 1,
      use_market_order: false,
      limit_offset_ticks: 0,
      fallback_price_tick: 100,
      side: "Buy",
    },
  };
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
  return `${response.participant_id} accepted`;
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
