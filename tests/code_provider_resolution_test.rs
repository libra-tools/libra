//! plan-20260825 PS-06 + PS-03: end-to-end zero/one/many credential
//! detection for `libra code` without `--provider`, and the persisted
//! `code.defaultProvider` config slot ahead of that detection.
//!
//! **Layer:** L1 — deterministic. Every run gets an isolated HOME and an
//! isolated `LIBRA_CONFIG_GLOBAL_DB`, and the six auto-selectable provider
//! keys are removed from the child environment, so ambient developer
//! credentials cannot leak into the verdicts.

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const AUTO_SELECTABLE_KEYS: [&str; 6] = [
    "GEMINI_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "DEEPSEEK_API_KEY",
    "MOONSHOT_API_KEY",
    "ZHIPU_API_KEY",
];

struct Repo {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
    global_db: std::path::PathBuf,
}

fn init_repo() -> Repo {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("repo");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&root).expect("repo dir");
    std::fs::create_dir_all(home.join(".config")).expect("isolated HOME");
    let global_db = temp.path().join("global-config.db");

    let status = base_command(&home, &global_db)
        .arg("init")
        .current_dir(&root)
        .status()
        .expect("libra init");
    assert!(status.success(), "libra init failed");
    Repo {
        _temp: temp,
        root,
        home,
        global_db,
    }
}

fn base_command(home: &std::path::Path, global_db: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USERPROFILE", home)
        .env("LIBRA_CONFIG_GLOBAL_DB", global_db)
        .env("LIBRA_TEST", "1");
    for key in AUTO_SELECTABLE_KEYS {
        cmd.env_remove(key);
    }
    cmd
}

#[test]
fn zero_candidates_exit_auth_with_the_a_mode_guidance() {
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .output()
        .expect("run zero-candidate probe");
    assert_eq!(
        out.status.code(),
        Some(128),
        "zero candidates must exit with the LBR-AUTH-001 code; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no provider credentials configured")
            && stderr.contains("Checked in order:")
            && stderr.contains("    libra code --provider codex")
            && stderr.contains("    libra code --provider ollama --model <name>"),
        "a-mode zero-state guidance missing: {stderr}"
    );
    assert!(
        stderr.contains("Error-Code: LBR-AUTH-001"),
        "stable code missing: {stderr}"
    );
}

#[test]
fn many_candidates_exit_usage_listing_sorted_candidates_and_layers() {
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("ZHIPU_API_KEY", "probe-zhipu")
        .env("DEEPSEEK_API_KEY", "probe-deepseek")
        .output()
        .expect("run many-candidate probe");
    assert_eq!(
        out.status.code(),
        Some(129),
        "ambiguity must exit with the LBR-CLI-002 code; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let deepseek = stderr
        .find("    deepseek  (DEEPSEEK_API_KEY in process environment)")
        .unwrap_or_else(|| panic!("deepseek candidate line missing: {stderr}"));
    let zhipu = stderr
        .find("    zhipu  (ZHIPU_API_KEY in process environment)")
        .unwrap_or_else(|| panic!("zhipu candidate line missing: {stderr}"));
    assert!(deepseek < zhipu, "candidates must be id-sorted: {stderr}");
    assert!(
        !stderr.contains("probe-zhipu") && !stderr.contains("probe-deepseek"),
        "key values must never appear (GC-PS-01): {stderr}"
    );
}

#[test]
fn one_candidate_auto_selects_announcing_on_stderr_only() {
    let repo = init_repo();
    let mut child = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start single-candidate launch");

    struct KillOnDrop(Option<std::process::Child>);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let stderr = child.stderr.take().expect("stderr pipe");
    let stdout = child.stdout.take().expect("stdout pipe");
    let mut guard = KillOnDrop(Some(child));

    // The auto-selection note precedes the web boot on stderr.
    let (note_tx, note_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = note_tx.send(line);
        }
    });
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = out_tx.send(line);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut saw_note = false;
    while Instant::now() < deadline && !saw_note {
        if let Ok(line) = note_rx.recv_timeout(Duration::from_millis(200)) {
            assert!(
                !line.contains("probe-gemini"),
                "key value on stderr (GC-PS-01): {line}"
            );
            if line.contains("provider 'gemini' auto-selected")
                && line.contains("GEMINI_API_KEY")
                && line.contains("process environment")
            {
                saw_note = true;
            }
        }
    }
    assert!(saw_note, "auto-selection note did not appear on stderr");

    // stdout must not carry the note (it belongs to diagnostics).
    while let Ok(line) = out_rx.recv_timeout(Duration::from_millis(500)) {
        assert!(
            !line.contains("auto-selected"),
            "auto-selection note leaked to stdout: {line}"
        );
        if line.contains("http://") {
            break; // web boot reached; detection is done
        }
    }

    if let Some(child) = guard.0.as_mut() {
        let _ = child.kill();
    }
}

