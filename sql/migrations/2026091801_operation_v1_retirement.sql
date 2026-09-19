-- OL-15: Operation v2 is the sole runtime fact source.
-- Historical legacy rows were already isolated by the v2 staging migration;
-- once all callers have cut over, remove that retired namespace forward-only.
DROP TABLE IF EXISTS legacy_operation_view_workspace;
DROP TABLE IF EXISTS legacy_operation_view_ref;
DROP TABLE IF EXISTS legacy_operation_view;
DROP TABLE IF EXISTS legacy_operation_parent;
DROP TABLE IF EXISTS legacy_operation;
