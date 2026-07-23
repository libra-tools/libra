-- 2026072303_layer_worktree_scope
--
-- plan-20260714 Part C W1 (§C.4.1.1): scope the layer overlay registry per
-- worktree. `layer` gains `worktree_id` with UNIQUE(worktree_id, name) and
-- `layer_path` gains `worktree_id` with UNIQUE(worktree_id, path) — the same
-- layer name and the same materialized destination may exist independently
-- in different worktrees.
--
-- Legacy-row rule (§C.4.1.1): layer registration/ownership is NOT rebuildable
-- advisory state — clearing it would make materialized overlay files
-- committable (breaking never-enters-commit), and guessing an owner is
-- forbidden. Legacy global rows are adopted to the MAIN scope ('') ONLY when
-- the guard below proves no linked worktree exists (no linked HEAD row in
-- `reference`): layer mutations have been main-only since the W0 transition
-- guard, so absent linked worktrees the rows can only belong to main. If
-- linked worktrees coexist with legacy rows, this migration FAILS CLOSED —
-- run `libra layer unapply` / `libra layer remove <name>` from the owning
-- worktree on the previous binary (or main), then upgrade.

-- Defensive: guarantee the legacy shapes exist so the rebuild below is
-- well-defined even on a database that somehow skipped 2026070501.
CREATE TABLE IF NOT EXISTS `layer` (
    `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
    `name`       TEXT NOT NULL UNIQUE,
    `source`     TEXT NOT NULL,
    `priority`   INTEGER NOT NULL DEFAULT 0,
    `enabled`    INTEGER NOT NULL DEFAULT 1,
    `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS `layer_path` (
    `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
    `layer_name`      TEXT NOT NULL,
    `path`            TEXT NOT NULL UNIQUE,
    `content_hash`    TEXT NOT NULL,
    `materialized_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Fail-closed guard: legacy global layer rows + linked worktree evidence
-- (a linked HEAD row) is ambiguous ownership — refuse the whole migration
-- (CHECK violation rolls back this transaction, including the version claim).
CREATE TABLE `layer__legacy_rows_need_explicit_adopt_2026072303` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);
INSERT INTO `layer__legacy_rows_need_explicit_adopt_2026072303` (`blocked`)
SELECT CASE
    WHEN ((SELECT COUNT(*) FROM `layer`) + (SELECT COUNT(*) FROM `layer_path`)) > 0
     AND EXISTS (SELECT 1 FROM `reference`
                 WHERE `worktree_id` IS NOT NULL AND `worktree_id` <> '')
    THEN 1 ELSE 0 END;
DROP TABLE `layer__legacy_rows_need_explicit_adopt_2026072303`;

ALTER TABLE `layer` RENAME TO `layer__old_2026072303`;
CREATE TABLE `layer` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id` TEXT NOT NULL DEFAULT '',
    `name`        TEXT NOT NULL,
    `source`      TEXT NOT NULL,
    `priority`    INTEGER NOT NULL DEFAULT 0,
    `enabled`     INTEGER NOT NULL DEFAULT 1,
    `created_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(`worktree_id`, `name`)
);
INSERT INTO `layer`
    (`id`, `worktree_id`, `name`, `source`, `priority`, `enabled`,
     `created_at`, `updated_at`)
SELECT `id`, '', `name`, `source`, `priority`, `enabled`,
       `created_at`, `updated_at`
FROM `layer__old_2026072303`;
DROP TABLE `layer__old_2026072303`;

ALTER TABLE `layer_path` RENAME TO `layer_path__old_2026072303`;
CREATE TABLE `layer_path` (
    `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id`     TEXT NOT NULL DEFAULT '',
    `layer_name`      TEXT NOT NULL,
    `path`            TEXT NOT NULL,
    `content_hash`    TEXT NOT NULL,
    `materialized_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(`worktree_id`, `path`)
);
INSERT INTO `layer_path`
    (`id`, `worktree_id`, `layer_name`, `path`, `content_hash`,
     `materialized_at`)
SELECT `id`, '', `layer_name`, `path`, `content_hash`, `materialized_at`
FROM `layer_path__old_2026072303`;
DROP TABLE `layer_path__old_2026072303`;
