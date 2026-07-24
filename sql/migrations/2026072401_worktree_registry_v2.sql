-- 2026072401_worktree_registry_v2
--
-- plan-20260714 Part C W3 (§C.7): repository capability marker for the
-- versioned worktree registry (worktrees.json schema_version 2 with
-- persisted stable worktree ids). The marker's presence in
-- `schema_versions` makes every OLDER binary refuse the repository at
-- connect time (future-schema fail-closed) BEFORE it could parse — or,
-- worse, recreate — the registry file; the v2 JSON layout additionally
-- renames the top-level key so a v1 parser errors instead of silently
-- reading stale data.
CREATE TABLE IF NOT EXISTS `worktree_registry_capability` (
    `version` INTEGER PRIMARY KEY
);
INSERT OR IGNORE INTO `worktree_registry_capability` (`version`) VALUES (2);
