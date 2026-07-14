//! plan-20260713 DR-04b — OpenCode export-bridge e2e through the REAL CLI
//! hook path with a deterministic fake exporter (GC-DR-07: no real opencode
//! binary, no network; bwrap must be present or tests skip).
//!
//! Each test drives `libra agent hooks opencode stop` (session.idle →
//! TurnEnd) in a scratch repo with an ISOLATED global config store
//! and the fake exporter's trust record seeded into the repo config store,
//! then inspects the checkpoint catalog, coverage claims (channel =
//! 'export'), the export job row, and the decoded traces blob. Covers the
//! DR-04b verification cases: whole-session idempotence, secret-never-
//! persists, trusted-binary revalidation on drift, and oversize/untrusted
//! degradation.

#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{Value, json};
use tempfile::TempDir;

struct BridgeRepo {
    _tmp: TempDir,
    repo: PathBuf,
    home: PathBuf,
    global_db: PathBuf,
    exporter: PathBuf,
}

impl BridgeRepo {
    async fn init(export_body: &str) -> Option<Self> {
        // Detect "trusted AND usable", not merely present (Codex M3 R3): the
        // export path degrades when bwrap fails the integrity policy (e.g. it
        // lives under a user-writable path), which would make the content
        // assertions below spuriously fail — skip instead.
        if !libra::internal::ai::observed_agents::opencode_export::trusted_bwrap_available() {
            eprintln!("skipped (no trusted, usable bwrap)");
            return None;
        }
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let trusted_dir = home.join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&trusted_dir).unwrap();
        // A fake `opencode` that ignores argv and prints the fixed export.
        let exporter = trusted_dir.join("opencode");
        std::fs::write(&exporter, format!("#!/bin/sh\n{export_body}\n")).unwrap();
        std::fs::set_permissions(&exporter, std::fs::Permissions::from_mode(0o755)).unwrap();

