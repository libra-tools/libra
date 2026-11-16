#![allow(dead_code)]
//! Shared driver for the Task A6.5 **local three-agent capture smoke**
//! (`tests/agent_local_capture_smoke_test.rs`, plan.md §0.3.2–§0.3.6).
//!
//! Drives the *real*, locally installed `codex`, `claude` and `opencode`
//! CLIs against throwaway Libra repositories and asserts the whole
//! first-batch capture chain: hook install → real non-interactive session
//! → `agent session/checkpoint/doctor` JSON surfaces → `refs/libra/traces`
//! → §0.3.5 uninstall smoke.
//!
//! ## Per-CLI isolation decisions (documented per plan.md §0.3.3)
//!
//! Each CLI keeps its **real auth state** (starting a paid session without
//! it is impossible) but every Libra-managed capture config lives in
//! disposable state:
//!
//! - **codex** — hooks and trust are *user-level* (`$CODEX_HOME/hooks.json`
//!   plus `config.toml` `[hooks.state]`), so the harness builds an
//!   **isolated `CODEX_HOME`** under the smoke root: `auth.json` is copied
//!   (0600) from the real `~/.codex` and `config.toml` is copied with every
//!   Libra-managed `[hooks.state.*]` section (and its marker comment)
//!   stripped. Both `libra agent add codex` and `codex exec` run with
//!   `CODEX_HOME` pointing there — the real `~/.codex` is never written.
//! - **claude** — runs with the **real `HOME`** (OAuth state and the
//!   `~/.claude` transcript root the checkpoint writer's provider-root
//!   gate requires cannot be relocated), but the installer target is
//!   *project-local* (`<repo>/.claude/settings.json`, see
//!   `providers/claude/settings.rs::claude_settings_path`), so nothing
//!   user-level is touched.
//! - **opencode** — runs with the **real `HOME`** (provider credentials
//!   under `~/.local/share/opencode` / `~/.config/opencode`); the
//!   Libra-managed plugin is *project-local*
//!   (`<repo>/.opencode/plugin/libra-hooks.js`), so nothing user-level is
//!   touched. Plugin envelopes stay lifecycle-only. Linux A6.5 remains
//!   lifecycle-only and is left for operator validation. macOS OG-15 additionally
//!   requires real content from the Required Seatbelt export/server pair and
//!   a stopped capture session, using the explicit authorized smoke model.
//!
//! ## Evidence discipline (plan.md §0.3.1)
//!
//! Evidence dirs are 0700 and files 0600. Raw child stdout/stderr only
//! ever land under `evidence/raw/`. The redacted `summary.json` holds
//! booleans, exit codes, versions, oids and `~`-normalised paths — never
//! tokens, prompts, transcript bodies or account identifiers (the
//! `claude auth status` output contains email/org ids and is discarded
//! after reading the exit code). Default teardown deletes the whole smoke
//! root; `LIBRA_KEEP_LOCAL_AGENT_SMOKE=1` keeps it (with a sensitivity
//! warning). A failing or blocked run keeps only the redacted
//! `summary.json` (flushed from `Drop` with `completed:false` plus the
//! last `stage` marker or the blocked reason) — `raw/`, the
//! `preinstall/` provider-config snapshots (which can carry copied user
//! config), the copied codex auth home, the repo and the pinned binary
//! copy are always deleted.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Gate: unset ⇒ the `#[ignore]` tests skip even when run with `--ignored`.
pub const GATE_ENV: &str = "LIBRA_RUN_LOCAL_AGENTS";
/// Comma-separated agent slugs to run (default: all three).
pub const SET_ENV: &str = "LIBRA_LOCAL_AGENT_SET";
/// Set to `1` to keep the smoke root (repos + evidence, incl. `raw/`).
pub const KEEP_ENV: &str = "LIBRA_KEEP_LOCAL_AGENT_SMOKE";
/// Per-agent child-process timeout in seconds (default 180).
pub const TIMEOUT_ENV: &str = "LIBRA_LOCAL_AGENT_TIMEOUT_SECS";
/// Custom evidence root; per-agent subdirs are created inside it.
pub const EVIDENCE_DIR_ENV: &str = "LIBRA_LOCAL_AGENT_EVIDENCE_DIR";

const DEFAULT_AGENT_SET: &str = "codex,claude-code,opencode";
const DEFAULT_TIMEOUT_SECS: u64 = 180;

/// §0.3.2 canonical smoke prompt: non-destructive, trivially cheap.
pub const SMOKE_PROMPT: &str = "Libra local agent capture smoke. Do not read secrets, do not \
     edit files, do not run shell commands. Reply exactly: libra-agent-smoke-ok.";

/// macOS content smoke writes one fixed file inside the disposable project.
#[cfg(target_os = "macos")]
pub const OPENCODE_SMOKE_PROMPT: &str = "In the current project, create exactly one file named \
     libra-agent-smoke.txt containing exactly this line: libra-agent-smoke-file. Do not read \
     any other files and do not run shell commands. Reply exactly: libra-agent-smoke-ok.";
/// Explicit user-authorized Volcano Coding Plan model; no default fallback.
#[cfg(target_os = "macos")]
pub const OPENCODE_SMOKE_MODEL: &str = "volcengine-agent-plan/glm-5.3-flash";
#[cfg(target_os = "macos")]
pub const OPENCODE_NATIVE_BINARY_ENV: &str = "LIBRA_OPENCODE_NATIVE_BINARY";

/// Data-only acceptance check, never issuer registration or native archive proof.
#[cfg(target_os = "macos")]
pub fn opencode_acceptance_identity(version: &str, digest: &str, bytes: u64) -> Result<(), String> {
    use libra::internal::ai::{
        hooks::providers::opencode::events::{OPENCODE_BUILD_ID, OPENCODE_PIN},
        observed_agents::native_origin::approved_build,
    };
    if version != format!("opencode v{OPENCODE_PIN}") {
        return Err(format!(
            "OpenCode version differs from the current private acceptance baseline {OPENCODE_PIN}; explicitly select the reviewed private installation"
        ));
    }
    let build = approved_build::lookup(OPENCODE_PIN, OPENCODE_BUILD_ID, digest)
        .map_err(|_| "OpenCode build differs from the reviewed private artifact; use its unchanged separate installation".to_string())?;
    if bytes != build.identity().binary_bytes {
        return Err("OpenCode binary size differs from the reviewed private artifact; use its unchanged separate installation".into());
    }
    Ok(())
}

/// No PATH/global fallback for current macOS acceptance. Reject drift before
/// login or any model command; this does not enroll or activate an issuer.
#[cfg(target_os = "macos")]
pub fn opencode_acceptance_binary(selected: Option<&Path>) -> Result<PathBuf, String> {
    use libra::internal::ai::{
        hooks::providers::opencode::events::OPENCODE_PIN, observed_agents::trust,
    };
    let selected = selected.ok_or_else(|| {
        format!("set {OPENCODE_NATIVE_BINARY_ENV} to the reviewed separate private opencode binary before macOS acceptance")
    })?;
    if !selected.is_absolute() {
        return Err(format!(
            "{OPENCODE_NATIVE_BINARY_ENV} must be an absolute private binary path"
        ));
    }
    let canonical = selected.canonicalize().map_err(|_| {
        "selected private OpenCode binary is missing; check the separate installation".to_string()
    })?;
    let meta = fs::metadata(&canonical)
        .map_err(|_| "cannot inspect the selected private OpenCode binary".to_string())?;
    if !meta.is_file()
        || canonical.file_name() != Some(std::ffi::OsStr::new("opencode"))
        || meta.permissions().mode() & 0o022 != 0
        || meta.permissions().mode() & 0o111 == 0
    {
        return Err(
            "select a protected executable named opencode in the reviewed private installation"
                .into(),
        );
    }
    opencode_child_path(&canonical, std::ffi::OsStr::new(""))?;
    // Refuse unexpected sizes before hashing an unbounded operator-selected file.
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/agent_transcripts/opencode/native-origin-build.json"
    ))
    .map_err(|_| {
        "private acceptance descriptor is invalid; repair the test baseline".to_string()
    })?;
    if expected["binary_bytes"].as_u64() != Some(meta.len()) {
        return Err("OpenCode binary size differs from the reviewed private artifact; use its unchanged separate installation".into());
    }
    trust::ensure_parent_not_world_writable(&canonical).map_err(|_| {
        "selected private OpenCode parent is writable; move it to a protected installation"
            .to_string()
    })?;
    let measured = trust::compute_provenance(&canonical).map_err(|_| {
        "cannot measure the selected private OpenCode binary; keep acceptance blocked".to_string()
    })?;
    opencode_acceptance_identity(
        &format!("opencode v{OPENCODE_PIN}"),
        &measured.sha256,
        meta.len(),
    )?;
    Ok(measured.canonical_path)
}

