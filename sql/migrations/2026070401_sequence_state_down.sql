-- Reverse 2026070401: restore the pre-unification cherry_pick_state and the
-- revert_sequence orphan, carrying any in-progress cherry-pick back. Rolling
-- back ONLY this migration must leave both tables present (matching the
-- applied state of 2026060401 / 2026060801).
CREATE TABLE IF NOT EXISTS `cherry_pick_state` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `head_name`   TEXT NOT NULL,
    `head_orig`   TEXT NOT NULL,
    `current_oid` TEXT NOT NULL,
    `todo`        TEXT NOT NULL,
    `opts_json`   TEXT NOT NULL,
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO `cherry_pick_state`
    (`head_name`, `head_orig`, `current_oid`, `todo`, `opts_json`, `updated_at`)
SELECT `head_name`, `head_orig`, `current_oid`, `todo`, `payload`, `updated_at`
FROM `sequence_state`
WHERE `kind` = 'cherry_pick'
LIMIT 1;

-- Recreate the orphan so its 2026060801 applied-state is honored on rollback.
CREATE TABLE IF NOT EXISTS `revert_sequence` (
    `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `head_name`   TEXT NOT NULL,
    `head_orig`   TEXT NOT NULL,
    `current_oid` TEXT NOT NULL,
    `todo`        TEXT NOT NULL,
    `opts_json`   TEXT NOT NULL,
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

DROP TABLE IF EXISTS `sequence_state`;
