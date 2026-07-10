//! External `libra-agent-<name>` binary RPC.
//!
//! Phase 4.5 (entire.md §14.4 item 5) — bring Libra to parity with
//! EntireIO's plugin model. An external binary named
//! `libra-agent-<name>` (e.g. `libra-agent-cursor`) found on `PATH`
//! becomes a recognisable adapter: Libra spawns it, exchanges
//! line-delimited JSON requests / responses on stdin/stdout, and
//! routes the answers back through the same `ObservedAgent`-style
//! API used by built-in adapters.
//!
//! v1 surface intentionally narrow:
//! - **Discovery**: scan every `PATH` entry for executables matching
//!   the `libra-agent-*` pattern. Returns one [`RpcAgent`] per binary.
//! - **Protocol**: a single JSON object per line. Request:
//!   `{"jsonrpc": "2.0", "method": <name>, "params": <object|null>, "id": <int>}`.
//!   Response: `{"jsonrpc": "2.0", "result": <value>, "id": <int>}` or
//!   `{"jsonrpc": "2.0", "error": {"code": <int>, "message": <string>}, "id": <int>}`.
//! - **Methods** (registered names — adapters must answer these):
//!   `provider_kind` → `{"kind": "<snake_case>"}`,
//!   `provider_name` → `{"name": "<slug>"}`,
//!   `protected_dirs` → `{"dirs": ["..."]}`,
//!   `read_transcript` → `{"bytes": "<base64>"} | {"none": true}`.
//! - **Capability negotiation**: response to a `capabilities` method
//!   (mandatory) returns the set of methods the binary implements.
//!   The runtime ONLY invokes methods listed there.
//!
//! Out of scope for v1: streaming responses, hooks/lifecycle events
//! (these go through the in-process `HookProvider` via `ObservedAgent::as_hooks()`),
//! truncation. Future work in `entire.md` §14 phase 5 picks up
//! capability v2 (events stream, hook installation by binary).

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// File-name prefix the discovery scan looks for.
pub const RPC_BINARY_PREFIX: &str = "libra-agent-";

/// Hard cap on how long a single RPC request may take. Protects the
/// runtime from hanging on a misbehaving binary.
pub const RPC_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on a single response frame. A binary that streams bytes
/// without newlines (or with one obscenely large line) is a DoS
/// vector — capping read lengths means we fail fast instead of
/// growing memory unbounded.
pub const RPC_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// Bound on outstanding response frames buffered in the reader-thread
/// channel. A misbehaving binary that floods stdout cannot grow
/// memory beyond this many in-flight lines.
pub const RPC_RESPONSE_CHANNEL_CAPACITY: usize = 64;

/// Bound on a single serialized request frame. The OS pipe buffer
/// is small (~64 KiB on Linux, ~16 KiB on macOS), so any write
/// past that point blocks `writeln!` until the child drains its
/// stdin. The cap does NOT guarantee fit-in-pipe — a 1 MiB write
/// will still block if the child has stopped reading. What it
/// DOES guarantee is that no single request grows unbounded
/// (e.g. an accidentally megabyte-sized `params` blob), which
/// keeps the worst-case stall short and bounded.
///
/// The actual safety contract is upstream: v1 assumes the child
/// reads stdin promptly. Truly async writes (writer thread,
/// nonblocking pipe) are deferred to v2. Callers whose payload
/// exceeds the cap get a typed error before we touch the pipe.
pub const RPC_MAX_REQUEST_BYTES: usize = 1024 * 1024; // 1 MiB

/// Protocol version this runtime speaks (E2). Injected into the child's
/// environment as `LIBRA_AGENT_PROTOCOL_VERSION`; binaries answering `info`
/// with a higher major version are rejected fail-closed
/// (`LBR-AGENT-003` semantics at the CLI layer).
pub const RPC_PROTOCOL_VERSION: u32 = 2;

/// Hard cap on captured child stderr (AG-18). Anything past this is
/// dropped with a truncation marker; stderr is never inherited and never
/// surfaced raw — only redacted excerpts.
pub const RPC_MAX_STDERR_BYTES: usize = 64 * 1024;

/// Non-secret parent-environment variables passed through to a spawned
/// `libra-agent-*` binary after `env_clear()` (AG-18 / E2 allowlist,
/// agent.md §强制补强项 #2). Real external CLIs need these to locate their
/// interpreter, dependencies, config and dotfiles; a child with an empty
/// `PATH`/`HOME` typically cannot run at all. Membership is an exact,
/// case-sensitive name match — no wildcards — so credential/endpoint
/// variables (`*_API_KEY`, `LIBRA_STORAGE_*`, `LIBRA_D1_*`, `*_TOKEN`,
/// `*_SECRET`, `*_PASSWORD`, `*_BASE_URL`, …) are never on this list and
/// stay cleared. `LIBRA_AGENT_PROTOCOL_DEBUG` is only forwarded when the
/// parent has it set (default: cleared).
pub const RPC_ENV_PASSTHROUGH_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TZ",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "LIBRA_AGENT_PROTOCOL_DEBUG",
];

/// Typed classification for RPC failures (AG-18). Attached to the
/// `anyhow` chain via `.context(...)` so callers can `downcast_ref`
/// instead of substring-matching over messages that may embed
/// child-controlled text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RpcFailureKind {
    /// Deadline elapsed; the child was killed (`LBR-AGENT-012`).
    #[error("rpc timeout")]
    Timeout,
    /// A hard IO cap (request/frame/stderr) was violated (`LBR-AGENT-007`).
    #[error("rpc io cap violation")]
    IoCap,
    /// The transport broke (stdout closed, read error) (`LBR-AGENT-012`).
    #[error("rpc transport failure")]
    Transport,
    /// The binary answered with a JSON-RPC error frame; the payload is the
    /// frame's `error.code` (−32601 = method not found → v1 fallback).
    #[error("rpc error frame (code {0})")]
    ErrorFrame(i32),
    /// Protocol violation (bad JSON, wrong id, wrong jsonrpc version)
    /// (`LBR-AGENT-012`).
    #[error("rpc protocol violation")]
    Protocol,
    /// The binary negotiated an incompatible protocol version
    /// (`LBR-AGENT-003`).
    #[error("rpc protocol version mismatch")]
    ProtocolVersion,
}

