-- Memory core projection schema down (plan-20260926 DM-01 migration A).
--
-- `forward-only` (ADR-DM-13): this down is a controlled test/ops tool, not the
-- published rollback path. Drop in FK-reverse order (children before parents):
-- `memory_episode_evidence` references `memory_episode` and must be dropped first.
DROP TABLE IF EXISTS `memory_episode_evidence`;
DROP TABLE IF EXISTS `memory_episode`;
DROP TABLE IF EXISTS `memory_projection_state`;
