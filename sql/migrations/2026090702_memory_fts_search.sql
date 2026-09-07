-- M2-02F: rebuildable Episode search document plus external-content FTS5.
--
-- The ordinary table owns the only copy of searchable text. The virtual table
-- stores postings only and is maintained by internal::ai::memory::fts_sql in
-- the same caller-owned transaction as the content row.

CREATE TABLE IF NOT EXISTS `memory_episode_search_doc` (
    `rowid` INTEGER PRIMARY KEY,
    `note_id` TEXT NOT NULL CHECK (
        length(`note_id`) = 36
        AND substr(`note_id`, 9, 1) = '-'
        AND substr(`note_id`, 14, 1) = '-'
        AND substr(`note_id`, 19, 1) = '-'
        AND substr(`note_id`, 24, 1) = '-'
        AND length(replace(`note_id`, '-', '')) = 32
        AND replace(`note_id`, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    `revision_oid` TEXT NOT NULL CHECK (
        length(`revision_oid`) IN (40,64)
        AND `revision_oid` NOT GLOB '*[^0-9a-f]*'
    ),
    `root_kind` TEXT NOT NULL CHECK (`root_kind` IN ('task','intent')),
    `root_id` TEXT NOT NULL CHECK (
        length(`root_id`) > 0
        AND length(CAST(`root_id` AS BLOB)) <= 120
    ),
    `completion_status` TEXT NOT NULL CHECK (
        `completion_status` IN ('completed','failed','cancelled')
    ),
    `code_change_status` TEXT NOT NULL CHECK (
        `code_change_status` IN ('changed','unchanged','unknown')
    ),
    `ended_at` TEXT CHECK (
        `ended_at` IS NULL
        OR length(CAST(`ended_at` AS BLOB)) BETWEEN 1 AND 64
    ),
    `goal` TEXT NOT NULL,
    `summary` TEXT NOT NULL,
    `decisions` TEXT NOT NULL,
    `failed_attempts` TEXT NOT NULL,
    `unresolved` TEXT NOT NULL,
    UNIQUE (`note_id`, `revision_oid`),
    CHECK (
        length(CAST(`goal` AS BLOB))
        + length(CAST(`summary` AS BLOB))
        + length(CAST(`decisions` AS BLOB))
        + length(CAST(`failed_attempts` AS BLOB))
        + length(CAST(`unresolved` AS BLOB)) <= 65536
    ),
    FOREIGN KEY (`note_id`, `revision_oid`)
        REFERENCES `memory_revision_index`(`note_id`, `revision_oid`)
        ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS `idx_memory_episode_search_root`
    ON `memory_episode_search_doc`(
        `root_kind`, `root_id`, `ended_at`, `note_id`, `revision_oid`
    );
CREATE INDEX IF NOT EXISTS `idx_memory_episode_search_filters`
    ON `memory_episode_search_doc`(
        `completion_status`, `code_change_status`, `ended_at`, `note_id`, `revision_oid`
    );

CREATE VIRTUAL TABLE IF NOT EXISTS `memory_episode_fts` USING fts5(
    `goal`,
    `summary`,
    `decisions`,
    `failed_attempts`,
    `unresolved`,
    content='memory_episode_search_doc',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
