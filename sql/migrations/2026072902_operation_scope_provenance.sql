-- plan-20260714 Part C W0 (§C.11 "operation scope schema/migration"): record
-- whether an operation's worktree scope was DECLARED by the process that ran
-- it, or merely inherited from a backfill.
--
-- 2026072201 added `operation.worktree_id TEXT NOT NULL DEFAULT ''`, and ''
-- is also how the MAIN worktree spells itself. Every operation that predates
-- that migration therefore claims main scope, whether or not it ran there.
-- In a single-worktree repository that claim is correct and harmless. In a
-- repository that has (or had) linked worktrees it is a guess, and `op
-- restore` acting on a guess rewrites HEAD, the index and the working tree of
-- a worktree the operation may never have touched.
--
-- W0 requires: safe repositories backfill to main; rows with linked/legacy
-- evidence are marked `unknown` and are NOT restored until an explicit doctor
-- action establishes their provenance (ADR-0714-08 — never guess an owner).
--
-- The rule below is deliberately narrow. It marks `unknown` only for rows
-- that BOTH claim main scope and started before 2026072201 was applied to
-- this database, and only when this repository shows linked-worktree
-- evidence. Operations recorded after that migration carry a scope their
-- process actually declared, so they stay `declared` — the marking cannot
-- strand work that was correctly scoped all along.
ALTER TABLE `operation` ADD COLUMN `scope_provenance` TEXT NOT NULL DEFAULT 'declared';

UPDATE `operation`
SET `scope_provenance` = 'unknown'
WHERE `worktree_id` = ''
  AND EXISTS (
      SELECT 1 FROM `reference`
      WHERE `kind` = 'Head' AND `remote` IS NULL AND `worktree_id` IS NOT NULL
  )
  AND `start_ts` < COALESCE(
      (
          SELECT CAST(strftime('%s', `applied_at`) AS INTEGER)
          FROM `schema_versions`
          WHERE `version` = 2026072201
      ),
      0
  );

-- Domain enforcement. SQLite cannot add a CHECK constraint to an existing
-- column, so triggers carry it. Without this the column is free text, and a
-- corrupted or mistyped value ("declraed") would slip past a reader that
-- only tests for the literal "unknown" — failing OPEN on exactly the rows
-- whose trustworthiness is in question.
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
