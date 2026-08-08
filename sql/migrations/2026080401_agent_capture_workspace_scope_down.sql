-- Rollback of 2026080401_agent_capture_workspace_scope.
--
-- Scoped rows encode owner information that an older binary cannot preserve.
-- Refuse instead of dropping the scope columns or audit trail.  With no
-- scoped rows/audit events left, all remaining rows are legacy_unknown and
-- were already unsupported before this migration, so their additive columns
-- can be removed safely.
CREATE TABLE `agent_capture_scope_down_guard` (`blocked` INTEGER NOT NULL CHECK(`blocked` = 0));
INSERT INTO `agent_capture_scope_down_guard` (`blocked`)
SELECT
    (SELECT COUNT(*) FROM `agent_session` WHERE `scope_state` = 'scoped')
  + (SELECT COUNT(*) FROM `agent_export_job` WHERE `scope_state` = 'scoped')
  + (SELECT COUNT(*) FROM `agent_import_identity` WHERE `scope_state` = 'scoped')
  + (SELECT COUNT(*) FROM `agent_workspace_scope_audit`);
DROP TABLE `agent_capture_scope_down_guard`;

DROP INDEX `idx_agent_workspace_scope_audit_session`;
DROP TRIGGER `agent_workspace_scope_audit_append_only_update`;
DROP TRIGGER `agent_workspace_scope_audit_append_only_delete`;
DROP TABLE `agent_workspace_scope_audit`;

DROP TRIGGER `agent_session_scope_guard_insert`;
DROP TRIGGER `agent_session_scope_guard_update`;
DROP TRIGGER `agent_export_job_scope_guard_insert`;
DROP TRIGGER `agent_export_job_scope_guard_update`;
DROP TRIGGER `agent_import_identity_scope_guard_insert`;
DROP TRIGGER `agent_import_identity_scope_guard_update`;
DROP TRIGGER `agent_session_scope_immutable`;
DROP TRIGGER `agent_export_job_scope_immutable`;
DROP TRIGGER `agent_import_identity_scope_immutable`;

DROP INDEX `idx_agent_session_workspace_scope`;
DROP INDEX `idx_agent_export_job_workspace_scope`;
DROP INDEX `idx_agent_import_identity_workspace_scope`;
DROP INDEX `idx_agent_session_capture_provider`;
DROP INDEX `idx_agent_export_job_capture_provider`;
DROP INDEX `idx_agent_import_identity_capture_provider`;

ALTER TABLE `agent_session` DROP COLUMN `scope_state`;
ALTER TABLE `agent_session` DROP COLUMN `workspace_fence`;
ALTER TABLE `agent_session` DROP COLUMN `workspace_id`;
ALTER TABLE `agent_session` DROP COLUMN `repo_id`;

ALTER TABLE `agent_export_job` DROP COLUMN `scope_state`;
ALTER TABLE `agent_export_job` DROP COLUMN `workspace_fence`;
ALTER TABLE `agent_export_job` DROP COLUMN `workspace_id`;
ALTER TABLE `agent_export_job` DROP COLUMN `worktree_id`;
ALTER TABLE `agent_export_job` DROP COLUMN `repo_id`;

ALTER TABLE `agent_import_identity` DROP COLUMN `scope_state`;
ALTER TABLE `agent_import_identity` DROP COLUMN `workspace_fence`;
ALTER TABLE `agent_import_identity` DROP COLUMN `workspace_id`;
ALTER TABLE `agent_import_identity` DROP COLUMN `worktree_id`;
ALTER TABLE `agent_import_identity` DROP COLUMN `repo_id`;
