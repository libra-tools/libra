-- Forward-only compatibility barrier for independently shipped 0101 and 0601.
-- The runner holds its claim-first writer transaction while it validates an
-- existing operation_v2 receipt, or copies a complete v1 namespace and records
-- precisely the missing 0101 receipt. It never replays other historical holes.
-- Keeping this receipt above 0601 makes older binaries refuse the v2 schema.
SELECT 1;
