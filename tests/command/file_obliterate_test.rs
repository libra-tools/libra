//! Integration tests for `libra file obliterate` (lore.md 2.5).
//!
//! Covers the safety gate (dry-run / --yes), the payload delete, the durable
//! 0600 audit record, fsck's IntentionalAbsence distinction (exit stays 0),
//! and idempotent re-runs.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command,
};

/// Commit a file and return (repo, blob_oid) for its content blob.
fn repo_with_secret() -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("secret.txt"), "top secret payload\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "secret.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add secret", "--no-verify"], p),
        "commit",
    );
    let ls = run_libra_command(&["ls-tree", "HEAD"], p);
    let out = String::from_utf8_lossy(&ls.stdout);
    let oid = out
        .lines()
        .find(|l| l.contains("secret.txt"))
        .and_then(|l| {
            l.split_whitespace()
                .find(|w| w.len() == 40 || w.len() == 64)
        })
        .expect("blob oid")
        .to_string();
    (repo, oid)
}

fn loose_path(repo: &Path, oid: &str) -> PathBuf {
    repo.join(".libra/objects").join(&oid[..2]).join(&oid[2..])
}

async fn connect_raw_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts)
        .await
        .expect("connect raw repository database")
}

fn mark_obliteration_as_interrupted(repo: &Path) {
    let runtime = tokio::runtime::Runtime::new().expect("create runtime for crash-recovery setup");
    runtime.block_on(async {
        let conn = connect_raw_repo_db(repo).await;
        let result = conn
            .execute(Statement::from_string(
                conn.get_database_backend(),
                "UPDATE object_obliteration SET state='obliterating', payload_deleted_at=NULL",
            ))
            .await
            .expect("reset obliteration state to 'obliterating'");
        assert_eq!(
            result.rows_affected(),
            1,
            "reset exactly one obliteration row to 'obliterating'"
        );
        conn.close()
            .await
            .expect("close crash-recovery setup database");
    });
}

#[test]
fn obliterate_dry_run_previews_and_deletes_nothing() {
    let (repo, oid) = repo_with_secret();
    let p = repo.path();
    let out = run_libra_command(&["file", "obliterate", &oid, "--dry-run"], p);
    assert_cli_success(&out, "dry-run");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("DRY RUN"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(loose_path(p, &oid).exists(), "dry-run deletes nothing");
    // No audit record written on a dry run.
    assert!(!p.join(".libra/obliteration-audit.jsonl").exists());
}

#[test]
fn obliterate_requires_confirmation() {
    let (repo, oid) = repo_with_secret();
    let p = repo.path();
    let out = run_libra_command(&["file", "obliterate", &oid], p);
    assert_eq!(out.status.code(), Some(128), "no --yes refuses");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-OBLITERATE-003");
    assert!(loose_path(p, &oid).exists(), "refused run deletes nothing");
}

#[test]
fn obliterate_removes_payload_writes_audit_and_fsck_distinguishes() {
    let (repo, oid) = repo_with_secret();
    let p = repo.path();

    let out = run_libra_command(
        &["file", "obliterate", &oid, "--reason", "gdpr", "--yes"],
        p,
    );
    assert_cli_success(&out, "obliterate");
    // Payload physically gone.
    assert!(!loose_path(p, &oid).exists(), "payload deleted");

    // Durable audit: 0600, two records (requested + payload_deleted), no
    // cleartext payload.
    let audit_file = p.join(".libra/obliteration-audit.jsonl");
    assert!(audit_file.exists(), "audit written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&audit_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "audit log is 0600");
    }
    let audit = fs::read_to_string(&audit_file).unwrap();
    assert!(
        audit.contains("payload_deleted"),
        "final outcome recorded: {audit}"
    );
    assert!(audit.contains(&oid), "oid (address) recorded");
    assert!(
        !audit.contains("top secret payload"),
        "no cleartext payload in audit"
    );

    // fsck: the obliterated object is reported as intentionally absent
    // (distinct from missing) and the exit code stays 0.
    let fsck = run_libra_command(&["fsck"], p);
    assert_eq!(
        fsck.status.code(),
        Some(0),
        "obliteration does not fail fsck"
    );
    let text = String::from_utf8_lossy(&fsck.stdout);
    assert!(
        text.contains("intentionally absent"),
        "fsck distinguishes obliteration from corruption: {text}"
    );

    // Idempotent re-run.
    let again = run_libra_command(&["file", "obliterate", &oid, "--yes"], p);
    assert_cli_success(&again, "idempotent");
    assert!(String::from_utf8_lossy(&again.stdout).contains("already obliterated"));
}

#[test]
fn obliterate_recover_finishes_interrupted() {
    // Model a crash mid-obliteration: the tombstone was written
    // ('obliterating') but the payload is still on disk. The recovery path
    // must re-delete the payload and finalize the state. We reproduce the
    // mid-state via the same `obliterate` command PAUSED at the tombstone by
    // seeding a fresh loose object and forcing the row back to 'obliterating'.
    let (repo, oid) = repo_with_secret();
    let p = repo.path();

    // Obliterate once to create the tombstone (this also removes the payload).
    assert_cli_success(
        &run_libra_command(&["file", "obliterate", &oid, "--yes"], p),
        "obliterate",
    );

    // Recreate the loose payload EXACTLY (a crash could leave it present) by
    // re-hashing the identical bytes through hash-object -w, and force the row
    // back to 'obliterating' to model the interrupted state.
    fs::write(p.join("secret.txt"), "top secret payload\n").expect("rewrite");
    let hashed = run_libra_command(&["hash-object", "-w", "secret.txt"], p);
    assert_cli_success(&hashed, "re-hash payload");
    assert!(
        loose_path(p, &oid).exists(),
        "payload restored on disk for the test"
    );

    // Force the mid-state in the repository database without relying on host
    // sqlite3 binaries, which are not guaranteed on self-hosted runners.
    mark_obliteration_as_interrupted(p);

    // Recovery must re-delete the payload and finalize.
    let recover = run_libra_command(&["file", "obliterate", "--recover"], p);
    assert_cli_success(&recover, "recover");
    assert!(
        String::from_utf8_lossy(&recover.stdout).contains("recovered 1"),
        "one interrupted obliteration completed: {}",
        String::from_utf8_lossy(&recover.stdout)
    );
    // The payload is gone again and fsck still reports intentional absence.
    assert!(
        !loose_path(p, &oid).exists(),
        "recovery re-deleted the payload"
    );
    let fsck = run_libra_command(&["fsck"], p);
    assert_eq!(fsck.status.code(), Some(0), "still exit 0 after recovery");
    assert!(String::from_utf8_lossy(&fsck.stdout).contains("intentionally absent"));
}
