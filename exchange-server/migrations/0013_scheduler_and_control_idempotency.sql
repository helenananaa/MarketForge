-- Scheduler progress is stored as room mutations (kind scheduler_progress).
-- Control-write idempotency is a dedicated lookup so pause/step retries do not
-- double-apply after a lost HTTP response.

CREATE TABLE IF NOT EXISTS marketforge_control_idempotency (
    user_id TEXT NOT NULL,
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    response_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, room_id, idempotency_key)
);
