CREATE TABLE IF NOT EXISTS marketforge_room_mutations (
    mutation_seq BIGSERIAL PRIMARY KEY,
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    command_cursor BIGINT NOT NULL CHECK (command_cursor >= 0),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    mutation_kind TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS marketforge_room_mutations_replay_idx
    ON marketforge_room_mutations(room_id, command_cursor, mutation_seq);

-- Existing installations only recorded non-command state in snapshots. Preserve the
-- latest compatible snapshot as a one-time replay checkpoint before snapshots become
-- an optional optimization. Prefer the serialized room-global cursor; older actors
-- fall back to the sum of their per-venue cursors. The timestamp check is only for
-- legacy actors with neither representation, where a sequence-0 snapshot may predate
-- command 0.
INSERT INTO marketforge_room_mutations (
    room_id,
    command_cursor,
    schema_version,
    mutation_kind,
    payload_json
)
SELECT DISTINCT ON (snapshot.room_id)
    snapshot.room_id,
    replay_cursor.command_cursor,
    1,
    'state_checkpoint',
    jsonb_build_object(
        'kind',
        'state_checkpoint',
        'actor',
        jsonb_set(
            snapshot.actor_json,
            '{next_command_seq}',
            to_jsonb(replay_cursor.command_cursor),
            true
        )
    )
FROM marketforge_room_snapshots AS snapshot
CROSS JOIN LATERAL (
    SELECT COALESCE(
        (snapshot.actor_json ->> 'next_command_seq')::BIGINT,
        (
            SELECT SUM((exchange_entry.value ->> 'next_command_seq')::BIGINT)
            FROM jsonb_each(snapshot.actor_json -> 'exchanges') AS exchange_entry
        ),
        CASE
            WHEN EXISTS (
                SELECT 1
                FROM marketforge_executions AS execution
                WHERE execution.room_id = snapshot.room_id
                  AND execution.command_seq = snapshot.command_seq
                  AND execution.created_at <= snapshot.created_at
            )
            THEN (snapshot.command_seq::numeric + 1)::BIGINT
            ELSE 0
        END
    ) AS command_cursor
) AS replay_cursor
WHERE snapshot.actor_json ? 'exchanges'
  AND snapshot.actor_json ? 'primary_venue_id'
ORDER BY snapshot.room_id, snapshot.command_seq DESC;

-- A snapshot historically did not move when a room was paused or resumed. Record
-- the authoritative current status after the migrated checkpoint so recovery can
-- validate it instead of silently overwriting replayed state from the rooms table.
INSERT INTO marketforge_room_mutations (
    room_id,
    command_cursor,
    schema_version,
    mutation_kind,
    payload_json
)
SELECT
    room.room_id,
    GREATEST(
        checkpoint.command_cursor,
        COALESCE((
            SELECT (MAX(execution.command_seq)::numeric + 1)::BIGINT
            FROM marketforge_executions AS execution
            WHERE execution.room_id = room.room_id
        ), checkpoint.command_cursor)
    ),
    1,
    'status_changed',
    jsonb_build_object(
        'kind',
        'status_changed',
        'status',
        CASE room.status
            WHEN 'running' THEN 'Running'
            WHEN 'paused' THEN 'Paused'
            WHEN 'closed' THEN 'Closed'
        END
    )
FROM marketforge_rooms AS room
JOIN marketforge_room_mutations AS checkpoint
  ON checkpoint.room_id = room.room_id
 AND checkpoint.mutation_kind = 'state_checkpoint';
