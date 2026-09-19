-- OL-15 follow-up: restore the boundary-claim columns that were lost when
-- the OL-02 v2 operation table replaced the earlier operation namespace.
--
-- The 2026073003 migration added these columns to the then-current table, but
-- 2026090101 intentionally rebuilt that table and its canonical DDL omitted
-- them. Keep the repair monotonic so both fresh databases and already-upgraded
-- databases receive the same operation boundary contract.
ALTER TABLE `operation` ADD COLUMN `restorable` INTEGER NOT NULL DEFAULT 1;
ALTER TABLE `operation` ADD COLUMN `control_slot` TEXT;
ALTER TABLE `operation` ADD COLUMN `claim_owner` TEXT;
ALTER TABLE `operation` ADD COLUMN `scope_provenance` TEXT NOT NULL DEFAULT 'declared';

CREATE TRIGGER IF NOT EXISTS `operation_scope_provenance_domain_insert`
BEFORE INSERT ON `operation`
FOR EACH ROW WHEN NEW.`scope_provenance` NOT IN ('declared', 'unknown')
BEGIN
    SELECT RAISE(
        ABORT,
        'operation.scope_provenance must be either declared or unknown'
    );
END;

CREATE TRIGGER IF NOT EXISTS `operation_scope_provenance_domain_update`
BEFORE UPDATE OF `scope_provenance` ON `operation`
FOR EACH ROW WHEN NEW.`scope_provenance` NOT IN ('declared', 'unknown')
BEGIN
    SELECT RAISE(
        ABORT,
        'operation.scope_provenance must be either declared or unknown'
    );
END;

CREATE TRIGGER IF NOT EXISTS `operation_scope_kind_domain_insert`
BEFORE INSERT ON `operation`
FOR EACH ROW WHEN NEW.`scope_kind` NOT IN ('main', 'linked', 'repository', 'unknown')
BEGIN
    SELECT RAISE(
        ABORT,
        'operation.scope_kind must be main, linked, repository or unknown'
    );
END;

CREATE TRIGGER IF NOT EXISTS `operation_scope_kind_domain_update`
BEFORE UPDATE OF `scope_kind` ON `operation`
FOR EACH ROW WHEN NEW.`scope_kind` NOT IN ('main', 'linked', 'repository', 'unknown')
BEGIN
    SELECT RAISE(
        ABORT,
        'operation.scope_kind must be main, linked, repository or unknown'
    );
END;

CREATE UNIQUE INDEX IF NOT EXISTS `idx_operation_control_slot`
    ON `operation` (`repo_id`, COALESCE(`worktree_id`, ''))
    WHERE `status` = 'running' AND `control_slot` IS NOT NULL;
