 -- OL-02: replace the development-only operation schema with v2.
 --
 -- This migration is intentionally forward-only. The old operation tables did
 -- not contain enough information to reconstruct a WorkspaceSnapshotV2, so a
 -- down migration would imply a lossy rollback. Repositories that need the old
 -- audit rows must export them before upgrading.

-- Copy-first legacy migration is orchestrated by the Rust runner so it can
-- inspect the on-disk v1 shape, validate counts and key fields, and roll
-- back the version claim together with every schema/data change.
 CREATE TABLE IF NOT EXISTS `operation` (
     `op_id`               TEXT PRIMARY KEY,
     `repo_id`             TEXT NOT NULL,
     `format_version`      INTEGER NOT NULL DEFAULT 2,
     `kind`                TEXT NOT NULL,
     `status`              TEXT NOT NULL,
     `command_name`        TEXT,
     `description`         TEXT,
     `args_digest`         TEXT,
     `actor`               TEXT,
     `worktree_id`         TEXT,
     `scope_kind`          TEXT NOT NULL,
     `pre_view_oid`        TEXT NOT NULL,
     `post_view_oid`       TEXT NOT NULL,
     `restores_op_id`      TEXT,
     `reverts_op_id`       TEXT,
     `predecessor_map_oid` TEXT,
     `causal_context_id`   TEXT,
     `start_ts`            INTEGER NOT NULL,
     `end_ts`              INTEGER
 );
 CREATE INDEX IF NOT EXISTS `idx_operation_v2_repo_order`
     ON `operation`(`repo_id`, `end_ts` DESC, `start_ts` DESC, `op_id` DESC);

 CREATE TABLE IF NOT EXISTS `operation_parent` (
     `op_id`        TEXT NOT NULL,
     `parent_op_id` TEXT NOT NULL,
     `ordinal`      INTEGER NOT NULL,
     PRIMARY KEY (`op_id`, `parent_op_id`)
 );
 CREATE INDEX IF NOT EXISTS `idx_operation_parent_v2_parent`
     ON `operation_parent`(`parent_op_id`, `op_id`);

 CREATE TABLE IF NOT EXISTS `operation_head` (
     `repo_id`    TEXT NOT NULL,
     `scope_key`  TEXT NOT NULL,
     `op_id`      TEXT NOT NULL,
     `generation` INTEGER NOT NULL,
     PRIMARY KEY (`repo_id`, `scope_key`, `op_id`)
 );
 CREATE INDEX IF NOT EXISTS `idx_operation_head_v2_scope_generation`
     ON `operation_head`(`repo_id`, `scope_key`, `generation` DESC, `op_id`);

 CREATE TABLE IF NOT EXISTS `operation_journal` (
     `journal_id`       TEXT PRIMARY KEY,
     `op_id`            TEXT NOT NULL,
     `phase`            TEXT NOT NULL,
     `pre_view_oid`     TEXT,
     `target_view_oid`  TEXT,
     `owner`            TEXT NOT NULL,
     `updated_at`       INTEGER NOT NULL,
     `recovery_payload` TEXT
 );
 CREATE INDEX IF NOT EXISTS `idx_operation_journal_v2_op`
     ON `operation_journal`(`op_id`, `updated_at` DESC);

 CREATE TABLE IF NOT EXISTS `change_identity` (
     `change_id`     TEXT PRIMARY KEY,
     `repo_id`       TEXT NOT NULL,
     `origin`        TEXT NOT NULL,
     `created_op_id` TEXT NOT NULL,
     `created_at`    INTEGER NOT NULL
 );

 CREATE TABLE IF NOT EXISTS `change_revision` (
     `change_id`        TEXT NOT NULL,
     `commit_oid`       TEXT NOT NULL,
     `created_op_id`    TEXT NOT NULL,
     `visibility`       TEXT NOT NULL,
     `revision_ordinal` INTEGER NOT NULL,
     PRIMARY KEY (`change_id`, `commit_oid`)
 );
 CREATE INDEX IF NOT EXISTS `idx_change_revision_v2_commit`
     ON `change_revision`(`commit_oid`);

 CREATE TABLE IF NOT EXISTS `change_predecessor` (
     `successor_oid`   TEXT NOT NULL,
     `predecessor_oid` TEXT NOT NULL,
     `op_id`           TEXT NOT NULL,
     `relation_kind`   TEXT NOT NULL,
     `ordinal`         INTEGER NOT NULL,
     PRIMARY KEY (`successor_oid`, `predecessor_oid`, `op_id`)
 );

 CREATE TABLE IF NOT EXISTS `ai_operation_link` (
     `operation_id`             TEXT PRIMARY KEY,
     `session_id`               TEXT,
     `run_id`                   TEXT,
     `tool_invocation_id`       TEXT,
     `intent_id`                TEXT,
     `repo_id`                  TEXT NOT NULL,
     `worktree_id`              TEXT,
     `workspace_id`             TEXT,
     `lease_generation`         INTEGER,
     `config_provenance_digest` TEXT,
     `redaction_version`        TEXT NOT NULL
 );
