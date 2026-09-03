-- Rollback of 2026082502_memory_fts_search.
--
-- An empty search projection can be removed. Once a document was indexed,
-- repair moves forward so rollback cannot silently discard search state.

INSERT INTO `memory_episode_fts`(`memory_episode_fts`, `rank`)
VALUES ('integrity-check', 1);

CREATE TABLE IF NOT EXISTS `memory_fts_search_down_guard` (
    `blocked` INTEGER NOT NULL,
    CONSTRAINT `memory_fts_search_down_guard_empty` CHECK (`blocked` = 0)
);

INSERT INTO `memory_fts_search_down_guard` (`blocked`)
SELECT COUNT(*) FROM `memory_episode_search_doc`;

DROP TABLE `memory_fts_search_down_guard`;

DROP TABLE IF EXISTS `memory_episode_fts`;
DROP INDEX IF EXISTS `idx_memory_episode_search_filters`;
DROP INDEX IF EXISTS `idx_memory_episode_search_root`;
DROP TABLE IF EXISTS `memory_episode_search_doc`;
