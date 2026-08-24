-- 2026082401_agent_bridge_link_relations: make the bridge association catalogue
-- a real relation graph (plan-20260818 LB-04/LB-05 VCS wiring).
--
-- `2026081801_agent_bridge_capture.sql` declared `agent_bridge_link` with
-- `UNIQUE(source_type, source_id)` and a `source_type` CHECK limited to the
-- ingress vocabulary (`event`/`operation`/`checkpoint`/`evidence`/`provenance`).
-- Two problems surfaced once mutations started recording real provenance:
--
--  1. `UNIQUE(source_type, source_id)` allows exactly ONE association per
--     result. A checkpoint (or commit) must link to its operation AND its
--     workspace AND its parent session AND each evidence id — under the old
--     constraint every association after the first was reported as a
--     retarget conflict, so rich provenance could never be persisted.
--  2. `commit.create`, `checkpoint.restore` and `review.run` produce results
--     whose source kind (`commit` / `restore` / `review`) was not in the CHECK
--     list at all, so recording their association failed outright.
--
-- The rebuild keys uniqueness on the full edge `(source_type, source_id,
-- target_type, target_id)`, so a replay of the same edge is still an idempotent
-- no-op while a result may carry several associations. `target_type` stays an
-- open TEXT column: `evidence.append` / `provenance.append` let the peer name
-- the target's own kind (`ai_evidence`, …), and narrowing it here would break
-- that established contract. Singular-relation conflicts — a source pointing at
-- a DIFFERENT target than the one recorded — remain fail-closed, enforced by
-- the service layer, which knows which relations are singular and which are
-- multi-valued.
--
-- No rows are dropped: every existing edge is copied over verbatim.

-- Phase 1: rename the existing table out of the way (indexes first).
DROP INDEX IF EXISTS `idx_agent_bridge_link_target`;
ALTER TABLE `agent_bridge_link` RENAME TO `agent_bridge_link__old_2026081801`;

-- Phase 2: re-create with the widened vocabulary and edge-level uniqueness.
CREATE TABLE `agent_bridge_link` (
    `link_id`          INTEGER PRIMARY KEY AUTOINCREMENT,
    `bridge_session_id` TEXT NOT NULL REFERENCES `agent_bridge_session`(`bridge_session_id`) ON DELETE CASCADE,
    -- What produced the association. Ingress kinds plus the mutation result
    -- kinds (`commit.create` / `checkpoint.restore` / `review.run`).
    `source_type`      TEXT NOT NULL CHECK(`source_type` IN (
        'event','operation','checkpoint','evidence','provenance',
        'commit','restore','review'
    )),
    `source_id`        TEXT NOT NULL,
    -- The relation kind. Open TEXT: ingress peers name the target's own kind.
    `target_type`      TEXT NOT NULL,
    `target_id`        TEXT NOT NULL,
    `created_at`       INTEGER NOT NULL,
    UNIQUE(`source_type`, `source_id`, `target_type`, `target_id`)
);

-- Phase 3: copy every existing edge verbatim.
INSERT INTO `agent_bridge_link` (
    `link_id`, `bridge_session_id`, `source_type`, `source_id`,
    `target_type`, `target_id`, `created_at`
)
SELECT
    `link_id`, `bridge_session_id`, `source_type`, `source_id`,
    `target_type`, `target_id`, `created_at`
FROM `agent_bridge_link__old_2026081801`;

-- Phase 4: drop the temporary copy.
DROP TABLE `agent_bridge_link__old_2026081801`;

-- Phase 5: re-create the indexes, plus a source index for the "all
-- associations of this result" read the provenance projection performs.
CREATE INDEX IF NOT EXISTS `idx_agent_bridge_link_target`
    ON `agent_bridge_link`(`target_type`, `target_id`);
CREATE INDEX IF NOT EXISTS `idx_agent_bridge_link_source`
    ON `agent_bridge_link`(`source_type`, `source_id`);
