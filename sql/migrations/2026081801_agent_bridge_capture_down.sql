-- Rollback of 2026081801_agent_bridge_capture (plan-20260818 LB-02).
--
-- FORWARD-ONLY: the bridge durable projection is acked state. The down path
-- REFUSES while any bridge row exists (freeze) — it must NEVER delete acked
-- events, checkpoints, operations, evidence or audit rows as a rollback
-- shortcut (GC-LB-09 / ER-LB-04). On a database with bridge rows, the operator
-- must forward-migrate to a compatible version instead.
--
-- The freeze uses a guard table whose CHECK constraint fails when a bridge
-- row exists: SQLite aborts the statement and the enclosing migration
-- transaction rolls back, leaving the tables and rows intact.

CREATE TABLE IF NOT EXISTS `agent_bridge_capture_down_guard` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);

INSERT INTO `agent_bridge_capture_down_guard` (`blocked`)
SELECT
    (SELECT COUNT(*) FROM `agent_bridge_session`)
    + (SELECT COUNT(*) FROM `agent_bridge_event`)
    + (SELECT COUNT(*) FROM `agent_bridge_operation`)
    + (SELECT COUNT(*) FROM `agent_bridge_checkpoint`)
    + (SELECT COUNT(*) FROM `agent_bridge_link`);

-- Only reached when the guard passed (no bridge rows): drop the tables.
DROP TABLE IF EXISTS `agent_bridge_link`;
DROP TABLE IF EXISTS `agent_bridge_checkpoint`;
DROP TABLE IF EXISTS `agent_bridge_operation`;
DROP TABLE IF EXISTS `agent_bridge_event`;
DROP TABLE IF EXISTS `agent_bridge_session`;
DROP TABLE IF EXISTS `agent_bridge_capture_down_guard`;
