//! Transaction and ownership gates; no ambient configuration is consulted.

use libra::internal::{
    config::ConfigKv,
    db::{self, DatabaseRole, schema},
};
use sea_orm::{ConnectionTrait, Statement, TransactionTrait};

pub async fn atomic_barrier() {
    for role in [DatabaseRole::GlobalConfig, DatabaseRole::SystemConfig] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.db");
        let conn = db::create_database_for_role(path.to_str().unwrap(), role)
            .await
            .unwrap();
        assert!(
            !schema::inspect_configuration_schema(&conn, role)
                .await
                .unwrap()
                .barrier_present
        );
        let baseline = std::fs::read(&path).unwrap();
        for ledger in ["schema_versions", "configuration_schema_versions"] {
            let txn = conn.begin().await.unwrap();
            txn.execute_unprepared(&format!("DROP TABLE IF EXISTS {ledger}; CREATE VIEW {ledger} AS SELECT 1 AS version, 'untrusted' AS name")).await.unwrap();
            let error = schema::inspect_configuration_schema(&txn, role)
                .await
                .err()
                .expect("a view is not an absent ledger");
            assert!(error.to_string().contains("not a table"));
            txn.rollback().await.unwrap();
        }
        assert_eq!(baseline, std::fs::read(&path).unwrap());
        // Caller rollback undoes both marker DDL and the setting itself.
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        db::write_configuration_barrier(&txn, role).await.unwrap();
        db::write_configuration_barrier(&txn, role).await.unwrap();
        ConfigKv::set_with_conn(&txn, "test.atomic", "rollback", false)
            .await
            .unwrap();
        txn.rollback().await.unwrap();
        assert_eq!(baseline, std::fs::read(&path).unwrap());
        assert!(
            ConfigKv::get_with_conn(&conn, "test.atomic")
                .await
                .unwrap()
                .is_none()
        );

        // A setting write failure cannot leave a standalone barrier.
        conn.execute_unprepared("CREATE TRIGGER reject_config BEFORE INSERT ON config_kv BEGIN SELECT RAISE(ABORT, 'injected config failure'); END").await.unwrap();
        let baseline = std::fs::read(&path).unwrap();
        let txn = conn.begin().await.unwrap();
        db::write_configuration_barrier(&txn, role).await.unwrap();
        assert!(
            ConfigKv::set_with_conn(&txn, "test.atomic", "forbidden", false)
                .await
                .is_err()
        );
        txn.rollback().await.unwrap();
        assert_eq!(baseline, std::fs::read(&path).unwrap());
        conn.execute_unprepared("DROP TRIGGER reject_config")
            .await
            .unwrap();

        // A marker insertion failure cannot persist an earlier data change in
        // the caller-owned transaction. Historical receipts remain intact.
        conn.execute_unprepared(
            schema::schema_manifest()
                .configuration_barrier
                .legacy_ledger_sql,
        )
        .await
        .unwrap();
        let known = schema::migrations_for_role(DatabaseRole::Repository)
            .pop()
            .unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO schema_versions VALUES (?, ?, 'preserved')",
            [known.version.into(), known.name.into()],
        ))
        .await
        .unwrap();
        conn.execute_unprepared("CREATE TRIGGER reject_barrier BEFORE INSERT ON schema_versions BEGIN SELECT RAISE(ABORT, 'injected barrier failure'); END").await.unwrap();
        let baseline = std::fs::read(&path).unwrap();
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        ConfigKv::set_with_conn(&txn, "test.atomic", "forbidden", false)
            .await
            .unwrap();
        assert!(db::write_configuration_barrier(&txn, role).await.is_err());
        txn.rollback().await.unwrap();
        assert_eq!(baseline, std::fs::read(&path).unwrap());
        conn.execute_unprepared("DROP TRIGGER reject_barrier")
            .await
            .unwrap();

        // Independent pools contend on one file; the writer lock precedes all
        // classification reads and the second writer recognizes the marker.
        let other = db::establish_connection_for_role(path.to_str().unwrap(), role)
            .await
            .unwrap();
        let first = async {
            let txn = db::begin_write_transaction(&conn).await.unwrap();
            db::write_configuration_barrier(&txn, role).await.unwrap();
            ConfigKv::set_with_conn(&txn, "test.first", "one", false)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        };
        let second = async {
            let txn = db::begin_write_transaction(&other).await.unwrap();
            db::write_configuration_barrier(&txn, role).await.unwrap();
            ConfigKv::set_with_conn(&txn, "test.second", "two", false)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        };
        tokio::join!(first, second);
        let inspection = schema::inspect_configuration_schema(&conn, role)
            .await
            .unwrap();
        assert!(
            inspection.barrier_present
                && inspection.base_receipt_present
                && inspection.issue.is_none()
        );
        let count: i64 = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) FROM schema_versions",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
        assert_eq!(count, 2, "one original receipt plus exactly one barrier");
        assert_eq!(
            ConfigKv::get_with_conn(&conn, "test.second")
                .await
                .unwrap()
                .unwrap()
                .value,
            "two"
        );
        for forbidden in [DatabaseRole::Repository, DatabaseRole::Derived] {
            let txn = conn.begin().await.unwrap();
            assert!(
                db::write_configuration_barrier(&txn, forbidden)
                    .await
                    .is_err()
            );
            txn.rollback().await.unwrap();
        }
        other.close().await.unwrap();
        conn.close().await.unwrap();
    }
}