        let global_db = home.join("config.db");
        let this = Self {
            _tmp: tmp,
            repo,
            home,
            global_db,
            exporter,
        };
        let out = this.run(&["init"], None);
        assert!(out.status.success(), "libra init: {}", describe(&out));
        // Seed the export-bridge trust store directly (the RPC trust CLI is
        // built for `libra-agent-*` binaries, not the real provider CLI —
        // an operator registration path for the export binary is a
        // documented DR-04b follow-up). This faithfully populates exactly
        // what `read_trust`/`revalidate_trust` consume, via the lib's own
        // provenance computation.
        this.seed_trust().await;
        Some(this)
    }

    /// Write `agent.external_agents.trusted_dirs` + `agent.trust.opencode`
    /// into the isolated global config store using `compute_provenance`.
    async fn seed_trust(&self) {
        use libra::internal::ai::observed_agents::compute_provenance;
        // Ensure the global config DB + config_kv table exist (the enable
        // write below also creates them, but do it explicitly).
        let out = self.run(
            &["config", "set", "agent.external_agents.enabled", "true"],
            None,
        );
        assert!(
            out.status.success(),
            "enable external agents: {}",
            describe(&out)
        );

        let provenance = compute_provenance(&self.exporter).expect("compute provenance");
        let dir = self
            .exporter
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = json!({
            "path": provenance.canonical_path.to_string_lossy(),
            "sha256": provenance.sha256,
            "device": provenance.device,
            "inode": provenance.inode,
            "mtime": provenance.mtime,
        })
        .to_string();
        let dirs = json!([dir]).to_string();

        // Trust records live in the REPO config_kv (ConfigKv::get uses the
        // repo db instance), not the global config store.
        let url = format!(
            "sqlite://{}?mode=rwc",
            self.repo.join(".libra").join("libra.db").display()
        );
        let conn: DatabaseConnection = Database::connect(url).await.expect("open repo db");
        let backend = conn.get_database_backend();
        for (key, value) in [
            ("agent.external_agents.trusted_dirs", dirs.as_str()),
            ("agent.trust.opencode", record.as_str()),
        ] {
            conn.execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO config_kv (key, value, encrypted) VALUES (?, ?, 0)",
                [key.into(), value.into()],
            ))
            .await
            .expect("seed config_kv row");
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .env("LIBRA_CONFIG_GLOBAL_DB", &self.global_db)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env_remove("CODEX_HOME");
        cmd
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = self.command();
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn libra");
        if let Some(input) = stdin {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        drop(child.stdin.take());
        child.wait_with_output().expect("wait libra")
    }

    fn stop(&self, session_id: &str) -> Output {
        let envelope = json!({
            "hook_event_name": "session.idle",
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
        })
        .to_string();
        self.run(&["agent", "hooks", "opencode", "stop"], Some(&envelope))
    }

    fn checkpoints(&self) -> Vec<Value> {
        let out = self.run(&["agent", "checkpoint", "list", "--json"], None);
        assert!(out.status.success(), "checkpoint list: {}", describe(&out));
        let parsed: Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json");
        parsed["data"]["checkpoints"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    async fn query_rows(&self, sql: &str) -> Vec<sea_orm::QueryResult> {
        let url = format!(
            "sqlite://{}?mode=ro",
            self.repo.join(".libra").join("libra.db").display()
        );
        let conn: DatabaseConnection = Database::connect(url).await.expect("open db");
        conn.query_all(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("query")
    }

    /// Read every persisted transcript blob via `checkpoint export` (which
    /// emits the decoded traces content; `show --json` is only a summary).
    fn traces_text(&self) -> String {
        let cps = self.checkpoints();
        let mut all = String::new();
        for cp in &cps {
            if let Some(id) = cp["checkpoint_id"].as_str() {
                let out = self.run(&["agent", "checkpoint", "export", id], None);
                all.push_str(&String::from_utf8_lossy(&out.stdout));
                all.push_str(&String::from_utf8_lossy(&out.stderr));
            }
        }
        all
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

/// A minimal valid export with one user turn (`hi` / `hello`) — normalizes
/// to golden vector 1.
const EXPORT_HELLO: &str = r#"printf '%s' '{"info":{"id":"ses_x"},"messages":[{"info":{"role":"user","id":"msg_u1"},"parts":[{"type":"text","text":"hi"}]},{"info":{"role":"assistant","id":"msg_a1"},"parts":[{"type":"text","text":"hello"}]}]}'"#;

/// opencode_export_whole_session_idempotent: two idles over unchanged export
/// content append exactly one checkpoint; the export job converges.
#[tokio::test]
async fn opencode_export_whole_session_idempotent() {
    let Some(repo) = BridgeRepo::init(EXPORT_HELLO).await else {
        return;
    };
    let first = repo.stop("ses_idem");
    assert!(first.status.success(), "first idle: {}", describe(&first));
    assert_eq!(repo.checkpoints().len(), 1, "first idle appends once");

    let second = repo.stop("ses_idem");
    assert!(
        second.status.success(),
        "second idle: {}",
        describe(&second)
    );
    assert_eq!(
        repo.checkpoints().len(),
        1,
        "repeated export over unchanged content must not append again"
    );

    let claims = repo
        .query_rows("SELECT state, source_channel FROM agent_coverage_claim")
        .await;
    assert_eq!(claims.len(), 1);
    let channel: String = claims[0].try_get_by("source_channel").unwrap();
    assert_eq!(
        channel, "export",
        "claims carry the export provenance channel"
    );
    let jobs = repo
        .query_rows("SELECT observed_generation, processed_generation FROM agent_export_job")
        .await;
    assert_eq!(jobs.len(), 1);
    let observed: i64 = jobs[0].try_get_by("observed_generation").unwrap();
    let processed: i64 = jobs[0].try_get_by("processed_generation").unwrap();
    assert_eq!(observed, processed, "export job converged (clean)");
}

/// opencode_export_plaintext_never_in_persist_or_logs: a secret in the
/// export content is redacted before it reaches the traces blob.
#[tokio::test]
async fn opencode_export_plaintext_never_in_persist_or_logs() {
    let secret = "AKIAZZZZZZZZZZZZZZZZ";
    let body = format!(
        r#"printf '%s' '{{"info":{{"id":"ses_x"}},"messages":[{{"info":{{"role":"user","id":"msg_u1"}},"parts":[{{"type":"text","text":"use {secret} now"}}]}}]}}'"#
    );
    let Some(repo) = BridgeRepo::init(&body).await else {
        return;
    };
    let out = repo.stop("ses_secret");
    assert!(out.status.success(), "idle: {}", describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains(secret),
        "secret leaked to stderr"
    );
    let traces = repo.traces_text();
    assert!(
        !traces.contains(secret),
        "secret must never reach the traces blob"
    );
    assert!(traces.contains("REDACTED"), "redaction marker expected");
}

/// opencode_export_binary_trust_revalidates: tampering with the trusted
/// binary after registration revokes trust — the next idle degrades to
/// metadata-only capture (no content append), never an untrusted spawn.
#[tokio::test]
async fn opencode_export_binary_trust_revalidates() {
    let Some(repo) = BridgeRepo::init(EXPORT_HELLO).await else {
        return;
    };
    // Healthy first capture.
    assert!(repo.stop("ses_drift").status.success());
    let before = repo.checkpoints().len();
    assert_eq!(before, 1);

    // Tamper with the binary (sha256/mtime drift) → trust must revoke.
    std::fs::write(&repo.exporter, "#!/bin/sh\nprintf 'tampered'\n").unwrap();
    std::fs::set_permissions(&repo.exporter, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = repo.stop("ses_drift2");
    assert!(
        out.status.success(),
        "drift must degrade gracefully, not crash: {}",
        describe(&out)
    );
    // A metadata-only checkpoint may still be written, but NO export-channel
    // claim: the tampered content never flowed through the gate.
    let claims = repo
        .query_rows("SELECT session_id FROM agent_coverage_claim WHERE source_channel = 'export'")
        .await;
    // Only the pre-drift session produced an export claim.
    for row in &claims {
        let sid: String = row.try_get_by("session_id").unwrap();
        assert!(
            !sid.contains("ses_drift2"),
            "tampered binary must not produce an export-channel claim"
        );
    }
}

/// opencode_export_oversize_session_degrades: an over-cap export terminates
/// the child (RLIMIT_FSIZE) and degrades to metadata-only — no truncated
/// content claim, the write still succeeds.
#[tokio::test]
async fn opencode_export_oversize_session_degrades() {
    // 32 MiB of zeros — well past the 16 MiB cap.
    let Some(repo) = BridgeRepo::init("head -c 33554432 /dev/zero").await else {
        return;
    };
    let out = repo.stop("ses_big");
    assert!(
        out.status.success(),
        "oversize export must degrade, not fail the hook: {}",
        describe(&out)
    );
    let claims = repo
        .query_rows(
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE source_channel = 'export'",
        )
        .await;
    let n: i64 = claims[0].try_get_by("n").unwrap();
    assert_eq!(n, 0, "oversize content must not produce an export claim");
}
