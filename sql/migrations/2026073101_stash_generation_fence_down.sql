-- 2026073101_stash_generation_fence_down
--
-- Rolling the fence back re-admits binaries that write generation-less stash
-- reflog lines. The lines already written keep their generations (they are
-- ordinary message-adjacent columns to an old reader), so nothing breaks —
-- only the ABA protection stops being guaranteed for entries written while
-- rolled back.
DELETE FROM `metadata_kv` WHERE `scope` = 'repository' AND `target` = '' AND `key` = 'stash.reflog.generation';
