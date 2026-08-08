-- Rollback of 2026072501_workspace_record.
--
-- REFUSES while any NON-TERMINAL workspace exists (provisioning / active /
-- releasing / orphaned): dropping the table would orphan live leases and
-- lose the recovery state the scavenger/doctor needs. Release or recover
-- them first (`libra worktree repair --confirm`, agent lease release), then retry.
CREATE TABLE IF NOT EXISTS `workspace_record_down_guard` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);
INSERT INTO `workspace_record_down_guard` (`blocked`)
SELECT COUNT(*) FROM `workspace_record`
WHERE `state` IN ('provisioning', 'active', 'releasing', 'orphaned');
DROP TABLE `workspace_record_down_guard`;
DROP INDEX IF EXISTS `idx_workspace_linked_live`;
DROP INDEX IF EXISTS `idx_workspace_active_path`;
DROP INDEX IF EXISTS `idx_workspace_state`;
DROP TABLE IF EXISTS `workspace_record`;
