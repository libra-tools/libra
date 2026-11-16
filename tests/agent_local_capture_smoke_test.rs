//! Task A6.5 — Agent 第一期本地三 Agent 采集 smoke (plan.md §0.3, first-batch
//! hard gate).
//!
//! Drives the **real**, locally installed `codex`, `claude` and `opencode`
//! CLIs (one minimal paid session each) in throwaway Libra repositories
//! and asserts the whole capture chain end to end: `libra agent add` hook
//! install (pinned-binary provenance + user-config preservation) → real
//! non-interactive session → `agent session/checkpoint list`,
//! safe-summary `checkpoint show`, `session show`, `refs/libra/traces`,
//! `agent doctor` → §0.3.5 uninstall smoke (semantic config restore,
//! idempotent remove, captured data retained). Deterministic fixtures
//! cannot stand in for this target (plan.md A6.5 acceptance).
//!
//! CI-safe by construction: every paid test is `#[ignore]` **and** skips unless
//! `LIBRA_RUN_LOCAL_AGENTS=1`, so neither `cargo test --all` nor a stray
//! `--ignored` run can start a paid session. Tests are `#[serial]` and the
//! canonical invocation adds `--test-threads=1` (§0.3.6):
//!
//! ```bash
//! LIBRA_RUN_LOCAL_AGENTS=1 \
//! LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode \
//! cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1
//! ```
//!
//! Knobs (plan.md §0.3.6): `LIBRA_LOCAL_AGENT_SET` (default all three),
//! `LIBRA_LOCAL_AGENT_TIMEOUT_SECS` (default 180; expiry kills the
//! child's whole **process group**), `LIBRA_KEEP_LOCAL_AGENT_SMOKE=1`
//! (keep evidence — sensitive, never commit),
//! `LIBRA_LOCAL_AGENT_EVIDENCE_DIR` (custom 0700 evidence root).
//! macOS OpenCode additionally requires `LIBRA_OPENCODE_NATIVE_BINARY` to
//! select the reviewed separate private build; PATH/global OpenCode is never
//! a fallback. Nonignored regressions below make no model calls.
//!
//! A missing binary or a failed read-only login probe marks that agent's
//! run **BLOCKED** without starting a paid session (§0 blocked rule) and
//! **fails the test with a `BLOCKED` panic**: with the gate explicitly
//! open, blocked stays machine-distinguishable (exit code / pass count)
//! from a real green run and can never be faked green. The shared driver — and
//! the documented per-CLI isolation decisions (isolated `CODEX_HOME` for
//! codex; real `HOME` + project-local capture configs for claude and
//! opencode) — lives in `tests/harness/agent_local_capture.rs`.

#![cfg(unix)]

mod harness;

use harness::agent_local_capture as smoke;
use serial_test::serial;

/// Guard against `LIBRA_LOCAL_AGENT_SET` typos silently skipping an agent
/// (checked in the gated path only — unknown slugs are a hard error).
fn check_agent_set() {
    if std::env::var(smoke::GATE_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        let unknown = smoke::unknown_requested_agents();
        assert!(
            unknown.is_empty(),
            "unknown agent slug(s) in {}: {unknown:?} (known: codex, claude-code, opencode)",
            smoke::SET_ENV
        );
    }
}

#[test]
#[ignore = "drives a real paid codex session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_codex() {
    check_agent_set();
    smoke::run_slug("codex");
}

#[test]
#[ignore = "drives a real paid claude session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_claude_code() {
    check_agent_set();
    smoke::run_slug("claude-code");
}

#[test]
#[ignore = "drives a real paid opencode session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_opencode() {
    check_agent_set();
    smoke::run_slug("opencode");
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "drives a real paid opencode session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan.md §0.3.6)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_opencode_macos() {
    check_agent_set();
    smoke::run_slug("opencode");
}

/// PI-23: one real Pi 1.5.0 session in an isolated home and trusted
/// fixture. The operator authorized Pi 1.5.0 and `astra/gpt-6.1-sol` (the
/// agent-plan key is dead); the extension is installed through the PI-14
/// function, not the closed CLI. The checkpoint must retain the session's
/// thinking and the asset must uninstall idempotently.
#[test]
#[ignore = "drives a real paid Pi session; set LIBRA_RUN_LOCAL_AGENTS=1 and run with \
            --ignored --test-threads=1 (plan-20260911 PI-23)"]
