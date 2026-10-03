//! Validated ingress for external-agent capture.
//!
//! This module owns the untrusted hook-frame boundary: size limiting, UTF-8
//! and JSON parsing, canonical envelope validation, provider recognition, and
//! lowering into a [`LifecycleEvent`]. It deliberately has no database, ref,
//! filesystem-store, or tracing persistence dependency.

#[cfg(not(unix))]
use std::io::Read;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::process::Stdio;
use std::{
    future::Future,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use ring::{digest, hmac};
use thiserror::Error;
use uuid::Uuid;

pub(crate) use crate::internal::ai::capture::runtime_scope::CaptureRuntimeScope;
use crate::internal::ai::{
    authorized_read::{
        AUTHORIZED_READ_HELPER_ARG, AUTHORIZED_READ_HELPER_CAP_ENV, CancellationSafeChild,
        configure_private_helper_process_group, helper_program, read_async_strictly_bounded,
    },
    hooks::{
        lifecycle::{
            LifecycleEvent, LifecycleEventKind, LifecycleIdentityScheme, SessionHookEnvelope,
            validate_session_hook_envelope,
        },
        provider::{HookProvider, ProviderHookCommand},
    },
};

/// Maximum bytes accepted from a hook frame before parsing.
pub const MAX_STDIN_BYTES: usize = 1_048_576;
/// Maximum length accepted for a provider transcript path in an envelope.
pub const MAX_TRANSCRIPT_PATH_BYTES: usize = 4096;
/// Maximum length accepted for a provider-reported working directory.
///
/// The command preserves this bounded scope fact alongside the envelope for a
/// later scope resolver, so accepting an unbounded path would duplicate an
/// attacker-controlled allocation across the ingress boundary.
pub const MAX_REPORTED_CWD_BYTES: usize = 4096;
/// Fixed size of the digest sent to the bounded scope-binding helper.  The
/// helper never receives the provider/session/native-id components that were
/// used to derive this preimage.
pub(crate) const CAPTURE_DEDUP_PREIMAGE_BYTES: usize = 32;

// Production never re-anchors a host deadline. This narrow test seam waits
// until a fixture has actually forked its pipe-holding descendant, then gives
// that established process group a short deadline. It prevents the reaping
// regression from depending on scheduler timing during helper startup.
/// One host-owned capture budget represented in both clock domains.
///
/// The monotonic deadline bounds in-process work. The wall-clock value is
/// captured at command dispatch and persisted in terminal finalizer receipts,
/// so a delayed ingress/runtime handoff cannot silently re-anchor a host's
/// original timeout window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureDeadline {
    monotonic: Instant,
    absolute_millis: i64,
}

impl CaptureDeadline {
    /// Establish the deadline before any hook-specific configuration or
    /// storage work begins.
    pub(crate) fn from_budget_millis(budget_millis: u64) -> Result<Self> {
        Self::from_budget_millis_with_clocks(budget_millis, SystemTime::now, Instant::now)
    }

    /// Build the paired deadline from the two command-dispatch clocks.
    ///
    /// Keeping the sampling order in this small helper makes the conservative
    /// wall-clock-before-monotonic invariant directly testable without a
    /// scheduler-dependent sleep test.
    fn from_budget_millis_with_clocks(
        budget_millis: u64,
        wall_clock: impl FnOnce() -> SystemTime,
        monotonic_clock: impl FnOnce() -> Instant,
    ) -> Result<Self> {
        let budget_millis_i64 = i64::try_from(budget_millis)
            .context("capture deadline exceeds the persistent finalizer range")?;
        // Sample the persisted clock first. If this task is descheduled
        // between the two samples, taking `Instant` first would make the
        // SQLite authorization deadline later than the host's monotonic
        // budget. The reverse ordering makes the durable gate conservative.
        let now_millis = wall_clock()
            .duration_since(UNIX_EPOCH)
            .context("system clock precedes the Unix epoch while establishing capture deadline")?
            .as_millis();
        let now_millis = i64::try_from(now_millis)
            .context("system clock exceeds the persistent finalizer range")?;
        let absolute_millis = now_millis
            .checked_add(budget_millis_i64)
            .context("capture deadline exceeds the persistent finalizer range")?;
        let monotonic = monotonic_clock()
            .checked_add(Duration::from_millis(budget_millis))
            .context("capture deadline exceeds this platform's monotonic clock range")?;
        Ok(Self {
            monotonic,
            absolute_millis,
        })
    }

    /// Construct a deadline from already-established clock values. Production
    /// command dispatch must use [`Self::from_budget_millis`]; this narrow
    /// constructor exists for in-process adapter tests that need an expired
    /// host deadline without waiting in real time.
    #[cfg(test)]
    pub(crate) fn from_parts(monotonic: Instant, absolute_millis: i64) -> Self {
        Self {
            monotonic,
            absolute_millis,
        }
    }

    pub(crate) fn monotonic(self) -> Instant {
        self.monotonic
    }

    pub(crate) fn absolute_millis(self) -> i64 {
        self.absolute_millis
    }

    /// Fail before crossing a boundary that may create repository state.
    /// Terminal events intentionally take their dedicated recovery path after
    /// worktree binding so they can leave a content-free pending receipt.
    pub(crate) fn ensure_before_mutation(self, stage: &str) -> Result<()> {
        if Instant::now() >= self.monotonic {
            bail!("capture ingress deadline expired before {stage}");
        }
        Ok(())
    }
}

/// Stable classification for malformed hook-envelope input.
///
/// The command layer maps this error to `LBR-AGENT-008`; downstream capture
/// failures must not use it, so an infrastructure fault can never masquerade
/// as an invalid caller payload.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct HookEnvelopeInvalid(pub String);

/// Provider-supplied scope facts that are bounded enough to pass from ingress
/// to the coordinator. They are not ownership proof: the runtime binds this
/// value to the actual worktree before any catalog or transcript operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureScopeInput {
    /// The working directory declared by the validated hook envelope.
    pub reported_cwd: String,
    /// A fixed SHA-256 commitment to the always-present canonical lifecycle
    /// identity. The scope helper HMACs this to make the action/event UUID
    /// opaque even when no provider-native replay field exists.
    pub(crate) event_identity_preimage: [u8; CAPTURE_DEDUP_PREIMAGE_BYTES],
    /// A fixed SHA-256 commitment to the allowed native replay components.
    /// This is deliberately the only replay-related provider material that
    /// may cross the ingress-to-runtime helper boundary.
    pub(crate) dedup_preimage: Option<[u8; CAPTURE_DEDUP_PREIMAGE_BYTES]>,
}

/// Runtime facts supplied after generic frame validation and before a trusted
/// command is formed.
///
/// The ingress module deliberately does not resolve worktrees or open local
/// state itself. Its caller supplies this narrow capability only after it has
/// bound the reported cwd to the active worktree. The private HMAC key lets
/// ingress turn a raw frame into an opaque replay identity without allowing
/// that frame to escape into a durable dedup ring.
pub(crate) struct CaptureIngressBinding {
    verified_cwd: String,
    dedup: CaptureIngressDedup,
    runtime_scope: CaptureRuntimeScope,
}

/// The synchronous production path computes a commitment locally from its
/// private key.  The deadline-bounded child instead returns only an already
/// opaque commitment, never the key itself.
enum CaptureIngressDedup {
    Secret([u8; 32]),
    Opaque {
        event_id: Uuid,
        dedup: Option<OpaqueDedupIdentity>,
    },
}

impl CaptureIngressBinding {
    pub(crate) fn new(
        verified_cwd: String,
        dedup_secret: [u8; 32],
        runtime_scope: CaptureRuntimeScope,
    ) -> Self {
        Self {
            verified_cwd,
            dedup: CaptureIngressDedup::Secret(dedup_secret),
            runtime_scope,
        }
    }

    /// Construct the bounded-helper result after it has HMACed the ingress
    /// preimage in the child process.  This deliberately cannot carry the
    /// repository key back across the helper wire.
    pub(crate) fn from_opaque_identity(
        verified_cwd: String,
        event_id: Uuid,
        dedup: Option<OpaqueDedupIdentity>,
        runtime_scope: CaptureRuntimeScope,
    ) -> Self {
        Self {
            verified_cwd,
            dedup: CaptureIngressDedup::Opaque { event_id, dedup },
            runtime_scope,
        }
    }

    /// Split a verified binding for the private scope-binding helper protocol.
    ///
    /// This extraction is available only to the helper's in-process key-use
    /// path. The secret never becomes a helper response field or stdout data.
    pub(crate) fn into_scope_binding_parts(
        self,
    ) -> Result<(String, [u8; 32], CaptureRuntimeScope)> {
        let CaptureIngressDedup::Secret(dedup_secret) = self.dedup else {
            bail!("scope-binding helper received an already-opaque dedup identity")
        };
        Ok((self.verified_cwd, dedup_secret, self.runtime_scope))
    }

