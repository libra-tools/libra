#![allow(dead_code)]
//! Data-driven matrix runner for the Code UI Remote L2 test suite.
//!
//! Each JSON case in `tests/data/code_ui_remote/*.json` is mapped to a
//! sequence of typed [`Step`]s and run against a fresh [`CodeSession`]. The
//! runner is intentionally minimal: it knows about a fixed catalogue of
//! `op` / `auth` / `token` / `assertion` strings so a stale data fixture
//! fails loud at deserialization time rather than silently changing
//! behaviour.
//!
//! Per-case JSON shape (subset; see `docs/development/commands/_general.md`):
//!
//! ```jsonc
//! {
//!   "schemaVersion": 1,
//!   "defaults": {
//!     "fixture": { "path": "tests/fixtures/code_ui/basic_chat.json" },
//!     "options": { "control": "write", "browserControl": "off", "leaseDurationMs": null }
//!   },
//!   "cases": [
//!     {
//!       "name": "...",
//!       "priority": "P0",
//!       "options": { "leaseDurationMs": 500 },
//!       "steps": [ { "op": "attach", "kind": "automation", ... } ]
//!     }
//!   ]
//! }
//! ```

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Output,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;

use super::{CodeSession, CodeSessionOptions, EventStream, SseEvent};

/// Loaded matrix file. Cases are kept as raw JSON values and only
/// deserialised into typed [`Case`]s on demand by [`find_case`].
///
/// Why lazy: each Wave (`docs/development/commands/_general.md`) lands new
/// `Step` variants alongside the runner code. If we deserialised
/// every case upfront, Wave 1's runner would refuse to load the
/// shared `sse_cases.json` file just because Wave 2's case relies
/// on a `collectEventsUntil` step the Wave 1 runner doesn't
/// implement yet. Per-case deserialisation lets each Wave run only
/// the cases it has wired up while leaving the JSON file as the
/// shared source of truth.
#[derive(Debug, Deserialize)]
pub struct CaseFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[allow(dead_code)]
    pub matrix: String,
    pub defaults: Defaults,
    pub cases: Vec<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Defaults {
    /// File-level fake-provider fixture default. Used when no
    /// per-case `provider` block overrides it. Both `fixture` and
    /// `provider` are optional so a matrix file can opt into
    /// `provider.mode == "model_from_env_file"` without supplying
    /// a fake fixture (Wave 11 model-generation case).
    #[serde(default)]
    pub fixture: FixtureRef,
    /// Wave 11 / PR 11 — alternative provider selector. When
    /// present and `mode == "model_from_env_file"`, the matrix
    /// runner reads `envFile` (path relative to the repo root,
    /// default `.env.test`) and parses `providerEnv` + `modelEnv`
    /// to fetch the live provider/model identifiers. The fake
    /// fixture is then NOT used.
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    #[serde(default)]
    pub options: CaseOptions,
}

/// Wave 11 / PR 11 — case-level provider override.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ProviderSpec {
    /// Default fake provider — uses the `fixture` field on
    /// `Case`/`Defaults`. Listed for symmetry; when omitted the
    /// matrix falls back to fake automatically.
    Fake,
    /// Read `envFile` (default `.env.test`) and resolve the live
    /// provider + model identifiers from the named env vars. The
    /// runner spawns `libra code --env-file <path> --provider
    /// <providerEnv value> --model <modelEnv value>`.
    ModelFromEnvFile {
        #[serde(default = "default_env_file_path", rename = "envFile")]
        env_file: String,
        #[serde(rename = "providerEnv")]
        provider_env: String,
        #[serde(rename = "modelEnv")]
        model_env: String,
        /// When `true` (default), the runner fails loudly if the
        /// env file is missing or one of the env vars is unset.
        /// When `false`, the runner skips the case with a clear
        /// reason. The model_generation matrix sets `required:
        /// true` so a missing `.env.test` is a hard failure under
        /// `LIBRA_RUN_LIVE=1`.
        #[serde(default = "default_provider_required")]
        required: bool,
    },
}

fn default_env_file_path() -> String {
    ".env.test".to_string()
}

