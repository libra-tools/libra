-- Rollback for 2026071401_agent_export_job.
--
-- Export-job rows are pure coordination state (generation counters and
-- leases); the exported content itself lives in checkpoints + coverage
-- claims, untouched here. Dropping the table returns to pre-bridge behavior
-- (dev/test rollback only, plan-20260713 GC-DR-05a).

DROP INDEX IF EXISTS `idx_agent_export_job_ttl`;
DROP INDEX IF EXISTS `idx_agent_export_job_session`;
DROP TABLE IF EXISTS `agent_export_job`;
