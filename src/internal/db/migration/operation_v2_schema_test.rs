//! Receipt validation rejects drift without repairing or rewriting repository state.

use sea_orm::{ConnectionTrait, Database, DbErr, TransactionTrait};

use super::{legacy_operation_namespace_ddl, operation_v2_schema::validate_operation_v2_schema};

const CANONICAL_SQL: &str = include_str!("../../../../sql/migrations/2026090101_operation_v2.sql");

async fn validate_fixture(schema_sql: &str, mutation: &str) -> Result<(), DbErr> {
    // Given an isolated schema, with exactly the requested structural mutation.
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    let txn = conn.begin().await.unwrap();
    let legacy_sql = legacy_operation_namespace_ddl();
    txn.execute_unprepared(schema_sql).await.unwrap();
    txn.execute_unprepared(&legacy_sql).await.unwrap();
    txn.execute_unprepared(mutation).await.unwrap();
    // When validating a receipt, compare against the unchanged shipped definitions.
    let result = validate_operation_v2_schema(&txn, CANONICAL_SQL, &legacy_sql).await;
    txn.rollback().await.unwrap();
    result
}

fn changed_definition(original: &str, replacement: &str) -> String {
    assert!(
        CANONICAL_SQL.contains(original),
        "missing fixture token: {original}"
    );
    CANONICAL_SQL.replacen(original, replacement, 1)
}

macro_rules! rejects_mutation {
    ($name:ident, $sql:expr) => {
        #[tokio::test]
        async fn $name() {
            // Then the validator, not fixture setup, rejects the damaged schema.
            validate_fixture(CANONICAL_SQL, $sql)
                .await
                .expect_err(stringify!($name));
        }
    };
}

macro_rules! rejects_definition {
    ($name:ident, $original:expr, $replacement:expr) => {
        #[tokio::test]
        async fn $name() {
            let schema = changed_definition($original, $replacement);
            validate_fixture(&schema, "SELECT 1;")
                .await
                .expect_err(stringify!($name));
        }
    };
}

#[tokio::test]
async fn complete_shipped_schema_is_accepted() {
    validate_fixture(CANONICAL_SQL, "SELECT 1;").await.unwrap();
}

#[tokio::test]
async fn additional_legacy_recopy_guard_is_accepted() {
    validate_fixture(
        CANONICAL_SQL,
        "CREATE TRIGGER forbid_legacy_recopy BEFORE INSERT ON legacy_operation
         BEGIN SELECT RAISE(ABORT, 'legacy rows must not be recopied'); END;",
    )
    .await
    .unwrap();
}

