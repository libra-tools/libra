BEGIN TRANSACTION;
CREATE TABLE agent_audit_log (
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
CREATE TABLE `agent_bridge_checkpoint` (
    `checkpoint_id`       TEXT PRIMARY KEY,
    `bridge_session_id`   TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    `agent_checkpoint_id` TEXT,
    `target_oid`          TEXT,
    `created_at`          INTEGER NOT NULL
);
CREATE TABLE `agent_bridge_event` (
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
CREATE TABLE `agent_bridge_link` (
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
CREATE TABLE `agent_bridge_operation` (
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
CREATE TABLE `agent_bridge_session` (
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
CREATE TABLE `agent_capture_cloud_base` (
    `repo_id`             TEXT    PRIMARY KEY,
    `remote_generation`   INTEGER NOT NULL CHECK(`remote_generation` > 0),
    `updated_at`          INTEGER NOT NULL
);
CREATE TABLE `agent_capture_incarnation` (
    `agent_kind`                    TEXT    NOT NULL,
    `provider_session_id`           TEXT    NOT NULL,
    `next_session_sync_revision`    INTEGER NOT NULL
        CHECK(`next_session_sync_revision` > 1),
    `source_namespace`              TEXT    NOT NULL
        CHECK(length(`source_namespace`) = 32),
    `updated_at`                    INTEGER NOT NULL,
    PRIMARY KEY (`agent_kind`, `provider_session_id`)
);
CREATE TABLE `agent_checkpoint` (
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
CREATE TABLE `agent_checkpoint_prune_tombstone` (
    `checkpoint_id`  TEXT    PRIMARY KEY,
    `session_id`     TEXT    NOT NULL,
    `pruned_at`      INTEGER NOT NULL
);
CREATE TABLE `agent_coverage_claim` (
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
CREATE TABLE `agent_coverage_conflict` (
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
CREATE TABLE `agent_coverage_revision` (
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
CREATE TABLE `agent_export_job` (
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
CREATE TABLE `agent_import_identity` (
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
CREATE TABLE `agent_import_tombstone` (
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
CREATE TABLE `agent_session` (
    `session_id`           TEXT PRIMARY KEY,
    -- Closed enum mirrored by `AgentKind::as_db_str` in
    -- `src/internal/ai/observed_agents/adapter.rs`. Adding a new value here
    -- requires a paired migration that bumps the CHECK constraint.
    `agent_kind`           TEXT NOT NULL CHECK(`agent_kind` IN (
        'claude_code', 'cursor', 'codex', 'gemini',
        'opencode', 'copilot', 'factory_ai'
    )),
    `provider_session_id`  TEXT NOT NULL,
    -- Soft FK to `ai_thread(thread_id)`; ON DELETE SET NULL because losing the
    -- thread row should not cascade-delete captured external-agent sessions.
    `thread_id`            TEXT REFERENCES `ai_thread`(`thread_id`) ON DELETE SET NULL,
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
CREATE TABLE `agent_subagent_content_claim` (
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
CREATE TABLE `agent_subagent_content_revision` (
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
CREATE TABLE `agent_subagent_link` (
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
CREATE TABLE `agent_usage_stats` (
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
CREATE TABLE `agent_workspace_scope_audit` (
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
CREATE TABLE `ai_decision_proposal` (
    `proposal_id` TEXT PRIMARY KEY,
    `thread_id` TEXT NOT NULL,
    `validation_report_id` TEXT,
    `risk_score_breakdown_id` TEXT,
    `policy_version` TEXT NOT NULL,
    `stale` INTEGER NOT NULL DEFAULT 0 CHECK (`stale` IN (0, 1)),
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `summary_json` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL,
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_final_decision` (
    `decision_id` TEXT PRIMARY KEY,
    `thread_id` TEXT NOT NULL,
    `decision_proposal_id` TEXT,
    `validation_report_id` TEXT,
    `policy_version` TEXT NOT NULL,
    `verdict` TEXT NOT NULL,
    `stale` INTEGER NOT NULL DEFAULT 0 CHECK (`stale` IN (0, 1)),
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `summary_json` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL,
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_index_intent_context_frame` (
    `intent_id` TEXT NOT NULL,
    `context_frame_id` TEXT NOT NULL,
    `relation_kind` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`intent_id`, `context_frame_id`, `relation_kind`)
);
CREATE TABLE `ai_index_intent_plan` (
    `intent_id` TEXT NOT NULL,
    `plan_id` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`intent_id`, `plan_id`)
);
CREATE TABLE `ai_index_intent_task` (
    `intent_id` TEXT NOT NULL,
    `task_id` TEXT NOT NULL,
    `parent_task_id` TEXT,
    `origin_step_id` TEXT,
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`intent_id`, `task_id`)
);
CREATE TABLE `ai_index_plan_step_task` (
    `plan_id` TEXT NOT NULL,
    `task_id` TEXT NOT NULL,
    `step_id` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`plan_id`, `task_id`)
);
CREATE TABLE `ai_index_run_event` (
    `run_id` TEXT NOT NULL,
    `event_id` TEXT NOT NULL,
    `event_kind` TEXT NOT NULL,
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`run_id`, `event_id`)
);
CREATE TABLE `ai_index_run_patchset` (
    `run_id` TEXT NOT NULL,
    `patchset_id` TEXT NOT NULL,
    `sequence` INTEGER NOT NULL,
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`run_id`, `patchset_id`)
);
CREATE TABLE `ai_index_task_run` (
    `task_id` TEXT NOT NULL,
    `run_id` TEXT NOT NULL,
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `created_at` INTEGER NOT NULL,
    PRIMARY KEY (`task_id`, `run_id`)
);
CREATE TABLE `ai_live_context_window` (
    `thread_id` TEXT NOT NULL,
    `context_frame_id` TEXT NOT NULL,
    `position` INTEGER NOT NULL,
    `source_kind` TEXT NOT NULL,
    `pin_kind` TEXT,
    `inserted_at` INTEGER NOT NULL,
    PRIMARY KEY (`thread_id`, `context_frame_id`),
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_operation_link` (
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
CREATE TABLE `ai_risk_score_breakdown` (
    `breakdown_id` TEXT PRIMARY KEY,
    `thread_id` TEXT NOT NULL,
    `validation_report_id` TEXT,
    `policy_version` TEXT NOT NULL,
    `stale` INTEGER NOT NULL DEFAULT 0 CHECK (`stale` IN (0, 1)),
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `summary_json` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL,
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_scheduler_plan_head` (
    `thread_id` TEXT NOT NULL,
    `plan_id` TEXT NOT NULL,
    `ordinal` INTEGER NOT NULL,
    PRIMARY KEY (`thread_id`, `plan_id`),
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_scheduler_selected_plan` (
    `thread_id` TEXT NOT NULL,
    `plan_id` TEXT NOT NULL,
    `ordinal` INTEGER NOT NULL,
    PRIMARY KEY (`thread_id`, `plan_id`),
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_scheduler_state` (
    `thread_id` TEXT PRIMARY KEY,
    `selected_plan_id` TEXT,
    `active_task_id` TEXT,
    `active_run_id` TEXT,
    `metadata_json` TEXT,
    `version` INTEGER NOT NULL DEFAULT 0,
    `updated_at` INTEGER NOT NULL,
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_thread` (
    `thread_id` TEXT PRIMARY KEY,
    `title` TEXT,
    `owner_kind` TEXT NOT NULL,
    `owner_id` TEXT NOT NULL,
    `owner_display_name` TEXT,
    `current_intent_id` TEXT,
    `latest_intent_id` TEXT,
    `metadata_json` TEXT,
    `archived` INTEGER NOT NULL DEFAULT 0 CHECK (`archived` IN (0, 1)),
    `version` INTEGER NOT NULL DEFAULT 0,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL
);
CREATE TABLE `ai_thread_intent` (
    `thread_id` TEXT NOT NULL,
    `intent_id` TEXT NOT NULL,
    `ordinal` INTEGER NOT NULL,
    `is_head` INTEGER NOT NULL DEFAULT 0 CHECK (`is_head` IN (0, 1)),
    `linked_at` INTEGER NOT NULL,
    `link_reason` TEXT NOT NULL,
    PRIMARY KEY (`thread_id`, `intent_id`),
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_thread_participant` (
    `thread_id` TEXT NOT NULL,
    `actor_kind` TEXT NOT NULL,
    `actor_id` TEXT NOT NULL,
    `actor_display_name` TEXT,
    `role` TEXT NOT NULL,
    `joined_at` INTEGER NOT NULL,
    PRIMARY KEY (`thread_id`, `actor_kind`, `actor_id`),
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_thread_provider_metadata` (
    `thread_id` TEXT PRIMARY KEY,
    `legacy_session_id` TEXT,
    `provider_thread_id` TEXT,
    `provider_kind` TEXT,
    `metadata_json` TEXT,
    `updated_at` INTEGER NOT NULL,
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `ai_validation_report` (
    `report_id` TEXT PRIMARY KEY,
    `thread_id` TEXT NOT NULL,
    `run_id` TEXT,
    `policy_version` TEXT NOT NULL,
    `stale` INTEGER NOT NULL DEFAULT 0 CHECK (`stale` IN (0, 1)),
    `is_latest` INTEGER NOT NULL DEFAULT 0 CHECK (`is_latest` IN (0, 1)),
    `summary_json` TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL,
    FOREIGN KEY (`thread_id`) REFERENCES `ai_thread`(`thread_id`) ON DELETE CASCADE
);
CREATE TABLE `approved_permission` (
    `project_id`  TEXT NOT NULL,
    `permission`  TEXT NOT NULL,
    `pattern`     TEXT NOT NULL,
    `created_at`  INTEGER NOT NULL, `source_worktree_id` TEXT NOT NULL DEFAULT '', `source_session_id` TEXT NOT NULL DEFAULT '', `source_workspace_id` TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (`project_id`, `permission`, `pattern`)
);
CREATE TABLE `automation_log` (
    `id` TEXT PRIMARY KEY,
    `rule_id` TEXT NOT NULL,
    `trigger_kind` TEXT NOT NULL,
    `action_kind` TEXT NOT NULL,
    `status` TEXT NOT NULL,
    `message` TEXT NOT NULL,
    `started_at` TEXT NOT NULL,
    `finished_at` TEXT NOT NULL,
    `details_json` TEXT NOT NULL
);
CREATE TABLE `bisect_state` (
    `worktree_id`    TEXT PRIMARY KEY NOT NULL,
    `orig_head`      TEXT NOT NULL,
    `orig_head_name` TEXT,
    `bad`            TEXT,
    `good`           TEXT NOT NULL,
    `current`        TEXT,
    `skipped`        TEXT,
    `steps`          INTEGER,
    `completed`      INTEGER NOT NULL DEFAULT 0,
    `first_parent`   INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE `change_identity` (
     `change_id`     TEXT PRIMARY KEY,
     `repo_id`       TEXT NOT NULL,
     `origin`        TEXT NOT NULL,
     `created_op_id` TEXT NOT NULL,
     `created_at`    INTEGER NOT NULL
 );
CREATE TABLE `change_predecessor` (
     `successor_oid`   TEXT NOT NULL,
     `predecessor_oid` TEXT NOT NULL,
     `op_id`           TEXT NOT NULL,
     `relation_kind`   TEXT NOT NULL,
     `ordinal`         INTEGER NOT NULL,
     PRIMARY KEY (`successor_oid`, `predecessor_oid`, `op_id`)
 );
CREATE TABLE `change_revision` (
     `change_id`        TEXT NOT NULL,
     `commit_oid`       TEXT NOT NULL,
     `created_op_id`    TEXT NOT NULL,
     `visibility`       TEXT NOT NULL,
     `revision_ordinal` INTEGER NOT NULL,
     PRIMARY KEY (`change_id`, `commit_oid`)
 );
CREATE TABLE `config` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `configuration` TEXT NOT NULL,
    `name` TEXT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL
);
CREATE TABLE `config_kv` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL,
    `encrypted` INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE `layer` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id` TEXT NOT NULL DEFAULT '',
    `name`        TEXT NOT NULL,
    `source`      TEXT NOT NULL,
    `priority`    INTEGER NOT NULL DEFAULT 0,
    `enabled`     INTEGER NOT NULL DEFAULT 1,
    `created_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(`worktree_id`, `name`)
);
CREATE TABLE `layer_path` (
    `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id`     TEXT NOT NULL DEFAULT '',
    `layer_name`      TEXT NOT NULL,
    `path`            TEXT NOT NULL,
    `content_hash`    TEXT NOT NULL,
    `materialized_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(`worktree_id`, `path`)
);
CREATE TABLE "legacy_operation" (
     op_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, view_id TEXT NOT NULL,
     command_name TEXT NOT NULL, description TEXT NOT NULL, actor TEXT NOT NULL,
     args_digest TEXT, start_ts INTEGER NOT NULL, end_ts INTEGER, status TEXT NOT NULL,
     worktree_id TEXT NOT NULL DEFAULT '', scope_provenance TEXT NOT NULL DEFAULT 'unknown',
     restorable INTEGER NOT NULL DEFAULT 1, control_slot TEXT, claim_owner TEXT,
     scope_kind TEXT NOT NULL DEFAULT 'unknown');
CREATE TABLE "legacy_operation_parent" (op_id TEXT NOT NULL, parent_op_id TEXT NOT NULL, PRIMARY KEY (op_id, parent_op_id));
CREATE TABLE "legacy_operation_view" (view_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, head_kind TEXT NOT NULL, head_target TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE "legacy_operation_view_ref" (view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL, ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL, PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote));
CREATE TABLE "legacy_operation_view_workspace" (view_id TEXT NOT NULL, pointer_kind TEXT NOT NULL, pointer_value TEXT NOT NULL, PRIMARY KEY (view_id, pointer_kind));
CREATE TABLE `metadata_kv` (
    `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
    `scope`      TEXT NOT NULL,
    `target`     TEXT NOT NULL,
    `key`        TEXT NOT NULL,
    `value`      TEXT NOT NULL,
    `value_type` TEXT NOT NULL DEFAULT 'text',
    `created_at` TEXT NOT NULL,
    `updated_at` TEXT NOT NULL,
    UNIQUE(`scope`, `target`, `key`)
);
INSERT INTO "metadata_kv" VALUES(1,'repository','','stash.reflog.generation','1','text','2026-09-14 10:58:04','2026-09-14 10:58:04');
CREATE TABLE `notes` (
    `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
    `notes_ref`  TEXT NOT NULL,
    `object`     TEXT NOT NULL,
    `blob`       TEXT NOT NULL,
    UNIQUE(`notes_ref`, `object`)
);
CREATE TABLE `object_index` (
    `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
    `o_id`       TEXT NOT NULL,
    `o_type`     TEXT NOT NULL,
    `o_size`     INTEGER NOT NULL,
    `repo_id`    TEXT NOT NULL,
    `created_at` INTEGER NOT NULL,
    `is_synced`  INTEGER DEFAULT 0,
    UNIQUE(`repo_id`, `o_id`)
);
CREATE TABLE `object_obliteration` (
    `id`                   INTEGER PRIMARY KEY AUTOINCREMENT,
    `oid`                  TEXT NOT NULL,
    `hash_kind`            TEXT NOT NULL,
    `state`                TEXT NOT NULL CHECK (`state` IN ('obliterating', 'obliterated')),
    `reason`               TEXT,
    `actor`                TEXT,
    `approval_source`      TEXT,
    `requested_at`         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `tombstone_written_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `payload_deleted_at`   TEXT,
    `updated_at`           TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (`oid`, `hash_kind`)
);
CREATE TABLE `operation` (
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
CREATE TABLE `operation_head` (
     `repo_id`    TEXT NOT NULL,
     `scope_key`  TEXT NOT NULL,
     `op_id`      TEXT NOT NULL,
     `generation` INTEGER NOT NULL,
     PRIMARY KEY (`repo_id`, `scope_key`, `op_id`)
 );
CREATE TABLE `operation_journal` (
     `journal_id`       TEXT PRIMARY KEY,
     `op_id`            TEXT NOT NULL,
     `phase`            TEXT NOT NULL,
     `pre_view_oid`     TEXT,
     `target_view_oid`  TEXT,
     `owner`            TEXT NOT NULL,
     `updated_at`       INTEGER NOT NULL,
     `recovery_payload` TEXT
 );
CREATE TABLE `operation_parent` (
     `op_id`        TEXT NOT NULL,
     `parent_op_id` TEXT NOT NULL,
     `ordinal`      INTEGER NOT NULL,
     PRIMARY KEY (`op_id`, `parent_op_id`)
 );
CREATE TABLE `rebase_state` (
    `worktree_id`  TEXT PRIMARY KEY NOT NULL,
    `head_name`    TEXT NOT NULL,
    `onto`         TEXT NOT NULL,
    `orig_head`    TEXT NOT NULL,
    `current_head` TEXT NOT NULL,
    `todo`         TEXT NOT NULL,
    `todo_actions` TEXT NOT NULL DEFAULT '',
    `done`         TEXT NOT NULL,
    `stopped_sha`  TEXT,
    `autosquash`   INTEGER NOT NULL DEFAULT 0,
    `empty_mode`   TEXT NOT NULL DEFAULT 'keep'
);
CREATE TABLE `reference` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    -- name can't be ''
    `name` TEXT CHECK (name <> '' OR name IS NULL),
    `kind` TEXT NOT NULL CHECK (kind IN ('Branch', 'Tag', 'Head')),
    `commit` TEXT,
    -- remote can't be ''. If kind is Tag, remote must be NULL.
    `remote` TEXT CHECK (remote <> '' OR remote IS NULL), `worktree_id` TEXT,
    CHECK (
        (kind <> 'Tag' OR remote IS NULL)
    )
);
CREATE TABLE `reflog` (
    `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
    `ref_name`        TEXT NOT NULL,
    `old_oid`         TEXT NOT NULL,
    `new_oid`         TEXT NOT NULL,
    `committer_name`  TEXT NOT NULL,
    `committer_email` TEXT NOT NULL,
    `timestamp`       INTEGER NOT NULL,
    `action`          TEXT NOT NULL,
    `message`         TEXT NOT NULL
, `worktree_id` TEXT);
CREATE TABLE `revision_ordinal` (
    `id`       INTEGER PRIMARY KEY AUTOINCREMENT,
    `ref_name` TEXT NOT NULL,
    `ordinal`  INTEGER NOT NULL,
    `oid`      TEXT NOT NULL,
    UNIQUE(`ref_name`, `ordinal`),
    UNIQUE(`ref_name`, `oid`)
);
CREATE TABLE `revision_ordinal_meta` (
    `ref_name`    TEXT PRIMARY KEY,
    `tip_oid`     TEXT NOT NULL,
    `replace_sig` TEXT NOT NULL DEFAULT '',
    `max_ordinal` INTEGER NOT NULL,
    `built_at`    TEXT NOT NULL
);
CREATE TABLE `schema_versions` (
    `version` INTEGER PRIMARY KEY,
    `name` TEXT NOT NULL,
    `applied_at` TEXT NOT NULL
);
INSERT INTO "schema_versions" VALUES(2026050301,'automation_log','2026-09-14T10:58:04.357539937+00:00');
INSERT INTO "schema_versions" VALUES(2026050302,'agent_usage_stats','2026-09-14T10:58:04.358120484+00:00');
INSERT INTO "schema_versions" VALUES(2026050303,'agent_capture','2026-09-14T10:58:04.358583614+00:00');
INSERT INTO "schema_versions" VALUES(2026050501,'agent_checkpoint_parent_nullable','2026-09-14T10:58:04.359092577+00:00');
INSERT INTO "schema_versions" VALUES(2026050601,'approved_permission','2026-09-14T10:58:04.360228130+00:00');
INSERT INTO "schema_versions" VALUES(2026050801,'agent_usage_stats_agent_name','2026-09-14T10:58:04.360640884+00:00');
INSERT INTO "schema_versions" VALUES(2026052301,'source_call_log','2026-09-14T10:58:04.361239306+00:00');
INSERT INTO "schema_versions" VALUES(2026053101,'ai_final_decision','2026-09-14T10:58:04.361564851+00:00');
INSERT INTO "schema_versions" VALUES(2026060201,'source_call_log_agent_run_id','2026-09-14T10:58:04.361650435+00:00');
INSERT INTO "schema_versions" VALUES(2026060401,'cherry_pick_state','2026-09-14T10:58:04.361972271+00:00');
INSERT INTO "schema_versions" VALUES(2026060801,'revert_sequence','2026-09-14T10:58:04.362161440+00:00');
INSERT INTO "schema_versions" VALUES(2026061401,'notes','2026-09-14T10:58:04.362301275+00:00');
INSERT INTO "schema_versions" VALUES(2026062301,'rename_agent_traces_branch','2026-09-14T10:58:04.362491818+00:00');
INSERT INTO "schema_versions" VALUES(2026070201,'metadata_kv','2026-09-14T10:58:04.362663278+00:00');
INSERT INTO "schema_versions" VALUES(2026070202,'working_dirty','2026-09-14T10:58:04.362797113+00:00');
INSERT INTO "schema_versions" VALUES(2026070301,'revision_ordinal','2026-09-14T10:58:04.362988698+00:00');
INSERT INTO "schema_versions" VALUES(2026070401,'sequence_state','2026-09-14T10:58:04.363206200+00:00');
INSERT INTO "schema_versions" VALUES(2026070501,'layer','2026-09-14T10:58:04.363410785+00:00');
INSERT INTO "schema_versions" VALUES(2026070601,'object_obliteration','2026-09-14T10:58:04.363559495+00:00');
INSERT INTO "schema_versions" VALUES(2026070701,'sparse_view','2026-09-14T10:58:04.363743580+00:00');
INSERT INTO "schema_versions" VALUES(2026070801,'worktree_isolation','2026-09-14T10:58:04.363935540+00:00');
INSERT INTO "schema_versions" VALUES(2026070802,'agent_checkpoint_paging','2026-09-14T10:58:04.364645214+00:00');
INSERT INTO "schema_versions" VALUES(2026070803,'agent_audit_log','2026-09-14T10:58:04.365041593+00:00');
INSERT INTO "schema_versions" VALUES(2026071301,'agent_coverage_gate','2026-09-14T10:58:04.365469597+00:00');
INSERT INTO "schema_versions" VALUES(2026071401,'agent_export_job','2026-09-14T10:58:04.365978602+00:00');
INSERT INTO "schema_versions" VALUES(2026071402,'agent_import_identity','2026-09-14T10:58:04.366292647+00:00');
INSERT INTO "schema_versions" VALUES(2026071403,'agent_import_tombstone','2026-09-14T10:58:04.366474732+00:00');
INSERT INTO "schema_versions" VALUES(2026071404,'agent_tombstone_compat_barrier','2026-09-14T10:58:04.366751776+00:00');
INSERT INTO "schema_versions" VALUES(2026071405,'agent_coverage_conflict','2026-09-14T10:58:04.366989862+00:00');
INSERT INTO "schema_versions" VALUES(2026071406,'agent_subagent_content','2026-09-14T10:58:04.367141446+00:00');
INSERT INTO "schema_versions" VALUES(2026071407,'agent_subagent_replication','2026-09-14T10:58:04.367648410+00:00');
INSERT INTO "schema_versions" VALUES(2026071901,'sequencer_worktree_scope','2026-09-14T10:58:04.370279102+00:00');
INSERT INTO "schema_versions" VALUES(2026072101,'rebase_state_worktree_scope','2026-09-14T10:58:04.371799075+00:00');
INSERT INTO "schema_versions" VALUES(2026072201,'operation_worktree_scope','2026-09-14T10:58:04.373087712+00:00');
INSERT INTO "schema_versions" VALUES(2026072301,'bisect_state_worktree_scope','2026-09-14T10:58:04.373658343+00:00');
INSERT INTO "schema_versions" VALUES(2026072302,'working_dirty_worktree_scope','2026-09-14T10:58:04.375039189+00:00');
INSERT INTO "schema_versions" VALUES(2026072303,'layer_worktree_scope','2026-09-14T10:58:04.375460443+00:00');
INSERT INTO "schema_versions" VALUES(2026072304,'sparse_view_worktree_scope','2026-09-14T10:58:04.378278346+00:00');
INSERT INTO "schema_versions" VALUES(2026072401,'worktree_registry_v2','2026-09-14T10:58:04.379845319+00:00');
INSERT INTO "schema_versions" VALUES(2026072402,'worktree_lifecycle_journal','2026-09-14T10:58:04.380035988+00:00');
INSERT INTO "schema_versions" VALUES(2026072403,'worktree_migrate_intent','2026-09-14T10:58:04.380273448+00:00');
INSERT INTO "schema_versions" VALUES(2026072501,'workspace_record','2026-09-14T10:58:04.381849464+00:00');
INSERT INTO "schema_versions" VALUES(2026072502,'workspace_paging_index','2026-09-14T10:58:04.382200759+00:00');
INSERT INTO "schema_versions" VALUES(2026072901,'head_scope_unique','2026-09-14T10:58:04.382412386+00:00');
INSERT INTO "schema_versions" VALUES(2026072902,'operation_scope_provenance','2026-09-14T10:58:04.382757389+00:00');
INSERT INTO "schema_versions" VALUES(2026073001,'operation_args_digest_canonical','2026-09-14T10:58:04.383481438+00:00');
INSERT INTO "schema_versions" VALUES(2026073002,'operation_dedup_index','2026-09-14T10:58:04.383652898+00:00');
INSERT INTO "schema_versions" VALUES(2026073003,'operation_boundary_claim','2026-09-14T10:58:04.383865566+00:00');
INSERT INTO "schema_versions" VALUES(2026073004,'operation_scope_kind','2026-09-14T10:58:04.385332330+00:00');
INSERT INTO "schema_versions" VALUES(2026073005,'worktree_registry_v3_capability','2026-09-14T10:58:04.386014545+00:00');
INSERT INTO "schema_versions" VALUES(2026073101,'stash_generation_fence','2026-09-14T10:58:04.386173880+00:00');
INSERT INTO "schema_versions" VALUES(2026080401,'agent_capture_workspace_scope','2026-09-14T10:58:04.386351424+00:00');
INSERT INTO "schema_versions" VALUES(2026080402,'agent_usage_runtime_attribution','2026-09-14T10:58:04.393282324+00:00');
INSERT INTO "schema_versions" VALUES(2026080403,'agent_usage_event_session_scope','2026-09-14T10:58:04.395212759+00:00');
INSERT INTO "schema_versions" VALUES(2026081301,'approved_permission_provenance','2026-09-14T10:58:04.395567304+00:00');
INSERT INTO "schema_versions" VALUES(2026081801,'agent_bridge_capture','2026-09-14T10:58:04.397480365+00:00');
INSERT INTO "schema_versions" VALUES(2026082401,'agent_bridge_link_relations','2026-09-14T10:58:04.398157329+00:00');
INSERT INTO "schema_versions" VALUES(2026090101,'operation_v2','2026-09-14T10:58:04.400710354+00:00');
INSERT INTO "schema_versions" VALUES(2026090601,'legacy_config_table','2026-09-14T10:58:04.414694573+00:00');
INSERT INTO "schema_versions" VALUES(2026090801,'operation_v2_branch_convergence','2026-09-14T10:58:04.415219078+00:00');
CREATE TABLE `sequence_state` (
    `worktree_id` TEXT NOT NULL PRIMARY KEY,
    `kind`        TEXT NOT NULL,
    `head_name`   TEXT NOT NULL,
    `head_orig`   TEXT NOT NULL,
    `current_oid` TEXT NOT NULL,
    `todo`        TEXT NOT NULL,
    `payload`     TEXT NOT NULL DEFAULT '',
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE `source_call_log` (
    `id` TEXT PRIMARY KEY,
    `session_id` TEXT NOT NULL,
    `source_slug` TEXT NOT NULL,
    `tool_name` TEXT NOT NULL,
    `registered_tool_name` TEXT NOT NULL,
    `tool_call_id` TEXT NOT NULL,
    `credential_ref` TEXT,
    `latency_ms` INTEGER,
    `input_bytes` INTEGER NOT NULL DEFAULT 0,
    `output_bytes` INTEGER NOT NULL DEFAULT 0,
    `cost_estimate_micros` INTEGER,
    `approval_decision` TEXT,
    `state_namespace` TEXT NOT NULL,
    `success` INTEGER NOT NULL DEFAULT 1,
    `created_at` TEXT NOT NULL
, `agent_run_id` TEXT);
CREATE TABLE `sparse_view` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id` TEXT NOT NULL DEFAULT '',
    `pattern`     TEXT NOT NULL,
    `ordinal`     INTEGER NOT NULL,
    UNIQUE(`worktree_id`, `ordinal`)
);
CREATE TABLE `sparse_view_meta` (
    `worktree_id` TEXT PRIMARY KEY NOT NULL,
    `enabled`     INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE `working_dirty` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id` TEXT NOT NULL DEFAULT '',
    `path`        TEXT NOT NULL,
    `kind`        TEXT NOT NULL DEFAULT 'unknown',
    `source`      TEXT NOT NULL,
    `marked_at`   TEXT NOT NULL,
    `verified_at` TEXT,
    UNIQUE(`worktree_id`, `path`, `kind`)
);
CREATE TABLE `working_dirty_meta` (
    `worktree_id`       TEXT PRIMARY KEY NOT NULL,
    `state`             TEXT NOT NULL DEFAULT 'stale',
    `index_fingerprint` TEXT,
    `head_oid`          TEXT,
    `scanned_at`        TEXT,
    `scan_lock_pid`     INTEGER,
    `scan_lock_at`      TEXT
);
CREATE TABLE `workspace_record` (
    `workspace_id` TEXT NOT NULL PRIMARY KEY,
    `repo_id` TEXT NOT NULL,
    `kind` TEXT NOT NULL CHECK (`kind` IN ('linked', 'task_copy', 'task_fuse', 'remote')),
    `worktree_id` TEXT,
    `path` TEXT NOT NULL,
    `owner_kind` TEXT NOT NULL CHECK (`owner_kind` IN ('human', 'agent', 'automation')),
    `owner_id` TEXT,
    `task_id` TEXT,
    `session_id` TEXT,
    `base_commit` TEXT,
    `branch` TEXT,
    `state` TEXT NOT NULL CHECK (
        `state` IN ('provisioning', 'active', 'releasing', 'released', 'orphaned')
    ),
    `lease_owner` TEXT,
    `lease_fence` INTEGER NOT NULL DEFAULT 0,
    `lease_expires_at` INTEGER,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL
);
CREATE TABLE `worktree_intent_journal` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `op` TEXT NOT NULL CHECK (`op` IN ('add', 'move', 'remove', 'prune', 'migrate')),
    `worktree_id` TEXT,
    `payload` TEXT NOT NULL,
    `stage` TEXT,
    `created_at` INTEGER NOT NULL
);
CREATE TABLE `worktree_lifecycle` (
    `worktree_id` TEXT NOT NULL PRIMARY KEY,
    `state` TEXT NOT NULL CHECK (`state` IN ('detached_from_registry', 'tombstone')),
    `path` TEXT NOT NULL,
    `reason` TEXT,
    `created_at` INTEGER NOT NULL,
    `updated_at` INTEGER NOT NULL
);
CREATE TABLE `worktree_registry_capability` (
    `version` INTEGER PRIMARY KEY
);
INSERT INTO "worktree_registry_capability" VALUES(2);
INSERT INTO "worktree_registry_capability" VALUES(3);
CREATE INDEX idx_config_kv_key ON config_kv(`key`);
CREATE UNIQUE INDEX idx_name_kind_remote ON `reference`(`name`, `kind`, `remote`)
WHERE `remote` IS NOT NULL;
CREATE UNIQUE INDEX idx_name_kind ON `reference`(`name`, `kind`)
WHERE `remote` IS NULL;
CREATE INDEX idx_ref_name_timestamp ON `reflog`(`ref_name`, `timestamp`);
CREATE UNIQUE INDEX idx_object_repo_oid ON `object_index`(`repo_id`, `o_id`);
CREATE INDEX idx_object_sync ON `object_index`(`repo_id`, `is_synced`);
CREATE INDEX idx_ai_thread_latest_intent ON `ai_thread`(`latest_intent_id`);
CREATE INDEX idx_ai_thread_current_intent ON `ai_thread`(`current_intent_id`);
CREATE INDEX idx_ai_thread_archived_updated ON `ai_thread`(`archived`, `updated_at`);
CREATE INDEX idx_ai_thread_participant_actor
    ON `ai_thread_participant`(`actor_kind`, `actor_id`);
CREATE UNIQUE INDEX idx_ai_thread_intent_thread_ordinal
    ON `ai_thread_intent`(`thread_id`, `ordinal`);
CREATE UNIQUE INDEX uq_ai_thread_intent_intent
    ON `ai_thread_intent`(`intent_id`);
CREATE INDEX idx_ai_thread_intent_head
    ON `ai_thread_intent`(`thread_id`, `is_head`);
CREATE INDEX idx_ai_scheduler_selected_plan
    ON `ai_scheduler_state`(`selected_plan_id`);
CREATE INDEX idx_ai_scheduler_active_task
    ON `ai_scheduler_state`(`active_task_id`);
CREATE INDEX idx_ai_scheduler_active_run
    ON `ai_scheduler_state`(`active_run_id`);
CREATE UNIQUE INDEX idx_ai_scheduler_plan_head_thread_ordinal
    ON `ai_scheduler_plan_head`(`thread_id`, `ordinal`);
CREATE UNIQUE INDEX idx_ai_scheduler_selected_plan_thread_ordinal
    ON `ai_scheduler_selected_plan`(`thread_id`, `ordinal`);
CREATE INDEX idx_ai_scheduler_selected_plan_plan
    ON `ai_scheduler_selected_plan`(`plan_id`);
CREATE UNIQUE INDEX idx_ai_live_context_window_thread_position
    ON `ai_live_context_window`(`thread_id`, `position`);
CREATE INDEX idx_ai_live_context_window_frame
    ON `ai_live_context_window`(`context_frame_id`);
CREATE INDEX idx_ai_index_intent_task_parent
    ON `ai_index_intent_task`(`parent_task_id`);
CREATE INDEX idx_ai_index_plan_step_task_step
    ON `ai_index_plan_step_task`(`plan_id`, `step_id`);
CREATE INDEX idx_ai_index_task_run_task_created
    ON `ai_index_task_run`(`task_id`, `created_at`);
CREATE UNIQUE INDEX idx_ai_index_task_run_latest
    ON `ai_index_task_run`(`task_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_index_run_event_run_created
    ON `ai_index_run_event`(`run_id`, `created_at`);
CREATE UNIQUE INDEX idx_ai_index_run_event_latest
    ON `ai_index_run_event`(`run_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_index_run_patchset_run_sequence
    ON `ai_index_run_patchset`(`run_id`, `sequence`);
CREATE UNIQUE INDEX idx_ai_index_run_patchset_latest
    ON `ai_index_run_patchset`(`run_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_index_intent_context_frame_relation
    ON `ai_index_intent_context_frame`(`intent_id`, `relation_kind`);
CREATE INDEX idx_ai_validation_report_thread_created
    ON `ai_validation_report`(`thread_id`, `created_at`);
CREATE UNIQUE INDEX idx_ai_validation_report_latest
    ON `ai_validation_report`(`thread_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_risk_score_breakdown_thread_created
    ON `ai_risk_score_breakdown`(`thread_id`, `created_at`);
CREATE UNIQUE INDEX idx_ai_risk_score_breakdown_latest
    ON `ai_risk_score_breakdown`(`thread_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_decision_proposal_thread_created
    ON `ai_decision_proposal`(`thread_id`, `created_at`);
CREATE UNIQUE INDEX idx_ai_decision_proposal_latest
    ON `ai_decision_proposal`(`thread_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_final_decision_thread_created
    ON `ai_final_decision`(`thread_id`, `created_at`);
CREATE UNIQUE INDEX idx_ai_final_decision_latest
    ON `ai_final_decision`(`thread_id`) WHERE `is_latest` = 1;
CREATE INDEX idx_ai_thread_provider_metadata_legacy_session
    ON `ai_thread_provider_metadata`(`legacy_session_id`);
CREATE INDEX idx_ai_thread_provider_metadata_provider_thread
    ON `ai_thread_provider_metadata`(`provider_thread_id`);
CREATE INDEX `idx_automation_log_finished_at`
    ON `automation_log` (`finished_at`);
CREATE INDEX `idx_automation_log_rule_id`
    ON `automation_log` (`rule_id`);
CREATE INDEX `idx_agent_usage_stats_provider_model`
    ON `agent_usage_stats` (`provider`, `model`);
CREATE INDEX `idx_agent_usage_stats_thread`
    ON `agent_usage_stats` (`thread_id`);
CREATE INDEX `idx_agent_usage_stats_session`
    ON `agent_usage_stats` (`session_id`);
CREATE INDEX `idx_agent_usage_stats_started`
    ON `agent_usage_stats` (`started_at`);
CREATE UNIQUE INDEX `idx_agent_session_provider`
    ON `agent_session`(`agent_kind`, `provider_session_id`);
CREATE INDEX `idx_agent_session_active`
    ON `agent_session`(`state`, `working_dir`) WHERE `state` = 'active';
CREATE INDEX `idx_agent_session_thread`
    ON `agent_session`(`thread_id`);
CREATE INDEX `idx_agent_checkpoint_session`
    ON `agent_checkpoint`(`session_id`, `created_at`);
CREATE INDEX `idx_agent_checkpoint_scope`
    ON `agent_checkpoint`(`scope`);
CREATE INDEX `idx_approved_permission_project`
    ON `approved_permission` (`project_id`, `created_at`);
CREATE INDEX `idx_agent_usage_stats_agent_name_provider_model`
    ON `agent_usage_stats` (`agent_name`, `provider`, `model`);
CREATE INDEX `idx_source_call_log_session`
    ON `source_call_log` (`session_id`);
CREATE INDEX `idx_source_call_log_source_slug`
    ON `source_call_log` (`source_slug`);
CREATE INDEX `idx_source_call_log_tool_call_id`
    ON `source_call_log` (`tool_call_id`);
CREATE INDEX `idx_source_call_log_created`
    ON `source_call_log` (`created_at`);
CREATE INDEX `idx_source_call_log_agent_run_id`
    ON `source_call_log` (`agent_run_id`);
CREATE INDEX idx_notes_ref ON `notes`(`notes_ref`);
CREATE INDEX `idx_object_obliteration_oid` ON `object_obliteration` (`oid`);
CREATE INDEX idx_reference_head_worktree
    ON `reference`(`kind`, `worktree_id`) WHERE `remote` IS NULL;
CREATE INDEX idx_reflog_worktree
    ON `reflog`(`ref_name`, `worktree_id`, `timestamp`);
CREATE INDEX `idx_agent_checkpoint_traces_commit`
    ON `agent_checkpoint`(`traces_commit`);
CREATE INDEX `idx_agent_session_started_paging`
    ON `agent_session`(`started_at` DESC, `session_id`);
CREATE INDEX `idx_agent_checkpoint_created_paging`
    ON `agent_checkpoint`(`created_at` DESC, `checkpoint_id`);
CREATE INDEX idx_agent_audit_log_timestamp
    ON agent_audit_log (timestamp);
CREATE INDEX idx_agent_audit_log_checkpoint
    ON agent_audit_log (checkpoint_id);
CREATE TRIGGER agent_audit_log_no_update
    BEFORE UPDATE ON agent_audit_log
    FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent_audit_log is append-only: UPDATE is not permitted');
END;
CREATE TRIGGER agent_audit_log_no_delete
    BEFORE DELETE ON agent_audit_log
    FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'agent_audit_log is append-only: DELETE is not permitted');
END;
CREATE UNIQUE INDEX `idx_agent_coverage_claim_logical_key`
    ON `agent_coverage_claim`(`session_id`, `logical_turn_key`, `coverage_schema_version`);
CREATE INDEX `idx_agent_coverage_claim_session_state`
    ON `agent_coverage_claim`(`session_id`, `state`);
CREATE INDEX `idx_agent_coverage_claim_checkpoint_id`
    ON `agent_coverage_claim`(`checkpoint_id`);
CREATE INDEX `idx_agent_coverage_revision_checkpoint_id`
    ON `agent_coverage_revision`(`checkpoint_id`);
CREATE UNIQUE INDEX `idx_agent_export_job_session`
    ON `agent_export_job`(`agent_kind`, `provider_session_id`);
CREATE INDEX `idx_agent_export_job_ttl`
    ON `agent_export_job`(`ttl_expires_at`);
CREATE UNIQUE INDEX `idx_agent_import_identity_key`
    ON `agent_import_identity`(
        `agent_kind`, `provider_session_id`, `source_kind`, `source_id`, `schema_version`
    );
CREATE UNIQUE INDEX `idx_agent_import_tombstone_provider`
    ON `agent_import_tombstone`(`agent_kind`, `provider_session_id`);
CREATE UNIQUE INDEX `idx_agent_import_tombstone_erased_session`
    ON `agent_import_tombstone`(`erased_session_id`);
CREATE TRIGGER `agent_tombstone_block_session_insert`
BEFORE INSERT ON `agent_session`
WHEN EXISTS (
    SELECT 1 FROM `agent_import_tombstone`
    WHERE `agent_kind` = NEW.`agent_kind`
      AND `provider_session_id` = NEW.`provider_session_id`
)
BEGIN
    SELECT RAISE(ABORT, 'agent session is protected by an erasure tombstone');
END;
CREATE TRIGGER `agent_tombstone_block_session_update`
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
CREATE TRIGGER `agent_tombstone_block_checkpoint_insert`
BEFORE INSERT ON `agent_checkpoint`
WHEN EXISTS (
    SELECT 1 FROM `agent_import_tombstone`
    WHERE `erased_session_id` = NEW.`session_id`
)
BEGIN
    SELECT RAISE(ABORT, 'agent checkpoint is protected by an erasure tombstone');
END;
CREATE INDEX `idx_agent_coverage_conflict_observed_at`
    ON `agent_coverage_conflict`(`incoming_observed_at`);
CREATE INDEX `idx_agent_subagent_content_claim_current`
    ON `agent_subagent_content_claim`(`current_checkpoint_id`);
CREATE INDEX `idx_agent_subagent_content_claim_state`
    ON `agent_subagent_content_claim`(`state`, `lease_expires_at`);
CREATE INDEX `idx_agent_subagent_content_revision_source`
    ON `agent_subagent_content_revision`(
        `parent_session_id`, `provider_kind`, `source_key`, `revision`
    );
CREATE INDEX `idx_agent_subagent_link_parent_state`
    ON `agent_subagent_link`(`parent_session_id`, `link_state`);
CREATE INDEX `idx_agent_subagent_link_boundary`
    ON `agent_subagent_link`(`boundary_checkpoint_id`);
CREATE INDEX `idx_agent_checkpoint_prune_tombstone_session`
    ON `agent_checkpoint_prune_tombstone`(`session_id`, `pruned_at`);
CREATE TRIGGER `trg_agent_subagent_boundary_delete`
BEFORE DELETE ON `agent_checkpoint`
BEGIN
    UPDATE `agent_subagent_link`
       SET `link_state` = 'unresolved',
           `boundary_checkpoint_id` = NULL,
           `sync_revision` = `sync_revision` + 1,
           `updated_at` = CAST(strftime('%s', 'now') AS INTEGER) * 1000
     WHERE `boundary_checkpoint_id` = OLD.`checkpoint_id`;
END;
CREATE UNIQUE INDEX `idx_workspace_linked_live`
ON `workspace_record` (`repo_id`, `worktree_id`)
WHERE `kind` = 'linked' AND `state` IN ('provisioning', 'active', 'releasing');
CREATE UNIQUE INDEX `idx_workspace_active_path`
ON `workspace_record` (`repo_id`, `path`)
WHERE `state` IN ('provisioning', 'active', 'releasing');
CREATE INDEX `idx_workspace_state`
ON `workspace_record` (`repo_id`, `state`);
CREATE INDEX `idx_workspace_repo_paging`
ON `workspace_record` (`repo_id`, `workspace_id`);
CREATE UNIQUE INDEX `idx_reference_head_scope_unique`
    ON `reference` (`worktree_id`)
    WHERE `kind` = 'Head' AND `remote` IS NULL AND `worktree_id` IS NOT NULL;
CREATE UNIQUE INDEX `idx_reference_head_main_unique`
    ON `reference` ((1))
    WHERE `kind` = 'Head' AND `remote` IS NULL AND `worktree_id` IS NULL;
CREATE INDEX `idx_agent_session_workspace_scope`
    ON `agent_session`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';
CREATE INDEX `idx_agent_export_job_workspace_scope`
    ON `agent_export_job`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';
CREATE INDEX `idx_agent_import_identity_workspace_scope`
    ON `agent_import_identity`(
        `repo_id`, `worktree_id`, `workspace_id`, `agent_kind`, `provider_session_id`
    ) WHERE `scope_state` = 'scoped';
CREATE INDEX `idx_agent_session_capture_provider`
    ON `agent_session`(`provider_session_id`);
CREATE INDEX `idx_agent_export_job_capture_provider`
    ON `agent_export_job`(`provider_session_id`);
CREATE INDEX `idx_agent_import_identity_capture_provider`
    ON `agent_import_identity`(`provider_session_id`);
CREATE TRIGGER `agent_session_scope_guard_insert`
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
CREATE TRIGGER `agent_session_scope_guard_update`
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
CREATE TRIGGER `agent_export_job_scope_guard_insert`
BEFORE INSERT ON `agent_export_job`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent export job is missing repository, worktree, or workspace fence');
END;
CREATE TRIGGER `agent_export_job_scope_guard_update`
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
CREATE TRIGGER `agent_import_identity_scope_guard_insert`
BEFORE INSERT ON `agent_import_identity`
WHEN NEW.`scope_state` = 'scoped'
 AND (NEW.`repo_id` IS NULL OR NEW.`repo_id` = '' OR NEW.`worktree_id` IS NULL
      OR (NEW.`workspace_id` IS NULL AND NEW.`workspace_fence` IS NOT NULL)
      OR (NEW.`workspace_id` IS NOT NULL
          AND (NEW.`workspace_id` = '' OR NEW.`workspace_fence` IS NULL)))
BEGIN
    SELECT RAISE(ABORT, 'scoped agent import identity is missing repository, worktree, or workspace fence');
END;
CREATE TRIGGER `agent_import_identity_scope_guard_update`
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
CREATE TRIGGER `agent_session_scope_immutable`
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
CREATE TRIGGER `agent_export_job_scope_immutable`
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
CREATE TRIGGER `agent_import_identity_scope_immutable`
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
CREATE INDEX `idx_agent_workspace_scope_audit_session`
    ON `agent_workspace_scope_audit`(`agent_kind`, `provider_session_id`, `created_at`);
CREATE TRIGGER `agent_workspace_scope_audit_append_only_update`
BEFORE UPDATE ON `agent_workspace_scope_audit`
BEGIN
    SELECT RAISE(ABORT, 'agent workspace scope audit is append-only');
END;
CREATE TRIGGER `agent_workspace_scope_audit_append_only_delete`
BEFORE DELETE ON `agent_workspace_scope_audit`
BEGIN
    SELECT RAISE(ABORT, 'agent workspace scope audit is append-only');
END;
CREATE INDEX `idx_agent_usage_stats_repo_session`
    ON `agent_usage_stats` (`repo_id`, `session_id`);
CREATE INDEX `idx_agent_usage_stats_turn`
    ON `agent_usage_stats` (`turn_id`);
CREATE INDEX `idx_agent_usage_stats_agent_run`
    ON `agent_usage_stats` (`agent_run_id`);
CREATE UNIQUE INDEX `idx_agent_usage_stats_event_id`
    ON `agent_usage_stats` (`session_id`, `event_id`);
CREATE INDEX `idx_agent_bridge_event_session_seq`
    ON `agent_bridge_event`(`bridge_session_id`, `event_seq`);
CREATE INDEX `idx_agent_bridge_event_operation`
    ON `agent_bridge_event`(`operation_id`) WHERE `operation_id` IS NOT NULL;
CREATE INDEX `idx_agent_bridge_checkpoint_session`
    ON `agent_bridge_checkpoint`(`bridge_session_id`, `created_at`);
CREATE INDEX `idx_agent_bridge_link_target`
    ON `agent_bridge_link`(`target_type`, `target_id`);
CREATE INDEX `idx_agent_bridge_link_source`
    ON `agent_bridge_link`(`source_type`, `source_id`);
CREATE INDEX idx_legacy_operation_repo_order
       ON legacy_operation(repo_id, end_ts DESC, start_ts DESC, op_id DESC);
CREATE INDEX idx_legacy_operation_dedup_scope
       ON legacy_operation(repo_id, worktree_id, command_name, args_digest, status, end_ts);
CREATE UNIQUE INDEX idx_legacy_operation_control_slot
       ON legacy_operation(repo_id, worktree_id)
       WHERE status = 'running' AND control_slot IS NOT NULL;
CREATE TRIGGER legacy_operation_scope_provenance_domain_insert
       BEFORE INSERT ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_provenance NOT IN ('declared', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_provenance must be either declared or unknown');
       END;
CREATE TRIGGER legacy_operation_scope_provenance_domain_update
       BEFORE UPDATE OF scope_provenance ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_provenance NOT IN ('declared', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_provenance must be either declared or unknown');
       END;
CREATE TRIGGER legacy_operation_scope_kind_domain_insert
       BEFORE INSERT ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_kind NOT IN ('main', 'linked', 'repository', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_kind must be main, linked, repository or unknown');
       END;
CREATE TRIGGER legacy_operation_scope_kind_domain_update
       BEFORE UPDATE OF scope_kind ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_kind NOT IN ('main', 'linked', 'repository', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_kind must be main, linked, repository or unknown');
       END;
CREATE INDEX idx_legacy_operation_parent_parent
       ON legacy_operation_parent(parent_op_id, op_id);
CREATE INDEX idx_legacy_operation_view_repo_created
       ON legacy_operation_view(repo_id, created_at DESC);
CREATE INDEX `idx_operation_v2_repo_order`
     ON `operation`(`repo_id`, `end_ts` DESC, `start_ts` DESC, `op_id` DESC);
CREATE INDEX `idx_operation_parent_v2_parent`
     ON `operation_parent`(`parent_op_id`, `op_id`);
CREATE INDEX `idx_operation_head_v2_scope_generation`
     ON `operation_head`(`repo_id`, `scope_key`, `generation` DESC, `op_id`);
CREATE INDEX `idx_operation_journal_v2_op`
     ON `operation_journal`(`op_id`, `updated_at` DESC);
CREATE INDEX `idx_change_revision_v2_commit`
     ON `change_revision`(`commit_oid`);
DELETE FROM "sqlite_sequence";
INSERT INTO "sqlite_sequence" VALUES('layer',0);
INSERT INTO "sqlite_sequence" VALUES('layer_path',0);
INSERT INTO "sqlite_sequence" VALUES('sparse_view',0);
INSERT INTO "sqlite_sequence" VALUES('worktree_intent_journal',0);
INSERT INTO "sqlite_sequence" VALUES('metadata_kv',1);
INSERT INTO "sqlite_sequence" VALUES('agent_bridge_link',0);
INSERT INTO "sqlite_sequence" VALUES('config_kv',1);
COMMIT;
