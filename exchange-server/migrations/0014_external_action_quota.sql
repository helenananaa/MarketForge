-- Durable per-step external action quota. Counts are consumed in the same
-- transaction as the order execution they belong to so a failed commit does
-- not permanently decrement the remaining budget.

CREATE TABLE IF NOT EXISTS marketforge_external_action_counts (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    step BIGINT NOT NULL,
    count INTEGER NOT NULL CHECK (count >= 0),
    PRIMARY KEY (room_id, user_id, step)
);
