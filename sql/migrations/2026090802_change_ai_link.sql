-- CH-04: associate AI operations with a stable Change ID.
--
-- The operation-v2 migration is immutable and already shipped without this
-- projection column. This additive migration is deliberately forward-only;
-- the migration runner claims the version before executing the DDL, so the
-- SQLite ALTER TABLE is applied exactly once per database.
ALTER TABLE `ai_operation_link` ADD COLUMN `change_id` TEXT;

-- CH-02: preserve repository-scoped Change ID prefix resolution after the
-- independently shipped 0801 convergence migration claimed that version.
CREATE INDEX IF NOT EXISTS `idx_change_identity_v2_repo_change`
    ON `change_identity`(`repo_id`, `change_id`);

CREATE INDEX IF NOT EXISTS `idx_ai_operation_link_repo_change`
    ON `ai_operation_link`(`repo_id`, `change_id`);

CREATE INDEX IF NOT EXISTS `idx_ai_operation_link_repo_intent`
    ON `ai_operation_link`(`repo_id`, `intent_id`);
