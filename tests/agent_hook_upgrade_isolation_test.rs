//! Issue #502 regression: `libra hooks codex <event>` must not run the
//! auto-upgrade startup recovery gate or the `upgrade.mode=auto` check.
//!
//! A Codex hook can fire with the Libra installation on a read-only
//! filesystem (immutable container, CI sandbox, read-only mount). Before the
//! fix, every hook invocation entered the global recovery gate, which takes
//! `<install-dir>/.libra-upgrade.lock`; on a read-only install that write
//! fails and the hook emits an `auto-upgrade recovery could not complete`
//! warning the host can surface as a generic hook failure.
//!
//! These tests stage the real built binary as an official-style install
//! (a file named `libra` alone in a directory the upgrade orchestrator
//! accepts) and assert:
//!
//! - every installed Codex hook event completes with exit 0 and no
//!   auto-upgrade warning from a read-only install, while a normal command
//!   from the SAME install still exercises the gate and warns (the fixture
//!   is sensitive — the skip is hook-specific, not global);
//! - a hook from a WRITABLE install creates no `.libra-upgrade.lock` (it
//!   never attempts the gate), while a normal command does;
//! - a synthetic SessionStart payload still reaches the capture dispatcher:
//!   the session surfaces in `libra agent session list --json`.

#![cfg(unix)]

use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::Instant,
};

use serde_json::{Value, json};