/// Keep every capture/export child on the same explicit installation.
#[cfg(target_os = "macos")]
pub fn opencode_child_path(
    binary: &Path,
    inherited: &std::ffi::OsStr,
) -> Result<std::ffi::OsString, String> {
    let parent = binary
        .parent()
        .ok_or_else(|| "private OpenCode binary has no installation directory".to_string())?;
    std::env::join_paths(std::iter::once(parent.to_owned()).chain(std::env::split_paths(inherited)))
        .map_err(|_| "private OpenCode directory cannot be represented in PATH; use a directory without a colon".into())
}

/// A read-only version probe has the same process-group timeout as login.
#[cfg(target_os = "macos")]
pub fn opencode_version_probe(
    binary: &Path,
    raw: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let stdout = raw.join("opencode-version.stdout");
    let stderr = raw.join("opencode-version.stderr");
    let mut command = Command::new(binary);
    command.arg("--version");
    let result = run_child_with_timeout(
        command,
        &stdout,
        &stderr,
        timeout,
        "opencode version preflight",
    );
    let version = if result.exit_ok && !result.timed_out {
        fs::read_to_string(&stdout)
            .ok()
            .and_then(|text| text.lines().next().map(|line| line.trim().to_owned()))
    } else {
        None
    };
    let _ = fs::remove_file(stdout);
    let _ = fs::remove_file(stderr);
    version.ok_or_else(|| {
        "private OpenCode --version failed or timed out; no login or model session started".into()
    })
}

/// One first-batch agent's local contract (§0.3.2/§0.3.4).
pub struct AgentSpec {
    /// Libra CLI slug (`libra agent add <slug>`).
    pub slug: &'static str,
    /// External binary name on `$PATH`.
    pub binary: &'static str,
    /// `agent_session.agent_kind` value Libra must record.
    pub agent_kind: &'static str,
    /// Read-only login-status probe (§0.3.2); output is discarded, only
    /// the boolean + exit code are recorded.
    pub login_args: &'static [&'static str],
}

pub const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        slug: "codex",
        binary: "codex",
        agent_kind: "codex",
        login_args: &["login", "status"],
    },
    AgentSpec {
        slug: "claude-code",
        binary: "claude",
        agent_kind: "claude_code",
        // NOTE: `claude auth status` prints email/orgId/orgName — the
        // harness never persists its stdout (§0.3.2 redaction).
        login_args: &["auth", "status"],
    },
    AgentSpec {
        slug: "pi",
        binary: "pi",
        agent_kind: "pi",
        // PI-23 smoke only: the isolated fixture drops the dead agent-plan
        // provider and uses the operator-authorized astra/gpt-6.1-sol model.
        login_args: &["auth", "check", "--model", "astra/gpt-6.1-sol", "--json"],
    },
    AgentSpec {
        slug: "opencode",
        binary: "opencode",
        agent_kind: "opencode",
        #[cfg(target_os = "macos")]
        login_args: &["auth", "list", "--standalone", "--format", "json"],
        #[cfg(not(target_os = "macos"))]
        login_args: &["providers", "list"],
    },
];

/// Claude-installer forward map pin (providers/claude/settings.rs).
const CLAUDE_EVENT_VERBS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "tool-use"),
    ("PostToolUse", "tool-use"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

/// Codex-installer forward map pin (providers/codex/settings.rs
/// `CODEX_HOOK_FORWARD_MAP`, 11 entries).
const CODEX_EVENT_VERBS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "tool-use"),
    ("PostToolUse", "tool-use"),
    ("PermissionRequest", "permission-request"),
    ("PreCompact", "compaction"),
    ("PostCompact", "compaction"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-end"),
];

/// First line of the Libra-managed OpenCode plugin
/// (providers/opencode/settings.rs `LIBRA_MANAGED_MARKER`).
const OPENCODE_MANAGED_MARKER: &str =
    "// libra-managed: do not edit — installed by libra agent enable (AG-19)";

pub fn spec_for(slug: &str) -> &'static AgentSpec {
    AGENTS
        .iter()
        .find(|spec| spec.slug == slug)
        .unwrap_or_else(|| panic!("unknown agent slug '{slug}'"))
}