    /// Split out non-secret scope facts after an opaque-helper response. This
    /// is intentionally separate from [`Self::into_scope_binding_parts`] so
    /// no caller can accidentally recover a key from an opaque binding.
    #[cfg(test)]
    pub(crate) fn into_verified_scope_parts(self) -> (String, CaptureRuntimeScope) {
        (self.verified_cwd, self.runtime_scope)
    }
}

/// The small set of provider facts which later capture layers may use.
///
/// It intentionally excludes the flattened provider `extra` map and the raw
/// hook-event spelling. `LifecycleEvent` is the only canonical event handoff;
/// this context exists solely for worktree-bound source selection and catalog
/// attribution. It has no `Debug` implementation because its runtime scope is
/// private process state, not a general-purpose provider payload.
pub(crate) struct CaptureEventContext {
    pub(crate) provider_session_id: String,
    pub(crate) working_dir: String,
    pub(crate) runtime_scope: CaptureRuntimeScope,
}

/// A fully validated, canonical capture command.
///
/// The command is constructed only by the ingress-owned
/// [`CaptureIngressCommand::from_stdin`] or
/// [`CaptureIngressCommand::from_payload`] lowering paths.
/// It carries no database/ref handle and no raw stdin buffer, preventing an
/// ingress consumer from accidentally coupling validation to durable writes.
pub struct CaptureIngressCommand {
    /// Bounded input size retained solely for content-free observability.
    frame_bytes: usize,
    /// CLI hook verb that selected this provider callback.
    hook_command: ProviderHookCommand,
    /// Stable provider kind for attribution only; coordinator policy must
    /// not branch on it.
    provider_kind: &'static str,
    /// Provider source label used in sanitized metadata.
    provider_source: &'static str,
    /// Canonical, stable identity for dedup/replay.
    event_id: uuid::Uuid,
    /// Explicit declaration of how `event_id` was derived.  This travels with
    /// the trusted ingress handoff so the lifecycle sidecar never has to
    /// guess from a UUID shape or from the optional replay receipt.
    identity_scheme: LifecycleIdentityScheme,
    /// Absolute deadline injected by the caller. Managed Claude/Codex hook
    /// commands derive this from their provider-owned timeout before stdin is
    /// read; direct/test callers may omit it.
    deadline: Option<CaptureDeadline>,
    /// Opaque, per-repository replay identity. It is a keyed commitment, never
    /// a raw field value or an unkeyed hash that could become an offline oracle.
    dedup_key: Option<String>,
    /// Bounded provider/session facts retained after the full envelope drops.
    context: CaptureEventContext,
    /// Canonical lifecycle event lowered by the provider adapter.
    event: LifecycleEvent,
}

/// Ingress either yields a canonical command or an explicit safe no-op for a
/// newer provider event unknown to this build.
pub enum CaptureIngressOutcome {
    Command(Box<CaptureIngressCommand>),
    /// Sanitized diagnostics only. The raw event name may be arbitrary input
    /// and must never escape to a tracing sink or CLI error.
    UnknownEvent {
        frame_bytes: usize,
        event_name_len: usize,
    },
}

/// Parsed untrusted input that has passed generic envelope and provider
/// validation but has not yet crossed the worktree-binding boundary.
///
/// This stays private to ingress so the raw envelope cannot escape to the
/// runtime while the asynchronous scope-binding path is awaiting its helper.
struct ParsedCaptureIngress {
    frame_bytes: usize,
    hook_command: ProviderHookCommand,
    provider_kind: &'static str,
    provider_source: &'static str,
    deadline: Option<CaptureDeadline>,
    envelope: SessionHookEnvelope,
    event: LifecycleEvent,
}

enum ParsedCaptureIngressOutcome {
    Command(Box<ParsedCaptureIngress>),
    UnknownEvent {
        frame_bytes: usize,
        event_name_len: usize,
    },
}

/// Trusted handoff from ingress to the next capture layer.
///
/// This type intentionally has no `Debug` implementation because the parsed
/// envelope and event may still contain provider payload fields. It can only
/// be produced by consuming a validated [`CaptureIngressCommand`].
pub(crate) struct CaptureIngressParts {
    pub(crate) frame_bytes: usize,
    pub(crate) hook_command: ProviderHookCommand,
    pub(crate) provider_kind: &'static str,
    pub(crate) provider_source: &'static str,
    pub(crate) event_id: uuid::Uuid,
    pub(crate) identity_scheme: LifecycleIdentityScheme,
    pub(crate) deadline: Option<CaptureDeadline>,
    pub(crate) dedup_key: Option<String>,
    pub(crate) context: CaptureEventContext,
    pub(crate) event: LifecycleEvent,
}

impl CaptureIngressCommand {
    /// Read, validate, and lower one bounded hook frame from process stdin.
    ///
    /// This is the production ingress entrypoint.  Keeping the raw byte
    /// buffer local to this module ensures it cannot survive in a runtime
    /// stack frame after validation or accidentally reach persistence.
    pub(crate) async fn from_stdin<F, Fut>(
        hook_command: ProviderHookCommand,
        expected_kind: LifecycleEventKind,
        provider: &dyn HookProvider,
        deadline: Option<CaptureDeadline>,
        bind_runtime: F,
    ) -> Result<CaptureIngressOutcome>
    where
        F: FnOnce(CaptureScopeInput) -> Fut,
        Fut: Future<Output = Result<CaptureIngressBinding>>,
    {
        let payload = read_stdin_until_deadline(deadline).await?;
        let parsed = parse_capture_ingress_payload(
            &payload,
            hook_command,
            expected_kind,
            provider,
            deadline,
        )?;
        // Do not retain raw hook bytes across the potentially slow runtime
        // binding boundary. Only the validated prebinding representation below
        // can survive the helper await.
        drop(payload);
        match parsed {
            ParsedCaptureIngressOutcome::UnknownEvent {
                frame_bytes,
                event_name_len,
            } => Ok(CaptureIngressOutcome::UnknownEvent {
                frame_bytes,
                event_name_len,
            }),
            ParsedCaptureIngressOutcome::Command(parsed) => {
                let parsed = *parsed;
                let scope_input = CaptureScopeInput {
                    reported_cwd: parsed.envelope.cwd.clone(),
                    event_identity_preimage: event_identity_preimage(
                        provider,
                        &parsed.envelope,
                        &parsed.event,
                    ),
                    dedup_preimage: native_dedup_preimage(
                        provider,
                        &parsed.envelope,
                        parsed.event.kind,
                    ),
                };
                let binding = bind_runtime(scope_input).await?;
                finish_capture_ingress(parsed, binding, provider)
            }
        }
    }

    /// Parse, validate, and canonicalize one untrusted provider hook frame.
    ///
    /// The provider parser only runs after generic envelope validation. An
    /// unrecognized provider event becomes an explicit no-op so callers can
    /// log it without creating catalog/checkpoint side effects.
    pub(crate) fn from_payload(
        payload: &[u8],
        hook_command: ProviderHookCommand,
        expected_kind: LifecycleEventKind,
        provider: &dyn HookProvider,
        deadline: Option<CaptureDeadline>,
        bind_runtime: impl FnOnce(&CaptureScopeInput) -> Result<CaptureIngressBinding>,
    ) -> Result<CaptureIngressOutcome> {
        match parse_capture_ingress_payload(
            payload,
            hook_command,
            expected_kind,
            provider,
            deadline,
        )? {
            ParsedCaptureIngressOutcome::UnknownEvent {
                frame_bytes,
                event_name_len,
            } => Ok(CaptureIngressOutcome::UnknownEvent {
                frame_bytes,
                event_name_len,
            }),
            ParsedCaptureIngressOutcome::Command(parsed) => {
                let parsed = *parsed;
                let scope_input = CaptureScopeInput {
                    reported_cwd: parsed.envelope.cwd.clone(),
                    event_identity_preimage: event_identity_preimage(
                        provider,
                        &parsed.envelope,
                        &parsed.event,
                    ),
                    dedup_preimage: native_dedup_preimage(
                        provider,
                        &parsed.envelope,
                        parsed.event.kind,
                    ),
                };
                // The runtime resolver receives only the bounded cwd claim,
                // never the raw frame or its flattened provider fields. It
                // must bind that claim before key material is created or any
                // command is returned.
                let binding = bind_runtime(&scope_input)?;
                finish_capture_ingress(parsed, binding, provider)
            }
        }
    }

    /// The command that selected this provider callback.
    pub fn hook_command(&self) -> ProviderHookCommand {
        self.hook_command
    }