/// Extract the typed [`RpcFailureKind`] marker from an `anyhow` error.
///
/// The marker may sit in the chain in either shape:
/// - as the **root error** (`Err(RpcFailureKind::Timeout).with_context(..)`),
///   which `chain()` + `dyn Error::downcast_ref` finds; or
/// - as a **context layer** (`Err(io_err).context(RpcFailureKind::Transport)`),
///   which only `anyhow::Error::downcast_ref` finds — `chain()` yields the
///   wrapping `ContextError`, whose concrete type never matches.
///
/// Checking only one shape silently drops the classification for the
/// other (the CLI then falls back to `LBR-INTERNAL-001`), so both are
/// consulted.
pub fn rpc_failure_kind(error: &anyhow::Error) -> Option<RpcFailureKind> {
    if let Some(kind) = error.downcast_ref::<RpcFailureKind>() {
        return Some(*kind);
    }
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RpcFailureKind>().copied())
}

/// v2 `info` response (E2). `protocol_version` is optional on the wire —
/// a missing value means the binary is a v1 speaker.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    #[serde(default)]
    pub protocol_version: Option<u32>,
    pub name: String,
    #[serde(rename = "type", default)]
    pub agent_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_preview: bool,
    #[serde(default)]
    pub protected_dirs: Vec<String>,
    #[serde(default)]
    pub protected_files: Vec<String>,
    #[serde(default)]
    pub hook_names: Vec<String>,
    /// E1 8-bool capability object. NOTE: same *name* as the v1
    /// `capabilities` method but a different wire shape (`{methods:[..]}`)
    /// — do not conflate the two.
    #[serde(default)]
    pub capabilities: super::capability::DeclaredAgentCaps,
}

/// Discovered binary plus its launch path. The runtime owns one of
/// these per binary; capability negotiation happens lazily on first
/// invocation.
#[derive(Debug, Clone)]
pub struct RpcAgentBinary {
    /// Slug suffix after `libra-agent-` (e.g. `cursor`).
    pub slug: String,
    /// Absolute path to the executable.
    pub binary_path: PathBuf,
}

/// Live JSON-RPC channel against a spawned [`RpcAgentBinary`]. Owns
/// the child process; `Drop` reaps it gracefully (sends a `shutdown`
/// notification before killing as a fallback). Use [`RpcAgent::spawn`]
/// to create one.
///
/// Stdout is read on a dedicated reader thread that pumps complete
/// JSON lines into a bounded `sync_channel`. The invoke loop polls
/// the channel with a deadline so a child that never writes a
/// newline gets killed at the timeout — a blocking `read_line` here
/// would hang the runtime indefinitely.
pub struct RpcAgent {
    binary: RpcAgentBinary,
    child: Child,
    stdin: ChildStdin,
    /// Lines (newline-terminated, trimmed) and read failures arrive
    /// here from the reader thread. The error variant is an
    /// `anyhow::Error` carrying the slug + IO context. The thread
    /// drops its sender on EOF so the receiver detects
    /// `Disconnected`.
    response_rx: Receiver<Result<String>>,
    reader_handle: Option<JoinHandle<()>>,
    next_id: AtomicU64,
    /// Cached capabilities returned by the first `capabilities` call.
    /// Once populated, the runtime refuses to call any method outside
    /// this set.
    capabilities: Mutex<Option<Vec<String>>>,
    /// Cached v2 `info` response (None until negotiated; also None for
    /// pure-v1 binaries that lack the method).
    info: Mutex<Option<AgentInfo>>,
    /// Captured child stderr, capped at [`RPC_MAX_STDERR_BYTES`] (AG-18:
    /// stderr is never inherited; only redacted excerpts may surface).
    stderr_buf: std::sync::Arc<Mutex<Vec<u8>>>,
    stderr_truncated: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stderr_handle: Option<JoinHandle<()>>,
}

/// One JSON-RPC request frame. `id` is monotonic per binary; the
/// runtime correlates responses by id.
#[derive(Debug, Clone, Serialize)]
pub struct RpcRequest {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    pub id: u64,
}

