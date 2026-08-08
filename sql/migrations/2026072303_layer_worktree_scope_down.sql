-- Rollback of 2026072303_layer_worktree_scope.
--
-- Restores the repository-global shapes. The old schema cannot represent a
-- linked worktree's layer registrations or path ownership, and dropping them
-- would make materialized overlay files committable — so this rollback FAILS
-- CLOSED while any linked-scope row exists (§C.4.1.1: down migrations must
-- not silently discard linked state; `layer unapply`/`layer remove` from the
-- owning worktree first). The guards below violate their CHECK constraints
-- when linked rows are present.

CREATE TABLE `layer__down_guard_2026072303` (
    `linked_rows` INTEGER NOT NULL CHECK (`linked_rows` = 0)
);
INSERT INTO `layer__down_guard_2026072303` (`linked_rows`)
SELECT COUNT(*) FROM `layer` WHERE `worktree_id` <> '';
INSERT INTO `layer__down_guard_2026072303` (`linked_rows`)
SELECT COUNT(*) FROM `layer_path` WHERE `worktree_id` <> '';
DROP TABLE `layer__down_guard_2026072303`;

ALTER TABLE `layer` RENAME TO `layer__new_2026072303`;
CREATE TABLE `layer` (
    `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
    `name`       TEXT NOT NULL UNIQUE,
    `source`     TEXT NOT NULL,
    `priority`   INTEGER NOT NULL DEFAULT 0,
    `enabled`    INTEGER NOT NULL DEFAULT 1,
    `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO `layer` (`id`, `name`, `source`, `priority`, `enabled`, `created_at`, `updated_at`)
SELECT `id`, `name`, `source`, `priority`, `enabled`, `created_at`, `updated_at`
FROM `layer__new_2026072303`
WHERE `worktree_id` = '';
DROP TABLE `layer__new_2026072303`;

ALTER TABLE `layer_path` RENAME TO `layer_path__new_2026072303`;
CREATE TABLE `layer_path` (
    `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
    `layer_name`      TEXT NOT NULL,
    `path`            TEXT NOT NULL UNIQUE,
    `content_hash`    TEXT NOT NULL,
    `materialized_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO `layer_path` (`id`, `layer_name`, `path`, `content_hash`, `materialized_at`)
SELECT `id`, `layer_name`, `path`, `content_hash`, `materialized_at`
FROM `layer_path__new_2026072303`
WHERE `worktree_id` = '';
DROP TABLE `layer_path__new_2026072303`;
