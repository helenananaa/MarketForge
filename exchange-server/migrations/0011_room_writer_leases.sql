CREATE TABLE IF NOT EXISTS marketforge_room_writer_leases (
    room_id TEXT PRIMARY KEY REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL,
    fencing_token BIGINT NOT NULL CHECK (fencing_token > 0),
    lease_expires_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX IF NOT EXISTS marketforge_room_writer_leases_expiry_idx
    ON marketforge_room_writer_leases(lease_expires_at);
