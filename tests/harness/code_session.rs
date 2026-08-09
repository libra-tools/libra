#![allow(dead_code)]

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Debug, Clone)]
pub struct CodeSessionOptions {
    pub fixture: PathBuf,
    pub name: String,
    pub use_default_control_paths: bool,
    /// Pass `--browser-control loopback` to the spawned `libra code` so the
    /// browser controller surface is available in tests that exercise the
    /// browser write path. Defaults to `false`.
    pub browser_control_loopback: bool,
    /// When `true`, spawn `libra code --control write` so the harness can
    /// drive automation writes; when `false`, omit the flag so the runtime
    /// keeps the default `observe` posture and automation attach is rejected
    /// with `CONTROL_DISABLED`. Defaults to `true` for back-compat with
    /// existing scenario tests.
    pub control_write: bool,
    /// Test-only override for the controller-lease TTL. When `Some(n)`, the
    /// harness exports `LIBRA_CODE_LEASE_DURATION_MS=n` so the spawned
    /// runtime issues short-TTL leases for expiry tests. Production
    /// builds ignore this env var.
    pub lease_duration_ms: Option<u64>,
    /// Wave 5 / PR 5 — operating context (`dev` / `review` /
    /// `research`). When `Some`, spawn `libra code --context <value>`
    /// so the intent classifier doesn't filter `apply_patch` /
    /// `shell` out of the allowed-tool set. Generation cases need
    /// `dev` for apply_patch to actually fire; the lease / SSE
    /// matrix leaves it `None` to preserve the auto-classify path
    /// the runtime ships by default.
    pub context: Option<String>,
    /// Wave 5 / PR 5 — `--approval-policy` override. Generation
    /// cases set `never` so workspace-bounded `apply_patch` calls
    /// don't queue an approval interaction the harness can't
    /// answer. Other matrices leave it `None` and inherit the
    /// runtime's `on-request` default.
    pub approval_policy: Option<String>,
    /// Wave 7 / PR 7 — extra env vars to export to the spawned
    /// process. Applied AFTER the harness's own defaults so a case
    /// can override `LIBRA_LOG_FILE` (security redaction case
    /// drives this) or `LIBRA_LOG`. Empty by default.
    pub extra_env: Vec<(String, String)>,
    /// Wave 9 / PR 9 — pass `--resume <thread_id>` to the spawned
    /// `libra code` so the runtime loads an existing
    /// `SessionState` from `.libra/storage/code-sessions/`. The
    /// resume test pairs this with `with_existing_repo_dir(...)`
    /// so the second spawn shares the first spawn's storage.
    pub resume_thread_id: Option<String>,
    /// Wave 9 / PR 9 — when `Some`, spawn against the supplied
    /// repo directory instead of building a fresh tempdir + running
    /// `libra init`. The resume test uses this to persist state
    /// between two CodeSession lifecycles.
    pub existing_repo_dir: Option<PathBuf>,
    /// Wave 11 / PR 11 — when `Some`, replaces `--provider fake`
    /// (and removes the paired `--fake-fixture` arg) so the
    /// spawned process can drive a real provider through the
    /// model_generation matrix. Pair with `model_override` and
    /// `env_file` for a complete live-provider invocation.
    pub provider_override: Option<String>,
    /// Wave 11 / PR 11 — when `Some`, replaces `--model fake-local`.
    pub model_override: Option<String>,
    /// Wave 11 / PR 11 — when `Some`, passes
    /// `--env-file <path>` so the runtime loads provider
    /// credentials (e.g. `DEEPSEEK_API_KEY`) from a `.env.test`-
    /// style file alongside the spawn.
    pub env_file: Option<PathBuf>,
    /// Wave 11 / PR 11 — extra arbitrary CLI flags passed to
    /// `libra code` after the harness's standard args. The model
    /// matrix uses this for `--deepseek-thinking enabled
    /// --deepseek-reasoning-effort high`.
    pub extra_cli_args: Vec<String>,
}

impl CodeSessionOptions {
    pub fn new(name: impl Into<String>, fixture: impl Into<PathBuf>) -> Self {
        Self {
            fixture: fixture.into(),
            name: name.into(),
            use_default_control_paths: false,
            browser_control_loopback: false,
            control_write: true,
            lease_duration_ms: None,
            context: None,
            approval_policy: None,
            extra_env: Vec::new(),
            resume_thread_id: None,
            existing_repo_dir: None,
            provider_override: None,
            model_override: None,
            env_file: None,
            extra_cli_args: Vec::new(),
        }
    }

    /// Wave 11 / PR 11 — point the spawn at a live provider
    /// (`--provider <name> --model <model>`) instead of the
    /// fake fixture.
    pub fn with_live_provider(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        self.provider_override = Some(provider.into());
        self.model_override = Some(model.into());
        self
    }

    /// Wave 11 / PR 11 — pass `--env-file <path>` so the
    /// spawned process loads provider credentials from disk.
    pub fn with_env_file(mut self, path: PathBuf) -> Self {
        self.env_file = Some(path);
        self
    }

    /// Wave 11 / PR 11 — append an extra CLI flag (e.g.
    /// `--deepseek-thinking enabled`).
    pub fn push_extra_cli_arg(mut self, arg: impl Into<String>) -> Self {
        self.extra_cli_args.push(arg.into());
        self
    }

