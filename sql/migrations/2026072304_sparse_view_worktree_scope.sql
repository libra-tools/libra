-- 2026072304_sparse_view_worktree_scope
--
-- plan-20260714 Part C W1 (§C.4.1.1): scope the read-only sparse view per
-- worktree. `sparse_view` gains `worktree_id` with UNIQUE(worktree_id,
-- ordinal) — pattern order is a per-scope fact — and the `sparse.enabled`
-- toggle moves OUT of the scope-less repo-global `config_kv` key into the
-- new `sparse_view_meta` table projection (one enabled row per worktree).
--
-- Legacy-state rule (§C.4.1.1): sparse patterns/enable state cannot be
-- owner-guessed. They are adopted to the MAIN scope ('') ONLY when the
-- guard below proves no linked worktree exists (no linked HEAD row in
-- `reference`) — sparse-view mutations have been main-only since the W0
-- transition guard. If linked worktrees coexist with legacy state (any
-- pattern row, or a truthy `sparse.enabled`), this migration FAILS CLOSED:
-- run `libra sparse-view clear` (or re-create the view in the intended
-- worktree) on the previous binary first, then upgrade. Patterns are never
-- copied to every worktree.

-- Defensive: guarantee the legacy shape exists so the rebuild below is
-- well-defined even on a database that somehow skipped 2026070701.
CREATE TABLE IF NOT EXISTS `sparse_view` (
    `id`      INTEGER PRIMARY KEY AUTOINCREMENT,
    `pattern` TEXT NOT NULL,
    `ordinal` INTEGER NOT NULL
);
-- `config_kv` is a bootstrap-SQL table (like `reference` in 2026062301):
-- migration-only (bare) test databases lack it, so ensure the shape here —
-- production databases always already have it.
CREATE TABLE IF NOT EXISTS `config_kv` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL,
    `encrypted` INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_config_kv_key ON config_kv(`key`);

-- Fail-closed guard: legacy sparse state + linked worktree evidence is
-- ambiguous ownership — refuse the whole migration (CHECK violation rolls
-- back this transaction, including the version claim).
CREATE TABLE `sparse_view__legacy_needs_explicit_adopt_2026072304` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);
-- The EFFECTIVE legacy toggle follows `ConfigKv::get` semantics: the row
-- with the highest `id` (last write) wins — a stale earlier `true` under a
-- later `false` means DISABLED, and must neither project as enabled nor
-- trip this guard.
INSERT INTO `sparse_view__legacy_needs_explicit_adopt_2026072304` (`blocked`)
SELECT CASE
    WHEN ((SELECT COUNT(*) FROM `sparse_view`) > 0
          OR TRIM(COALESCE((SELECT `value` FROM `config_kv`
                            WHERE `key` = 'sparse.enabled'
                            ORDER BY `id` DESC LIMIT 1), ''))
             IN ('true', '1', 'yes', 'on'))
     AND EXISTS (SELECT 1 FROM `reference`
                 WHERE `worktree_id` IS NOT NULL AND `worktree_id` <> '')
    THEN 1 ELSE 0 END;
DROP TABLE `sparse_view__legacy_needs_explicit_adopt_2026072304`;

ALTER TABLE `sparse_view` RENAME TO `sparse_view__old_2026072304`;
CREATE TABLE `sparse_view` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id` TEXT NOT NULL DEFAULT '',
    `pattern`     TEXT NOT NULL,
    `ordinal`     INTEGER NOT NULL,
    UNIQUE(`worktree_id`, `ordinal`)
);
INSERT INTO `sparse_view` (`id`, `worktree_id`, `pattern`, `ordinal`)
SELECT `id`, '', `pattern`, `ordinal`
FROM `sparse_view__old_2026072304`;
DROP TABLE `sparse_view__old_2026072304`;

-- Per-worktree enabled projection. The legacy repo-global config key is
-- migrated into the main scope's row and then REMOVED — `sparse.enabled`
-- in `config_kv` is retired (an old binary reading it sees the view as
-- disabled, which is the safe read-only default).
CREATE TABLE `sparse_view_meta` (
    `worktree_id` TEXT PRIMARY KEY NOT NULL,
    `enabled`     INTEGER NOT NULL DEFAULT 0
);
INSERT INTO `sparse_view_meta` (`worktree_id`, `enabled`)
SELECT '', CASE
    WHEN TRIM(COALESCE((SELECT `value` FROM `config_kv`
                        WHERE `key` = 'sparse.enabled'
                        ORDER BY `id` DESC LIMIT 1), ''))
         IN ('true', '1', 'yes', 'on')
    THEN 1 ELSE 0 END
WHERE EXISTS (SELECT 1 FROM `config_kv` WHERE `key` = 'sparse.enabled')
   OR (SELECT COUNT(*) FROM `sparse_view`) > 0;
DELETE FROM `config_kv` WHERE `key` = 'sparse.enabled';
