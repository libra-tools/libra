-- CH-02: make repository-scoped Change ID prefix resolution index-backed.
CREATE INDEX IF NOT EXISTS `idx_change_identity_v2_repo_change`
    ON `change_identity`(`repo_id`, `change_id`);
