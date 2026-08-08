-- plan-20260714 Part C §C.6 / W3-s3: allow the layout-migration op in the
-- worktree intent journal. SQLite cannot ALTER a CHECK constraint, so the
-- table is rebuilt via RENAME (safe: the runner claims the version row
-- before executing, guaranteeing single application). Rows are carried
-- over verbatim; `stage` records the C.6 state machine position for
-- 'migrate' intents (NULL for the others).
ALTER TABLE `worktree_intent_journal` RENAME TO `worktree_intent_journal__old_2026072403`;
CREATE TABLE `worktree_intent_journal` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `op` TEXT NOT NULL CHECK (`op` IN ('add', 'move', 'remove', 'prune', 'migrate')),
    `worktree_id` TEXT,
    `payload` TEXT NOT NULL,
    `stage` TEXT,
    `created_at` INTEGER NOT NULL
);
INSERT INTO `worktree_intent_journal` (`id`, `op`, `worktree_id`, `payload`, `created_at`)
SELECT `id`, `op`, `worktree_id`, `payload`, `created_at`
FROM `worktree_intent_journal__old_2026072403`;
DROP TABLE `worktree_intent_journal__old_2026072403`;
