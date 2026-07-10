-- AG-24a compliance (plan.md Task A8.5): append-only audit log for raw
-- (un-redacted) checkpoint access / export.
--
-- Every `libra agent checkpoint export --allow-raw` (and any raw-access
-- denial) writes exactly one row here. The table is APPEND-ONLY: only
-- INSERT and SELECT are permitted; UPDATE and DELETE are rejected at the
-- database level by the triggers below (RAISE(ABORT) → SQLITE_CONSTRAINT),
-- so audit history cannot be silently rewritten or trimmed. Dropping the
-- whole table / bulk deletion must go through a compliance-approval flow,
-- never ordinary `libra agent clean` / GC (agent.md §合规).
--
-- Rollback semantics: the paired `_down.sql` does NOT delete audit data.
-- It installs a BEFORE INSERT freeze trigger that stops new writes while
-- preserving every recorded row and the UPDATE/DELETE-reject triggers.
-- Re-applying this forward migration drops that freeze trigger to
-- re-enable writes (see the leading DROP TRIGGER), so an up→down→up cycle
-- is well defined and never destroys history.

DROP TRIGGER IF EXISTS agent_audit_log_frozen_after_rollback;

CREATE TABLE IF NOT EXISTS agent_audit_log (
    -- UUID assigned at write time.
    audit_id       TEXT    NOT NULL PRIMARY KEY,
    -- UTC ISO-8601 timestamp of the access.
    timestamp      TEXT    NOT NULL,
    -- Resolved end-user identity from the GIT_COMMITTER_* / GIT_AUTHOR_* /
    -- EMAIL / LIBRA_COMMITTER_* environment variables only (see
    -- src/command/agent/checkpoint.rs); NULL when none are set. NOTE: does
    -- NOT currently fall back to the repo config user.name/user.email — a
    -- repo whose identity lives only in `libra config` (no committer env
    -- exported) records a NULL actor. Never the checkpoint's hardcoded
    -- `Libra <ai@libra>` committer.
    user_id        TEXT,
    user_name      TEXT,
    -- Audited action; `raw_export` today (kept as free text for additive
    -- evolution rather than a CHECK that would need a migration to widen).
    action         TEXT    NOT NULL,
    -- The checkpoint whose raw content was accessed.
    checkpoint_id  TEXT    NOT NULL,
    -- Read scope: transcript / prompt / context / stderr / full.
    scope          TEXT    NOT NULL,
    -- Destination path when the raw content was written out (NULL for a
    -- denied access or an in-place raw read).
    export_path    TEXT,
    -- Operator-supplied authorization justification.
    justification  TEXT,
    -- Whether the access was granted (1) or denied fail-closed (0). A
    -- denial still records a row so refusals are auditable.
    granted        INTEGER NOT NULL DEFAULT 1
);

-- Chronological / per-checkpoint lookup without scanning the whole log.
CREATE INDEX IF NOT EXISTS idx_agent_audit_log_timestamp
    ON agent_audit_log (timestamp);
CREATE INDEX IF NOT EXISTS idx_agent_audit_log_checkpoint
    ON agent_audit_log (checkpoint_id);

-- Append-only enforcement. Row-level BEFORE triggers with no `OF <column>`
-- clause so ANY update/delete statement fires them (per the
-- sql/publish/0002 rationale: per-column triggers miss statements that
-- omit the column). Unconditional RAISE(ABORT).
CREATE TRIGGER IF NOT EXISTS agent_audit_log_no_update
    BEFORE UPDATE ON agent_audit_log
    FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent_audit_log is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER IF NOT EXISTS agent_audit_log_no_delete
    BEFORE DELETE ON agent_audit_log
    FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent_audit_log is append-only: DELETE is not permitted');
END;