/// One JSON-RPC response frame. Either `result` or `error` is
/// populated, never both. `id` MUST match the request the binary is
/// answering.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResponse {
    #[serde(default)]
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Structured error payload returned by an RPC binary.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcAgent {
    /// Spawn `binary` as a child process and prepare a JSON-RPC
    /// channel against it. The child's stderr is captured into a capped,
    /// redacted in-memory buffer — never inherited to the operator's
    /// terminal (see [`Self::redacted_stderr_excerpt`]); stdout/stdin are
    /// piped for RPC traffic.
    ///
    /// A dedicated reader thread pumps complete lines from stdout
    /// into a bounded sync channel so the timeout in `invoke` can
    /// actually fire on a non-responsive child.
    pub fn spawn(binary: RpcAgentBinary) -> Result<Self> {
        Self::spawn_in_repo(binary, None)
    }

    /// [`Self::spawn`] with an explicit repository root exported to the
    /// child as `LIBRA_REPO_ROOT`.
    ///
    /// Security contract (AG-18 / E2): the child environment is cleared
    /// and only an explicit allowlist is re-injected — the derived
    /// `LIBRA_AGENT_PROTOCOL_VERSION` / `LIBRA_CLI_VERSION` / (when given)
    /// `LIBRA_REPO_ROOT`, plus the non-secret parent variables in
    /// [`RPC_ENV_PASSTHROUGH_ALLOWLIST`] (`PATH`, `HOME`, locale, …) that
    /// real external CLIs need to locate their interpreter, dependencies
    /// and config. Provider API keys, `LIBRA_STORAGE_*`, `LIBRA_D1_*`,
    /// `*_TOKEN`/`*_SECRET` and the rest of the parent environment never
    /// reach the binary. Stderr is piped into a capped in-memory buffer
    /// (never inherited); use [`Self::redacted_stderr_excerpt`] for
    /// diagnostics.
    pub fn spawn_in_repo(
        binary: RpcAgentBinary,
        repo_root: Option<&std::path::Path>,
    ) -> Result<Self> {
        Self::spawn_in_repo_with_env(binary, repo_root, &[])
    }

    /// [`Self::spawn_in_repo`] plus an operator-configured
    /// `extra_env_allowlist` of additional exact env-var names to pass through
    /// (A0-08 `agent.external_agents.env_allowlist_extra`). Each name is
    /// re-checked against [`env_name_is_forbidden`] here as defense-in-depth,
    /// so a credential/endpoint name can never reach the child even if it
    /// slipped into config.
    pub fn spawn_in_repo_with_env(
        binary: RpcAgentBinary,
        repo_root: Option<&std::path::Path>,
        extra_env_allowlist: &[String],
    ) -> Result<Self> {
        let mut command = Command::new(&binary.binary_path);
        command
            .env_clear()
            .env(
                "LIBRA_AGENT_PROTOCOL_VERSION",
                RPC_PROTOCOL_VERSION.to_string(),
            )
            .env("LIBRA_CLI_VERSION", env!("CARGO_PKG_VERSION"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Re-inject the non-secret passthrough allowlist (only the vars the
        // parent actually has set). `env_clear()` above guarantees anything
        // not on this list — including every credential/endpoint variable —
        // stays out of the child.
        for name in RPC_ENV_PASSTHROUGH_ALLOWLIST {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        // A0-08 extra passthrough: operator-approved additional names, with a
        // hard secret/wildcard denial re-applied here.
        for name in extra_env_allowlist {
            if super::trust::env_name_is_forbidden(name) {
                continue;
            }
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(root) = repo_root {
            command.env("LIBRA_REPO_ROOT", root);
        }
        // Retry briefly on ETXTBSY (os error 26): when another thread in
        // this process forks while a just-written executable still has a
        // write fd open (inherited across the fork), the first exec can
        // transiently fail with "Text file busy". Seen under parallel
        // test load; a bounded retry is the standard mitigation and is
        // harmless for real spawns.
        let child = {
            let mut attempt = 0u8;
            loop {
                match command.spawn() {
                    Ok(child) => break child,
                    Err(err) if err.raw_os_error() == Some(26 /* ETXTBSY */) && attempt < 5 => {
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!(
                                "spawn libra-agent binary at {}",
                                binary.binary_path.display()
                            )
                        });
                    }
                }
            }
        };

        // RAII guard: if anything below `?`s out, drop kills+reaps
        // the child so we never leak a running process. On the
        // success path we `forget` the guard and move ownership into
        // `Self`.
        let mut guard = ChildReapGuard { child: Some(child) };
        let stdin;
        let stdout;
        let stderr;
        {
            // Borrow the child through the guard for stdin/stdout
            // extraction so an early ? still triggers reaping.
            let child_ref = guard.child.as_mut().ok_or_else(|| {
                anyhow!(
                    "internal error: child reap guard for {} was empty",
                    binary.slug
                )
            })?;
            stdin = child_ref
                .stdin
                .take()
                .ok_or_else(|| anyhow!("child {} closed stdin unexpectedly", binary.slug))?;
            stdout = BufReader::new(
                child_ref
                    .stdout
                    .take()
                    .ok_or_else(|| anyhow!("child {} closed stdout unexpectedly", binary.slug))?,
            );
            stderr = child_ref
                .stderr
                .take()
                .ok_or_else(|| anyhow!("child {} closed stderr unexpectedly", binary.slug))?;
        }
        let (tx, response_rx) = mpsc::sync_channel::<Result<String>>(RPC_RESPONSE_CHANNEL_CAPACITY);
        let reader_slug = binary.slug.clone();
        let reader_handle = thread::Builder::new()
            .name(format!("libra-rpc-reader-{}", reader_slug))
            .spawn(move || pump_stdout_lines(stdout, tx, &reader_slug))
            .context("spawn RPC reader thread")?;
        let stderr_buf = std::sync::Arc::new(Mutex::new(Vec::new()));
        let stderr_truncated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stderr_handle = thread::Builder::new()
            .name(format!("libra-rpc-stderr-{}", binary.slug))
            .spawn({
                let buf = std::sync::Arc::clone(&stderr_buf);
                let truncated = std::sync::Arc::clone(&stderr_truncated);
                move || pump_stderr_capped(stderr, &buf, &truncated)
            })
            .context("spawn RPC stderr capture thread")?;
        let child = guard.child.take().ok_or_else(|| {
            anyhow!(
                "internal error: child reap guard for {} was empty after stdio extraction",
                binary.slug
            )
        })?;
        std::mem::forget(guard);
        Ok(Self {
            binary,
            child,
            stdin,
            response_rx,
            reader_handle: Some(reader_handle),
            next_id: AtomicU64::new(1),
            capabilities: Mutex::new(None),
            info: Mutex::new(None),
            stderr_buf,
            stderr_truncated,
            stderr_handle: Some(stderr_handle),
        })
    }

    /// A short, redacted, control-sequence-free excerpt of the child's
    /// captured stderr for diagnostics. Never returns raw bytes: the
    /// buffer is capped at [`RPC_MAX_STDERR_BYTES`], run through the
    /// default [`super::redaction::Redactor`], and ANSI/control
    /// characters are stripped so a hostile binary cannot inject
    /// terminal escapes. Returns `None` when nothing was written.
    pub fn redacted_stderr_excerpt(&self) -> Option<String> {
        let buf = self.stderr_buf.lock().ok()?;
        if buf.is_empty() {
            return None;
        }
        let redactor = super::redaction::Redactor::new_default();
        let (redacted, _report) = redactor.redact(&buf);
        let mut clean: String = String::from_utf8_lossy(redacted.as_ref())
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' && c != '\t' {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect();
        const EXCERPT_CHARS: usize = 2048;
        if clean.chars().count() > EXCERPT_CHARS {
            clean = clean.chars().take(EXCERPT_CHARS).collect::<String>() + "…[excerpt truncated]";
        }
        if self
            .stderr_truncated
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            clean.push_str(" [stderr capped at 64 KiB]");
        }
        Some(clean)
    }

    /// Send a JSON-RPC request and wait for the matching response,
    /// using [`RPC_DEFAULT_TIMEOUT`].
    pub fn invoke(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        self.invoke_with_timeout(method, params, RPC_DEFAULT_TIMEOUT)
    }

    /// Send a JSON-RPC request and wait for the matching response,
    /// up to `timeout`. The binary is killed on timeout so a hang
    /// doesn't propagate.
    ///
    /// Capability gating: any method other than `capabilities` is
    /// rejected with `Err` if the binary did not advertise it via the
    /// `capabilities` exchange. Callers therefore typically invoke
    /// `negotiate_capabilities` once before any other method.
    pub fn invoke_with_timeout(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        // `capabilities` (v1) and `info` (v2) are the negotiation
        // bootstrap methods — everything else is capability-gated
        // fail-closed (E1/E2; `LBR-AGENT-004` semantics upstream).
        if method != "capabilities" && method != "info" {
            let caps = self.capabilities.lock().map_err(|_| {
                anyhow!(
                    "RPC capabilities mutex for {} was poisoned by an earlier panic",
                    self.binary.slug
                )
            })?;
            match caps.as_ref() {
                None => bail!(
                    "must call `capabilities` before any other RPC against {}",
                    self.binary.slug
                ),
                Some(c) if !c.iter().any(|s| s == method) => bail!(
                    "binary {} does not advertise method '{method}' (capabilities: {c:?})",
                    self.binary.slug
                ),
                _ => {}
            }
        }
        let span_protocol_version = self.negotiated_protocol_version();
        let invoke_span = tracing::info_span!(
            "agent.rpc.invoke",
            slug = %self.binary.slug,
            method = %method,
            protocol_version = span_protocol_version,
            timeout_ms = timeout.as_millis() as u64,
            frame_bytes = tracing::field::Empty,
            terminal_state = tracing::field::Empty,
        );
        let _entered = invoke_span.enter();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = RpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
            id,
        };
        let line = serde_json::to_string(&request)
            .with_context(|| format!("serialize RPC request for {method}"))?;
        // Bound the request size before we write so a runaway
        // `params` payload cannot block `writeln!` on a stuck
        // child's stdin pipe. See [`RPC_MAX_REQUEST_BYTES`] docs
        // for the v1 contract limits.
        if line.len() + 1 > RPC_MAX_REQUEST_BYTES {
            invoke_span.record("frame_bytes", 0_u64);
            invoke_span.record("terminal_state", "io_cap");
            return Err(RpcFailureKind::IoCap).with_context(|| {
                format!(
                    "RPC request for '{method}' against {} is {} bytes, exceeds limit of {} bytes",
                    self.binary.slug,
                    line.len() + 1,
                    RPC_MAX_REQUEST_BYTES
                )
            });
        }
        // Write request line + LF terminator.
        if let Err(err) = writeln!(self.stdin, "{line}") {
            invoke_span.record("frame_bytes", 0_u64);
            invoke_span.record("terminal_state", "error");
            return Err(err)
                .context(RpcFailureKind::Transport)
                .with_context(|| {
                    format!(
                        "write RPC request to {} stdin (likely the child died)",
                        self.binary.slug
                    )
                });
        }
        if let Err(err) = self.stdin.flush() {
            invoke_span.record("frame_bytes", 0_u64);
            invoke_span.record("terminal_state", "error");
            return Err(err)
                .context(RpcFailureKind::Transport)
                .with_context(|| format!("flush RPC request to {} stdin", self.binary.slug));
        }

        // Read responses through the dedicated reader thread's
        // channel. `recv_timeout` lets us enforce the deadline even
        // if the child never writes a newline — the previous
        // blocking `read_line` could not.
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = self.child.kill();
                invoke_span.record("frame_bytes", 0_u64);
                invoke_span.record("terminal_state", "timeout");
                return Err(RpcFailureKind::Timeout).with_context(|| {
                    format!(
                        "RPC method '{method}' against {} timed out after {:?}{}",
                        self.binary.slug,
                        timeout,
                        self.stderr_suffix()
                    )
                });
            }
            let line = match self.response_rx.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(err)) => {
                    invoke_span.record("frame_bytes", 0_u64);
                    invoke_span.record("terminal_state", "error");
                    return Err(err)
                        .with_context(|| format!("read RPC response from {}", self.binary.slug));
                }
                Err(RecvTimeoutError::Timeout) => {
                    let _ = self.child.kill();
                    invoke_span.record("frame_bytes", 0_u64);
                    invoke_span.record("terminal_state", "timeout");
                    return Err(RpcFailureKind::Timeout).with_context(|| {
                        format!(
                            "RPC method '{method}' against {} timed out after {:?}{}",
                            self.binary.slug,
                            timeout,
                            self.stderr_suffix()
                        )
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    invoke_span.record("frame_bytes", 0_u64);
                    invoke_span.record("terminal_state", "error");
                    return Err(RpcFailureKind::Transport).with_context(|| {
                        format!(
                            "RPC binary {} closed stdout before answering '{method}'{}",
                            self.binary.slug,
                            self.stderr_suffix()
                        )
                    });
                }
            };
            if line.is_empty() {
                continue;
            }
            let resp: RpcResponse = match serde_json::from_str(&line) {
                Ok(resp) => resp,
                Err(err) => {
                    invoke_span.record("frame_bytes", line.len() as u64);
                    invoke_span.record("terminal_state", "error");
                    return Err(err).context(RpcFailureKind::Protocol).with_context(|| {
                        format!("parse RPC response line from {}: {line}", self.binary.slug)
                    });
                }
            };
            if resp.jsonrpc != "2.0" {
                invoke_span.record("frame_bytes", line.len() as u64);
                invoke_span.record("terminal_state", "error");
                return Err(RpcFailureKind::Protocol).with_context(|| {
                    format!(
                        "RPC binary {} returned unsupported jsonrpc version {:?} (expected \"2.0\")",
                        self.binary.slug, resp.jsonrpc
                    )
                });
            }
            if resp.id != id {
                // v1 is strictly synchronous: we never have a second
                // request in flight, so any other id is the binary
                // breaking the protocol — surface it instead of
                // burning the deadline waiting for the right id.
                invoke_span.record("frame_bytes", line.len() as u64);
                invoke_span.record("terminal_state", "error");
                return Err(RpcFailureKind::Protocol).with_context(|| {
                    format!(
                        "RPC binary {} returned response for id {} while waiting for id {} (method '{method}')",
                        self.binary.slug, resp.id, id
                    )
                });
            }
            if let Some(err) = resp.error {
                invoke_span.record("frame_bytes", line.len() as u64);
                invoke_span.record("terminal_state", "rpc_error");
                return Err(RpcFailureKind::ErrorFrame(err.code)).with_context(|| {
                    format!(
                        "RPC method '{method}' against {} returned error {}: {}",
                        self.binary.slug, err.code, err.message
                    )
                });
            }
            invoke_span.record("frame_bytes", line.len() as u64);
            invoke_span.record("terminal_state", "ok");
            return resp.result.ok_or_else(|| {
                anyhow!("RPC response for '{method}' had neither result nor error")
            });
        }
    }

    /// Redacted stderr suffix for error messages (empty when the child
    /// wrote nothing).
    fn stderr_suffix(&self) -> String {
        match self.redacted_stderr_excerpt() {
            Some(excerpt) => format!("; child stderr (redacted): {excerpt}"),
            None => String::new(),
        }
    }

    /// Protocol version negotiated via `info`, defaulting to 1 (a binary
    /// that never answered `info` is treated as a v1 speaker per E2).
    pub fn negotiated_protocol_version(&self) -> u32 {
        self.info
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|info| info.protocol_version.unwrap_or(1))
            })
            .unwrap_or(1)
    }

    /// Cached v2 `info` payload, when the binary provided one.
    pub fn negotiated_info(&self) -> Option<AgentInfo> {
        self.info.lock().ok().and_then(|guard| guard.clone())
    }

    /// v2 negotiation (E2): try `info` first; a binary that does not
    /// implement it stays a v1 speaker (`Ok(None)`), per the
    /// `info -> v1 capabilities -> skip-and-log` client order. A binary
    /// that answers with an incompatible `protocol_version` is rejected
    /// fail-closed (`LBR-AGENT-003` semantics at the CLI layer).
    pub fn negotiate_info(&mut self) -> Result<Option<AgentInfo>> {
        let value = match self.invoke("info", None) {
            Ok(value) => value,
            // Strict v1 fallback: ONLY JSON-RPC "method not found"
            // (−32601) downgrades to v1. Timeouts, IO-cap violations,
            // transport breaks and protocol violations must propagate
            // fail-closed instead of silently degrading.
            Err(err)
                if matches!(
                    rpc_failure_kind(&err),
                    Some(RpcFailureKind::ErrorFrame(-32601))
                ) =>
            {
                return Ok(None);
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("`info` negotiation against {} failed", self.binary.slug)
                });
            }
        };
        let info: AgentInfo = serde_json::from_value(value).with_context(|| {
            format!(
                "parse `info` response from {} (E2 v2 contract)",
                self.binary.slug
            )
        })?;
        let version = info.protocol_version.unwrap_or(1);
        if version > RPC_PROTOCOL_VERSION {
            return Err(RpcFailureKind::ProtocolVersion).with_context(|| {
                format!(
                    "binary {} speaks protocol version {version}, newer than this runtime's {} — \
                     refusing fail-closed (upgrade libra or downgrade the agent binary)",
                    self.binary.slug, RPC_PROTOCOL_VERSION
                )
            });
        }
        let mut guard = self.info.lock().map_err(|_| {
            anyhow!(
                "RPC info mutex for {} was poisoned by an earlier panic",
                self.binary.slug
            )
        })?;
        *guard = Some(info.clone());
        Ok(Some(info))
    }

    /// Mandatory first call. Asks the binary for the set of methods
    /// it implements and caches the result. Subsequent
    /// [`Self::invoke`] calls reject methods not in this set.
    pub fn negotiate_capabilities(&mut self) -> Result<Vec<String>> {
        let value = self.invoke("capabilities", None)?;
        let raw = value
            .get("methods")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                anyhow!(
                    "capabilities response from {} missing `methods` array",
                    self.binary.slug
                )
            })?;
        let mut methods = Vec::with_capacity(raw.len());
        for (idx, entry) in raw.iter().enumerate() {
            let s = entry.as_str().ok_or_else(|| {
                anyhow!(
                    "capabilities.methods[{}] from {} is {} not a string — protocol violation",
                    idx,
                    self.binary.slug,
                    match entry {
                        Value::Null => "null",
                        Value::Bool(_) => "a bool",
                        Value::Number(_) => "a number",
                        Value::Array(_) => "an array",
                        Value::Object(_) => "an object",
                        Value::String(_) => "a string", // unreachable
                    }
                )
            })?;
            methods.push(s.to_string());
        }
        let mut guard = self.capabilities.lock().map_err(|_| {
            anyhow!(
                "RPC capabilities mutex for {} was poisoned by an earlier panic",
                self.binary.slug
            )
        })?;
        *guard = Some(methods.clone());
        Ok(methods)
    }
}

