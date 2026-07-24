-- Rollback of 2026072403_worktree_migrate_intent.
--
-- REFUSES while any 'migrate' intent exists (the narrower CHECK could not
-- represent it — finish or recover the layout migration first via
-- `libra worktree repair`), then restores the pre-2026072403 shape.
CREATE TABLE IF NOT EXISTS `worktree_migrate_intent_down_guard` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);
INSERT INTO `worktree_migrate_intent_down_guard` (`blocked`)
SELECT COUNT(*) FROM `worktree_intent_journal` WHERE `op` = 'migrate';
DROP TABLE `worktree_migrate_intent_down_guard`;
ALTER TABLE `worktree_intent_journal` RENAME TO `worktree_intent_journal__down_2026072403`;
CREATE TABLE `worktree_intent_journal` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `op` TEXT NOT NULL CHECK (`op` IN ('add', 'move', 'remove', 'prune')),
    `worktree_id` TEXT,
    `payload` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL
);
INSERT INTO `worktree_intent_journal` (`id`, `op`, `worktree_id`, `payload`, `created_at`)
SELECT `id`, `op`, `worktree_id`, `payload`, `created_at`
FROM `worktree_intent_journal__down_2026072403`;
DROP TABLE `worktree_intent_journal__down_2026072403`;
