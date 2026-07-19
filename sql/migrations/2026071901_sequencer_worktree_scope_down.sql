-- Rollback of 2026071901_sequencer_worktree_scope.
--
-- Restores the repository-global single-row shape. Only the MAIN worktree's
-- row (`worktree_id = ''`) can survive: the old schema has no way to represent
-- a linked worktree's sequence, so rolling back while one is active would
-- silently drop it. Linked rows are therefore NOT copied — the operator is
-- expected to finish or abort linked-worktree sequences before rolling back
-- (Part C §C.11: down migration must not silently discard linked state).

-- ── sequence_state ──────────────────────────────────────────────────────────
ALTER TABLE `sequence_state` RENAME TO `sequence_state__new_2026071901`;

CREATE TABLE `sequence_state` (
    `id`          INTEGER PRIMARY KEY CHECK (`id` = 1),
    `kind`        TEXT NOT NULL,
    `head_name`   TEXT NOT NULL,
    `head_orig`   TEXT NOT NULL,
    `current_oid` TEXT NOT NULL,
    `todo`        TEXT NOT NULL,
    `payload`     TEXT NOT NULL DEFAULT '',
    `updated_at`  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO `sequence_state`
    (`id`, `kind`, `head_name`, `head_orig`, `current_oid`, `todo`, `payload`, `updated_at`)
SELECT 1, `kind`, `head_name`, `head_orig`, `current_oid`, `todo`, `payload`, `updated_at`
FROM `sequence_state__new_2026071901`
WHERE `worktree_id` = '';

DROP TABLE `sequence_state__new_2026071901`;

-- `rebase_state` is untouched by the forward migration (its shape is owned by
-- lazy DDL in `src/command/rebase.rs`), so there is nothing to roll back here.
