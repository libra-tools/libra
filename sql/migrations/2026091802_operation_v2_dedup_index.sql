-- OL-15 follow-up: keep the v2 duplicate-operation lookup indexed after the
-- legacy operation namespace is retired.
--
-- The v2 boundary records repository-scoped success rows and checks
-- (repo_id, scope_kind, command_name, args_digest, status, end_ts).  The old
-- v1 index was dropped with the v1 operation table during the OL-02 schema
-- replacement, so existing v2 repositories need this forward-only repair.
CREATE INDEX IF NOT EXISTS `idx_operation_v2_dedup_scope`
    ON `operation`(
        `repo_id`,
        `scope_kind`,
        `command_name`,
        `args_digest`,
        `status`,
        `end_ts`
    );