#[serial(cwd, env, hash_kind)]
fn local_capture_smoke_pi() {
    use std::{
        os::unix::{fs::PermissionsExt, process::CommandExt},
        path::Path,
        process::{Command, Output, Stdio},
        time::{Duration, Instant},
    };

    use libra::internal::ai::hooks::providers::pi::settings::{
        PiAssetOwnership, PiAssetRemove, PiDiagnostic, PiProjectTrust, diagnose_pi_project,
        pi_asset_ownership, remove_owned_pi_asset, write_owned_pi_asset,
    };
    use serde_json::{Value, json};

    fn run_with_timeout(mut command: Command, timeout: Duration, label: &str) -> Output {
        let mut child = command.spawn().expect("spawn smoke child");
        let started = Instant::now();
        loop {
            if child.try_wait().expect("poll smoke child").is_some() {
                return child.wait_with_output().expect("collect smoke child");
            }
            if started.elapsed() > timeout {
                // SAFETY: the child was started in its own process group.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.wait();
                panic!("{label} exceeded {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn libra_command(libra: &Path, repo: &Path, home: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(libra);
        command
            .args(args)
            .current_dir(repo)
            .env("HOME", home)
            .env("LIBRA_TEST_HOME", home)
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run_with_timeout(command, Duration::from_secs(120), "libra")
    }

    if std::env::var(smoke::GATE_ENV).as_deref() != Ok("1") {
        eprintln!("skipped (pi): set {} = 1", smoke::GATE_ENV);
        return;
    }
    let requested = std::env::var(smoke::SET_ENV).unwrap_or_default();
    if !requested.split(',').any(|slug| slug.trim() == "pi") {
        eprintln!("skipped (pi): {} does not contain pi", smoke::SET_ENV);
        return;
    }

    let root = tempfile::tempdir().expect("Pi smoke root");
    let home = root.path().join("home");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(home.join(".pi").join("agent")).expect("isolated Pi home");
    std::fs::create_dir_all(&repo).expect("smoke repo");
    let libra = Path::new(env!("CARGO_BIN_EXE_libra")).to_path_buf();
    let repo = repo.canonicalize().expect("canonical repo");
    let home = home.canonicalize().expect("canonical home");
    let agent_dir = home.join(".pi").join("agent");

    let init = libra_command(&libra, &repo, &home, &["init"]);
    assert!(
        init.status.success(),
        "libra init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    write_owned_pi_asset(&repo, &libra).expect("install the Pi extension through PI-14");
    assert_eq!(
        pi_asset_ownership(&repo).expect("asset ownership"),
        PiAssetOwnership::Owned
    );

    // The real models.json currently carries a dead agent-plan key (and an
    // empty key makes Pi reject the whole file); the authorized astra
    // provider is copied alone into the isolated home.
    let real_models = Path::new(&std::env::var("HOME").expect("real HOME"))
        .join(".pi")
        .join("agent")
        .join("models.json");
    let mut models: Value =
        serde_json::from_slice(&std::fs::read(&real_models).expect("read real models.json"))
            .expect("parse models.json");
    models["providers"]
        .as_object_mut()
        .expect("providers")
        .remove("agent-plan");
    let models_path = agent_dir.join("models.json");
    std::fs::write(
        &models_path,
        serde_json::to_vec_pretty(&models).expect("models json"),
    )
    .expect("write isolated models.json");
    std::fs::set_permissions(&models_path, std::fs::Permissions::from_mode(0o600))
        .expect("private models.json");
    std::fs::write(
        agent_dir.join("trust.json"),
        serde_json::to_vec(&json!({ repo.display().to_string(): true })).expect("trust json"),
    )
    .expect("isolated trust");

    let prompt = "Write a playable terminal Snake game to a new file named snake.py in the \
                  current directory. Use only the Python standard library: a grid, snake \
                  movement with WASD or arrow keys, food, score, game over, and no external \
                  files. Do not run the game. After writing the file, reply exactly \
                  PI23_SNAKE_DONE.";
    let mut pi = Command::new("pi");
    pi.args([
        "-p",
        "--approve",
        "--model",
        "astra/gpt-6.1-sol",
        "--thinking",
        "max",
        prompt,
    ])
    .current_dir(&repo)
    .env("HOME", &home)
    .env("LIBRA_TEST_HOME", &home)
    .env("PI_CODING_AGENT_DIR", &agent_dir)
    .env("PATH", std::env::var("PATH").unwrap_or_default())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .process_group(0);
    let session = run_with_timeout(pi, Duration::from_secs(600), "pi smoke");
    assert!(
        session.status.success(),
        "pi exited {:?}: stdout={} stderr={}",
        session.status.code(),
        String::from_utf8_lossy(&session.stdout),
        String::from_utf8_lossy(&session.stderr)
    );
    let snake = repo.join("snake.py");
    let snake_text = std::fs::read_to_string(&snake).expect("snake.py written by the model");
    assert!(
        snake_text.to_ascii_lowercase().contains("snake"),
        "snake.py does not look like the requested game"
    );

    let encoded = repo
        .to_str()
        .expect("utf-8 repo")
        .trim_start_matches('/')
        .replace(['/', '\\', ':'], "-");
    let session_dir = agent_dir.join("sessions").join(format!("--{encoded}--"));
    let session_file = std::fs::read_dir(&session_dir)
        .expect("Pi session directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .expect("Pi session file");
    let session_text = std::fs::read_to_string(&session_file).expect("read Pi session");
    let mut thinking = Vec::new();
    let mut session_id = None;
    for line in session_text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["type"] == json!("session") {
            session_id = entry["id"].as_str().map(str::to_string);
        }
        if entry["type"] != json!("message") || entry["message"]["role"] != json!("assistant") {
            continue;
        }
        if let Some(blocks) = entry["message"]["content"].as_array() {
            for block in blocks {
                if block["type"] == json!("thinking")
                    && let Some(text) = block["thinking"].as_str()
                    && !text.is_empty()
                {
                    thinking.push(text.to_string());
                }
            }
        }
    }
    let session_id = session_id.expect("Pi session header id");
    assert!(
        !thinking.is_empty(),
        "the real Pi session produced no thinking to compare"
    );

    let sessions = libra_command(
        &libra,
        &repo,
        &home,
        &["agent", "session", "list", "--json"],
    );
    assert!(
        sessions.status.success(),
        "session list: {}",
        String::from_utf8_lossy(&sessions.stderr)
    );
    let sessions: Value = serde_json::from_slice(&sessions.stdout).expect("session list json");
    assert!(
        sessions["data"]["sessions"]
            .as_array()
            .is_some_and(|rows| rows
                .iter()
                .any(|row| row["session_id"] == json!(format!("pi__{session_id}")))),
        "Libra did not record the Pi session: {sessions}"
    );
    let checkpoints = libra_command(
        &libra,
        &repo,
        &home,
        &["agent", "checkpoint", "list", "--json"],
    );
    assert!(
        checkpoints.status.success(),
        "checkpoint list: {}",
        String::from_utf8_lossy(&checkpoints.stderr)
    );
    let checkpoints: Value =
        serde_json::from_slice(&checkpoints.stdout).expect("checkpoint list json");
    let row = checkpoints["data"]["checkpoints"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["session_id"] == json!(format!("pi__{session_id}")))
        })
        .unwrap_or_else(|| panic!("no Pi checkpoint: {checkpoints}"));
    let checkpoint_id = row["checkpoint_id"].as_str().expect("checkpoint id");
    let export = libra_command(
        &libra,
        &repo,
        &home,
        &["agent", "checkpoint", "export", checkpoint_id],
    );
    assert!(
        export.status.success(),
        "checkpoint export: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let exported_text = String::from_utf8_lossy(&export.stdout);
    let exported_thinking: Vec<String> = exported_text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["type"] == json!("message"))
        .flat_map(|entry| {
            entry["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|block| block["type"] == json!("thinking"))
        .filter_map(|block| block["thinking"].as_str().map(str::to_string))
        .filter(|text| !text.is_empty())
        .collect();
    assert_eq!(
        exported_thinking, thinking,
        "the checkpoint export thinking differs from the Pi source"
    );

    let diagnosis = diagnose_pi_project(&repo, Some(Path::new("pi")), PiProjectTrust::Granted)
        .expect("PI-17 diagnosis");
    assert_eq!(diagnosis.ownership, PiAssetOwnership::Owned);
    assert!(
        !diagnosis.findings.contains(&PiDiagnostic::AssetMissing)
            && !diagnosis.findings.contains(&PiDiagnostic::AssetDrifted)
            && !diagnosis.findings.contains(&PiDiagnostic::TrustNotGranted),
        "diagnosis: {:?}",
        diagnosis.findings
    );
    assert_eq!(
        remove_owned_pi_asset(&repo).expect("uninstall the Pi extension"),
        PiAssetRemove::Removed
    );
    assert_eq!(
        pi_asset_ownership(&repo).expect("ownership after uninstall"),
        PiAssetOwnership::Absent
    );
    assert_eq!(
        remove_owned_pi_asset(&repo).expect("second uninstall"),
        PiAssetRemove::Absent
    );

    eprintln!(
        "PI-23 GO: pi 1.5.0 + astra/gpt-6.1-sol, session pi__{session_id}, {} thinking block(s) / {} bytes, checkpoint {checkpoint_id}, snake.py {} bytes, asset installed and removed",
        thinking.len(),
        thinking.iter().map(String::len).sum::<usize>(),
        snake_text.len()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn opencode_project_preflight_requires_disposable_directory() {
    use std::{ffi::OsStr, path::Path};

    assert_eq!(
        smoke::OPENCODE_SMOKE_MODEL,
        "volcengine-agent-plan/glm-5.3-flash"
    );
    let args = smoke::opencode_smoke_args();
    assert_eq!(args.first().map(String::as_str), Some("run"));
    assert!(args.iter().any(|arg| arg == "--standalone"));
    assert!(!args.iter().any(|arg| arg == "--dir"));
    assert!(
        args.windows(2)
            .any(|pair| { pair[0] == "--model" && pair[1] == smoke::OPENCODE_SMOKE_MODEL })
    );
    assert_eq!(
        args[9..].join(" "),
        smoke::OPENCODE_SMOKE_PROMPT,
        "the pinned CLI must receive the exact prompt without added quotes"
    );
    assert!(args[9..].iter().all(|part| !part.contains(' ')));
    let root = tempfile::tempdir().expect("create smoke fixture");
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).expect("create disposable project");
    let mut command = std::process::Command::new("opencode");
    let project = smoke::configure_opencode_project(&mut command, &repo);
    assert_eq!(command.get_current_dir(), Some(project.as_path()));
    assert_eq!(
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("PWD"))
            .and_then(|(_, value)| value),
        Some(project.as_os_str()),
        "the OpenCode 2.0.26 run command prefers PWD over cwd"
    );
    let response = serde_json::json!({ "directory": project });
    assert!(smoke::opencode_location_matches_project(
        &serde_json::to_vec(&response).expect("serialize location"),
        &project
    ));
    let foreign = serde_json::json!({ "directory": root.path() });
    assert!(!smoke::opencode_location_matches_project(
        &serde_json::to_vec(&foreign).expect("serialize foreign location"),
        &project
    ));
    assert!(!smoke::opencode_location_matches_project(
        br#"{"project":{"directory":"/"}}"#,
        &project
    ));
    assert!(!smoke::opencode_location_matches_project(
        b"not JSON",
        Path::new(&project)
    ));
}

/// Keep Linux's original A6.5 invocation while the separate macOS gate
/// requires content. This is a source guard, never Linux operator evidence.
#[test]
fn linux_a65_criteria_unchanged() {
    let harness = include_str!("harness/agent_local_capture.rs");
    assert!(harness.contains("Linux A6.5 remains"));
    assert!(
        harness.contains("#[cfg(not(target_os = \"macos\"))]\n            \"opencode\" => vec![")
    );
    let smoke = include_str!("agent_local_capture_smoke_test.rs");
    let original = [
        "fn ",
        &["local_capture", "smoke", "opencode"].join("_"),
        "(",
    ]
    .concat();
    assert!(smoke.contains(&original));
    let call = ["smoke::run_slug(", r#""opencode")"#].concat();
    assert!(smoke.contains(&call));
    assert!(harness.contains("assert_opencode_content_capture("));
}

#[cfg(target_os = "macos")]
#[test]
fn opencode_login_probe_timeout_is_not_success() {
    use std::{os::unix::fs::PermissionsExt, process::Command, time::Duration};

    let root = tempfile::tempdir().expect("create probe fixture");
    let stdout = root.path().join("stdout");
    let stderr = root.path().join("stderr");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 30"]);
    let result = smoke::run_child_with_timeout(
        command,
        &stdout,
        &stderr,
        Duration::from_millis(200),
        "synthetic login probe",
    );
    assert!(result.timed_out);
    assert!(!result.exit_ok);
    assert_eq!(result.exit_code, None);
    for path in [stdout, stderr] {
        assert_eq!(
            std::fs::metadata(path)
                .expect("private probe output")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn opencode_private_path_is_consistent_for_all_children() {
    use std::{ffi::OsStr, path::Path, process::Command};

    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private/bin");
    std::fs::create_dir_all(&private).unwrap();
    let binary = private.join("opencode");
    std::fs::write(&binary, b"not executable; PATH resolution only").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let child_path = smoke::opencode_child_path(&binary, OsStr::new("/usr/bin:/bin")).unwrap();
    let entries: Vec<_> = std::env::split_paths(&child_path).collect();
    assert_eq!(
        entries,
        [
            private.clone(),
            Path::new("/usr/bin").to_owned(),
            Path::new("/bin").to_owned()
        ]
    );
    let selected = Command::new("/usr/bin/which")
        .arg("opencode")
        .env("PATH", &child_path)
        .output()
        .unwrap();
    assert!(selected.status.success());
    assert_eq!(
        String::from_utf8(selected.stdout).unwrap().trim(),
        binary.to_str().unwrap()
    );
    assert!(
        smoke::opencode_child_path(
            Path::new("/private/colon:path/opencode"),
            OsStr::new("/bin")
        )
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn opencode_version_timeout_blocks_before_login_and_model() {
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("probe");
    let marker = root.path().join("late-marker");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nsleep 30\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let reason = smoke::opencode_version_probe(&script, root.path(), Duration::from_millis(200))
        .unwrap_err();
    assert!(reason.contains("no login or model session started"));
    assert!(!marker.exists());
    assert!(!root.path().join("opencode-version.stdout").exists());
    assert!(!root.path().join("opencode-version.stderr").exists());
}