fn default_provider_required() -> bool {
    true
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct FixtureRef {
    pub path: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CaseOptions {
    #[serde(default)]
    pub control: Option<String>,
    #[serde(default, rename = "browserControl")]
    pub browser_control: Option<String>,
    #[serde(default, rename = "leaseDurationMs")]
    pub lease_duration_ms: Option<u64>,
    /// Wave 5 / PR 5 — operating context (`dev` / `review` /
    /// `research`). When set, the harness spawns
    /// `libra code --context <value>`; generation cases use `"dev"`
    /// so `apply_patch` survives the intent classifier's
    /// allow-list filter.
    #[serde(default)]
    pub context: Option<String>,
    /// Wave 5 / PR 5 — `--approval-policy` override. Generation
    /// cases use `"never"` so workspace-bounded apply_patch skips
    /// the human approval gate.
    #[serde(default, rename = "approvalPolicy")]
    pub approval_policy: Option<String>,
    /// Wave 7 / PR 7 — extra environment variables to export to the
    /// spawned `libra code` process. Security cases use this to
    /// inject `LIBRA_LOG_FILE` paths that contain secret-like
    /// substrings so the diagnostics redactor can be exercised
    /// end-to-end.
    #[serde(default, rename = "extraEnv")]
    pub extra_env: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Case {
    pub name: String,
    #[allow(dead_code)]
    pub priority: String,
    /// Per-case fixture override. Wave 2's
    /// `sse_streaming_fixture_transcript_content_grows_monotonically`
    /// case in `sse_cases.json` requires the streaming fixture
    /// instead of the file's default basic-chat fixture. Codex
    /// pass-1 P3: surfacing it here lets `build_session_options`
    /// honour the override deterministically.
    #[serde(default)]
    pub fixture: Option<FixtureRef>,
    /// Wave 11 / PR 11 — per-case provider override (parallel
    /// to `Defaults::provider`). When `Some`, the case takes
    /// precedence over the file-level default.
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    #[serde(default)]
    pub options: CaseOptions,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Step {
    Attach {
        name: String,
        kind: String,
        #[serde(rename = "clientId")]
        client_id: String,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        expect: AttachExpect,
    },
    Detach {
        name: String,
        #[serde(rename = "clientId")]
        client_id: String,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        expect: SimpleExpect,
    },
    Submit {
        name: String,
        text: String,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        expect: SimpleExpect,
    },
    Sleep {
        name: String,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
    },
    WaitSnapshot {
        name: String,
        expect: AssertionsExpect,
    },
    /// Open a new SSE subscription against `/api/code/events` and
    /// label it with `stream` so later steps can wait for events on
    /// it. `timeoutMs` is the wait budget for individual reads on
    /// this stream (NOT the open call itself).
    ///
    /// `closeImmediately` lets reconnect tests open a stream just
    /// to consume the initial replay and then drop it before any
    /// later submit fires; downstream Waves (SSE reconnect case)
    /// rely on this.
    OpenEvents {
        name: String,
        stream: String,
        #[serde(default = "default_event_timeout_ms", rename = "timeoutMs")]
        timeout_ms: u64,
        #[serde(default, rename = "closeImmediately")]
        close_immediately: bool,
    },
    /// Read the very next event off `stream` and assert it has the
    /// requested `event:` field plus all expected assertions. Use
    /// this when the next event is deterministic (e.g. SSE initial
    /// replay always emits `session_updated` first).
    ExpectEvent {
        name: String,
        stream: String,
        event: String,
        #[serde(default = "default_event_timeout_ms", rename = "timeoutMs")]
        timeout_ms: u64,
        expect: AssertionsExpect,
    },
    /// Wave 4 / PR 4 — drain events off `stream` until one with
    /// the requested `event:` field arrives. Intermediate events
    /// of other types are accepted and discarded silently. Used
    /// for cases where the order or count of supporting events
    /// (e.g. status_changed bursts) is not stable but the matrix
    /// still wants to assert the eventual state-change event
    /// matches its assertions.
    CollectEventsUntil {
        name: String,
        stream: String,
        event: String,
        #[serde(default = "default_event_timeout_ms", rename = "timeoutMs")]
        timeout_ms: u64,
        expect: AssertionsExpect,
    },
    /// Wave 4 / PR 4 — drain every `session_updated` event off
    /// `stream` until either:
    ///
    ///   * the snapshot contained in the latest event has
    ///     `status == "idle"` (terminal state), OR
    ///   * `timeoutMs` elapses (whichever comes first).
    ///
    /// Run multi-event assertions on the COLLECTED sequence — e.g.
    /// `assistant_content_monotonic` walks the assistant message
    /// content across each session_updated and asserts it grows
    /// monotonically (no truncation, no shrink).
    CollectSessionUpdates {
        name: String,
        stream: String,
        #[serde(default = "default_event_timeout_ms", rename = "timeoutMs")]
        timeout_ms: u64,
        expect: AssertionsExpect,
    },
    /// Wave 4 / PR 4 — submit then poll `/session` until the
    /// session is back to `status == "idle"`. Used by the SSE
    /// reconnect case where we need to ensure the assistant's
    /// reply is fully recorded BEFORE re-opening the stream so
    /// the new initial-replay snapshot contains it.
    /// Wave 11 / PR 11 — `timeoutMs` lets live-model cases
    /// extend the wait beyond the 10 s fake-provider default
    /// (the model_generation_cases use 120 s).
    SubmitAndWaitIdle {
        name: String,
        text: String,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
        expect: SimpleExpect,
    },
    /// Wave 5 / PR 5 — submit then poll `/session` until the
    /// session reaches a terminal state — either `idle` or `error`.
    /// Generation cases use this for the fault-injection branch
    /// (invalid patch) where the runtime is allowed to surface an
    /// error in the transcript without the test mistaking it for
    /// hang. Assertions on the FINAL snapshot (status / transcript)
    /// run via the `assertions` list, not via a follow-up
    /// `WaitSnapshot` step, so the matrix file stays terse.
    SubmitAndWaitTerminal {
        name: String,
        text: String,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        expect: SimpleExpect,
    },
    /// Wave 5 / PR 5 — read a file from the spawned `libra code`
    /// working directory and run `file_contains:<needle>` /
    /// `file_contains_any:<a>|<b>` assertions over its contents.
    /// Used by the apply_patch generation cases to verify the patch
    /// landed and the produced source compiles when downstream
    /// `Step::RunRepoCommand` invokes `rustc`.
    ReadRepoFile {
        name: String,
        path: String,
        expect: AssertionsExpect,
    },
    /// Wave 5 / PR 5 — assert a path under the working directory
    /// does NOT exist. Used by the invalid-patch branch to prove
    /// the runtime did not leave a half-written file behind.
    RepoFileAbsent { name: String, path: String },
    /// Wave 5 / PR 5 — run a shell command inside the spawned
    /// working directory with a hard `timeout_ms` budget; stdout
    /// and stderr are captured so the matrix can assert on them via
    /// `stdout_or_stderr_contains:<needle>`. The expected exit code
    /// is matched against `expect.status` (default 0).
    RunRepoCommand {
        name: String,
        command: Vec<String>,
        #[serde(default = "default_run_command_timeout_ms", rename = "timeoutMs")]
        timeout_ms: u64,
        expect: RunCommandExpect,
    },
    /// Wave 6 / PR 6 — poll `/session` until at least one
    /// interaction with `id == interaction_id` is present and has
    /// `status == "pending"`. Used by the approval flow matrix to
    /// synchronise with the runtime before POSTing
    /// `/interactions/{id}` — without this, the responder can race
    /// the runtime and target an interaction that has not been
    /// upserted yet.
    WaitInteractionPending {
        name: String,
        #[serde(rename = "interactionId")]
        interaction_id: String,
        #[serde(default = "default_interaction_wait_timeout_ms", rename = "timeoutMs")]
        timeout_ms: u64,
    },
    /// Wave 6 / PR 6 — POST `/api/code/interactions/{id}`. The
    /// `approved` / `applyToFuture` / `selectedOption` /
    /// `answers` fields map directly onto the runtime's
    /// `CodeUiInteractionResponse` envelope so the JSON case can
    /// drive accept, reject, or scoped allow without growing extra
    /// vocabulary in the runner.
    RespondInteraction {
        name: String,
        #[serde(rename = "interactionId")]
        interaction_id: String,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        #[serde(default)]
        response: InteractionResponseSpec,
        expect: SimpleExpect,
    },
    /// Wave 7 / PR 7 — fire two automation `attach` requests
    /// concurrently and assert the runtime accepts exactly one and
    /// rejects the other with `CONTROLLER_CONFLICT`. State case
    /// `state_two_clients_attach_parallel_one_wins_one_conflict`
    /// owns this — without a parallel primitive a serial harness
    /// would never see the race.
    ParallelAttach {
        name: String,
        kind: String,
        #[serde(rename = "clientIds")]
        client_ids: Vec<String>,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        expect: ParallelAttachExpect,
    },
    /// Wave 7 / PR 7 — POST `/api/code/messages` with a body of
    /// `bytes` repeated `'x'`s. The runtime exposes 256 KiB as the
    /// accepted ceiling; 257 KiB and above must surface
    /// `PAYLOAD_TOO_LARGE` without hanging the connection.
    /// `timeoutMs` overrides the per-call HTTP client timeout for
    /// the 1 MiB drain case.
    SubmitBytes {
        name: String,
        bytes: usize,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        #[serde(default, rename = "timeoutMs")]
        timeout_ms: Option<u64>,
        expect: SimpleExpect,
    },
    /// Wave 7 / PR 7 — POST `/api/code/control/cancel`. Used by
    /// the idle-cancel state case which expects 409 / SESSION_BUSY
    /// per the documented contract.
    Cancel {
        name: String,
        token: TokenSource,
        #[serde(default = "default_auth")]
        auth: AuthMode,
        expect: SimpleExpect,
    },
    /// Wave 7 / PR 7 — GET `/api/code/diagnostics` and run a list
    /// of "does_not_contain:<needle>" / "contains:<needle>" /
    /// `does_not_contain_control_token` /
    /// `does_not_contain_controller_token` assertions over the raw
    /// response body so the security matrix can pin redaction
    /// behaviour without re-deserialising the JSON.
    DiagnosticsRaw {
        name: String,
        expect: AssertionsExpect,
    },
    /// Wave 7 / PR 7 — GET `/api/code/threads` with optional
    /// `limit` / `offset` query params. Both are sent verbatim
    /// (caller may pass `Some("abc")` to drive the validation-error
    /// case). Assertions cover the response body.
    GetThreads {
        name: String,
        #[serde(default)]
        limit: Option<String>,
        #[serde(default)]
        offset: Option<String>,
        expect: ThreadsExpect,
    },
    /// Wave 7 / PR 7 — read the spawned process's `libra.log` file
    /// and run contains/does-not-contain assertions over the raw
    /// text. Used by the audit-redaction case to prove a
    /// secret-like client id was scrubbed.
    LibraLogRaw {
        name: String,
        expect: AssertionsExpect,
    },
}

fn default_event_timeout_ms() -> u64 {
    5_000
}

/// 10 s default for `Step::RunRepoCommand` — matches the smoke
/// timeout used elsewhere for tool-driven workflows. JSON cases
/// override this when their command (e.g. `rustc --test`) needs a
/// larger budget.
fn default_run_command_timeout_ms() -> u64 {
    10_000
}

/// Wave 6 default for `Step::WaitInteractionPending` — the runtime
/// upserts the interaction synchronously after the tool-call event
/// fires, so 5 s is a generous ceiling that still keeps a
/// runaway-test failure tight.
fn default_interaction_wait_timeout_ms() -> u64 {
    5_000
}

fn default_auth() -> AuthMode {
    AuthMode::None
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    None,
    /// Send the live process's `X-Libra-Control-Token` header.
    ValidControl,
    /// Omit `X-Libra-Control-Token` even on routes that require it.
    MissingControl,
    /// Send a clearly-bogus `X-Libra-Control-Token`.
    InvalidControl,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenSource {
    /// No `X-Code-Controller-Token` header.
    None,
    /// The most recently saved token under the `current` slot.
    Current,
    /// A token previously saved into the `stale` slot.
    Stale,
    /// A clearly-bogus controller token (4242…42).
    Forged,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachExpect {
    pub status: u16,
    #[serde(default, rename = "errorCode")]
    pub error_code: Option<String>,
    #[serde(default, rename = "saveToken")]
    pub save_token: Option<TokenSlot>,
    #[serde(default, rename = "saveLeaseExpiresAt")]
    pub save_lease_expires_at: Option<String>,
    #[serde(default)]
    pub assertions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimpleExpect {
    pub status: u16,
    #[serde(default, rename = "errorCode")]
    pub error_code: Option<String>,
    #[serde(default)]
    pub assertions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssertionsExpect {
    #[serde(default)]
    pub assertions: Vec<String>,
}

/// Expectation envelope for `Step::RunRepoCommand`. `status`
/// defaults to 0 when absent so the common "must succeed" case
/// stays terse in JSON. Set it to `null` only when the command is
/// allowed to exit non-zero and only the captured output matters.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RunCommandExpect {
    #[serde(default = "default_run_command_status")]
    pub status: Option<i32>,
    #[serde(default)]
    pub assertions: Vec<String>,
}

fn default_run_command_status() -> Option<i32> {
    Some(0)
}

/// Wave 6 — JSON-driven `CodeUiInteractionResponse` envelope used
/// by `Step::RespondInteraction`. The runtime accepts the same
/// `{ approved, applyToFuture, selectedOption, note, answers }`
/// schema; mapping straight to that shape lets reject / accept /
/// scoped-allow cases be expressed in pure data without runner
/// vocabulary growth.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InteractionResponseSpec {
    #[serde(default)]
    pub approved: Option<bool>,
    #[serde(default, rename = "applyToFuture")]
    pub apply_to_future: Option<String>,
    #[serde(default, rename = "selectedOption")]
    pub selected_option: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub answers: HashMap<String, Vec<String>>,
}

/// Wave 7 — expectation envelope for `Step::ParallelAttach`. The
/// runtime non-deterministically picks a winner so we list both
/// expected statuses + error codes "in any order"; the runner
/// matches the multiset of (status, error_code) pairs it
/// actually observed.
#[derive(Debug, Clone, Deserialize)]
pub struct ParallelAttachExpect {
    #[serde(rename = "statusesAnyOrder")]
    pub statuses_any_order: Vec<u16>,
    #[serde(default, rename = "errorCodes")]
    pub error_codes: Vec<Option<String>>,
}

/// Wave 7 — expectation envelope for `Step::GetThreads`. The
/// success case asserts the body is an array clamped to a
/// per-page maximum; the failure case asserts the JSON error
/// envelope shape (status / errorCode / message). Both share
/// `assertions` for the shared needle vocabulary.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ThreadsExpect {
    pub status: u16,
    #[serde(default, rename = "errorCode")]
    pub error_code: Option<String>,
    #[serde(default)]
    pub assertions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TokenSlot {
    Current,
    Stale,
}

/// Shared state across the steps of a single case.
struct CaseRuntime<'a> {
    session: &'a mut CodeSession,
    case_name: &'a str,
    /// Saved `controllerToken` values, keyed by slot.
    tokens: HashMap<TokenSlot, String>,
    /// Saved `leaseExpiresAt` values, keyed by user-supplied label.
    lease_timestamps: HashMap<String, chrono::DateTime<chrono::Utc>>,
    /// Open SSE subscriptions, keyed by the user-supplied `stream`
    /// label. Streams persist across steps so a single case can
    /// `openEvents` early and `expectEvent` later.
    streams: HashMap<String, EventStream>,
}

const FORGED_CONTROLLER_TOKEN: &str = "42424242-4242-4242-4242-424242424242";

/// Resolve a case-file path under `CARGO_MANIFEST_DIR`.
pub fn data_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Repo-root-relative fixture path → absolute path.
pub fn repo_root_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Load and parse the matrix file at `path`.
pub fn load_case_file(path: &Path) -> Result<CaseFile> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read matrix data file '{}'", path.display()))?;
    let parsed: CaseFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse matrix data file '{}'", path.display()))?;
    if parsed.schema_version != 1 {
        bail!(
            "unsupported matrix schemaVersion {} in '{}'",
            parsed.schema_version,
            path.display(),
        );
    }
    Ok(parsed)
}

/// Locate a case by name in the loaded file and deserialise it
/// into a typed [`Case`]. The returned value is owned because
/// each case in the file is stored as raw JSON until requested
/// (see the doc-comment on [`CaseFile`] for why).
pub fn find_case(file: &CaseFile, name: &str) -> Result<Case> {
    let raw = file
        .cases
        .iter()
        .find(|c| {
            c.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n == name)
        })
        .ok_or_else(|| anyhow!("matrix case '{name}' not present in case file"))?;
    serde_json::from_value::<Case>(raw.clone())
        .with_context(|| format!("failed to deserialise matrix case '{name}'"))
}

/// Merge per-case options on top of the file-level defaults.
fn effective_options(defaults: &CaseOptions, case: &CaseOptions) -> CaseOptions {
    let mut extra_env = defaults.extra_env.clone();
    for (k, v) in &case.extra_env {
        extra_env.insert(k.clone(), v.clone());
    }
    CaseOptions {
        control: case.control.clone().or_else(|| defaults.control.clone()),
        browser_control: case
            .browser_control
            .clone()
            .or_else(|| defaults.browser_control.clone()),
        lease_duration_ms: case.lease_duration_ms.or(defaults.lease_duration_ms),
        context: case.context.clone().or_else(|| defaults.context.clone()),
        approval_policy: case
            .approval_policy
            .clone()
            .or_else(|| defaults.approval_policy.clone()),
        extra_env,
    }
}

/// Build a [`CodeSessionOptions`] from the matrix config, including
/// per-case overrides for `control`, `leaseDurationMs`, and
/// `fixture`. Codex pass-1 P3: case-level `fixture` overrides the
/// file's default, matching the JSON schema documented in
/// `docs/development/commands/_general.md`.
///
/// Wave 11 / PR 11 — when the case (or file defaults) carries a
/// `provider` block in `model_from_env_file` mode AND the live
/// env file is present + readable, the returned options point at
/// the live provider/model
/// (`CodeSessionOptions::with_live_provider` + `with_env_file`).
/// Otherwise the fake-fixture path is used, matching every prior
/// wave's behaviour.
pub fn build_session_options(file: &CaseFile, case: &Case) -> CodeSessionOptions {
    let merged = effective_options(&file.defaults.options, &case.options);
    let fixture_path = case
        .fixture
        .as_ref()
        .unwrap_or(&file.defaults.fixture)
        .path
        .clone();
    let fixture = repo_root_path(&fixture_path);
    let mut options = CodeSessionOptions::new(case.name.clone(), fixture);
    // Wave 11 — apply live-provider override if the case/defaults
    // request one. Errors here panic with the case name so the
    // failing case is obvious in cargo output; the runner is
    // expected to gate on `LIBRA_RUN_LIVE=1` BEFORE calling
    // `build_session_options`, so a panic at this point indicates
    // a misconfigured live invocation rather than incidental
    // fake-fixture flakiness.
    if let Some(provider) = case.provider.as_ref().or(file.defaults.provider.as_ref()) {
        match provider {
            ProviderSpec::Fake => {} // current default; nothing to do.
            ProviderSpec::ModelFromEnvFile {
                env_file,
                provider_env,
                model_env,
                required,
            } => {
                let env_path = repo_root_path(env_file);
                let env_text = match std::fs::read_to_string(&env_path) {
                    Ok(text) => text,
                    Err(error) => {
                        if *required {
                            panic!(
                                "case '{}' requires env file '{}', but reading it failed: {error}",
                                case.name,
                                env_path.display(),
                            );
                        }
                        return options;
                    }
                };
                let provider_value = lookup_env_value(&env_text, provider_env);
                let model_value = lookup_env_value(&env_text, model_env);
                let (provider_value, model_value) = match (provider_value, model_value) {
                    (Some(p), Some(m)) => (p, m),
                    _ => {
                        if *required {
                            panic!(
                                "case '{}' requires env file '{}' to define both '{provider_env}' and '{model_env}'",
                                case.name,
                                env_path.display(),
                            );
                        }
                        return options;
                    }
                };
                let provider_lower = provider_value.to_ascii_lowercase();
                options = options
                    .with_live_provider(provider_value.clone(), model_value)
                    .with_env_file(env_path);
                // W5-07: the legacy-TUI rollback env is gone, so live cases
                // ride the default Web launch; W3-13 parity makes `--env-file`
                // accepted there for non-Codex providers (managed Codex keeps
                // its own credential surface and rejects `--env-file`).
                // Wave 11 Codex pass-2 fix: §5.19 specifies the
                // DeepSeek live invocation runs with thinking
                // mode + high reasoning effort
                // (`docs/development/commands/_general.md:581`). Without
                // those flags the nightly matrix would exercise
                // the wrong code path. Apply them automatically
                // when the env-resolved provider is deepseek so
                // the JSON case file doesn't have to re-state
                // them, and so other providers don't accidentally
                // get DeepSeek-specific flags.
                if provider_lower == "deepseek" {
                    options = options
                        .push_extra_cli_arg("--deepseek-thinking")
                        .push_extra_cli_arg("enabled")
                        .push_extra_cli_arg("--deepseek-reasoning-effort")
                        .push_extra_cli_arg("high");
                }
            }
        }
    }
    if let Some(control) = merged.control.as_deref() {
        match control {
            "write" => {} // default
            "observe" => {
                options = options.with_control_observe();
            }
            other => panic!(
                "matrix case '{}' uses unsupported control mode '{other}'",
                case.name,
            ),
        }
    }
    if matches!(merged.browser_control.as_deref(), Some("loopback")) {
        options = options.with_browser_control_loopback();
    }
    if let Some(ms) = merged.lease_duration_ms {
        options = options.with_lease_duration_ms(ms);
    }
    if let Some(context) = merged.context.as_deref() {
        options = options.with_context(context);
    }
    if let Some(policy) = merged.approval_policy.as_deref() {
        options = options.with_approval_policy(policy);
    }
    // Wave 7: extra_env iteration order is HashMap-arbitrary, but
    // each `(key, value)` is independent so the runtime sees the
    // same final environment regardless of insertion order.
    for (key, value) in &merged.extra_env {
        options = options.with_extra_env(key, value);
    }
    options
}

/// Run an entire case top-to-bottom against a fresh session. Caller is
/// responsible for spawning + shutting down the session so the lifetime is
/// visible in `cargo test` output (each case becomes its own `#[test]`).
pub fn run_case(session: &mut CodeSession, case: &Case) -> Result<()> {
    let mut runtime = CaseRuntime {
        session,
        case_name: &case.name,
        tokens: HashMap::new(),
        lease_timestamps: HashMap::new(),
        streams: HashMap::new(),
    };
    for (idx, step) in case.steps.iter().enumerate() {
        let step_name = step_name(step);
        runtime
            .run_step(step)
            .with_context(|| format!("case '{}' step #{idx} ({step_name}) failed", case.name))?;
    }
    Ok(())
}

fn step_name(step: &Step) -> &str {
    match step {
        Step::Attach { name, .. } => name,
        Step::Detach { name, .. } => name,
        Step::Submit { name, .. } => name,
        Step::Sleep { name, .. } => name,
        Step::WaitSnapshot { name, .. } => name,
        Step::OpenEvents { name, .. } => name,
        Step::ExpectEvent { name, .. } => name,
        Step::CollectEventsUntil { name, .. } => name,
        Step::CollectSessionUpdates { name, .. } => name,
        Step::SubmitAndWaitIdle { name, .. } => name,
        Step::SubmitAndWaitTerminal { name, .. } => name,
        Step::ReadRepoFile { name, .. } => name,
        Step::RepoFileAbsent { name, .. } => name,
        Step::RunRepoCommand { name, .. } => name,
        Step::WaitInteractionPending { name, .. } => name,
        Step::RespondInteraction { name, .. } => name,
        Step::ParallelAttach { name, .. } => name,
        Step::SubmitBytes { name, .. } => name,
        Step::Cancel { name, .. } => name,
        Step::DiagnosticsRaw { name, .. } => name,
        Step::GetThreads { name, .. } => name,
        Step::LibraLogRaw { name, .. } => name,
    }
}

impl CaseRuntime<'_> {
    fn run_step(&mut self, step: &Step) -> Result<()> {
        match step {
            Step::Attach {
                kind,
                client_id,
                auth,
                expect,
                ..
            } => self.run_attach(kind, client_id, auth, expect),
            Step::Detach {
                client_id,
                token,
                auth,
                expect,
                ..
            } => self.run_detach(client_id, token, auth, expect),
            Step::Submit {
                text,
                token,
                auth,
                expect,
                ..
            } => self.run_submit(text, token, auth, expect),
            Step::Sleep { duration_ms, .. } => {
                std::thread::sleep(Duration::from_millis(*duration_ms));
                Ok(())
            }
            Step::WaitSnapshot { expect, .. } => self.run_wait_snapshot(expect),
            Step::OpenEvents {
                stream,
                close_immediately,
                ..
            } => self.run_open_events(stream, *close_immediately),
            Step::ExpectEvent {
                stream,
                event,
                timeout_ms,
                expect,
                ..
            } => self.run_expect_event(stream, event, *timeout_ms, expect),
            Step::CollectEventsUntil {
                stream,
                event,
                timeout_ms,
                expect,
                ..
            } => self.run_collect_events_until(stream, event, *timeout_ms, expect),
            Step::CollectSessionUpdates {
                stream,
                timeout_ms,
                expect,
                ..
            } => self.run_collect_session_updates(stream, *timeout_ms, expect),
            Step::SubmitAndWaitIdle {
                text,
                token,
                auth,
                timeout_ms,
                expect,
                ..
            } => self.run_submit_and_wait_idle(text, token, auth, *timeout_ms, expect),
            Step::SubmitAndWaitTerminal {
                text,
                token,
                auth,
                expect,
                ..
            } => self.run_submit_and_wait_terminal(text, token, auth, expect),
            Step::ReadRepoFile { path, expect, .. } => self.run_read_repo_file(path, expect),
            Step::RepoFileAbsent { path, .. } => self.run_repo_file_absent(path),
            Step::RunRepoCommand {
                command,
                timeout_ms,
                expect,
                ..
            } => self.run_run_repo_command(command, *timeout_ms, expect),
            Step::WaitInteractionPending {
                interaction_id,
                timeout_ms,
                ..
            } => self.run_wait_interaction_pending(interaction_id, *timeout_ms),
            Step::RespondInteraction {
                interaction_id,
                token,
                auth,
                response,
                expect,
                ..
            } => self.run_respond_interaction(interaction_id, token, auth, response, expect),
            Step::ParallelAttach {
                kind,
                client_ids,
                auth,
                expect,
                ..
            } => self.run_parallel_attach(kind, client_ids, auth, expect),
            Step::SubmitBytes {
                bytes,
                token,
                auth,
                timeout_ms,
                expect,
                ..
            } => self.run_submit_bytes(*bytes, token, auth, *timeout_ms, expect),
            Step::Cancel {
                token,
                auth,
                expect,
                ..
            } => self.run_cancel(token, auth, expect),
            Step::DiagnosticsRaw { expect, .. } => self.run_diagnostics_raw(expect),
            Step::GetThreads {
                limit,
                offset,
                expect,
                ..
            } => self.run_get_threads(limit.as_deref(), offset.as_deref(), expect),
            Step::LibraLogRaw { expect, .. } => self.run_libra_log_raw(expect),
        }
    }

    fn run_open_events(&mut self, stream: &str, close_immediately: bool) -> Result<()> {
        // Open the SSE subscription. The downstream Wave 2 case
        // `sse_reconnect_initial_replay_contains_latest_transcript`
        // depends on `closeImmediately` to consume the initial
        // replay then drop the stream before any later submit.
        let mut event_stream = self
            .session
            .open_event_stream()
            .with_context(|| format!("failed to open SSE stream '{stream}'"))?;
        if close_immediately {
            event_stream.close();
            return Ok(());
        }
        if self
            .streams
            .insert(stream.to_string(), event_stream)
            .is_some()
        {
            bail!(
                "case '{}' opened SSE stream label '{stream}' twice",
                self.case_name
            );
        }
        Ok(())
    }

    fn run_expect_event(
        &mut self,
        stream: &str,
        event: &str,
        timeout_ms: u64,
        expect: &AssertionsExpect,
    ) -> Result<()> {
        let event_stream = self.streams.get_mut(stream).ok_or_else(|| {
            anyhow!(
                "case '{}' references SSE stream '{stream}' before openEvents",
                self.case_name
            )
        })?;
        let timeout = Duration::from_millis(timeout_ms);
        let received = event_stream
            .next_event(timeout)?
            .ok_or_else(|| anyhow!("timed out waiting for SSE event '{event}' on '{stream}'"))?;
        if received.event != event {
            bail!(
                "expected SSE event '{event}' on '{stream}', got '{}': {}",
                received.event,
                received.data
            );
        }
        let payload = parse_event_data(&received).with_context(|| {
            format!(
                "case '{}' SSE event '{}' had invalid JSON payload",
                self.case_name, received.event
            )
        })?;
        for assertion in &expect.assertions {
            evaluate_event_assertion(assertion, &received, &payload).with_context(|| {
                format!(
                    "case '{}' SSE assertion '{assertion}' failed; payload: {payload}",
                    self.case_name
                )
            })?;
        }
        Ok(())
    }

    fn run_collect_events_until(
        &mut self,
        stream: &str,
        target_event: &str,
        timeout_ms: u64,
        expect: &AssertionsExpect,
    ) -> Result<()> {
        let event_stream = self.streams.get_mut(stream).ok_or_else(|| {
            anyhow!(
                "case '{}' references SSE stream '{stream}' before openEvents",
                self.case_name
            )
        })?;
        // Per-event budget within an overall deadline so a stream
        // that drips one stale event per second can't quietly
        // consume the entire test budget without ever reaching
        // the target.
        //
        // Wave 4 fix: the initial-replay `session_updated` carries
        // the snapshot at SUBSCRIPTION time (typically idle, empty
        // transcript). For an assertion like
        // `event_transcript_contains:<reply>` the first matching
        // event won't satisfy it — the assistant hasn't streamed
        // yet. Treat assertion failure as "this isn't the event we
        // want, keep waiting" and only surface the LAST error on
        // timeout. The rule guarantees we don't silently lose a
        // genuinely failing assertion: if the deadline elapses,
        // the caller still sees the diagnostic from the most
        // recent matching event.
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut last_error: Option<anyhow::Error> = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if let Some(error) = last_error {
                    return Err(error.context(format!(
                        "case '{}' timed out after {timeout_ms}ms waiting for matching '{target_event}' on '{stream}'",
                        self.case_name,
                    )));
                }
                bail!(
                    "case '{}' timed out after {timeout_ms}ms waiting for SSE event '{target_event}' on '{stream}'",
                    self.case_name,
                );
            }
            let event = match event_stream.next_event(remaining)? {
                Some(event) => event,
                None => continue,
            };
            if event.event != target_event {
                continue;
            }
            let payload = parse_event_data(&event).with_context(|| {
                format!(
                    "case '{}' SSE event '{}' had invalid JSON payload",
                    self.case_name, event.event
                )
            })?;
            let mut all_ok = true;
            for assertion in &expect.assertions {
                if let Err(error) = evaluate_event_assertion(assertion, &event, &payload) {
                    last_error = Some(error.context(format!(
                        "case '{}' SSE assertion '{assertion}' failed on payload: {payload}",
                        self.case_name
                    )));
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                return Ok(());
            }
        }
    }

    fn run_collect_session_updates(
        &mut self,
        stream: &str,
        timeout_ms: u64,
        expect: &AssertionsExpect,
    ) -> Result<()> {
        let event_stream = self.streams.get_mut(stream).ok_or_else(|| {
            anyhow!(
                "case '{}' references SSE stream '{stream}' before openEvents",
                self.case_name
            )
        })?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        // Wave 4 fix: the initial-replay `session_updated` is
        // always emitted with the snapshot at subscription time —
        // for a fresh session that's `status=idle, transcript=[]`.
        // Treating that initial idle as terminal would exit before
        // any streaming chunks arrive.
        //
        // Codex pass-1 P2 fix: the runtime emits `status_changed`
        // for status flips (see code_ui.rs `set_status`), NOT
        // `session_updated`. So the terminal "idle" signal we wait
        // for is a `status_changed` event whose snapshot has
        // `status == idle`, observed AFTER we've seen at least
        // one non-idle status_changed (which marks the start of
        // the turn). This avoids relying on timeout to terminate
        // the collector and unblocks fast/no-op runtimes too.
        let mut collected: Vec<Value> = Vec::new();
        let mut saw_non_idle = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let event = match event_stream.next_event(remaining)? {
                Some(event) => event,
                None => break,
            };
            if event.event != "session_updated" && event.event != "status_changed" {
                continue;
            }
            let payload = parse_event_data(&event).with_context(|| {
                format!(
                    "case '{}' SSE {} had invalid JSON payload",
                    self.case_name, event.event,
                )
            })?;
            let is_idle = payload
                .pointer("/data/status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "idle");
            // Track the turn lifecycle from BOTH event streams.
            // status_changed: thinking flips the gate; the
            // matching status_changed: idle (which fires after
            // every transcript mutation has already produced its
            // session_updated) closes the collection.
            if !is_idle {
                saw_non_idle = true;
            }
            // Only collect session_updated payloads — that's the
            // shape the multi-event assertions look at. The
            // status_changed events are observed purely for the
            // terminal-idle signal and dropped from the buffer.
            if event.event == "session_updated" {
                collected.push(payload);
            } else if is_idle && saw_non_idle {
                break;
            }
        }
        if collected.is_empty() {
            bail!(
                "case '{}' collected zero session_updated events on '{stream}' within {timeout_ms}ms",
                self.case_name,
            );
        }
        for assertion in &expect.assertions {
            evaluate_collected_assertion(assertion, &collected).with_context(|| {
                format!(
                    "case '{}' SSE multi-event assertion '{assertion}' failed across {} events",
                    self.case_name,
                    collected.len()
                )
            })?;
        }
        Ok(())
    }

    fn run_submit_and_wait_idle(
        &mut self,
        text: &str,
        token: &TokenSource,
        auth: &AuthMode,
        timeout_ms: Option<u64>,
        expect: &SimpleExpect,
    ) -> Result<()> {
        // Codex pass-1 P2 — capture the pre-submit transcript
        // length so the wait predicate AND the post-wait assertion
        // evaluator both ignore entries from prior turns. Without
        // this baseline a previously-completed assistant_message
        // would satisfy the predicate immediately, and any
        // `transcript_contains:<needle>` assertion could match a
        // stale entry instead of the response under test.
        let baseline_len = current_transcript_len(&self.session.snapshot()?);
        // Reuse the standard submit path so the response status /
        // error code semantics stay identical to `run_submit`.
        self.run_submit(text, token, auth, expect)?;
        // Then poll /session until the runtime drains the turn.
        // 10 s ceiling matches the lease/submit smoke timeout used
        // elsewhere in the harness.
        //
        // Wave 4 race fix (Codex pass-1 follow-up): a naive
        // "status == idle" wait can fire on the PRE-submit snapshot
        // — POST /messages returns before the runtime begins
        // processing, so the very next /session call may still
        // observe the initial idle state. To pin "the assistant
        // reply has actually landed", require BOTH:
        //   * status == idle (turn drained), AND
        //   * a NEW transcript entry (appended after `baseline_len`)
        //     is a completed assistant_message with non-empty
        //     content (the streaming completion marker the runtime
        //     sets when it flushes the final delta).
        //
        // Wave 5 fix: the assistant_message is no longer guaranteed
        // to be the LAST entry — apply_patch tool calls land
        // afterwards in their own transcript entry — so iterate the
        // post-baseline tail instead of inspecting only the
        // absolute last entry.
        let wait_budget = timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(10));
        let final_snapshot = self.session.wait_for_snapshot(wait_budget, |snapshot| {
            let status_idle = snapshot
                .pointer("/status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "idle");
            let assistant_complete = new_transcript_entries(snapshot, baseline_len).any(|entry| {
                let kind = entry.pointer("/kind").and_then(Value::as_str).unwrap_or("");
                let content = entry
                    .pointer("/content")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let entry_status = entry
                    .pointer("/status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                kind == "assistant_message" && !content.is_empty() && entry_status == "completed"
            });
            status_idle && assistant_complete
        })?;
        for assertion in &expect.assertions {
            evaluate_post_submit_assertion(assertion, &final_snapshot, baseline_len)
                .with_context(|| {
                    format!(
                        "case '{}' SubmitAndWaitIdle assertion '{assertion}' failed; snapshot:\n{final_snapshot:#}",
                        self.case_name,
                    )
                })?;
        }
        Ok(())
    }

    fn run_submit_and_wait_terminal(
        &mut self,
        text: &str,
        token: &TokenSource,
        auth: &AuthMode,
        expect: &SimpleExpect,
    ) -> Result<()> {
        // Codex pass-1 P2 — same baseline trick as
        // `run_submit_and_wait_idle`: capture the transcript length
        // BEFORE submit so the predicate and assertion evaluator
        // both restrict their view to entries appended for THIS
        // turn. Otherwise a stale completed entry from a prior
        // submit could satisfy the wait, and a stale failure
        // marker could match `transcript_contains_any:` even when
        // the new turn is still streaming.
        let baseline_len = current_transcript_len(&self.session.snapshot()?);
        // POST /messages first; reuse the standard submit path so
        // status / error code semantics match `run_submit`.
        self.run_submit(text, token, auth, expect)?;
        // Then poll /session until either:
        //   * status == "error" (fault branch — invalid patch
        //     bubbled up to a session-level error), OR
        //   * status == "idle" AND a NEW transcript entry (after
        //     `baseline_len`) is either a completed
        //     assistant_message or a tool_call with completed /
        //     failed status (apply_patch failure lands as a
        //     tool_call entry with status="failed"; the fixture's
        //     follow-up text lands an assistant_message before).
        let final_snapshot =
            self.session
                .wait_for_snapshot(Duration::from_secs(15), |snapshot| {
                    let status = snapshot
                        .pointer("/status")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if status == "error" {
                        return true;
                    }
                    if status != "idle" {
                        return false;
                    }
                    new_transcript_entries(snapshot, baseline_len).any(|entry| {
                        let kind = entry.pointer("/kind").and_then(Value::as_str).unwrap_or("");
                        let entry_status = entry
                            .pointer("/status")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        (kind == "assistant_message" && entry_status == "completed")
                            || (kind == "tool_call"
                                && (entry_status == "completed" || entry_status == "failed"))
                            || kind == "tool_result"
                            || kind == "system_message"
                    })
                })?;
        for assertion in &expect.assertions {
            evaluate_post_submit_assertion(assertion, &final_snapshot, baseline_len)
                .with_context(|| {
                    format!(
                        "case '{}' SubmitAndWaitTerminal assertion '{assertion}' failed; snapshot:\n{final_snapshot:#}",
                        self.case_name,
                    )
                })?;
        }
        Ok(())
    }

    fn run_read_repo_file(&self, path: &str, expect: &AssertionsExpect) -> Result<()> {
        let contents = self.session.read_repo_file(path)?.ok_or_else(|| {
            anyhow!(
                "case '{}' expected repo file '{path}' to exist; not found under {}",
                self.case_name,
                self.session.repo_dir().display(),
            )
        })?;
        for assertion in &expect.assertions {
            evaluate_file_assertion(assertion, &contents).with_context(|| {
                format!(
                    "case '{}' file assertion '{assertion}' failed for '{path}'",
                    self.case_name
                )
            })?;
        }
        Ok(())
    }

    fn run_repo_file_absent(&self, path: &str) -> Result<()> {
        if let Some(contents) = self.session.read_repo_file(path)? {
            bail!(
                "case '{}' expected repo file '{path}' to be absent, but it exists ({} bytes)",
                self.case_name,
                contents.len(),
            );
        }
        Ok(())
    }

    fn run_run_repo_command(
        &self,
        command: &[String],
        timeout_ms: u64,
        expect: &RunCommandExpect,
    ) -> Result<()> {
        let output = self
            .session
            .run_repo_command(command, Duration::from_millis(timeout_ms))?;
        if let Some(expected_status) = expect.status
            && output.status.code() != Some(expected_status)
        {
            bail!(
                "case '{}' command {:?} exited with {:?} (expected {expected_status})\nstdout:\n{}\nstderr:\n{}",
                self.case_name,
                command,
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        for assertion in &expect.assertions {
            evaluate_command_output_assertion(assertion, &output).with_context(|| {
                format!(
                    "case '{}' command output assertion '{assertion}' failed for {:?}",
                    self.case_name, command
                )
            })?;
        }
        Ok(())
    }

    fn run_wait_interaction_pending(&self, interaction_id: &str, timeout_ms: u64) -> Result<()> {
        // Poll /session until at least one interaction with the
        // requested id is observed in `pending` state. Failures
        // beyond the deadline include the last snapshot's
        // `interactions` array so the diagnostic identifies whether
        // the runtime never created the interaction or created it
        // with a different id.
        let final_snapshot =
            self.session
                .wait_for_snapshot(Duration::from_millis(timeout_ms), |snapshot| {
                    snapshot
                        .pointer("/interactions")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter().any(|i| {
                                i.pointer("/id").and_then(Value::as_str) == Some(interaction_id)
                                    && i.pointer("/status").and_then(Value::as_str)
                                        == Some("pending")
                            })
                        })
                        .unwrap_or(false)
                });
        if let Err(err) = final_snapshot {
            return Err(err.context(format!(
                "case '{}' timed out after {timeout_ms}ms waiting for pending interaction '{interaction_id}'",
                self.case_name,
            )));
        }
        Ok(())
    }

    fn run_respond_interaction(
        &self,
        interaction_id: &str,
        token: &TokenSource,
        auth: &AuthMode,
        response: &InteractionResponseSpec,
        expect: &SimpleExpect,
    ) -> Result<()> {
        // Build a minimal `CodeUiInteractionResponse` JSON envelope
        // — empty fields are dropped so the runtime sees the same
        // shape an automation client would actually send.
        let mut body = serde_json::Map::new();
        if let Some(approved) = response.approved {
            body.insert("approved".to_string(), Value::Bool(approved));
        }
        if let Some(scope) = response.apply_to_future.as_deref() {
            body.insert(
                "applyToFuture".to_string(),
                Value::String(scope.to_string()),
            );
        }
        if let Some(option) = response.selected_option.as_deref() {
            body.insert(
                "selectedOption".to_string(),
                Value::String(option.to_string()),
            );
        }
        if let Some(note) = response.note.as_deref() {
            body.insert("note".to_string(), Value::String(note.to_string()));
        }
        if !response.answers.is_empty() {
            body.insert(
                "answers".to_string(),
                serde_json::to_value(&response.answers).unwrap_or(Value::Null),
            );
        }
        let (status, payload) = self.session.matrix_respond_interaction(
            interaction_id,
            &Value::Object(body),
            token,
            auth,
            &self.tokens,
        )?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &payload)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&payload, code)?;
        }
        Ok(())
    }

    fn run_parallel_attach(
        &mut self,
        kind: &str,
        client_ids: &[String],
        auth: &AuthMode,
        expect: &ParallelAttachExpect,
    ) -> Result<()> {
        if client_ids.len() != expect.statuses_any_order.len() {
            bail!(
                "case '{}' parallelAttach: clientIds.len={} but statusesAnyOrder.len={}",
                self.case_name,
                client_ids.len(),
                expect.statuses_any_order.len(),
            );
        }
        // Spawn one OS thread per client_id so the runtime sees
        // genuinely racing connections. Keep the worker inputs independent
        // from the mutable session lifecycle, so we don't share
        // `&self.session` across threads. Each thread
        // builds its own short-lived `reqwest::blocking::Client`
        // and gets the small set of values it needs by value
        // (URL + control token); the actual race surface is the
        // axum router, not the harness.
        let url = self.session.matrix_attach_url();
        let control_token = self.session.control_token_value().to_string();
        let kind = kind.to_string();
        let results: Vec<Result<(StatusCode, Value)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = client_ids
                .iter()
                .map(|client_id| {
                    let url = url.clone();
                    let control_token = control_token.clone();
                    let kind = kind.clone();
                    let client_id = client_id.clone();
                    let auth = auth.clone();
                    scope.spawn(move || {
                        parallel_attach_request(&url, &control_token, &kind, &client_id, &auth)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("attach thread panicked"))
                .collect()
        });
        let mut observed: Vec<(StatusCode, Option<String>)> = Vec::new();
        for result in results {
            let (status, body) = result?;
            let code = body
                .pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| body.get("code").and_then(Value::as_str))
                .map(str::to_string);
            observed.push((status, code));
        }
        // Match observed (status, error_code) pairs against the
        // expected multiset. Order-independence matters because the
        // OS scheduler decides which thread reaches the runtime
        // first.
        let mut remaining_expected: Vec<(StatusCode, Option<String>)> = expect
            .statuses_any_order
            .iter()
            .zip(
                expect
                    .error_codes
                    .iter()
                    .cloned()
                    .chain(std::iter::repeat(None)),
            )
            .map(|(status, code)| {
                StatusCode::from_u16(*status)
                    .map(|s| (s, code.clone()))
                    .map_err(|e| anyhow!("invalid expected status {status}: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;
        for (status, code) in &observed {
            if let Some(idx) = remaining_expected
                .iter()
                .position(|(es, ec)| es == status && ec == code)
            {
                remaining_expected.remove(idx);
            } else {
                bail!(
                    "case '{}' parallelAttach observed unexpected (status={status}, errorCode={:?}); observed={:?}, expected={:?}",
                    self.case_name,
                    code,
                    observed,
                    expect
                        .statuses_any_order
                        .iter()
                        .zip(expect.error_codes.iter())
                        .collect::<Vec<_>>(),
                );
            }
        }
        if !remaining_expected.is_empty() {
            bail!(
                "case '{}' parallelAttach missing expected outcomes: {remaining_expected:?}; observed={observed:?}",
                self.case_name,
            );
        }
        Ok(())
    }

    fn run_submit_bytes(
        &mut self,
        bytes: usize,
        token: &TokenSource,
        auth: &AuthMode,
        timeout_ms: Option<u64>,
        expect: &SimpleExpect,
    ) -> Result<()> {
        let timeout = timeout_ms.map(Duration::from_millis);
        let (status, body) =
            self.session
                .matrix_submit_bytes(bytes, token, auth, &self.tokens, timeout)?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &body)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&body, code)?;
        }
        Ok(())
    }

    fn run_cancel(
        &mut self,
        token: &TokenSource,
        auth: &AuthMode,
        expect: &SimpleExpect,
    ) -> Result<()> {
        let (status, body) = self.session.matrix_cancel(token, auth, &self.tokens)?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &body)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&body, code)?;
        }
        Ok(())
    }

    fn run_diagnostics_raw(&self, expect: &AssertionsExpect) -> Result<()> {
        let body = self.session.diagnostics_raw_text()?;
        for assertion in &expect.assertions {
            evaluate_raw_text_assertion(assertion, &body, self.session).with_context(|| {
                format!(
                    "case '{}' diagnostics raw assertion '{assertion}' failed",
                    self.case_name
                )
            })?;
        }
        Ok(())
    }

    fn run_get_threads(
        &self,
        limit: Option<&str>,
        offset: Option<&str>,
        expect: &ThreadsExpect,
    ) -> Result<()> {
        let (status, body) = self.session.matrix_get_threads(limit, offset)?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &body)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&body, code)?;
        }
        for assertion in &expect.assertions {
            evaluate_threads_assertion(assertion, &body).with_context(|| {
                format!(
                    "case '{}' threads assertion '{assertion}' failed; body:\n{body:#}",
                    self.case_name
                )
            })?;
        }
        Ok(())
    }

    fn run_libra_log_raw(&self, expect: &AssertionsExpect) -> Result<()> {
        let text = self.session.libra_log_text()?;
        for assertion in &expect.assertions {
            evaluate_raw_text_assertion(assertion, &text, self.session).with_context(|| {
                format!(
                    "case '{}' libra log assertion '{assertion}' failed",
                    self.case_name
                )
            })?;
        }
        Ok(())
    }

    fn run_attach(
        &mut self,
        kind: &str,
        client_id: &str,
        auth: &AuthMode,
        expect: &AttachExpect,
    ) -> Result<()> {
        let (status, body) = self.session.matrix_attach(kind, client_id, auth)?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &body)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&body, code)?;
        }
        if let Some(slot) = expect.save_token {
            let token = body
                .get("controllerToken")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("attach response did not include controllerToken: {body}"))?
                .to_string();
            // Codex pass-1 P1 fix: also stash the token on the
            // session so security cases (e.g. the
            // `does_not_contain_controller_token` assertion in
            // `evaluate_raw_text_assertion`) can compare against a
            // concrete value instead of `None`. Without this the
            // diagnostics-leak check would pass vacuously even if
            // the token actually appeared in the response.
            self.session.set_controller_token(token.clone());
            self.tokens.insert(slot, token);
        }
        if let Some(label) = expect.save_lease_expires_at.as_deref() {
            let ts = body
                .get("leaseExpiresAt")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("attach response did not include leaseExpiresAt: {body}"))?;
            let parsed = chrono::DateTime::parse_from_rfc3339(ts)
                .map_err(|err| anyhow!("invalid leaseExpiresAt '{ts}': {err}"))?
                .with_timezone(&chrono::Utc);
            self.lease_timestamps.insert(label.to_string(), parsed);
        }
        for assertion in &expect.assertions {
            self.evaluate_attach_assertion(assertion, &body)?;
        }
        Ok(())
    }

    fn run_detach(
        &mut self,
        client_id: &str,
        token: &TokenSource,
        auth: &AuthMode,
        expect: &SimpleExpect,
    ) -> Result<()> {
        let (status, body) = self
            .session
            .matrix_detach(client_id, token, auth, &self.tokens)?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &body)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&body, code)?;
        }
        Ok(())
    }

    fn run_submit(
        &mut self,
        text: &str,
        token: &TokenSource,
        auth: &AuthMode,
        expect: &SimpleExpect,
    ) -> Result<()> {
        let (status, body) = self
            .session
            .matrix_submit(text, token, auth, &self.tokens)?;
        let expected_status = StatusCode::from_u16(expect.status).with_context(|| {
            format!(
                "invalid expected status {} in case '{}'",
                expect.status, self.case_name
            )
        })?;
        ensure_status(status, expected_status, &body)?;
        if let Some(code) = expect.error_code.as_deref() {
            ensure_error_code(&body, code)?;
        }
        Ok(())
    }

    fn run_wait_snapshot(&mut self, expect: &AssertionsExpect) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_snapshot: Value;
        loop {
            let snapshot = self.session.snapshot()?;
            last_snapshot = snapshot.clone();
            if expect.assertions.iter().all(|a| {
                evaluate_snapshot_assertion(a, &snapshot, &self.lease_timestamps).unwrap_or(false)
            }) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let mut errs = Vec::new();
                for assertion in &expect.assertions {
                    if let Err(err) = evaluate_snapshot_assertion(
                        assertion,
                        &last_snapshot,
                        &self.lease_timestamps,
                    )
                    .and_then(|ok| {
                        if ok {
                            Ok(())
                        } else {
                            Err(anyhow!("assertion '{assertion}' did not hold"))
                        }
                    }) {
                        errs.push(format!("- {err}"));
                    }
                }
                bail!(
                    "waitSnapshot timed out for case '{}'\nfailing assertions:\n{}\nlast snapshot:\n{last_snapshot:#}",
                    self.case_name,
                    errs.join("\n"),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn evaluate_attach_assertion(&self, assertion: &str, body: &Value) -> Result<()> {
        match assertion {
            "controller_token_non_empty" => {
                let token = body
                    .get("controllerToken")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if token.is_empty() {
                    bail!("attach response controllerToken was empty: {body}");
                }
            }
            "lease_expires_at_future" => {
                let ts = body
                    .get("leaseExpiresAt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("attach response missing leaseExpiresAt: {body}"))?;
                let parsed = chrono::DateTime::parse_from_rfc3339(ts)
                    .map_err(|err| anyhow!("invalid leaseExpiresAt '{ts}': {err}"))?;
                if parsed.with_timezone(&chrono::Utc) <= chrono::Utc::now() {
                    bail!("leaseExpiresAt {ts} is not in the future");
                }
            }
            "controller_kind_automation" => {
                let kind = body
                    .pointer("/controller/kind")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if kind != "automation" {
                    bail!("expected controller.kind == 'automation', got '{kind}' (body: {body})");
                }
            }
            other if other.starts_with("lease_expires_after:") => {
                let label = other.trim_start_matches("lease_expires_after:");
                let baseline = self
                    .lease_timestamps
                    .get(label)
                    .ok_or_else(|| anyhow!("no saved leaseExpiresAt under label '{label}'"))?;
                let ts = body
                    .get("leaseExpiresAt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("attach response missing leaseExpiresAt: {body}"))?;
                let parsed = chrono::DateTime::parse_from_rfc3339(ts)
                    .map_err(|err| anyhow!("invalid leaseExpiresAt '{ts}': {err}"))?
                    .with_timezone(&chrono::Utc);
                if parsed <= *baseline {
                    bail!("renew leaseExpiresAt {parsed} did not extend past baseline {baseline}",);
                }
            }
            // Wave 7 — state-serial case asserts the post-detach
            // re-attach issues a NEW controllerToken so a stale
            // token from the prior session can't drive the new
            // lease. Compares the freshly returned `controllerToken`
            // against whatever was previously saved into the
            // `Stale` slot.
            "controller_token_differs_from_stale" => {
                let stale = self.tokens.get(&TokenSlot::Stale).ok_or_else(|| {
                    anyhow!("no saved controllerToken under Stale slot for differs assertion")
                })?;
                let current = body
                    .get("controllerToken")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("attach response missing controllerToken: {body}"))?;
                if current == stale {
                    bail!(
                        "expected new controllerToken to differ from stale, but both are '{stale}'",
                    );
                }
            }
            other => bail!("unsupported attach assertion '{other}'"),
        }
        Ok(())
    }
}

fn evaluate_snapshot_assertion(
    assertion: &str,
    snapshot: &Value,
    _lease_timestamps: &HashMap<String, chrono::DateTime<chrono::Utc>>,
) -> Result<bool> {
    match assertion {
        "controller_kind_none" => {
            // W5-02: the LocalTui initial controller no longer exists — a
            // released session settles to `none`, never `tui`.
            let kind = snapshot
                .pointer("/controller/kind")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(kind == "none")
        }
        "controller_can_write_false" => {
            let can_write = snapshot
                .pointer("/controller/canWrite")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(!can_write)
        }
        "controller_kind_automation" => {
            let kind = snapshot
                .pointer("/controller/kind")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(kind == "automation")
        }
        // Wave 6 / Wave 7 — session status assertions. Both the
        // bare `status_*` and the `snapshot_status_*` aliases are
        // accepted so a JSON case can use either naming under
        // `Step::WaitSnapshot` without runner vocabulary drift.
        "status_idle" | "snapshot_status_idle" => Ok(snapshot_status(snapshot) == "idle"),
        "status_thinking" | "snapshot_status_thinking" => {
            Ok(snapshot_status(snapshot) == "thinking")
        }
        "status_executing_tool" | "snapshot_status_executing_tool" => {
            Ok(snapshot_status(snapshot) == "executing_tool")
        }
        "status_awaiting_interaction" | "snapshot_status_awaiting_interaction" => {
            Ok(snapshot_status(snapshot) == "awaiting_interaction")
        }
        "status_error" | "snapshot_status_error" => Ok(snapshot_status(snapshot) == "error"),
        "snapshot_status_error_or_idle" => {
            let s = snapshot_status(snapshot);
            Ok(s == "error" || s == "idle")
        }
        // Wave 6 — interaction-shape assertions used after
        // RespondInteraction resolves the awaiting state.
        // `interaction_pending:<id>` matches a PENDING interaction;
        // `interaction_resolved:<id>` matches one in RESOLVED.
        other if other.starts_with("interaction_pending:") => {
            let id = other.trim_start_matches("interaction_pending:");
            Ok(snapshot_interaction_has_status(snapshot, id, "pending"))
        }
        other if other.starts_with("interaction_resolved:") => {
            let id = other.trim_start_matches("interaction_resolved:");
            Ok(snapshot_interaction_has_status(snapshot, id, "resolved"))
        }
        // Wave 6 — tool-call status pinning. Approve-path cases
        // assert `tool_call_completed:<id>`; reject-path cases
        // assert `tool_call_failed:<id>` where <id> matches the
        // fixture's tool_call.id.
        other if other.starts_with("tool_call_completed:") => {
            let id = other.trim_start_matches("tool_call_completed:");
            Ok(snapshot_tool_call_has_status(snapshot, id, "completed"))
        }
        other if other.starts_with("tool_call_failed:") => {
            let id = other.trim_start_matches("tool_call_failed:");
            Ok(snapshot_tool_call_has_status(snapshot, id, "failed"))
        }
        // Wave 6 — coarse needle scan over the FULL transcript.
        // Submit-anchored cases should prefer
        // `evaluate_post_submit_assertion`'s baselined variant; this
        // form is fine for WaitSnapshot when the case only does one
        // submit so a stale entry can't pollute the match.
        other if other.starts_with("transcript_contains:") => {
            let needle = other.trim_start_matches("transcript_contains:");
            let transcript = snapshot
                .pointer("/transcript")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("snapshot missing /transcript array"))?;
            let haystack = serde_json::to_string(transcript).unwrap_or_default();
            Ok(haystack.contains(needle))
        }
        other if other.starts_with("transcript_contains_any:") => {
            let raw = other.trim_start_matches("transcript_contains_any:");
            let needles: Vec<&str> = raw.split('|').filter(|s| !s.is_empty()).collect();
            if needles.is_empty() {
                bail!("'transcript_contains_any:' assertion needs at least one needle");
            }
            let transcript = snapshot
                .pointer("/transcript")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("snapshot missing /transcript array"))?;
            let haystack = serde_json::to_string(transcript).unwrap_or_default();
            Ok(needles.iter().any(|needle| haystack.contains(needle)))
        }
        other => bail!("unsupported snapshot assertion '{other}'"),
    }
}

