-- CH-02: repair the repository-scoped Change ID prefix index for databases
-- that already recorded 2026090802 before the index was included in the
-- immutable change_ai_link migration.
CREATE INDEX IF NOT EXISTS `idx_change_identity_v2_repo_change`
    ON `change_identity`(`repo_id`, `change_id`);
