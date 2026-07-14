-- 2026071401_agent_export_job: OpenCode export-bridge job state
-- (plan-20260713 DR-04b, ADR-DR-11, GC-DR-11/12).
--
-- One row per (agent_kind, provider_session_id): tracks the observed vs
-- processed generation counters that make per-idle `opencode export` runs
-- convergent — every idle bumps `observed_generation`; the lease-holding
-- runner exports, gates each turn through the coverage claim, then advances
-- `processed_generation` under its owner+fence. A crashed runner's work is
-- taken over after `lease_expires_at`; rows expire via `ttl_expires_at`
-- (clean --gc / retention / startup scavenging), NOT via session cascade —
-- export jobs may reference provider sessions that no longer exist locally.
--
-- Idempotency: every DDL statement uses `IF NOT EXISTS` (house style).

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
);

CREATE UNIQUE INDEX IF NOT EXISTS `idx_agent_export_job_session`
    ON `agent_export_job`(`agent_kind`, `provider_session_id`);
CREATE INDEX IF NOT EXISTS `idx_agent_export_job_ttl`
    ON `agent_export_job`(`ttl_expires_at`);
