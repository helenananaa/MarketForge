-- New checkpoints retain the complete checked JSON payload as bytes. Avoid
-- building a full PostgreSQL JSONB tree on the synchronous execution lane.
-- Old JSONB rows remain readable and are never rewritten by this migration.
ALTER TABLE marketforge_room_snapshots ADD COLUMN actor_bytes BYTEA;
ALTER TABLE marketforge_room_snapshots ALTER COLUMN actor_json DROP NOT NULL;
ALTER TABLE marketforge_room_snapshots ADD CONSTRAINT marketforge_snapshot_one_encoding
    CHECK (num_nonnulls(actor_json, actor_bytes) = 1);

-- Large checkpoints must not spend hundreds of milliseconds in pglz while
-- holding a room execution lane. Change future TOAST compression only;
-- canonical JSONB contents, transactional durability and old rows are unchanged.
-- PostgreSQL builds without lz4 retain their existing compression method.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_settings
        WHERE name = 'default_toast_compression' AND 'lz4' = ANY(enumvals)
    ) THEN
        ALTER TABLE marketforge_room_snapshots
            ALTER COLUMN actor_json SET COMPRESSION lz4;
        ALTER TABLE marketforge_room_snapshots
            ALTER COLUMN actor_bytes SET COMPRESSION lz4;
        ALTER TABLE marketforge_room_mutations
            ALTER COLUMN payload_json SET COMPRESSION lz4;
    END IF;
END
$$;
