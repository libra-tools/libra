-- Memory path / search-document projection schema down (plan-20260926 DM-10 migration B).
--
-- `forward-only` (ADR-DM-13): this down is a controlled test/ops tool, not the
-- published rollback path. Drop in FK-reverse order (children before parents).
-- DM-08 will DROP the FTS5 virtual table (and its shadow tables) before it
-- drops this content table; here we only drop the content tables.
DROP TABLE IF EXISTS `memory_episode_search_doc`;
DROP TABLE IF EXISTS `memory_episode_path`;
