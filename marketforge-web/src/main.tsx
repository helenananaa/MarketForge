import { StrictMode, useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  Activity,
  Bot,
  Pause,
  Play,
  Plus,
  RefreshCw,
  Send,
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
type AccountSnapshots =
  | { Spot: SpotAccount[] }
  | { Perp: PerpAccount[] };
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
  const bestBid = view?.book.bids[0]?.price_tick ?? "-";
  const bestAsk = view?.book.asks[0]?.price_tick ?? "-";

  const pushLog = useCallback((entry: Omit<LogEntry, "id">) => {
    setLogs((current) => [
      { ...entry, id: `${Date.now()}-${Math.random()}` },
      ...current.slice(0, 23),
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
        const body = await response.json().catch(() => ({ error: response.statusText }));
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
      refresh().catch((error: Error) => pushLog({ level: "warn", text: error.message }));
    }, 900);
    return () => window.clearInterval(handle);
  }, [activeRoom, autoRefresh, pushLog, refresh]);

  const createRoom = async () => {
    const nextRoom = roomId.trim() || ROOM_DEFAULT;
    setBusy(true);
    try {
      const payload = sampleRoomPayload(nextRoom);
      await api("/rooms", {
        method: "POST",
        body: JSON.stringify(payload),
      });
      setActiveRoom(nextRoom);
      pushLog({ level: "ok", text: `room ${nextRoom} ready` });
      await refresh(nextRoom);
    } catch (error) {
      const message = error instanceof Error ? error.message : "create room failed";
      if (message.includes("RoomAlreadyExists")) {
        setActiveRoom(nextRoom);
        pushLog({ level: "info", text: `room ${nextRoom} loaded` });
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
      pushLog({ level: "warn", text: "create or load a room first" });
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
      pushLog({ level: "warn", text: error instanceof Error ? error.message : "order failed" });
    } finally {
      setBusy(false);
    }
  };

  const startAi = async () => {
    if (!activeRoom) {
      pushLog({ level: "warn", text: "create or load a room first" });
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
      pushLog({ level: "ok", text: "dca-worker running" });
      await refresh();
    } catch (error) {
      pushLog({ level: "warn", text: error instanceof Error ? error.message : "start AI failed" });
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
      pushLog({ level: "info", text: "AI stopped" });
    } catch (error) {
      pushLog({ level: "warn", text: error instanceof Error ? error.message : "stop AI failed" });
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="shell">
      <header className="topbar">
        <div>
          <h1>MarketForge</h1>
          <p>{activeRoom ? `room ${activeRoom}` : "local exchange control"}</p>
        </div>
        <div className="connection">
          <Activity size={18} aria-hidden="true" />
          <input
            value={apiBase}
            onChange={(event) => setApiBase(event.target.value)}
            aria-label="API base URL"
          />
        </div>
      </header>

      <section className="toolbar">
        <label>
          Room
          <input value={roomId} onChange={(event) => setRoomId(event.target.value)} />
        </label>
        <button onClick={createRoom} disabled={busy}>
          <Plus size={17} aria-hidden="true" />
          Create
        </button>
        <button onClick={() => refresh()} disabled={!activeRoom || busy}>
          <RefreshCw size={17} aria-hidden="true" />
          Refresh
        </button>
        <label className="toggle">
          <input
            type="checkbox"
            checked={autoRefresh}
            onChange={(event) => setAutoRefresh(event.target.checked)}
          />
          Auto
        </label>
      </section>

      <section className="metrics" aria-label="market summary">
        <Metric label="Status" value={view?.status ?? "-"} />
        <Metric label="Best bid" value={bestBid} />
        <Metric label="Best ask" value={bestAsk} />
        <Metric label="AI" value={agentStatus?.running ? "Running" : "Stopped"} />
      </section>

      <div className="workspace">
        <section className="panel book-panel">
          <div className="panel-head">
            <h2>Order Book</h2>
            <span>{view?.room_id ?? "-"}</span>
          </div>
          <div className="book-grid">
            <BookSide title="Bids" levels={view?.book.bids ?? []} side="bid" />
            <BookSide title="Asks" levels={view?.book.asks ?? []} side="ask" />
          </div>
        </section>

        <section className="panel ticket">
          <div className="panel-head">
            <h2>Order Ticket</h2>
            <Send size={17} aria-hidden="true" />
          </div>
          <div className="segmented">
            <button className={side === "Buy" ? "active buy" : ""} onClick={() => setSide("Buy")}>
              Buy
            </button>
            <button className={side === "Sell" ? "active sell" : ""} onClick={() => setSide("Sell")}>
              Sell
            </button>
          </div>
          <div className="segmented">
            <button className={orderKind === "Limit" ? "active" : ""} onClick={() => setOrderKind("Limit")}>
              Limit
            </button>
            <button className={orderKind === "Market" ? "active" : ""} onClick={() => setOrderKind("Market")}>
              Market
            </button>
          </div>
          <label>
            Account
            <input
              type="number"
              value={accountId}
              onChange={(event) => setAccountId(Number(event.target.value))}
            />
          </label>
          <label>
            Price
            <input
              type="number"
              value={price}
              disabled={orderKind === "Market"}
              onChange={(event) => setPrice(Number(event.target.value))}
            />
          </label>
          <label>
            Qty
            <input type="number" min={1} value={qty} onChange={(event) => setQty(Number(event.target.value))} />
          </label>
          <button className="primary" onClick={submitOrder} disabled={busy || !activeRoom}>
            <Send size={17} aria-hidden="true" />
            Send Order
          </button>
        </section>

        <section className="panel ai-panel">
          <div className="panel-head">
            <h2>AI Worker</h2>
            <Bot size={18} aria-hidden="true" />
          </div>
          <div className="agent-state">
            <span className={agentStatus?.running ? "dot running" : "dot"} />
            <strong>{agentStatus?.running ? "Running" : "Stopped"}</strong>
            <span>{agentStatus?.interval_ms ? `${agentStatus.interval_ms}ms` : "-"}</span>
          </div>
          <div className="agent-list">
            {(agentStatus?.participants.length ? agentStatus.participants : ["dca-worker"]).map((agent) => (
              <span key={agent}>{agent}</span>
            ))}
          </div>
          <div className="button-row">
            <button onClick={startAi} disabled={busy || !activeRoom}>
              <Play size={17} aria-hidden="true" />
              Start
            </button>
            <button onClick={stopAi} disabled={busy || !activeRoom}>
              <Square size={17} aria-hidden="true" />
              Stop
            </button>
          </div>
        </section>

        <section className="panel accounts">
          <div className="panel-head">
            <h2>Accounts</h2>
            <span>{accounts.length}</span>
          </div>
          <div className="account-table">
            <div className="table-row table-head">
              <span>ID</span>
              <span>Cash</span>
              <span>Pos</span>
              <span>Equity</span>
            </div>
            {accounts.map((account) => (
              <div className="table-row" key={account.account_id}>
                <span>{account.account_id}</span>
                <span>{account.cash_balance}</span>
                <span>{account.position_qty}</span>
                <span>{accountEquity(account)}</span>
              </div>
            ))}
          </div>
        </section>

        <section className="panel tape">
          <div className="panel-head">
            <h2>Tape</h2>
            <Pause size={17} aria-hidden="true" />
          </div>
          <div className="log-list">
            {logs.length === 0 ? (
              <div className="empty">No activity</div>
            ) : (
              logs.map((log) => (
                <div className={`log ${log.level}`} key={log.id}>
                  {log.text}
                </div>
              ))
            )}
          </div>
        </section>
      </div>
    </main>
  );
}

function Metric({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function BookSide({ title, levels, side }: { title: string; levels: BookLevel[]; side: "bid" | "ask" }) {
  const maxQty = Math.max(1, ...levels.map((level) => level.qty));
  return (
    <div className={`book-side ${side}`}>
      <div className="book-title">
        <span>{title}</span>
        <span>Qty @ Price</span>
      </div>
      <div className="levels">
        {levels.length === 0 ? (
          <div className="empty">Empty</div>
        ) : (
          levels.slice(0, 9).map((level) => (
            <div className="level" key={`${side}-${level.price_tick}`}>
              <span className="bar" style={{ width: `${Math.max(8, (level.qty / maxQty) * 100)}%` }} />
              <span>{level.qty}</span>
              <strong>{level.price_tick}</strong>
            </div>
          ))
        )}
      </div>
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
    return `${response.participant_id} traded ${trade.qty} @ ${trade.price_tick}`;
  }
  const rested = response.events.find((event) => event.type === "OrderRested");
  if (rested) {
    return `${response.participant_id} rested ${rested.remaining_qty} @ ${rested.price_tick}`;
  }
  return `${response.participant_id} accepted`;
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
