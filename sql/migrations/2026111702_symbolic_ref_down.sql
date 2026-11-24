-- PM-01 down: remove the symbolic ref table and its exclusivity triggers.
DROP TRIGGER IF EXISTS reference_symbolic_update_guard;
DROP TRIGGER IF EXISTS reference_symbolic_insert_guard;
DROP TRIGGER IF EXISTS symbolic_ref_entity_update_guard;
DROP TRIGGER IF EXISTS symbolic_ref_entity_insert_guard;
DROP TABLE IF EXISTS symbolic_ref;