    /// Bounded frame size for sanitized telemetry; never the frame itself.
    pub(crate) fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    /// Stable provider kind for attribution. It is data, never coordinator
    /// dispatch policy.
    pub fn provider_kind(&self) -> &'static str {
        self.provider_kind
    }

    /// Provider source label for sanitized metadata.
    pub fn provider_source(&self) -> &'static str {
        self.provider_source
    }

    /// Stable identity for deduplication and replay.
    pub fn event_id(&self) -> uuid::Uuid {
        self.event_id
    }

    /// Typed derivation declaration for the canonical lifecycle sidecar.
    #[cfg(test)]
    pub(crate) fn identity_scheme(&self) -> LifecycleIdentityScheme {
        self.identity_scheme
    }

    /// Absolute deadline injected by the caller, if one applies.
    pub(crate) fn deadline(&self) -> Option<CaptureDeadline> {
        self.deadline
    }

    /// The canonical lifecycle kind, safe for metrics and response policy.
    pub fn event_kind(&self) -> LifecycleEventKind {
        self.event.kind
    }

    /// The runtime worktree pin selected before this command was created.
    /// It is intentionally crate-private: callers outside the capture runtime
    /// must resolve their own request scope rather than reuse hook state.
    pub(crate) fn runtime_scope(&self) -> &CaptureRuntimeScope {
        &self.context.runtime_scope
    }

    /// Consume the command at the trusted runtime boundary.
    ///
    /// The parsed envelope and normalized event can still contain provider
    /// payload fields, so they are intentionally not exposed through a
    /// `Debug` implementation or borrow accessors. The next capture layers
    /// own redaction before durable use.
    pub(crate) fn into_parts(self) -> CaptureIngressParts {
        CaptureIngressParts {
            frame_bytes: self.frame_bytes,
            hook_command: self.hook_command,
            provider_kind: self.provider_kind,
            provider_source: self.provider_source,
            event_id: self.event_id,
            identity_scheme: self.identity_scheme,
            deadline: self.deadline,
            dedup_key: self.dedup_key,
            context: self.context,
            event: self.event,
        }
    }
}

/// Lower an in-process integration-test frame at the ingress boundary.
///
/// Rust compiles integration tests as external crates, so they cannot use the
/// crate-private binding capability that production hook dispatch owns. Keep
/// that exception here — the only module allowed to receive raw hook bytes —
/// rather than letting a runtime or test harness accept a second raw-input
/// seam. This function returns only the opaque typed ingress outcome and
/// drops the frame before its caller can reach the runtime.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn lower_in_process_capture_frame_for_test(
    payload: &[u8],
    command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    repo_path: Option<&std::path::Path>,
) -> Result<CaptureIngressOutcome> {
    // An in-process test supplies its repository explicitly. Never make a
    // test helper an escape hatch that adopts the envelope's claimed cwd.
    let runtime_root = match repo_path {
        Some(path) => path
            .canonicalize()
            .context("canonicalize explicit in-process capture repository")?,
        None => std::path::PathBuf::from("/in-process-capture"),
    };
    let verified_cwd = runtime_root
        .to_str()
        .context("in-process capture repository path is not valid UTF-8")?
        .to_string();
    let runtime_storage = runtime_root.clone();
    CaptureIngressCommand::from_payload(payload, command, expected_kind, provider, None, |_| {
        Ok(CaptureIngressBinding::new(
            verified_cwd,
            [0xA5; 32],
            CaptureRuntimeScope::new(runtime_storage, runtime_root),
        ))
    })
}

fn parse_capture_ingress_payload(
    payload: &[u8],
    hook_command: ProviderHookCommand,
    expected_kind: LifecycleEventKind,
    provider: &dyn HookProvider,
    deadline: Option<CaptureDeadline>,
) -> Result<ParsedCaptureIngressOutcome> {
    if payload.len() > MAX_STDIN_BYTES {
        return Err(hook_input_exceeds_limit().into());
    }
    let stdin = std::str::from_utf8(payload)
        .map_err(|error| HookEnvelopeInvalid(format!("hook input is not valid UTF-8: {error}")))?;
    if stdin.trim().is_empty() {
        return Err(HookEnvelopeInvalid("hook input is empty".to_string()).into());
    }
    let envelope: SessionHookEnvelope = serde_json::from_str(stdin).map_err(|error| {
        HookEnvelopeInvalid(format!(
            "invalid hook JSON payload ({:?} at line {} column {})",
            error.classify(),
            error.line(),
            error.column()
        ))
    })?;
    validate_session_hook_envelope(&envelope, MAX_TRANSCRIPT_PATH_BYTES)
        .map_err(|error| HookEnvelopeInvalid(error.to_string()))?;
    if envelope.cwd.len() > MAX_REPORTED_CWD_BYTES {
        return Err(HookEnvelopeInvalid(format!(
            "hook cwd exceeds {MAX_REPORTED_CWD_BYTES} bytes"
        ))
        .into());
    }
    if !provider.recognizes_event(&envelope.hook_event_name) {
        return Ok(ParsedCaptureIngressOutcome::UnknownEvent {
            frame_bytes: payload.len(),
            event_name_len: envelope.hook_event_name.len(),
        });
    }

    let event = provider.parse_hook_event(&envelope.hook_event_name, &envelope)?;
    if event.kind != expected_kind {
        // This is a bounded grammar mismatch between the selected CLI verb
        // and the already parsed event name; unlike a later runtime failure,
        // it contains no provider-controlled path or session content and is
        // safe to surface as an actionable envelope rejection.
        return Err(HookEnvelopeInvalid(format!(
            "hook event kind mismatch: expected '{expected_kind}', got '{}'",
            event.kind,
        ))
        .into());
    }

    // A non-terminal callback has no recovery obligation, so never let a
    // deadline-expired frame create the per-repository HMAC key while binding
    // its reported cwd. SessionEnd is deliberately exempt: its runtime path
    // must bind trusted scope before it can persist the content-free pending
    // receipt that makes a later retry recoverable.
    if event.kind != LifecycleEventKind::SessionEnd
        && let Some(deadline) = deadline
    {
        deadline.ensure_before_mutation("binding hook runtime scope")?;
    }

    Ok(ParsedCaptureIngressOutcome::Command(Box::new(
        ParsedCaptureIngress {
            frame_bytes: payload.len(),
            hook_command,
            provider_kind: provider.provider_name(),
            provider_source: provider.source_name(),
            deadline,
            envelope,
            event,
        },
    )))
}

fn hook_input_exceeds_limit() -> HookEnvelopeInvalid {
    HookEnvelopeInvalid(format!("hook input exceeds {MAX_STDIN_BYTES} bytes"))
}

fn finish_capture_ingress(
    parsed: ParsedCaptureIngress,
    binding: CaptureIngressBinding,
    provider: &dyn HookProvider,
) -> Result<CaptureIngressOutcome> {
    let ParsedCaptureIngress {
        frame_bytes,
        hook_command,
        provider_kind,
        provider_source,
        deadline,
        envelope,
        mut event,
    } = parsed;
    let CaptureIngressBinding {
        verified_cwd,
        dedup: binding_dedup,
        runtime_scope,
    } = binding;
    if verified_cwd.is_empty() {
        bail!("capture ingress resolver returned an empty verified cwd");
    }
    let event_identity_preimage = event_identity_preimage(provider, &envelope, &event);
    let (event_id, dedup, identity_scheme) = match binding_dedup {
        CaptureIngressDedup::Secret(dedup_secret) => {
            if let Some(dedup) =
                opaque_dedup_identity(&dedup_secret, provider, &envelope, event.kind)
            {
                (
                    dedup.event_id,
                    Some(dedup),
                    LifecycleIdentityScheme::NativeReplayHmacV2,
                )
            } else {
                let (event_id, _) =
                    opaque_event_identity_from_preimage(&dedup_secret, &event_identity_preimage);
                (
                    event_id,
                    None,
                    LifecycleIdentityScheme::FallbackActionHmacV1,
                )
            }
        }
        CaptureIngressDedup::Opaque { event_id, dedup } => {
            let identity_scheme = if dedup.is_some() {
                LifecycleIdentityScheme::NativeReplayHmacV2
            } else {
                LifecycleIdentityScheme::FallbackActionHmacV1
            };
            (event_id, dedup, identity_scheme)
        }
    };
    // A provider transcript pointer is an input to later provenance
    // verification, not a canonical lifecycle-event property. Avoid carrying
    // it twice through the trusted handoff.
    event.session_ref = None;
    let context = CaptureEventContext {
        provider_session_id: envelope.session_id,
        working_dir: verified_cwd,
        runtime_scope,
    };

    Ok(CaptureIngressOutcome::Command(Box::new(
        CaptureIngressCommand {
            frame_bytes,
            hook_command,
            provider_kind,
            provider_source,
            event_id,
            identity_scheme,
            deadline,
            dedup_key: dedup.map(|identity| identity.key),
            context,
            event,
        },
    )))
}