fn snapshot_status(snapshot: &Value) -> &str {
    snapshot
        .pointer("/status")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn snapshot_interaction_has_status(snapshot: &Value, id: &str, status: &str) -> bool {
    snapshot
        .pointer("/interactions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter().any(|i| {
                i.pointer("/id").and_then(Value::as_str) == Some(id)
                    && i.pointer("/status").and_then(Value::as_str) == Some(status)
            })
        })
        .unwrap_or(false)
}

fn snapshot_tool_call_has_status(snapshot: &Value, id: &str, status: &str) -> bool {
    snapshot
        .pointer("/toolCalls")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter().any(|c| {
                c.pointer("/id").and_then(Value::as_str) == Some(id)
                    && c.pointer("/status").and_then(Value::as_str) == Some(status)
            })
        })
        .unwrap_or(false)
}

/// Parse an SSE event's `data:` field as JSON. The `/api/code/events`
/// handler always serialises a `CodeUiEventEnvelope` via
/// `Event::json_data`, so a deserialisation failure here means the
/// runtime emitted a malformed envelope — surface as an error rather
/// than silently empty.
fn parse_event_data(event: &SseEvent) -> Result<Value> {
    serde_json::from_str(&event.data)
        .with_context(|| format!("failed to parse SSE data as JSON: {}", event.data))
}

