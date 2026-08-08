-- Rollback of 2026073004_operation_scope_kind.
--
-- Fail closed on any `repository`-scope row: the older binary has no way to
-- express that kind, so dropping the column would present a repository-wide
-- operation as a main-worktree one — and `op restore` would then offer to
-- replay it into a single worktree, which is the exact hazard the column was
-- added to prevent.
CREATE TABLE `operation__down_guard_2026073004` (
    `guard` TEXT NOT NULL PRIMARY KEY
        CHECK (`guard` = 'no repository-scope operation')
);
INSERT INTO `operation__down_guard_2026073004` (`guard`)
SELECT 'no repository-scope operation'
UNION ALL
SELECT 'a repository-scope operation exists; an older binary cannot represent '
       || 'its scope, so this migration cannot be rolled back'
FROM `operation`
WHERE `scope_kind` = 'repository'
LIMIT 1;
DROP TABLE `operation__down_guard_2026073004`;

DROP TRIGGER IF EXISTS `operation_scope_kind_domain_update`;
DROP TRIGGER IF EXISTS `operation_scope_kind_domain_insert`;
ALTER TABLE `operation` DROP COLUMN `scope_kind`;
