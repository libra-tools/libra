//! plan-20260713 DR-04b — OpenCode export-bridge e2e through the REAL CLI
//! hook path with a deterministic fake exporter (GC-DR-07: no real opencode
//! binary, no network). Linux retains its trusted-bwrap availability gate;
//! macOS manual cases require Seatbelt and fail when it is unavailable.
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
        #[cfg(target_os = "macos")]
        assert!(
            libra::internal::ai::observed_agents::opencode_export::required_export_sandbox_available().await,
            "the macOS Required Seatbelt export sandbox is unavailable"
        );
        // Detect "trusted AND usable", not merely present (Codex M3 R3): the
        // export path degrades when bwrap fails the integrity policy (e.g. it
        // lives under a user-writable path), which would make the content
        // assertions below spuriously fail — skip instead.
        #[cfg(not(target_os = "macos"))]
        {
            if !libra::internal::ai::observed_agents::opencode_export::trusted_bwrap_available()
                .await
            {
                eprintln!("skipped (no trusted, usable bwrap)");
                return None;
            }
        }
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        let trusted_dir = home.join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&trusted_dir).unwrap();
        // Match ingress's canonical identity and the Required store preflight;
        // macOS temp paths commonly arrive through /var -> /private/var.
        #[cfg(target_os = "macos")]
        let (repo, home) = (repo.canonicalize().unwrap(), home.canonicalize().unwrap());
        #[cfg(target_os = "macos")]
        let trusted_dir = home.join("bin");
        #[cfg(target_os = "macos")]
        std::fs::create_dir_all(home.join(".local/share/opencode")).unwrap();
        let exporter = trusted_dir.join("opencode");
        let export_body = export_body.replace("__REPO__", &repo.to_string_lossy());
        #[cfg(target_os = "macos")]
        let script = macos_fake_exporter_script(&export_body);
        #[cfg(not(target_os = "macos"))]
        let script = format!("#!/bin/sh\n{export_body}\n");
        std::fs::write(&exporter, script).unwrap();
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
        // Only the macOS harness changes environment handling. Its isolated
        // processes need no operator account, model settings or credentials.
        #[cfg(target_os = "macos")]
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LIBRA_CONFIG_SYSTEM_DB", self.home.join("system-config.db"));
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
        let mut out = self.run_no_drain(args, stdin, log_filter);
        if args.iter().take(3).any(|arg| *arg == "hooks") && out.status.success() {
            // The hook only queues; the consumer runs the export and writes
            // the catalog. Store the queue here and keep the consumer's log
            // lines with the hook's so log assertions see both processes.
            out.stderr.extend(self.drain_spool(log_filter));
        }
        out
    }

    fn run_no_drain(&self, args: &[&str], stdin: Option<&str>, log_filter: Option<&str>) -> Output {
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

    /// Run `libra __capture-spool-drain` until `ready/` is empty and return
    /// the consumers' stderr.
    fn drain_spool(&self, log_filter: Option<&str>) -> Vec<u8> {
        let ready = self.repo.join(".libra/private/capture-spool/ready");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut stderr = Vec::new();
        loop {
            let pending = std::fs::read_dir(&ready)
                .map(|entries| entries.count())
                .unwrap_or(0);
            if pending == 0 {
                return stderr;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "capture spool still holds {pending} ready file(s) after 90s"
            );
            let mut cmd = self.command();
            if let Some(log_filter) = log_filter {
                cmd.env("LIBRA_LOG", log_filter);
            }
            let drained = cmd
                .arg("__capture-spool-drain")
                .stdin(Stdio::null())
                .output()
                .expect("run capture spool drain");
            stderr.extend(drained.stderr);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn stop(&self, session_id: &str) -> Output {
        // Always capture debug logs: the CLI's stderr is only rendered when an
        // assertion fails, and a D-group failure needs the export bridge's
        // opt-in failure detail to stay diagnosable.
        self.stop_with_log(session_id, Some("debug"))
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
        // Capture tables live in capture.db; repository tables in libra.db.
        let database = if sql.contains("agent_") {
            self.repo.join(".libra").join("private").join("capture.db")
        } else {
            self.repo.join(".libra").join("libra.db")
        };
        let url = format!("sqlite://{}?mode=ro", database.display());
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

    #[cfg(target_os = "macos")]
    fn client_invocations(&self) -> String {
        std::fs::read_to_string(
            self.home
                .join(".local/share/opencode/cov2-client-invocations"),
        )
        .expect("the trusted Seatbelt client must actually execute")
    }

    #[cfg(target_os = "macos")]
    fn cov2_stop(&self, session_id: &str, event_id: &str) -> Output {
        let envelope = json!({
            "hook_event_name":"session.status",
            "session_id":session_id,
            "cwd":self.repo.to_string_lossy(),
            "status":{"type":"idle"},
            "event_id":event_id,
        })
        .to_string();
        self.run_with_log(
            &["agent", "hooks", "opencode", "stop"],
            Some(&envelope),
            Some("debug"),
        )
    }

    #[cfg(target_os = "macos")]
    fn object_files(&self) -> Vec<PathBuf> {
        let mut pending = vec![self.repo.join(".libra/objects")];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(
                    std::fs::read_dir(path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            } else if path.is_file() {
                files.push(path);
            }
        }
        files.sort();
        files
    }

    #[cfg(target_os = "macos")]
    async fn cov2_claim_state(&self) -> Vec<Value> {
        self.query_rows("SELECT logical_turn_key, coverage_schema_version, coverage_digest, completeness, revision, state, owner, checkpoint_id, fence_token, source_channel FROM agent_coverage_claim ORDER BY logical_turn_key, coverage_schema_version")
            .await.into_iter().map(|row| json!({
                "key":row.try_get_by::<String,_>("logical_turn_key").unwrap(),
                "version":row.try_get_by::<i64,_>("coverage_schema_version").unwrap(),
                "digest":row.try_get_by::<String,_>("coverage_digest").unwrap(),
                "completeness":row.try_get_by::<String,_>("completeness").unwrap(),
                "revision":row.try_get_by::<i64,_>("revision").unwrap(),
                "state":row.try_get_by::<String,_>("state").unwrap(),
                "owner":row.try_get_by::<Option<String>,_>("owner").unwrap(),
                "checkpoint":row.try_get_by::<Option<String>,_>("checkpoint_id").unwrap(),
                "fence":row.try_get_by::<Option<i64>,_>("fence_token").unwrap(),
                "channel":row.try_get_by::<String,_>("source_channel").unwrap(),
            })).collect()
    }

    #[cfg(target_os = "macos")]
    async fn cov2_canonical_checkpoint(&self, channel: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};

        let claims = self.cov2_claim_state().await;
        assert_eq!(claims.len(), 1, "exactly one actual committed turn");
        assert_eq!(claims[0]["key"], "u");
        assert_eq!(claims[0]["version"], 2);
        assert_eq!(claims[0]["digest"], COV2_CANONICAL_SHA);
        // Native idle ends a turn, not the import session. Preserve the
        // existing active-session final-turn fence rather than forcecomplete.
        let completeness = if channel == "import" {
            "incomplete"
        } else {
            "complete"
        };
        assert_eq!(claims[0]["completeness"], completeness);
        assert_eq!(claims[0]["revision"], 1);
        assert_eq!(claims[0]["state"], "catalog_committed");
        assert!(claims[0]["owner"].is_null());
        assert_eq!(claims[0]["channel"], channel);
        assert!(claims[0]["fence"].as_i64().unwrap() > 0);
        assert_eq!(
            self.query_rows("SELECT revision FROM agent_coverage_revision")
                .await
                .len(),
            1
        );
        let id = claims[0]["checkpoint"].as_str().unwrap();
        let out = self.run(&["agent", "checkpoint", "export", id], None);
        assert!(
            out.status.success(),
            "read actual checkpoint: {}",
            describe(&out)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("sk-ant-EXAMPLE"));
        let stored: Value = serde_json::from_slice(&out.stdout).expect("stored transcript JSON");
        let records = if channel == "import" {
            assert_eq!(stored["schema_version"], 2);
            assert_eq!(stored["agent_kind"], "opencode");
            assert_eq!(stored["logical_turn_key"], "u");
            assert_eq!(stored["completeness"], "incomplete");
            stored["records"].as_array().unwrap().clone()
        } else {
            let messages = stored["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages[0]["type"], "user");
            assert_eq!(messages[1]["type"], "assistant");
            assert_eq!(messages[2]["type"], "idle");
            assert_eq!(messages[2]["outcome"], "succeeded");
            let parts = messages[1]["content"].as_array().unwrap();
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[0]["type"], "reasoning");
            assert_eq!(parts[1]["type"], "text");
            vec![
                json!({"type":"user","text":messages[0]["text"]}),
                json!({"type":"reasoning","provider":"opencode","source_kind":parts[0]["type"],"text":parts[0]["text"]}),
                json!({"type":"assistant","text":parts[1]["text"]}),
            ]
        };
        assert_eq!(records.len(), 3);
        // Rebuild this fixture's canonical record bytes from actual persisted
        // text, independently of the producer and its stored claim digest.
        let canonical: Vec<Value> = records.into_iter().map(|record| match record["type"].as_str().unwrap() {
            "user" => json!({"role":"user","text":record["text"]}),
            "assistant" => json!({"role":"assistant","text":record["text"]}),
            "reasoning" => {
                assert_eq!(record["provider"], "opencode");
                assert_eq!(record["source_kind"], "reasoning");
                json!({"provider":record["provider"],"role":"reasoning","source_kind":record["source_kind"],"text":record["text"]})
            }
            _ => panic!("unexpected record in fixed canonical fixture"),
        }).collect();
        let bytes = serde_json::to_vec(&canonical).unwrap();
        assert_eq!(bytes, COV2_CANONICAL.as_bytes());
        assert_eq!(hex::encode(Sha256::digest(&bytes)), COV2_CANONICAL_SHA);
        bytes
    }
}

#[cfg(target_os = "macos")]
fn macos_fake_exporter_script(export_body: &str) -> String {
    // The production Required runner owns both bounded process groups. This
    // fake speaks its readiness/stdin-lease protocol without a socket/model.
    format!(
        "#!/bin/sh\n\
        if [ \"$1\" = serve ]; then\n\
          while [ \"$#\" -gt 0 ]; do\n\
            if [ \"$1\" = --port ]; then shift; port=$1; fi\n\
            shift\n\
          done\n\
          printf '{{\"url\":\"http://127.0.0.1:%s/\"}}\\n' \"$port\"\n\
          while IFS= read -r line; do :; done\n\
          exit 0\n\
        fi\n\
        if [ \"$1\" != session ] || [ \"$2\" != export ]; then exit 73; fi\n\
        printf '%s\\n' \"$3\" >> \"$XDG_DATA_HOME/opencode/cov2-client-invocations\"\n\
        {export_body}\n"
    )
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
const EXPORT_HELLO: &str = r#"printf '{"info":{"id":"%s","directory":"__REPO__"},"messages":[{"info":{"role":"user","id":"msg_u1"},"parts":[{"type":"text","text":"hi"}]},{"info":{"role":"assistant","id":"msg_a1"},"parts":[{"type":"text","text":"hello"}]}]}' "$3""#;

const EXPORT_IMPORT: &str = r#"printf '%s' '{"info":{"id":"ses_import","directory":"__REPO__","status":"stopped","time":{"created":1784077200000,"updated":1784077260000}},"messages":[{"info":{"role":"user","id":"msg_u1","time":{"created":1784077210000}},"parts":[{"type":"text","text":"historical"}]},{"info":{"role":"assistant","id":"msg_a1","time":{"created":1784077250000}},"parts":[{"type":"text","text":"imported"}]}]}'"#;

const EXPORT_WRONG_SESSION: &str = r#"printf '%s' '{"info":{"id":"ses_other","directory":"__REPO__"},"messages":[{"info":{"role":"user","id":"msg_u1"},"parts":[{"type":"text","text":"wrong session"}]}]}'"#;

#[cfg(target_os = "macos")]
const COV2_EXPORT: &str = r#"printf '{"info":{"id":"%s","directory":"__REPO__","time":{"created":1784077200000,"updated":1784077260000}},"messages":[{"id":"u","type":"user","text":"hi"},{"id":"a-u","type":"assistant","content":[{"id":"r-u","type":"reasoning","text":"before sk-ant-EXAMPLE0000000000000000 after"},{"id":"t-u","type":"text","text":"hello"}]},{"id":"i-u","type":"idle","outcome":"succeeded"}]}' "$3""#;
#[cfg(target_os = "macos")]
const COV2_CANONICAL: &str = r#"[{"role":"user","text":"hi"},{"provider":"opencode","role":"reasoning","source_kind":"reasoning","text":"before <REDACTED:anthropic-api-key> after"},{"role":"assistant","text":"hello"}]"#;
#[cfg(target_os = "macos")]
const COV2_CANONICAL_SHA: &str = "7973604f43b515e1af6b1206e85c4fdc88447eed5a7822ca0ec3a77224b792a9";

/// M4 DR-05: the explicit-ID import command consumes the trusted sandboxed
/// export bridge and persists an import-channel checkpoint, not an export-job
/// side effect or a file-backed source.
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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
            "SELECT checkpoint.metadata_blob_oid \
             FROM agent_checkpoint AS checkpoint \
             JOIN agent_session AS session ON session.session_id = checkpoint.session_id \
             WHERE session.provider_session_id = 'ses_hmac_snapshot' \
             ORDER BY checkpoint.created_at DESC LIMIT 1",
        )
        .await;
    assert_eq!(rows.len(), 1, "successful export must have a checkpoint");
    let metadata_blob_oid: String = rows[0].try_get_by("metadata_blob_oid").unwrap();
    let metadata_blob = repo.run(&["cat-file", "-p", &metadata_blob_oid], None);
    assert!(
        metadata_blob.status.success(),
        "checkpoint metadata blob: {}",
        describe(&metadata_blob)
    );
    let metadata_json =
        String::from_utf8(metadata_blob.stdout).expect("checkpoint metadata blob is UTF-8 JSON");
    let metadata: Value =
        serde_json::from_str(&metadata_json).expect("OpenCode metadata is valid JSON");
    let Some(digest) = metadata["transcript_snapshot"]["source"]["digest_sha256"].as_str() else {
        let jobs = repo
            .query_rows(
                "SELECT state, observed_generation, processed_generation, last_error_code \
                 FROM agent_export_job WHERE provider_session_id = 'ses_hmac_snapshot'",
            )
            .await;
        let job = jobs.first().map(|row| {
            format!(
                "state={:?}, observed={:?}, processed={:?}, last_error={:?}",
                row.try_get_by::<String, _>("state"),
                row.try_get_by::<i64, _>("observed_generation"),
                row.try_get_by::<i64, _>("processed_generation"),
                row.try_get_by::<Option<String>, _>("last_error_code")
            )
        });
        let sessions = repo
            .query_rows(
                "SELECT state, sync_revision, stopped_at FROM agent_session \
                 WHERE provider_session_id = 'ses_hmac_snapshot'",
            )
            .await;
        let session = sessions.first().map(|row| {
            format!(
                "state={:?}, revision={:?}, stopped_at={:?}",
                row.try_get_by::<String, _>("state"),
                row.try_get_by::<i64, _>("sync_revision"),
                row.try_get_by::<Option<i64>, _>("stopped_at")
            )
        });
        let checkpoints = repo.checkpoints();
        panic!(
            "successful OpenCode snapshot has a durable source commitment; stdout: {}; metadata: {metadata_json}; job: {job:?}; session: {session:?}; checkpoints: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            checkpoints.len(),
            String::from_utf8_lossy(&output.stderr)
        );
    };
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_registered_failure_releases_dirty_job_claim_and_marker() {
    let Some(repo) = BridgeRepo::init(EXPORT_HELLO).await else {
        return;
    };
    repo.block_object_directory();
    // The hook only queues, so it succeeds. The consumer hits the blocked
    // object store, keeps the envelope in ready/ (a filesystem failure is
    // retryable) and reports the failure.
    let envelope = json!({
        "hook_event_name": "session.idle",
        "session_id": "ses_registered_failure",
        "cwd": repo.repo.to_string_lossy(),
    })
    .to_string();
    let queued = {
        let mut cmd = repo.command();
        cmd.env("LIBRA_TEST", "1")
            .env("LIBRA_TEST_CAPTURE_SPOOL_NO_CONSUMER", "1")
            .args(["agent", "hooks", "opencode", "stop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn libra");
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(envelope.as_bytes())
                .expect("write envelope");
        }
        child.wait_with_output().expect("wait libra")
    };
    assert!(queued.status.success(), "{}", describe(&queued));
    let failed = repo.run_no_drain(&["__capture-spool-drain"], None, Some("debug"));
    assert!(!failed.status.success(), "{}", describe(&failed));
    assert_eq!(
        std::fs::read_dir(repo.repo.join(".libra/private/capture-spool/ready"))
            .map(|entries| entries.count())
            .unwrap_or(0),
        1,
        "a retryable consumer failure keeps the envelope queued"
    );
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_export_plaintext_never_in_persist_or_logs() {
    let secret = "AKIAZZZZZZZZZZZZZZZZ";
    let body = format!(
        r#"printf '{{"info":{{"id":"%s","directory":"__REPO__"}},"messages":[{{"info":{{"role":"user","id":"msg_u1"}},"parts":[{{"type":"text","text":"use {secret} now"}}]}}]}}' "$3""#
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
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

/// The Required macOS pair must still reject a substituted exporter before spawn.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
#[serial_test::serial(opencode_export_bridge)]
async fn opencode_export_macos_requires_trust_before_spawn() {
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
        (rendered.contains("trust") || rendered.contains("provenance"))
            && !rendered.contains("must-not-execute"),
        "unexpected macOS rejection: {rendered}"
    );
    assert!(
        !marker.exists(),
        "macOS failure must occur before the exporter is spawned"
    );
}

/// Real local CLI hook, trusted fake exporter and Required Seatbelt pair.
/// All content and identities are synthetic; no native-origin claim is made.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn cov2_macos_fake_exporter_live_session_capture() {
    let repo = BridgeRepo::init(COV2_EXPORT)
        .await
        .expect("macOS prerequisites must fail, never skip");
    let first = repo.cov2_stop("ses_cov2_visible", "cov2-visible-idle-1");
    assert!(first.status.success(), "live hook: {}", describe(&first));
    assert_eq!(
        repo.cov2_canonical_checkpoint("export").await,
        COV2_CANONICAL.as_bytes()
    );
    assert_eq!(repo.checkpoints().len(), 1);
    assert!(
        repo.client_invocations()
            .lines()
            .any(|line| line == "ses_cov2_visible")
    );
    let before = repo.cov2_claim_state().await;
    let invocations_before_replay = repo.client_invocations();
    let replay = repo.cov2_stop("ses_cov2_visible", "cov2-visible-idle-1");
    assert!(
        replay.status.success(),
        "repeated hook: {}",
        describe(&replay)
    );
    assert_eq!(
        repo.checkpoints().len(),
        1,
        "repeated delivery is idempotent"
    );
    assert_eq!(repo.cov2_claim_state().await, before);
    assert_eq!(repo.client_invocations(), invocations_before_replay);
    // A new receipt over the same export reaches coverage idempotence too;
    // the same-event check above alone could stop at receipt deduplication.
    let fresh = repo.cov2_stop("ses_cov2_visible", "cov2-visible-idle-2");
    assert!(
        fresh.status.success(),
        "unchanged export under a fresh event: {}",
        describe(&fresh)
    );
    assert!(
        repo.client_invocations().lines().count() > invocations_before_replay.lines().count(),
        "a fresh event must execute the actual exporter"
    );
    assert_eq!(repo.checkpoints().len(), 1);
    assert_eq!(repo.cov2_claim_state().await, before);
    assert_eq!(
        repo.query_rows("SELECT revision FROM agent_coverage_revision")
            .await
            .len(),
        1
    );
    let jobs = repo
        .query_rows("SELECT observed_generation, processed_generation, owner FROM agent_export_job")
        .await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].try_get_by::<i64, _>("observed_generation").unwrap(),
        jobs[0]
            .try_get_by::<i64, _>("processed_generation")
            .unwrap()
    );
    assert!(
        jobs[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none()
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn cov2_macos_fake_exporter_import_session_capture() {
    // Separate stores ensure this import actually writes its own checkpoint;
    // cross-channel coverage in the live store cannot turn it into a no-op.
    let live = BridgeRepo::init(COV2_EXPORT)
        .await
        .expect("macOS prerequisites must fail, never skip");
    let out = live.cov2_stop("ses_cov2_visible", "cov2-parity-idle");
    assert!(
        out.status.success(),
        "live parity control: {}",
        describe(&out)
    );
    let live_bytes = live.cov2_canonical_checkpoint("export").await;

    let imported = BridgeRepo::init(COV2_EXPORT)
        .await
        .expect("macOS prerequisites must fail, never skip");
    let args = [
        "agent",
        "import",
        "--session",
        "ses_cov2_visible",
        "--agent",
        "opencode",
        "--yes",
        "--json",
    ];
    let first = imported.run(&args, None);
    assert!(
        first.status.success(),
        "actual import: {}",
        describe(&first)
    );
    let import_bytes = imported.cov2_canonical_checkpoint("import").await;
    assert_eq!(
        import_bytes, live_bytes,
        "actual persisted live/import canonical records agree"
    );
    assert_eq!(import_bytes, COV2_CANONICAL.as_bytes());
    assert!(
        imported
            .client_invocations()
            .lines()
            .any(|line| line == "ses_cov2_visible")
    );
    assert!(
        imported
            .query_rows("SELECT job_id FROM agent_export_job")
            .await
            .is_empty()
    );
    let before = imported.cov2_claim_state().await;
    let replay = imported.run(&args, None);
    assert!(
        replay.status.success(),
        "repeated import: {}",
        describe(&replay)
    );
    assert_eq!(
        imported.checkpoints().len(),
        1,
        "repeated import is idempotent"
    );
    assert_eq!(imported.cov2_claim_state().await, before);
    assert_eq!(
        imported
            .query_rows("SELECT revision FROM agent_coverage_revision")
            .await
            .len(),
        1
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn cov2_macos_failed_export_session_zero_write() {
    // A nonzero subprocess exit intentionally has a metadata-only fallback.
    // Instead return a trusted, identity-correct export that fails source
    // preparation. Session/job/pending metadata may exist, content may not.
    let body = r#"printf '{"info":{"id":"%s","directory":"__REPO__"},"messages":{}}' "$3""#;
    let repo = BridgeRepo::init(body)
        .await
        .expect("macOS prerequisites must fail, never skip");
    let objects_before = repo.object_files();
    let rejected = repo.cov2_stop("ses_cov2_failure", "cov2-rejection-idle");
    assert!(
        repo.client_invocations()
            .lines()
            .any(|line| line == "ses_cov2_failure"),
        "the real sandboxed exporter client executed"
    );
    assert!(
        !rejected.status.success(),
        "source rejection must fail the actual hook: {}",
        describe(&rejected)
    );
    // The public hook mapper deliberately hides the private source error.
    // Bind its fixed public refusal to the executed client, failed job and
    // pending receipt below; do not weaken that privacy boundary for a test.
    let error: Value = String::from_utf8_lossy(&rejected.stderr)
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("the failed hook emits its public error envelope");
    assert_eq!(error["error_code"], "LBR-INTERNAL-001");
    assert_eq!(error["exit_code"], 128);
    assert_eq!(
        error["message"],
        "agent hook ingestion failed: capture could not be completed; retry the hook or inspect the local repository"
    );
    assert!(String::from_utf8_lossy(&rejected.stdout).contains("SourceReadError"));
    assert!(repo.checkpoints().is_empty());
    assert!(
        repo.query_rows("SELECT checkpoint_id FROM agent_checkpoint")
            .await
            .is_empty()
    );
    assert!(
        repo.query_rows("SELECT state FROM agent_coverage_claim")
            .await
            .is_empty()
    );
    assert!(
        repo.query_rows("SELECT revision FROM agent_coverage_revision")
            .await
            .is_empty()
    );
    assert_eq!(
        repo.object_files(),
        objects_before,
        "no checkpoint content was published"
    );
    let sessions = repo
        .query_rows("SELECT metadata_json FROM agent_session")
        .await;
    assert_eq!(sessions.len(), 1, "the actual Session ingress ran");
    let metadata: Value = serde_json::from_str(
        &sessions[0]
            .try_get_by::<String, _>("metadata_json")
            .unwrap(),
    )
    .unwrap();
    let entries = metadata["capture_catalog_receipts_v1"]["entries"]
        .as_array()
        .expect("real receipt ledger");
    let checkpoint_receipts: Vec<_> = entries
        .iter()
        .filter(|entry| entry["intent"]["checkpoint"] != "none")
        .collect();
    assert_eq!(checkpoint_receipts.len(), 1);
    assert_eq!(checkpoint_receipts[0]["status"], "pending");
    assert_eq!(
        checkpoint_receipts
            .iter()
            .filter(|entry| entry["status"] == "complete")
            .count(),
        0
    );
    let jobs = repo
        .query_rows("SELECT state, owner FROM agent_export_job")
        .await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].try_get_by::<String, _>("state").unwrap(), "failed");
    assert!(
        jobs[0]
            .try_get_by::<Option<String>, _>("owner")
            .unwrap()
            .is_none()
    );
}

/// Actual live candidates remain partial without gaining native authority.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn cov2_macos_fake_exporter_candidate_live_partial_no_plaintext() {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};

    const WINDOW: &str = "COV15_PRIVATE_WINDOW_7A3E";
    const SIGNATURE: &str = "COV15_PRIVATE_SIGNATURE_4D9C";
    const REDACTED: &str = "COV15_PRIVATE_REDACTED_6B2F";
    const ENCRYPTED: &str = "COV15_PRIVATE_ENCRYPTED_8E1A";
    const PROOF: &str = "COV15_PRIVATE_PROOF_5C0D";

    fn assert_absent(bytes: &[u8], canaries: &[&str], sink: &str) {
        for canary in canaries {
            assert!(
                !bytes
                    .windows(canary.len())
                    .any(|part| part == canary.as_bytes()),
                "candidate canary reached {sink}"
            );
        }
    }

    fn assert_present(bytes: &[u8], canaries: &[&str], sink: &str) {
        for canary in canaries {
            assert!(
                bytes
                    .windows(canary.len())
                    .any(|part| part == canary.as_bytes()),
                "unproven reasoning field missing from {sink}"
            );
        }
    }

    fn read_object(repo: &BridgeRepo, oid: &str) -> Vec<u8> {
        let hash = ObjectHash::from_str(oid).expect("actual loose object id");
        libra::utils::object::read_git_object(&repo.repo.join(".libra"), &hash)
            .expect("decode actual published object")
    }

    fn tree_entries(tree: &[u8]) -> Vec<(bool, String, String)> {
        let mut entries = Vec::new();
        let mut cursor = 0;
        while cursor < tree.len() {
            let mode_end = cursor
                + tree[cursor..]
                    .iter()
                    .position(|byte| *byte == b' ')
                    .unwrap();
            let name_end = mode_end
                + 1
                + tree[mode_end + 1..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap();
            let oid_end = name_end + 21;
            assert!(oid_end <= tree.len());
            entries.push((
                &tree[cursor..mode_end] == b"40000",
                String::from_utf8(tree[mode_end + 1..name_end].to_vec()).unwrap(),
                hex::encode(&tree[name_end + 1..oid_end]),
            ));
            cursor = oid_end;
        }
        entries
    }

    fn checkpoint_blobs(
        repo: &BridgeRepo,
        checkpoint_id: &str,
        tree_oid: &str,
    ) -> Vec<(String, Vec<u8>)> {
        let mut oid = tree_oid.to_owned();
        for component in ["checkpoint", &checkpoint_id[..2], &checkpoint_id[2..]] {
            oid = tree_entries(&read_object(repo, &oid))
                .into_iter()
                .find(|(_, name, _)| name == component)
                .expect("actual checkpoint subtree")
                .2;
        }
        let mut pending = vec![(String::new(), oid)];
        let mut blobs = Vec::new();
        while let Some((prefix, tree)) = pending.pop() {
            for (is_tree, name, oid) in tree_entries(&read_object(repo, &tree)) {
                let path = format!("{prefix}{name}");
                if is_tree {
                    pending.push((format!("{path}/"), oid));
                } else {
                    blobs.push((path, read_object(repo, &oid)));
                }
            }
        }
        blobs
    }

    let state_fixture = json!({"info":{"id":"ses_cov15_state","location":{"directory":"__REPO__"}},
        "messages":[{"id":"u","type":"user","text":"cov15 safe request"},
            {"id":"a","type":"assistant","content":[
                {"id":"r","type":"reasoning","text":WINDOW,"state":{
                    "signature":SIGNATURE,"redactedData":REDACTED,"reasoningEncryptedContent":ENCRYPTED}},
                {"id":"t","type":"text","text":"cov15 safe reply"}]},
            {"id":"i","type":"idle","outcome":"succeeded"}]});
    let mut proof_fixture = state_fixture.clone();
    proof_fixture["info"]["id"] = json!("ses_cov15_proof");
    proof_fixture["messages"][1]["content"][0]["id"] =
        json!("npart_0123456789abcdef0123456789abcdef");
    proof_fixture["messages"][1]["content"][0]["originProof"] = json!({"nonce":PROOF});
    let state_body = format!("printf '%s' '{}'", state_fixture);
    let proof_body = format!("printf '%s' '{}'", proof_fixture);
    let repo = BridgeRepo::init(&state_body)
        .await
        .expect("Required macOS candidate fixture must run");
    let proof_repo = BridgeRepo::init(&proof_body)
        .await
        .expect("Required macOS proof fixture must run");

    for (repo, fixture, session_id, proof_bearing) in [
        (&repo, state_fixture, "ses_cov15_state", false),
        (&proof_repo, proof_fixture, "ses_cov15_proof", true),
    ] {
        let canaries = if proof_bearing {
            vec![WINDOW, SIGNATURE, REDACTED, ENCRYPTED, PROOF]
        } else {
            vec![WINDOW, SIGNATURE, REDACTED, ENCRYPTED]
        };
        let original = fixture
            .to_string()
            .replace("__REPO__", &repo.repo.to_string_lossy());
        for canary in &canaries {
            assert!(
                original.contains(canary),
                "fixture contains each sink probe"
            );
        }
        // This supporting preview destroys authority. Actual live admission is
        // established separately by the client marker, warning and catalog.
        let (ordinary, typed) = CaptureSnapshotService::preview_redacted_coverage_v2(
            "opencode",
            original.into_bytes(),
            CaptureSnapshotPolicy::default(),
        )
        .expect("bounded data-only candidate preview");
        assert_eq!(typed["partial"], true);
        assert_present(ordinary.bytes(), &canaries, "ordinary plaintext preview");
        let ordinary_json: Value = serde_json::from_slice(ordinary.bytes()).unwrap();
        assert_eq!(ordinary_json["messages"][0]["text"], "cov15 safe request");
        assert_eq!(
            ordinary_json["messages"][1]["content"][1]["text"],
            "cov15 safe reply"
        );
        assert_absent(
            &serde_json::to_vec(&typed).unwrap(),
            &canaries,
            "typed plaintext preview",
        );
        let records = typed["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap();
        assert_eq!(
            records.len(),
            2,
            "safe typed records are nonempty and exact"
        );
        assert_eq!(records[0]["type"], "user");
        assert_eq!(records[0]["text"], "cov15 safe request");
        assert_eq!(records[1]["type"], "assistant");
        assert_eq!(records[1]["text"], "cov15 safe reply");

        let objects_before = repo.object_files();
        let hook = repo.cov2_stop(session_id, &format!("cov15-{session_id}-idle"));
        assert_eq!(
            repo.client_invocations().lines().collect::<Vec<_>>(),
            [session_id]
        );
        let stderr = String::from_utf8_lossy(&hook.stderr);
        let live_logs = format!("{}{}", String::from_utf8_lossy(&hook.stdout), stderr);
        assert!(
            live_logs.contains("reasoning source is partial"),
            "actual live classifier warning is required: {}",
            describe(&hook)
        );
        assert!(
            live_logs.contains("unsupported_reasoning_shape"),
            "actual live partial reason is required"
        );
        assert_absent(&hook.stdout, &canaries, "live hook stdout");
        assert_absent(&hook.stderr, &canaries, "live logs and public errors");
        let sessions = repo
            .query_rows("SELECT session_id, metadata_json FROM agent_session")
            .await;
        assert_eq!(sessions.len(), 1, "actual Session ingress must run");
        let libra_session: String = sessions[0].try_get_by("session_id").unwrap();
        let metadata_text: String = sessions[0].try_get_by("metadata_json").unwrap();
        assert_absent(
            metadata_text.as_bytes(),
            &canaries,
            "durable Session metadata",
        );
        let metadata: Value = serde_json::from_str(&metadata_text).unwrap();
        let receipts = metadata["capture_catalog_receipts_v1"]["entries"]
            .as_array()
            .unwrap();
        assert_eq!(receipts.len(), 1, "real explicit-event receipt");
        let public = repo.run(
            &["agent", "session", "show", &libra_session, "--json"],
            None,
        );
        assert!(public.status.success(), "Session readback must run");
        assert_absent(&public.stdout, &canaries, "public Session readback");
        assert_absent(&public.stderr, &canaries, "public Session errors");
        assert!(
            repo.query_rows("SELECT state FROM agent_coverage_claim")
                .await
                .is_empty()
        );
        assert!(
            repo.query_rows("SELECT revision FROM agent_coverage_revision")
                .await
                .is_empty()
        );

        // Enrollment, identity and inflight stores are independently inspected;
        // absence of a checkpoint alone cannot prove no native artifact mint.
        assert!(
            repo.query_rows(
                "SELECT key FROM config_kv WHERE lower(key) = 'agent.native_origin.opencode.v1'"
            )
            .await
            .is_empty(),
            "no native issuer enrollment"
        );
        assert!(
            repo.query_rows("SELECT key FROM metadata_kv WHERE scope = 'agent_reasoning_identity'")
                .await
                .is_empty(),
            "no native reasoning identity"
        );
        assert!(
            repo.query_rows("SELECT key FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
                .await
                .is_empty(),
            "no retained unpublished object attempt"
        );
        for row in repo.query_rows("SELECT value FROM metadata_kv").await {
            let value: String = row.try_get_by("value").unwrap();
            assert_absent(value.as_bytes(), &canaries, "metadata_kv");
        }
        let checkpoints = repo
            .query_rows("SELECT checkpoint_id, tree_oid FROM agent_checkpoint")
            .await;
        if proof_bearing {
            assert!(
                !hook.status.success(),
                "unverified proof must refuse the live source"
            );
            let error: Value = stderr
                .lines()
                .find_map(|line| serde_json::from_str(line).ok())
                .unwrap();
            assert_eq!(error["error_code"], "LBR-INTERNAL-001");
            assert_eq!(
                error["message"],
                "agent hook ingestion failed: capture could not be completed; retry the hook or inspect the local repository"
            );
            assert_eq!(receipts[0]["status"], "pending");
            assert!(checkpoints.is_empty());
            assert_eq!(
                repo.object_files(),
                objects_before,
                "rejected proof publishes no objects"
            );
        } else {
            assert!(
                hook.status.success(),
                "safe state-only partial source persists"
            );
            assert_eq!(receipts[0]["status"], "complete");
            assert_eq!(checkpoints.len(), 1, "nonvacuous partial checkpoint");
            let id: String = checkpoints[0].try_get_by("checkpoint_id").unwrap();
            let tree: String = checkpoints[0].try_get_by("tree_oid").unwrap();
            let blobs = checkpoint_blobs(repo, &id, &tree);
            assert!(blobs.len() >= 6, "actual checkpoint roles are inspected");
            for (path, bytes) in &blobs {
                if path.contains("transcript") {
                    assert_present(bytes, &canaries, "decoded transcript");
                } else {
                    assert_absent(bytes, &canaries, "non-transcript checkpoint object");
                }
                assert!(
                    !path.starts_with("reasoning/"),
                    "no opaque reasoning object path"
                );
            }
            let manifest: Value = serde_json::from_slice(
                &blobs
                    .iter()
                    .find(|(path, _)| path == "manifest.json")
                    .unwrap()
                    .1,
            )
            .unwrap();
            assert_eq!(manifest["checkpoint_id"], id);
            assert!(
                manifest.get("reasoning_artifacts").is_none(),
                "no opaque artifact declared"
            );
            assert_eq!(manifest["entries"].as_object().unwrap().len(), 5);
            let stored_metadata: Value = serde_json::from_slice(
                &blobs
                    .iter()
                    .find(|(path, _)| path == "metadata.json")
                    .unwrap()
                    .1,
            )
            .unwrap();
            assert_eq!(
                stored_metadata["transcript_snapshot"]["completeness"], "partial",
                "actual live snapshot remains partial"
            );
            let exported = repo.run(&["agent", "checkpoint", "export", &id], None);
            assert!(
                exported.status.success(),
                "actual ordinary transcript readback"
            );
            assert_present(&exported.stdout, &canaries, "persisted ordinary plaintext");
            assert_absent(&exported.stderr, &canaries, "checkpoint public errors");
            let actual: Value = serde_json::from_slice(&exported.stdout).unwrap();
            assert_eq!(actual["messages"][0]["text"], "cov15 safe request");
            assert_eq!(
                actual["messages"][1]["content"][1]["text"],
                "cov15 safe reply"
            );
            assert!(actual["messages"][1]["content"][0].get("state").is_some());
        }
        // Decode every loose object, including anything not catalog-reachable.
        // A stored ordinary transcript is allowed to contain the unproven fields.
        let mut transcript_objects = 0usize;
        for path in repo.object_files() {
            let relative = path.strip_prefix(repo.repo.join(".libra/objects")).unwrap();
            let oid = relative
                .components()
                .map(|part| part.as_os_str().to_str().unwrap())
                .collect::<String>();
            assert_eq!(oid.len(), 40, "fixture only publishes loose SHA-1 objects");
            let bytes = read_object(repo, &oid);
            let hit = canaries.iter().any(|canary| {
                bytes
                    .windows(canary.len())
                    .any(|part| part == canary.as_bytes())
            });
            if !hit {
                continue;
            }
            assert!(
                !proof_bearing,
                "rejected proof published an unproven reasoning field"
            );
            let json_at = bytes
                .iter()
                .position(|byte| *byte == b'{')
                .expect("json object");
            let value: Value = serde_json::from_slice(&bytes[json_at..]).expect("transcript json");
            assert!(
                value.get("messages").is_some(),
                "unproven reasoning field left the transcript object"
            );
            transcript_objects += 1;
        }
        if !proof_bearing {
            assert!(
                transcript_objects >= 1,
                "stored transcript omitted unproven fields"
            );
        }
        let jobs = repo
            .query_rows("SELECT state, owner FROM agent_export_job")
            .await;
        assert_eq!(jobs.len(), 1, "actual export job exists");
        assert!(
            jobs[0]
                .try_get_by::<Option<String>, _>("owner")
                .unwrap()
                .is_none()
        );
        if proof_bearing {
            assert_eq!(jobs[0].try_get_by::<String, _>("state").unwrap(), "failed");
        }
    }
}

/// Historical candidate imports refuse before gaining native authority.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
async fn cov2_macos_fake_exporter_candidate_import_partial_no_plaintext() {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use libra::internal::ai::capture::snapshot::{CaptureSnapshotPolicy, CaptureSnapshotService};

    const WINDOW: &str = "COV18_PRIVATE_WINDOW_7A3E";
    const SIGNATURE: &str = "COV18_PRIVATE_SIGNATURE_4D9C";
    const REDACTED: &str = "COV18_PRIVATE_REDACTED_6B2F";
    const ENCRYPTED: &str = "COV18_PRIVATE_ENCRYPTED_8E1A";
    const PROOF: &str = "COV18_PRIVATE_PROOF_5C0D";

    fn assert_absent(bytes: &[u8], canaries: &[&str], sink: &str) {
        for canary in canaries {
            assert!(
                !bytes
                    .windows(canary.len())
                    .any(|part| part == canary.as_bytes()),
                "candidate canary reached {sink}"
            );
        }
    }

    fn assert_present(bytes: &[u8], canaries: &[&str], sink: &str) {
        for canary in canaries {
            assert!(
                bytes
                    .windows(canary.len())
                    .any(|part| part == canary.as_bytes()),
                "unproven reasoning field missing from {sink}"
            );
        }
    }

    fn read_object(repo: &BridgeRepo, oid: &str) -> Vec<u8> {
        let hash = ObjectHash::from_str(oid).expect("actual loose object id");
        libra::utils::object::read_git_object(&repo.repo.join(".libra"), &hash)
            .expect("decode actual published object")
    }

    let state_fixture = json!({"info":{"id":"ses_cov18_state","location":{"directory":"__REPO__"}},
        "messages":[{"id":"u","type":"user","text":"cov18 safe request"},
            {"id":"a","type":"assistant","content":[
                {"id":"r","type":"reasoning","text":WINDOW,"state":{
                    "signature":SIGNATURE,"redactedData":REDACTED,"reasoningEncryptedContent":ENCRYPTED}},
                {"id":"t","type":"text","text":"cov18 safe reply"}]},
            {"id":"i","type":"idle","outcome":"succeeded"}]});
    let mut proof_fixture = state_fixture.clone();
    proof_fixture["info"]["id"] = json!("ses_cov18_proof");
    proof_fixture["messages"][1]["content"][0]["id"] =
        json!("npart_0123456789abcdef0123456789abcdef");
    proof_fixture["messages"][1]["content"][0]["originProof"] = json!({"nonce":PROOF});
    let mut control_fixture = state_fixture.clone();
    control_fixture["info"]["id"] = json!("ses_cov18_control");
    control_fixture["messages"][0]["text"] = json!("hi");
    control_fixture["messages"][1]["content"][0]
        .as_object_mut()
        .unwrap()
        .remove("state");
    control_fixture["messages"][1]["content"][0]["text"] =
        json!("before sk-ant-EXAMPLE0000000000000000 after");
    control_fixture["messages"][1]["content"][1]["text"] = json!("hello");
    let state_body = format!("printf '%s' '{}'", state_fixture);
    let proof_body = format!("printf '%s' '{}'", proof_fixture);
    let control_body = format!("printf '%s' '{}'", control_fixture);
    let repo = BridgeRepo::init(&state_body)
        .await
        .expect("Required macOS import fixture must run");
    let proof_repo = BridgeRepo::init(&proof_body)
        .await
        .expect("Required macOS proof import fixture must run");
    let control_repo = BridgeRepo::init(&control_body)
        .await
        .expect("Required macOS ordinary import control must run");
    // The same Required fixture and actual import CLI must also accept an
    // ordinary visible source. Its existing independent gold binds a real
    // committed import, rather than treating all exporter failures as safe.
    let control_source = control_fixture
        .to_string()
        .replace("__REPO__", &control_repo.repo.to_string_lossy());
    assert_absent(
        control_source.as_bytes(),
        &[WINDOW, SIGNATURE, REDACTED, ENCRYPTED, PROOF],
        "ordinary control fixture",
    );
    let (_, control_preview) = CaptureSnapshotService::preview_redacted_coverage_v2(
        "opencode",
        control_source.into_bytes(),
        CaptureSnapshotPolicy::default(),
    )
    .expect("ordinary control preview");
    assert_eq!(
        control_preview["partial"], false,
        "idle control is complete before import's active-session fence"
    );
    let control = control_repo.run(
        &[
            "agent",
            "import",
            "--session",
            "ses_cov18_control",
            "--agent",
            "opencode",
            "--yes",
            "--json",
        ],
        None,
    );
    assert!(
        control.status.success(),
        "ordinary import control: {}",
        describe(&control)
    );
    assert_eq!(
        control_repo
            .client_invocations()
            .lines()
            .collect::<Vec<_>>(),
        ["ses_cov18_control"]
    );
    assert_eq!(
        control_repo.cov2_canonical_checkpoint("import").await,
        COV2_CANONICAL.as_bytes()
    );
    assert!(
        control_repo
            .query_rows("SELECT job_id FROM agent_export_job")
            .await
            .is_empty()
    );

    for (repo, fixture, session_id, proof_bearing) in [
        (&repo, state_fixture, "ses_cov18_state", false),
        (&proof_repo, proof_fixture, "ses_cov18_proof", true),
    ] {
        let canaries = if proof_bearing {
            vec![WINDOW, SIGNATURE, REDACTED, ENCRYPTED, PROOF]
        } else {
            vec![WINDOW, SIGNATURE, REDACTED, ENCRYPTED]
        };
        let original = fixture
            .to_string()
            .replace("__REPO__", &repo.repo.to_string_lossy());
        for canary in &canaries {
            assert!(
                original.contains(canary),
                "fixture contains each sink probe"
            );
        }
        // Supporting data-only observations destroy authority. The import
        // helper deliberately hides its warning and rejects nonordinary
        // dispositions before returning any snapshot or creating a Session.
        let (ordinary, typed) = CaptureSnapshotService::preview_redacted_coverage_v2(
            "opencode",
            original.into_bytes(),
            CaptureSnapshotPolicy::default(),
        )
        .expect("bounded data-only candidate preview");
        assert_eq!(typed["partial"], true);
        assert_present(ordinary.bytes(), &canaries, "ordinary plaintext preview");
        let ordinary_json: Value = serde_json::from_slice(ordinary.bytes()).unwrap();
        assert_eq!(ordinary_json["messages"][0]["text"], "cov18 safe request");
        assert_eq!(
            ordinary_json["messages"][1]["content"][1]["text"],
            "cov18 safe reply"
        );
        assert_absent(
            &serde_json::to_vec(&typed).unwrap(),
            &canaries,
            "typed plaintext preview",
        );
        let records = typed["turns"][0]["projection"]["records"]
            .as_array()
            .unwrap();
        assert_eq!(records.len(), 2, "nonempty exact safe typed records");
        assert_eq!(records[0]["type"], "user");
        assert_eq!(records[0]["text"], "cov18 safe request");
        assert_eq!(records[1]["type"], "assistant");
        assert_eq!(records[1]["text"], "cov18 safe reply");

        let objects_before = repo.object_files();
        let imported = repo.run_with_log(
            &[
                "agent",
                "import",
                "--session",
                session_id,
                "--agent",
                "opencode",
                "--yes",
                "--json",
            ],
            None,
            Some("debug"),
        );
        assert_eq!(
            repo.client_invocations().lines().collect::<Vec<_>>(),
            [session_id],
            "actual trusted historical-export client must execute"
        );
        assert!(
            !imported.status.success(),
            "candidate import must refuse: {}",
            describe(&imported)
        );
        assert_absent(&imported.stdout, &canaries, "actual import stdout/logs");
        assert_absent(
            &imported.stderr,
            &canaries,
            "actual import public errors/logs",
        );
        let stderr = String::from_utf8(imported.stderr.clone()).unwrap();
        let (notice, envelope) = stderr.split_once('\n').expect("existing import notice");
        assert_eq!(
            notice,
            "Provider-encrypted reasoning original bytes may be archived without decryption and mirrored to cloud storage with their checkpoint; session erasure hides the references and local GC reclaims unreachable objects. Native source proof is required; unproved fields are not archived."
        );
        let error: Value = serde_json::from_str(envelope).expect("fixed public import refusal");
        assert_eq!(error["error_code"], "LBR-AGENT-020");
        assert_eq!(
            error["message"],
            "agent import failed; resolve the reported stable error code and retry (run `libra agent doctor --repair` if object-index repair cannot complete)"
        );
        assert_eq!(error["details"]["succeeded"], 0);
        assert_eq!(error["details"]["partial"], 0);
        assert_eq!(error["details"]["failed"], 1);
        assert!(error["details"]["next_cursor"].is_null());
        assert_eq!(error["details"]["failures"].as_array().unwrap().len(), 1);
        assert_eq!(
            error["details"]["failures"][0]["error_code"],
            "LBR-AGENT-020"
        );

        // Import's rejection precedes its identity/cursor and Session writes;
        // it must never inherit live's export-job or pending-receipt behavior.
        assert!(
            repo.query_rows("SELECT session_id FROM agent_session")
                .await
                .is_empty()
        );
        assert!(
            repo.query_rows("SELECT identity_id, state, next_ordinal FROM agent_import_identity")
                .await
                .is_empty(),
            "no import identity/cursor advance"
        );
        assert!(
            repo.query_rows("SELECT job_id FROM agent_export_job")
                .await
                .is_empty(),
            "historical import never creates an export job"
        );
        assert!(
            repo.query_rows("SELECT checkpoint_id FROM agent_checkpoint")
                .await
                .is_empty()
        );
        assert!(repo.checkpoints().is_empty());
        assert!(
            repo.query_rows("SELECT source_channel FROM agent_coverage_claim")
                .await
                .is_empty()
        );
        assert!(
            repo.query_rows("SELECT revision FROM agent_coverage_revision")
                .await
                .is_empty()
        );
        assert_eq!(
            repo.object_files(),
            objects_before,
            "rejected import publishes no object"
        );

        // Inspect authority/publication stores independently of empty catalog.
        assert!(
            repo.query_rows(
                "SELECT key FROM config_kv WHERE lower(key) = 'agent.native_origin.opencode.v1'"
            )
            .await
            .is_empty(),
            "no native issuer enrollment"
        );
        assert!(
            repo.query_rows("SELECT key FROM metadata_kv WHERE scope = 'agent_reasoning_identity'")
                .await
                .is_empty(),
            "no native reasoning identity"
        );
        assert!(
            repo.query_rows("SELECT key FROM metadata_kv WHERE scope = 'agent_traces_inflight'")
                .await
                .is_empty(),
            "no unpublished artifact attempt"
        );
        assert!(
            repo.query_rows("SELECT key FROM metadata_kv WHERE scope LIKE 'agent_import%'")
                .await
                .is_empty(),
            "no import index/cursor repair artifact"
        );
        for row in repo.query_rows("SELECT value FROM metadata_kv").await {
            let value: String = row.try_get_by("value").unwrap();
            assert_absent(value.as_bytes(), &canaries, "persisted import metadata");
        }
        // Scan any loose object even if the catalog cannot reach it. Any
        // unexpected manifest/artifact is independently rejected as well.
        for path in repo.object_files() {
            let relative = path.strip_prefix(repo.repo.join(".libra/objects")).unwrap();
            let oid = relative
                .components()
                .map(|part| part.as_os_str().to_str().unwrap())
                .collect::<String>();
            assert_eq!(oid.len(), 40, "fixture only publishes loose SHA-1 objects");
            let bytes = read_object(repo, &oid);
            assert_absent(
                &bytes,
                &canaries,
                "all decoded persisted import/checkpoint objects",
            );
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                assert!(
                    value.get("reasoning_artifacts").is_none(),
                    "no opaque artifact manifest"
                );
            }
        }
    }
}

/// plan-20261116 CQ-05 (local capture test): the OpenCode forwarder hook
/// returns as soon as its event is queued. It runs no exporter and opens no
/// database, so it does not wait for the export window. Like every case in
/// this `test-live-agent` target it is an ignored, operator-local gate: run it
/// with an explicit ignored-test selection; it never skips once selected.
#[test]
#[ignore = "OpenCode Session capture is an operator-local manual gate"]
fn capture_spool_opencode_forwarder_returns_after_queueing() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::create_dir_all(&home).expect("home dir");
    let repo = repo.canonicalize().expect("canonical repo");
    let command = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
            .current_dir(&repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("LIBRA_TEST_HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("LIBRA_TEST_CAPTURE_SPOOL_NO_CONSUMER", "1");
        cmd
    };
    let init = command(&["init"]).output().expect("libra init");
    assert!(init.status.success(), "{}", describe(&init));
    let envelope = json!({
        "hook_event_name": "session.idle",
        "session_id": "ses_spool_forwarder",
        "cwd": repo.to_string_lossy(),
    })
    .to_string();
    let started = std::time::Instant::now();
    let mut child = command(&["agent", "hooks", "opencode", "stop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn OpenCode hook");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(envelope.as_bytes())
            .expect("write envelope");
    }
    let out = child.wait_with_output().expect("wait OpenCode hook");
    let elapsed = started.elapsed();
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the forwarder must return after queueing, not after an export: {elapsed:?}"
    );
    let ready = std::fs::read_dir(repo.join(".libra/private/capture-spool/ready"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(ready, 1, "the OpenCode event must be queued");
    assert!(
        !repo.join(".libra/private/capture.db").exists(),
        "queueing must not open the capture catalog or start an export"
    );
}