pub fn sole_writer() {
    use syn::visit::Visit;
    #[derive(Default)]
    struct Calls {
        owner: String,
        writer_calls: Vec<String>,
        ddl_uses: Vec<String>,
    }
    impl<'ast> Visit<'ast> for Calls {
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            let previous = std::mem::replace(&mut self.owner, item.sig.ident.to_string());
            syn::visit::visit_item_fn(self, item);
            self.owner = previous;
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let previous = std::mem::replace(&mut self.owner, item.sig.ident.to_string());
            syn::visit::visit_impl_item_fn(self, item);
            self.owner = previous;
        }
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = call.func.as_ref()
                && path
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "write_configuration_barrier")
            {
                self.writer_calls.push(self.owner.clone());
            }
            syn::visit::visit_expr_call(self, call);
        }
        fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
            if matches!(&field.member, syn::Member::Named(name) if name == "legacy_ledger_sql") {
                self.ddl_uses.push(self.owner.clone());
            }
            syn::visit::visit_expr_field(self, field);
        }
    }
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut calls = Calls::default();
    for path in crate::workspace_sources::collect_workspace_rust_files(repo_root) {
        calls.visit_file(&syn::parse_file(&std::fs::read_to_string(path).unwrap()).unwrap());
    }
    calls.writer_calls.sort();
    assert_eq!(
        calls.writer_calls,
        ["apply_configuration_repair", "begin_mutation"]
    );
    assert_eq!(calls.ddl_uses, ["write_configuration_barrier"]);
    let manifest = schema::schema_manifest();
    assert_eq!(manifest.configuration_barrier.version, i64::MAX);
    assert!(
        !manifest
            .migrations
            .iter()
            .any(|entry| entry.migration.version == manifest.configuration_barrier.version)
    );
    let source = concat!(
        include_str!("../../src/command/config.rs"),
        "\n",
        include_str!("../../crates/libra-core/src/internal/config_scope.rs"),
    );
    for name in [
        "set",
        "add",
        "unset",
        "unset_matching",
        "replace_all",
        "unset_all",
        "handle_remove_section",
        "handle_rename_section",
    ] {
        // Parse each function's body via the same visitor to ensure all public
        // mutations keep their shared transaction entrance.
        assert!(
            source.contains(&format!("fn {name}(")),
            "missing mutation {name}"
        );
    }
    // `set`, `add`, `unset`, `unset_matching`, `replace_all`, `unset_all`.
    assert_eq!(source.matches("Self::begin_mutation(scope)").count(), 6);
    assert_eq!(
        source
            .matches("ScopedConfig::begin_mutation(scope)")
            .count(),
        2
    );
}