fn evaluate_event_assertion(assertion: &str, event: &SseEvent, payload: &Value) -> Result<()> {
    match assertion {
        "event_data_has_transcript_array" => {
            // Initial replay must include the snapshot's transcript
            // array so a fresh subscriber renders the room state.
            let transcript = payload
                .pointer("/data/transcript")
                .and_then(Value::as_array);
            if transcript.is_none() {
                bail!("payload missing /data/transcript array");
            }
        }
        "event_data_has_controller" => {
            let controller = payload.pointer("/data/controller");
            if !controller.is_some_and(Value::is_object) {
                bail!("payload missing /data/controller object");
            }
        }
        "event_data_status_thinking" => {
            let status = payload
                .pointer("/data/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "thinking" {
                bail!("expected /data/status == 'thinking', got '{status}'");
            }
        }
        "event_data_status_idle" => {
            let status = payload
                .pointer("/data/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "idle" {
                bail!("expected /data/status == 'idle', got '{status}'");
            }
        }
        "event_data_controller_kind_automation" => {
            let kind = payload
                .pointer("/data/controller/kind")
                .and_then(Value::as_str)
                .unwrap_or("");
            if kind != "automation" {
                bail!("expected /data/controller.kind == 'automation', got '{kind}'");
            }
        }
        "event_data_controller_kind_none" => {
            // W5-02: see `controller_kind_none`.
            let kind = payload
                .pointer("/data/controller/kind")
                .and_then(Value::as_str)
                .unwrap_or("");
            if kind != "none" {
                bail!("expected /data/controller.kind 'none', got '{kind}'");
            }
        }
        other if other.starts_with("event_transcript_contains:") => {
            let needle = other.trim_start_matches("event_transcript_contains:");
            let transcript = payload
                .pointer("/data/transcript")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("payload missing /data/transcript array"))?;
            let haystack = serde_json::to_string(transcript).unwrap_or_default();
            if !haystack.contains(needle) {
                bail!("transcript did not contain '{needle}'; serialised transcript:\n{haystack}");
            }
        }
        other => bail!(
            "unsupported SSE event assertion '{other}' (event '{}')",
            event.event
        ),
    }
    Ok(())
}

