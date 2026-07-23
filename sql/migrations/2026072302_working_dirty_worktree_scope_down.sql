-- Rollback of 2026072302_working_dirty_worktree_scope.
--
-- Restores the repository-global shapes. The old schema cannot represent a
-- linked worktree's dirty rows or meta, so this rollback FAILS CLOSED while
-- any linked-scope row exists (§C.4.1.1 / §C.11: down migrations must not
-- silently discard linked state — clear or rescan-from-main first). The
-- guards below violate their CHECK constraints when linked rows are present.

CREATE TABLE `working_dirty__down_guard_2026072302` (
    `linked_rows` INTEGER NOT NULL CHECK (`linked_rows` = 0)
);
INSERT INTO `working_dirty__down_guard_2026072302` (`linked_rows`)
SELECT COUNT(*) FROM `working_dirty` WHERE `worktree_id` <> '';
INSERT INTO `working_dirty__down_guard_2026072302` (`linked_rows`)
SELECT COUNT(*) FROM `working_dirty_meta` WHERE `worktree_id` <> '';
DROP TABLE `working_dirty__down_guard_2026072302`;

ALTER TABLE `working_dirty` RENAME TO `working_dirty__new_2026072302`;
CREATE TABLE `working_dirty` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `path`        TEXT NOT NULL,
    `kind`        TEXT NOT NULL DEFAULT 'unknown',
    `source`      TEXT NOT NULL,
    `marked_at`   TEXT NOT NULL,
    `verified_at` TEXT,
    UNIQUE(`path`, `kind`)
);
INSERT INTO `working_dirty` (`path`, `kind`, `source`, `marked_at`, `verified_at`)
SELECT `path`, `kind`, `source`, `marked_at`, `verified_at`
FROM `working_dirty__new_2026072302`
WHERE `worktree_id` = '';
DROP TABLE `working_dirty__new_2026072302`;

ALTER TABLE `working_dirty_meta` RENAME TO `working_dirty_meta__new_2026072302`;
CREATE TABLE `working_dirty_meta` (
    `id`                INTEGER PRIMARY KEY CHECK (`id` = 1),
    `state`             TEXT NOT NULL DEFAULT 'stale',
    `index_fingerprint` TEXT,
    `head_oid`          TEXT,
    `scanned_at`        TEXT,
    `scan_lock_pid`     INTEGER,
    `scan_lock_at`      TEXT
);
INSERT INTO `working_dirty_meta`
    (`id`, `state`, `index_fingerprint`, `head_oid`, `scanned_at`,
     `scan_lock_pid`, `scan_lock_at`)
SELECT 1, `state`, `index_fingerprint`, `head_oid`, `scanned_at`,
       `scan_lock_pid`, `scan_lock_at`
FROM `working_dirty_meta__new_2026072302`
WHERE `worktree_id` = '';
DROP TABLE `working_dirty_meta__new_2026072302`;
