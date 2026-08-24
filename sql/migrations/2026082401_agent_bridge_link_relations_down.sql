-- Rollback of 2026082401_agent_bridge_link_relations (plan-20260818 LB-04/LB-05).
--
-- FORWARD-ONLY, matching `2026081801_agent_bridge_capture_down.sql`: the
-- association catalogue is acked provenance. The narrower pre-2026082401
-- shape (`UNIQUE(source_type, source_id)`) cannot hold the multi-edge graph
-- this migration enables, so a down on a database that already recorded links
-- would have to DELETE provenance to fit — exactly what ER-LB-04 forbids.
--
-- The down therefore FREEZES while any link row exists and only restores the
-- old shape on an empty catalogue. On a database with links, forward-migrate
-- to a compatible version instead.

CREATE TABLE IF NOT EXISTS `agent_bridge_link_relations_down_guard` (
    `blocked` INTEGER NOT NULL CHECK (`blocked` = 0)
);

INSERT INTO `agent_bridge_link_relations_down_guard` (`blocked`)
SELECT (SELECT COUNT(*) FROM `agent_bridge_link`);

-- Only reached when the guard passed (no link rows): restore the old shape.
DROP INDEX IF EXISTS `idx_agent_bridge_link_source`;
DROP INDEX IF EXISTS `idx_agent_bridge_link_target`;
DROP TABLE IF EXISTS `agent_bridge_link`;

CREATE TABLE `agent_bridge_link` (
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

DROP TABLE IF EXISTS `agent_bridge_link_relations_down_guard`;