#[test]
fn model_without_provider_is_rejected_before_env_file_io() {
    // PS-06 terra R1: the pairing guard must precede env-file reading —
    // pointing --env-file at a DIRECTORY would IO-error if the file were
    // read first, so a 129 usage error here proves the ordering.
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--model",
            "arbitrary-model",
            "--env-file",
            ".",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .output()
        .expect("run model-without-provider probe");
    assert_eq!(
        out.status.code(),
        Some(129),
        "pairing guard must fire before env-file IO; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--model without --provider"),
        "pairing message missing: {stderr}"
    );
    assert!(
        !stderr.contains("env-file") || !stderr.contains("LBR-IO-001"),
        "env-file IO must not have been attempted: {stderr}"
    );
}

#[test]
fn explicit_provider_skips_detection_probes() {
    // PS-06 terra R1: with LIBRA_CONFIG_GLOBAL_DB poisoned (a directory),
    // any detection probe would fail loudly with "credential detection
    // failed"; an explicit --provider must therefore never mention it and
    // instead reach the provider's own missing-key a-mode error.
    let repo = init_repo();
    let poisoned = repo.home.join("poisoned-db-dir");
    std::fs::create_dir_all(&poisoned).expect("poisoned dir");
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--provider",
            "gemini",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .env("LIBRA_CONFIG_GLOBAL_DB", &poisoned)
        .output()
        .expect("run explicit-provider probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("credential detection failed"),
        "explicit --provider must not run detection probes: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "explicit gemini without a key lands on its own auth error: {stderr}"
    );
    assert!(
        stderr.contains("GEMINI_API_KEY is not configured for provider 'gemini'"),
        "the provider-specific a-mode error is expected: {stderr}"
    );
}

