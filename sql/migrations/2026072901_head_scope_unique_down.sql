-- Rollback of 2026072901_head_scope_unique.
--
-- Dropping the uniqueness constraints cannot corrupt anything on its own —
-- it only stops the database from rejecting a duplicate the code should not
-- create — so this rollback is unconditional. The non-unique lookup index
-- from 2026070801 stays in place, so scoped HEAD reads keep their index.
DROP INDEX IF EXISTS `idx_reference_head_scope_unique`;
DROP INDEX IF EXISTS `idx_reference_head_main_unique`;
