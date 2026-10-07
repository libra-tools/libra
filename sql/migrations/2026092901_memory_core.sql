-- Memory core projection schema (plan-20260926 DM-01, migration A).
--
-- These tables are ZERO-AUTHORITY and fully REBUILDABLE (GC-DM-01): every row is
-- a pure function of the repository's durable facts (commit/change, agent
-- session, agent run, bridge operation). They hold no authoritative state and
-- can be re-derived at any time via `libra memory rebuild`. A stale index never
-- answers (freshness is validated on read, mirroring 2026070301_revision_ordinal).
--
-- This migration is delivered `forward-only` (ADR-DM-13): once applied, an older
-- binary cannot reopen the repository database (future-schema rejection is
-- exercised by DM-10's simulated-old-runner test). The `_down.sql` is provided
-- and tested but is only a controlled test/ops tool, not the published rollback
-- path (progression by patching forward).
--
-- Table layout is frozen verbatim by plan-20260926 "Schema 规范" (DM-01 rows only).
-- `memory_episode_path` / `memory_episode_search_doc` belong to DM-10.
-- DM-01 deliberately does NOT create the FTS5 virtual table (that is DM-08).

CREATE TABLE IF NOT EXISTS `memory_episode` (
    `episode_id`     TEXT PRIMARY KEY,
    `repo_id`        TEXT NOT NULL,
    `source_kind`    TEXT NOT NULL CHECK (`source_kind` IN
                       ('commit','agent_session','agent_run','bridge_operation')),
    `source_key`     TEXT NOT NULL,
    `outcome`        TEXT NOT NULL CHECK (`outcome` IN
                       ('succeeded','failed','aborted','partial','unknown')),
    `actor`          TEXT,
    `started_at`     INTEGER NOT NULL,
    `ended_at`       INTEGER NOT NULL,
    `anchor_commit`  TEXT,
    `change_id`      TEXT,
    `title`          TEXT NOT NULL,
    `body`           TEXT NOT NULL,
    `content_digest` TEXT NOT NULL,
    `producer`       TEXT NOT NULL DEFAULT 'derived-v1',
    `rules_version`  INTEGER NOT NULL,
    UNIQUE(`repo_id`, `source_kind`, `source_key`)
);

CREATE TABLE IF NOT EXISTS `memory_episode_evidence` (
    `episode_id`         TEXT NOT NULL,
    `ordinal`            INTEGER NOT NULL,
    `kind`               TEXT NOT NULL CHECK (`kind` IN
                            ('commit','checkpoint','review_run','operation')),
    `ref_id`             TEXT NOT NULL,
    `link_confidence`    TEXT NOT NULL CHECK (`link_confidence` IN
                            ('identity','operation','temporal')),
    `resolution_status`  TEXT NOT NULL DEFAULT 'resolved' CHECK (`resolution_status` IN
                            ('resolved','unresolved')),
    PRIMARY KEY (`episode_id`, `ordinal`),
    FOREIGN KEY (`episode_id`) REFERENCES `memory_episode`(`episode_id`) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS `memory_projection_state` (
    `repo_id`            TEXT NOT NULL,
    `source_kind`        TEXT NOT NULL CHECK (`source_kind` IN
                            ('commit','agent_session','agent_run','bridge_operation')),
    `cursor_json`        TEXT NOT NULL,
    `fingerprint`        TEXT NOT NULL,
    `rules_version`      INTEGER NOT NULL,
    `schema_version`     INTEGER NOT NULL,
    `horizon_truncated`  INTEGER NOT NULL CHECK (`horizon_truncated` IN (0,1)),
    `revoked_count`      INTEGER NOT NULL DEFAULT 0,
    `aged_out_count`     INTEGER NOT NULL DEFAULT 0,
    `rebuilt_at`         INTEGER NOT NULL,
    PRIMARY KEY (`repo_id`, `source_kind`)
);
