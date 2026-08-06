ALTER TABLE marketforge_executions
    ADD COLUMN IF NOT EXISTS market_time_ms BIGINT;

ALTER TABLE marketforge_orders
    ADD COLUMN IF NOT EXISTS created_market_time_ms BIGINT;

ALTER TABLE marketforge_orders
    ADD COLUMN IF NOT EXISTS updated_market_time_ms BIGINT;

ALTER TABLE marketforge_trades
    ADD COLUMN IF NOT EXISTS market_time_ms BIGINT;

ALTER TABLE marketforge_market_ticks
    ADD COLUMN IF NOT EXISTS market_time_ms BIGINT;

UPDATE marketforge_orders AS projected_order
SET created_market_time_ms = execution.market_time_ms
FROM marketforge_executions AS execution
WHERE projected_order.room_id = execution.room_id
  AND projected_order.created_command_seq = execution.command_seq
  AND projected_order.created_market_time_ms IS NULL;

UPDATE marketforge_orders AS projected_order
SET updated_market_time_ms = execution.market_time_ms
FROM marketforge_executions AS execution
WHERE projected_order.room_id = execution.room_id
  AND projected_order.updated_command_seq = execution.command_seq
  AND projected_order.updated_market_time_ms IS NULL;

UPDATE marketforge_trades AS projected_trade
SET market_time_ms = execution.market_time_ms
FROM marketforge_executions AS execution
WHERE projected_trade.room_id = execution.room_id
  AND projected_trade.command_seq = execution.command_seq
  AND projected_trade.market_time_ms IS NULL;

UPDATE marketforge_market_ticks AS projected_tick
SET market_time_ms = execution.market_time_ms
FROM marketforge_executions AS execution
WHERE projected_tick.room_id = execution.room_id
  AND projected_tick.command_seq = execution.command_seq
  AND projected_tick.market_time_ms IS NULL;

CREATE INDEX IF NOT EXISTS marketforge_executions_room_market_time_idx
    ON marketforge_executions(room_id, market_time_ms, command_seq);

DROP INDEX IF EXISTS marketforge_market_ticks_room_time_idx;
CREATE INDEX IF NOT EXISTS marketforge_market_ticks_room_time_idx
    ON marketforge_market_ticks(
        room_id,
        instrument_id,
        market_time_ms,
        command_seq,
        event_seq
    );
