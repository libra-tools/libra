-- plan-20260714 Part C W0 (§C.11 "HEAD schema hardening"): at most ONE local
-- HEAD row per worktree scope, enforced by the database.
--
-- 2026070801 added `reference.worktree_id` and a deliberately NON-unique
-- lookup index, leaving the single-HEAD-per-scope invariant to the update
-- path's find-then-update. Find-then-update is not an invariant: two
-- concurrent detached-HEAD updates in the same scope can both miss and both
-- insert, and `Head::query_local_head_result_with_conn` then resolves the
-- duplicate with `.one()`, which returns an ARBITRARY row. The worktree
-- silently reads whichever HEAD SQLite happened to hand back, and the next
-- ref write moves that one.
--
-- The partial unique index below makes the second insert fail instead.
--
-- PRE-EXISTING DUPLICATES FAIL THE MIGRATION CLOSED (ADR-0714-08: an
-- ambiguous scope is never resolved by guessing). A repository that already
-- holds two HEAD rows for one scope has a real ownership question that only
-- the operator can answer, and picking one here would destroy the evidence
-- needed to answer it. The guard table below aborts the migration with the
-- duplicate count; recover by deleting the duplicate rows BY HAND with a
-- SQLite client — every migration-applying path (including
-- `libra worktree repair --confirm`) hits this same guard before it can run,
-- and an unconfirmed repair refuses outright, so neither can be the recovery
-- route. `libra worktree doctor` IS safe to inspect with first: it opens the
-- database without applying migrations. Then, for a HEAD row whose worktree
-- no longer exists:
--   DELETE FROM `reference`
--    WHERE `kind` = 'Head' AND `remote` IS NULL
--      AND `worktree_id` = '<the-orphaned-worktree-id>';
-- For the main scope (spelled `worktree_id IS NULL`), keep exactly one row:
--   DELETE FROM `reference`
--    WHERE `kind` = 'Head' AND `remote` IS NULL AND `worktree_id` IS NULL
--      AND `rowid` NOT IN (SELECT MIN(`rowid`) FROM `reference`
--                           WHERE `kind` = 'Head' AND `remote` IS NULL
--                             AND `worktree_id` IS NULL);
-- Re-run any migration-applying command afterwards to resume the upgrade.
CREATE TABLE IF NOT EXISTS `head_scope_unique_guard` (
    `duplicates` INTEGER NOT NULL CHECK (`duplicates` = 0)
);
INSERT INTO `head_scope_unique_guard` (`duplicates`)
SELECT COUNT(*) FROM (
    SELECT `worktree_id`
    FROM `reference`
    WHERE `kind` = 'Head' AND `remote` IS NULL
    GROUP BY `worktree_id`
    HAVING COUNT(*) > 1
);
DROP TABLE `head_scope_unique_guard`;

-- SQLite treats every NULL as distinct in a UNIQUE index, so the main
-- worktree (spelled `worktree_id IS NULL`, per the `reference` table's
-- convention) needs its own single-row index rather than relying on the
-- combined one.
CREATE UNIQUE INDEX IF NOT EXISTS `idx_reference_head_scope_unique`
    ON `reference` (`worktree_id`)
    WHERE `kind` = 'Head' AND `remote` IS NULL AND `worktree_id` IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS `idx_reference_head_main_unique`
    ON `reference` ((1))
    WHERE `kind` = 'Head' AND `remote` IS NULL AND `worktree_id` IS NULL;
