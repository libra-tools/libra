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
//!
//! The end-to-end cases each start a CLI, exporter and bubblewrap namespace;
//! serialize them so shared runner load does not consume the export deadline.

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
        if !libra::internal::ai::observed_agents::opencode_export::trusted_bwrap_available().await {
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
        let export_body = export_body.replace("__REPO__", &repo.to_string_lossy());
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
        // Seed the export-bridge trust store directly for harness speed.
        // The operator registration path is `libra agent rpc trust --dir
        // <path>` followed by `libra agent rpc trust opencode` (DR-04b; its
        // CLI contract is covered by tests/command/agent_rpc_trust_test.rs::
        // provider_exporter_trust_is_ungated_and_trusted_dir_bound). This
        // seed faithfully populates exactly what `read_trust`/
        // `revalidate_trust` consume, via the lib's own provenance
        // computation.
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
            conn.execute_raw(Statement::from_sql_and_values(
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
        self.run_with_log(args, stdin, None)
    }

    fn run_with_log(&self, args: &[&str], stdin: Option<&str>, log_filter: Option<&str>) -> Output {
        let mut cmd = self.command();
        if let Some(log_filter) = log_filter {
            cmd.env("LIBRA_LOG", log_filter);
        }
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
        self.stop_with_log(session_id, None)
    }

    fn stop_with_log(&self, session_id: &str, log_filter: Option<&str>) -> Output {
        let envelope = json!({
            "hook_event_name": "session.idle",
            "session_id": session_id,
            "cwd": self.repo.to_string_lossy(),
        })
        .to_string();
        self.run_with_log(
            &["agent", "hooks", "opencode", "stop"],
            Some(&envelope),
            log_filter,
        )
    }

    /// Make object publication fail after the checkpoint writer owns its
    /// finalizer marker. This keeps the crash-path assertion on a real
    /// filesystem failure instead of exposing a production environment seam.
    fn block_object_directory(&self) {
        let objects = self.repo.join(".libra").join("objects");
        let preserved = self.repo.join(".libra").join("objects-preserved");
        if objects.exists() {
            std::fs::rename(&objects, &preserved).expect("preserve objects directory");
        }
        std::fs::write(&objects, "not a directory").expect("block objects directory");
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
        conn.query_all_raw(Statement::from_string(
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
const EXPORT_HELLO: &str = r#"printf '{"info":{"id":"%s","directory":"__REPO__"},"messages":[{"info":{"role":"user","id":"msg_u1"},"parts":[{"type":"text","text":"hi"}]},{"info":{"role":"assistant","id":"msg_a1"},"parts":[{"type":"text","text":"hello"}]}]}' "$2""#;

const EXPORT_IMPORT: &str = r#"printf '%s' '{"info":{"id":"ses_import","directory":"__REPO__","status":"stopped","time":{"created":1784077200000,"updated":1784077260000}},"messages":[{"info":{"role":"user","id":"msg_u1","time":{"created":1784077210000}},"parts":[{"type":"text","text":"historical"}]},{"info":{"role":"assistant","id":"msg_a1","time":{"created":1784077250000}},"parts":[{"type":"text","text":"imported"}]}]}'"#;

const EXPORT_WRONG_SESSION: &str = r#"printf '%s' '{"info":{"id":"ses_other","directory":"__REPO__"},"messages":[{"info":{"role":"user","id":"msg_u1"},"parts":[{"type":"text","text":"wrong session"}]}]}'"#;

/// M4 DR-05: the explicit-ID import command consumes the trusted sandboxed
/// export bridge and persists an import-channel checkpoint, not an export-job
/// side effect or a file-backed source.
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_historical_import_e2e_uses_trusted_export_bytes() {
    let Some(repo) = BridgeRepo::init(EXPORT_IMPORT).await else {
        return;
    };
    let output = repo.run(
        &[
            "agent",
            "import",
            "--session",
            "ses_import",
            "--agent",
            "opencode",
            "--yes",
            "--json",
        ],
        None,
    );
    assert!(output.status.success(), "import: {}", describe(&output));
    let claims = repo
        .query_rows("SELECT state, source_channel FROM agent_coverage_claim")
        .await;
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].try_get_by::<String, _>("source_channel").unwrap(),
        "import"
    );
    assert!(
        repo.query_rows("SELECT job_id FROM agent_export_job")
            .await
            .is_empty(),
        "historical import must not create an idle/export generation job"
    );
    let sessions = repo
        .query_rows("SELECT started_at, last_event_at, stopped_at FROM agent_session")
        .await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].try_get_by::<i64, _>("started_at").unwrap(),
        1_784_077_200,
        "real OpenCode info.time.created must drive lifecycle start"
    );
    assert_eq!(
        sessions[0].try_get_by::<i64, _>("last_event_at").unwrap(),
        1_784_077_260
    );
    assert_eq!(
        sessions[0]
            .try_get_by::<Option<i64>, _>("stopped_at")
            .unwrap(),
        Some(1_784_077_260)
    );
}

/// opencode_export_whole_session_idempotent: two idles over unchanged export
/// content append exactly one checkpoint; the export job converges.
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
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

/// The trusted exporter hands `capture_authorized` a transient redacted
/// checksum. Its successful durable path must bind that checksum to this
/// repository before a snapshot projection reaches `agent_session` metadata.
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_export_durable_snapshot_uses_repository_keyed_hmac() {
    let Some(repo) = BridgeRepo::init(EXPORT_HELLO).await else {
        return;
    };
    let output = repo.stop_with_log("ses_hmac_snapshot", Some("debug"));
    assert!(
        output.status.success(),
        "successful export: {}",
        describe(&output)
    );

    let rows = repo
        .query_rows(
            "SELECT metadata_json FROM agent_session \
             WHERE provider_session_id = 'ses_hmac_snapshot'",
        )
        .await;
    assert_eq!(rows.len(), 1, "successful export must have one session row");
    let metadata_json: String = rows[0].try_get_by("metadata_json").unwrap();
    let metadata: Value =
        serde_json::from_str(&metadata_json).expect("OpenCode metadata is valid JSON");
    let digest = metadata["transcript_snapshot"]["source"]["digest_sha256"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "successful OpenCode snapshot has a durable source commitment; stdout: {}; metadata: {metadata_json}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    assert!(
        digest.strip_prefix("source/hmac-v2/").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        }),
        "durable OpenCode source commitment must be repository-keyed HMAC v2: {digest}"
    );
    assert!(
        !metadata_json.contains("sha256:"),
        "durable OpenCode metadata retained a transient helper SHA-256: {metadata_json}"
    );
}

