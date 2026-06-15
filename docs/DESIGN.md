# MarketForge Design Document

## 1. Project Positioning

MarketForge is not a normal trading dashboard, simulated broker, or charting app.

The long-term direction is:

> Build an order-book-driven virtual exchange and trading training sandbox where human traders and AI agents participate under the same market rules.

The core product value comes from market microstructure simulation:

- Orders create prices through a real matching process.
- Human traders and AI agents submit orders through the same gateway.
- The market can be replayed, audited, analyzed, and explained.
- Training focuses on order flow, liquidity, execution, risk, and market behavior instead of only candlestick patterns.

This means the project should be designed more like a small exchange engine plus a multiplayer simulation game than a traditional Web App.

## 2. Core Design Principle

The most important rule:

> The matching engine is the only source of truth. AI, users, scenarios, and administrators can submit commands, but nobody directly edits price.

Prices must emerge from the order book.

Human traders, AI traders, and system scenarios should all interact with the market through explicit commands:

- Place order
- Cancel order
- Amend order
- Inject scenario event
- Pause or resume market

The engine then emits authoritative events:

- Order accepted
- Order rejected
- Order filled
- Order partially filled
- Order canceled
- Trade printed
- Book updated
- Position updated
- Margin updated
- Liquidation triggered

This gives the project four important properties:

- Fairness: AI and humans use the same market rules.
- Reproducibility: the same seed and command stream can replay the same market.
- Debuggability: every state change has an event trail.
- Training value: the coach can explain what actually happened, not invent a story after the fact.

## 3. Product Vision

The end product should feel like a virtual exchange training arena.

Users can enter a market room, trade a virtual instrument, compete or train alongside other humans and AI agents, and then replay the session to understand:

- Why they paid slippage.
- Where liquidity disappeared.
- How their order affected the book.
- Whether they exposed execution intent.
- How market makers reacted.
- Whether stop-loss clusters or liquidation zones shaped the move.
- How their PnL came from execution quality, risk, and market structure.

The product should not primarily teach "buy when this indicator crosses that indicator." It should teach how markets behave when orders, liquidity, and participants interact.

## 4. High-Level Architecture

The long-term system can be understood as these layers:

```text
Web Client / Desktop Client
        |
API Gateway / WebSocket Gateway
        |
Order Gateway
        |
Risk Engine
        |
Matching Engine
        |
Clearing / Accounts / Positions
        |
Market Data Broadcast
        |
Replay / Analytics / AI Coach

AI Agent Runtime
        |
Same Order Gateway used by humans
```

The system should be built around market instances.

Each market room is an actor-like state machine:

```text
MarketActor(room_id)
    state:
        order_book
        accounts
        positions
        trigger_orders
        liquidation_queue
        scenario_state

    input:
        command stream

    output:
        event stream
```

Only the market actor mutates its own order book and market state. Other services submit commands or consume events.

## 5. Main Domain Concepts

MarketForge should model a virtual exchange with these core concepts:

- `World` / `Room`: an isolated market simulation instance.
- `Instrument`: a tradable virtual product, such as `V-BTC`.
- `Account`: a human or AI trader account.
- `Cash`: available virtual balance.
- `Position`: current inventory per instrument.
- `Order`: an instruction to buy or sell.
- `Trade`: a matched execution between two orders.
- `OrderBook`: price-time-priority queue of resting orders.
- `RiskRule`: constraints before an order reaches matching.
- `Event`: immutable record of a state transition.
- `Replay`: reconstruction of market state from events and snapshots.
- `Agent`: AI trader that observes market data and submits orders.
- `Coach`: AI or rule-based explanation layer over event history.

## 6. Matching Engine Requirements

The matching engine is the heart of the system.

It should be deterministic, auditable, and conservative.

Core rules:

- Use integer price ticks instead of floats.
- Use integer quantity units instead of floats where possible.
- Bids are sorted from highest price to lowest price.
- Asks are sorted from lowest price to highest price.
- Orders at the same price level are matched FIFO.
- A buy limit order crosses when its price is greater than or equal to best ask.
- A sell limit order crosses when its price is less than or equal to best bid.
- A market buy consumes asks from the lowest ask upward.
- A market sell consumes bids from the highest bid downward.
- Unfilled limit quantity rests on the book if allowed.

Initial order types:

- Limit order
- Market order
- Cancel order
- Post-only limit order
- Stop market order

Later order types:

