-- Rollback of 2026082501_memory_core.
--
-- Every table in this migration must be empty. Once a projection or compiler
-- observation exists, repair moves forward; rollback must not erase it.

CREATE TABLE IF NOT EXISTS `memory_core_down_guard` (
    `blocked` INTEGER NOT NULL,
    CONSTRAINT `memory_core_down_guard_empty` CHECK (`blocked` = 0)
);

INSERT INTO `memory_core_down_guard` (`blocked`)
SELECT
    (SELECT COUNT(*) FROM `memory_compile_observer_state`)
    + (SELECT COUNT(*) FROM `memory_compile_job`)
    + (SELECT COUNT(*) FROM `memory_episode_path`)
    + (SELECT COUNT(*) FROM `memory_link_index`)
    + (SELECT COUNT(*) FROM `memory_head`)
    + (SELECT COUNT(*) FROM `memory_revision_index`)
    + (SELECT COUNT(*) FROM `memory_note_index`)
    + (SELECT COUNT(*) FROM `memory_projection_state`)
    + (SELECT COUNT(*) FROM `memory_path_summary`);

DROP TABLE `memory_core_down_guard`;

DROP TABLE IF EXISTS `memory_compile_observer_state`;
DROP INDEX IF EXISTS `idx_memory_compile_job_scope_generation`;
DROP INDEX IF EXISTS `idx_memory_compile_job_runnable`;
DROP TABLE IF EXISTS `memory_compile_job`;
DROP INDEX IF EXISTS `idx_memory_episode_path_code`;
DROP TABLE IF EXISTS `memory_episode_path`;
DROP INDEX IF EXISTS `idx_memory_link_target`;
DROP INDEX IF EXISTS `idx_memory_link_source`;
DROP TABLE IF EXISTS `memory_link_index`;
DROP INDEX IF EXISTS `idx_memory_head_path_prefix`;
DROP INDEX IF EXISTS `idx_memory_head_lookup`;
DROP TABLE IF EXISTS `memory_head`;
DROP INDEX IF EXISTS `idx_memory_revision_producer`;
DROP INDEX IF EXISTS `idx_memory_revision_note`;
DROP TABLE IF EXISTS `memory_revision_index`;
DROP INDEX IF EXISTS `idx_memory_note_idempotency_ns`;
DROP INDEX IF EXISTS `idx_memory_note_idempotency_cell`;
DROP TABLE IF EXISTS `memory_note_index`;
DROP TABLE IF EXISTS `memory_projection_state`;
DROP INDEX IF EXISTS `idx_memory_path_summary_prefix`;
DROP TABLE IF EXISTS `memory_path_summary`;