/// Read one bounded hook frame without allowing an open pipe to outlive the
/// host-owned capture budget. Unix uses Tokio readiness on a duplicated,
/// nonblocking descriptor for pollable input; regular-file redirection uses a
/// killable private helper because epoll cannot register it and filesystem I/O
/// cannot be cancelled reliably in-process. Either path keeps an embedded
/// `exec_async` caller's runtime pollable while preserving the original
/// capture deadline.
async fn read_stdin_until_deadline(deadline: Option<CaptureDeadline>) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        read_stdin_unix(deadline).await
    }
    #[cfg(not(unix))]
    {
        match deadline {
            Some(deadline) => read_stdin_bounded(deadline).await,
            None => read_stdin_unbounded().await,
        }
    }
}

#[cfg(unix)]
struct AsyncHookStdin {
    fd: OwnedFd,
    original_status_flags: libc::c_int,
}

#[cfg(unix)]
fn duplicate_hook_stdin() -> Result<OwnedFd> {
    // SAFETY: stdin is inherited from the hook host. The duplicate is owned
    // immediately below and cannot close the host's original descriptor.
    let raw_fd = unsafe { libc::dup(libc::STDIN_FILENO) };
    if raw_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("duplicate hook stdin");
    }
    // SAFETY: `dup` returned a new owned descriptor on success.
    Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
}

#[cfg(unix)]
fn stdin_is_regular_file(fd: &OwnedFd) -> Result<bool> {
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `fd` is a live descriptor and `stat` points to writable storage
    // for exactly one `libc::stat` value.
    let result = unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("inspect hook stdin type");
    }
    // SAFETY: successful `fstat` initialized the complete `libc::stat`.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFREG)
}

#[cfg(unix)]
impl AsyncHookStdin {
    fn from_owned_fd_nonblocking(fd: OwnedFd) -> Result<Self> {
        // SAFETY: the duplicate is a valid open descriptor.
        let original_status_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if original_status_flags < 0 {
            return Err(std::io::Error::last_os_error()).context("inspect hook stdin flags");
        }
        // `O_NONBLOCK` is a file-status flag and therefore shared with the
        // inherited descriptor. The hook has one ingress reader; restore the
        // exact prior flags in Drop before returning to an embedded caller.
        // SAFETY: the duplicate remains valid and the flags came from F_GETFL.
        let set = unsafe {
            libc::fcntl(
                fd.as_raw_fd(),
                libc::F_SETFL,
                original_status_flags | libc::O_NONBLOCK,
            )
        };
        if set < 0 {
            return Err(std::io::Error::last_os_error()).context("make hook stdin nonblocking");
        }
        Ok(Self {
            fd,
            original_status_flags,
        })
    }
}

#[cfg(unix)]
impl AsRawFd for AsyncHookStdin {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(unix)]
impl Drop for AsyncHookStdin {
    fn drop(&mut self) {
        // SAFETY: this owned duplicate is still live during Drop. Restoring
        // flags avoids leaking O_NONBLOCK onto the host's shared stdin file
        // description after an embedded hook returns.
        let _ = unsafe {
            libc::fcntl(
                self.fd.as_raw_fd(),
                libc::F_SETFL,
                self.original_status_flags,
            )
        };
    }
}

#[cfg(unix)]
async fn read_stdin_unix(deadline: Option<CaptureDeadline>) -> Result<Vec<u8>> {
    let stdin = duplicate_hook_stdin()?;
    // Linux epoll rejects regular files with EPERM. More importantly, a
    // regular-file read may block on filesystem I/O even though it has no
    // producer to wait for. This is intentionally detected before setting
    // O_NONBLOCK, so the helper path never mutates the host's shared stdin
    // flags.
    if stdin_is_regular_file(&stdin)? {
        return read_regular_stdin_until_deadline(stdin, deadline).await;
    }
    let stdin = AsyncHookStdin::from_owned_fd_nonblocking(stdin)?;
    let stdin = tokio::io::unix::AsyncFd::new(stdin)
        .context("register hook stdin for asynchronous readiness")?;
    let mut payload = Vec::new();
    let read_cap = MAX_STDIN_BYTES.saturating_add(1);
    payload
        .try_reserve_exact(read_cap)
        .context("allocate bounded hook stdin frame")?;
    if payload.capacity() > read_cap {
        bail!("allocate bounded hook stdin frame exceeded its cap");
    }
    loop {
        let mut readiness = match deadline {
            Some(deadline) => {
                let remaining = deadline
                    .monotonic()
                    .saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("capture ingress deadline expired while reading stdin");
                }
                tokio::time::timeout(remaining, stdin.readable())
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!("capture ingress deadline expired while reading stdin")
                    })?
                    .context("wait for hook stdin readiness")?
            }
            None => stdin
                .readable()
                .await
                .context("wait for hook stdin readiness")?,
        };
        let remaining_capacity = read_cap.saturating_sub(payload.len());
        if remaining_capacity == 0 {
            return Ok(payload);
        }
        let mut buffer = [0_u8; 8192];
        let read_capacity = remaining_capacity.min(buffer.len());
        let read = readiness.try_io(|inner| {
            // SAFETY: the local buffer is writable for `read_capacity`
            // bytes, and `AsyncHookStdin` owns a live duplicated descriptor.
            let read = unsafe {
                libc::read(
                    inner.get_ref().as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    read_capacity,
                )
            };
            if read < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                usize::try_from(read)
                    .map_err(|_| std::io::Error::other("hook stdin read count exceeds usize"))
            }
        });
        match read {
            Ok(Ok(0)) => return Ok(payload),
            Ok(Ok(read)) => payload.extend_from_slice(&buffer[..read]),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error).context("read hook stdin"),
            Err(_) => continue,
        }
    }
}

/// Read regular-file stdin in a killable helper process.
///
/// A regular file can block in an NFS/FUSE/kernel read despite being finite.
/// Tokio cannot cancel an already-running `spawn_blocking` operation, and its
/// runtime shutdown would wait for that operation. The helper owns the
/// duplicated descriptor instead, so deadline expiry can SIGKILL the blocking
/// read without holding this hook process or its runtime hostage.
#[cfg(unix)]
async fn read_regular_stdin_until_deadline(
    stdin: OwnedFd,
    deadline: Option<CaptureDeadline>,
) -> Result<Vec<u8>> {
    if deadline.is_some_and(|deadline| deadline.monotonic() <= Instant::now()) {
        bail!("capture ingress deadline expired while reading stdin");
    }

    let program = helper_program()
        .ok_or_else(|| anyhow::anyhow!("killable regular hook stdin reader is unavailable"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .arg(AUTHORIZED_READ_HELPER_ARG)
        .env(AUTHORIZED_READ_HELPER_CAP_ENV, MAX_STDIN_BYTES.to_string())
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_private_helper_process_group(&mut command);
    let spawned = command
        .spawn()
        .context("start killable regular hook stdin reader")?;
    let mut child = CancellationSafeChild::new_process_group(spawned);
    let mut stdout = child
        .child_mut()
        .and_then(|child| child.stdout.take())
        .ok_or_else(|| anyhow::anyhow!("killable regular hook stdin reader has no stdout pipe"))?;
    let helper_response_cap = (MAX_STDIN_BYTES as u64).saturating_add(9);
    let mut stdout_task =
        tokio::spawn(
            async move { read_async_strictly_bounded(&mut stdout, helper_response_cap).await },
        );
    child.register_abort_on_cancel(&stdout_task);

    #[cfg(test)]
    let deadline =
        match tests::regular_stdin_helper_support::deadline_after_helper_ready(deadline).await {
            Ok(deadline) => deadline,
            Err(error) => {
                stdout_task.abort();
                child.terminate_and_reap();
                return Err(error);
            }
        };

    // Read stdout to EOF before `wait()`. A helper descendant may hold the
    // raw regular-file stdin and inherited stdout after the leader exits; a
    // deadline or outer Drop must still own the unreaped leader PGID to kill
    // that entire group safely.
    let output = match deadline {
        Some(deadline) => {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline.monotonic()),
                &mut stdout_task,
            )
            .await
            {
                Ok(Ok(Ok(output))) => output,
                Ok(Ok(Err(_))) | Ok(Err(_)) => {
                    child.terminate_and_reap();
                    bail!("killable regular hook stdin reader returned an invalid response");
                }
                Err(_) => {
                    stdout_task.abort();
                    child.terminate_and_reap();
                    bail!("capture ingress deadline expired while reading stdin");
                }
            }
        }
        None => match (&mut stdout_task).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) | Err(_) => {
                child.terminate_and_reap();
                bail!("killable regular hook stdin reader returned an invalid response");
            }
        },
    };

    if output.len() > MAX_STDIN_BYTES + 9 {
        child.terminate_and_reap();
        bail!("killable regular hook stdin reader returned an invalid response");
    }
    let status = match child.child_mut() {
        Some(child_process) => match deadline {
            Some(deadline) => match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline.monotonic()),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => {
                    child.terminate_and_reap();
                    return Err(error).context("wait for killable regular hook stdin reader");
                }
                Err(_) => {
                    child.terminate_and_reap();
                    bail!("capture ingress deadline expired while reading stdin");
                }
            },
            None => match child_process.wait().await {
                Ok(status) => status,
                Err(error) => {
                    child.terminate_and_reap();
                    return Err(error).context("wait for killable regular hook stdin reader");
                }
            },
        },
        None => {
            child.terminate_and_reap();
            bail!("killable regular hook stdin reader was unavailable");
        }
    };
    child.disarm_child_after_wait();
    child.finish();

    if !status.success() {
        bail!("killable regular hook stdin reader returned an invalid response");
    }
    let (status, raw_bytes) = output
        .split_first()
        .and_then(|(status, rest)| {
            let raw_bytes = <[u8; 8]>::try_from(rest.get(..8)?).ok()?;
            Some((*status, u64::from_le_bytes(raw_bytes)))
        })
        .ok_or_else(|| {
            anyhow::anyhow!("killable regular hook stdin reader returned a truncated frame")
        })?;
    let payload = &output[9..];
    match status {
        0 if raw_bytes <= MAX_STDIN_BYTES as u64 && payload.len() as u64 == raw_bytes => {
            Ok(payload.to_vec())
        }
        1 if raw_bytes > MAX_STDIN_BYTES as u64 && payload.is_empty() => {
            Err(hook_input_exceeds_limit().into())
        }
        2 if payload.is_empty() => bail!("killable regular hook stdin reader failed"),
        _ => bail!("killable regular hook stdin reader returned an invalid response"),
    }
}