- Stop limit
- IOC
- FOK
- Iceberg
- Reduce-only
- Take-profit
- Trailing stop

The first version should avoid overbuilding order types. A small set of correct order behavior is more valuable than a large set of vague behavior.

## 7. Event Sourcing And Replay

MarketForge should not only store final state.

It should store the command and event stream:

```json
{
  "room_id": "room_001",
  "seq": 184283,
  "type": "OrderFilled",
  "order_id": "O123",
  "account_id": "A56",
  "price_tick": 100235,
  "qty": 10,
  "exchange_ts": 18293849388
}
```

Event sourcing is important because it enables:

- Full session replay.
- Bug reproduction.
- AI coaching based on actual behavior.
- Leaderboards and training metrics.
- Offline analysis of many simulations.
- Deterministic regression tests.

The design target should be:

> Initial state + random seed + command stream = same final state and same event stream.

Snapshots can be introduced later to speed up replay, but the event stream remains the durable truth.

## 8. Market Data Design

The matching engine should produce market data as a derived output.

Market data levels:

- L1: best bid, best ask, last price, spread.
- L2: aggregated depth by price level.
- L3: individual order queue details.
- Tape: trade-by-trade execution stream.
- K-line: bars aggregated from trades.
- Heatmap: depth history over time.

The client should sync through:

```text
snapshot + incremental updates
```

Each incremental update must carry a sequence number. If the client detects a sequence gap, it should request a fresh snapshot.

K-lines are not the source of truth. They are a derived visualization from trades.

## 9. AI Agent Design

AI agents are market participants, not market controllers.

They should observe market data, decide on actions, and submit orders through the same order gateway as humans.

First-layer agents should be rule-based:

- Noise Trader: creates random flow.
- Market Maker: posts two-sided quotes and manages inventory.
- Momentum Trader: chases breakouts.
- Mean Reversion Trader: fades extreme moves.
- Stop-loss Trader: reacts to stop conditions.
- Large Execution Trader: splits a large target order.
- Liquidation Trader: simulates forced buying or selling.

Later-layer agents can include:

- Reinforcement learning market maker.
- Execution algorithm agent.
- Order-flow prediction strategy.
- Imitation-learning retail trader.
- LLM high-level strategist.

LLMs should not run inside the matching loop.

Good LLM use cases:

- Explain what happened after a session.
- Generate training scenarios.
- Adjust high-level agent parameters.
- Provide coaching every few seconds.
- Produce readable post-game reports.

Bad LLM use cases:

- Generate every tick.
- Directly set prices.
- Bypass risk checks.
- See hidden information unavailable to humans.

## 10. Human Multiplayer Design

Long-term, each room should support:

- Human traders
- AI traders
- Spectators
- Instructors or moderators

Possible room modes:

- Free trading mode
- Low-slippage execution challenge
- Market-making challenge
- Stop-hunt observation
- Liquidity crisis scenario
- Panic selloff scenario
- Team competition
- Classroom mode

Each room owns its own:

- Instrument
- Order book
- Accounts
- Agent set
- Scenario rules
- Event log
- Market clock

This makes horizontal scaling possible later: one process can run one high-load room or several low-load rooms.

## 11. Risk And Fairness

Even though assets are virtual, risk rules are required for realism.

Minimum risk controls:

- Cash balance check
- Maximum order quantity
- Maximum position size
- Price band limit
- Cancel-rate limit
- Self-trade prevention
- Fee model
- Margin check if leverage is enabled
- Liquidation rule if margin is enabled
- Kill switch for abnormal accounts or agents

Fairness rules:

- AI cannot see hidden user information.
- AI cannot read future events.
- AI cannot bypass latency, fees, or risk.
- AI cannot submit commands unavailable to humans unless the scenario explicitly says so.
- The system, not the client, assigns authoritative exchange timestamps.

These constraints make the market feel real.

## 12. Frontend Direction

The frontend should be an order-flow microscope, not a normal broker clone.

Primary views:

- Order book depth
- Trade tape
- Price chart
- User orders
- Positions and PnL
- Queue position
- Spread and imbalance
- AI coach panel
- Replay controls

Important visualizations:

- L2 depth bars
- Active buy and active sell tape colors
- Large trade markers
- Cancel intensity
- Slippage display
- Liquidity gaps
- Stop-trigger areas
- User order queue position
- Market-maker withdrawal behavior

Candlesticks can exist, but they should be secondary.