/// Evaluate an assertion across a sequence of collected SSE
/// payloads. Used by `Step::CollectSessionUpdates`. Each payload
/// is the full envelope (`{ seq, type, at, data: snapshot }`).
///
/// New cross-event assertions live here; per-event assertions
/// stay on `evaluate_event_assertion`. Suffix assertions of the
/// form `event_transcript_contains:NEEDLE` are also accepted —
/// they must hold against the FINAL collected payload.
fn evaluate_collected_assertion(assertion: &str, collected: &[Value]) -> Result<()> {
    match assertion {
        "assistant_content_monotonic" => {
            // Walk every collected snapshot, extract the LAST
            // assistant_message entry's `content`, and assert each
            // observation is a prefix of the next (or equal). Any
            // shrink or non-prefix change is a regression in the
            // streaming pipeline.
            let mut prev: Option<String> = None;
            for (idx, payload) in collected.iter().enumerate() {
                let transcript = payload
                    .pointer("/data/transcript")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("payload #{idx} missing /data/transcript array"))?;
                let assistant_content = transcript.iter().rev().find_map(|entry| {
                    let kind = entry.pointer("/kind").and_then(Value::as_str).unwrap_or("");
                    if kind == "assistant_message" {
                        entry
                            .pointer("/content")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    } else {
                        None
                    }
                });
                let Some(current) = assistant_content else {
                    // Snapshots before the first assistant chunk
                    // legitimately have no assistant message yet.
                    // Skip but keep `prev` so the next observation
                    // still has to extend the previous run.
                    continue;
                };
                if let Some(prev_content) = &prev
                    && !current.starts_with(prev_content)
                {
                    bail!(
                        "assistant content #{idx} is not a prefix-extension of the previous; \
                         prev: {prev_content:?}, current: {current:?}",
                    );
                }
                prev = Some(current);
            }
            if prev.is_none() {
                bail!("collected sessions had no assistant_message entries");
            }
        }
        other if other.starts_with("event_transcript_contains:") => {
            // Apply the per-event assertion to the FINAL collected
            // payload — by then the streamed reply has fully
            // landed in the snapshot.
            let last = collected.last().ok_or_else(|| {
                anyhow!("event_transcript_contains assertion needs at least one collected event")
            })?;
            let needle = other.trim_start_matches("event_transcript_contains:");
            let transcript = last
                .pointer("/data/transcript")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("payload missing /data/transcript array"))?;
            let haystack = serde_json::to_string(transcript).unwrap_or_default();
            if !haystack.contains(needle) {
                bail!("transcript did not contain '{needle}'; serialised transcript:\n{haystack}");
            }
        }
        other => bail!("unsupported collected-events assertion '{other}'"),
    }
    Ok(())
}

