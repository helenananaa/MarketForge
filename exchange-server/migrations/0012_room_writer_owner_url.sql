ALTER TABLE marketforge_room_writer_leases
    ADD COLUMN IF NOT EXISTS owner_url TEXT
    CHECK (
        owner_url IS NULL
        OR char_length(owner_url) BETWEEN 1 AND 2048
    );