/// RAII helper for [`RpcAgent::spawn`]: kills + reaps the wrapped
/// child on Drop unless `take()` has moved ownership out first. We
/// `mem::forget` the guard on the success path so ownership transfers
/// cleanly into [`RpcAgent`].
struct ChildReapGuard {
    child: Option<Child>,
}

impl Drop for ChildReapGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RpcAgent {
    fn drop(&mut self) {
        // Best-effort graceful shutdown: send a `shutdown`
        // notification (id-less) so the child can clean up, then kill
        // if it doesn't exit promptly.
        let notify = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "shutdown",
        });
        if let Ok(s) = serde_json::to_string(&notify) {
            let _ = writeln!(self.stdin, "{s}");
            let _ = self.stdin.flush();
        }
        // Give the child a brief window to exit on its own.
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // Reader thread exits on EOF once the child's stdout closes
        // (kill() above forces this if the child was still alive).
        // We must keep draining `response_rx` while we wait — if the
        // reader is mid-`tx.send(...)` on a full bounded channel, it
        // will block until we make space. Spinning on `try_recv` +
        // `is_finished` breaks that deadlock without holding the
        // current thread hostage.
        //
        // If the deadline expires WITHOUT the reader finishing
        // (worst case: an OS pipe quirk leaves stdout open even
        // after kill+wait), we deliberately do NOT call `join()` —
        // that would hang Drop indefinitely. Letting the
        // `JoinHandle` drop is safe in Rust: the OS thread keeps
        // running but is detached from our control, and will exit
        // on its next EOF read; the runtime can move on.
        if let Some(handle) = self.reader_handle.take() {
            let drain_deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < drain_deadline {
                while self.response_rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(5));
            }
            while self.response_rx.try_recv().is_ok() {}
            if handle.is_finished() {
                let _ = handle.join();
            }
            // else: detach. Drop returns; process exit will reap.
        }
        // The stderr thread exits on EOF (child stderr closed by the
        // kill/wait above); it never blocks on a channel, so a bounded
        // join is safe — detach on the same worst-case pipe quirk.
        if let Some(handle) = self.stderr_handle.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

