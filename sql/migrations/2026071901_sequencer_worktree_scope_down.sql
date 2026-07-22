-- Rollback of 2026071901_sequencer_worktree_scope.
--
-- Restores the repository-global single-row shape. Only the MAIN worktree's
-- row (`worktree_id = ''`) can survive: the old schema has no way to represent
-- a linked worktree's sequence, so rolling back while one is active would
-- silently drop it. This rollback therefore FAILS CLOSED while any
-- linked-scope row exists (Part C §C.11 / §C.4.2: the down migration must not
-- silently discard linked state — finish or abort linked-worktree sequences
-- first). The guard below violates its CHECK constraint when a linked row is
-- present, aborting the runner's transaction (same pattern as 2026072101).

CREATE TABLE `sequence_state__down_guard_2026071901` (
    `linked_rows` INTEGER NOT NULL CHECK (`linked_rows` = 0)
);

INSERT INTO `sequence_state__down_guard_2026071901` (`linked_rows`)
SELECT COUNT(*) FROM `sequence_state` WHERE `worktree_id` <> '';

DROP TABLE `sequence_state__down_guard_2026071901`;

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