The main interface should make the user feel the order book changing, not only the price chart moving.

## 13. Recommended Technical Direction

For a serious long-term project:

- Matching core: Rust
- Real-time gateway: Rust or Go
- Agent runtime: Python, with a Rust/HTTP/WebSocket SDK
- AI coach: Python
- Frontend: React + TypeScript + Vite
- Real-time client communication: WebSocket
- Early storage: SQLite + JSONL or Parquet event logs
- Later business database: PostgreSQL
- Later event bus: NATS JetStream, Redpanda, or Kafka
- Later analytics database: ClickHouse
- Later object storage: MinIO or S3
- Later observability: OpenTelemetry + Prometheus + Grafana

For the first working version, avoid distributed complexity.

The first version can be:

```text
Rust exchange core
Rust WebSocket server
React frontend
JSONL event log
SQLite metadata
Rule-based Python or Rust agents
```

This keeps the project serious without turning the first milestone into infrastructure work.

## 14. Development Roadmap

### Phase 1: Single-Market Core

Goal:

Build one reliable virtual market for one instrument, `V-BTC`.

Scope:

- Deterministic matching engine
- Limit order, market order, cancel order
- Integer ticks and quantities
- Basic account, cash, position, PnL
- Fee model
- Basic risk checks
- Event log with sequence numbers
- Snapshot and replay
- Simple WebSocket market data
- Minimal frontend
- 5 to 10 rule-based agents

Success criteria:

- A user can join one room and trade against AI agents.
- The order book updates in real time.
- Trades update positions and PnL.
- The full session can be replayed.
- Automated tests prove deterministic matching behavior.

### Phase 2: Training Product Layer

Goal:

Turn the market engine into a useful training tool.

Scope:

- Scenario system
- Training missions
- Replay viewer
- Performance metrics
- Slippage and execution analysis
- Basic AI coach
- Session summary
- Leaderboard for selected tasks

Example missions:

- Build a position with low slippage.
- Exit without revealing intent.
- Survive a liquidity shock.
- Identify stop-loss cascades.
- Act as a market maker without inventory blowup.

### Phase 3: Multiplayer Rooms

Goal:

Support multiple humans and rooms.

Scope:

- User accounts
- Room creation
- Multiplayer trading
- Spectator mode
- Instructor controls
- Room-level configuration
- Persistent session records

### Phase 4: Agent Platform

Goal:

Make AI participants programmable and scalable.

Scope:

- Agent SDK
- Python strategy interface
- Agent parameter system
- Agent latency simulation
- Agent population presets
- Batch simulation mode
- Offline evaluation

### Phase 5: Scalable Platform

Goal:

Prepare for many rooms, large logs, and heavier analytics.

Scope:

- Room scheduling
- Event bus
- ClickHouse analytics
- Object storage
- Monitoring
- Distributed deployment
- Automated replay verification

## 15. What Not To Do Early

Avoid these in the first phase:

- Do not let AI generate prices directly.
- Do not call LLMs every tick.
- Do not use floats for price.
- Do not make K-line charts the center of the product.
- Do not store every matching mutation through PostgreSQL.
- Do not build Kubernetes, Kafka, or ClickHouse before one market works.
- Do not add many order types before basic order behavior is tested.
- Do not build many instruments before one instrument is stable.
- Do not optimize for HFT latency before correctness and replayability are proven.

The early product should be small, strict, and correct.

## 16. First Implementation Target

The first concrete milestone should be:

> A single-room `V-BTC` market where one human can trade against several rule-based AI agents, with real-time order book updates, position/PnL, and deterministic replay.

Suggested initial repository shape:

```text
MarketForge/
├── docs/
│   └── DESIGN.md
├── exchange-core/
│   ├── src/
│   └── tests/
├── exchange-server/
│   ├── src/
│   └── tests/
├── frontend/
│   └── src/
├── agents/
│   └── python/
├── scenarios/
│   └── v_btc_basic.json
└── data/
    ├── events/
    └── snapshots/
```

Initial engineering focus:

- Correct matching.
- Deterministic replay.
- Clean event model.
- Clear risk checks.
- Simple but useful order book UI.

If this foundation is solid, the later AI, multiplayer, analytics, and coaching layers will have something real to build on.

## 17. One-Sentence Summary

MarketForge should be built as a small but serious virtual exchange first, and only then expanded into a multiplayer AI trading training platform.
