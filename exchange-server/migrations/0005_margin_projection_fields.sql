ALTER TABLE marketforge_account_ledger
    ADD COLUMN IF NOT EXISTS maintenance_margin BIGINT;

ALTER TABLE marketforge_account_ledger
    ADD COLUMN IF NOT EXISTS margin_status TEXT;

ALTER TABLE marketforge_position_snapshots
    ADD COLUMN IF NOT EXISTS maintenance_margin BIGINT;

ALTER TABLE marketforge_position_snapshots
    ADD COLUMN IF NOT EXISTS margin_status TEXT;