fn ensure_status(actual: StatusCode, expected: StatusCode, body: &Value) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        bail!("expected HTTP {expected}, got {actual}: {body}")
    }
}

fn ensure_error_code(body: &Value, expected: &str) -> Result<()> {
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .or_else(|| body.get("code").and_then(Value::as_str))
        .unwrap_or("");
    if code == expected {
        Ok(())
    } else {
        bail!("expected error.code == '{expected}', got '{code}' (body: {body})")
    }
}

/// Helper exposed to tests: forged controller token literal.
pub fn forged_controller_token() -> &'static str {
    FORGED_CONTROLLER_TOKEN
}

/// Wave 11 / PR 11 — `.env`-style parser sized for the
/// model_generation matrix lookup. Mirrors the production
/// parser in `src/command/code.rs::parse_env_file_value` /
/// `parse_double_quoted_env_value` /
/// `strip_inline_env_comment` so a value the runtime would
/// accept also passes through this lookup.
///
/// Coverage:
///   * Comments — leading `#`, AND inline `#` after whitespace
///     on unquoted values.
///   * `export KEY=VALUE` prefix.
///   * Double-quoted values with `\n` / `\r` / `\t` / `\\` /
///     `\"` escape sequences.
///   * Single-quoted values are taken verbatim (no escape
///     processing) per POSIX shell convention.
///   * Empty values reject as `None` so the model_generation
///     "required" check fires correctly when an env var is
///     present but blank.
fn lookup_env_value(env_text: &str, key: &str) -> Option<String> {
    for raw_line in env_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            continue;
        }
        let value = v.trim();
        if value.is_empty() {
            return None;
        }
        let parsed = match value.as_bytes()[0] {
            b'"' => {
                if value.as_bytes().last() != Some(&b'"') || value.len() < 2 {
                    // Malformed quoted value; treat as missing
                    // (the caller's `required: true` check will
                    // then surface the misconfiguration cleanly).
                    return None;
                }
                parse_double_quoted_env_value_inline(&value[1..value.len() - 1])?
            }
            b'\'' => {
                if value.as_bytes().last() != Some(&b'\'') || value.len() < 2 {
                    return None;
                }
                value[1..value.len() - 1].to_string()
            }
            _ => strip_inline_env_comment_inline(value)
                .trim_end()
                .to_string(),
        };
        if parsed.is_empty() {
            return None;
        }
        return Some(parsed);
    }
    None
}

