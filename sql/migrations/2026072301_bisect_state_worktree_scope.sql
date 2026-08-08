-- 2026072301_bisect_state_worktree_scope
--
-- plan-20260714 Part C W1 (§C.4.2): retire the last sequencer-family lazy
-- DDL. `bisect_state`'s shape was owned by
-- `command/bisect.rs::ensure_bisect_state_table_exists` (`completed`,
-- `first_parent`, and `worktree_id` were `ADD COLUMN`ed on demand), so
-- databases in the wild carry any subset of those columns.
-- `normalize_bisect_state_shape` in `src/internal/db/migration.rs` runs
-- BEFORE the migration runner on every connection open and normalizes the
-- table to the full lazy column set, so this static rebuild may assume all
-- eleven columns exist. The lazy DDL itself is deleted in the same release —
-- from now on the schema is owned here.
--
-- Re-key: `worktree_id TEXT PRIMARY KEY NOT NULL` (main worktree = the EMPTY
-- STRING, same convention as 2026071901/2026072101). The behavioral contract
-- was already "at most one row per scope" (`save` now upserts the scope's
-- row in place), but the AUTOINCREMENT id never enforced it, so the
-- newest id per scope wins. Unlike `rebase_state`, linked-scope rows may
-- already exist in the wild (the lazy `worktree_id` shipped in v0.19.34), so
-- the copy groups by scope instead of assuming everything is main.

-- Self-provision for databases where the lazily-created table never
-- materialized (and for bare test runners that skip the normalize hook): the
-- rebuild below then starts from an empty full-shape table.
CREATE TABLE IF NOT EXISTS `bisect_state` (
    `id`             INTEGER PRIMARY KEY AUTOINCREMENT,
    `orig_head`      TEXT NOT NULL,
    `orig_head_name` TEXT,
    `bad`            TEXT,
    `good`           TEXT NOT NULL,
    `current`        TEXT,
    `skipped`        TEXT,
    `steps`          INTEGER,
    `completed`      INTEGER NOT NULL DEFAULT 0,
    `first_parent`   INTEGER NOT NULL DEFAULT 0,
    `worktree_id`    TEXT NOT NULL DEFAULT ''
);

ALTER TABLE `bisect_state` RENAME TO `bisect_state__old_2026072301`;

CREATE TABLE `bisect_state` (
    `worktree_id`    TEXT PRIMARY KEY NOT NULL,
    `orig_head`      TEXT NOT NULL,
    `orig_head_name` TEXT,
    `bad`            TEXT,
    `good`           TEXT NOT NULL,
    `current`        TEXT,
    `skipped`        TEXT,
    `steps`          INTEGER,
    `completed`      INTEGER NOT NULL DEFAULT 0,
    `first_parent`   INTEGER NOT NULL DEFAULT 0
);

-- Newest id per scope wins; older rows for the same scope are stale
-- leftovers the behavioral truncate-then-insert contract already treated as
-- dead.
INSERT INTO `bisect_state`
    (`worktree_id`, `orig_head`, `orig_head_name`, `bad`, `good`, `current`,
     `skipped`, `steps`, `completed`, `first_parent`)
SELECT `old`.`worktree_id`, `old`.`orig_head`, `old`.`orig_head_name`,
       `old`.`bad`, `old`.`good`, `old`.`current`, `old`.`skipped`,
       `old`.`steps`, `old`.`completed`, `old`.`first_parent`
FROM `bisect_state__old_2026072301` AS `old`
WHERE `old`.`id` = (
    SELECT MAX(`inner`.`id`)
    FROM `bisect_state__old_2026072301` AS `inner`
    WHERE `inner`.`worktree_id` = `old`.`worktree_id`
);

DROP TABLE `bisect_state__old_2026072301`;