#[cfg(not(unix))]
async fn read_stdin_unbounded() -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    let read_cap = MAX_STDIN_BYTES.saturating_add(1);
    payload
        .try_reserve_exact(read_cap)
        .context("allocate bounded hook stdin frame")?;
    if payload.capacity() > read_cap {
        bail!("allocate bounded hook stdin frame exceeded its cap");
    }
    let mut stdin = std::io::stdin().lock();
    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = read_cap.saturating_sub(payload.len());
        if remaining == 0 {
            return Ok(payload);
        }
        let read_len = remaining.min(chunk.len());
        match stdin.read(&mut chunk[..read_len]) {
            Ok(0) => return Ok(payload),
            Ok(read) => payload.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("failed to read stdin"),
        }
    }
}

#[cfg(windows)]
async fn read_stdin_bounded(deadline: CaptureDeadline) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || read_stdin_bounded_blocking(deadline))
        .await
        .context("bounded hook stdin reader task terminated")?
}

#[cfg(windows)]
fn read_stdin_bounded_blocking(deadline: CaptureDeadline) -> Result<Vec<u8>> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{
        Foundation::{ERROR_BROKEN_PIPE, HANDLE},
        System::Pipes::PeekNamedPipe,
    };

    const POLL_SLICE: Duration = Duration::from_millis(5);
    let stdin = std::io::stdin();
    let handle = stdin.as_raw_handle() as HANDLE;
    let mut payload = Vec::new();
    let read_cap = MAX_STDIN_BYTES.saturating_add(1);
    payload
        .try_reserve_exact(read_cap)
        .context("allocate bounded hook stdin frame")?;
    if payload.capacity() > read_cap {
        bail!("allocate bounded hook stdin frame exceeded its cap");
    }
    loop {
        let remaining = deadline
            .monotonic()
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("capture ingress deadline expired while reading stdin");
        }
        let mut available = 0_u32;
        // SAFETY: the hook host supplies stdin as an inherited pipe; the
        // pointer arguments are either null or refer to initialized locals.
        let ready = unsafe {
            PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ready == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(payload);
            }
            return Err(error).context("wait for hook stdin readiness");
        }
        if available == 0 {
            std::thread::sleep(POLL_SLICE.min(remaining));
            continue;
        }
        let remaining_capacity = read_cap.saturating_sub(payload.len());
        if remaining_capacity == 0 {
            return Ok(payload);
        }
        let mut buffer = [0_u8; 8192];
        let to_read = remaining_capacity
            .min(buffer.len())
            .min(usize::try_from(available).context("hook stdin availability exceeds usize")?);
        let read = stdin
            .lock()
            .read(&mut buffer[..to_read])
            .context("read hook stdin")?;
        if read == 0 {
            return Ok(payload);
        }
        payload.extend_from_slice(&buffer[..read]);
    }
}

#[cfg(not(any(unix, windows)))]
async fn read_stdin_bounded(_deadline: CaptureDeadline) -> Result<Vec<u8>> {
    bail!("bounded hook stdin reads are unsupported on this platform")
}

/// Opaque replay identity generated while the raw frame is still local to
/// ingress. `key` is safe to store in a bounded processed-event ring; it is a
/// keyed HMAC commitment rather than a spelling of any caller value.
pub(crate) struct OpaqueDedupIdentity {
    event_id: Uuid,
    key: String,
}

impl OpaqueDedupIdentity {
    pub(crate) fn event_id(&self) -> Uuid {
        self.event_id
    }

    /// Convert the safe opaque result into the bounded helper wire shape.
    /// Neither component permits reconstructing the repository key or any
    /// provider-native input.
    pub(crate) fn into_wire_parts(self) -> (Uuid, String) {
        (self.event_id, self.key)
    }

    /// Reconstitute an opaque result returned by the trusted scope helper.
    /// Keep the public wire constrained to the exact current HMAC commitment
    /// grammar so a malformed helper response cannot smuggle arbitrary text
    /// into the durable receipt ring.
    pub(crate) fn from_wire_parts(event_id: Uuid, key: String) -> Result<Self> {
        let expected = opaque_commitment_event_id(
            &key,
            "capture-dedup-v2:",
            "scope-binding helper returned an invalid opaque dedup key",
        )?;
        // Match the exact UUID normalization used when the helper creates the
        // opaque identity. A response with a detached event ID could otherwise
        // split the catalog action key from its processed-event receipt.
        if event_id != expected {
            bail!("scope-binding helper returned mismatched opaque dedup identity");
        }
        Ok(Self { event_id, key })
    }
}

/// Validate the short-lived HMAC commitment that proves an opaque action
/// UUID returned by the scope helper. This commitment never crosses into a
/// catalog receipt or telemetry: the parent retains only the validated UUID.
pub(crate) fn opaque_event_id_from_wire_parts(event_id: Uuid, commitment: String) -> Result<Uuid> {
    let expected = if commitment.starts_with("capture-event-v1:") {
        opaque_commitment_event_id(
            &commitment,
            "capture-event-v1:",
            "scope-binding helper returned an invalid opaque event commitment",
        )?
    } else if commitment.starts_with("capture-dedup-v2:") {
        opaque_commitment_event_id(
            &commitment,
            "capture-dedup-v2:",
            "scope-binding helper returned an invalid opaque event commitment",
        )?
    } else {
        bail!("scope-binding helper returned an invalid opaque event commitment");
    };
    if event_id != expected {
        bail!("scope-binding helper returned mismatched opaque event identity");
    }
    Ok(event_id)
}

fn opaque_commitment_event_id(
    commitment: &str,
    prefix: &str,
    invalid_message: &'static str,
) -> Result<Uuid> {
    let Some(hex) = commitment.strip_prefix(prefix) else {
        bail!("{invalid_message}");
    };
    if hex.len() != 64
        || !hex
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("{invalid_message}");
    }
    let mac_bytes = hex::decode(hex).map_err(|_| anyhow::anyhow!(invalid_message))?;
    let mut event_bytes = [0_u8; 16];
    event_bytes.copy_from_slice(&mac_bytes[..16]);
    // Match the UUID normalization applied at HMAC derivation time.
    event_bytes[6] = (event_bytes[6] & 0x0f) | 0x50;
    event_bytes[8] = (event_bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(event_bytes))
}

