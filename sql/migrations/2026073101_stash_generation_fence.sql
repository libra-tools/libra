-- 2026073101_stash_generation_fence
--
-- plan-20260714 Part C W2 (§C.10): version fence for the stash reflog's
-- GENERATION column.
--
-- Every stash reflog line minted from this version on carries a third
-- tab-separated `gen=<uuid>` column — the non-reusable identity every
-- raw-line CAS (`do_drop`, pop's phase 2, autostash) compares. The column is
-- what defeats the ABA reuse of a line's visible fields: a drop-and-repush of
-- the same commit onto the same parent within the same second reproduces
-- every OTHER byte of the line.
--
-- The column only holds while every WRITER mints it. An older binary writes
-- generation-less lines, whose visible bytes are reusable — reopening the
-- hole for exactly the entries it writes. This fence makes every older
-- binary refuse the repository at connect time (future-schema fail-closed)
-- before it can write the stash log at all, the same mechanism as the
-- worktree registry's capability markers. Existing generation-less lines are
-- backfilled by the first locked stack publication.
INSERT INTO `metadata_kv` (`scope`, `target`, `key`, `value`, `value_type`, `created_at`, `updated_at`)
VALUES ('repository', '', 'stash.reflog.generation', '1', 'text', datetime('now'), datetime('now'))
ON CONFLICT(`scope`, `target`, `key`) DO NOTHING;
