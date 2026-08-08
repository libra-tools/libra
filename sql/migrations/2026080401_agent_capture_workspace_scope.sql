-- 2026080401_agent_capture_workspace_scope
--
-- plan-20260714 Part C W4: capture/export/import ownership must be bound to
-- the repository and the worktree/workspace that observed it.  Existing rows
-- cannot be attributed safely: a shared provider session was historically
-- keyed only by provider id, so this migration marks them legacy_unknown and
-- requires an explicit doctor adoption rather than guessing MAIN.

ALTER TABLE `agent_session` ADD COLUMN `repo_id` TEXT;
ALTER TABLE `agent_session` ADD COLUMN `workspace_id` TEXT;
ALTER TABLE `agent_session` ADD COLUMN `workspace_fence` INTEGER;
ALTER TABLE `agent_session` ADD COLUMN `scope_state` TEXT NOT NULL
    DEFAULT 'legacy_unknown'
    CHECK(`scope_state` IN ('legacy_unknown', 'scoped'));

ALTER TABLE `agent_export_job` ADD COLUMN `repo_id` TEXT;
ALTER TABLE `agent_export_job` ADD COLUMN `worktree_id` TEXT;
ALTER TABLE `agent_export_job` ADD COLUMN `workspace_id` TEXT;
ALTER TABLE `agent_export_job` ADD COLUMN `workspace_fence` INTEGER;
ALTER TABLE `agent_export_job` ADD COLUMN `scope_state` TEXT NOT NULL
    DEFAULT 'legacy_unknown'
    CHECK(`scope_state` IN ('legacy_unknown', 'scoped'));

ALTER TABLE `agent_import_identity` ADD COLUMN `repo_id` TEXT;
ALTER TABLE `agent_import_identity` ADD COLUMN `worktree_id` TEXT;
ALTER TABLE `agent_import_identity` ADD COLUMN `workspace_id` TEXT;
ALTER TABLE `agent_import_identity` ADD COLUMN `workspace_fence` INTEGER;
ALTER TABLE `agent_import_identity` ADD COLUMN `scope_state` TEXT NOT NULL
    DEFAULT 'legacy_unknown'
    CHECK(`scope_state` IN ('legacy_unknown', 'scoped'));

-- These indexes serve the scope verification reads.  The historical global
-- unique keys intentionally remain: a provider session observed by a second
-- scope must fail closed until an operator resolves it, never become a
-- last-writer-wins duplicate.
CREATE INDEX `idx_agent_session_workspace_scope`
    ON `agent_session`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';
CREATE INDEX `idx_agent_export_job_workspace_scope`
    ON `agent_export_job`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';
CREATE INDEX `idx_agent_import_identity_workspace_scope`
    ON `agent_import_identity`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';

-- Provider ids arrive from external adapters, so ownership validation must
-- inspect every capture table by provider id, even if its catalog session was
-- already pruned. These direct indexes keep that cross-table collision check
-- bounded on the hook/import hot paths.
CREATE INDEX `idx_agent_session_capture_provider`
    ON `agent_session`(`provider_session_id`);
CREATE INDEX `idx_agent_export_job_capture_provider`
    ON `agent_export_job`(`provider_session_id`);
CREATE INDEX `idx_agent_import_identity_capture_provider`
    ON `agent_import_identity`(`provider_session_id`);

-- A scoped row must carry a complete repository/worktree owner key.  A
-- workspace is optional for ordinary main/human linked worktrees, but its id
-- and fence are an all-or-nothing pair. Runtime writes additionally compare
-- that fence to workspace_record in the same statement.
CREATE TRIGGER `agent_session_scope_guard_insert`
BEFORE INSERT ON `agent_session`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = ''
      OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent session is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER `agent_session_scope_guard_update`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_session`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = ''
      OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent session is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER `agent_export_job_scope_guard_insert`
BEFORE INSERT ON `agent_export_job`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER `agent_export_job_scope_guard_update`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_export_job`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER `agent_import_identity_scope_guard_insert`
BEFORE INSERT ON `agent_import_identity`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER `agent_import_identity_scope_guard_update`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_import_identity`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity is missing repository, worktree, or workspace fence');
END;

-- Adoption is a one-way, explicit transition from legacy_unknown to scoped.
-- Once a row is scoped, changing any part of its owner key would invalidate
-- the identity/fence used by in-flight capture workers, so reject it at the
-- database boundary as well as in the service layer.
CREATE TRIGGER `agent_session_scope_immutable`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_session`
WHEN OLD.`scope_state` = 'scoped'
 AND (NEW.`scope_state` <> OLD.`scope_state`
      OR NEW.`repo_id` IS NOT OLD.`repo_id`
      OR NEW.`worktree_id` IS NOT OLD.`worktree_id`
      OR NEW.`workspace_id` IS NOT OLD.`workspace_id`
      OR NEW.`workspace_fence` IS NOT OLD.`workspace_fence`)
BEGIN
    SELECT RAISE(ABORT, 'scoped agent session ownership is immutable');
END;

CREATE TRIGGER `agent_export_job_scope_immutable`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_export_job`
WHEN OLD.`scope_state` = 'scoped'
 AND (NEW.`scope_state` <> OLD.`scope_state`
      OR NEW.`repo_id` IS NOT OLD.`repo_id`
      OR NEW.`worktree_id` IS NOT OLD.`worktree_id`
      OR NEW.`workspace_id` IS NOT OLD.`workspace_id`
      OR NEW.`workspace_fence` IS NOT OLD.`workspace_fence`)
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job ownership is immutable');
END;

CREATE TRIGGER `agent_import_identity_scope_immutable`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_import_identity`
WHEN OLD.`scope_state` = 'scoped'
 AND (NEW.`scope_state` <> OLD.`scope_state`
      OR NEW.`repo_id` IS NOT OLD.`repo_id`
      OR NEW.`worktree_id` IS NOT OLD.`worktree_id`
      OR NEW.`workspace_id` IS NOT OLD.`workspace_id`
      OR NEW.`workspace_fence` IS NOT OLD.`workspace_fence`)
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity ownership is immutable');
END;

-- Every explicit legacy adoption is recorded separately from the raw-content
-- audit log.  It contains only stable identifiers and the target scope.
CREATE TABLE `agent_workspace_scope_audit` (
    `audit_id` TEXT NOT NULL PRIMARY KEY,
    `action` TEXT NOT NULL CHECK(`action` = 'adopt_legacy_capture_scope'),
    `agent_kind` TEXT NOT NULL,
    `provider_session_id` TEXT NOT NULL,
    `repo_id` TEXT NOT NULL,
    `worktree_id` TEXT NOT NULL,
    `workspace_id` TEXT,
    `workspace_fence` INTEGER,
    `actor` TEXT,
    `created_at` INTEGER NOT NULL
);
CREATE INDEX `idx_agent_workspace_scope_audit_session`
    ON `agent_workspace_scope_audit`(`agent_kind`, `provider_session_id`, `created_at`);

-- The audit log records the operator decision that made a previously
-- ambiguous owner claim usable again.  It must not be alterable or erasable
-- by a later command, otherwise a scoped row could no longer be explained.
CREATE TRIGGER `agent_workspace_scope_audit_append_only_update`
BEFORE UPDATE ON `agent_workspace_scope_audit`
BEGIN
    SELECT RAISE(ABORT, 'agent workspace scope audit is append-only');
END;

CREATE TRIGGER `agent_workspace_scope_audit_append_only_delete`
BEFORE DELETE ON `agent_workspace_scope_audit`
BEGIN
    SELECT RAISE(ABORT, 'agent workspace scope audit is append-only');
END;