rejects_mutation!(missing_operation_is_rejected, "DROP TABLE operation;");
rejects_mutation!(missing_parent_is_rejected, "DROP TABLE operation_parent;");
rejects_mutation!(missing_head_is_rejected, "DROP TABLE operation_head;");
rejects_mutation!(missing_journal_is_rejected, "DROP TABLE operation_journal;");
rejects_mutation!(missing_identity_is_rejected, "DROP TABLE change_identity;");
rejects_mutation!(missing_revision_is_rejected, "DROP TABLE change_revision;");
rejects_mutation!(
    missing_predecessor_is_rejected,
    "DROP TABLE change_predecessor;"
);
rejects_mutation!(missing_ai_link_is_rejected, "DROP TABLE ai_operation_link;");
rejects_mutation!(
    missing_legacy_operation_is_rejected,
    "DROP TABLE legacy_operation;"
);
rejects_mutation!(
    missing_legacy_parent_is_rejected,
    "DROP TABLE legacy_operation_parent;"
);
rejects_mutation!(
    missing_legacy_view_is_rejected,
    "DROP TABLE legacy_operation_view;"
);
rejects_mutation!(
    missing_legacy_refs_is_rejected,
    "DROP TABLE legacy_operation_view_ref;"
);
rejects_mutation!(
    missing_legacy_workspace_is_rejected,
    "DROP TABLE legacy_operation_view_workspace;"
);
rejects_mutation!(
    omitted_remote_primary_key_component_is_rejected,
    "DROP TABLE legacy_operation_view_ref;
     CREATE TABLE legacy_operation_view_ref (
       view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL,
       ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL,
       PRIMARY KEY (view_id, ref_kind, ref_name));"
);
rejects_mutation!(
    missing_required_column_is_rejected,
    "ALTER TABLE operation_journal DROP COLUMN recovery_payload;"
);
rejects_definition!(
    wrong_column_type_is_rejected,
    "`format_version`      INTEGER NOT NULL DEFAULT 2",
    "`format_version`      TEXT NOT NULL DEFAULT 2"
);
rejects_definition!(
    wrong_column_default_is_rejected,
    "`format_version`      INTEGER NOT NULL DEFAULT 2",
    "`format_version`      INTEGER NOT NULL DEFAULT 1"
);
rejects_definition!(
    missing_not_null_is_rejected,
    "`format_version`      INTEGER NOT NULL DEFAULT 2",
    "`format_version`      INTEGER DEFAULT 2"
);
rejects_definition!(
    shortened_head_primary_key_is_rejected,
    "PRIMARY KEY (`repo_id`, `scope_key`, `op_id`)",
    "PRIMARY KEY (`repo_id`, `scope_key`)"
);
rejects_definition!(
    replace_conflict_policy_is_rejected,
    "`op_id`               TEXT PRIMARY KEY",
    "`op_id`               TEXT PRIMARY KEY ON CONFLICT REPLACE"
);
rejects_definition!(
    ignore_not_null_policy_is_rejected,
    "`format_version`      INTEGER NOT NULL DEFAULT 2",
    "`format_version`      INTEGER NOT NULL ON CONFLICT IGNORE DEFAULT 2"
);
rejects_definition!(
    additional_check_constraint_is_rejected,
    "`format_version`      INTEGER NOT NULL DEFAULT 2",
    "`format_version`      INTEGER NOT NULL DEFAULT 2 CHECK (format_version = 2)"
);
rejects_mutation!(
    missing_index_is_rejected,
    "DROP INDEX idx_operation_v2_repo_order;"
);
rejects_mutation!(
    unique_replacement_index_is_rejected,
    "DROP INDEX idx_operation_v2_repo_order;
     CREATE UNIQUE INDEX idx_operation_v2_repo_order
     ON operation(repo_id, end_ts DESC, start_ts DESC, op_id DESC);"
);
rejects_mutation!(
    partial_replacement_index_is_rejected,
    "DROP INDEX idx_operation_v2_repo_order;
     CREATE INDEX idx_operation_v2_repo_order
     ON operation(repo_id, end_ts DESC, start_ts DESC, op_id DESC) WHERE end_ts IS NOT NULL;"
);
rejects_mutation!(
    wrong_index_direction_is_rejected,
    "DROP INDEX idx_operation_v2_repo_order;
     CREATE INDEX idx_operation_v2_repo_order
     ON operation(repo_id, end_ts ASC, start_ts DESC, op_id DESC);"
);
rejects_mutation!(
    wrong_index_table_is_rejected,
    "DROP INDEX idx_operation_v2_repo_order;
     CREATE INDEX idx_operation_v2_repo_order
     ON legacy_operation(repo_id, end_ts DESC, start_ts DESC, op_id DESC);"
);
rejects_mutation!(
    wrong_index_collation_is_rejected,
    "DROP INDEX idx_operation_v2_repo_order;
     CREATE INDEX idx_operation_v2_repo_order
     ON operation(repo_id COLLATE NOCASE, end_ts DESC, start_ts DESC, op_id DESC);"
);
rejects_mutation!(
    nonunique_legacy_control_slot_is_rejected,
    "DROP INDEX idx_legacy_operation_control_slot;
     CREATE INDEX idx_legacy_operation_control_slot ON legacy_operation(repo_id, worktree_id)
     WHERE status = 'running' AND control_slot IS NOT NULL;"
);
rejects_mutation!(
    wrong_legacy_control_slot_predicate_is_rejected,
    "DROP INDEX idx_legacy_operation_control_slot;
     CREATE UNIQUE INDEX idx_legacy_operation_control_slot
     ON legacy_operation(repo_id, worktree_id) WHERE status = 'running';"
);
rejects_mutation!(
    nbsp_in_trigger_identifier_is_rejected,
    "DROP TRIGGER legacy_operation_scope_kind_domain_insert;
     CREATE TRIGGER IF NOT EXISTS legacy_operation_scope_kind_domain_insert
     BEFORE INSERT ON legacy_operation
     FOR EACH ROW WHEN NEW.\u{a0}scope_kind NOT IN ('main', 'linked', 'repository', 'unknown')
     BEGIN
       SELECT RAISE(ABORT, 'legacy_operation.scope_kind must be main, linked, repository or unknown');
     END;"
);

macro_rules! rejects_trigger_drift {
    ($missing:ident, $replaced:ident, $trigger:literal) => {
        rejects_mutation!($missing, concat!("DROP TRIGGER ", $trigger, ";"));
        rejects_mutation!(
            $replaced,
            concat!(
                "DROP TRIGGER ",
                $trigger,
                "; CREATE TRIGGER ",
                $trigger,
                " BEFORE INSERT ON legacy_operation BEGIN SELECT 1; END;"
            )
        );
    };
}

rejects_trigger_drift!(
    missing_provenance_insert_trigger_is_rejected,
    replaced_provenance_insert_trigger_is_rejected,
    "legacy_operation_scope_provenance_domain_insert"
);
rejects_trigger_drift!(
    missing_provenance_update_trigger_is_rejected,
    replaced_provenance_update_trigger_is_rejected,
    "legacy_operation_scope_provenance_domain_update"
);
rejects_trigger_drift!(
    missing_kind_insert_trigger_is_rejected,
    replaced_kind_insert_trigger_is_rejected,
    "legacy_operation_scope_kind_domain_insert"
);
rejects_trigger_drift!(
    missing_kind_update_trigger_is_rejected,
    replaced_kind_update_trigger_is_rejected,
    "legacy_operation_scope_kind_domain_update"
);