/// Pump complete lines from the child's stdout into `tx` until EOF,
/// IO error, or a frame larger than [`RPC_MAX_FRAME_BYTES`]. Each
/// `Ok(line)` is the trimmed contents of one `\n`-terminated frame.
/// An `Err(...)` carries the failure context and ends the stream.
/// On EOF the thread returns silently — the receiver detects this
/// as `Disconnected`.
///
/// The frame cap exists so a binary that streams unterminated bytes
/// cannot grow memory unbounded; we fail loudly the moment a single
/// line exceeds the cap, and the channel closes so [`RpcAgent::invoke`]
/// surfaces the protocol violation.
///
/// `tx` is a bounded `SyncSender`. The blocking `send` waits for
/// drain when the channel fills, providing natural backpressure.
/// Teardown safety: [`RpcAgent`]'s `Drop` keeps draining `response_rx`
/// in a loop until the reader thread finishes, so a misbehaving
/// binary that floods the channel cannot wedge the reader on a
/// stuck send.
fn pump_stdout_lines(
    mut reader: BufReader<ChildStdout>,
    tx: SyncSender<Result<String>>,
    slug: &str,
) {
    loop {
        let mut buf = String::new();
        // `read_line` is unbounded, so cap the underlying reader.
        // `take()` consumes the BufReader; we restore it after the
        // read so the next iteration sees the rest of the stream.
        let mut limited = (&mut reader).take((RPC_MAX_FRAME_BYTES + 1) as u64);
        match limited.read_line(&mut buf) {
            Ok(0) => return,
            Ok(_) => {
                if buf.len() > RPC_MAX_FRAME_BYTES {
                    let _ = tx.send(Err(anyhow::Error::from(RpcFailureKind::IoCap).context(
                        format!(
                            "libra-agent-{slug} sent a frame larger than {} bytes (DoS guard)",
                            RPC_MAX_FRAME_BYTES
                        ),
                    )));
                    return;
                }
                let line = buf.trim_end_matches(['\r', '\n']).to_string();
                if tx.send(Ok(line)).is_err() {
                    // Receiver dropped — the agent is going away
                    // (timeout fired, runtime shut down). Exit
                    // cleanly so the child can be reaped.
                    return;
                }
            }
            Err(err) => {
                let _ = tx.send(Err(anyhow::Error::from(RpcFailureKind::Transport)
                    .context(format!("read line from libra-agent-{slug} stdout: {err}"))));
                return;
            }
        }
    }
}

