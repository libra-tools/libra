-- Rollback of 2026090601_legacy_config_table (#472).
--
-- Intentionally a no-op: `config` belongs to the bootstrap schema and older
-- binaries still read it. The forward migration cannot tell whether it created
-- the table or found a healthy existing one. Preserve both empty tables and
-- legacy rows when removing this migration's receipt; dropping either would
-- reintroduce #472 for a downgraded binary. Reapplying the repair is idempotent.
SELECT 1;