/// Build a stable opaque identity from a provider-native scalar ID.
///
/// The capture layer intentionally declines semantic fallback identity. A
/// repeatable lifecycle boundary without a provider-guaranteed delivery ID is
/// indistinguishable from a later valid event; hashing its payload would risk
/// silently dropping a checkpoint. Native identity fields are deliberately
/// restricted to JSON scalars, and an absent or structured value simply means
/// the event has no replay receipt.
fn opaque_dedup_identity(
    dedup_secret: &[u8; 32],
    provider: &dyn HookProvider,
    envelope: &SessionHookEnvelope,
    event_kind: LifecycleEventKind,
) -> Option<OpaqueDedupIdentity> {
    native_dedup_preimage(provider, envelope, event_kind)
        .map(|preimage| opaque_dedup_identity_from_preimage(dedup_secret, &preimage))
}

/// Derive a total, length-delimited commitment to the lifecycle identity
/// used for the catalog action UUID. Unlike replay identity, this exists even
/// when the provider has no native delivery ID; it does *not* authorize a
/// replay receipt on its own.
fn event_identity_preimage(
    provider: &dyn HookProvider,
    envelope: &SessionHookEnvelope,
    event: &LifecycleEvent,
) -> [u8; CAPTURE_DEDUP_PREIMAGE_BYTES] {
    let mut preimage = digest::Context::new(&digest::SHA256);
    preimage.update(b"libra-capture-ingress-event-preimage-v1\0");
    hash_dedup_preimage_component(&mut preimage, provider.provider_name().as_bytes());
    hash_dedup_preimage_component(&mut preimage, envelope.hook_event_name.as_bytes());
    hash_dedup_preimage_component(&mut preimage, envelope.session_id.as_bytes());
    hash_dedup_preimage_component(
        &mut preimage,
        &event
            .timestamp
            .timestamp_nanos_opt()
            .unwrap_or(0)
            .to_be_bytes(),
    );
    hash_dedup_preimage_component(&mut preimage, &[event.kind as u8]);
    let digest = preimage.finish();
    let mut output = [0_u8; CAPTURE_DEDUP_PREIMAGE_BYTES];
    output.copy_from_slice(digest.as_ref());
    output
}

/// Derive a fixed, length-delimited SHA-256 commitment to the only provider
/// fields allowed to influence replay identity. The helper receives this
/// digest, not the provider name, event spelling, session ID, native key, or
/// native scalar value.
fn native_dedup_preimage(
    provider: &dyn HookProvider,
    envelope: &SessionHookEnvelope,
    event_kind: LifecycleEventKind,
) -> Option<[u8; CAPTURE_DEDUP_PREIMAGE_BYTES]> {
    let native_identity = provider
        .dedup_identity_keys_for_event(&envelope.hook_event_name, event_kind)
        .iter()
        .find_map(|key| {
            envelope
                .extra
                .get(*key)
                .and_then(opaque_scalar_identity)
                .map(|value| (*key, value))
        });
    let (key, value) = native_identity?;

    let mut preimage = digest::Context::new(&digest::SHA256);
    preimage.update(b"libra-capture-ingress-native-preimage-v1\0");
    hash_dedup_preimage_component(&mut preimage, provider.provider_name().as_bytes());
    hash_dedup_preimage_component(&mut preimage, envelope.hook_event_name.as_bytes());
    hash_dedup_preimage_component(&mut preimage, envelope.session_id.as_bytes());
    hash_dedup_preimage_component(&mut preimage, key.as_bytes());
    hash_dedup_preimage_component(&mut preimage, value.as_bytes());
    let digest = preimage.finish();
    let mut output = [0_u8; CAPTURE_DEDUP_PREIMAGE_BYTES];
    output.copy_from_slice(digest.as_ref());
    Some(output)
}

/// Use the repository-private key only to HMAC an ingress-produced fixed
/// digest. This is shared by the in-process path and the killable helper, so
/// their replay identities remain byte-for-byte identical without placing the
/// key on the helper response wire.
pub(crate) fn opaque_dedup_identity_from_preimage(
    dedup_secret: &[u8; 32],
    preimage: &[u8; CAPTURE_DEDUP_PREIMAGE_BYTES],
) -> OpaqueDedupIdentity {
    let (event_id, key) = opaque_hmac_identity(
        dedup_secret,
        preimage,
        b"libra-capture-ingress-dedup-v2\0",
        "capture-dedup-v2:",
    );
    OpaqueDedupIdentity { event_id, key }
}

/// Derive an opaque action UUID for an event that has no native replay key.
/// Its HMAC commitment is used only to validate the helper response and is
/// discarded before catalog/telemetry handoff.
pub(crate) fn opaque_event_identity_from_preimage(
    dedup_secret: &[u8; 32],
    preimage: &[u8; CAPTURE_DEDUP_PREIMAGE_BYTES],
) -> (Uuid, String) {
    opaque_hmac_identity(
        dedup_secret,
        preimage,
        b"libra-capture-ingress-event-v1\0",
        "capture-event-v1:",
    )
}

fn opaque_hmac_identity(
    dedup_secret: &[u8; 32],
    preimage: &[u8; CAPTURE_DEDUP_PREIMAGE_BYTES],
    domain: &[u8],
    prefix: &str,
) -> (Uuid, String) {
    let mut hmac = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, dedup_secret));
    hmac.update(domain);
    hmac.update(preimage);

    let mac = hmac.sign();
    let mac_bytes = mac.as_ref();
    let mut event_bytes = [0u8; 16];
    event_bytes.copy_from_slice(&mac_bytes[..16]);
    // Mark the opaque identifier as a conventional UUID variant/version while
    // retaining the HMAC's entropy and determinism.
    event_bytes[6] = (event_bytes[6] & 0x0f) | 0x50;
    event_bytes[8] = (event_bytes[8] & 0x3f) | 0x80;
    (
        Uuid::from_bytes(event_bytes),
        format!("{prefix}{}", hex::encode(mac_bytes)),
    )
}

