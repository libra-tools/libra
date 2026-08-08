-- 2026073002_operation_dedup_index
--
-- plan-20260714 Part C W1 (§C.9): give the duplicate-suppression point query
-- an index that matches it.
--
-- The check is now `repo_id = ? AND worktree_id = ? AND command_name = ?
-- AND args_digest = ? AND status = 'succeeded' AND end_ts >= ?`. The only
-- existing index is `(repo_id, end_ts, start_ts, op_id)` (2026072201), so the
-- planner walks every operation this repository recorded inside the window and
-- filters the rest in SQLite — on a busy repository that is a scan on the hot
-- path of every logged command, to find at most a handful of rows.
--
-- Column order follows the predicate's selectivity: the four equalities
-- first, then the range column last so it can still be used as a bound.
CREATE INDEX IF NOT EXISTS `idx_operation_dedup_scope`
    ON `operation` (
        `repo_id`,
        `worktree_id`,
        `command_name`,
        `args_digest`,
        `status`,
        `end_ts`
    );