#[test]
fn many_candidates_report_the_global_vault_layer() {
    // PS-06 terra R1: the repo/global layer labels are user-facing — cover
    // the global-vault label end to end by storing one key in the isolated
    // global DB and one in the process environment.
    let repo = init_repo();
    let set = base_command(&repo.home, &repo.global_db)
        .args([
            "config",
            "set",
            "--global",
            "vault.env.ZHIPU_API_KEY",
            "layer-probe-zhipu",
        ])
        .current_dir(&repo.root)
        .output()
        .expect("store global vault key");
    assert!(
        set.status.success(),
        "config set --global failed: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("DEEPSEEK_API_KEY", "probe-deepseek")
        .output()
        .expect("run mixed-layer probe");
    assert_eq!(out.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("    deepseek  (DEEPSEEK_API_KEY in process environment)")
            && stderr.contains("    zhipu  (ZHIPU_API_KEY in global vault)"),
        "layer labels must distinguish the sources: {stderr}"
    );
    assert!(
        !stderr.contains("layer-probe-zhipu") && !stderr.contains("probe-deepseek"),
        "values must never leak: {stderr}"
    );
}

#[test]
fn session_target_repo_vault_drives_detection_not_caller_cwd() {
    // PS-06 terra R2: running `libra code --repo B` from repository A must
    // detect against B's repo-local vault. A configures gemini locally,
    // B configures deepseek locally; the session targets B, so deepseek
    // must be the auto-selected candidate with the repo-local layer label.
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".config")).expect("home");
    let global_db = temp.path().join("global.db");
    let repo_a = temp.path().join("a");
    let repo_b = temp.path().join("b");
    for repo in [&repo_a, &repo_b] {
        std::fs::create_dir_all(repo).expect("repo dir");
        let st = base_command(&home, &global_db)
            .arg("init")
            .current_dir(repo)
            .status()
            .expect("init");
        assert!(st.success());
    }
    for (repo, key) in [
        (&repo_a, "vault.env.GEMINI_API_KEY"),
        (&repo_b, "vault.env.DEEPSEEK_API_KEY"),
    ] {
        let st = base_command(&home, &global_db)
            .args(["config", "set", key, "local-probe"])
            .current_dir(repo)
            .status()
            .expect("config set local");
        assert!(st.success(), "local config set failed in {repo:?}");
    }

    let out = base_command(&home, &global_db)
        .args(["code", "--repo"])
        .arg(&repo_b)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(&repo_a)
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stderr.contains("provider 'deepseek' auto-selected")
            && stderr.contains("DEEPSEEK_API_KEY")
            && stderr.contains("repo-local vault"),
        "session target B's vault must drive detection: {stderr}"
    );
    assert!(
        !stderr.contains("gemini' auto-selected"),
        "caller repo A's vault must not leak into the session: {stderr}"
    );
}

#[test]
fn machine_mode_announces_auto_selection_as_structured_event() {
    // PS-06 terra R2 (ADR-PS-03 ⑤): machine surfaces must not get prose.
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["--machine", "code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    let event_line = stderr
        .lines()
        .find(|l| l.contains("provider_auto_selected"))
        .unwrap_or_else(|| panic!("structured event missing: {stderr}"));
    let parsed: serde_json::Value =
        serde_json::from_str(event_line).expect("event line must be valid JSON");
    assert_eq!(parsed["provider"], "gemini");
    assert_eq!(parsed["api_key_env"], "GEMINI_API_KEY");
    assert_eq!(parsed["layer"], "process environment");
    assert!(
        !stderr.contains("auto-selected:"),
        "prose form must not appear under --machine: {stderr}"
    );
}

#[test]
fn agent_binding_labels_follow_the_effective_provider() {
    // PS-04: with an `--agent` profile whose structured binding selects
    // zhipu, the startup banner must label the session with the binding's
    // provider (and detection stays silent — the binding decided, so no
    // auto-selection note may appear).
    let repo = init_repo();
    let agents_dir = repo.root.join(".libra").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    std::fs::write(
        agents_dir.join("labeler.md"),
        "---\nname: labeler\ndescription: Label probe\ntools: []\nmodel: zhipu/glm-4.7-probe\n---\nYou label.",
    )
    .expect("write agent profile");
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--agent",
            "labeler",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .env("ZHIPU_API_KEY", "probe-zhipu")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stdout = String::from_utf8_lossy(&out.0);
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stdout.contains("Provider: Zhipu"),
        "banner must carry the binding's provider; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("auto-selected"),
        "the binding decided — detection must stay silent: {stderr}"
    );
    assert!(
        !stdout.contains("probe-zhipu") && !stderr.contains("probe-zhipu"),
        "key values must never leak (GC-PS-01)"
    );
}

/// Seed a resumable session directly through the library store (the same
/// store the CLI reads), returning the thread id to pass to `--resume`.
/// `provider`/`model` seed the PS-05 provenance metadata when given.
fn seed_resumable_session(repo: &Repo, provider: Option<&str>, model: Option<&str>) -> String {
    use libra::internal::ai::session::{SessionState, SessionStore};
    // The CLI's working dir comes from getcwd, which resolves symlinks
    // (macOS /var → /private/var), so the recorded working_dir must be the
    // canonical form for load_for_thread_id to match.
    let cli_dir = repo.root.canonicalize().expect("canonical repo root");
    let store = SessionStore::from_storage_path(&repo.root.join(".libra"));
    let mut session = SessionState::new(&cli_dir.to_string_lossy());
    if let Some(provider) = provider {
        session
            .metadata
            .insert("provider".to_string(), serde_json::json!(provider));
    }
    if let Some(model) = model {
        session
            .metadata
            .insert("model".to_string(), serde_json::json!(model));
    }
    store.save(&session).expect("save seeded session");
    session.id
}