/// Convert one permitted JSON scalar into a typed HMAC component.
fn opaque_scalar_identity(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(format!("string:{value}")),
        serde_json::Value::Number(value) => Some(format!("number:{value}")),
        serde_json::Value::Bool(value) => Some(format!("bool:{value}")),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

/// Feed a length-delimited byte component into the native identity preimage.
fn hash_dedup_preimage_component(preimage: &mut digest::Context, value: &[u8]) {
    preimage.update(&(value.len() as u64).to_be_bytes());
    preimage.update(value);
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        path::{Path, PathBuf},
    };
    #[cfg(unix)]
    use std::{
        ffi::CString,
        os::{fd::FromRawFd, unix::ffi::OsStrExt, unix::fs::PermissionsExt},
    };

    use anyhow::{Result, bail};
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::internal::ai::event::Event;

    // The production ingress boundary deliberately has no filesystem or
    // persistence dependency. This fixture-only coordination is nested under
    // the test module so it cannot become a production capability by
    // accident while exercising the killable regular-stdin helper.
    #[cfg(unix)]
    pub(crate) mod regular_stdin_helper_support {
        use std::{
            future::Future,
            path::PathBuf,
            time::{Duration, Instant},
        };

        use anyhow::{Context, Result, bail};

        use super::super::CaptureDeadline;

        tokio::task_local! {
            static HELPER_READY: (PathBuf, Duration);
        }

        pub(crate) async fn with_helper_ready<F>(
            ready_file: PathBuf,
            post_ready_budget: Duration,
            future: F,
        ) -> F::Output
        where
            F: Future,
        {
            HELPER_READY
                .scope((ready_file, post_ready_budget), future)
                .await
        }

        pub(crate) async fn deadline_after_helper_ready(
            initial_deadline: Option<CaptureDeadline>,
        ) -> Result<Option<CaptureDeadline>> {
            let Ok((ready_file, post_ready_budget)) = HELPER_READY.try_with(Clone::clone) else {
                return Ok(initial_deadline);
            };
            let Some(initial_deadline) = initial_deadline else {
                return Ok(None);
            };

            loop {
                if ready_file.is_file() {
                    let budget_millis = u64::try_from(post_ready_budget.as_millis())
                        .context("regular-stdin helper test deadline exceeds supported range")?;
                    return CaptureDeadline::from_budget_millis(budget_millis).map(Some);
                }
                if Instant::now() >= initial_deadline.monotonic() {
                    bail!(
                        "regular-stdin helper test fixture did not report readiness before its startup deadline"
                    );
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    struct TestProvider;

    // Ingress fixtures never reach the capture runtime; any closed kind
    // satisfies the typed identity contract.
    impl crate::internal::ai::hooks::provider::HookProviderIdentity for TestProvider {
        fn agent_kind(&self) -> crate::internal::ai::observed_agents::AgentKind {
            crate::internal::ai::observed_agents::AgentKind::Codex
        }
    }

    impl HookProvider for TestProvider {
        fn provider_name(&self) -> &'static str {
            "test-provider"
        }

        fn source_name(&self) -> &'static str {
            "test-source"
        }

        fn supported_commands(&self) -> &'static [ProviderHookCommand] {
            &[ProviderHookCommand::SessionStart]
        }

        fn parse_hook_event(
            &self,
            hook_event_name: &str,
            envelope: &SessionHookEnvelope,
        ) -> Result<LifecycleEvent> {
            let kind = match hook_event_name {
                "known" => LifecycleEventKind::SessionStart,
                "tool" => LifecycleEventKind::ToolUse,
                _ => bail!("unknown test hook event"),
            };
            Ok(LifecycleEvent {
                kind,
                session_id: envelope.session_id.clone(),
                session_ref: None,
                prompt: None,
                model: None,
                source: None,
                tool_name: None,
                tool_input: None,
                tool_response: None,
                assistant_message: None,
                timestamp: Utc
                    .timestamp_opt(1_700_000_000, 0)
                    .single()
                    .expect("valid timestamp"),
            })
        }

        fn recognizes_event(&self, hook_event_name: &str) -> bool {
            matches!(hook_event_name, "known" | "tool")
        }

        fn dedup_identity_keys(&self) -> &'static [&'static str] {
            &["event_id"]
        }

        fn install_hooks(
            &self,
            _options: &crate::internal::ai::hooks::provider::ProviderInstallOptions,
        ) -> Result<()> {
            Ok(())
        }

        fn uninstall_hooks(&self) -> Result<()> {
            Ok(())
        }

        fn hooks_are_installed(&self) -> Result<bool> {
            Ok(false)
        }
    }

    fn payload(event: &str) -> Vec<u8> {
        serde_json::json!({
            "hook_event_name": event,
            "session_id": "safe-session-id",
            "cwd": Path::new("/workspace").display().to_string(),
        })
        .to_string()
        .into_bytes()
    }

    fn test_binding(scope_input: &CaptureScopeInput) -> Result<CaptureIngressBinding> {
        Ok(CaptureIngressBinding::new(
            scope_input.reported_cwd.clone(),
            [0x11; 32],
            CaptureRuntimeScope::new(
                PathBuf::from("/test-storage"),
                PathBuf::from("/test-worktree"),
            ),
        ))
    }

    /// A regular file can hang inside an NFS/FUSE read after Tokio has begun
    /// a blocking operation. The private helper must be killed and reaped at
    /// the ingress deadline, leaving this task free to return without any
    /// capture side effects. A FIFO drives the same held-descriptor behavior
    /// through the helper without relying on a network filesystem in CI.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn regular_stdin_helper_deadline_kills_and_reaps_blocked_reader() {
        let temp = tempfile::tempdir().expect("create regular-stdin helper fixture");
        let script = temp.path().join("blocked-reader.sh");
        let ready_file = temp.path().join("regular-stdin-helper.ready");
        let escaped_ready = ready_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n/bin/sleep 60 &\ndescendant=\"$!\"\nprintf '%s %s\\n' \"$$\" \"$descendant\" > '{escaped_ready}'\nexit 0\n"
            ),
        )
        .expect("write blocked reader helper");
        let mut permissions = std::fs::metadata(&script)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).expect("make helper executable");

        let fifo = temp.path().join("blocked-regular-reader.fifo");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path has no NUL");
        // SAFETY: `fifo_c` is a NUL-terminated path in an isolated tempdir.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        // Open a separate O_RDWR descriptor which stays in the parent so the
        // child reader cannot observe EOF before the deadline.
        // SAFETY: `fifo_c` remains valid for both opens, and successful calls
        // return independent owned descriptors below.
        let keeper_raw = unsafe { libc::open(fifo_c.as_ptr(), libc::O_RDWR) };
        assert!(
            keeper_raw >= 0,
            "open FIFO keeper: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `keeper_raw` is a successful owned descriptor.
        let _keeper = unsafe { OwnedFd::from_raw_fd(keeper_raw) };
        // SAFETY: as above, this descriptor becomes the helper's stdin.
        let reader_raw = unsafe { libc::open(fifo_c.as_ptr(), libc::O_RDWR) };
        assert!(
            reader_raw >= 0,
            "open FIFO reader: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `reader_raw` is a successful owned descriptor.
        let reader = unsafe { OwnedFd::from_raw_fd(reader_raw) };

        let started = Instant::now();
        let result = crate::internal::ai::authorized_read::with_test_helper_program(
            script,
            regular_stdin_helper_support::with_helper_ready(
                ready_file.clone(),
                Duration::from_millis(250),
                read_regular_stdin_until_deadline(
                    reader,
                    Some(CaptureDeadline::from_budget_millis(5_000).expect("startup deadline")),
                ),
            ),
        )
        .await;
        let error = result.expect_err("blocked helper must hit the ingress deadline");
        assert!(
            error
                .to_string()
                .contains("deadline expired while reading stdin"),
            "unexpected helper failure: {error:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "blocked helper outlived the bounded ingress deadline: {:?}",
            started.elapsed()
        );

        let (leader, descendant) = async {
            for _ in 0..50 {
                if let Ok(value) = std::fs::read_to_string(&ready_file) {
                    let pids = value
                        .split_whitespace()
                        .filter_map(|pid| pid.parse::<libc::pid_t>().ok())
                        .collect::<Vec<_>>();
                    if let [leader, descendant] = pids.as_slice() {
                        return Some((*leader, *descendant));
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            None
        }
        .await
        .expect("ready regular-stdin helper must publish leader and descendant PIDs");
        let reaped = async {
            for _ in 0..50 {
                // SAFETY: signal zero only probes the exact child PIDs and
                // never alters their state.
                let leader_gone = unsafe { libc::kill(leader, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                let descendant_gone = unsafe { libc::kill(descendant, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if leader_gone && descendant_gone {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        }
        .await;
        assert!(
            reaped,
            "blocked helper leader or its raw-stdin/stdout-holding descendant survived deadline"
        );
    }

    #[test]
    fn validated_command_contains_only_canonical_capture_inputs() {
        let outcome = CaptureIngressCommand::from_payload(
            &payload("known"),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            &TestProvider,
            None,
            test_binding,
        )
        .expect("valid ingress");
        let CaptureIngressOutcome::Command(command) = outcome else {
            panic!("known event must yield a command")
        };
        assert_eq!(command.hook_command(), ProviderHookCommand::SessionStart);
        assert_eq!(command.provider_kind(), "test-provider");
        assert_eq!(command.provider_source(), "test-source");
        assert_eq!(command.event_kind(), LifecycleEventKind::SessionStart);
        assert_ne!(
            command.event_id(),
            command.event.event_id(),
            "a callback without native replay identity must not expose the public UUIDv5 derivation"
        );
        assert_eq!(
            command.identity_scheme(),
            LifecycleIdentityScheme::FallbackActionHmacV1,
            "a callback without a native replay field must identify its action-HMAC form"
        );
        assert!(command.dedup_key.is_none());
        assert!(command.deadline().is_none());
    }

    #[test]
    fn deadline_samples_persisted_clock_before_monotonic_clock() {
        let wall_clock_was_sampled = Cell::new(false);
        let wall_clock = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let monotonic_clock = Instant::now();
        let deadline = CaptureDeadline::from_budget_millis_with_clocks(
            250,
            || {
                wall_clock_was_sampled.set(true);
                wall_clock
            },
            || {
                assert!(
                    wall_clock_was_sampled.get(),
                    "the persisted SQLite deadline must be sampled before the monotonic budget"
                );
                monotonic_clock
            },
        )
        .expect("construct paired capture deadline");

        assert_eq!(deadline.absolute_millis(), 1_700_000_000_250);
        assert_eq!(
            deadline.monotonic(),
            monotonic_clock + Duration::from_millis(250)
        );
    }

    /// A no-native callback deliberately has no replay receipt, but its
    /// action UUID is still repository-keyed. The public lifecycle UUIDv5
    /// algorithm must not be usable to reproduce the identifier that reaches
    /// catalog action keys or hook telemetry.
    #[test]
    fn fallback_event_id_is_keyed_and_not_public_uuidv5() {
        let raw_session_id = "fallback-native-session-sentinel";
        let payload = serde_json::json!({
            "hook_event_name": "known",
            "session_id": raw_session_id,
            "cwd": "/workspace",
        })
        .to_string()
        .into_bytes();
        let envelope: SessionHookEnvelope =
            serde_json::from_slice(&payload).expect("parse fixed test envelope");
        let event = TestProvider
            .parse_hook_event(&envelope.hook_event_name, &envelope)
            .expect("lower fixed test event");
        let public_uuid_v5 = event.event_id();
        let preimage = event_identity_preimage(&TestProvider, &envelope, &event);
        let (expected_opaque, commitment) =
            opaque_event_identity_from_preimage(&[0x11; 32], &preimage);
        assert!(
            commitment.starts_with("capture-event-v1:"),
            "fallback identity must use a domain-separated opaque HMAC commitment"
        );

        let outcome = CaptureIngressCommand::from_payload(
            &payload,
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            &TestProvider,
            None,
            test_binding,
        )
        .expect("valid fallback ingress");
        let CaptureIngressOutcome::Command(command) = outcome else {
            panic!("known event must yield a command")
        };
        assert_eq!(command.event_id(), expected_opaque);
        assert_ne!(
            command.event_id(),
            public_uuid_v5,
            "a raw session/timestamp/kind tuple must not reproduce the persisted action UUID"
        );
        assert!(
            command.dedup_key.is_none(),
            "lack of native identity must remain receipt-free rather than becoming fallback replay dedup"
        );
        assert_eq!(
            command.identity_scheme(),
            LifecycleIdentityScheme::FallbackActionHmacV1
        );
    }

    #[test]
    fn unknown_provider_event_is_an_explicit_safe_noop() {
        let outcome = CaptureIngressCommand::from_payload(
            &payload("future-event"),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            &TestProvider,
            None,
            |_| panic!("unknown events must not request an ingress binding"),
        )
        .expect("unknown event is not an error");
        assert!(matches!(
            outcome,
            CaptureIngressOutcome::UnknownEvent {
                frame_bytes: _,
                event_name_len: 12,
            }
        ));
    }

    #[test]
    fn malformed_payload_keeps_stable_invalid_classification() {
        let error = match CaptureIngressCommand::from_payload(
            b"not json",
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            &TestProvider,
            None,
            |_| panic!("malformed events must not request an ingress binding"),
        ) {
            Ok(_) => panic!("malformed JSON must fail"),
            Err(error) => error,
        };
        assert!(error.chain().any(|cause| cause.is::<HookEnvelopeInvalid>()));
    }

    #[test]
    fn expired_nonterminal_payload_never_binds_a_runtime_scope() {
        let bound = Cell::new(false);
        let error = match CaptureIngressCommand::from_payload(
            &payload("known"),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            &TestProvider,
            Some(CaptureDeadline::from_parts(
                Instant::now() - Duration::from_millis(1),
                1_700_000_000_123,
            )),
            |_| {
                bound.set(true);
                test_binding(&CaptureScopeInput {
                    reported_cwd: "/workspace".to_string(),
                    event_identity_preimage: [0; CAPTURE_DEDUP_PREIMAGE_BYTES],
                    dedup_preimage: None,
                })
            },
        ) {
            Ok(_) => panic!("expired nonterminal ingress must stop before scope binding"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("deadline expired before binding hook runtime scope")
        );
        assert!(
            !bound.get(),
            "the binding creates repository key material and must not run after expiry"
        );
    }

    /// Built-in providers must not mint a session-scoped replay key for a
    /// repeatable callback that lacks a provider-unique native ID. Two empty
    /// `Stop` envelopes in one Claude session can be two real turns; treating
    /// the latter as a retry would silently lose a checkpoint.
    #[test]
    fn repeatable_builtin_boundary_without_native_id_has_no_replay_key() {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "safe-session-id",
            "cwd": "/workspace",
        })
        .to_string()
        .into_bytes();
        let command = |payload: &[u8]| {
            let outcome = CaptureIngressCommand::from_payload(
                payload,
                ProviderHookCommand::Stop,
                LifecycleEventKind::TurnEnd,
                crate::internal::ai::hooks::claude_provider(),
                None,
                test_binding,
            )?;
            let CaptureIngressOutcome::Command(command) = outcome else {
                panic!("known Stop must form a command")
            };
            Ok::<_, anyhow::Error>(command)
        };
        let first = command(&payload).expect("first Stop ingress");
        let second = command(&payload).expect("second Stop ingress");
        assert!(first.dedup_key.is_none());
        assert!(second.dedup_key.is_none());
    }

    #[test]
    fn tool_callbacks_prefer_the_tool_identity_over_a_shared_turn_identity() {
        let command = |tool_use_id: &str| {
            let payload = serde_json::json!({
                "hook_event_name": "tool",
                "session_id": "safe-session-id",
                "cwd": "/workspace",
                "turn_id": "turn-one",
                "tool_use_id": tool_use_id,
            })
            .to_string()
            .into_bytes();
            let outcome = CaptureIngressCommand::from_payload(
                &payload,
                ProviderHookCommand::ToolUse,
                LifecycleEventKind::ToolUse,
                &TestProvider,
                None,
                test_binding,
            )?;
            let CaptureIngressOutcome::Command(command) = outcome else {
                panic!("recognized tool callback must form a command")
            };
            let identity_scheme = command.identity_scheme();
            Ok::<_, anyhow::Error>((command.event_id, command.dedup_key, identity_scheme))
        };

        let first = command("tool-one").expect("first tool callback");
        let retry = command("tool-one").expect("tool retry");
        let second = command("tool-two").expect("second tool callback");
        assert_eq!(first, retry, "an exact tool retry must be replayable");
        assert_ne!(
            first, second,
            "two tools in one turn must not collapse to one receipt"
        );
        assert_eq!(
            first.2,
            LifecycleIdentityScheme::NativeReplayHmacV2,
            "a provider-native tool identity must remain distinguishable from the fallback action HMAC"
        );
    }

    #[test]
    fn opaque_helper_identity_rejects_an_event_id_detached_from_its_receipt_key() {
        let identity =
            opaque_dedup_identity_from_preimage(&[0x44; 32], &[0x55; CAPTURE_DEDUP_PREIMAGE_BYTES]);
        let (_event_id, key) = identity.into_wire_parts();
        let error = match OpaqueDedupIdentity::from_wire_parts(Uuid::nil(), key) {
            Ok(_) => panic!("a helper response must not decouple event and receipt identities"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("mismatched opaque dedup identity"),
            "the parent must reject a response whose catalog event ID is not derived from its ring key"
        );
    }

    #[test]
    fn opaque_event_helper_identity_rejects_a_detached_event_uuid() {
        let (event_id, commitment) =
            opaque_event_identity_from_preimage(&[0x44; 32], &[0x55; CAPTURE_DEDUP_PREIMAGE_BYTES]);
        assert_eq!(
            opaque_event_id_from_wire_parts(event_id, commitment).expect("valid opaque event wire"),
            event_id
        );
        let error = opaque_event_id_from_wire_parts(
            Uuid::nil(),
            format!("capture-event-v1:{}", "00".repeat(32)),
        )
        .expect_err("a helper response must not detach the opaque event UUID from its HMAC proof");
        assert!(
            error
                .to_string()
                .contains("mismatched opaque event identity"),
            "unexpected opaque event validation error: {error:#}"
        );
    }

    struct UnlistedProvider;

    impl crate::internal::ai::hooks::provider::HookProviderIdentity for UnlistedProvider {
        fn agent_kind(&self) -> crate::internal::ai::observed_agents::AgentKind {
            crate::internal::ai::observed_agents::AgentKind::Codex
        }
    }

    impl HookProvider for UnlistedProvider {
        fn provider_name(&self) -> &'static str {
            "unlisted-test-provider"
        }

        fn source_name(&self) -> &'static str {
            "unlisted-test-source"
        }

        fn supported_commands(&self) -> &'static [ProviderHookCommand] {
            &[ProviderHookCommand::SessionStart]
        }

        fn parse_hook_event(
            &self,
            hook_event_name: &str,
            _envelope: &SessionHookEnvelope,
        ) -> Result<LifecycleEvent> {
            bail!("parser must not receive unknown hook event '{hook_event_name}'")
        }

        fn dedup_identity_keys(&self) -> &'static [&'static str] {
            &["event_id"]
        }

        fn install_hooks(
            &self,
            _options: &crate::internal::ai::hooks::provider::ProviderInstallOptions,
        ) -> Result<()> {
            Ok(())
        }

        fn uninstall_hooks(&self) -> Result<()> {
            Ok(())
        }

        fn hooks_are_installed(&self) -> Result<bool> {
            Ok(false)
        }
    }

    #[test]
    fn providers_without_a_name_table_fail_safe_as_unknown_events() {
        let outcome = CaptureIngressCommand::from_payload(
            &payload("future-provider-event"),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            &UnlistedProvider,
            None,
            |_| panic!("unrecognized events must not bind runtime state"),
        )
        .expect("unknown provider event must be an explicit safe no-op");
        assert!(matches!(
            outcome,
            CaptureIngressOutcome::UnknownEvent { .. }
        ));
    }
}
