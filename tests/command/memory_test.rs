//! Integration tests for `libra memory` (plan-20260926 DM-03 command surface).

use super::*;

fn memory_repo() -> tempfile::TempDir {
    create_committed_repo_via_cli()
}

/// A `libra memory` subcommand is read-only and must not write an `operation`
/// row (AC memory_writes_no_operation_row).
#[tokio::test]
async fn memory_writes_no_operation_row() {
    let repo = memory_repo();
    let p = repo.path();
    let db_path = p.join(".libra/libra.db");
    let conn = libra::internal::db::establish_connection(db_path.to_str().unwrap())
        .await
        .expect("open repo DB");

    async fn operation_count<C: sea_orm::ConnectionTrait>(conn: &C) -> i64 {
        use sea_orm::Statement;
        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS c FROM operation",
            ))
            .await
            .unwrap()
            .expect("count row");
        row.try_get("", "c").expect("count value")
    }

    let before = operation_count(&conn).await;
    let run = run_libra_command(&["memory", "status"], p);
    assert_cli_success(&run, "memory status");
    let after = operation_count(&conn).await;
    assert_eq!(
        before, after,
        "`libra memory status` must not write an operation row"
    );
}

/// The `memory` command parses and dispatches its placeholder subcommands.
#[test]
fn memory_subcommands_parse_and_run() {
    let repo = memory_repo();
    let p = repo.path();
    for sub in ["status", "list", "rebuild"] {
        let run = run_libra_command(&["memory", sub], p);
        assert_cli_success(&run, &format!("memory {sub}"));
    }
    let show = run_libra_command(&["memory", "show", "abcd"], p);
    assert_cli_success(&show, "memory show");
}
