-- Rollback of 2026072301_bisect_state_worktree_scope.
--
-- Restores the pre-rekey lazy shape (AUTOINCREMENT id, `worktree_id` kept as
-- a plain column — the older binary's lazy DDL expects it). The old shape's
-- behavioral single-row contract cannot safely represent a linked worktree's
-- bisect under an old binary, so this rollback FAILS CLOSED while any
-- linked-scope row exists (plan-20260714 §C.4.2: the down migration must not
-- silently discard linked state — finish or `bisect reset` linked-worktree
-- sessions first). The guard below violates its CHECK constraint when a
-- linked row is present, aborting the runner's transaction.

CREATE TABLE `bisect_state__down_guard_2026072301` (
    `linked_rows` INTEGER NOT NULL CHECK (`linked_rows` = 0)
);

INSERT INTO `bisect_state__down_guard_2026072301` (`linked_rows`)
SELECT COUNT(*) FROM `bisect_state` WHERE `worktree_id` <> '';

DROP TABLE `bisect_state__down_guard_2026072301`;

-- ── rebuild the legacy lazy shape ───────────────────────────────────────────
ALTER TABLE `bisect_state` RENAME TO `bisect_state__new_2026072301`;

CREATE TABLE `bisect_state` (
    `id`             INTEGER PRIMARY KEY AUTOINCREMENT,
    `orig_head`      TEXT NOT NULL,
    `orig_head_name` TEXT,
    `bad`            TEXT,
    `good`           TEXT NOT NULL,
    `current`        TEXT,
    `skipped`        TEXT,
    `steps`          INTEGER,
    `completed`      INTEGER NOT NULL DEFAULT 0,
    `first_parent`   INTEGER NOT NULL DEFAULT 0,
    `worktree_id`    TEXT NOT NULL DEFAULT ''
);

INSERT INTO `bisect_state`
    (`orig_head`, `orig_head_name`, `bad`, `good`, `current`, `skipped`,
     `steps`, `completed`, `first_parent`, `worktree_id`)
SELECT `orig_head`, `orig_head_name`, `bad`, `good`, `current`, `skipped`,
       `steps`, `completed`, `first_parent`, `worktree_id`
FROM `bisect_state__new_2026072301`;

DROP TABLE `bisect_state__new_2026072301`;
