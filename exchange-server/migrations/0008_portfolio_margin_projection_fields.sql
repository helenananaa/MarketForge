ALTER TABLE marketforge_account_ledger
    ADD COLUMN IF NOT EXISTS portfolio_initial_margin BIGINT;

ALTER TABLE marketforge_account_ledger
    ADD COLUMN IF NOT EXISTS portfolio_maintenance_margin BIGINT;

ALTER TABLE marketforge_position_snapshots
    ADD COLUMN IF NOT EXISTS portfolio_initial_margin BIGINT;

ALTER TABLE marketforge_position_snapshots
    ADD COLUMN IF NOT EXISTS portfolio_maintenance_margin BIGINT;
