-- Capture catalog schema for `.libra/private/capture.db` only.
-- This file is the Capture role bootstrap and its only migration.
-- It is not a Repository migration and must not run against `libra.db`.
-- The `ai_thread` foreign key is omitted: that table stays in the repository
-- database, and the two files are never attached.

CREATE TABLE IF NOT EXISTS schema_versions (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS capture_catalog_copy_receipt (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    copied_at TEXT NOT NULL
);

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

CREATE TABLE IF NOT EXISTS `agent_bridge_checkpoint` (
    `checkpoint_id`       TEXT PRIMARY KEY,
    `bridge_session_id`   TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    `agent_checkpoint_id` TEXT,
    `target_oid`          TEXT,
    `created_at`          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS `agent_bridge_event` (
    `event_id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `bridge_session_id` TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    `event_seq`         INTEGER NOT NULL,
    `event_type`        TEXT NOT NULL,
    `payload_sha256`    TEXT NOT NULL,
    `payload`           TEXT NOT NULL CHECK(length(`payload`) <= 262144),
    `operation_id`      TEXT,
    `created_at`        INTEGER NOT NULL,
    UNIQUE(`bridge_session_id`, `event_seq`)
);

CREATE TABLE IF NOT EXISTS `agent_bridge_link` (
    `link_id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `bridge_session_id` TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    -- What produced the association. Ingress kinds plus the mutation result
    -- kinds (`commit.create` / `checkpoint.restore` / `review.run`).
    `source_type`      TEXT NOT NULL CHECK(`source_type` IN (
        'event','operation','checkpoint','evidence','provenance',
        'commit','restore','review'
    )),
    `source_id`        TEXT NOT NULL,
    -- The relation kind. Open TEXT: ingress peers name the target's own kind.
    `target_type`      TEXT NOT NULL,
    `target_id`        TEXT NOT NULL,
    `created_at`       INTEGER NOT NULL,
    UNIQUE(`source_type`, `source_id`, `target_type`, `target_id`)
);

CREATE TABLE IF NOT EXISTS `agent_bridge_operation` (
    `operation_id`   TEXT PRIMARY KEY,
    `bridge_session_id` TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    `method`         TEXT NOT NULL,
    `params_digest`  TEXT NOT NULL,
    `repository_id`  TEXT NOT NULL,
    `workspace_id`   TEXT,
    `status`         TEXT NOT NULL CHECK(`status` IN ('pending','applied','failed','quarantined')),
    `result_digest`  TEXT,
    `created_at`     INTEGER NOT NULL,
    `completed_at`   INTEGER
);

CREATE TABLE IF NOT EXISTS `agent_bridge_session` (
    `bridge_session_id`  TEXT PRIMARY KEY,
    -- Fixed source (ADR-LB-02). `deepseek-harness` is never an AgentKind.
    `source`             TEXT NOT NULL CHECK(`source` = 'deepseek-harness'),
    `repository_id`      TEXT NOT NULL,
    `worktree_id`        TEXT,
    `workspace_id`       TEXT,
    `parent_session_id`  TEXT,
    -- Harness agent/subagent identity (GC-LB-07 derived lineage, not self-reported).
    `agent_id`           TEXT,
    `session_state`      TEXT NOT NULL CHECK(`session_state` IN ('open','flushing','closed','quarantined')),
    `schema_version`     INTEGER NOT NULL DEFAULT 1,
    -- Highest contiguous event_seq accepted and the highest acked to the peer.
    `last_event_seq`     INTEGER NOT NULL DEFAULT 0,
    `last_acked_seq`     INTEGER NOT NULL DEFAULT 0,
    `created_at`         INTEGER NOT NULL,
    `updated_at`         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS `agent_capture_cloud_base` (
    `repo_id`             TEXT    PRIMARY KEY,
    `remote_generation`   INTEGER NOT NULL CHECK(`remote_generation` > 0),
    `updated_at`          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS `agent_capture_incarnation` (
    `agent_kind`                    TEXT    NOT NULL,
    `provider_session_id`           TEXT    NOT NULL,
    `next_session_sync_revision`    INTEGER NOT NULL
        CHECK(`next_session_sync_revision` > 1),
    `source_namespace`              TEXT    NOT NULL
        CHECK(length(`source_namespace`) = 32),
    `updated_at`                    INTEGER NOT NULL,
    PRIMARY KEY (`agent_kind`, `provider_session_id`)
);

CREATE TABLE IF NOT EXISTS `agent_checkpoint` (
    `checkpoint_id`        TEXT PRIMARY KEY,
    `session_id`           TEXT NOT NULL REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `parent_checkpoint_id` TEXT,
    `scope`                TEXT NOT NULL CHECK(`scope` IN ('temporary','committed','subagent')),
    `parent_commit`        TEXT,
    `tree_oid`             TEXT NOT NULL,
    `metadata_blob_oid`    TEXT NOT NULL,
    `traces_commit`        TEXT NOT NULL,
    `tool_use_id`          TEXT,
    `subagent_session_id`  TEXT,
    `description`          TEXT,
    `created_at`           INTEGER NOT NULL
, `sync_revision` INTEGER NOT NULL DEFAULT 1);

CREATE TABLE IF NOT EXISTS `agent_checkpoint_prune_tombstone` (
    `checkpoint_id`  TEXT    PRIMARY KEY,
    `session_id`     TEXT    NOT NULL,
    `pruned_at`      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS `agent_coverage_claim` (
    -- Logical identity (UNIQUE below). `coverage_digest` is the CONTENT
    -- version of the turn and deliberately NOT part of the unique key: a
    -- truncated and a completed snapshot of the same turn must collide here
    -- and resolve via the revision model, not become two turns (ADR-DR-08).
    `session_id`               TEXT    NOT NULL
        REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `logical_turn_key`         TEXT    NOT NULL,
    `coverage_schema_version`  INTEGER NOT NULL,
    `coverage_digest`          TEXT    NOT NULL,
    `completeness`             TEXT    NOT NULL
        CHECK(`completeness` IN ('incomplete','complete')),
    -- Current committed revision for this turn; 0 = reserved but nothing
    -- committed yet (readers skip such rows — plan-20260713 ADR-DR-20).
    `revision`                 INTEGER NOT NULL DEFAULT 0,
    `state`                    TEXT    NOT NULL
        CHECK(`state` IN (
            'reserved_live','reserved_import','catalog_committed',
            'abandoned','conflicted'
        )),
    -- Checkpoint id minted for the in-flight attempt (may become unreachable
    -- garbage if the attempt loses the race; never becomes visible then).
    `attempt_checkpoint_id`    TEXT,
    -- Reservation lease (ADR-DR-09/10). All three nullable: a claim that has
    -- reached `catalog_committed` no longer carries an active lease. Fence
    -- increments use COALESCE(fence_token, 0) + 1 so takeover stays monotonic.
    `owner`                    TEXT,
    `lease_expires_at`         INTEGER,
    `fence_token`              INTEGER,
    -- Set only by the final atomic commit transaction (`catalog_committed`
    -- state invariant, ADR-DR-10): both stay NULL before that.
    `checkpoint_id`            TEXT,
    `traces_commit`            TEXT,
    -- Provenance only — never participates in dedup arbitration (ADR-DR-09).
    `source_channel`           TEXT    NOT NULL
        CHECK(`source_channel` IN ('live','import','export')),
    `created_at`               INTEGER NOT NULL,
    `updated_at`               INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS `agent_coverage_conflict` (
    `session_id`                TEXT    NOT NULL
        REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `logical_turn_key`          TEXT    NOT NULL,
    `coverage_schema_version`   INTEGER NOT NULL,
    `incumbent_revision`        INTEGER NOT NULL,
    `incumbent_digest`          TEXT    NOT NULL,
    `incumbent_checkpoint_id`   TEXT,
    `incoming_digest`           TEXT    NOT NULL,
    `incoming_source_channel`   TEXT    NOT NULL
        CHECK(`incoming_source_channel` IN ('live','import','export')),
    `incoming_observed_at`      INTEGER NOT NULL,
    `incoming_canonical_json`   TEXT    NOT NULL,
    `incoming_redaction_report_json` TEXT NOT NULL,
    PRIMARY KEY (`session_id`, `logical_turn_key`, `coverage_schema_version`),
    FOREIGN KEY (`session_id`, `logical_turn_key`, `coverage_schema_version`)
        REFERENCES `agent_coverage_claim`(
            `session_id`, `logical_turn_key`, `coverage_schema_version`
        ) ON DELETE CASCADE,
    FOREIGN KEY (`incumbent_checkpoint_id`)
        REFERENCES `agent_checkpoint`(`checkpoint_id`) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS `agent_coverage_revision` (
    -- Append-only committed history: every column NOT NULL (a committed
    -- revision always has a checkpoint / digest / completeness / channel),
    -- which is the schema-level basis for the graph JSON v1 non-null promise
    -- (plan-20260713 ADR-DR-20).
    `session_id`               TEXT    NOT NULL
        REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `logical_turn_key`         TEXT    NOT NULL,
    `coverage_schema_version`  INTEGER NOT NULL,
    `revision`                 INTEGER NOT NULL,
    `checkpoint_id`            TEXT    NOT NULL,
    `coverage_digest`          TEXT    NOT NULL,
    `completeness`             TEXT    NOT NULL
        CHECK(`completeness` IN ('incomplete','complete')),
    `source_channel`           TEXT    NOT NULL
        CHECK(`source_channel` IN ('live','import','export')),
    `created_at`               INTEGER NOT NULL,
    PRIMARY KEY (`session_id`, `logical_turn_key`, `coverage_schema_version`, `revision`)
);

CREATE TABLE IF NOT EXISTS `agent_export_job` (
    `job_id`               TEXT PRIMARY KEY,
    `agent_kind`           TEXT    NOT NULL,
    `provider_session_id`  TEXT    NOT NULL,
    `owner`                TEXT,
    `lease_expires_at`     INTEGER,
    `fence_token`          INTEGER,
    -- Monotonic counters (ADR-DR-11): observed >= processed always; a
    -- runner that finishes with observed > processed keeps looping within
    -- its deadline or leaves the job dirty for the next idle/takeover.
    `observed_generation`  INTEGER NOT NULL DEFAULT 0,
    `processed_generation` INTEGER NOT NULL DEFAULT 0,
    `state`                TEXT    NOT NULL
        CHECK(`state` IN ('idle','inflight','dirty','failed')),
    `last_error_code`      TEXT,
    `created_at`           INTEGER NOT NULL,
    `updated_at`           INTEGER NOT NULL,
    `ttl_expires_at`       INTEGER NOT NULL
, `repo_id` TEXT, `worktree_id` TEXT, `workspace_id` TEXT, `workspace_fence` INTEGER, `scope_state` TEXT NOT NULL
    DEFAULT 'legacy_unknown'
    CHECK(`scope_state` IN ('legacy_unknown', 'scoped')));

CREATE TABLE IF NOT EXISTS `agent_import_identity` (
    `identity_id`            TEXT PRIMARY KEY,
    `agent_kind`             TEXT    NOT NULL,
    `provider_session_id`    TEXT    NOT NULL,
    `source_kind`            TEXT    NOT NULL,
    -- Provider-root-relative id or salted fingerprint — NEVER an absolute
    -- home path (GC-DR-13 / ADR-DR-06: must not leak the user's home).
    `source_id`              TEXT    NOT NULL,
    `schema_version`         INTEGER NOT NULL,
    -- Content digests: what the source currently presents vs what has been
    -- committed. A digest change appends a coverage revision (never rewrites
    -- the structural checkpoint parent).
    `observed_digest`        TEXT,
    `committed_digest`       TEXT,
    -- Crash-recovery cursor: the injectable attempt checkpoint id and the
    -- next per-turn ordinal to write.
    `attempt_id`             TEXT,
    `attempt_checkpoint_id`  TEXT,
    `next_ordinal`           INTEGER NOT NULL DEFAULT 0,
    `state`                  TEXT    NOT NULL
        CHECK(`state` IN ('discovered','leased','writing','partial','committed','failed')),
    `owner`                  TEXT,
    `lease_expires_at`       INTEGER,
    `fence_token`            INTEGER,
    `last_error_code`        TEXT,
    `created_at`             INTEGER NOT NULL,
    `updated_at`             INTEGER NOT NULL
, `repo_id` TEXT, `worktree_id` TEXT, `workspace_id` TEXT, `workspace_fence` INTEGER, `scope_state` TEXT NOT NULL
    DEFAULT 'legacy_unknown'
    CHECK(`scope_state` IN ('legacy_unknown', 'scoped')));

CREATE TABLE IF NOT EXISTS `agent_import_tombstone` (
    `tombstone_id`         TEXT PRIMARY KEY,
    `agent_kind`           TEXT    NOT NULL,
    `provider_session_id`  TEXT    NOT NULL,
    -- The `<provider>__<provider_session_id>` capture session id; no FK so it
    -- survives deletion of `agent_session` (DR-07 reads it to show `erased`).
    `erased_session_id`    TEXT    NOT NULL,
    -- Audit-only, non-reversible; NEVER a match/resurrection condition.
    `source_fingerprint`   TEXT,
    `erased_at`            INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS "agent_session" (
    `session_id`           TEXT PRIMARY KEY,
    -- Historical CHECK from capture introduction. `AgentKind::as_db_str`
    -- can be wider: later kinds are added by a dedicated rebuild
    -- (`2026101901` adds `grok`). Do not edit this list to match the enum.
    `agent_kind`           TEXT NOT NULL CHECK(`agent_kind` IN ('claude_code', 'cursor', 'codex', 'gemini', 'opencode', 'copilot', 'factory_ai', 'grok', 'hermes', 'pi')),
    `provider_session_id`  TEXT NOT NULL,
    -- Soft FK to `ai_thread(thread_id)`; ON DELETE SET NULL because losing the
    -- thread row should not cascade-delete captured external-agent sessions.
    `thread_id`            TEXT,
    `state`                TEXT NOT NULL CHECK(`state` IN ('pending','active','condensed','stopped','quarantined')),
    `working_dir`          TEXT NOT NULL,
    `worktree_id`          TEXT,
    `parent_commit`        TEXT,
    `parent_session_id`    TEXT,
    `metadata_json`        TEXT NOT NULL DEFAULT '{}',
    `redaction_report`     TEXT NOT NULL DEFAULT '{}',
    `started_at`           INTEGER NOT NULL,
    `last_event_at`        INTEGER NOT NULL,
    `stopped_at`           INTEGER,
    `schema_version`       INTEGER NOT NULL DEFAULT 1
, `sync_revision` INTEGER NOT NULL DEFAULT 1, `repo_id` TEXT, `workspace_id` TEXT, `workspace_fence` INTEGER, `scope_state` TEXT NOT NULL
    DEFAULT 'legacy_unknown'
    CHECK(`scope_state` IN ('legacy_unknown', 'scoped')));

CREATE TABLE IF NOT EXISTS `agent_session_commit` (
    `session_id` TEXT NOT NULL REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `hash_kind` TEXT NOT NULL CHECK (`hash_kind` IN ('sha1', 'sha256', 'blake3')),
    `commit_oid` TEXT NOT NULL CHECK (
        `commit_oid` NOT GLOB '*[^0-9a-f]*' AND
        ((`hash_kind` = 'sha1' AND length(`commit_oid`) = 40) OR
         (`hash_kind` IN ('sha256', 'blake3') AND length(`commit_oid`) = 64))
    ),
    PRIMARY KEY (`session_id`, `hash_kind`, `commit_oid`)
);

CREATE TABLE IF NOT EXISTS `agent_subagent_content_claim` (
    `parent_session_id`       TEXT    NOT NULL
        REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `provider_kind`           TEXT    NOT NULL,
    `source_key`              TEXT    NOT NULL,
    `content_schema_version`  INTEGER NOT NULL,
    `current_revision`        INTEGER NOT NULL DEFAULT 0,
    `current_checkpoint_id`   TEXT,
    `current_digest`          TEXT,
    `state`                   TEXT    NOT NULL
        CHECK(`state` IN ('idle','reserved')),
    `attempt_digest`          TEXT,
    `attempt_checkpoint_id`   TEXT,
    `owner`                   TEXT,
    `lease_expires_at`        INTEGER,
    `fence_token`             INTEGER NOT NULL DEFAULT 0,
    `created_at`              INTEGER NOT NULL,
    `updated_at`              INTEGER NOT NULL, `revision_cursor` INTEGER NOT NULL DEFAULT 0, `sync_revision` INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (
        `parent_session_id`, `provider_kind`, `source_key`,
        `content_schema_version`
    ),
    CHECK(
        (`current_revision` = 0 AND `current_checkpoint_id` IS NULL AND `current_digest` IS NULL)
        OR
        (`current_revision` > 0 AND `current_checkpoint_id` IS NOT NULL AND `current_digest` IS NOT NULL)
    ),
    CHECK(
        (`state` = 'idle' AND `attempt_digest` IS NULL AND `attempt_checkpoint_id` IS NULL
            AND `owner` IS NULL AND `lease_expires_at` IS NULL)
        OR
        (`state` = 'reserved' AND `attempt_digest` IS NOT NULL AND `owner` IS NOT NULL
            AND `lease_expires_at` IS NOT NULL)
    )
);

CREATE TABLE IF NOT EXISTS `agent_subagent_content_revision` (
    `parent_session_id`       TEXT    NOT NULL
        REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `provider_kind`           TEXT    NOT NULL,
    `source_key`              TEXT    NOT NULL,
    `content_schema_version`  INTEGER NOT NULL,
    `revision`                INTEGER NOT NULL,
    `checkpoint_id`           TEXT    NOT NULL
        REFERENCES `agent_checkpoint`(`checkpoint_id`) ON DELETE CASCADE,
    `content_digest`          TEXT    NOT NULL,
    `source_channel`          TEXT    NOT NULL
        CHECK(`source_channel` IN ('live','import')),
    `partial`                 INTEGER NOT NULL CHECK(`partial` IN (0, 1)),
    `created_at`              INTEGER NOT NULL,
    PRIMARY KEY (
        `parent_session_id`, `provider_kind`, `source_key`,
        `content_schema_version`, `revision`
    ),
    UNIQUE(`checkpoint_id`),
    FOREIGN KEY (
        `parent_session_id`, `provider_kind`, `source_key`,
        `content_schema_version`
    ) REFERENCES `agent_subagent_content_claim`(
        `parent_session_id`, `provider_kind`, `source_key`,
        `content_schema_version`
    ) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS `agent_subagent_link` (
    `content_checkpoint_id`   TEXT    PRIMARY KEY
        REFERENCES `agent_checkpoint`(`checkpoint_id`) ON DELETE CASCADE,
    `parent_session_id`       TEXT    NOT NULL
        REFERENCES `agent_session`(`session_id`) ON DELETE CASCADE,
    `link_state`              TEXT    NOT NULL
        CHECK(`link_state` IN ('resolved','unresolved')),
    `boundary_checkpoint_id`  TEXT
        REFERENCES `agent_checkpoint`(`checkpoint_id`) ON DELETE SET NULL,
    `stable_subagent_id`      TEXT,
    `created_at`              INTEGER NOT NULL,
    `updated_at`              INTEGER NOT NULL, `sync_revision` INTEGER NOT NULL DEFAULT 1,
    CHECK(
        (`link_state` = 'resolved' AND `boundary_checkpoint_id` IS NOT NULL
            AND `stable_subagent_id` IS NOT NULL)
        OR
        (`link_state` = 'unresolved' AND `boundary_checkpoint_id` IS NULL)
    )
);

CREATE TABLE IF NOT EXISTS `agent_usage_stats` (
    `id` TEXT PRIMARY KEY,
    `session_id` TEXT,
    `thread_id` TEXT,
    `agent_run_id` TEXT,
    `run_id` TEXT,
    `provider` TEXT NOT NULL,
    `model` TEXT NOT NULL,
    `request_kind` TEXT NOT NULL DEFAULT 'completion',
    `intent` TEXT,
    `prompt_tokens` INTEGER NOT NULL DEFAULT 0,
    `completion_tokens` INTEGER NOT NULL DEFAULT 0,
    `cached_tokens` INTEGER NOT NULL DEFAULT 0,
    `reasoning_tokens` INTEGER NOT NULL DEFAULT 0,
    `total_tokens` INTEGER NOT NULL DEFAULT 0,
    `tool_call_count` INTEGER NOT NULL DEFAULT 0,
    `wall_clock_ms` INTEGER NOT NULL DEFAULT 0,
    `provider_latency_ms` INTEGER,
    `cost_estimate_micro_dollars` INTEGER,
    `cost_usd` REAL,
    `usage_estimated` INTEGER NOT NULL DEFAULT 0,
    `started_at` TEXT,
    `finished_at` TEXT,
    `success` INTEGER NOT NULL DEFAULT 1,
    `error_kind` TEXT,
    `schema_version` INTEGER NOT NULL DEFAULT 1,
    `created_at` TEXT NOT NULL
, `agent_name` TEXT, `repo_id` TEXT, `turn_id` TEXT, `event_id` TEXT);

CREATE TABLE IF NOT EXISTS `agent_workspace_scope_audit` (
    `audit_id` TEXT NOT NULL PRIMARY KEY,
    `action` TEXT NOT NULL CHECK(`action` = 'adopt_legacy_capture_scope'),
    `agent_kind` TEXT NOT NULL,
    `provider_session_id` TEXT NOT NULL,
    `repo_id` TEXT NOT NULL,
    `worktree_id` TEXT NOT NULL,
    `workspace_id` TEXT,
    `workspace_fence` INTEGER,
    `actor` TEXT,
    `created_at` INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_audit_log_checkpoint
    ON agent_audit_log (checkpoint_id);

CREATE INDEX IF NOT EXISTS idx_agent_audit_log_timestamp
    ON agent_audit_log (timestamp);

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_checkpoint_session`
    ON `agent_bridge_checkpoint`(`bridge_session_id`, `created_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_event_operation`
    ON `agent_bridge_event`(`operation_id`) WHERE `operation_id` IS NOT NULL;

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_event_session_seq`
    ON `agent_bridge_event`(`bridge_session_id`, `event_seq`);

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_link_source`
    ON `agent_bridge_link`(`source_type`, `source_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_link_target`
    ON `agent_bridge_link`(`target_type`, `target_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_created_paging`
    ON `agent_checkpoint`(`created_at` DESC, `checkpoint_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_prune_tombstone_session`
    ON `agent_checkpoint_prune_tombstone`(`session_id`, `pruned_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_scope`
    ON `agent_checkpoint`(`scope`);

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_session`
    ON `agent_checkpoint`(`session_id`, `created_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_checkpoint_traces_commit`
    ON `agent_checkpoint`(`traces_commit`);

CREATE INDEX IF NOT EXISTS `idx_agent_coverage_claim_checkpoint_id`
    ON `agent_coverage_claim`(`checkpoint_id`);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_coverage_claim_logical_key`
    ON `agent_coverage_claim`(`session_id`, `logical_turn_key`, `coverage_schema_version`);

CREATE INDEX IF NOT EXISTS `idx_agent_coverage_claim_session_state`
    ON `agent_coverage_claim`(`session_id`, `state`);

CREATE INDEX IF NOT EXISTS `idx_agent_coverage_conflict_observed_at`
    ON `agent_coverage_conflict`(`incoming_observed_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_coverage_revision_checkpoint_id`
    ON `agent_coverage_revision`(`checkpoint_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_export_job_capture_provider`
    ON `agent_export_job`(`provider_session_id`);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_export_job_session`
    ON `agent_export_job`(`agent_kind`, `provider_session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_export_job_ttl`
    ON `agent_export_job`(`ttl_expires_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_export_job_workspace_scope`
    ON `agent_export_job`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';

CREATE INDEX IF NOT EXISTS `idx_agent_import_identity_capture_provider`
    ON `agent_import_identity`(`provider_session_id`);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_import_identity_key`
    ON `agent_import_identity`(
        `agent_kind`, `provider_session_id`, `source_kind`, `source_id`, `schema_version`
    );

CREATE INDEX IF NOT EXISTS `idx_agent_import_identity_workspace_scope`
    ON `agent_import_identity`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_import_tombstone_erased_session`
    ON `agent_import_tombstone`(`erased_session_id`);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_import_tombstone_provider`
    ON `agent_import_tombstone`(`agent_kind`, `provider_session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_session_active`
    ON `agent_session`(`state`, `working_dir`) WHERE `state` = 'active';

CREATE INDEX IF NOT EXISTS `idx_agent_session_capture_provider`
    ON `agent_session`(`provider_session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_session_commit_oid`
    ON `agent_session_commit` (`hash_kind`, `commit_oid`, `session_id`);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_session_provider`
    ON `agent_session`(`agent_kind`, `provider_session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_session_started_paging`
    ON `agent_session`(`started_at` DESC, `session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_session_thread`
    ON `agent_session`(`thread_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_session_workspace_scope`
    ON `agent_session`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';

CREATE INDEX IF NOT EXISTS `idx_agent_subagent_content_claim_current`
    ON `agent_subagent_content_claim`(`current_checkpoint_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_subagent_content_claim_state`
    ON `agent_subagent_content_claim`(`state`, `lease_expires_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_subagent_content_revision_source`
    ON `agent_subagent_content_revision`(
        `parent_session_id`, `provider_kind`, `source_key`, `revision`
    );

CREATE INDEX IF NOT EXISTS `idx_agent_subagent_link_boundary`
    ON `agent_subagent_link`(`boundary_checkpoint_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_subagent_link_parent_state`
    ON `agent_subagent_link`(`parent_session_id`, `link_state`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_agent_name_provider_model`
    ON `agent_usage_stats` (`agent_name`, `provider`, `model`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_agent_run`
    ON `agent_usage_stats` (`agent_run_id`);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_usage_stats_event_id`
    ON `agent_usage_stats` (`session_id`, `event_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_provider_model`
    ON `agent_usage_stats` (`provider`, `model`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_repo_session`
    ON `agent_usage_stats` (`repo_id`, `session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_session`
    ON `agent_usage_stats` (`session_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_started`
    ON `agent_usage_stats` (`started_at`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_thread`
    ON `agent_usage_stats` (`thread_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_usage_stats_turn`
    ON `agent_usage_stats` (`turn_id`);

CREATE INDEX IF NOT EXISTS `idx_agent_workspace_scope_audit_session`
    ON `agent_workspace_scope_audit`(`agent_kind`, `provider_session_id`, `created_at`);

CREATE TRIGGER IF NOT EXISTS agent_audit_log_no_delete
    BEFORE DELETE ON agent_audit_log
    FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent_audit_log is append-only: DELETE is not permitted');
END;

CREATE TRIGGER IF NOT EXISTS agent_audit_log_no_update
    BEFORE UPDATE ON agent_audit_log
    FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent_audit_log is append-only: UPDATE is not permitted');
END;

CREATE TRIGGER IF NOT EXISTS `agent_export_job_scope_guard_insert`
BEFORE INSERT ON `agent_export_job`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER IF NOT EXISTS `agent_export_job_scope_guard_update`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_export_job`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER IF NOT EXISTS `agent_export_job_scope_immutable`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_export_job`
WHEN OLD.`scope_state` = 'scoped'
 AND (NEW.`scope_state` <> OLD.`scope_state`
      OR NEW.`repo_id` IS NOT OLD.`repo_id`
      OR NEW.`worktree_id` IS NOT OLD.`worktree_id`
      OR NEW.`workspace_id` IS NOT OLD.`workspace_id`
      OR NEW.`workspace_fence` IS NOT OLD.`workspace_fence`)
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job ownership is immutable');
END;

CREATE TRIGGER IF NOT EXISTS `agent_import_identity_scope_guard_insert`
BEFORE INSERT ON `agent_import_identity`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER IF NOT EXISTS `agent_import_identity_scope_guard_update`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_import_identity`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER IF NOT EXISTS `agent_import_identity_scope_immutable`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_import_identity`
WHEN OLD.`scope_state` = 'scoped'
 AND (NEW.`scope_state` <> OLD.`scope_state`
      OR NEW.`repo_id` IS NOT OLD.`repo_id`
      OR NEW.`worktree_id` IS NOT OLD.`worktree_id`
      OR NEW.`workspace_id` IS NOT OLD.`workspace_id`
      OR NEW.`workspace_fence` IS NOT OLD.`workspace_fence`)
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity ownership is immutable');
END;

CREATE TRIGGER IF NOT EXISTS `agent_session_scope_guard_insert`
BEFORE INSERT ON `agent_session`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = ''
      OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent session is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER IF NOT EXISTS `agent_session_scope_guard_update`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_session`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = ''
      OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent session is missing repository, worktree, or workspace fence');
END;

CREATE TRIGGER IF NOT EXISTS `agent_session_scope_immutable`
BEFORE UPDATE OF `repo_id`, `worktree_id`, `workspace_id`, `workspace_fence`, `scope_state`
ON `agent_session`
WHEN OLD.`scope_state` = 'scoped'
 AND (NEW.`scope_state` <> OLD.`scope_state`
      OR NEW.`repo_id` IS NOT OLD.`repo_id`
      OR NEW.`worktree_id` IS NOT OLD.`worktree_id`
      OR NEW.`workspace_id` IS NOT OLD.`workspace_id`
      OR NEW.`workspace_fence` IS NOT OLD.`workspace_fence`)
BEGIN
    SELECT RAISE(ABORT, 'scoped agent session ownership is immutable');
END;

CREATE TRIGGER IF NOT EXISTS `agent_tombstone_block_checkpoint_insert`
BEFORE INSERT ON `agent_checkpoint`
WHEN EXISTS (
    SELECT 1 FROM `agent_import_tombstone`
    WHERE `erased_session_id` = NEW.`session_id`
)
BEGIN
    SELECT RAISE(ABORT, 'agent checkpoint is protected by an erasure tombstone');
END;

CREATE TRIGGER IF NOT EXISTS `agent_tombstone_block_session_insert`
BEFORE INSERT ON `agent_session`
WHEN EXISTS (
    SELECT 1 FROM `agent_import_tombstone`
    WHERE `agent_kind` = NEW.`agent_kind`
      AND `provider_session_id` = NEW.`provider_session_id`
)
BEGIN
    SELECT RAISE(ABORT, 'agent session is protected by an erasure tombstone');
END;

CREATE TRIGGER IF NOT EXISTS `agent_tombstone_block_session_update`
BEFORE UPDATE OF `agent_kind`, `provider_session_id`, `state`, `last_event_at`, `stopped_at`
ON `agent_session`
WHEN EXISTS (
    SELECT 1 FROM `agent_import_tombstone`
    WHERE `agent_kind` = NEW.`agent_kind`
      AND `provider_session_id` = NEW.`provider_session_id`
)
BEGIN
    SELECT RAISE(ABORT, 'agent session is protected by an erasure tombstone');
END;

CREATE TRIGGER IF NOT EXISTS `agent_workspace_scope_audit_append_only_delete`
BEFORE DELETE ON `agent_workspace_scope_audit`
BEGIN
    SELECT RAISE(ABORT, 'agent workspace scope audit is append-only');
END;

CREATE TRIGGER IF NOT EXISTS `agent_workspace_scope_audit_append_only_update`
BEFORE UPDATE ON `agent_workspace_scope_audit`
BEGIN
    SELECT RAISE(ABORT, 'agent workspace scope audit is append-only');
END;

CREATE TRIGGER IF NOT EXISTS `trg_agent_subagent_boundary_delete`
BEFORE DELETE ON `agent_checkpoint`
BEGIN
    UPDATE `agent_subagent_link`
       SET `link_state` = 'unresolved',
           `boundary_checkpoint_id` = NULL,
           `sync_revision` = `sync_revision` + 1,
           `updated_at` = CAST(strftime('%s', 'now') AS INTEGER) * 1000
     WHERE `boundary_checkpoint_id` = OLD.`checkpoint_id`;
END;
