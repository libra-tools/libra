-- plan-20260714 Part C §C.7 / W3-s1b: worktree lifecycle + intent journal.
--
-- `worktree_lifecycle` mirrors the registry's non-active entry states into
-- SQL so the down-migration guard (and doctor) can see them:
--   * detached_from_registry — `worktree remove` (keep-dir) unregistered the
--     directory but its scoped DB rows are KEPT; mutations in that directory
--     fail closed until re-add/repair or `--delete-dir` completes.
--   * tombstone — `worktree remove --delete-dir` deleted the directory but
--     the scoped-row cleanup failed; `worktree repair` retries it.
--
-- `worktree_intent_journal` is the durable intent record for registry
-- mutators (add/move/remove/prune): the intent row is written BEFORE any
-- filesystem/registry mutation and resolved (deleted) after publication, so
-- a crash leaves a pending row that `worktree repair` can roll forward or
-- back. SQLite cannot join a filesystem rename into one transaction — this
-- is a recoverable state machine, not cross-medium ACID.
--
-- Both tables also make the v2 down-migration guard concrete: rolling back
-- below this migration is refused while ANY lifecycle or journal row exists
-- (see the _down file) — v2 lifecycle must never be folded into a v1 active
-- entry or silently dropped.
CREATE TABLE IF NOT EXISTS `worktree_lifecycle` (
    `worktree_id` TEXT NOT NULL PRIMARY KEY,
    `state` TEXT NOT NULL CHECK (`state` IN ('detached_from_registry', 'tombstone')),
    `path` TEXT NOT NULL,
    `reason` TEXT,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS `worktree_intent_journal` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `op` TEXT NOT NULL CHECK (`op` IN ('add', 'move', 'remove', 'prune')),
    `worktree_id` TEXT,
    `payload` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL
);
