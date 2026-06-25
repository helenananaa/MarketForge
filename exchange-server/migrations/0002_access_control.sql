CREATE TABLE IF NOT EXISTS marketforge_users (
    user_id TEXT PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS marketforge_room_members (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES marketforge_users(user_id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, user_id)
);

CREATE INDEX IF NOT EXISTS marketforge_room_members_user_idx
    ON marketforge_room_members(user_id, room_id);

CREATE TABLE IF NOT EXISTS marketforge_account_owners (
    room_id TEXT NOT NULL REFERENCES marketforge_rooms(room_id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL,
    user_id TEXT NOT NULL REFERENCES marketforge_users(user_id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (room_id, account_id, user_id)
);

CREATE INDEX IF NOT EXISTS marketforge_account_owners_user_idx
    ON marketforge_account_owners(user_id, room_id, account_id);
