-- Small mutable recovery metadata; immutable command/event journals remain intact.
CREATE TABLE IF NOT EXISTS marketforge_recovery_heads (
    room_id TEXT PRIMARY KEY REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    next_order_id BIGINT NOT NULL DEFAULT 1 CHECK (next_order_id > 0),
    checkpoint_mutation_seq BIGINT,
    snapshot_command_seq BIGINT,
    checkpoint_command_cursor BIGINT NOT NULL DEFAULT 0 CHECK (checkpoint_command_cursor >= 0)
);

INSERT INTO marketforge_recovery_heads (room_id, next_order_id, checkpoint_mutation_seq, checkpoint_command_cursor)
SELECT room.room_id,
    COALESCE((SELECT MAX((execution.command_json->'NewOrder'->>'order_id')::BIGINT)+1
              FROM marketforge_executions execution
              WHERE execution.room_id=room.room_id
                AND execution.command_json ? 'NewOrder'
                AND (execution.command_json->'NewOrder'->>'order_id')::NUMERIC < 9000000000000000000),1),
    checkpoint.mutation_seq, COALESCE(checkpoint.command_cursor,0)
FROM marketforge_rooms room
LEFT JOIN LATERAL (
    SELECT mutation_seq,command_cursor FROM marketforge_room_mutations
    WHERE room_id=room.room_id AND mutation_kind='state_checkpoint'
    ORDER BY mutation_seq DESC LIMIT 1
) checkpoint ON true
ON CONFLICT (room_id) DO NOTHING;

CREATE INDEX IF NOT EXISTS marketforge_mutations_room_sequence_idx
    ON marketforge_room_mutations(room_id, mutation_seq);

CREATE INDEX IF NOT EXISTS marketforge_mutations_kind_latest_idx
    ON marketforge_room_mutations(room_id, mutation_kind, mutation_seq DESC);
CREATE INDEX IF NOT EXISTS marketforge_ticks_latest_instrument_idx
    ON marketforge_market_ticks(room_id, instrument_id, command_seq DESC, event_seq DESC);
CREATE INDEX IF NOT EXISTS marketforge_orders_room_id_latest_idx
    ON marketforge_orders(room_id, order_id DESC);

-- Latest scheduler/training payloads before the actor checkpoint are needed even
-- though their clock/state effects must not be replayed a second time.
CREATE OR REPLACE VIEW marketforge_runtime_mutations AS
SELECT mutation.* FROM marketforge_room_mutations mutation
LEFT JOIN marketforge_recovery_heads head USING (room_id)
WHERE mutation.mutation_seq > COALESCE(head.checkpoint_mutation_seq,0)
   OR (head.snapshot_command_seq IS NULL AND mutation.mutation_seq=COALESCE(head.checkpoint_mutation_seq,0))
UNION
SELECT latest_scheduler.* FROM (
    SELECT DISTINCT ON (room_id) * FROM marketforge_room_mutations
    WHERE mutation_kind='scheduler_progress' ORDER BY room_id, mutation_seq DESC
) latest_scheduler
UNION
SELECT latest_training.* FROM (
    SELECT DISTINCT ON (room_id, payload_json->'run'->'spec'->>'run_id') *
    FROM marketforge_room_mutations WHERE mutation_kind='training_progress'
    ORDER BY room_id, payload_json->'run'->'spec'->>'run_id', mutation_seq DESC
) latest_training;

CREATE INDEX IF NOT EXISTS marketforge_invalid_system_orders_idx
    ON marketforge_executions(room_id, command_seq)
    WHERE command_json ? 'NewOrder'
      AND (command_json->'NewOrder'->>'order_id')::NUMERIC >= 9000000000000000000
      AND NOT COALESCE(
          participant_id IS NULL AND (command_json->'NewOrder'->>'reduce_only')::boolean
          AND command_json->'NewOrder'->'kind' IN (
              '"Market"'::jsonb,
              '{"ImmediateOrCancel":{"price_tick":null}}'::jsonb,
              '{"FillOrKill":{"price_tick":null}}'::jsonb
          ),false);

CREATE OR REPLACE VIEW marketforge_runtime_executions AS
SELECT execution.* FROM marketforge_executions execution
LEFT JOIN marketforge_recovery_heads head USING (room_id)
WHERE execution.command_seq >= COALESCE(head.checkpoint_command_cursor,0)
UNION
-- Preserve validation of malformed legacy records in the reserved range.
SELECT execution.* FROM marketforge_executions execution
WHERE execution.command_json ? 'NewOrder'
  AND (execution.command_json->'NewOrder'->>'order_id')::NUMERIC >= 9000000000000000000
  AND NOT COALESCE(
      execution.participant_id IS NULL
      AND (execution.command_json->'NewOrder'->>'reduce_only')::boolean
      AND execution.command_json->'NewOrder'->'kind' IN (
          '"Market"'::jsonb,
          '{"ImmediateOrCancel":{"price_tick":null}}'::jsonb,
          '{"FillOrKill":{"price_tick":null}}'::jsonb
      ),false);
