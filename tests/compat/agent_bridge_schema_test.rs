//! `tests/compat/agent_bridge_schema_test.rs` — guard pinning the bridge
//! durable-projection migration (plan-20260818 LB-02) to the registered
//! migration set and its SQL files.
//!
//! A silent registry regression (forgetting to register the migration, or
//! bumping the version pin without adding a migration) surfaces here as well
//! as in `db_migration_test`/`agent_bridge_migration_test`.

use libra::internal::db::migration::{builtin_migrations, builtin_runner};

/// The migration that creates the bridge durable projection (LB-02).
const BRIDGE_MIGRATION_VERSION: i64 = 2026081801;
/// The follow-up that turns `agent_bridge_link` into a real relation graph so
/// one mutation result can carry its operation, workspace, parent-session and
/// evidence associations (LB-04/LB-05 VCS wiring).
const BRIDGE_LINK_RELATIONS_VERSION: i64 = 2026082401;

#[test]
fn bridge_migrations_are_registered_and_link_relations_is_the_latest() {
    let runner = builtin_runner().expect("builtin registry builds clean");
    assert!(
        builtin_migrations()
            .iter()
            .any(|migration| migration.version == BRIDGE_MIGRATION_VERSION),
        "2026081801_agent_bridge_capture must stay registered"
    );
    assert_eq!(
        builtin_migrations()
            .iter()
            .filter(|migration| migration.version <= BRIDGE_LINK_RELATIONS_VERSION)
            .map(|migration| migration.version)
            .max(),
        Some(BRIDGE_LINK_RELATIONS_VERSION),
        "2026082401_agent_bridge_link_relations must remain the latest bridge migration"
    );
    assert!(
        runner.max_registered_version() > Some(BRIDGE_LINK_RELATIONS_VERSION),
        "newer foundation migrations may follow the bridge migration"
    );
}

#[test]
fn bridge_link_relations_sql_widens_the_edge_key_and_source_kinds() {
    let up = include_str!("../../sql/migrations/2026082401_agent_bridge_link_relations.sql");
    // Edge-level uniqueness: without target_type/target_id in the key a result
    // could only ever record ONE association (LB-05 AC4 regression guard).
    assert!(
        up.contains("UNIQUE(`source_type`, `source_id`, `target_type`, `target_id`)"),
        "uniqueness must key the full edge, not just the source"
    );
    for kind in ["'commit'", "'restore'", "'review'"] {
        assert!(
            up.contains(kind),
            "the mutation result source kind {kind} must be allowed"
        );
    }
    let down = include_str!("../../sql/migrations/2026082401_agent_bridge_link_relations_down.sql");
    assert!(
        down.contains("_down_guard") && down.contains("CHECK"),
        "down must freeze (refuse) while link rows exist"
    );
}

#[test]
fn bridge_migration_sql_defines_all_bridge_tables() {
    let up = include_str!("../../sql/migrations/2026081801_agent_bridge_capture.sql");
    for table in [
        "agent_bridge_session",
        "agent_bridge_event",
        "agent_bridge_operation",
        "agent_bridge_checkpoint",
        "agent_bridge_link",
    ] {
        assert!(
            up.contains(&format!("CREATE TABLE IF NOT EXISTS `{table}`")),
            "{table} must be created by the migration SQL"
        );
    }
    // The fixed source constraint must be present (ADR-LB-02).
    assert!(
        up.contains("deepseek-harness"),
        "source must be fixed to deepseek-harness"
    );
}

#[test]
fn bridge_migration_down_is_forward_only_freeze() {
    let down = include_str!("../../sql/migrations/2026081801_agent_bridge_capture_down.sql");
    // The down path must guard against deleting bridge rows, never silently
    // drop acked data as a rollback shortcut (GC-LB-09 / ER-LB-04).
    assert!(
        down.contains("_down_guard") && down.contains("CHECK"),
        "down must freeze (refuse) while bridge rows exist"
    );
}