#[test]
fn resume_inherits_provider_over_config_and_detection() {
    // PS-05: the recorded provider outranks BOTH the persisted config
    // default and a unique detection candidate — and a recorded provider
    // missing its key errors on ITS OWN key instead of silently switching
    // models (the manual VER scenario, automated).
    let repo = init_repo();
    let thread_id = seed_resumable_session(&repo, Some("zhipu"), Some("glm-recorded"));
    set_default_provider(&repo, true, "gemini");
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--resume"])
        .arg(&thread_id)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(repo.root.canonicalize().expect("canonical root"))
        .env("GEMINI_API_KEY", "probe-gemini")
        .output()
        .expect("run resume-inheritance probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(128),
        "the inherited provider's own auth error is expected: {stderr}"
    );
    assert!(
        stderr.contains("ZHIPU_API_KEY is not configured for provider 'zhipu'"),
        "the recorded provider must drive the run: {stderr}"
    );
    assert!(
        !stderr.contains("auto-selected"),
        "neither detection nor the config default may fire on a resume hit: {stderr}"
    );
}

#[test]
fn resume_inherits_provider_missing_metadata_falls_through() {
    // PS-05: a pre-PS-05 session without recorded provenance keeps
    // resolving through the remaining chain (here: single-candidate
    // detection) — never fatal.
    let repo = init_repo();
    let thread_id = seed_resumable_session(&repo, None, None);
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--resume"])
        .arg(&thread_id)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(repo.root.canonicalize().expect("canonical root"))
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stderr.contains("provider 'gemini' auto-selected"),
        "metadata-less resume must fall through to detection: {stderr}"
    );
    assert!(
        !stderr.contains("provider_resume_mismatch") && !stderr.contains("was recorded with"),
        "no mismatch warning without a record: {stderr}"
    );

    // The boot must have stamped the previously-recordless session with
    // the provider that actually drove it (insert-if-absent).
    use libra::internal::ai::session::SessionStore;
    let store = SessionStore::from_storage_path(&repo.root.join(".libra"));
    let session = store.load(&thread_id).expect("reload resumed session");
    assert_eq!(
        session.recorded_provider_id(),
        Some("gemini"),
        "boot must record the effective provider on the session"
    );
    assert!(
        session.recorded_model_id().is_some(),
        "boot must record the paired model id"
    );
}

#[test]
fn resume_inherits_provider_provenance_recorded_on_new_session() {
    // PS-05 AC1: a brand-new session records its effective provider and
    // model ids in the session metadata at first boot — ids only, no
    // credential material anywhere in the session file.
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    use libra::internal::ai::session::SessionStore;
    let store = SessionStore::from_storage_path(&repo.root.join(".libra"));
    let session = store
        .load_latest()
        .expect("list sessions")
        .unwrap_or_else(|| panic!("the boot must have created a session; stderr: {stderr}"));
    assert_eq!(session.recorded_provider_id(), Some("gemini"));
    assert!(session.recorded_model_id().is_some());
    let raw = serde_json::to_string(&session.metadata).expect("serialize metadata");
    assert!(
        !raw.contains("probe-gemini"),
        "session metadata must never contain credential material: {raw}"
    );
}

