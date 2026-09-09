-- Self-heal databases missing the legacy `config` table (#472).
--
-- The bootstrap schema (`sql/sqlite_20260309_init.sql`) has always defined
-- both `config` and `config_kv`, but some stores were created by builds whose
-- bootstrap omitted the legacy table. The schema top-up on open repaired
-- `config_kv`, the AI projection and the operation tables, yet nothing
-- recreated `config` — so every legacy-config reader failed with
-- `no such table: config` (observed as `libra init` refusing to start when
-- the config cascade probed such a store).
--
-- The DDL matches the bootstrap definition exactly and is idempotent: stores
-- that already carry the table are untouched, stores missing it gain an empty
-- one (they cannot hold legacy rows by construction).

CREATE TABLE IF NOT EXISTS `config` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `configuration` TEXT NOT NULL,
    `name` TEXT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL
);