fn parse_double_quoted_env_value_inline(value: &str) -> Option<String> {
    let mut parsed = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            parsed.push(ch);
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            'n' => parsed.push('\n'),
            'r' => parsed.push('\r'),
            't' => parsed.push('\t'),
            '\\' => parsed.push('\\'),
            '"' => parsed.push('"'),
            other => parsed.push(other),
        }
    }
    Some(parsed)
}

fn strip_inline_env_comment_inline(value: &str) -> &str {
    for (idx, ch) in value.char_indices() {
        if ch == '#' && (idx == 0 || value[..idx].ends_with(char::is_whitespace)) {
            return &value[..idx];
        }
    }
    value
}

/// Wave 7 — per-thread parallel attach helper. Builds a fresh
/// `reqwest::blocking::Client` so each thread holds its own
/// connection, which lets two attaches genuinely race the
/// runtime instead of being serialised by a shared client.
fn parallel_attach_request(
    url: &str,
    control_token: &str,
    kind: &str,
    client_id: &str,
    auth: &AuthMode,
) -> Result<(StatusCode, Value)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("failed to build per-thread parallel-attach client")?;
    let mut request = client
        .post(url)
        .json(&serde_json::json!({ "clientId": client_id, "kind": kind }));
    request = match auth {
        AuthMode::ValidControl => request.header("X-Libra-Control-Token", control_token),
        AuthMode::InvalidControl => request.header("X-Libra-Control-Token", "00000000-deadbeef"),
        AuthMode::MissingControl | AuthMode::None => request,
    };
    let response = request
        .send()
        .context("failed to send parallel attach request")?;
    let status = response.status();
    let body = response
        .json()
        .unwrap_or_else(|_| Value::Object(Default::default()));
    Ok((status, body))
}

