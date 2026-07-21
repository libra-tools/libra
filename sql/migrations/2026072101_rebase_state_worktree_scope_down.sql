-- Rollback of 2026072101_rebase_state_worktree_scope.
--
-- Restores the repository-global single-row shape. The old schema has no way
-- to represent a linked worktree's rebase, so this rollback FAILS CLOSED when
-- any linked-scope row exists (plan-20260714 §C.4.2: the down migration must
-- not silently discard linked state — finish or abort linked-worktree rebases
-- first). The guard below violates its CHECK constraint when a linked row is
-- present, aborting the runner's transaction.

CREATE TABLE `rebase_state__down_guard_2026072101` (
    `linked_rows` INTEGER NOT NULL CHECK (`linked_rows` = 0)
);

INSERT INTO `rebase_state__down_guard_2026072101` (`linked_rows`)
SELECT COUNT(*) FROM `rebase_state` WHERE `worktree_id` <> '';

DROP TABLE `rebase_state__down_guard_2026072101`;

-- ── rebuild the legacy shape ────────────────────────────────────────────────
ALTER TABLE `rebase_state` RENAME TO `rebase_state__new_2026072101`;

CREATE TABLE `rebase_state` (
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

INSERT INTO `rebase_state`
    (`head_name`, `onto`, `orig_head`, `current_head`, `todo`,
     `todo_actions`, `done`, `stopped_sha`, `autosquash`, `empty_mode`)
SELECT `head_name`, `onto`, `orig_head`, `current_head`, `todo`,
       `todo_actions`, `done`, `stopped_sha`, `autosquash`, `empty_mode`
FROM `rebase_state__new_2026072101`
WHERE `worktree_id` = '';

DROP TABLE `rebase_state__new_2026072101`;