/// Slugs requested via `LIBRA_LOCAL_AGENT_SET` (default: all three).
pub fn requested_agents() -> Vec<String> {
    std::env::var(SET_ENV)
        .unwrap_or_else(|_| DEFAULT_AGENT_SET.to_string())
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Entry point used by each per-agent `#[test]`: handles the
/// `LIBRA_RUN_LOCAL_AGENTS` gate and the agent-set filter (both skip with
/// an explanatory notice), then runs the full smoke for `slug`, panicking
/// on any genuine capture-chain failure. A missing binary or failed
/// login-state probe is an **environment block** (§0.3.6 failure
/// layering): no paid session is started, the redacted blocked reason is
/// flushed to summary.json, and the test FAILS with a `BLOCKED` panic —
/// once the gate is explicitly opened, blocked must stay machine
/// distinguishable (exit code / pass count) from a real green run.
pub fn run_slug(slug: &str) {
    if !std::env::var(GATE_ENV).map(|v| v == "1").unwrap_or(false) {
        eprintln!(
            "skipped ({slug}): set {GATE_ENV}=1 (with --ignored --test-threads=1) to drive \
             the real local agent capture smoke — plan.md §0.3.6"
        );
        return;
    }
    if !requested_agents().iter().any(|s| s == slug) {
        eprintln!("skipped ({slug}): not in {SET_ENV}");
        return;
    }
    let spec = spec_for(slug);
    let mut smoke = SmokeAgent::new(spec);
    match smoke.preflight() {
        Preflight::Ready => {}
        Preflight::Blocked(reason) => {
            // §0 blocked rule: record, never fake and never pay — but once
            // the gate is explicitly opened, blocked must be MACHINE
            // distinguishable from a real green run: a plain `return` here
            // would let `cargo test` report "3 passed" / exit 0 on a
            // machine with zero installed agents, indistinguishable from
            // real capture evidence. The redacted reason still lands in
            // summary.json via the Drop flush during unwind.
            smoke.mark_blocked(&reason);
            panic!(
                "BLOCKED ({slug}): {reason} — environment block, no paid session was \
                 started; fix the environment (install/login the agent CLI) or drop \
                 {slug} from {SET_ENV} to run a partial set (plan.md §0.3.2/§0.3.6)"
            );
        }
    }
    smoke.run();
    smoke.completed = true;
}

pub enum Preflight {
    Ready,
    Blocked(String),
}

/// State for one agent's smoke run: smoke root layout, pinned binary and
/// accumulated redacted summary.
pub struct SmokeAgent {
    spec: &'static AgentSpec,
    root: PathBuf,
    repo: PathBuf,
    evidence: PathBuf,
    raw: PathBuf,
    /// Pinned, canonicalized copy of `CARGO_BIN_EXE_libra` (§0.3.2 pin
    /// rule: `cargo test` itself rebuilds `target/`, so hooks must point
    /// at an immutable copy).
    libra_bin: PathBuf,
    libra_sha256: String,
    /// Isolated `$CODEX_HOME` (codex only).
    codex_home: Option<PathBuf>,
    agent_binary: Option<PathBuf>,
    #[cfg(target_os = "macos")]
    opencode_path: Option<std::ffi::OsString>,
    timeout: Duration,
    keep: bool,
    summary: serde_json::Map<String, Value>,
    completed: bool,
}

impl SmokeAgent {
    pub fn new(spec: &'static AgentSpec) -> Self {
        let keep = std::env::var(KEEP_ENV).map(|v| v == "1").unwrap_or(false);
        let timeout = Duration::from_secs(
            std::env::var(TIMEOUT_ENV)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
        );
        let root = smoke_root_for(spec.slug);
        let repo = root.join("repo");
        let evidence = root.join("evidence");
        let raw = evidence.join("raw");
        for dir in [&root, &repo, &evidence, &raw] {
            fs::create_dir_all(dir).expect("create smoke dir");
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        }

        // Pin the built binary (§0.3.2): copy, chmod 0755, canonicalize,
        // sha256. Hook installers embed `canonicalize(current_exe)`, so
        // the canonical path of this copy is the exact expected prefix.
        let src = PathBuf::from(env!("CARGO_BIN_EXE_libra"));
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700)).expect("chmod bin");
        let copy = bin_dir.join("libra");
        fs::copy(&src, &copy).expect("pin libra binary copy");
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o755)).expect("chmod libra");
        let libra_bin = fs::canonicalize(&copy).expect("canonicalize pinned libra");
        let libra_sha256 = sha256_file(&libra_bin);

        let mut summary = serde_json::Map::new();
        summary.insert("schema_version".into(), json!(1));
        summary.insert("slug".into(), json!(spec.slug));
        summary.insert("libra_bin_sha256".into(), json!(libra_sha256));

        Self {
            spec,
            root,
            repo,
            evidence,
            raw,
            libra_bin,
            libra_sha256,
            codex_home: None,
            agent_binary: None,
            #[cfg(target_os = "macos")]
            opencode_path: None,
            timeout,
            keep,
            summary,
            completed: false,
        }
    }

    /// §0.3.2 preflight: binary presence + `--version` + read-only login
    /// probe. Evidence keeps only redacted facts (path `~`-normalised,
    /// version string, login boolean + exit code).
    pub fn preflight(&mut self) -> Preflight {
        self.stage("preflight");
        let binary = match self.selected_binary() {
            Ok(binary) => binary,
            Err(reason) => return Preflight::Blocked(reason),
        };
        self.summary.insert(
            "binary_path".into(),
            json!(normalize_home(&binary.display().to_string())),
        );

        #[cfg(target_os = "macos")]
        let native_version = if self.spec.slug == "opencode" {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            match opencode_child_path(&binary, &inherited) {
                Ok(path) => self.opencode_path = Some(path),
                Err(reason) => return Preflight::Blocked(reason),
            }
            match opencode_version_probe(&binary, &self.raw, Duration::from_secs(30)) {
                Ok(version) => Some(version),
                Err(reason) => return Preflight::Blocked(reason),
            }
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let native_version: Option<String> = None;
        let version = native_version.or_else(|| {
            Command::new(&binary)
                .arg("--version")
                .stdin(Stdio::null())
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| {
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string()
                })
        });
        let Some(version) = version else {
            return Preflight::Blocked(format!("'{} --version' failed", self.spec.binary));
        };
        #[cfg(target_os = "macos")]
        if self.spec.slug == "opencode" {
            let meta =
                match fs::metadata(&binary) {
                    Ok(meta) => meta,
                    Err(_) => return Preflight::Blocked(
                        "selected private OpenCode binary disappeared before version validation"
                            .into(),
                    ),
                };
            let measured = match libra::internal::ai::observed_agents::trust::compute_provenance(&binary) {
                Ok(measured) => measured,
                Err(_) => return Preflight::Blocked("cannot remeasure private OpenCode after its version probe; no login or model session started".into()),
            };
            let digest = measured.sha256;
            if let Err(reason) = opencode_acceptance_identity(&version, &digest, meta.len()) {
                return Preflight::Blocked(reason);
            }
            self.summary
                .insert("native_acceptance_binary_sha256".into(), json!(digest));
            self.summary.insert(
                "native_acceptance_scope".into(),
                json!("reviewed private binary; source mechanism only, not native archive GO"),
            );
        }
        self.summary.insert("version".into(), json!(version));

        // codex: build the isolated CODEX_HOME *before* the login probe so
        // the probe validates the exact auth state the paid session uses.
        if self.spec.slug == "codex" {
            match self.build_isolated_codex_home() {
                Ok(home) => self.codex_home = Some(home),
                Err(reason) => return Preflight::Blocked(reason),
            }
        }

        // Read-only login probe. stdout/stderr are read and DISCARDED
        // (claude's output carries email/org ids); only the boolean and
        // exit code enter the evidence.
        let mut login = Command::new(&binary);
        login.args(self.spec.login_args).stdin(Stdio::null());
        for (key, value) in self.extra_env() {
            login.env(key, value);
        }
        #[cfg(target_os = "macos")]
        if self.spec.slug == "opencode" {
            configure_opencode_project(&mut login, &self.repo);
            let stdout = self.raw.join("opencode-login.stdout.json");
            let stderr = self.raw.join("opencode-login.stderr.log");
            let result = run_child_with_timeout(
                login,
                &stdout,
                &stderr,
                Duration::from_secs(30),
                "opencode login preflight",
            );
            // Auth output may identify accounts: retain only exit facts,
            // including on a timeout, just like the other login probes.
            let _ = fs::remove_file(stdout);
            let _ = fs::remove_file(stderr);
            self.summary.insert(
                "login".into(),
                json!({
                    "probe": self.spec.login_args.join(" "),
                    "logged_in": result.exit_ok && !result.timed_out,
                    "exit_code": result.exit_code,
                    "timed_out": result.timed_out,
                }),
            );
            if result.timed_out || !result.exit_ok {
                return Preflight::Blocked(
                    "OpenCode private login preflight failed or exceeded 30 seconds; no model session started"
                        .into(),
                );
            }
            self.agent_binary = Some(binary);
            return Preflight::Ready;
        }
        if let Some(home) = &self.codex_home {
            login.env("CODEX_HOME", home);
        }
        let login_out = match login.output() {
            Ok(out) => out,
            Err(err) => return Preflight::Blocked(format!("login probe failed to spawn: {err}")),
        };
        let logged_in = login_out.status.success();
        self.summary.insert(
            "login".into(),
            json!({
                "probe": self.spec.login_args.join(" "),
                "logged_in": logged_in,
                "exit_code": login_out.status.code(),
            }),
        );
        if !logged_in {
            return Preflight::Blocked(format!(
                "'{} {}' exited {:?} — not logged in; recover per plan.md §0.3.6 \
                 (codex login / claude login flow / opencode provider credentials)",
                self.spec.binary,
                self.spec.login_args.join(" "),
                login_out.status.code(),
            ));
        }
        self.agent_binary = Some(binary);
        Preflight::Ready
    }

    fn selected_binary(&self) -> Result<PathBuf, String> {
        #[cfg(target_os = "macos")]
        if self.spec.slug == "opencode" {
            let selected = std::env::var_os(OPENCODE_NATIVE_BINARY_ENV).map(PathBuf::from);
            return opencode_acceptance_binary(selected.as_deref());
        }
        which(self.spec.binary).ok_or_else(|| {
            format!(
                "'{}' not found on PATH (install it or drop it from {SET_ENV})",
                self.spec.binary
            )
        })
    }

    /// Isolated `$CODEX_HOME`: `auth.json` copy (0600) + `config.toml`
    /// with Libra-managed `[hooks.state.*]` sections stripped. Never
    /// writes to the real `~/.codex`.
    fn build_isolated_codex_home(&self) -> Result<PathBuf, String> {
        let real_home = std::env::var("HOME").map_err(|_| "HOME is unset".to_string())?;
        let real_codex = Path::new(&real_home).join(".codex");
        let auth = real_codex.join("auth.json");
        if !auth.is_file() {
            return Err(format!(
                "no {} — codex is not logged in on this machine",
                normalize_home(&auth.display().to_string())
            ));
        }
        let isolated = self.root.join("codex-home");
        fs::create_dir_all(&isolated).map_err(|err| format!("create codex home: {err}"))?;
        fs::set_permissions(&isolated, fs::Permissions::from_mode(0o700))
            .map_err(|err| format!("chmod codex home: {err}"))?;

        let auth_bytes = fs::read(&auth).map_err(|err| format!("read auth.json: {err}"))?;
        write_0600(&isolated.join("auth.json"), &auth_bytes);

        let real_config = real_codex.join("config.toml");
        if real_config.is_file() {
            let raw =
                fs::read_to_string(&real_config).map_err(|err| format!("read config: {err}"))?;
            write_0600(
                &isolated.join("config.toml"),
                strip_libra_hook_state(&raw).as_bytes(),
            );
        }
        Ok(isolated)
    }

    /// Extra env for every `libra` / agent child in this smoke (codex gets
    /// the isolated `CODEX_HOME` on all of them, so install, doctor and
    /// the hook processes spawned by `codex exec` agree on one home).
    fn extra_env(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let mut values = Vec::new();
        if let Some(home) = &self.codex_home {
            values.push(("CODEX_HOME".into(), home.as_os_str().to_owned()));
        }
        #[cfg(target_os = "macos")]
        if self.spec.slug == "opencode"
            && let Some(path) = &self.opencode_path
        {
            values.push(("PATH".into(), path.clone()));
        }
        values
    }

    /// Run the pinned libra binary inside the smoke repo.
    fn libra(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(&self.libra_bin);
        cmd.args(args)
            .current_dir(&self.repo)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in self.extra_env() {
            cmd.env(key, value);
        }
        cmd.output()
            .unwrap_or_else(|err| panic!("{}: spawn libra {args:?}: {err}", self.spec.slug))
    }

    fn libra_ok(&self, args: &[&str]) -> String {
        let out = self.libra(args);
        assert!(
            out.status.success(),
            "{}: libra {args:?} failed: {}\n{}",
            self.spec.slug,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `libra <args> --json` parsed with the `{"ok":true}` envelope check.
    fn libra_json(&self, args: &[&str]) -> Value {
        let stdout = self.libra_ok(args);
        let parsed: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|err| panic!("{}: libra {args:?} non-JSON ({err})", self.spec.slug));
        assert_eq!(
            parsed["ok"],
            json!(true),
            "{}: libra {args:?} envelope not ok",
            self.spec.slug
        );
        parsed
    }

    /// The full smoke: install → real session → capture assertions →
    /// uninstall smoke → summary. Panics on genuine failure.
    pub fn run(&mut self) {
        let slug = self.spec.slug;
        eprintln!("[{slug}] smoke root: {}", self.root.display());

        // Temp Libra repo (§0.3.3). `-q` keeps banners out of evidence.
        self.stage("install");
        self.libra_ok(&["init", "-q", "."]);

        self.seed_user_config();
        self.snapshot_preinstall();

        // Install (AG-17 alias surface) + §0.3.3 capability assertions.
        self.libra_ok(&["agent", "add", slug]);
        let row = self.agent_row();
        for field in [
            "supported",
            "registered",
            "transcript_readable",
            "hook_installable",
            "installed",
        ] {
            assert_eq!(
                row[field],
                json!(true),
                "{slug}: agent list --json row must have {field}=true after add: {row}"
            );
        }
        assert_eq!(
            row["support_wave"],
            json!("first_batch"),
            "{slug}: support_wave"
        );
        // Non-target first-batch agents must not become installed (§0.3.3).
        for other in AGENTS.iter().filter(|spec| spec.slug != slug) {
            let other_row = self.slug_row(other.slug);
            assert_ne!(
                other_row["installed"],
                json!(true),
                "{slug}: installing {slug} must not mark {} installed",
                other.slug
            );
        }
        self.assert_hooks_pinned();
        #[cfg(target_os = "macos")]
        if slug == "opencode" {
            let binary = self.agent_binary.as_ref().expect("OpenCode preflight ran");
            let directory = binary.parent().expect("OpenCode binary has a parent");
            self.libra_ok(&[
                "agent",
                "rpc",
                "trust",
                "--dir",
                &directory.to_string_lossy(),
            ]);
            let trusted = self.libra_json(&["--json", "agent", "rpc", "trust", "opencode"]);
            let canonical = fs::canonicalize(binary).expect("canonicalize preflight OpenCode CLI");
            assert_eq!(
                trusted["data"]["path"],
                json!(canonical.to_string_lossy().as_ref()),
                "OpenCode exporter trust did not pin the preflight CLI"
            );
            self.assert_opencode_project_root();
        }
        self.summary.insert(
            "install".into(),
            json!({
                "installed": true,
                "hook_commands_pinned": true,
                "opencode_exporter_trusted": cfg!(target_os = "macos") && slug == "opencode",
            }),
        );

        // §0.3.4: one minimal real non-interactive session.
        self.stage("real-session");
        let run = self.drive_real_session();
        assert!(
            !run.timed_out,
            "{slug}: real agent session timed out after {:?} (raw evidence under {})",
            self.timeout,
            self.raw.display()
        );
        // A non-zero agent exit is advisory only (e.g. claude's post-turn
        // `--max-budget-usd` cap exits non-zero after completing the
        // turn): §0.3.4's gate is Libra-side capture, asserted below. A
        // run that captured nothing still fails there.
        if !run.exit_ok {
            eprintln!(
                "[{slug}] agent exited non-zero ({:?}) — treating as advisory; the smoke \
                 gate is the Libra-side capture assertions (raw evidence under {})",
                run.exit_code,
                self.raw.display()
            );
        }
        self.summary.insert(
            "session_run".into(),
            json!({
                "exit_ok": run.exit_ok,
                "exit_code": run.exit_code,
                "timed_out": run.timed_out,
                "duration_secs": run.duration_secs,
            }),
        );

        // §0.3.5 Libra-side capture assertions.
        self.stage("capture-assertions");
        let session_id = self.assert_capture();

        // §0.3.5 uninstall smoke.
        self.stage("uninstall");
        self.assert_uninstall(&session_id);

        // The summary itself is flushed by Drop (single writer for the
        // success-with-keep, failure and blocked paths alike).
        self.stage("done");
        eprintln!(
            "[{slug}] local capture smoke ok (session {session_id}, libra sha256 {})",
            &self.libra_sha256[..12]
        );
    }

    /// Seed pre-existing user provider config so install/uninstall can
    /// prove they preserve it (§0.3.3/§0.3.5).
    fn seed_user_config(&self) {
        match self.spec.slug {
            "claude-code" => {
                let dir = self.repo.join(".claude");
                fs::create_dir_all(&dir).expect("create .claude");
                fs::write(
                    dir.join("settings.json"),
                    "{\n  \"_libra_smoke_user_marker\": \"preserve-me\"\n}\n",
                )
                .expect("seed user claude settings");
            }
            "opencode" => {
                let dir = self.repo.join(".opencode").join("plugin");
                fs::create_dir_all(&dir).expect("create .opencode/plugin");
                fs::write(
                    dir.join("user-noop.js"),
                    "export const UserNoop = async () => ({});\n",
                )
                .expect("seed user opencode plugin");
            }
            // codex: the copied real config.toml *is* the pre-existing
            // user config inside the isolated CODEX_HOME.
            _ => {}
        }
    }

    fn provider_config_paths(&self) -> Vec<PathBuf> {
        match self.spec.slug {
            "claude-code" => vec![self.repo.join(".claude").join("settings.json")],
            "opencode" => vec![
                self.repo
                    .join(".opencode")
                    .join("plugin")
                    .join("user-noop.js"),
            ],
            "codex" => {
                let home = self.codex_home.as_ref().expect("codex home built");
                vec![home.join("config.toml"), home.join("hooks.json")]
            }
            other => panic!("unknown slug {other}"),
        }
    }

    /// `$evidence/preinstall/` copies of the provider config state before
    /// `agent add` (§0.3.3), used by the §0.3.5 semantic-restore check.
    fn snapshot_preinstall(&self) {
        let dir = self.evidence.join("preinstall");
        fs::create_dir_all(&dir).expect("create preinstall snapshot dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod snapshot");
        for path in self.provider_config_paths() {
            let name = path.file_name().and_then(|n| n.to_str()).expect("name");
            match fs::read(&path) {
                Ok(bytes) => write_0600(&dir.join(name), &bytes),
                // Absent file: record absence so restore can assert on it.
                Err(_) => write_0600(&dir.join(format!("{name}.absent")), b""),
            }
        }
    }

    fn preinstall_bytes(&self, name: &str) -> Option<Vec<u8>> {
        let dir = self.evidence.join("preinstall");
        if dir.join(format!("{name}.absent")).exists() {
            return None;
        }
        Some(fs::read(dir.join(name)).expect("read preinstall snapshot"))
    }

    fn agent_row(&self) -> Value {
        self.slug_row(self.spec.slug)
    }

    fn slug_row(&self, slug: &str) -> Value {
        let list = self.libra_json(&["agent", "list", "--json"]);
        list["data"]["agents"]
            .as_array()
            .unwrap_or_else(|| panic!("agent list --json has no agents array: {list}"))
            .iter()
            .find(|row| row["slug"] == json!(slug))
            .unwrap_or_else(|| panic!("no capability row for '{slug}': {list}"))
            .clone()
    }

    /// §0.3.3/A3-provenance: every Libra-managed hook command must start
    /// with the pinned canonical `$LIBRA_BIN` absolute path (never a bare
    /// `libra`), and pre-existing user config must survive the install.
    fn assert_hooks_pinned(&self) {
        let pinned = self.libra_bin.display().to_string();
        match self.spec.slug {
            "claude-code" => {
                let path = self.repo.join(".claude").join("settings.json");
                let settings: Value =
                    serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
                        .expect("settings.json parses");
                assert_eq!(
                    settings["_libra_smoke_user_marker"],
                    json!("preserve-me"),
                    "claude-code: user settings key must survive install"
                );
                for (event, verb) in CLAUDE_EVENT_VERBS {
                    // Default 30s handlers carry 29000ms; the timeout-less
                    // SessionEnd keeps 9000ms.
                    let budget = if *event == "SessionEnd" {
                        9_000
                    } else {
                        29_000
                    };
                    let expected =
                        format!("{pinned} hooks claude {verb} --capture-budget-ms {budget}");
                    let found = settings["hooks"][event].as_array().is_some_and(|matchers| {
                        matchers.iter().any(|matcher| {
                            matcher["hooks"].as_array().is_some_and(|hooks| {
                                hooks.iter().any(|hook| hook["command"] == json!(expected))
                            })
                        })
                    });
                    assert!(
                        found,
                        "claude-code: '{event}' must carry pinned command '{expected}': {settings}"
                    );
                }
            }
            "codex" => {
                let home = self.codex_home.as_ref().expect("codex home");
                let hooks: Value = serde_json::from_str(
                    &fs::read_to_string(home.join("hooks.json")).expect("read hooks.json"),
                )
                .expect("hooks.json parses");
                for (event, verb) in CODEX_EVENT_VERBS {
                    let (expected_timeout, expected_budget) = if *event == "SessionEnd" {
                        (3, 2_000)
                    } else {
                        (30, 29_000)
                    };
                    let expected = format!(
                        "{pinned} hooks codex {verb} --capture-budget-ms {expected_budget}"
                    );
                    let found = hooks["hooks"][event].as_array().is_some_and(|groups| {
                        groups.iter().any(|group| {
                            group["hooks"].as_array().is_some_and(|handlers| {
                                handlers.iter().any(|handler| {
                                    handler["command"] == json!(expected)
                                        && handler["timeout"] == json!(expected_timeout)
                                })
                            })
                        })
                    });
                    assert!(
                        found,
                        "codex: '{event}' must carry pinned timeout/budget command '{expected}': {hooks}"
                    );
                }
                let config =
                    fs::read_to_string(home.join("config.toml")).expect("read config.toml");
                assert_eq!(
                    config.matches("[hooks.state.\"").count(),
                    CODEX_EVENT_VERBS.len(),
                    "codex: one [hooks.state] trust section per forwarded event"
                );
            }
            "opencode" => {
                let plugin = self
                    .repo
                    .join(".opencode")
                    .join("plugin")
                    .join("libra-hooks.js");
                let content = fs::read_to_string(&plugin).expect("read libra-hooks.js");
                assert!(
                    content.starts_with(OPENCODE_MANAGED_MARKER),
                    "opencode: plugin must start with the Libra-managed marker"
                );
                let command_line = content
                    .lines()
                    .find(|line| line.starts_with("const LIBRA_COMMAND = "))
                    .expect("plugin LIBRA_COMMAND line");
                assert!(
                    command_line.contains(&pinned),
                    "opencode: LIBRA_COMMAND must embed pinned path '{pinned}': {command_line}"
                );
                let user = self
                    .repo
                    .join(".opencode")
                    .join("plugin")
                    .join("user-noop.js");
                assert!(user.is_file(), "opencode: user plugin must survive install");
            }
            other => panic!("unknown slug {other}"),
        }
    }

    /// §0.3.4 non-interactive invocation matrix. Raw stdout/stderr land
    /// under `evidence/raw/` (0600) only.
    #[cfg(target_os = "macos")]
    fn assert_opencode_project_root(&mut self) {
        self.stage("project-preflight");
        let binary = self.agent_binary.as_ref().expect("OpenCode preflight ran");
        let mut command = Command::new(binary);
        let project = configure_opencode_project(&mut command, &self.repo);
        command.args(["api", "--standalone", "get", "/api/location"]);
        let output = self.raw.join("opencode-project-preflight.stdout.json");
        let result = run_child_with_timeout(
            command,
            &output,
            &self.raw.join("opencode-project-preflight.stderr.log"),
            Duration::from_secs(30),
            "opencode project preflight",
        );
        assert!(
            !result.timed_out && result.exit_ok,
            "BLOCKED (opencode): private server project preflight failed before the paid session"
        );
        let response = fs::read(&output).expect("read private server location response");
        assert!(
            opencode_location_matches_project(&response, &project),
            "BLOCKED (opencode): private server did not resolve the disposable smoke project"
        );
        self.summary
            .insert("project_preflight".into(), json!({ "matched": true }));
    }

    /// §0.3.4 non-interactive invocation matrix. Raw stdout/stderr land
    /// under `evidence/raw/` (0600) only.
    fn drive_real_session(&self) -> ChildRun {
        let binary = self.agent_binary.clone().expect("preflight ran");
        let repo = self.repo.display().to_string();
        let last_message = self.raw.join("codex.last-message.txt");
        let last_message_str = last_message.display().to_string();
        let args: Vec<String> = match self.spec.slug {
            "codex" => vec![
                "exec".into(),
                "-C".into(),
                repo,
                "--skip-git-repo-check".into(),
                "--sandbox".into(),
                "read-only".into(),
                "--json".into(),
                "-o".into(),
                last_message_str,
                SMOKE_PROMPT.into(),
            ],
            "claude-code" => vec![
                "-p".into(),
                "--permission-mode".into(),
                "plan".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                "--include-hook-events".into(),
                // Cost bound only — one turn on the current default model
                // runs ≈ $0.13, and hitting the cap exits non-zero *after*
                // the turn completes (tolerated above).
                "--max-budget-usd".into(),
                "0.50".into(),
                SMOKE_PROMPT.into(),
            ],
            #[cfg(target_os = "macos")]
            "opencode" => opencode_smoke_args(),
            #[cfg(not(target_os = "macos"))]
            "opencode" => vec![
                "run".into(),
                "--dir".into(),
                repo,
                "--print-logs".into(),
                "--format".into(),
                "json".into(),
                "--title".into(),
                "libra-agent-smoke-opencode".into(),
                SMOKE_PROMPT.into(),
            ],
            other => panic!("unknown slug {other}"),
        };
        let mut cmd = Command::new(binary);
        cmd.args(&args).current_dir(&self.repo);
        #[cfg(target_os = "macos")]
        if self.spec.slug == "opencode" {
            configure_opencode_project(&mut cmd, &self.repo);
        }
        for (key, value) in self.extra_env() {
            cmd.env(key, value);
        }
        run_child_with_timeout(
            cmd,
            &self.raw.join(format!("{}.stdout.jsonl", self.spec.slug)),
            &self.raw.join(format!("{}.stderr.log", self.spec.slug)),
            self.timeout,
            self.spec.slug,
        )
    }

    /// §0.3.5 capture assertions: session row → checkpoint rows →
    /// safe-summary `checkpoint show` / `session show` → traces ref →
    /// doctor. Returns the captured Libra session id.
    fn assert_capture(&mut self) -> String {
        let slug = self.spec.slug;
        let kind = self.spec.agent_kind;

        // Hook ingestion is synchronous inside the agent process, but be
        // tolerant of trailing hook children: poll briefly.
        let sessions = self.poll_json(&["agent", "session", "list", "--json"], |value| {
            find_session(value, kind).is_some()
        });
        let session = find_session(&sessions, kind).unwrap_or_else(|| {
            panic!(
                "{slug}: no captured session with agent_kind={kind} — the real CLI ran but \
                 Libra saw no hook events (hook config not read, lifecycle ingest, or owner \
                 filtering; §0.3.6 failure layering). session list: {sessions}"
            )
        });
        let session_id = session["session_id"]
            .as_str()
            .expect("session_id string")
            .to_string();
        let session_state = session["state"].as_str().unwrap_or_default().to_string();

        let checkpoints = self.poll_json(&["agent", "checkpoint", "list", "--json"], |value| {
            !checkpoints_for(value, &session_id).is_empty()
        });
        let rows = checkpoints_for(&checkpoints, &session_id);
        assert!(
            !rows.is_empty(),
            "{slug}: session {session_id} captured but no checkpoint (A5 writer / \
             refs/libra/traces / object_index; §0.3.6 failure layering): {checkpoints}"
        );
        let checkpoint_id = rows[0]["checkpoint_id"]
            .as_str()
            .expect("checkpoint_id")
            .to_string();
        let traces_commit = rows[0]["traces_commit"]
            .as_str()
            .expect("traces_commit")
            .to_string();

        // Default `checkpoint show` is a safe fixed summary: no metadata
        // document, object identifiers, source details, or transcript body.
        let show = self.libra_json(&["agent", "checkpoint", "show", &checkpoint_id, "--json"]);
        let summary = &show["data"]["checkpoint"];
        assert!(
            summary.is_object(),
            "{slug}: checkpoint show must carry its structural summary: {show}"
        );
        assert_eq!(summary["checkpoint_id"], json!(checkpoint_id));
        assert_eq!(summary["scope"], rows[0]["scope"]);
        assert!(summary["created_at"].is_i64(), "{slug}: created_at: {show}");
        assert!(
            summary["parent_snapshot_recorded"].is_boolean(),
            "{slug}: parent snapshot flag: {show}"
        );
        assert!(
            show["data"].get("metadata").is_none() && show["data"].get("layout").is_none(),
            "{slug}: checkpoint show must expose no metadata or layout document: {show}"
        );
        let show_text = show.to_string();
        assert!(
            !show_text.contains("libra-agent-smoke-ok")
                && !show_text.contains("Do not read secrets"),
            "{slug}: default checkpoint show leaked transcript/prompt content"
        );

        // `session show` reads the same catalog metadata by OID pointers.
        let session_show = self.libra_json(&["agent", "session", "show", &session_id, "--json"]);
        assert_eq!(
            session_show["data"]["agent_kind"],
            json!(kind),
            "{slug}: session show kind"
        );

        // The checkpoint commit must be the reachable traces-ref tip (or
        // an ancestor once more checkpoints exist): assert the traces ref
        // resolves and mentions this commit chain.
        let refs = self.libra_ok(&["show-ref"]);
        let traces_line = refs
            .lines()
            .find(|line| line.contains("traces"))
            .unwrap_or_else(|| panic!("{slug}: no traces ref in show-ref output: {refs}"));
        assert!(
            self.traces_chain_contains(traces_line, &traces_commit),
            "{slug}: refs/libra/traces does not reach checkpoint commit {traces_commit}"
        );

        // Doctor: no missing hook/object/catalog/redaction findings.
        let doctor = self.libra_json(&["agent", "doctor", "--json"]);
        let store = &doctor["data"]["checkpoint_store"];
        assert_eq!(
            store["findings"],
            json!([]),
            "{slug}: doctor reported checkpoint-store findings: {doctor}"
        );
        assert_eq!(
            doctor["data"]["orphan_checkpoints"],
            json!(0),
            "{slug}: doctor reported orphan checkpoints: {doctor}"
        );
        let hooks_row = doctor["data"]["provider_hooks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["name"] == json!(slug))
            .cloned()
            .unwrap_or_else(|| panic!("{slug}: doctor has no provider row: {doctor}"));
        assert_eq!(
            hooks_row["error"],
            Value::Null,
            "{slug}: doctor provider row reports an error: {doctor}"
        );
        assert_eq!(
            hooks_row["installed"],
            json!(true),
            "{slug}: doctor must see the installed hooks: {doctor}"
        );

        #[cfg(target_os = "macos")]
        if slug == "opencode" {
            let content = self.assert_opencode_content_capture(
                &checkpoint_id,
                &traces_commit,
                &session_state,
            );
            self.summary
                .insert("opencode_content_capture".into(), content);
        }
        self.summary.insert(
            "capture".into(),
            json!({
                "session_id": session_id,
                "session_state": session_state,
                "checkpoint_count": rows.len(),
                "first_checkpoint_id": checkpoint_id,
                "traces_commit": traces_commit,
                "doctor_findings": 0,
            }),
        );
        session_id
    }

    #[cfg(target_os = "macos")]
    fn checkpoint_metadata(&self, traces_commit: &str, checkpoint_id: &str) -> Value {
        let Some(prefix) = checkpoint_id.get(..2) else {
            panic!("{}: invalid checkpoint id shape", self.spec.slug);
        };
        let Some(rest) = checkpoint_id.get(2..) else {
            panic!("{}: invalid checkpoint id shape", self.spec.slug);
        };
        let expected_path = format!("checkpoint/{prefix}/{rest}/metadata.json");
        let tree = self.libra(&[
            "ls-tree",
            "-r",
            "-z",
            "--format",
            "%(objectname)%x09%(path)",
            traces_commit,
        ]);
        assert!(
            tree.status.success(),
            "{}: traces tree lookup failed",
            self.spec.slug
        );
        let metadata_oid = String::from_utf8_lossy(&tree.stdout)
            .split('\0')
            .filter_map(|record| record.split_once('\t'))
            .find_map(|(oid, path)| (path == expected_path).then_some(oid.to_string()))
            .unwrap_or_else(|| panic!("{}: checkpoint metadata blob missing", self.spec.slug));
        let blob = self.libra(&["cat-file", "-p", &metadata_oid]);
        assert!(
            blob.status.success(),
            "{}: checkpoint metadata read failed",
            self.spec.slug
        );
        serde_json::from_slice(&blob.stdout)
            .unwrap_or_else(|_| panic!("{}: checkpoint metadata is invalid JSON", self.spec.slug))
    }

    #[cfg(target_os = "macos")]
    fn opencode_checkpoint_transcript(&self, checkpoint_id: &str) -> Value {
        let output = self.libra(&["agent", "checkpoint", "export", checkpoint_id]);
        assert!(
            output.status.success(),
            "opencode: redacted checkpoint transcript could not be read"
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("opencode: captured native transcript is invalid JSON"))
    }

    #[cfg(target_os = "macos")]
    fn assert_opencode_content_capture(
        &self,
        checkpoint_id: &str,
        traces_commit: &str,
        session_state: &str,
    ) -> Value {
        assert_eq!(
            session_state, "stopped",
            "opencode: session did not stop after export"
        );
        let metadata = self.checkpoint_metadata(traces_commit, checkpoint_id);
        let provider_session_id = metadata["provider_session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("opencode: checkpoint has no provider session id"));
        let source = self.opencode_checkpoint_transcript(checkpoint_id);
        assert_eq!(
            source["info"]["id"],
            json!(provider_session_id),
            "opencode: captured transcript identity differs from its checkpoint"
        );
        let messages = source["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("opencode: source export has no message array"));
        let source_prompts = messages
            .iter()
            .filter(|message| message["type"] == "user")
            .filter_map(|message| message["text"].as_str())
            .collect::<Vec<_>>();
        let source_assistant = messages
            .iter()
            .find(|message| {
                message["type"] == "assistant"
                    && message["model"]["id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty())
            })
            .unwrap_or_else(|| panic!("opencode: source export has no assistant model"));
        let source_model = source_assistant["model"]["id"]
            .as_str()
            .expect("filtered assistant model id");
        let source_provider = source_assistant["model"]["providerID"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            format!("{source_provider}/{source_model}"),
            OPENCODE_SMOKE_MODEL,
            "opencode: source assistant did not use the requested authorized model"
        );
        let source_bytes = serde_json::to_vec(&source)
            .expect("serialize OpenCode source for deterministic projection");
        let projection =
            libra::internal::ai::observed_agents::extract::extract_opencode(&source_bytes);
        assert!(
            !source_prompts.is_empty(),
            "opencode: source has no user prompt"
        );
        assert_eq!(
            projection.prompts,
            source_prompts
                .iter()
                .map(|prompt| prompt.to_string())
                .collect::<Vec<_>>(),
            "opencode: prompt projection differs from source messages"
        );
        assert!(
            projection
                .prompts
                .iter()
                .any(|prompt| prompt == OPENCODE_SMOKE_PROMPT),
            "opencode: synthetic smoke prompt is missing from the projection"
        );
        assert_eq!(projection.model.as_deref(), Some(source_model));
        let usage = projection
            .usage
            .as_ref()
            .unwrap_or_else(|| panic!("opencode: source token projection is absent"));
        assert!(
            usage.input_tokens > 0 || usage.output_tokens > 0,
            "opencode: source token projection is zero"
        );
        let smoke_file = "libra-agent-smoke.txt";
        assert!(
            projection
                .modified_files
                .iter()
                .any(|path| Path::new(path).ends_with(smoke_file)),
            "opencode: completed source export does not project the smoke file"
        );
        assert!(
            fs::read_to_string(self.repo.join(smoke_file)).is_ok_and(|contents| {
                matches!(
                    contents.as_str(),
                    "libra-agent-smoke-file" | "libra-agent-smoke-file\n"
                )
            }),
            "opencode: model did not create the exact file in the throwaway repo"
        );

        let extraction = &metadata["extraction"];
        assert_eq!(extraction["present"], json!(true));
        assert_eq!(extraction["partial"], json!(false));
        assert_eq!(extraction["prompt_count"], json!(projection.prompts.len()));
        assert_eq!(extraction["model"], json!(projection.model));
        assert_eq!(
            extraction["token_usage"],
            serde_json::to_value(usage).expect("serialize projected usage")
        );
        assert!(!projection.modified_files.is_empty());
        assert_eq!(
            extraction["modified_files"],
            json!(projection.modified_files),
            "opencode: captured file projection differs from source messages"
        );
        let captured_text =
            serde_json::to_string(&source).expect("serialize redacted captured transcript");
        assert!(
            captured_text.contains("libra-agent-smoke-ok")
                && captured_text.contains("libra-agent-smoke-file"),
            "opencode: captured checkpoint transcript lacks the synthetic content"
        );
        json!({
            "session_stopped": true,
            "source_matches_projection": true,
            "prompt_nonzero": true,
            "model_nonzero": true,
            "tokens_nonzero": true,
            "files_nonzero": true,
        })
    }

    /// Whether `commit` is reachable from the traces tip named on the
    /// given `show-ref` line (tip match or `rev-list` ancestor walk).
    fn traces_chain_contains(&self, traces_line: &str, commit: &str) -> bool {
        let tip = traces_line.split_whitespace().next().unwrap_or_default();
        if tip == commit {
            return true;
        }
        let out = self.libra(&["rev-list", tip]);
        out.status.success()
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == commit)
    }

    /// §0.3.5 uninstall smoke: semantic config restore vs the preinstall
    /// snapshot, `installed=false`, idempotent second remove, captured
    /// data retained.
    fn assert_uninstall(&mut self, session_id: &str) {
        let slug = self.spec.slug;
        self.libra_ok(&["agent", "remove", slug]);
        let row = self.agent_row();
        assert_eq!(
            row["installed"],
            json!(false),
            "{slug}: installed must flip to false after remove"
        );
        assert_eq!(
            row["hook_installable"],
            json!(true),
            "{slug}: hook_installable must survive remove"
        );
        // Idempotent second remove (exit 0, no error stack).
        self.libra_ok(&["agent", "remove", slug]);

        // Semantic restore vs `preinstall/` (§0.3.5).
        match slug {
            "claude-code" => {
                let path = self.repo.join(".claude").join("settings.json");
                let mut restored: Value =
                    serde_json::from_str(&fs::read_to_string(&path).expect("read settings"))
                        .expect("settings parse");
                // The remover leaves an empty `hooks` map behind; that is
                // semantically equal to the pre-install absence.
                if restored["hooks"] == json!({}) {
                    restored.as_object_mut().expect("object").remove("hooks");
                }
                let pre: Value = serde_json::from_slice(
                    &self
                        .preinstall_bytes("settings.json")
                        .expect("claude settings snapshot"),
                )
                .expect("snapshot parse");
                assert_eq!(
                    restored, pre,
                    "{slug}: settings.json must be semantically restored after remove"
                );
            }
            "codex" => {
                let home = self.codex_home.as_ref().expect("codex home");
                let config =
                    fs::read_to_string(home.join("config.toml")).expect("read config.toml");
                assert!(
                    !config.contains("[hooks.state.\"")
                        && !config.contains("libra-managed codex hook trust entry"),
                    "{slug}: remove must strip every Libra trust section"
                );
                if let Some(pre) = self.preinstall_bytes("config.toml") {
                    assert_eq!(
                        config.as_bytes(),
                        &pre[..],
                        "{slug}: user config.toml must be byte-restored after remove"
                    );
                }
                let hooks = fs::read_to_string(home.join("hooks.json")).unwrap_or_default();
                assert!(
                    !hooks.contains(" hooks codex "),
                    "{slug}: remove must strip every Libra hooks.json handler"
                );
            }
            "opencode" => {
                assert!(
                    !self
                        .repo
                        .join(".opencode")
                        .join("plugin")
                        .join("libra-hooks.js")
                        .exists(),
                    "{slug}: remove must delete the Libra-managed plugin"
                );
                let user = fs::read_to_string(
                    self.repo
                        .join(".opencode")
                        .join("plugin")
                        .join("user-noop.js"),
                )
                .expect("user plugin survives");
                let pre = self
                    .preinstall_bytes("user-noop.js")
                    .expect("user plugin snapshot");
                assert_eq!(
                    user.as_bytes(),
                    &pre[..],
                    "{slug}: user plugin must be byte-identical after remove"
                );
            }
            other => panic!("unknown slug {other}"),
        }

        // Captured data is retained (§0.3.5).
        let sessions = self.libra_json(&["agent", "session", "list", "--json"]);
        assert!(
            find_session(&sessions, self.spec.agent_kind).is_some(),
            "{slug}: captured session must survive uninstall"
        );
        let checkpoints = self.libra_json(&["agent", "checkpoint", "list", "--json"]);
        assert!(
            !checkpoints_for(&checkpoints, session_id).is_empty(),
            "{slug}: captured checkpoints must survive uninstall"
        );
        self.summary.insert(
            "uninstall".into(),
            json!({
                "installed_false": true,
                "second_remove_idempotent": true,
                "config_semantically_restored": true,
                "captured_data_retained": true,
            }),
        );
    }

    /// Poll a libra `--json` query for up to ~20 s.
    fn poll_json(&self, args: &[&str], predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let value = self.libra_json(args);
            if predicate(&value) || Instant::now() >= deadline {
                return value;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// Record the last-reached run stage in the summary — on a failed or
    /// blocked run this marker (plus `completed:false`) is the redacted
    /// diagnostic that survives teardown.
    fn stage(&mut self, name: &str) {
        self.summary.insert("stage".into(), json!(name));
    }

    /// Record a §0 environment block (redacted reason) so the flushed
    /// summary explains the skip.
    pub fn mark_blocked(&mut self, reason: &str) {
        self.stage("blocked");
        self.summary.insert("blocked_reason".into(), json!(reason));
    }

    /// Redacted per-agent summary (0600) with the `completed` flag and
    /// last `stage` marker. Only booleans, exit codes, versions, oids,
    /// uuids and `~`-normalised paths. Best-effort and never panics — it
    /// also runs inside `Drop` while unwinding from a failed assertion,
    /// where a panic would abort the whole test process.
    fn write_summary(&self) {
        let mut map = self.summary.clone();
        map.insert("completed".into(), json!(self.completed));
        let Ok(rendered) = serde_json::to_string_pretty(&Value::Object(map)) else {
            return;
        };
        let path = self.evidence.join("summary.json");
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            let _ = file.write_all(rendered.as_bytes());
        }
    }
}

impl Drop for SmokeAgent {
    fn drop(&mut self) {
        if self.keep {
            // The summary is flushed here (single writer) so keep-mode
            // captures it for completed AND failed/blocked runs.
            self.write_summary();
            eprintln!(
                "[{}] {KEEP_ENV}=1: keeping smoke root {} — it may contain sensitive raw agent \
                 output and copied codex auth state; treat as sensitive, never commit it.",
                self.spec.slug,
                self.root.display()
            );
            return;
        }
        if !self.completed {
            // Failure/blocked path (§0.3.4): keep ONLY the redacted
            // summary zone. Raw child output, the `preinstall/` provider
            // config snapshots (they can carry copied user config, e.g.
            // the codex config.toml from the real auth home), the copied
            // codex auth home, the repo and the pinned binary copy never
            // outlive the run — then the accumulated redacted summary
            // (with `completed:false` + the last `stage` marker, or the
            // blocked reason) is flushed as the surviving diagnostic.
            let _ = fs::remove_dir_all(&self.raw);
            let _ = fs::remove_dir_all(self.evidence.join("preinstall"));
            if let Some(home) = &self.codex_home {
                let _ = fs::remove_dir_all(home);
            }
            let _ = fs::remove_dir_all(self.root.join("repo"));
            let _ = fs::remove_dir_all(self.root.join("bin"));
            self.write_summary();
            eprintln!(
                "[{}] smoke did not complete: kept redacted summary at {} (raw/, preinstall/, \
                 repo and the codex auth copy already deleted); re-run with {KEEP_ENV}=1 to \
                 keep raw output.",
                self.spec.slug,
                self.evidence.join("summary.json").display()
            );
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Smoke root: per-agent subdir under `LIBRA_LOCAL_AGENT_EVIDENCE_DIR`
/// when set (a root we create fresh is tightened to 0700; a pre-existing
/// root is validated 0700 and the per-agent subdir must not pre-exist
/// non-empty — §0.3.6 refuses wide-open or dirty dirs), else a fresh 0700
/// dir under the system tempdir.
fn smoke_root_for(slug: &str) -> PathBuf {
    match std::env::var_os(EVIDENCE_DIR_ENV) {
        Some(custom) => {
            let base = PathBuf::from(custom);
            // A directory we create ourselves is safe to tighten to 0700
            // (create_dir_all inherits the umask, typically 755); only a
            // PRE-EXISTING wide-open directory is refused — its mode is a
            // user decision we must not silently override.
            let preexisting = base.exists();
            fs::create_dir_all(&base).expect("create custom evidence dir");
            if !preexisting {
                fs::set_permissions(&base, fs::Permissions::from_mode(0o700))
                    .expect("chmod fresh custom evidence dir 0700");
            }
            let mode = fs::metadata(&base)
                .expect("stat custom evidence dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{EVIDENCE_DIR_ENV} {} must be 0700 (found {mode:o}) — refusing to write \
                 sensitive evidence into a wide-open directory (§0.3.6)",
                base.display()
            );
            let root = base.join(slug);
            if root.exists() {
                let non_empty = fs::read_dir(&root)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(true);
                assert!(
                    !non_empty,
                    "{EVIDENCE_DIR_ENV}/{slug} is non-empty — refusing to mix evidence runs \
                     (§0.3.6); clean it first"
                );
            }
            root
        }
        None => {
            std::env::temp_dir().join(format!("libra-agent-smoke-{slug}-{}", std::process::id()))
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn opencode_smoke_args() -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "--standalone".into(),
        "--print-logs".into(),
        "--format".into(),
        "json".into(),
        "--model".into(),
        OPENCODE_SMOKE_MODEL.into(),
        "--title".into(),
        "libra-agent-smoke-opencode".into(),
    ];
    // OpenCode 2.0.26 quotes each argv item containing spaces when forming
    // the user message. Separate words preserve the intended prompt bytes.
    args.extend(OPENCODE_SMOKE_PROMPT.split_whitespace().map(str::to_owned));
    args
}

#[cfg(target_os = "macos")]
pub(crate) fn configure_opencode_project(command: &mut Command, repo: &Path) -> PathBuf {
    let project = fs::canonicalize(repo).expect("canonicalize disposable OpenCode smoke project");
    command
        .current_dir(&project)
        .env("PWD", &project)
        .env("OPENCODE_DISABLE_MODELS_FETCH", "1");
    project
}

#[cfg(target_os = "macos")]
pub(crate) fn opencode_location_matches_project(response: &[u8], project: &Path) -> bool {
    serde_json::from_slice::<Value>(response)
        .ok()
        .and_then(|value| value.get("directory")?.as_str().map(str::to_owned))
        .is_some_and(|directory| Path::new(&directory) == project)
}

pub struct ChildRun {
    pub exit_ok: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_secs: u64,
}

/// Spawn `cmd` in its own process group with raw stdout/stderr routed to
/// 0600 files, enforcing a wall-clock timeout with a **process-group**
/// SIGKILL (agents spawn their own children — hook handlers, node
/// workers — that a plain child kill would orphan).
pub fn run_child_with_timeout(
    mut cmd: Command,
    raw_stdout: &Path,
    raw_stderr: &Path,
    timeout: Duration,
    tag: &str,
) -> ChildRun {
    let stdout = open_0600(raw_stdout);
    let stderr = open_0600(raw_stderr);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .process_group(0);
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|err| panic!("{tag}: spawn: {err}"));
    let pid = child.id() as i32;
    let deadline = started + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return ChildRun {
                    exit_ok: status.success(),
                    exit_code: status.code(),
                    timed_out: false,
                    duration_secs: started.elapsed().as_secs(),
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Kill the whole process group (negative pid).
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    return ChildRun {
                        exit_ok: false,
                        exit_code: None,
                        timed_out: true,
                        duration_secs: started.elapsed().as_secs(),
                    };
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(err) => panic!("{tag}: wait: {err}"),
        }
    }
}

/// Strip Libra-managed `[hooks.state.*]` sections (and their marker
/// comments) from a codex `config.toml`, leaving all user content intact.
fn strip_libra_hook_state(config: &str) -> String {
    let mut out = String::with_capacity(config.len());
    let mut in_libra_section = false;
    for line in config.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("# libra-managed codex hook trust entry") {
            continue;
        }
        if trimmed.starts_with("[hooks.state.") {
            in_libra_section = true;
            continue;
        }
        if in_libra_section {
            if trimmed.starts_with('[') {
                in_libra_section = false;
            } else {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn find_session(value: &Value, kind: &str) -> Option<Value> {
    value["data"]["sessions"]
        .as_array()?
        .iter()
        .find(|row| row["agent_kind"] == json!(kind))
        .cloned()
}

fn checkpoints_for(value: &Value, session_id: &str) -> Vec<Value> {
    value["data"]["checkpoints"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["session_id"] == json!(session_id))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn which(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path).expect("read pinned binary for sha256");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

fn open_0600(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap_or_else(|err| panic!("open 0600 {}: {err}", path.display()))
}

fn write_0600(path: &Path, bytes: &[u8]) {
    let mut file = open_0600(path);
    file.write_all(bytes)
        .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

/// `$HOME` → `~` path redaction for evidence (§0.3.5 whitelist rule).
fn normalize_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => path.replacen(&home, "~", 1),
        _ => path.to_string(),
    }
}

/// Names of all agents whose slugs were requested but are unknown —
/// guards against typos in `LIBRA_LOCAL_AGENT_SET` silently skipping an
/// agent.
pub fn unknown_requested_agents() -> Vec<String> {
    let known: BTreeSet<&str> = AGENTS.iter().map(|spec| spec.slug).collect();
    requested_agents()
        .into_iter()
        .filter(|slug| !known.contains(slug.as_str()))
        .collect()
}
