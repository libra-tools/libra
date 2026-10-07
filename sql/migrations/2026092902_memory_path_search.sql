-- Memory path / search-document projection schema (plan-20260926 DM-10, migration B).
--
-- These tables are ZERO-AUTHORITY and fully REBUILDABLE (GC-DM-01): every row is
-- a pure function of the repository's durable facts (commit/change, agent
-- session, agent run, bridge operation). They hold no authoritative state and
-- can be re-derived at any time via `libra memory rebuild`. A stale index never
-- answers (freshness is validated on read, mirroring 2026070301_revision_ordinal).
--
-- This migration is delivered `forward-only` (ADR-DM-13): once applied, an older
-- binary cannot reopen the repository database (future-schema rejection is
-- exercised by DM-10's `memory_path_search_simulated_old_runner_rejects_future_schema`).
-- The `_down.sql` is provided and tested but is only a controlled test/ops tool,
-- not the published rollback path (progression by patching forward).
--
-- Table layout is frozen verbatim by plan-20260926 "Schema 规范" (DM-10 rows only).
-- The core three tables belong to DM-01 (`2026092901_memory_core`); DM-01
-- deliberately does NOT create the FTS5 virtual table (that is DM-08).

CREATE TABLE IF NOT EXISTS `memory_episode_path` (
    `episode_id`     TEXT NOT NULL,
    `code_path`      BLOB NOT NULL,
    `change_kind`    TEXT NOT NULL CHECK (`change_kind` IN
                        ('added','modified','deleted','renamed')),
    `blob_oid_at_end` TEXT,
    `ended_at`       INTEGER NOT NULL,
    PRIMARY KEY (`episode_id`, `code_path`),
    FOREIGN KEY (`episode_id`) REFERENCES `memory_episode`(`episode_id`) ON DELETE CASCADE,
    CHECK (length(`code_path`) > 0 AND substr(`code_path`, 1, 1) <> X'2F')
);

CREATE INDEX IF NOT EXISTS `idx_memory_episode_path_code_path_ended_at`
    ON `memory_episode_path` (`code_path`, `ended_at` DESC);

CREATE TABLE IF NOT EXISTS `memory_episode_search_doc` (
    `rowid`       INTEGER PRIMARY KEY,
    `episode_id`  TEXT NOT NULL UNIQUE,
    `title`       TEXT NOT NULL,
    `body`        TEXT NOT NULL,
    `paths_text`  TEXT NOT NULL,
    FOREIGN KEY (`episode_id`) REFERENCES `memory_episode`(`episode_id`) ON DELETE CASCADE
);