#[test]
fn resume_inherits_provider_mismatch_warns_and_continues() {
    // PS-05: an explicit provider that disagrees with the record warns on
    // stderr (ids only) and continues; under --machine the warning is a
    // structured event, not prose.
    let repo = init_repo();
    let thread_id = seed_resumable_session(&repo, Some("zhipu"), Some("glm-recorded"));
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--resume"])
        .arg(&thread_id)
        .args(["--provider", "gemini", "--port", "0", "--mcp-port", "0"])
        .current_dir(repo.root.canonicalize().expect("canonical root"))
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stderr.contains("was recorded with provider")
            && stderr.contains("'zhipu'")
            && stderr.contains("'gemini'"),
        "the mismatch warning must name both ids: {stderr}"
    );
    assert!(
        !stderr.contains("probe-gemini"),
        "no key material may appear (GC-PS-01): {stderr}"
    );
    assert!(
        stderr.contains("http://") || String::from_utf8_lossy(&out.0).contains("http://"),
        "the run must continue past the warning: {stderr}"
    );

    let out = base_command(&repo.home, &repo.global_db)
        .args(["--machine", "code", "--resume"])
        .arg(&thread_id)
        .args(["--provider", "gemini", "--port", "0", "--mcp-port", "0"])
        .current_dir(repo.root.canonicalize().expect("canonical root"))
        .env("GEMINI_API_KEY", "probe-gemini")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    let event_line = stderr
        .lines()
        .find(|line| line.contains("provider_resume_mismatch"))
        .unwrap_or_else(|| panic!("structured mismatch event missing: {stderr}"));
    let parsed: serde_json::Value =
        serde_json::from_str(event_line).expect("event line must be valid JSON");
    assert_eq!(parsed["recorded"], "zhipu");
    assert_eq!(parsed["using"], "gemini");
    assert!(
        !stderr.contains("was recorded with provider"),
        "prose form must not appear under --machine: {stderr}"
    );
}

#[test]
fn resume_inherits_provider_corrupt_record_never_leaks_and_falls_through() {
    // PS-05 terra R1: a corrupt recorded provider id (which could be a
    // mistakenly pasted secret) must neither surface anywhere in the
    // output — including tracing at LIBRA_LOG=warn — nor break the run:
    // the resolution falls through to detection.
    let repo = init_repo();
    let thread_id = seed_resumable_session(&repo, Some("sk-SENTINEL-corrupt-record"), None);
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--resume"])
        .arg(&thread_id)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(repo.root.canonicalize().expect("canonical root"))
        .env("GEMINI_API_KEY", "probe-gemini")
        .env("LIBRA_LOG", "warn")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stdout = String::from_utf8_lossy(&out.0);
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        !stdout.contains("SENTINEL") && !stderr.contains("SENTINEL"),
        "the corrupt recorded value must never be echoed (GC-PS-01);\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("provider 'gemini' auto-selected"),
        "a corrupt record must fall through to detection: {stderr}"
    );
}

#[test]
fn resume_inherits_provider_and_model_for_model_requiring_provider() {
    // PS-05 model inheritance: ollama has no default model, so a bare
    // `--provider ollama` launch would be a usage error — a resumed
    // thread recorded as ollama/llama-recorded must boot past that guard
    // by inheriting the model together with the provider.
    let repo = init_repo();
    let thread_id = seed_resumable_session(&repo, Some("ollama"), Some("llama-recorded"));
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--resume"])
        .arg(&thread_id)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(repo.root.canonicalize().expect("canonical root"))
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stdout = String::from_utf8_lossy(&out.0);
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stdout.contains("Provider: Ollama"),
        "the recorded provider must drive the boot; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("Model is required") && !stderr.contains("--model"),
        "the recorded model must satisfy ollama's explicit-model requirement: {stderr}"
    );
}

