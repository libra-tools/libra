-- Rollback of 2026072401_worktree_registry_v2.
--
-- Drops only the capability marker. The v2 registry FILE stays on disk: an
-- old binary still cannot silently misread it — the v2 layout renames the
-- top-level key, so a v1 parser errors (fail-closed) instead of reading
-- stale data or clobbering the file. W3-s1b adds lifecycle/journal/tombstone
-- tables here; their down guards will refuse while such state exists
-- (§C.7: v2 lifecycle must never be folded into a v1 active entry).
DROP TABLE IF EXISTS `worktree_registry_capability`;
