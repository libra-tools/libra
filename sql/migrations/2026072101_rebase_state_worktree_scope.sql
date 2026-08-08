-- 2026072101_rebase_state_worktree_scope
--
-- plan-20260714 Part C W1 (§C.4.2): make the rebase state store per-worktree,
-- completing the sequencer family (`sequence_state` was re-keyed by
-- 2026071901; `bisect_state` was scoped in place by lazy ADD COLUMN at the
-- time this shipped, and later re-keyed by 2026072301).
--
-- `rebase_state` was a repository-global single row (id AUTOINCREMENT +
-- behavioral DELETE-all-then-INSERT). With linked worktrees that is wrong: a
-- rebase running in one worktree occupies the row, so a second worktree's
-- rebase would overwrite it, silently destroying the first worktree's todo
-- list and stopping point. The new key is `worktree_id TEXT PRIMARY KEY NOT
-- NULL`, main worktree = the EMPTY STRING (same convention and rationale as
-- 2026071901: SQLite NULLs are all distinct, so a nullable unique key cannot
-- express "at most one row per scope").
--
-- SHAPE PRECONDITION: `rebase_state`'s historical shape was owned by lazy DDL
-- in `src/command/rebase.rs` (`autosquash`, `todo_actions`, `empty_mode` were
-- added on demand), so databases in the wild carry any subset of those
-- columns. A static INSERT..SELECT cannot reference a column that does not
-- exist, therefore `normalize_rebase_state_shape` in
-- `src/internal/db/migration.rs` runs BEFORE the migration runner on every
-- connection open and normalizes the table to the full pre-scope column set
-- (CREATE IF NOT EXISTS + duplicate-tolerant ADD COLUMNs). This migration may
-- assume all ten payload columns exist. The lazy DDL itself is deleted in the
-- same release — from now on the schema is owned here.
--
-- Existing rows belong to the main worktree (they predate any per-worktree
-- state). The behavioral contract was "at most one active row", but nothing
-- enforced it, so the newest row (highest id) wins and an in-progress rebase
-- survives the upgrade. The whole migration runs inside the runner's
-- transaction: a crash mid-rebuild rolls back rather than losing the rebase.

-- Self-provision for databases where the lazily-created table never
-- materialized (and for bare test runners that skip the normalize hook): the
-- rebuild below then starts from an empty full-shape table.
CREATE TABLE IF NOT EXISTS `rebase_state` (
    `id`           INTEGER PRIMARY KEY AUTOINCREMENT,
    `head_name`    TEXT NOT NULL,
    `onto`         TEXT NOT NULL,
    `orig_head`    TEXT NOT NULL,
    `current_head` TEXT NOT NULL,
    `todo`         TEXT NOT NULL,
    `todo_actions` TEXT NOT NULL DEFAULT '',
    `done`         TEXT NOT NULL,
    `stopped_sha`  TEXT,
    `autosquash`   INTEGER NOT NULL DEFAULT 0,
    `empty_mode`   TEXT NOT NULL DEFAULT 'keep'
);

ALTER TABLE `rebase_state` RENAME TO `rebase_state__old_2026072101`;

CREATE TABLE `rebase_state` (
    `worktree_id`  TEXT PRIMARY KEY NOT NULL,
    `head_name`    TEXT NOT NULL,
    `onto`         TEXT NOT NULL,
    `orig_head`    TEXT NOT NULL,
    `current_head` TEXT NOT NULL,
    `todo`         TEXT NOT NULL,
    `todo_actions` TEXT NOT NULL DEFAULT '',
    `done`         TEXT NOT NULL,
    `stopped_sha`  TEXT,
    `autosquash`   INTEGER NOT NULL DEFAULT 0,
    `empty_mode`   TEXT NOT NULL DEFAULT 'keep'
);

-- Any pre-existing row is the main worktree's active rebase; newest id wins.
INSERT INTO `rebase_state`
    (`worktree_id`, `head_name`, `onto`, `orig_head`, `current_head`, `todo`,
     `todo_actions`, `done`, `stopped_sha`, `autosquash`, `empty_mode`)
SELECT '', `head_name`, `onto`, `orig_head`, `current_head`, `todo`,
       `todo_actions`, `done`, `stopped_sha`, `autosquash`, `empty_mode`
FROM `rebase_state__old_2026072101`
ORDER BY `id` DESC
LIMIT 1;

DROP TABLE `rebase_state__old_2026072101`;
