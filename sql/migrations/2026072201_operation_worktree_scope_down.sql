-- Rollback of 2026072201_operation_worktree_scope.
--
-- Rebuilds `operation` without the `worktree_id` column. Unlike the sequencer
-- STATE stores (whose down migrations fail closed on linked rows — dropping
-- one would destroy resumable crash state), the operation table is an
-- append-only audit log: every row is preserved here and only the worktree
-- attribution is lost, which the old schema cannot represent anyway.

ALTER TABLE `operation` RENAME TO `operation__new_2026072201`;

CREATE TABLE `operation` (
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

INSERT INTO `operation`
    (`op_id`, `repo_id`, `view_id`, `command_name`, `description`, `actor`,
     `args_digest`, `start_ts`, `end_ts`, `status`)
SELECT `op_id`, `repo_id`, `view_id`, `command_name`, `description`, `actor`,
       `args_digest`, `start_ts`, `end_ts`, `status`
FROM `operation__new_2026072201`;

DROP TABLE `operation__new_2026072201`;

-- The rebuild dropped the repo-order index with the old table; restore it.
CREATE INDEX IF NOT EXISTS idx_operation_repo_order
    ON `operation`(`repo_id`, `end_ts` DESC, `start_ts` DESC, `op_id` DESC);