/// Spawn helper: run to first-seconds boot then kill, returning
/// (stdout, stderr) bytes — for launches that would otherwise run forever.
trait OutputWithTimeoutKill {
    fn output_with_timeout_kill(&mut self) -> (Vec<u8>, Vec<u8>);
}
impl OutputWithTimeoutKill for Command {
    fn output_with_timeout_kill(&mut self) -> (Vec<u8>, Vec<u8>) {
        use std::io::Read;
        let mut child = self
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        std::thread::sleep(Duration::from_secs(8));
        let _ = child.kill();
        let mut out = Vec::new();
        let mut err = Vec::new();
        if let Some(mut s) = child.stdout.take() {
            let _ = s.read_to_end(&mut out);
        }
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_end(&mut err);
        }
        let _ = child.wait();
        (out, err)
    }
}

/// Store `code.defaultProvider` through the real CLI in the given scope.
fn set_default_provider(repo: &Repo, scope_global: bool, value: &str) {
    let mut args = vec!["config", "set"];
    if scope_global {
        args.push("--global");
    }
    args.extend(["code.defaultProvider", value]);
    let out = base_command(&repo.home, &repo.global_db)
        .args(&args)
        .current_dir(&repo.root)
        .output()
        .expect("config set code.defaultProvider");
    assert!(
        out.status.success(),
        "config set failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn config_default_key_wins_over_unique_detection_candidate() {
    // PS-03: the persisted default outranks detection even when detection
    // would have auto-selected a different unique candidate — and when the
    // configured provider lacks its key, the run lands on that provider's
    // own missing-key error, naming only it.
    let repo = init_repo();
    set_default_provider(&repo, true, "gemini");
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("MOONSHOT_API_KEY", "probe-kimi")
        .output()
        .expect("run config-over-detection probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(128),
        "the configured provider's own auth error is expected: {stderr}"
    );
    assert!(
        stderr.contains("GEMINI_API_KEY is not configured for provider 'gemini'"),
        "the config-selected provider must drive the error: {stderr}"
    );
    assert!(
        !stderr.contains("auto-selected") && !stderr.contains("MOONSHOT_API_KEY"),
        "detection must not run on a config hit: {stderr}"
    );
}

#[test]
fn config_default_key_local_repo_overrides_global() {
    // PS-03: `read_cascaded_config_value` semantics — the repo-local key
    // shadows the global one.
    let repo = init_repo();
    set_default_provider(&repo, true, "gemini");
    set_default_provider(&repo, false, "zhipu");
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .output()
        .expect("run local-over-global probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "{stderr}");
    assert!(
        stderr.contains("ZHIPU_API_KEY is not configured for provider 'zhipu'"),
        "the repo-local value must win: {stderr}"
    );
    assert!(
        !stderr.contains("gemini"),
        "the shadowed global value must not surface: {stderr}"
    );
}

#[test]
fn config_default_key_invalid_value_is_usage_error_without_echo() {
    // PS-03: an unrecognized stored id is LBR-CLI-002 with the full legal
    // domain — and the stored value itself (which could be a mistakenly
    // pasted secret) never appears in the output.
    let repo = init_repo();
    set_default_provider(&repo, true, "sk-SENTINEL-not-a-provider");
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .output()
        .expect("run invalid-config probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(129),
        "invalid value must be a usage error: {stderr}"
    );
    assert!(
        stderr.contains("code.defaultProvider")
            && stderr.contains("unrecognized provider id")
            && stderr.contains(
                "Valid values: anthropic, codex, deepseek, gemini, kimi, ollama, openai, zhipu"
            ),
        "a-mode guidance with the sorted domain expected: {stderr}"
    );
    assert!(
        !stderr.contains("SENTINEL"),
        "the stored value must never be echoed: {stderr}"
    );
}

#[test]
fn config_default_key_is_overridden_by_explicit_provider() {
    // PS-03: `--provider` outranks the persisted default — the run must
    // land on the explicit provider's error surface, not the configured
    // one's.
    let repo = init_repo();
    set_default_provider(&repo, true, "zhipu");
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--provider",
            "gemini",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .output()
        .expect("run explicit-over-config probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(128), "{stderr}");
    assert!(
        stderr.contains("GEMINI_API_KEY is not configured for provider 'gemini'"),
        "explicit --provider must win over the config key: {stderr}"
    );
    assert!(
        !stderr.contains("zhipu") && !stderr.contains("ZHIPU_API_KEY"),
        "the overridden config value must not surface: {stderr}"
    );
}

#[test]
fn config_default_key_pairs_with_model_before_the_guard() {
    // PS-03: the config slot precedes the `--model` pairing guard — a
    // persisted default determines the pairing just as a flag would, so
    // the run proceeds to the provider's own credential check instead of
    // the 129 pairing rejection.
    let repo = init_repo();
    set_default_provider(&repo, true, "gemini");
    let out = base_command(&repo.home, &repo.global_db)
        .args([
            "code",
            "--model",
            "arbitrary-model",
            "--port",
            "0",
            "--mcp-port",
            "0",
        ])
        .current_dir(&repo.root)
        .output()
        .expect("run config-plus-model probe");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(128),
        "the pairing guard must not fire on a config hit: {stderr}"
    );
    assert!(
        !stderr.contains("--model without --provider"),
        "pairing rejection must be bypassed by the config slot: {stderr}"
    );
    assert!(
        stderr.contains("GEMINI_API_KEY is not configured for provider 'gemini'"),
        "the run must reach the configured provider's credential check: {stderr}"
    );
}

#[test]
fn empty_string_keys_are_not_configured() {
    // PS-06 terra R4: an empty value can never authenticate, so it must
    // count as unconfigured at every layer — process env and --env-file
    // both fall through to the zero-candidate auth guidance.
    let repo = init_repo();
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "")
        .output()
        .expect("run empty-process-env probe");
    assert_eq!(
        out.status.code(),
        Some(128),
        "empty process-env key must be zero candidates; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no provider credentials configured"),
        "zero-state guidance expected"
    );

    let env_file = repo.root.join("empty.env");
    std::fs::write(&env_file, "ZHIPU_API_KEY=\n").expect("write env file");
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--env-file"])
        .arg(&env_file)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .output()
        .expect("run empty-env-file probe");
    assert_eq!(
        out.status.code(),
        Some(128),
        "empty env-file key must be zero candidates; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn empty_upper_layer_falls_through_to_lower_layer() {
    // PS-06 terra R5: an empty upper-layer value must not shadow a usable
    // key beneath it — the empty process env falls through to the
    // repo-local vault (and detection and the real boot agree).
    let repo = init_repo();
    let st = base_command(&repo.home, &repo.global_db)
        .args(["config", "set", "vault.env.GEMINI_API_KEY", "vault-probe"])
        .current_dir(&repo.root)
        .status()
        .expect("store repo-local key");
    assert!(st.success());
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stderr.contains("provider 'gemini' auto-selected") && stderr.contains("repo-local vault"),
        "empty process env must fall through to the repo-local vault: {stderr}"
    );
    assert!(
        !stderr.contains("is not configured"),
        "the boot must agree with detection (no missing-key error): {stderr}"
    );

    // And the mirrored case: empty --env-file value, non-empty process env —
    // one coherent launch from the process-environment layer.
    let env_file = repo.root.join("empty2.env");
    std::fs::write(&env_file, "GEMINI_API_KEY=\n").expect("write env file");
    let out = base_command(&repo.home, &repo.global_db)
        .args(["code", "--env-file"])
        .arg(&env_file)
        .args(["--port", "0", "--mcp-port", "0"])
        .current_dir(&repo.root)
        .env("GEMINI_API_KEY", "proc-probe")
        .stdin(Stdio::null())
        .output_with_timeout_kill();
    let stderr = String::from_utf8_lossy(&out.1);
    assert!(
        stderr.contains("provider 'gemini' auto-selected")
            && stderr.contains("process environment"),
        "empty env-file value must fall through to the process env: {stderr}"
    );
    assert!(
        !stderr.contains("is not configured"),
        "boot and detection must use the same source: {stderr}"
    );
    assert!(
        !stderr.contains("vault-probe") && !stderr.contains("proc-probe"),
        "values must never leak: {stderr}"
    );
}
