-- Authority stays in the canonical journal. These compact keys preserve the
-- synchronous uniqueness checks formerly supplied by the query projections.
CREATE TABLE marketforge_order_keys (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    instrument_id TEXT NOT NULL,
    order_id BIGINT NOT NULL,
    PRIMARY KEY (room_id, instrument_id, order_id)
);
INSERT INTO marketforge_order_keys SELECT room_id,instrument_id,order_id FROM marketforge_orders;
CREATE INDEX marketforge_order_keys_latest_idx ON marketforge_order_keys(room_id,order_id DESC)
    WHERE order_id < 7000000000000000000;
CREATE TABLE marketforge_trade_keys (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    instrument_id TEXT NOT NULL,
    trade_id BIGINT NOT NULL,
    PRIMARY KEY (room_id, instrument_id, trade_id)
);
INSERT INTO marketforge_trade_keys SELECT room_id,instrument_id,trade_id FROM marketforge_trades;

-- An outbox entry references immutable execution data; it does not copy JSON.
-- Rows are removed only in the transaction that updates all query tables.
-- Unlike a global sequence cursor, this cannot skip a late-committing writer.
CREATE TABLE marketforge_projection_queue (
    queue_id BIGSERIAL UNIQUE NOT NULL,
    room_id TEXT NOT NULL,
    command_seq BIGINT NOT NULL,
    PRIMARY KEY (room_id, command_seq),
    FOREIGN KEY (room_id,command_seq) REFERENCES marketforge_executions(room_id,command_seq) ON DELETE CASCADE
);
CREATE TABLE marketforge_projection_progress (
    room_id TEXT PRIMARY KEY REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_seq BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO marketforge_projection_progress(room_id,command_seq)
SELECT room_id,MAX(command_seq) FROM marketforge_executions GROUP BY room_id;
CREATE INDEX marketforge_projection_room_queue_idx ON marketforge_projection_queue(room_id,queue_id);

CREATE TABLE marketforge_transfer_projection_queue (
    queue_id BIGSERIAL UNIQUE NOT NULL,
    room_id TEXT NOT NULL,
    transfer_id BIGINT NOT NULL,
    event_seq BIGINT NOT NULL,
    PRIMARY KEY(room_id,transfer_id,event_seq),
    FOREIGN KEY(room_id,transfer_id,event_seq)
        REFERENCES marketforge_transfer_events(room_id,transfer_id,event_seq) ON DELETE CASCADE
);
CREATE INDEX marketforge_transfer_projection_room_queue_idx ON marketforge_transfer_projection_queue(room_id,queue_id);

-- Recovery's latest trade comes directly from the authoritative journal, even
-- when query projections are delayed or their worker is unavailable.
CREATE INDEX marketforge_execution_latest_trade_idx ON marketforge_executions
    (room_id,(COALESCE(execution_json->>'instrument_id','legacy-primary')),command_seq DESC)
    WHERE execution_json->'events' @> '[{"type":"TradePrinted"}]'::JSONB;
