-- Down for 2026073001_operation_args_digest_canonical.
--
-- Trimming is not reversible: the removed whitespace is not recorded
-- anywhere, and re-padding would invent values. The down migration is
-- therefore a no-op that SUCCEEDS — the canonical form is valid input for
-- every earlier binary too (the pre-W1 dedup check compared trimmed values,
-- so a trimmed row behaves identically there).
SELECT 1;