/// Capped stderr pump: appends child stderr bytes to `buf` up to
/// [`RPC_MAX_STDERR_BYTES`], then keeps draining (so the child never
/// blocks on a full pipe) while flagging truncation. Raw bytes stay in
/// memory only; the sole read path is
/// [`RpcAgent::redacted_stderr_excerpt`].
fn pump_stderr_capped(
    mut stderr: std::process::ChildStderr,
    buf: &Mutex<Vec<u8>>,
    truncated: &std::sync::atomic::AtomicBool,
) {
    use std::io::Read;
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let Ok(mut guard) = buf.lock() else { return };
                let room = RPC_MAX_STDERR_BYTES.saturating_sub(guard.len());
                if room == 0 {
                    truncated.store(true, std::sync::atomic::Ordering::Relaxed);
                    continue; // keep draining, drop bytes
                }
                let take = room.min(n);
                guard.extend_from_slice(&chunk[..take]);
                if take < n {
                    truncated.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }
}

/// Discover every `libra-agent-*` executable on `$PATH`. The slug is
/// the substring after the prefix; duplicates from later PATH entries
/// are skipped (the first match wins, matching shell `which`
/// behaviour).
///
/// Built-in slug impersonation guard (E2 / `LBR-AGENT-006` semantics):
/// a binary whose slug collides with a built-in [`AgentKind`] CLI slug
/// (e.g. `libra-agent-claude-code`) is skipped-and-logged — it can never
/// shadow or override a built-in adapter.
///
/// Returns an empty vec when `$PATH` is unset or no binaries match.
pub fn discover_rpc_agents() -> Vec<RpcAgentBinary> {
    use std::collections::HashSet;

    let discover_span = tracing::info_span!("agent.rpc.discover");
    let _entered = discover_span.enter();

    let Some(path_var) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<RpcAgentBinary> = Vec::new();
    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else {
                continue;
            };
            let Some(slug) = name.strip_prefix(RPC_BINARY_PREFIX) else {
                continue;
            };
            if slug.is_empty() {
                continue;
            }
            if super::adapter::AgentKind::from_cli_slug(slug).is_some() {
                // Skip-and-log: never register a PATH binary that
                // impersonates a built-in agent slug.
                tracing::warn!(
                    target: "agent.rpc.discover",
                    slug,
                    external_binary = true,
                    quarantined = true,
                    reason = "builtin_slug_impersonation",
                    "skipping libra-agent binary impersonating a built-in agent slug"
                );
                continue;
            }
            // On Unix, only count executable files. Symlinks and
            // non-files are skipped.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            if seen.insert(slug.to_string()) {
                tracing::info!(
                    target: "agent.rpc.discover",
                    slug,
                    external_binary = true,
                    quarantined = true,
                    reason = "discovered_untrusted_default",
                    "discovered external libra-agent binary (quarantined until trusted)"
                );
                out.push(RpcAgentBinary {
                    slug: slug.to_string(),
                    binary_path: entry.path(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    #[serial(rpc_path_env)]
    fn discover_returns_empty_when_no_binaries_match() {
        // Point PATH at an empty tempdir — we expect no matches.
        let dir = tempfile::tempdir().unwrap();
        let original = std::env::var_os("PATH");
        // SAFETY: tests serialize this env mutation via the
        // `#[serial(rpc_path_env)]` lock so no other test under this
        // key races us; we restore PATH before returning.
        unsafe {
            std::env::set_var("PATH", dir.path());
        }
        let agents = discover_rpc_agents();
        unsafe {
            match original {
                Some(prev) => std::env::set_var("PATH", prev),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(agents.is_empty());
    }

    #[test]
    #[serial(rpc_path_env)]
    fn discover_picks_up_libra_agent_prefix() {
        let dir = tempfile::tempdir().unwrap();
        // Plant a file named `libra-agent-test-fixture` and chmod it
        // executable. Discovery should find it.
        let path = dir.path().join("libra-agent-test-fixture");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let original = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", dir.path());
        }
        let agents = discover_rpc_agents();
        unsafe {
            match original {
                Some(prev) => std::env::set_var("PATH", prev),
                None => std::env::remove_var("PATH"),
            }
        }

        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].slug, "test-fixture");
        assert_eq!(agents[0].binary_path, path);
    }

    #[test]
    #[serial(rpc_path_env)]
    fn discover_skips_files_without_executable_bit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra-agent-no-exec");
        std::fs::write(&path, b"plain file\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let original = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", dir.path());
        }
        let agents = discover_rpc_agents();
        unsafe {
            match original {
                Some(prev) => std::env::set_var("PATH", prev),
                None => std::env::remove_var("PATH"),
            }
        }

        assert!(
            agents.is_empty(),
            "non-executable file must be skipped: {agents:?}"
        );
    }

    #[test]
    #[serial(rpc_path_env)]
    fn discover_skips_files_with_empty_slug() {
        // `libra-agent-` (no slug) must NOT match.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libra-agent-");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let original = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", dir.path());
        }
        let agents = discover_rpc_agents();
        unsafe {
            match original {
                Some(prev) => std::env::set_var("PATH", prev),
                None => std::env::remove_var("PATH"),
            }
        }

        assert!(
            agents.is_empty(),
            "empty-slug binary must be skipped: {agents:?}"
        );
    }

    #[test]
    #[serial(rpc_path_env)]
    fn discover_dedups_across_path_entries() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        for dir in [dir_a.path(), dir_b.path()] {
            let path = dir.join("libra-agent-dup");
            std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let original = std::env::var_os("PATH");
        let combined = std::env::join_paths([dir_a.path(), dir_b.path()]).unwrap();
        unsafe {
            std::env::set_var("PATH", &combined);
        }
        let agents = discover_rpc_agents();
        unsafe {
            match original {
                Some(prev) => std::env::set_var("PATH", prev),
                None => std::env::remove_var("PATH"),
            }
        }

        assert_eq!(agents.len(), 1, "first match wins: {agents:?}");
        assert_eq!(agents[0].slug, "dup");
        // First match must be from the first PATH entry.
        assert!(agents[0].binary_path.starts_with(dir_a.path()));
    }

    // ── RpcAgent subprocess tests ──
    //
    // Each test plants a small `#!/bin/sh` script as the
    // `libra-agent-<slug>` binary and exercises one transport edge
    // case. Tests use `invoke_with_timeout` with a short deadline so
    // the suite stays fast even on the timeout path.

    #[cfg(unix)]
    fn plant_script(dir: &std::path::Path, slug: &str, body: &str) -> RpcAgentBinary {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("libra-agent-{slug}"));
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        RpcAgentBinary {
            slug: slug.to_string(),
            binary_path: path,
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_times_out_when_child_writes_no_response() {
        // Script reads stdin (so writeln! succeeds) but never
        // writes a response. The deadline must fire and we must
        // surface "timed out".
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "no-response",
            // /bin/sleep avoids PATH dependency when this test
            // runs concurrently with discover_* tests that set PATH.
            "#!/bin/sh\nread _line\n/bin/sleep 5\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent
            .invoke_with_timeout("capabilities", None, Duration::from_millis(500))
            .expect_err("must time out");
        assert!(
            format!("{err:#}").contains("timed out"),
            "expected timeout error, got: {err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_fails_when_child_exits_before_responding() {
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(dir.path(), "early-exit", "#!/bin/sh\nexit 0\n");
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent
            .invoke_with_timeout("capabilities", None, Duration::from_secs(2))
            .expect_err("must fail");
        let msg = format!("{err:#}");
        // Either "closed stdout before answering" (reader saw EOF
        // first) or a write error if the pipe broke mid-write.
        assert!(
            msg.contains("closed stdout before answering") || msg.contains("likely the child died"),
            "expected EOF/broken-pipe error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_fails_on_malformed_response_line() {
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "garbage",
            "#!/bin/sh\nread _line\nprintf 'not-json\\n'\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent
            .invoke_with_timeout("capabilities", None, Duration::from_secs(2))
            .expect_err("must fail");
        assert!(
            format!("{err:#}").contains("parse RPC response line"),
            "expected parse error, got: {err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_fails_on_response_with_wrong_id() {
        // Reply with id=999, but request id starts at 1.
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "wrong-id",
            "#!/bin/sh\nread _line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}\\n'\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent
            .invoke_with_timeout("capabilities", None, Duration::from_secs(2))
            .expect_err("must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("returned response for id 999") && msg.contains("waiting for id 1"),
            "expected id-mismatch error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_fails_on_unsupported_jsonrpc_version() {
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "wrong-version",
            "#!/bin/sh\nread _line\nprintf '{\"jsonrpc\":\"1.0\",\"id\":1,\"result\":{}}\\n'\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent
            .invoke_with_timeout("capabilities", None, Duration::from_secs(2))
            .expect_err("must fail");
        assert!(
            format!("{err:#}").contains("unsupported jsonrpc version"),
            "expected version error, got: {err:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_returns_result_on_well_formed_response() {
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "ok",
            "#!/bin/sh\nread _line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"protected_dirs\"]}}\\n'\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let value = agent
            .invoke_with_timeout("capabilities", None, Duration::from_secs(2))
            .expect("must succeed");
        assert_eq!(
            value
                .get("methods")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|s| s.as_str()),
            Some("protected_dirs")
        );
    }

    /// Pin the [`RPC_MAX_REQUEST_BYTES`] cap. A request with a 2 MiB
    /// `params` payload must be rejected before we touch stdin —
    /// otherwise a child that stalls on stdin would block the
    /// runtime forever.
    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_rejects_oversized_request_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        // The script doesn't matter — the cap fires before we write.
        let bin = plant_script(dir.path(), "ignored", "#!/bin/sh\nread _line\n");
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let huge_params = serde_json::json!({
            "blob": "a".repeat(2 * 1024 * 1024),
        });
        let err = agent
            .invoke_with_timeout("capabilities", Some(huge_params), Duration::from_secs(2))
            .expect_err("must reject");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("exceeds limit of") && msg.contains("bytes"),
            "expected request-cap error, got: {msg}"
        );
    }

    /// Pin the [`RPC_MAX_FRAME_BYTES`] DoS guard. The script writes
    /// `RPC_MAX_FRAME_BYTES + 1` non-newline bytes followed by `\n`,
    /// which the reader must refuse with the documented "frame larger
    /// than ... bytes" error rather than buffering it.
    ///
    /// Skipped on platforms where `head -c` or `tr` are unavailable
    /// (we rely on /usr/bin/yes being present on Unix CI runners).
    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn invoke_fails_on_oversized_frame() {
        let dir = tempfile::tempdir().unwrap();
        // Emit RPC_MAX_FRAME_BYTES+1 non-newline bytes then a newline.
        // `yes 'a' | tr -d '\n' | head -c <size>` is portable across
        // BSD/GNU coreutils.
        let body = format!(
            "#!/bin/sh\nread _line\nyes 'a' | tr -d '\\n' | head -c {}\nprintf '\\n'\n",
            RPC_MAX_FRAME_BYTES + 1
        );
        let bin = plant_script(dir.path(), "huge", &body);
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent
            .invoke_with_timeout("capabilities", None, Duration::from_secs(15))
            .expect_err("must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("frame larger than"),
            "expected frame-cap error, got: {msg}"
        );
    }

    /// Pin the strict `methods` array validation. A non-string entry
    /// must surface as a protocol error rather than being silently
    /// dropped.
    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn negotiate_capabilities_rejects_non_string_method_entry() {
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "bad-caps",
            "#!/bin/sh\nread _line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"provider_kind\",42]}}\\n'\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let err = agent.negotiate_capabilities().expect_err("must reject");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("capabilities.methods[1]") && msg.contains("not a string"),
            "expected methods-shape error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial(rpc_path_env)]
    fn negotiate_capabilities_caches_methods() {
        let dir = tempfile::tempdir().unwrap();
        let bin = plant_script(
            dir.path(),
            "caps",
            "#!/bin/sh\nread _line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"methods\":[\"provider_kind\"]}}\\n'\n",
        );
        let mut agent = RpcAgent::spawn(bin).unwrap();
        let methods = agent.negotiate_capabilities().expect("negotiate");
        assert_eq!(methods, vec!["provider_kind".to_string()]);
        // Second call to a non-advertised method must be rejected by
        // the gate without hitting the binary.
        let err = agent
            .invoke_with_timeout("read_transcript", None, Duration::from_secs(2))
            .expect_err("must reject non-advertised");
        assert!(
            format!("{err:#}").contains("does not advertise method 'read_transcript'"),
            "expected capability gate error, got: {err:#}"
        );
    }
}
