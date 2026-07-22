-- 2026072201_operation_worktree_scope
--
-- plan-20260714 Part C W1 (§C.9): record which WORKTREE an operation ran in,
-- so the operation wrapper's duplicate-submission window can be scoped
-- per-worktree. The same command with identical arguments run concurrently
-- in two worktrees is two legitimate operations, not a duplicate submission —
-- without this column the 5s dedup window (and the in-process active-key
-- set) wrongly rejects the second worktree's run.
--
-- Storage convention matches the sequencer family: main worktree = the empty
-- string, a linked worktree = its stable instance id.
--
-- Additive only: the fresh-repo bootstrap DDL (`OPERATION_SCHEMA_SQL` in
-- `src/internal/db.rs`) deliberately keeps the pre-scope shape so this ALTER
-- works on both fresh and existing databases (SQLite has no
-- `ADD COLUMN IF NOT EXISTS`; the runner's version tracking ensures this runs
-- exactly once per database). The CREATE below self-provisions bare test
-- databases that run the migration runner without the bootstrap DDL.

CREATE TABLE IF NOT EXISTS `operation` (
    `op_id` TEXT PRIMARY KEY,
    `repo_id` TEXT NOT NULL,
    `view_id` TEXT NOT NULL,
    `command_name` TEXT NOT NULL,
    `description` TEXT NOT NULL,
    `actor` TEXT NOT NULL,
    `args_digest` TEXT,
    `start_ts` INTEGER NOT NULL,
    `end_ts` INTEGER,
    `status` TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_operation_repo_order
    ON `operation`(`repo_id`, `end_ts` DESC, `start_ts` DESC, `op_id` DESC);

ALTER TABLE `operation` ADD COLUMN `worktree_id` TEXT NOT NULL DEFAULT '';
