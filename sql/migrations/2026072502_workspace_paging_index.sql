-- plan-20260714 Part C W4-s2 hardening: `agent workspace list` is a
-- bounded keyset-paginated machine surface ordered by workspace_id within the
-- current repository. The query must be an index SEARCH on the repo prefix
-- instead of scanning the whole workspace_record table as histories grow.
CREATE INDEX IF NOT EXISTS `idx_workspace_repo_paging`
ON `workspace_record` (`repo_id`, `workspace_id`);