/// The exporter binary may be trusted but still select a stale/incorrect
/// OpenCode session. Identity validation must keep those bytes out of the
/// export channel while the hook continues with its metadata-only fallback.
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_export_rejects_wrong_native_session_without_export_claim() {
    let Some(repo) = BridgeRepo::init(EXPORT_WRONG_SESSION).await else {
        return;
    };
    let output = repo.stop("ses_expected");
    assert!(
        output.status.success(),
        "identity mismatch must degrade safely: {}",
        describe(&output)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("ses_other"),
        "exporter-supplied session id leaked to hook stderr: {}",
        describe(&output)
    );
    let claims = repo
        .query_rows("SELECT session_id FROM agent_coverage_claim WHERE source_channel = 'export'")
        .await;
    assert!(
        claims.is_empty(),
        "mismatched export must not produce an export-channel coverage claim"
    );
}

#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_registered_failure_releases_dirty_job_claim_and_marker() {
    let Some(repo) = BridgeRepo::init(EXPORT_HELLO).await else {
        return;
    };
    repo.block_object_directory();
    let failed = repo.stop("ses_registered_failure");
    assert!(!failed.status.success(), "{}", describe(&failed));
    assert!(repo.checkpoints().is_empty());
    let claims = repo
        .query_rows("SELECT state, owner FROM agent_coverage_claim WHERE source_channel = 'export'")
        .await;
    assert_eq!(claims.len(), 1);
    assert_eq!(
        claims[0].try_get_by::<String, _>("state").unwrap(),
        "abandoned"
    );
    assert!(
        claims[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none()
    );
    let jobs = repo
        .query_rows("SELECT state, owner, lease_expires_at FROM agent_export_job")
        .await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].try_get_by::<String, _>("state").unwrap(), "dirty");
    assert!(
        jobs[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none(),
        "failed export job retained its lease owner"
    );
    assert!(
        repo.query_rows("SELECT `key` FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
            .await
            .is_empty(),
        "registered export failure leaked its marker"
    );
}

/// opencode_export_plaintext_never_in_persist_or_logs: a secret in the
/// export content is redacted before it reaches the traces blob.
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_export_plaintext_never_in_persist_or_logs() {
    let secret = "AKIAZZZZZZZZZZZZZZZZ";
    let body = format!(
        r#"printf '{{"info":{{"id":"%s","directory":"__REPO__"}},"messages":[{{"info":{{"role":"user","id":"msg_u1"}},"parts":[{{"type":"text","text":"use {secret} now"}}]}}]}}' "$2""#
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
#[serial_test::serial(opencode_export_bridge)]
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
/// the child (the 16 MiB stdout poll on every OS; Linux additionally holds
/// the strict RLIMIT_FSIZE write cap — FIX-SBX-01 keeps macOS at a coarse 8
/// GiB disk backstop) and degrades to metadata-only — no truncated content
/// claim, the write still succeeds.
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
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

/// macOS has no containment equivalent to bwrap's PID namespace for an
/// exporter that can fork and `setsid()` away from its direct leader. The
/// bridge must fail before it ever executes a trusted-looking exporter.
#[cfg(target_os = "macos")]
#[tokio::test]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_export_macos_is_unsupported_before_spawn() {
    use libra::internal::ai::observed_agents::opencode_export::{
        ExportLimits, run_export_subprocess_sandboxed,
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let marker = tmp.path().join("must-not-execute");
    let exporter = tmp.path().join("fake-opencode");
    std::fs::write(
        &exporter,
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&exporter, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = run_export_subprocess_sandboxed(&exporter, "sess-macos", ExportLimits::default())
        .await
        .expect_err("macOS OpenCode export must fail closed before spawn");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("unsupported on macOS") && rendered.contains("fail-closed"),
        "unexpected macOS rejection: {rendered}"
    );
    assert!(
        !marker.exists(),
        "macOS failure must occur before the exporter is spawned"
    );
}
