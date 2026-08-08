-- Rollback of 2026073003_operation_boundary_claim.
--
-- Fail-closed while a control claim is still open: rolling the index away
-- would let a second process take the same worktree's control slot, and
-- rolling the columns away would make a non-restorable operation look
-- restorable to the older binary that comes next. Either is a correctness
-- regression, so refuse and say what to clear.
CREATE TABLE `operation__down_guard_2026073003` (
    `guard` TEXT NOT NULL PRIMARY KEY
        CHECK (`guard` = 'no running control claim')
);
INSERT INTO `operation__down_guard_2026073003` (`guard`)
SELECT 'no running control claim'
UNION ALL
SELECT 'a running control claim still exists; let it finish, or resolve it '
       || 'with the owning command, before rolling this migration back'
FROM `operation`
WHERE `status` = 'running' AND `control_slot` IS NOT NULL
LIMIT 1;
DROP TABLE `operation__down_guard_2026073003`;

DROP INDEX IF EXISTS `idx_operation_control_slot`;
ALTER TABLE `operation` DROP COLUMN `claim_owner`;
ALTER TABLE `operation` DROP COLUMN `control_slot`;
ALTER TABLE `operation` DROP COLUMN `restorable`;
