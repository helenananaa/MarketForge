-- Versioned platform records share the journal writer and invitation grants commit
-- in the same PostgreSQL transaction as room membership/account ownership.
CREATE TABLE marketforge_platform_revision (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    revision BIGINT NOT NULL DEFAULT 0
);
INSERT INTO marketforge_platform_revision(singleton) VALUES (TRUE);
CREATE TABLE marketforge_platform_records (
    kind TEXT NOT NULL CHECK (kind IN ('users','sessions','invitations','competitions')),
    record_id TEXT NOT NULL,
    body JSONB NOT NULL,
    PRIMARY KEY(kind, record_id)
);
CREATE UNIQUE INDEX marketforge_platform_username_idx
    ON marketforge_platform_records ((body->>'username')) WHERE kind = 'users';
