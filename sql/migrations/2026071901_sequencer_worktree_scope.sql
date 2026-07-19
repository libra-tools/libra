-- 2026071901_sequencer_worktree_scope
--
-- plan-20260714 Part C W1 (§C.4.2): make the sequencer state stores
-- per-worktree.
--
-- `sequence_state` was declared `id INTEGER PRIMARY KEY CHECK (id = 1)` — one
-- active multi-step sequence per REPOSITORY. With linked worktrees that is
-- wrong: a cherry-pick running in one worktree occupies the single row, so a
-- second worktree's sequence would overwrite it (`save` does DELETE+INSERT),
-- silently destroying the first worktree's todo list and stopping point.
-- `rebase_state` has the same repository-global shape.
--
-- The new key is `worktree_id TEXT NOT NULL`, where the MAIN worktree is the
-- EMPTY STRING — deliberately not NULL. SQLite treats every NULL as distinct,
-- so a nullable column cannot express "at most one row per scope" through a
-- unique key; the empty-string sentinel can. (The `reference`/HEAD table uses
-- the opposite convention — main is NULL there — which is exactly why
-- `WorktreeScope` exposes both `storage_key()` and `worktree_id()`.)
--
-- SQLite cannot drop a CHECK constraint in place, so both tables are rebuilt
-- with the rename/recreate/copy/drop pattern used by
-- `2026050501_agent_checkpoint_parent_nullable.sql`. Existing rows belong to
-- the main worktree (they predate any per-worktree state), so they migrate to
-- `worktree_id = ''` and an in-progress cherry-pick/rebase survives the
-- upgrade. The whole migration runs inside the runner's transaction, so a
-- crash mid-rebuild rolls back rather than losing the sequence.

-- ── sequence_state ──────────────────────────────────────────────────────────
ALTER TABLE `sequence_state` RENAME TO `sequence_state__old_2026071901`;

CREATE TABLE `sequence_state` (
    `worktree_id` TEXT NOT NULL PRIMARY KEY,
    `kind`        TEXT NOT NULL,
    `head_name`   TEXT NOT NULL,
    `head_orig`   TEXT NOT NULL,
    `current_oid` TEXT NOT NULL,
    `todo`        TEXT NOT NULL,
    `payload`     TEXT NOT NULL DEFAULT '',
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Any pre-existing row is the main worktree's active sequence.
INSERT INTO `sequence_state`
    (`worktree_id`, `kind`, `head_name`, `head_orig`, `current_oid`, `todo`, `payload`, `updated_at`)
SELECT '', `kind`, `head_name`, `head_orig`, `current_oid`, `todo`, `payload`, `updated_at`
FROM `sequence_state__old_2026071901`;

DROP TABLE `sequence_state__old_2026071901`;

-- ── rebase_state: deliberately NOT migrated here ────────────────────────────
--
-- `rebase_state`'s live shape is defined by LAZY DDL in `src/command/rebase.rs`
-- (`CREATE TABLE IF NOT EXISTS` plus `ensure_rebase_state_columns`, which adds
-- `autosquash`, `todo_actions`, and `empty_mode` on demand). Its column set
-- therefore varies with the code version that last touched a given database, so
-- a static rebuild here would silently DROP whichever of those columns this
-- migration did not know about — destroying an in-progress rebase.
--
-- Retiring that lazy DDL is its own W1 step (§C.11 "清除 lazy DDL"); until then
-- `rebase` remains refused in a linked worktree by `ensure_main_worktree`, so
-- the repository-global `rebase_state` has no concurrent writer and no
-- cross-worktree hazard. Same for the lazily-created `bisect_state`.