/// Length of the snapshot's transcript array, or `0` if the
/// snapshot has no transcript field. Used by SubmitAndWait* steps
/// to pin a per-turn baseline so subsequent assertions can ignore
/// stale entries appended by earlier submits in the same case.
fn current_transcript_len(snapshot: &Value) -> usize {
    snapshot
        .pointer("/transcript")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0)
}

/// Iterator over transcript entries appended after `baseline_len`.
/// Returns an empty iterator when the snapshot has no transcript or
/// when the runtime trimmed the array below the baseline (defensive
/// — not expected in production cases).
fn new_transcript_entries(snapshot: &Value, baseline_len: usize) -> impl Iterator<Item = &Value> {
    snapshot
        .pointer("/transcript")
        .and_then(Value::as_array)
        .map(|entries| {
            if baseline_len >= entries.len() {
                [].iter()
            } else {
                entries[baseline_len..].iter()
            }
        })
        .into_iter()
        .flatten()
}

/// Evaluate an assertion against the snapshot returned after a
/// `Step::SubmitAndWaitIdle` / `Step::SubmitAndWaitTerminal`.
/// Snapshot-wide assertions (e.g. `snapshot_status_idle`) inspect
/// the whole snapshot; transcript needle assertions
/// (`transcript_contains:`, `transcript_contains_any:`) restrict
/// their search to entries appended after `baseline_len` so a
/// stale entry from a prior submit can't satisfy a needle intended
/// for the current turn — Codex pass-1 P2 follow-up.
fn evaluate_post_submit_assertion(
    assertion: &str,
    snapshot: &Value,
    baseline_len: usize,
) -> Result<()> {
    match assertion {
        "snapshot_status_idle" => {
            let status = snapshot
                .pointer("/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "idle" {
                bail!("expected /status == 'idle', got '{status}'");
            }
            Ok(())
        }
        "snapshot_status_error" => {
            let status = snapshot
                .pointer("/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "error" {
                bail!("expected /status == 'error', got '{status}'");
            }
            Ok(())
        }
        "snapshot_status_error_or_idle" => {
            let status = snapshot
                .pointer("/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "error" && status != "idle" {
                bail!("expected /status in {{error, idle}}, got '{status}'");
            }
            Ok(())
        }
        other if other.starts_with("transcript_contains:") => {
            let needle = other.trim_start_matches("transcript_contains:");
            let new_entries: Vec<&Value> = new_transcript_entries(snapshot, baseline_len).collect();
            if new_entries.is_empty() {
                bail!(
                    "expected at least one transcript entry appended after baseline_len={baseline_len}, but the runtime appended none",
                );
            }
            let haystack = serde_json::to_string(&new_entries).unwrap_or_default();
            if !haystack.contains(needle) {
                bail!("new transcript entries did not contain '{needle}'; entries:\n{haystack}",);
            }
            Ok(())
        }
        other if other.starts_with("transcript_contains_any:") => {
            let raw = other.trim_start_matches("transcript_contains_any:");
            let needles: Vec<&str> = raw.split('|').filter(|s| !s.is_empty()).collect();
            if needles.is_empty() {
                bail!("'transcript_contains_any:' assertion needs at least one needle");
            }
            let new_entries: Vec<&Value> = new_transcript_entries(snapshot, baseline_len).collect();
            if new_entries.is_empty() {
                bail!(
                    "expected at least one transcript entry appended after baseline_len={baseline_len}, but the runtime appended none",
                );
            }
            let haystack = serde_json::to_string(&new_entries).unwrap_or_default();
            if !needles.iter().any(|needle| haystack.contains(needle)) {
                bail!("new transcript entries matched none of {needles:?}; entries:\n{haystack}",);
            }
            Ok(())
        }
        other => bail!("unsupported post-submit assertion '{other}'"),
    }
}

/// Evaluate an assertion against the contents of a file read with
/// `Step::ReadRepoFile`. Generation cases use this to pin the exact
/// strings the apply_patch fixture was supposed to produce.
fn evaluate_file_assertion(assertion: &str, contents: &str) -> Result<()> {
    if let Some(needle) = assertion.strip_prefix("file_contains:") {
        if !contents.contains(needle) {
            bail!("file did not contain '{needle}'; full contents:\n{contents}");
        }
        return Ok(());
    }
    if let Some(raw) = assertion.strip_prefix("file_contains_any:") {
        let needles: Vec<&str> = raw.split('|').filter(|s| !s.is_empty()).collect();
        if needles.is_empty() {
            bail!("'file_contains_any:' assertion needs at least one needle");
        }
        if !needles.iter().any(|needle| contents.contains(needle)) {
            bail!("file matched none of {needles:?}; full contents:\n{contents}");
        }
        return Ok(());
    }
    bail!("unsupported file assertion '{assertion}'")
}

/// Evaluate an assertion against the captured stdout / stderr of a
/// `Step::RunRepoCommand`. The combined-stream form lets tests stay
/// agnostic to which channel `rustc` / `cargo` use for "test result:
/// ok"-style summaries (rustc emits to stdout; some wrappers send
/// it to stderr).
/// Wave 7 — generic raw-text assertion vocabulary used by
/// `Step::DiagnosticsRaw` and `Step::LibraLogRaw`. The session
/// reference is needed so `does_not_contain_control_token` /
/// `does_not_contain_controller_token` can substitute the
/// harness-issued tokens at evaluation time without baking them
/// into the JSON case file.
fn evaluate_raw_text_assertion(assertion: &str, text: &str, session: &CodeSession) -> Result<()> {
    if let Some(needle) = assertion.strip_prefix("contains:") {
        if !text.contains(needle) {
            bail!("expected text to contain '{needle}'; full text:\n{text}");
        }
        return Ok(());
    }
    if let Some(needle) = assertion.strip_prefix("does_not_contain:") {
        if text.contains(needle) {
            bail!("expected text to NOT contain '{needle}'; full text:\n{text}");
        }
        return Ok(());
    }
    if assertion == "does_not_contain_control_token" {
        let token = session.control_token_value();
        if !token.is_empty() && text.contains(token) {
            bail!("text leaks the harness control token");
        }
        return Ok(());
    }
    if assertion == "does_not_contain_controller_token" {
        if let Some(token) = session.current_controller_token()
            && !token.is_empty()
            && text.contains(token)
        {
            bail!("text leaks the issued controller token");
        }
        return Ok(());
    }
    bail!("unsupported raw-text assertion '{assertion}'")
}

/// Wave 7 — `/threads` body assertions. The success branch returns
/// `{ "items": [...] }` (or a bare array); accept both shapes via
/// `array_root_or_items`. `items_len_lte:<n>` clamps the per-page
/// page-size requirement; `error_message_contains:<needle>` pins
/// the human-readable error message field.
fn evaluate_threads_assertion(assertion: &str, body: &Value) -> Result<()> {
    if let Some(raw) = assertion.strip_prefix("items_len_lte:") {
        let max: usize = raw.trim().parse().with_context(|| {
            format!("'items_len_lte:' assertion expects a non-negative integer, got '{raw}'",)
        })?;
        let items = array_root_or_items(body)
            .ok_or_else(|| anyhow!("threads body is not an array nor has /items array: {body}"))?;
        if items.len() > max {
            bail!(
                "threads page len={} exceeds clamp {max}; body:\n{body:#}",
                items.len()
            );
        }
        return Ok(());
    }
    if let Some(needle) = assertion.strip_prefix("error_message_contains:") {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| body.get("message").and_then(Value::as_str))
            .unwrap_or("");
        if !message.contains(needle) {
            bail!("threads error message did not contain '{needle}'; body:\n{body:#}");
        }
        return Ok(());
    }
    bail!("unsupported threads assertion '{assertion}'")
}

/// Helper: return the items array regardless of whether the
/// runtime serialises threads as a bare `[...]` or wraps it in
/// `{ "items": [...] }`. The `/threads` route currently returns
/// the wrapped shape but the runner stays forwards-compatible.
fn array_root_or_items(body: &Value) -> Option<&Vec<Value>> {
    if let Some(arr) = body.as_array() {
        return Some(arr);
    }
    body.get("items").and_then(Value::as_array)
}

fn evaluate_command_output_assertion(assertion: &str, output: &Output) -> Result<()> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(needle) = assertion.strip_prefix("stdout_contains:") {
        if !stdout.contains(needle) {
            bail!("stdout did not contain '{needle}'\nstdout:\n{stdout}\nstderr:\n{stderr}",);
        }
        return Ok(());
    }
    if let Some(needle) = assertion.strip_prefix("stderr_contains:") {
        if !stderr.contains(needle) {
            bail!("stderr did not contain '{needle}'\nstdout:\n{stdout}\nstderr:\n{stderr}",);
        }
        return Ok(());
    }
    if let Some(needle) = assertion.strip_prefix("stdout_or_stderr_contains:") {
        if !stdout.contains(needle) && !stderr.contains(needle) {
            bail!(
                "neither stdout nor stderr contained '{needle}'\nstdout:\n{stdout}\nstderr:\n{stderr}",
            );
        }
        return Ok(());
    }
    bail!("unsupported command output assertion '{assertion}'")
}
