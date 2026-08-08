-- 2026072302_working_dirty_worktree_scope
--
-- plan-20260714 Part C W1 (§C.4.1.1): scope the dirty-set advisory cache per
-- worktree. `working_dirty` gains `worktree_id` with UNIQUE(worktree_id,
-- path, kind); `working_dirty_meta` drops the `id = 1` repository singleton
-- and is re-keyed to `worktree_id TEXT PRIMARY KEY` — fingerprint/HEAD/lock
-- describe ONE scope only.
--
-- Legacy rows are NOT copied: the dirty cache is REBUILDABLE advisory state,
-- and §C.4.1.1's migration rule for it is "mark the whole group stale and
-- clear, let each worktree rescan — never guess the owner". Clearing is the
-- safe universal branch (repos without linked evidence merely pay one
-- bounded rescan). Scan locks are transient and cleared with the rows.

CREATE TABLE IF NOT EXISTS `working_dirty` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `path`        TEXT NOT NULL,
    `kind`        TEXT NOT NULL DEFAULT 'unknown',
    `source`      TEXT NOT NULL,
    `marked_at`   TEXT NOT NULL,
    `verified_at` TEXT,
    UNIQUE(`path`, `kind`)
);
CREATE TABLE IF NOT EXISTS `working_dirty_meta` (
    `id`                INTEGER PRIMARY KEY CHECK (`id` = 1),
    `state`             TEXT NOT NULL DEFAULT 'stale',
    `index_fingerprint` TEXT,
    `head_oid`          TEXT,
    `scanned_at`        TEXT,
    `scan_lock_pid`     INTEGER,
    `scan_lock_at`      TEXT
);

DROP TABLE `working_dirty`;
CREATE TABLE `working_dirty` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `worktree_id` TEXT NOT NULL DEFAULT '',
    `path`        TEXT NOT NULL,
    `kind`        TEXT NOT NULL DEFAULT 'unknown',
    `source`      TEXT NOT NULL,
    `marked_at`   TEXT NOT NULL,
    `verified_at` TEXT,
    UNIQUE(`worktree_id`, `path`, `kind`)
);

DROP TABLE `working_dirty_meta`;
CREATE TABLE `working_dirty_meta` (
    `worktree_id`       TEXT PRIMARY KEY NOT NULL,
    `state`             TEXT NOT NULL DEFAULT 'stale',
    `index_fingerprint` TEXT,
    `head_oid`          TEXT,
    `scanned_at`        TEXT,
    `scan_lock_pid`     INTEGER,
    `scan_lock_at`      TEXT
);
