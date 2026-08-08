-- Rollback of 2026072902_operation_scope_provenance.
--
-- REFUSES while any operation is still marked `unknown`: dropping the column
-- would silently promote those rows back to "main scope, trustworthy", which
-- is the exact claim the migration exists to withhold, and `op restore` would
-- start acting on them again. Resolve them first (an explicit doctor
-- provenance repair, or delete the operations), then retry.
CREATE TABLE IF NOT EXISTS `operation_scope_provenance_down_guard` (
    `unresolved` INTEGER NOT NULL CHECK (`unresolved` = 0)
);
INSERT INTO `operation_scope_provenance_down_guard` (`unresolved`)
SELECT COUNT(*) FROM `operation` WHERE `scope_provenance` = 'unknown';
DROP TABLE `operation_scope_provenance_down_guard`;

DROP TRIGGER IF EXISTS `operation_scope_provenance_domain_insert`;
DROP TRIGGER IF EXISTS `operation_scope_provenance_domain_update`;
ALTER TABLE `operation` DROP COLUMN `scope_provenance`;
