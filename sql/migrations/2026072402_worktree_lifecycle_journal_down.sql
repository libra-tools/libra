-- Rollback of 2026072402_worktree_lifecycle_journal.
--
-- REFUSES (§C.7 plan line 1261) while ANY of the following exists:
--   * a lifecycle row (detached_from_registry / tombstone),
--   * an in-flight intent-journal row,
--   * active/nonterminal sequencer, rebase, or bisect state in a LINKED
--     scope (worktree_id set and non-empty) — v1 has no linked lifecycle,
--     so downgrading would strand those scopes' recovery state.
-- A downgrade would fold v2 lifecycle into a v1 active entry or silently
-- drop pending intents. Finish the pending work first — `libra worktree
-- repair` retries tombstone cleanup and resolves stale intents; re-add or
-- `worktree remove --delete-dir` settles detached directories; finish or
-- abort in-progress rebase/cherry-pick/bisect runs in their worktrees —
-- then retry. (Workspace leases are a W4 surface and will join this guard
-- with that slice.)
--
-- `bisect_state` is created lazily at first use, and bare test databases
-- may lack the sequencer tables entirely — create empty shells so the
-- guard SELECTs cannot fail on a missing table (matching the defensive
-- pattern of earlier migrations).
CREATE TABLE IF NOT EXISTS `sequence_state` (
    `worktree_id` TEXT
);
CREATE TABLE IF NOT EXISTS `rebase_state` (
    `worktree_id` TEXT
);
CREATE TABLE IF NOT EXISTS `bisect_state` (
    `worktree_id` TEXT
);
CREATE TABLE IF NOT EXISTS `worktree_lifecycle_down_guard` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);
INSERT INTO `worktree_lifecycle_down_guard` (`blocked`)
SELECT COUNT(*)
FROM (
    SELECT `worktree_id` AS token FROM `worktree_lifecycle`
    UNION ALL
    SELECT CAST(`id` AS TEXT) FROM `worktree_intent_journal`
    UNION ALL
    SELECT `worktree_id` FROM `sequence_state`
    WHERE `worktree_id` IS NOT NULL AND `worktree_id` <> ''
    UNION ALL
    SELECT `worktree_id` FROM `rebase_state`
    WHERE `worktree_id` IS NOT NULL AND `worktree_id` <> ''
    UNION ALL
    SELECT `worktree_id` FROM `bisect_state`
    WHERE `worktree_id` IS NOT NULL AND `worktree_id` <> ''
);
DROP TABLE `worktree_lifecycle_down_guard`;
DROP TABLE IF EXISTS `worktree_intent_journal`;
DROP TABLE IF EXISTS `worktree_lifecycle`;
