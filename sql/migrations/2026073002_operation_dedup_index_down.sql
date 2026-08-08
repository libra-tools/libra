-- Down for 2026073002_operation_dedup_index: drop the index. The query still
-- works without it (slower), so this is a clean reversal.
DROP INDEX IF EXISTS `idx_operation_dedup_scope`;
