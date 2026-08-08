-- Rollback of 2026072304_sparse_view_worktree_scope.
--
-- Restores the repository-global shape and the `config_kv` toggle. The old
-- schema cannot represent a linked worktree's patterns or enabled state, so
-- this rollback FAILS CLOSED while any linked-scope row exists (§C.4.1.1:
-- down migrations must not silently discard linked state — `sparse-view
-- clear` from the owning worktree first). The guards below violate their
-- CHECK constraints when linked rows are present.

CREATE TABLE `sparse_view__down_guard_2026072304` (
    `linked_rows` INTEGER NOT NULL CHECK (`linked_rows` = 0)
);
INSERT INTO `sparse_view__down_guard_2026072304` (`linked_rows`)
SELECT COUNT(*) FROM `sparse_view` WHERE `worktree_id` <> '';
INSERT INTO `sparse_view__down_guard_2026072304` (`linked_rows`)
SELECT COUNT(*) FROM `sparse_view_meta` WHERE `worktree_id` <> '';
DROP TABLE `sparse_view__down_guard_2026072304`;

ALTER TABLE `sparse_view` RENAME TO `sparse_view__new_2026072304`;
CREATE TABLE `sparse_view` (
    `id`      INTEGER PRIMARY KEY AUTOINCREMENT,
    `pattern` TEXT NOT NULL,
    `ordinal` INTEGER NOT NULL
);
INSERT INTO `sparse_view` (`id`, `pattern`, `ordinal`)
SELECT `id`, `pattern`, `ordinal`
FROM `sparse_view__new_2026072304`
WHERE `worktree_id` = '';
DROP TABLE `sparse_view__new_2026072304`;

-- Re-project the main scope's enabled state back into `config_kv` (only
-- when a meta row existed — absence keeps the disabled default implicit).
CREATE TABLE IF NOT EXISTS `config_kv` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL,
    `encrypted` INTEGER NOT NULL DEFAULT 0
);
INSERT INTO `config_kv` (`key`, `value`, `encrypted`)
SELECT 'sparse.enabled',
       CASE WHEN `enabled` <> 0 THEN 'true' ELSE 'false' END,
       0
FROM `sparse_view_meta`
WHERE `worktree_id` = '';
DROP TABLE `sparse_view_meta`;