    /// Wave 9 / PR 9 — pass `--resume <thread_id>` to the spawned
    /// process. Pair with `with_existing_repo_dir` so the storage
    /// the first spawn wrote is visible to the resuming spawn.
    pub fn with_resume_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.resume_thread_id = Some(thread_id.into());
        self
    }

    /// Wave 9 / PR 9 — spawn against an existing repo dir instead
    /// of a fresh tempdir + `libra init`. Caller owns the dir's
    /// lifetime (typically a `tempfile::TempDir` held alive across
    /// both spawns).
    pub fn with_existing_repo_dir(mut self, dir: PathBuf) -> Self {
        self.existing_repo_dir = Some(dir);
        self
    }

    /// Wave 7 / PR 7 — append `(key, value)` to the env vars
    /// exported to the spawned `libra code`. Caller-supplied keys
    /// override the harness defaults applied earlier (e.g. a case
    /// can redirect `LIBRA_LOG_FILE` to a path containing a
    /// secret-like substring to test redaction).
    pub fn with_extra_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// Force the spawned `libra code` into a specific context mode
    /// (`dev` / `review` / `research`). Wave 5 generation matrix
    /// uses `"dev"` so `apply_patch` is in the agent's allowed
    /// tools without needing the auto-classifier to hit the model.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Override `--approval-policy`. Wave 5 generation matrix uses
    /// `"never"` so workspace-bounded apply_patch calls don't queue
    /// an interaction; other matrices leave the default in place.
    pub fn with_approval_policy(mut self, policy: impl Into<String>) -> Self {
        self.approval_policy = Some(policy.into());
        self
    }

    pub fn with_default_control_paths(mut self) -> Self {
        self.use_default_control_paths = true;
        self
    }

    pub fn with_browser_control_loopback(mut self) -> Self {
        self.browser_control_loopback = true;
        self
    }

    /// Spawn the session in `--control observe` mode; suppresses the
    /// process-level control token. Tests that need to exercise the
    /// `CONTROL_DISABLED` rejection path call this.
    pub fn with_control_observe(mut self) -> Self {
        self.control_write = false;
        self
    }

    /// Override the controller-lease TTL via `LIBRA_CODE_LEASE_DURATION_MS`.
    /// Used by the lease-expiry matrix case so the test does not have to
    /// sleep for the production 120 s.
    pub fn with_lease_duration_ms(mut self, ms: u64) -> Self {
        self.lease_duration_ms = Some(ms);
        self
    }
}

