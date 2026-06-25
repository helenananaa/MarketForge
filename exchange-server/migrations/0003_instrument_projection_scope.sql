ALTER TABLE marketforge_orders
    ADD COLUMN IF NOT EXISTS instrument_id TEXT NOT NULL DEFAULT 'legacy-primary';

ALTER TABLE marketforge_order_events
    ADD COLUMN IF NOT EXISTS instrument_id TEXT NOT NULL DEFAULT 'legacy-primary';

ALTER TABLE marketforge_trades
    ADD COLUMN IF NOT EXISTS instrument_id TEXT NOT NULL DEFAULT 'legacy-primary';

ALTER TABLE marketforge_market_ticks
    ADD COLUMN IF NOT EXISTS instrument_id TEXT NOT NULL DEFAULT 'legacy-primary';

ALTER TABLE marketforge_account_ledger
    ADD COLUMN IF NOT EXISTS instrument_id TEXT NOT NULL DEFAULT 'legacy-primary';

ALTER TABLE marketforge_position_snapshots
    ADD COLUMN IF NOT EXISTS instrument_id TEXT NOT NULL DEFAULT 'legacy-primary';

ALTER TABLE marketforge_orders
    DROP CONSTRAINT IF EXISTS marketforge_orders_pkey;
ALTER TABLE marketforge_orders
    ADD PRIMARY KEY (room_id, instrument_id, order_id);

DROP INDEX IF EXISTS marketforge_orders_account_idx;
CREATE INDEX IF NOT EXISTS marketforge_orders_account_idx
    ON marketforge_orders(room_id, instrument_id, account_id, updated_command_seq);

ALTER TABLE marketforge_trades
    DROP CONSTRAINT IF EXISTS marketforge_trades_pkey;
ALTER TABLE marketforge_trades
    ADD PRIMARY KEY (room_id, instrument_id, trade_id);

DROP INDEX IF EXISTS marketforge_trades_taker_idx;
CREATE INDEX IF NOT EXISTS marketforge_trades_taker_idx
    ON marketforge_trades(room_id, instrument_id, taker_account_id, command_seq);

DROP INDEX IF EXISTS marketforge_trades_maker_idx;
CREATE INDEX IF NOT EXISTS marketforge_trades_maker_idx
    ON marketforge_trades(room_id, instrument_id, maker_account_id, command_seq);

DROP INDEX IF EXISTS marketforge_market_ticks_room_time_idx;
CREATE INDEX IF NOT EXISTS marketforge_market_ticks_room_time_idx
    ON marketforge_market_ticks(room_id, instrument_id, created_at, command_seq, event_seq);

DROP INDEX IF EXISTS marketforge_account_ledger_account_idx;
CREATE INDEX IF NOT EXISTS marketforge_account_ledger_account_idx
    ON marketforge_account_ledger(room_id, instrument_id, account_id, command_seq, ledger_seq);

DROP INDEX IF EXISTS marketforge_position_snapshots_account_latest_idx;
CREATE INDEX IF NOT EXISTS marketforge_position_snapshots_account_latest_idx
    ON marketforge_position_snapshots(room_id, instrument_id, account_id, command_seq DESC, ledger_seq DESC);
