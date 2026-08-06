ALTER TABLE marketforge_executions
    ADD COLUMN IF NOT EXISTS request_user_id TEXT;

ALTER TABLE marketforge_executions
    ADD COLUMN IF NOT EXISTS idempotency_key TEXT;

ALTER TABLE marketforge_executions
    ADD COLUMN IF NOT EXISTS request_fingerprint TEXT;

ALTER TABLE marketforge_executions
    DROP CONSTRAINT IF EXISTS marketforge_executions_idempotency_fields_check;

ALTER TABLE marketforge_executions
    ADD CONSTRAINT marketforge_executions_idempotency_fields_check CHECK (
        (request_user_id IS NULL AND idempotency_key IS NULL AND request_fingerprint IS NULL)
        OR
        (request_user_id IS NOT NULL AND idempotency_key IS NOT NULL AND request_fingerprint IS NOT NULL)
    );

CREATE UNIQUE INDEX IF NOT EXISTS marketforge_executions_idempotency_idx
    ON marketforge_executions(room_id, request_user_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