pub struct CodeSession {
    _temp: TempDir,
    repo_dir: PathBuf,
    fixture: PathBuf,
    logs_dir: PathBuf,
    token_path: PathBuf,
    info_path: PathBuf,
    base_url: String,
    /// MCP server URL captured from `control.json`. Populated only when the
    /// runtime starts an MCP server (not in `--web-only`-without-MCP modes).
    mcp_url: Option<String>,
    control_token: String,
    controller_token: Option<String>,
    /// Whether the session was spawned with `--control write`. Observe-mode
    /// sessions never get a control token file, so the harness should not
    /// look for one and authorized POSTs are limited to non-write routes.
    control_write: bool,
    child: Option<Box<dyn Child + Send + Sync>>,
    writer: Option<Box<dyn Write + Send>>,
    reader_thread: Option<thread::JoinHandle<()>>,
    client: Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlInfo {
    base_url: String,
    #[serde(default)]
    mcp_url: Option<String>,
}

impl CodeSession {
    pub fn spawn(options: CodeSessionOptions) -> Result<Self> {
        let bin = libra_bin();
        let temp = tempfile::Builder::new()
            .prefix(&format!("libra-code-ui-{}-", options.name))
            .tempdir()
            .context("failed to create code session tempdir")?;
        // Wave 9 / PR 9 — the resume case spawns twice against the
        // same repo so the second `libra code` can `--resume` the
        // session the first one persisted. When `existing_repo_dir`
        // is `Some`, skip the tempdir-managed `repo` subdir AND the
        // `libra init` step — the caller-supplied dir is already
        // initialised. Side effect: token / info files now live
        // beside the repo's `.libra/`, NOT under the harness
        // tempdir, so a follow-up spawn doesn't have to reproduce
        // the original tempdir's path layout.
        let repo_dir = options
            .existing_repo_dir
            .clone()
            .unwrap_or_else(|| temp.path().join("repo"));
        let logs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("code-ui-scenarios")
            .join(&options.name);
        fs::create_dir_all(&repo_dir).context("failed to create repo directory")?;
        if logs_dir.exists() {
            fs::remove_dir_all(&logs_dir).with_context(|| {
                format!("failed to clear previous logs dir '{}'", logs_dir.display())
            })?;
        }
        fs::create_dir_all(&logs_dir).context("failed to create code session logs directory")?;

        if options.existing_repo_dir.is_none() {
            let init_output = Command::new(&bin)
                .args(["init", "--vault=false", "--quiet"])
                .arg(&repo_dir)
                .output()
                .with_context(|| format!("failed to run '{} init'", bin.display()))?;
            if !init_output.status.success() {
                bail!(
                    "failed to initialize temp Libra repo\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&init_output.stdout),
                    String::from_utf8_lossy(&init_output.stderr)
                );
            }
        }

        let token_path = if options.use_default_control_paths {
            repo_dir.join(".libra").join("code").join("control-token")
        } else {
            temp.path().join("control-token")
        };
        let info_path = if options.use_default_control_paths {
            repo_dir.join(".libra").join("code").join("control.json")
        } else {
            temp.path().join("control.json")
        };
        let libra_log = logs_dir.join("libra.log");
        let pty_log = logs_dir.join("pty.log");
        let previous_info_text = fs::read_to_string(&info_path).ok();

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let reader_thread = thread::spawn(move || {
            let Ok(mut file) = File::create(&pty_log) else {
                return;
            };
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if file.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = file.flush();
                    }
                    Err(_) => break,
                }
            }
        });

        let writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;

        let mut cmd = CommandBuilder::new(&bin);
        // Wave 11 / PR 11 — split the provider / model / fixture
        // args so a live-provider spawn (model_generation matrix)
        // can drop `--fake-fixture` entirely and supply real
        // credentials via `--env-file`. Without an override, the
        // default fake-provider invocation matches every prior
        // wave's expected argv exactly.
        let provider_arg = options.provider_override.as_deref().unwrap_or("fake");
        let model_arg = options.model_override.as_deref().unwrap_or("fake-local");
        cmd.args([
            "code",
            "--provider",
            provider_arg,
            "--model",
            model_arg,
            "--port",
            "0",
            "--mcp-port",
            "0",
        ]);
        if options.provider_override.is_none() {
            cmd.args(["--fake-fixture", path_str(&options.fixture)?]);
        }
        if let Some(env_file) = options.env_file.as_ref() {
            cmd.args(["--env-file", path_str(env_file)?]);
        }
        for arg in &options.extra_cli_args {
            cmd.arg(arg);
        }
        if options.control_write {
            cmd.args(["--control", "write"]);
        }
        if !options.use_default_control_paths {
            cmd.args([
                "--control-token-file",
                path_str(&token_path)?,
                "--control-info-file",
                path_str(&info_path)?,
            ]);
        }
        if options.browser_control_loopback {
            cmd.args(["--browser-control", "loopback"]);
        }
        if let Some(context) = options.context.as_deref() {
            cmd.args(["--context", context]);
        }
        if let Some(policy) = options.approval_policy.as_deref() {
            cmd.args(["--approval-policy", policy]);
        }
        if let Some(thread_id) = options.resume_thread_id.as_deref() {
            cmd.args(["--resume", thread_id]);
        }
        cmd.cwd(&repo_dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("LIBRA_ENABLE_TEST_PROVIDER", "1");
        cmd.env("LIBRA_LOG_FILE", path_str(&libra_log)?);
        cmd.env("LIBRA_LOG", "info,libra::internal::ai::web=debug");
        if let Some(ms) = options.lease_duration_ms {
            cmd.env("LIBRA_CODE_LEASE_DURATION_MS", ms.to_string());
        }
        // Wave 7: per-case env overrides apply LAST so they can
        // shadow the harness defaults above (notably LIBRA_LOG_FILE
        // for the security redaction case).
        for (key, value) in &options.extra_env {
            cmd.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn libra code in PTY")?;
        drop(pair.slave);

        let mut session = Self {
            _temp: temp,
            repo_dir,
            fixture: options.fixture,
            logs_dir,
            token_path,
            info_path,
            base_url: String::new(),
            mcp_url: None,
            control_token: String::new(),
            controller_token: None,
            control_write: options.control_write,
            child: Some(child),
            writer: Some(writer),
            reader_thread: Some(reader_thread),
            client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .context("failed to build test HTTP client")?,
        };
        session.wait_for_control_info(Duration::from_secs(30), previous_info_text)?;
        Ok(session)
    }

    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    pub fn info_path(&self) -> &Path {
        &self.info_path
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn run_default_control_conflict(&self) -> Result<Output> {
        let mut child = Command::new(libra_bin())
            .args([
                "code",
                "--provider",
                "fake",
                "--fake-fixture",
                path_str(&self.fixture)?,
                "--model",
                "fake-local",
                "--control",
                "write",
                "--port",
                "0",
                "--mcp-port",
                "0",
            ])
            .current_dir(&self.repo_dir)
            .env("TERM", "xterm-256color")
            .env("LIBRA_ENABLE_TEST_PROVIDER", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn conflicting libra code process")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if child
                .try_wait()
                .context("failed to poll conflicting libra code process")?
                .is_some()
            {
                return child
                    .wait_with_output()
                    .context("failed to collect conflicting libra code output");
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = child.kill();
        let _ = child.wait();
        bail!("conflicting libra code process did not exit within 10s")
    }

    pub fn snapshot(&self) -> Result<Value> {
        self.get_json("/session")
    }

    /// Open a blocking SSE subscription against `/api/code/events`.
    /// The returned [`super::EventStream`] reads events on a worker
    /// thread; per-event timeouts are configured by the caller.
    ///
    /// Wave 1 of `docs/development/commands/_general.md` makes this the central
    /// entry point for the SSE matrix and downstream Waves
    /// (state / generation / approval) that need to observe runtime
    /// notifications without polling `/session`.
    pub fn open_event_stream(&self) -> Result<super::EventStream> {
        // Use a dedicated client with no overall timeout so the SSE
        // long-poll isn't cut off by the harness's default 5 s
        // request budget. Per-event timeouts are enforced by
        // `EventStream::next_event` itself.
        let client = Client::builder()
            .timeout(None)
            .build()
            .context("failed to build SSE HTTP client")?;
        let url = self.url("/events");
        super::EventStream::open(&client, &url, None)
    }

    pub fn diagnostics(&self) -> Result<Value> {
        self.get_json("/diagnostics")
    }

    pub fn artifact_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Wave 7 / PR 7 — accessor for the harness-issued control
    /// token so security cases can assert it does NOT leak into
    /// diagnostics or audit-log output.
    /// MCP server URL parsed from the runtime's `control.json`. `None` if
    /// the runtime did not start an MCP server (e.g. modes where `start_mcp_server`
    /// is intentionally skipped). Wave 9 §5.14 item 1 closure surface.
    pub fn mcp_url(&self) -> Option<&str> {
        self.mcp_url.as_deref()
    }

    pub fn control_token_value(&self) -> &str {
        &self.control_token
    }

    /// Wave 7 / PR 7 — fully-qualified `/api/code/controller/attach`
    /// URL string. The parallel-attach state case spawns its own
    /// per-thread `reqwest::blocking::Client` (because `CodeSession`
    /// is not `Sync`) and needs the URL by value to do so.
    pub fn matrix_attach_url(&self) -> String {
        self.url("/controller/attach")
    }

    /// Wave 7 / PR 7 — accessor for the most recently issued
    /// controller (lease) token, or `None` if no attach has run
    /// yet. Mirrors `control_token_value()` for redaction asserts.
    pub fn current_controller_token(&self) -> Option<&str> {
        self.controller_token.as_deref()
    }

    /// Wave 7 / PR 7 — record a controllerToken issued by a
    /// successful matrix attach so security assertions like
    /// `does_not_contain_controller_token` have a concrete value
    /// to scan for. Codex pass-1 P1 fix: without this hook the
    /// matrix `run_attach` path only updated `CaseRuntime.tokens`
    /// and the assertion would compare against `None`, silently
    /// passing even if the token leaked.
    pub fn set_controller_token(&mut self, token: impl Into<String>) {
        self.controller_token = Some(token.into());
    }

    /// Absolute path to the temporary repo this session was spawned in.
    /// Wave 5 / PR 5 — the generation matrix needs to read files
    /// produced by `apply_patch` and run verification commands inside
    /// that workspace.
    pub fn repo_dir(&self) -> &Path {
        &self.repo_dir
    }

    /// Read a file from the spawned `libra code` working directory.
    /// `relative` is rebased onto `repo_dir`; absolute or
    /// parent-traversing paths are rejected so a misconfigured matrix
    /// case can't read arbitrary host files. Returns `Ok(None)` when
    /// the file is missing — callers like `Step::RepoFileAbsent`
    /// distinguish absence from I/O failure.
    pub fn read_repo_file(&self, relative: &str) -> Result<Option<String>> {
        let resolved = self.resolve_repo_path(relative)?;
        match fs::read_to_string(&resolved) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read repo file '{}'", resolved.display())),
        }
    }

    /// Run `command` inside the spawned `libra code` working
    /// directory with a hard wall-clock timeout. Stdout / stderr are
    /// captured so matrix assertions like
    /// `stdout_or_stderr_contains:<needle>` can inspect them.
    /// Returns the raw `Output` once the child exits or the timeout
    /// kills it.
    pub fn run_repo_command(&self, command: &[String], timeout: Duration) -> Result<Output> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| anyhow!("repo command must have at least one element"))?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(&self.repo_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn repo command '{program}'"))?;
        let deadline = Instant::now() + timeout;
        loop {
            if child
                .try_wait()
                .with_context(|| format!("failed to poll repo command '{program}'"))?
                .is_some()
            {
                return child
                    .wait_with_output()
                    .with_context(|| format!("failed to collect repo command '{program}' output"));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "repo command '{program}' did not exit within {}ms",
                    timeout.as_millis()
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn resolve_repo_path(&self, relative: &str) -> Result<PathBuf> {
        let candidate = Path::new(relative);
        if candidate.is_absolute() {
            bail!("repo path '{relative}' must be relative, not absolute");
        }
        if candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("repo path '{relative}' must not contain '..' components");
        }
        Ok(self.repo_dir.join(candidate))
    }

    pub fn debug_context(&self) -> String {
        let snapshot = self
            .snapshot()
            .and_then(|snapshot| {
                serde_json::to_string_pretty(&snapshot)
                    .context("failed to serialize latest snapshot")
            })
            .unwrap_or_else(|error| format!("<unavailable: {error:#}>"));
        let control_info = fs::read_to_string(&self.info_path)
            .unwrap_or_else(|error| format!("<unavailable: {error}>"));
        let pty_tail = tail_file(&self.logs_dir.join("pty.log"), 20);
        let libra_tail = tail_file(&self.logs_dir.join("libra.log"), 20);
        let context = format!(
            "artifacts: {}\ncontrol.json:\n{}\nlatest snapshot:\n{}\npty.log tail:\n{}\nlibra.log tail:\n{}",
            self.logs_dir.display(),
            control_info,
            snapshot,
            pty_tail,
            libra_tail
        );
        self.redact_known_secrets(context)
    }

    /// Fetch a path from the static Web UI origin rather than the `/api/code`
    /// namespace. Browser smoke tests use this to assert the embedded Next.js
    /// app is what a loopback browser receives before driving write APIs.
    pub fn get_web_path(&self, path: &str) -> Result<(StatusCode, String)> {
        let normalized = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let response = self
            .client
            .get(format!("{}{}", self.base_url, normalized))
            .send()
            .with_context(|| format!("failed to GET web path {normalized}"))?;
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("failed to read web path {normalized} response body"))?;
        Ok((status, body))
    }

    pub fn attach_automation(&mut self, client_id: &str) -> Result<String> {
        let response = self
            .client
            .post(self.url("/controller/attach"))
            .header("X-Libra-Control-Token", &self.control_token)
            .json(&json!({ "clientId": client_id, "kind": "automation" }))
            .send()
            .context("failed to send automation attach request")?;
        let status = response.status();
        let text = response
            .text()
            .context("failed to read automation attach response body")?;
        ensure_success(status, &text)?;
        let value: Value =
            serde_json::from_str(&text).context("failed to parse automation attach response")?;
        let token = value
            .get("controllerToken")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("attach response did not include controllerToken: {value}"))?
            .to_string();
        self.controller_token = Some(token.clone());
        Ok(token)
    }

    /// Attach as a `browser` controller. Unlike automation attach, the
    /// browser path does **not** require `X-Libra-Control-Token` — only the
    /// returned `controllerToken` is needed for follow-up writes. Caller is
    /// responsible for spawning the session with
    /// [`CodeSessionOptions::with_browser_control_loopback`] so the runtime
    /// advertises the browser write surface.
    pub fn attach_browser(&self, client_id: &str) -> Result<String> {
        let response = self
            .client
            .post(self.url("/controller/attach"))
            .json(&json!({ "clientId": client_id, "kind": "browser" }))
            .send()
            .context("failed to send browser attach request")?;
        let status = response.status();
        let text = response
            .text()
            .context("failed to read browser attach response body")?;
        ensure_success(status, &text)?;
        let value: Value =
            serde_json::from_str(&text).context("failed to parse browser attach response")?;
        let token = value
            .get("controllerToken")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!("browser attach response did not include controllerToken: {value}")
            })?
            .to_string();
        Ok(token)
    }

    /// Variant of [`attach_browser`] that returns the raw error envelope when
    /// the server rejects the attach (e.g. `BROWSER_CONTROL_DISABLED` or
    /// `CONTROLLER_CONFLICT`).
    pub fn attach_browser_expect_error(&self, client_id: &str) -> Result<(StatusCode, Value)> {
        let response = self
            .client
            .post(self.url("/controller/attach"))
            .json(&json!({ "clientId": client_id, "kind": "browser" }))
            .send()
            .context("failed to send browser attach request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Submit a `/messages` request as a browser controller — only the lease
    /// token is sent, **not** the automation control token. Mirrors the
    /// frontend `useBrowserController()` write semantics.
    pub fn browser_submit_message(
        &self,
        controller_token: &str,
        text: &str,
    ) -> Result<(StatusCode, Value)> {
        let response = self
            .client
            .post(self.url("/messages"))
            .header("X-Code-Controller-Token", controller_token)
            .json(&json!({ "text": text }))
            .send()
            .context("failed to submit browser message")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Cancel the current turn via the browser write surface (browser leases
    /// reach parity with the TUI Esc cancel and do not require
    /// `X-Libra-Control-Token`).
    pub fn browser_cancel_turn(&self, controller_token: &str) -> Result<(StatusCode, Value)> {
        let response = self
            .client
            .post(self.url("/control/cancel"))
            .header("X-Code-Controller-Token", controller_token)
            .send()
            .context("failed to send browser cancel request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Detach a browser controller. The detach handler reads the lease from
    /// the `X-Code-Controller-Token` header — no automation token required.
    pub fn browser_detach(
        &self,
        controller_token: &str,
        client_id: &str,
    ) -> Result<(StatusCode, Value)> {
        let response = self
            .client
            .post(self.url("/controller/detach"))
            .header("X-Code-Controller-Token", controller_token)
            .json(&json!({ "clientId": client_id }))
            .send()
            .context("failed to send browser detach request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Submit an oversized `/messages` payload as a browser controller — the
    /// 256 KiB body limit middleware (`enforce_code_write_body_limit`) must
    /// reject the request with `PAYLOAD_TOO_LARGE` before the runtime
    /// observes it. Mirrors `submit_large_message` for the automation path.
    pub fn browser_submit_large_message(
        &self,
        controller_token: &str,
        bytes: usize,
    ) -> Result<(StatusCode, Value)> {
        let text = "x".repeat(bytes);
        let response = self
            .client
            .post(self.url("/messages"))
            .header("X-Code-Controller-Token", controller_token)
            .json(&json!({ "text": text }))
            .send()
            .context("failed to submit oversized browser message")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// POST `/interactions/{id}` as a browser controller — caller-supplied
    /// id is intentionally unconstrained so tests can assert behaviour for
    /// missing interactions (`INTERACTION_NOT_ACTIVE`).
    pub fn browser_respond_interaction(
        &self,
        controller_token: &str,
        interaction_id: &str,
    ) -> Result<(StatusCode, Value)> {
        let response = self
            .client
            .post(self.url(&format!("/interactions/{interaction_id}")))
            .header("X-Code-Controller-Token", controller_token)
            .json(&json!({ "approved": true }))
            .send()
            .context("failed to send browser interaction response")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    // -------------------------------------------------------------------
    // Matrix runner primitives
    //
    // These are deliberately lower-level than the `attach_*` / `submit_*`
    // helpers above: each one lets the data-driven runner choose the auth
    // and token state per-step without baking those decisions into the
    // helper. The matrix module owns the `AuthMode` / `TokenSource` enums.
    // -------------------------------------------------------------------

    pub fn matrix_attach(
        &self,
        kind: &str,
        client_id: &str,
        auth: &super::matrix::AuthMode,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .post(self.url("/controller/attach"))
            .json(&json!({ "clientId": client_id, "kind": kind }));
        request = match auth {
            super::matrix::AuthMode::ValidControl => {
                request.header("X-Libra-Control-Token", &self.control_token)
            }
            super::matrix::AuthMode::InvalidControl => {
                request.header("X-Libra-Control-Token", "00000000-deadbeef")
            }
            super::matrix::AuthMode::MissingControl | super::matrix::AuthMode::None => request,
        };
        let response = request
            .send()
            .with_context(|| format!("failed to send matrix attach (kind={kind})"))?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    pub fn matrix_detach(
        &self,
        client_id: &str,
        token: &super::matrix::TokenSource,
        auth: &super::matrix::AuthMode,
        tokens: &std::collections::HashMap<super::matrix::TokenSlot, String>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .post(self.url("/controller/detach"))
            .json(&json!({ "clientId": client_id }));
        request = self.apply_controller_token(request, token, tokens);
        request = self.apply_control_auth(request, auth);
        let response = request
            .send()
            .context("failed to send matrix detach request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    pub fn matrix_submit(
        &self,
        text: &str,
        token: &super::matrix::TokenSource,
        auth: &super::matrix::AuthMode,
        tokens: &std::collections::HashMap<super::matrix::TokenSlot, String>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .post(self.url("/messages"))
            .json(&json!({ "text": text }));
        request = self.apply_controller_token(request, token, tokens);
        request = self.apply_control_auth(request, auth);
        let response = request
            .send()
            .context("failed to send matrix submit request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Wave 7 / PR 7 — POST `/api/code/messages` with a body of
    /// `bytes` repeated `'x'`s, parameterised auth/token like the
    /// rest of the matrix submit family. State cases use this for
    /// the 256 KiB / 257 KiB / 1 MiB boundary asserts.
    pub fn matrix_submit_bytes(
        &self,
        bytes: usize,
        token: &super::matrix::TokenSource,
        auth: &super::matrix::AuthMode,
        tokens: &std::collections::HashMap<super::matrix::TokenSlot, String>,
        timeout: Option<Duration>,
    ) -> Result<(StatusCode, Value)> {
        let mut request_builder = self.client.post(self.url("/messages"));
        // Per-call timeout override so a 1 MiB drain test can give
        // the runtime more time without bumping the harness-wide
        // 5 s ceiling. None inherits the default client timeout.
        if let Some(timeout) = timeout {
            let scoped = Client::builder()
                .timeout(timeout)
                .build()
                .context("failed to build per-call HTTP client")?;
            request_builder = scoped.post(self.url("/messages"));
        }
        let text = "x".repeat(bytes);
        let mut request = request_builder.json(&json!({ "text": text }));
        request = self.apply_controller_token(request, token, tokens);
        request = self.apply_control_auth(request, auth);
        let response = request
            .send()
            .context("failed to send matrix submit-bytes request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Wave 7 / PR 7 — POST `/api/code/control/cancel`. Idle-state
    /// state cases use this to exercise the documented
    /// `SESSION_BUSY` rejection path.
    pub fn matrix_cancel(
        &self,
        token: &super::matrix::TokenSource,
        auth: &super::matrix::AuthMode,
        tokens: &std::collections::HashMap<super::matrix::TokenSlot, String>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self.client.post(self.url("/control/cancel"));
        request = self.apply_controller_token(request, token, tokens);
        request = self.apply_control_auth(request, auth);
        let response = request
            .send()
            .context("failed to send matrix cancel request")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Wave 7 / PR 7 — GET `/api/code/diagnostics` and return the
    /// raw response body string so security cases can pin redaction
    /// behaviour without re-deserialising the JSON.
    pub fn diagnostics_raw_text(&self) -> Result<String> {
        let response = self
            .client
            .get(self.url("/diagnostics"))
            .send()
            .context("failed to GET /diagnostics")?;
        response
            .text()
            .context("failed to read /diagnostics body as text")
    }

    /// Wave 7 / PR 7 — GET `/api/code/threads?limit=&offset=`. The
    /// query params are passed through verbatim (caller may use
    /// `Some("abc")` to drive the validation-error case) and the
    /// response is returned as `(StatusCode, body)` regardless of
    /// success status.
    pub fn matrix_get_threads(
        &self,
        limit: Option<&str>,
        offset: Option<&str>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self.client.get(self.url("/threads"));
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(l) = limit {
            query.push(("limit", l));
        }
        if let Some(o) = offset {
            query.push(("offset", o));
        }
        if !query.is_empty() {
            request = request.query(&query);
        }
        let response = request.send().context("failed to GET /threads")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    /// Wave 7 / PR 7 — return the contents of the spawned process's
    /// `libra.log` so the audit-redaction case can scan for both
    /// the present marker (`controller.attach`) and the absent
    /// secret-like client id.
    pub fn libra_log_text(&self) -> Result<String> {
        let path = self.logs_dir.join("libra.log");
        fs::read_to_string(&path)
            .with_context(|| format!("failed to read libra log at '{}'", path.display()))
    }

    /// Wave 6 / PR 6 — POST `/api/code/interactions/{id}` with a
    /// caller-supplied JSON body. The matrix runner builds the body
    /// itself (`{ "approved": true, ... }`) so individual cases can
    /// exercise approve/reject, `applyToFuture`, `selectedOption`,
    /// and `answers` without baking those choices into the helper.
    pub fn matrix_respond_interaction(
        &self,
        interaction_id: &str,
        body: &Value,
        token: &super::matrix::TokenSource,
        auth: &super::matrix::AuthMode,
        tokens: &std::collections::HashMap<super::matrix::TokenSlot, String>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .post(self.url(&format!("/interactions/{interaction_id}")))
            .json(body);
        request = self.apply_controller_token(request, token, tokens);
        request = self.apply_control_auth(request, auth);
        let response = request
            .send()
            .context("failed to send matrix interaction response")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    fn apply_controller_token(
        &self,
        request: reqwest::blocking::RequestBuilder,
        token: &super::matrix::TokenSource,
        tokens: &std::collections::HashMap<super::matrix::TokenSlot, String>,
    ) -> reqwest::blocking::RequestBuilder {
        match token {
            super::matrix::TokenSource::None => request,
            super::matrix::TokenSource::Current => {
                if let Some(value) = tokens.get(&super::matrix::TokenSlot::Current) {
                    request.header("X-Code-Controller-Token", value)
                } else {
                    request
                }
            }
            super::matrix::TokenSource::Stale => {
                if let Some(value) = tokens.get(&super::matrix::TokenSlot::Stale) {
                    request.header("X-Code-Controller-Token", value)
                } else {
                    request
                }
            }
            super::matrix::TokenSource::Forged => request.header(
                "X-Code-Controller-Token",
                super::matrix::forged_controller_token(),
            ),
        }
    }

    fn apply_control_auth(
        &self,
        request: reqwest::blocking::RequestBuilder,
        auth: &super::matrix::AuthMode,
    ) -> reqwest::blocking::RequestBuilder {
        match auth {
            super::matrix::AuthMode::ValidControl => {
                request.header("X-Libra-Control-Token", &self.control_token)
            }
            super::matrix::AuthMode::InvalidControl => {
                request.header("X-Libra-Control-Token", "00000000-deadbeef")
            }
            super::matrix::AuthMode::MissingControl | super::matrix::AuthMode::None => request,
        }
    }

    pub fn submit_message(&self, text: &str) -> Result<StatusCode> {
        let response = self
            .authorized_post("/messages")
            .json(&json!({ "text": text }))
            .send()
            .context("failed to submit automation message")?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "message submit failed with {status}: {}",
                response.text().unwrap_or_default()
            );
        }
        Ok(status)
    }

    pub fn detach_automation(&self, client_id: &str) -> Result<StatusCode> {
        let response = self
            .authorized_post("/controller/detach")
            .json(&json!({ "clientId": client_id }))
            .send()
            .context("failed to send automation detach request")?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "automation detach failed with {status}: {}",
                response.text().unwrap_or_default()
            );
        }
        Ok(status)
    }

    pub fn submit_message_expect_error(&self, text: &str) -> Result<(StatusCode, Value)> {
        let response = self
            .authorized_post("/messages")
            .json(&json!({ "text": text }))
            .send()
            .context("failed to submit automation message")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    pub fn respond_interaction_expect_error(
        &self,
        interaction_id: &str,
    ) -> Result<(StatusCode, Value)> {
        let response = self
            .authorized_post(&format!("/interactions/{interaction_id}"))
            .json(&json!({ "approved": true }))
            .send()
            .context("failed to submit automation interaction response")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    pub fn submit_large_message(&self, bytes: usize) -> Result<(StatusCode, Value)> {
        let text = "x".repeat(bytes);
        let response = self
            .authorized_post("/messages")
            .json(&json!({ "text": text }))
            .send()
            .context("failed to submit large automation message")?;
        let status = response.status();
        let body = response.json().unwrap_or_else(|_| json!({}));
        Ok((status, body))
    }

    pub fn cancel_turn(&self) -> Result<StatusCode> {
        let response = self
            .authorized_post("/control/cancel")
            .send()
            .context("failed to send cancel request")?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "cancel failed with {status}: {}",
                response.text().unwrap_or_default()
            );
        }
        Ok(status)
    }

    pub fn write_tui_line(&mut self, line: &str) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| anyhow!("PTY writer is closed"))?;
        writer
            .write_all(line.as_bytes())
            .context("failed to write line to PTY")?;
        writer
            .write_all(b"\r")
            .context("failed to write enter key to PTY")?;
        writer.flush().context("failed to flush PTY writer")
    }

    pub fn wait_for_snapshot<F>(&self, timeout: Duration, mut predicate: F) -> Result<Value>
    where
        F: FnMut(&Value) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut last = Value::Null;
        while Instant::now() < deadline {
            match self.snapshot() {
                Ok(snapshot) => {
                    if predicate(&snapshot) {
                        return Ok(snapshot);
                    }
                    last = snapshot;
                }
                Err(error) => {
                    last = json!({ "error": error.to_string() });
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        bail!(
            "timed out waiting for snapshot condition; last snapshot: {last}\n{}",
            self.debug_context()
        )
    }

    pub fn shutdown(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.write_all(b"/quit\r");
            let _ = writer.flush();
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.child_exited()? {
                self.join_reader();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }

        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.join_reader();
        Ok(())
    }

    /// Abruptly terminate the spawned TUI without sending `/quit`.
    ///
    /// This intentionally leaves default control files behind when the child is
    /// killed with SIGKILL, letting stale-PID takeover tests prove a follow-up
    /// process ignores old discovery metadata once the OS releases the lock.
    pub fn kill_without_cleanup(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            {
                if let Some(pid) = child.process_id() {
                    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                    if result != 0 {
                        return Err(std::io::Error::last_os_error())
                            .with_context(|| format!("failed to SIGKILL libra code child {pid}"));
                    }
                } else {
                    child.kill().context("failed to kill libra code child")?;
                }
            }
            #[cfg(not(unix))]
            {
                child.kill().context("failed to kill libra code child")?;
            }
            child
                .wait()
                .context("failed to wait for killed libra code child")?;
            self.child = None;
        }
        self.join_reader();
        Ok(())
    }

    /// Ask the spawned TUI to terminate without first sending `/quit`.
    ///
    /// Unlike [`Self::shutdown`], this exercises OS-level process termination
    /// while preserving any session snapshots the runtime already flushed.
    pub fn terminate_without_cleanup(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            #[cfg(unix)]
            {
                if let Some(pid) = child.process_id() {
                    let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                    if result != 0 {
                        return Err(std::io::Error::last_os_error())
                            .with_context(|| format!("failed to SIGTERM libra code child {pid}"));
                    }
                } else {
                    child.kill().context("failed to kill libra code child")?;
                }
            }
            #[cfg(not(unix))]
            {
                child.kill().context("failed to kill libra code child")?;
            }

            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if child
                    .try_wait()
                    .context("failed to poll terminated libra code child")?
                    .is_some()
                {
                    child
                        .wait()
                        .context("failed to wait for terminated libra code child")?;
                    self.child = None;
                    self.join_reader();
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }

            child
                .kill()
                .context("failed to SIGKILL libra code child after SIGTERM timeout")?;
            child
                .wait()
                .context("failed to wait for killed libra code child after SIGTERM timeout")?;
            self.child = None;
        }
        self.join_reader();
        Ok(())
    }

    /// SIGTERM the TUI and require a natural exit (no SIGKILL fallback).
    ///
    /// Used by W1-08 to prove the ProcessTerminateGate path in `App::run`
    /// reaches the shared lifecycle owner instead of hanging until force-kill.
    #[cfg(unix)]
    pub fn sigterm_expect_natural_exit(&mut self, timeout: Duration) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            self.join_reader();
            return Ok(());
        };
        let pid = child
            .process_id()
            .ok_or_else(|| anyhow!("TUI child has no process id for SIGTERM"))?;
        let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to SIGTERM libra code child {pid}"));
        }

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if child
                .try_wait()
                .context("failed to poll SIGTERM'd libra code child")?
                .is_some()
            {
                let status = child
                    .wait()
                    .context("failed to wait for naturally exited libra code child")?;
                self.child = None;
                self.join_reader();
                if !status.success() {
                    bail!(
                        "libra code TUI exited after SIGTERM with failure status {status:?}\n{}",
                        self.debug_context()
                    );
                }
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = child.kill();
        let _ = child.wait();
        self.child = None;
        self.join_reader();
        bail!(
            "libra code TUI did not exit naturally within {timeout:?} after SIGTERM; forced SIGKILL\n{}",
            self.debug_context()
        )
    }

    fn wait_for_control_info(
        &mut self,
        timeout: Duration,
        previous_info_text: Option<String>,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.child_exited()? {
                bail!(
                    "libra code exited before writing control info; logs: {}",
                    self.logs_dir.display()
                );
            }
            if self.info_path.exists() {
                let info_text = fs::read_to_string(&self.info_path)
                    .context("failed to read control info file")?;
                if previous_info_text.as_ref() == Some(&info_text) {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let info: ControlInfo =
                    serde_json::from_str(&info_text).context("failed to parse control info")?;
                // The runtime writes `control.json` TWICE for non-stdio
                // launches: once after the web server binds (mcpUrl=null,
                // see `src/command/code.rs:817`) and again after the MCP
                // server binds (mcpUrl populated, see `code.rs:832`).
                // Wait for the second write so consumers like
                // `mcp_url()` always see the populated value.
                if info.mcp_url.is_none() {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let _ = fs::write(self.logs_dir.join("control.json"), &info_text);
                self.base_url = info.base_url;
                self.mcp_url = info.mcp_url;
                if self.control_write {
                    // Token file is only written under `--control write`;
                    // observe-mode sessions skip it on purpose.
                    let token = fs::read_to_string(&self.token_path)
                        .context("failed to read control token file")?;
                    self.control_token = token.trim().to_string();
                }
                self.wait_for_snapshot(Duration::from_secs(10), |_| true)?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        bail!(
            "timed out waiting for control info file '{}'; logs: {}",
            self.info_path.display(),
            self.logs_dir.display()
        )
    }

    fn get_json(&self, path: &str) -> Result<Value> {
        let response = self
            .client
            .get(self.url(path))
            .send()
            .with_context(|| format!("failed to GET {path}"))?;
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("failed to read GET {path} response body"))?;
        if !status.is_success() {
            bail!("GET {path} failed with {status}: {body}");
        }
        serde_json::from_str(&body).with_context(|| format!("failed to parse GET {path} JSON"))
    }

    fn authorized_post(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        let mut request = self
            .client
            .post(self.url(path))
            .header("X-Libra-Control-Token", &self.control_token);
        if let Some(token) = self.controller_token.as_ref() {
            request = request.header("X-Code-Controller-Token", token);
        }
        request
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/code{}", self.base_url, path)
    }

    fn child_exited(&mut self) -> Result<bool> {
        let Some(child) = self.child.as_mut() else {
            return Ok(true);
        };
        match child
            .try_wait()
            .context("failed to poll libra code child")?
        {
            Some(_status) => {
                self.child = None;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn join_reader(&mut self) {
        self.writer.take();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }

    fn redact_known_secrets(&self, mut text: String) -> String {
        if !self.control_token.is_empty() {
            text = text.replace(&self.control_token, "[REDACTED_CONTROL_TOKEN]");
        }
        if let Some(token) = self.controller_token.as_ref() {
            text = text.replace(token, "[REDACTED_CONTROLLER_TOKEN]");
        }
        text
    }
}

impl Drop for CodeSession {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.shutdown();
        }
        // Wave 9 / PR 9 — Codex pass-1 fix: when the child already
        // exited (e.g. spawn failed because `--resume <bad>` made
        // the runtime abort early), `child_exited()` cleared
        // `self.child` long before Drop ran, so the
        // `if child.is_some()` shutdown branch was a no-op and
        // the PTY reader thread was never joined. The negative-
        // path resume tests then race the reader's pty.log flush.
        // Join unconditionally here so the log file is fully
        // flushed by the time the test body reads it.
        self.join_reader();
    }
}

fn ensure_success(status: StatusCode, body: &str) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        bail!("request failed with {status}: {body}")
    }
}

fn libra_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_libra")
        .map(PathBuf::from)
        .expect("CARGO_BIN_EXE_libra is set for integration tests")
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn tail_file(path: &Path, max_lines: usize) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return format!("<unavailable: {}>", path.display());
    };
    let mut lines: Vec<&str> = text.lines().rev().take(max_lines).collect();
    lines.reverse();
    if lines.is_empty() {
        "<empty>".to_string()
    } else {
        lines.join("\n")
    }
}
