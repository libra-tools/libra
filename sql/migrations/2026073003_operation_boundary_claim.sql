-- 2026073003_operation_boundary_claim
--
-- plan-20260714 Part C W1 (§C.9, §C.11 W1): let sequencer control actions
-- enter the operation log through BOUNDARY recording — a durable `running`
-- claim, the control action itself on pooled connections, then a second short
-- transaction that closes the row.
--
-- Three things the operation table cannot express yet:
--
-- 1. A worktree may run only ONE mutating control action at a time. The
--    exclusion must be ACROSS PROCESSES and it must not be keyed by the
--    command: two `am` starts with different patches, or an `am --continue`
--    racing a `rebase --skip`, are different identities and would both pass a
--    per-identity check, then both replace this worktree's single sequencer
--    row — losing one sequence while its checkout stays on disk. The claim is
--    therefore a worktree-wide SLOT: `control_slot` is NULL for ordinary
--    operations and non-NULL for a control action, and the partial unique
--    index below admits at most one running control per (repository,
--    worktree). Partial, so completed history stays unconstrained and ordinary
--    operations are unaffected.
--
-- 2. Whether an operation can be restored is a PROPERTY OF THE OPERATION, not
--    of its command name. The snapshot covers HEAD and refs only (§C.9 opening
--    paragraph): it cannot restore an index, a working tree, or sequencer
--    state. Restoring a `rebase --continue` would move HEAD while leaving
--    `sequence_state` pointing at a todo that no longer matches it. The
--    recording side declares that at write time; `op restore` refuses a row
--    with `restorable = 0` before it reads anything else. A column, rather
--    than a check against a command-name string, because the name is a mutable
--    label and a renamed command must not silently become restorable.
--
-- 3. A claim left behind by a killed process must be distinguishable from a
--    live one. `claim_owner` records `<host>/<pid>` so liveness can be PROVEN
--    on the machine that made the claim, instead of guessed from age — a
--    control action can legitimately sit for a long time in an editor or a
--    hook, and revoking a live claim is worse than refusing a dead one.
--
-- Existing rows keep `restorable = 1` and `control_slot = NULL`: every
-- operation recorded before this migration came from `with_operation_log`,
-- whose closure form is exactly the HEAD/refs-only case the snapshot covers.

ALTER TABLE `operation` ADD COLUMN `restorable` INTEGER NOT NULL DEFAULT 1;
ALTER TABLE `operation` ADD COLUMN `control_slot` TEXT;
ALTER TABLE `operation` ADD COLUMN `claim_owner` TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS `idx_operation_control_slot`
    ON `operation` (`repo_id`, `worktree_id`)
    WHERE `status` = 'running' AND `control_slot` IS NOT NULL;
