CREATE TABLE IF NOT EXISTS marketforge_transfers (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    transfer_id BIGINT NOT NULL,
    kind TEXT NOT NULL,
    account_id BIGINT NOT NULL,
    asset_id TEXT NOT NULL,
    amount BIGINT NOT NULL,
    requested_at_step BIGINT NOT NULL,
    available_after_step BIGINT NOT NULL,
    completed_at_step BIGINT,
    status TEXT NOT NULL,
    reject_reason TEXT,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, transfer_id)
);

CREATE INDEX IF NOT EXISTS marketforge_transfers_account_idx
    ON marketforge_transfers(room_id, account_id, transfer_id DESC);

CREATE INDEX IF NOT EXISTS marketforge_transfers_status_idx
    ON marketforge_transfers(room_id, status, available_after_step);

CREATE TABLE IF NOT EXISTS marketforge_transfer_events (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    transfer_id BIGINT NOT NULL,
    event_seq BIGINT NOT NULL,
    kind TEXT NOT NULL,
    account_id BIGINT NOT NULL,
    asset_id TEXT NOT NULL,
    amount BIGINT NOT NULL,
    requested_at_step BIGINT NOT NULL,
    available_after_step BIGINT NOT NULL,
    completed_at_step BIGINT,
    status TEXT NOT NULL,
    reject_reason TEXT,
    payload_json JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, transfer_id, event_seq)
);

CREATE INDEX IF NOT EXISTS marketforge_transfer_events_account_idx
    ON marketforge_transfer_events(room_id, account_id, transfer_id DESC, event_seq DESC);
