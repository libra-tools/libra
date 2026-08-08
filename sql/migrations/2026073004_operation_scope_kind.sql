-- 2026073004_operation_scope_kind
--
-- plan-20260714 Part C W1 (§C.9): record WHAT KIND of scope an operation ran
-- in, alongside the `worktree_id` that says which one.
--
-- `worktree_id` alone cannot express the distinction the restore gate needs.
-- A repository-scope operation (one that acts on refs shared by every
-- worktree) recorded from the main worktree stores the same empty
-- `worktree_id` as a main-scope operation, so `op restore` cannot tell them
-- apart — and replaying a repository-scope operation into one worktree is
-- exactly what §C.9 says must fail closed until LR-02.
--
-- Backfill mirrors the provenance rule from 2026072902 rather than guessing:
--
--   * `scope_provenance = 'unknown'` — the row predates scoped operation
--     records in a repository with linked-worktree evidence. Its scope kind is
--     `unknown` for the same reason its worktree is: nobody recorded it.
--   * a non-empty `worktree_id` — `linked`.
--   * everything else — `main`.
--
-- No existing row can be classified `repository`: nothing wrote that scope
-- before this column existed, so inferring it would be invention.

ALTER TABLE `operation` ADD COLUMN `scope_kind` TEXT NOT NULL DEFAULT 'unknown';

UPDATE `operation`
SET `scope_kind` = CASE
    WHEN `scope_provenance` <> 'declared' THEN 'unknown'
    WHEN `worktree_id` IS NOT NULL AND `worktree_id` <> '' THEN 'linked'
    ELSE 'main'
END;

-- Reject a value this version does not understand, in both directions, so a
-- future kind cannot be written by an old binary and read as a known one.
CREATE TRIGGER IF NOT EXISTS `operation_scope_kind_domain_insert`
BEFORE INSERT ON `operation`
FOR EACH ROW
WHEN NEW.`scope_kind` NOT IN ('main', 'linked', 'repository', 'unknown')
BEGIN
    SELECT RAISE(ABORT, 'operation.scope_kind must be main, linked, repository or unknown');
END;

CREATE TRIGGER IF NOT EXISTS `operation_scope_kind_domain_update`
BEFORE UPDATE OF `scope_kind` ON `operation`
FOR EACH ROW
WHEN NEW.`scope_kind` NOT IN ('main', 'linked', 'repository', 'unknown')
BEGIN
    SELECT RAISE(ABORT, 'operation.scope_kind must be main, linked, repository or unknown');
END;
