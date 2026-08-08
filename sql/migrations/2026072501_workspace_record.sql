-- plan-20260714 Part C §C.8 / W4-s1: the unified workspace association +
-- lease record. Linked worktrees, task copies/FUSE mounts, and future
-- remote workspaces all become queryable through one table.
--
-- Constraints (§C.8):
--   * A LINKED workspace's lease identity is (repo_id, worktree_id): the
--     partial unique index below guarantees at most one row in a live
--     state per pair.
--   * The canonical path is unique across every ACTIVE writable workspace
--     (second partial index) so a path alias can never be double-claimed.
--   * Acquire/renew/release are owner+fence conditional writes performed
--     by the service layer in one transaction; stale owners cannot release
--     a newer owner's lease.
CREATE TABLE IF NOT EXISTS `workspace_record` (
    `workspace_id` TEXT NOT NULL PRIMARY KEY,
    `repo_id` TEXT NOT NULL,
    `kind` TEXT NOT NULL CHECK (`kind` IN ('linked', 'task_copy', 'task_fuse', 'remote')),
    `worktree_id` TEXT,
    `path` TEXT NOT NULL,
    `owner_kind` TEXT NOT NULL CHECK (`owner_kind` IN ('human', 'agent', 'automation')),
    `owner_id` TEXT,
    `task_id` TEXT,
    `session_id` TEXT,
    `base_commit` TEXT,
    `branch` TEXT,
    `state` TEXT NOT NULL CHECK (
        `state` IN ('provisioning', 'active', 'releasing', 'released', 'orphaned')
    ),
    `lease_owner` TEXT,
    `lease_fence` INTEGER NOT NULL DEFAULT 0,
    `lease_expires_at` INTEGER,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_workspace_linked_live`
ON `workspace_record` (`repo_id`, `worktree_id`)
WHERE `kind` = 'linked' AND `state` IN ('provisioning', 'active', 'releasing');

CREATE UNIQUE INDEX IF NOT EXISTS `idx_workspace_active_path`
ON `workspace_record` (`repo_id`, `path`)
WHERE `state` IN ('provisioning', 'active', 'releasing');

CREATE INDEX IF NOT EXISTS `idx_workspace_state`
ON `workspace_record` (`repo_id`, `state`);
