-- 2026081801_agent_bridge_capture: DeepSeek Harness bridge durable projection.
--
-- plan-20260818 LB-02 / ADR-LB-02 / ADR-LB-03. Backs `libra agent bridge --stdio`
-- (LB-01 transport + protocol authority). These tables hold ONLY the bounded,
-- redacted projection of Harness events — never raw prompt/reasoning/secrets
-- (GC-LB-08). `deepseek-harness` is a fixed source identity, NOT an `AgentKind`
-- in the provider roster (ADR-LB-02).
--
-- Idempotency: every DDL statement uses `IF NOT EXISTS`.

-- One bridge session per Harness session. Scope is derived from the trusted
-- handshake/context (GC-LB-06); the request body never overrides it.
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

-- One row per accepted event, keyed idempotently by (bridge_session_id, event_seq).
-- A replay with the SAME payload_sha256 is a success no-op; a replay with a
-- DIFFERENT digest at the same sequence is a conflict (fail-closed, no
-- last-writer-wins). Payload is bounded to the v1 256 KiB event cap.
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

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_event_session_seq`
    ON `agent_bridge_event`(`bridge_session_id`, `event_seq`);
CREATE INDEX IF NOT EXISTS `idx_agent_bridge_event_operation`
    ON `agent_bridge_event`(`operation_id`) WHERE `operation_id` IS NOT NULL;

-- One row per mutating bridge operation, idempotent by operation_id. A retry
-- with the SAME params_digest is a success no-op; a DIFFERENT digest is a
-- conflict (fail-closed, never re-runs a non-idempotent service).
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

-- Bridge-created checkpoints. `agent_checkpoint_id` is a soft link to the
-- existing `agent_checkpoint` catalog (ADR-LB-02: link, don't alias).
CREATE TABLE IF NOT EXISTS `agent_bridge_checkpoint` (
    `checkpoint_id`       TEXT PRIMARY KEY,
    `bridge_session_id`   TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    `agent_checkpoint_id` TEXT,
    `target_oid`          TEXT,
    `created_at`          INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_checkpoint_session`
    ON `agent_bridge_checkpoint`(`bridge_session_id`, `created_at`);

-- Association catalog connecting bridge sessions/events/operations/checkpoints
-- to existing AI/VCS objects. A stable (source_type, source_id) may only ever
-- point at one target (prevents duplicate association on replay).
CREATE TABLE IF NOT EXISTS `agent_bridge_link` (
    `link_id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `bridge_session_id` TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    `source_type`      TEXT NOT NULL CHECK(`source_type` IN ('event','operation','checkpoint','evidence','provenance')),
    `source_id`        TEXT NOT NULL,
    `target_type`      TEXT NOT NULL,
    `target_id`        TEXT NOT NULL,
    `created_at`       INTEGER NOT NULL,
    UNIQUE(`source_type`, `source_id`)
);

CREATE INDEX IF NOT EXISTS `idx_agent_bridge_link_target`
    ON `agent_bridge_link`(`target_type`, `target_id`);
