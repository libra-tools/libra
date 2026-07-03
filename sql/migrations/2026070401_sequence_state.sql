-- lore.md 2.6: unified sequencer state store. One row (CHECK(id=1)) holds the
-- single active multi-step sequence (cherry-pick in v1; merge/revert/rebase
-- migrate in scoped follow-ups). Replaces cherry-pick's lazy in-command DDL
-- and folds any in-progress cherry-pick forward transactionally; drops the
-- never-read `revert_sequence` orphan (a dead copy of cherry_pick_state).
CREATE TABLE IF NOT EXISTS `sequence_state` (
    `id`          INTEGER PRIMARY KEY CHECK (`id` = 1),
    `kind`        TEXT NOT NULL,
    `head_name`   TEXT NOT NULL,
    `head_orig`   TEXT NOT NULL,
    `current_oid` TEXT NOT NULL,
    `todo`        TEXT NOT NULL,
    `payload`     TEXT NOT NULL DEFAULT '',
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Carry any in-progress cherry-pick forward (single-row by construction).
INSERT OR IGNORE INTO `sequence_state`
    (`id`, `kind`, `head_name`, `head_orig`, `current_oid`, `todo`, `payload`, `updated_at`)
SELECT 1, 'cherry_pick', `head_name`, `head_orig`, `current_oid`, `todo`, `opts_json`, `updated_at`
FROM `cherry_pick_state`
LIMIT 1;

DROP TABLE IF EXISTS `cherry_pick_state`;
DROP TABLE IF EXISTS `revert_sequence`;
