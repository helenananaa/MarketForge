CREATE TABLE IF NOT EXISTS marketforge_rooms (
    room_id TEXT PRIMARY KEY,
    scenario_json JSONB NOT NULL,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS marketforge_executions (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    participant_id TEXT,
    account_id TEXT,
    command_json JSONB NOT NULL,
    execution_json JSONB NOT NULL,
    accepted BOOLEAN NOT NULL,
    reject_reason TEXT,
    clearing_event_count INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, command_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_executions_room_created_idx
    ON marketforge_executions(room_id, created_at);

CREATE TABLE IF NOT EXISTS marketforge_room_snapshots (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    actor_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, command_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_room_snapshots_latest_idx
    ON marketforge_room_snapshots(room_id, command_seq DESC);

CREATE TABLE IF NOT EXISTS marketforge_orders (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    order_id BIGINT NOT NULL,
    account_id BIGINT NOT NULL,
    participant_id TEXT,
    side TEXT NOT NULL,
    order_type TEXT NOT NULL,
    limit_price_tick BIGINT,
    original_qty BIGINT NOT NULL,
    status TEXT NOT NULL,
    remaining_qty BIGINT NOT NULL,
    created_command_seq BIGINT NOT NULL,
    updated_command_seq BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, order_id)
);

CREATE INDEX IF NOT EXISTS marketforge_orders_account_idx
    ON marketforge_orders(room_id, account_id, updated_command_seq);

CREATE TABLE IF NOT EXISTS marketforge_order_events (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    event_seq BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    order_id BIGINT,
    reason TEXT,
    price_tick BIGINT,
    qty BIGINT,
    remaining_qty BIGINT,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, command_seq, event_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_order_events_order_idx
    ON marketforge_order_events(room_id, order_id, command_seq, event_seq);

CREATE TABLE IF NOT EXISTS marketforge_trades (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    trade_id BIGINT NOT NULL,
    command_seq BIGINT NOT NULL,
    event_seq BIGINT NOT NULL,
    maker_order_id BIGINT NOT NULL,
    maker_account_id BIGINT NOT NULL,
    taker_order_id BIGINT NOT NULL,
    taker_account_id BIGINT NOT NULL,
    price_tick BIGINT NOT NULL,
    qty BIGINT NOT NULL,
    taker_side TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, trade_id),
    UNIQUE (room_id, command_seq, event_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_trades_taker_idx
    ON marketforge_trades(room_id, taker_account_id, command_seq);

CREATE INDEX IF NOT EXISTS marketforge_trades_maker_idx
    ON marketforge_trades(room_id, maker_account_id, command_seq);

CREATE TABLE IF NOT EXISTS marketforge_market_ticks (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    event_seq BIGINT NOT NULL,
    trade_id BIGINT NOT NULL,
    price_tick BIGINT NOT NULL,
    qty BIGINT NOT NULL,
    taker_side TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, command_seq, event_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_market_ticks_room_time_idx
    ON marketforge_market_ticks(room_id, created_at, command_seq, event_seq);

CREATE TABLE IF NOT EXISTS marketforge_account_ledger (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    ledger_seq BIGINT NOT NULL,
    market_kind TEXT NOT NULL,
    account_id BIGINT NOT NULL,
    trade_id BIGINT NOT NULL,
    account_side TEXT NOT NULL,
    cash_delta BIGINT NOT NULL,
    position_delta BIGINT NOT NULL,
    fee BIGINT NOT NULL,
    realized_pnl BIGINT NOT NULL,
    price_tick BIGINT NOT NULL,
    qty BIGINT NOT NULL,
    notional BIGINT NOT NULL,
    cash_balance BIGINT NOT NULL,
    position_qty BIGINT NOT NULL,
    avg_entry_price_tick BIGINT,
    realized_pnl_total BIGINT,
    unrealized_pnl BIGINT,
    equity BIGINT,
    initial_margin BIGINT,
    fees_paid BIGINT NOT NULL,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, command_seq, ledger_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_account_ledger_account_idx
    ON marketforge_account_ledger(room_id, account_id, command_seq, ledger_seq);

CREATE TABLE IF NOT EXISTS marketforge_position_snapshots (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    ledger_seq BIGINT NOT NULL,
    market_kind TEXT NOT NULL,
    account_id BIGINT NOT NULL,
    trade_id BIGINT NOT NULL,
    cash_balance BIGINT NOT NULL,
    position_qty BIGINT NOT NULL,
    avg_entry_price_tick BIGINT,
    realized_pnl BIGINT,
    unrealized_pnl BIGINT,
    equity BIGINT,
    initial_margin BIGINT,
    fees_paid BIGINT NOT NULL,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, command_seq, ledger_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_position_snapshots_account_latest_idx
    ON marketforge_position_snapshots(room_id, account_id, command_seq DESC, ledger_seq DESC);