const LOCK_FILE_NAME: &str = ".libra-upgrade.lock";
/// The issue's hook config carries `timeout: 30` (Codex host budget); a
/// hook that detours into upgrade recovery/check work cannot reliably stay
/// inside it. The assertion is deliberately generous for slow CI.
const HOOK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The nine installed Codex hook events (issue #502 acceptance criteria).
const CODEX_EVENTS: &[(&str, &str)] = &[
    ("session-start", "SessionStart"),
    ("prompt", "UserPromptSubmit"),
    ("tool-use", "PreToolUse"),
    ("permission-request", "PermissionRequest"),
    ("compaction", "Compaction"),
    ("stop", "Stop"),
    ("session-end", "SessionEnd"),
    ("subagent-start", "SubagentStart"),
    ("subagent-end", "SubagentStop"),
];

/// One isolated libra repository, a fake `$HOME`, and the built binary
/// staged as an install the upgrade orchestrator manages.
struct InstallFixture {
    _tempdir: tempfile::TempDir,
    install_dir: PathBuf,
    installed_bin: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl InstallFixture {
    /// Stage the built binary as `<tempdir>/install/libra` and init a repo
    /// with the PLAIN binary (its own install context is `target/`, which
    /// is writable, so init stays quiet).
    fn stage() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let install_dir = tempdir.path().join("install");
        let home = tempdir.path().join("home");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&install_dir).expect("create install dir");
        std::fs::create_dir_all(&home).expect("create fake home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        let installed_bin = install_dir.join("libra");
        std::fs::copy(env!("CARGO_BIN_EXE_libra"), &installed_bin)
            .expect("stage built binary as an install");
        std::fs::set_permissions(&installed_bin, std::fs::Permissions::from_mode(0o755))
            .expect("mark staged binary executable");
        let this = Self {
            _tempdir: tempdir,
            install_dir,
            installed_bin,
            repo,
            home,
        };
        let out = this.run(env!("CARGO_BIN_EXE_libra"), &["init"], None);
        assert!(
            out.status.success(),
            "libra init failed: {}",
            describe(&out)
        );
        this
    }

    /// Make the staged install directory read-only, mimicking the
    /// read-only mount of the issue's Codex sandbox (the directory stays
    /// owned by the invoking user, like the reported environment).
    fn make_install_readonly(&self) {
        std::fs::set_permissions(&self.install_dir, std::fs::Permissions::from_mode(0o555))
            .expect("chmod install dir read-only");
    }

    fn run(&self, binary: &str, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(binary);
        cmd.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn libra binary");
        if let Some(payload) = stdin {
            child
                .stdin
                .take()
                .expect("stdin piped")
                .write_all(payload.as_bytes())
                .expect("write hook envelope to stdin");
        }
        child.wait_with_output().expect("wait for libra binary")
    }

    /// `libra hooks codex <verb>` from the STAGED install with a synthetic
    /// envelope piped via stdin.
    fn hook(
        &self,
        verb: &str,
        hook_event_name: &str,
        session: &str,
    ) -> (Output, std::time::Duration) {
        let envelope = json!({
            "hook_event_name": hook_event_name,
            "session_id": session,
            "cwd": self.repo.to_string_lossy(),
        })
        .to_string();
        let started = Instant::now();
        let out = self.run(
            self.installed_bin.to_str().expect("utf-8 install path"),
            &["hooks", "codex", verb],
            Some(&envelope),
        );
        (out, started.elapsed())
    }

    fn sessions(&self) -> Vec<Value> {
        let out = self.run(
            self.installed_bin.to_str().expect("utf-8 install path"),
            &["agent", "session", "list", "--json"],
            None,
        );
        assert!(out.status.success(), "session list: {}", describe(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|err| panic!("session list stdout is not JSON ({err}): {stdout}"));
        parsed["data"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("session list has no data.sessions rows: {parsed}"))
            .clone()
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn codex_hooks_skip_upgrade_recovery_on_readonly_install() {
    let fixture = InstallFixture::stage();
    fixture.make_install_readonly();

    // Fixture sensitivity: a NORMAL command from the same read-only install
    // still enters the recovery gate, fails to take the install-dir lock,
    // and warns — the hook skip below is specific, not a globally inert
    // upgrade subsystem.
    let status = fixture.run(
        fixture.installed_bin.to_str().expect("utf-8 install path"),
        &["status"],
        None,
    );
    assert!(
        status.status.success(),
        "read-only install must not break normal commands: {}",
        describe(&status)
    );
    assert!(
        stderr_of(&status).contains("auto-upgrade recovery could not complete"),
        "fixture is insensitive: the normal command should still hit the \
         recovery gate on the read-only install: {}",
        describe(&status)
    );

    // Every installed Codex hook event: acknowledged, inside the budget,
    // and free of any auto-upgrade recovery output.
    for (verb, hook_event_name) in CODEX_EVENTS {
        let (out, elapsed) = fixture.hook(verb, hook_event_name, "sess-readonly-502");
        assert!(
            out.status.success(),
            "codex hook {verb} must acknowledge the callback: {}",
            describe(&out)
        );
        let stderr = stderr_of(&out);
        assert!(
            !stderr.contains("auto-upgrade") && !stderr.contains(LOCK_FILE_NAME),
            "codex hook {verb} must not run auto-upgrade recovery: {}",
            describe(&out)
        );
        assert!(
            elapsed < HOOK_BUDGET,
            "codex hook {verb} took {elapsed:?}, outside the {HOOK_BUDGET:?} budget"
        );
    }

    // The SessionStart above reached the capture dispatcher: the codex
    // session is visible through the public inspection surface.
    let sessions = fixture.sessions();
    assert!(
        sessions
            .iter()
            .any(|row| row["session_id"] == json!("codex__sess-readonly-502")),
        "session-start capture must surface in `agent session list`: {sessions:?}"
    );
}

#[test]
fn codex_hooks_create_no_upgrade_lock_in_writable_install() {
    let fixture = InstallFixture::stage();

    // From a WRITABLE install the gate would succeed — and would leave its
    // lock file behind. A hook must not even attempt it (issue #502 AC-1:
    // no `.libra-upgrade.lock` attempt per event).
    for (verb, hook_event_name) in CODEX_EVENTS {
        let (out, _) = fixture.hook(verb, hook_event_name, "sess-writable-502");
        assert!(
            out.status.success(),
            "codex hook {verb} must acknowledge the callback: {}",
            describe(&out)
        );
    }
    assert!(
        !fixture.install_dir.join(LOCK_FILE_NAME).exists(),
        "codex hooks must not create {LOCK_FILE_NAME} in the install directory"
    );

    // Fixture sensitivity: the next NORMAL command from the same install
    // still runs the gate and takes the lock (issue #502 AC-5).
    let status = fixture.run(
        fixture.installed_bin.to_str().expect("utf-8 install path"),
        &["status"],
        None,
    );
    assert!(status.status.success(), "status: {}", describe(&status));
    assert!(
        fixture.install_dir.join(LOCK_FILE_NAME).exists(),
        "the normal command should exercise the recovery gate and create {LOCK_FILE_NAME}"
    );
}
