//! Killable capture scope-binding helper.
//!
//! Binds a provider-reported cwd to the invocation's exact worktree and its
//! repository-private replay identity. Managed hooks run the scope/key work in
//! a fresh, capped, killable helper process whose wire protocol flushes one
//! `U`/`N`/`T` scope-proof byte before any replay-key I/O: a timeout or EOF
//! before that proof, `U`, and `N` (no Libra repository at all) stay advisory;
//! after `T` the failure is a trusted terminal one.
//! The repository key never reaches the wire, argv, diagnostics or tracing;
//! the helper response carries only opaque HMAC identities or, for a trusted
//! failure of the active repository itself, one closed, content-free
//! [`ActiveRepositoryFailureClass`].

#[cfg(unix)]
use std::process::Stdio;
#[cfg(any(unix, test))]
use std::time::Instant;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[cfg(unix)]
use crate::internal::ai::authorized_read::{
    CancellationSafeChild, configure_private_helper_process_group, helper_program,
    read_async_strictly_bounded,
};
#[cfg(unix)]
use crate::internal::ai::capture::key::{
    ensure_scope_binding_mutation_deadline, load_capture_dedup_secret_with_mutation_deadline,
};
use crate::internal::ai::capture::{
    ingress::{
        CaptureIngressBinding, CaptureRuntimeScope, CaptureScopeInput, HookEnvelopeInvalid,
        OpaqueDedupIdentity, opaque_dedup_identity_from_preimage, opaque_event_id_from_wire_parts,
        opaque_event_identity_from_preimage,
    },
    key::CaptureDedupUnsupportedPlatform,
    live::HookExecutionDeadline,
};

#[cfg(test)]
mod test_support {
    use std::{
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use anyhow::{Context, Result, bail};

    use super::HookExecutionDeadline;

    tokio::task_local! {
        static SCOPE_BINDING_HELPER_READY: Option<(PathBuf, Duration)>;
    }

    /// Test-only handshake for a helper fixture that writes its PID before it
    /// begins blocking. Production never delays or re-anchors a deadline;
    /// this lets the regression prove a short deadline after the child has
    /// actually started rather than relying on scheduler luck during spawn.
    pub(crate) async fn with_scope_binding_helper_ready<F>(
        pid_file: PathBuf,
        post_ready_budget: Duration,
        future: F,
    ) -> F::Output
    where
        F: std::future::Future,
    {
        SCOPE_BINDING_HELPER_READY
            .scope(Some((pid_file, post_ready_budget)), future)
            .await
    }

    pub(crate) async fn deadline_after_scope_binding_helper_ready(
        initial_deadline: HookExecutionDeadline,
    ) -> Result<HookExecutionDeadline> {
        let Ok(Some((pid_file, post_ready_budget))) =
            SCOPE_BINDING_HELPER_READY.try_with(Clone::clone)
        else {
            return Ok(initial_deadline);
        };
        loop {
            if pid_file.is_file() {
                let monotonic = Instant::now()
                    .checked_add(post_ready_budget)
                    .context("scope-helper test deadline exceeds monotonic clock range")?;
                let budget_millis = i64::try_from(post_ready_budget.as_millis())
                    .context("scope-helper test deadline exceeds persistent range")?;
                let now_millis = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("scope-helper test clock precedes Unix epoch")?
                    .as_millis();
                let now_millis = i64::try_from(now_millis)
                    .context("scope-helper test clock exceeds persistent range")?;
                let absolute_millis = now_millis
                    .checked_add(budget_millis)
                    .context("scope-helper test deadline exceeds persistent range")?;
                return Ok(HookExecutionDeadline {
                    monotonic,
                    absolute_millis,
                });
            }
            if Instant::now() >= initial_deadline.monotonic {
                bail!(
                    "scope-helper test fixture did not report readiness before its startup deadline"
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Exact fixed argument accepted by the private, killable scope-binding
/// helper before normal CLI initialization.
pub const CAPTURE_SCOPE_BINDING_HELPER_ARG: &str = "--libra-internal-capture-scope-binding-helper";
/// The request has only one already-ingress-bounded cwd claim. The extra room
/// accounts for JSON escaping without accepting an unbounded helper frame.
pub const CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP: u64 =
    (crate::internal::ai::capture::ingress::MAX_REPORTED_CWD_BYTES as u64 * 6) + 1024;
/// Response contains only one verified UTF-8 cwd, two lossless paths, and an
/// opaque fixed-size HMAC-derived replay identity. Keep it bounded before
/// deserializing.
pub const CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP: u64 = 64 * 1024;
const CAPTURE_SCOPE_BINDING_HELPER_RESPONSE_CAP: u64 =
    CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP.saturating_sub(1);
const MAX_SCOPE_BINDING_WIRE_PATH_BYTES: usize = 16 * 1024;
const MAX_SCOPE_BINDING_VERIFIED_CWD_BYTES: usize = 16 * 1024;
/// The helper flushes exactly one classification proof before any key I/O or
/// final response. `U` means no active scope was proven; `N` means the
/// invocation is not inside any Libra repository (also unverified); `T` means
/// either an active-scope infrastructure failure was established or the
/// scope/key boundary was reached. This byte is deliberately not JSON so the
/// parent can observe it without waiting for a possibly blocked final response.
const SCOPE_BINDING_PHASE_UNVERIFIED: u8 = b'U';
const SCOPE_BINDING_PHASE_TRUSTED: u8 = b'T';
/// `N`: repository discovery from the helper's cwd found no Libra repository.
/// No active scope exists, so this is advisory exactly like `U`; it stays a
/// distinct byte only so the parent can keep the shipped, path-free
/// repository-not-found contract for hook surfaces that report it.
const SCOPE_BINDING_PHASE_NO_REPOSITORY: u8 = b'N';

/// The worktree identity has already been verified, but constructing the
/// repository-private replay identity failed. This is not an untrusted-frame
/// advisory: a terminal callback has crossed the scope boundary and must not
/// be silently acknowledged without durable recovery evidence.
#[derive(Debug, Error)]
#[error("trusted hook scope could not establish a capture replay identity: {source}")]
pub(crate) struct HookTrustedScopeBindingFailure {
    #[source]
    source: anyhow::Error,
}

/// The helper could not establish whether the provider claim belonged to the
/// active worktree. This deliberately remains distinct from a trusted scope
/// failure: without a returned binding/result, a terminal callback has no
/// identity from which it can safely write a recovery receipt.
#[derive(Debug, Error)]
#[error("hook callback ended before trusted scope binding: {source}")]
struct HookUnverifiedScopeBinding {
    #[source]
    source: anyhow::Error,
}

/// The hook was invoked from a working directory that is not inside any Libra
/// repository. There is no active scope to bind, so this is never a trusted
/// terminal boundary: it is carried only inside [`HookUnverifiedScopeBinding`]
/// and lets command adapters restore the fixed repository-not-found contract.
/// A damaged active repository (unreadable `commondir`, broken worktree
/// marker) is a different, trusted outcome and never produces this error.
#[derive(Debug, Error)]
#[error("hook callback was invoked outside a Libra repository")]
pub(crate) struct HookNoActiveRepository;

/// Whether a capture error says the callback ran outside any Libra
/// repository. The classification is typed, never derived from message text.
pub(crate) fn is_no_active_repository_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<HookNoActiveRepository>())
}

/// Closed, content-free class of a failure of the hook's ACTIVE repository.
///
/// Before hook dispatch became lazy, the generic CLI repository preflight
/// reported these with their own stable codes and remedies (a detached,
/// migrating or corrupt linked worktree, a missing or unopenable repository
/// database, an unreadable or unsupported `core.objectformat`). Command
/// adapters restore that public contract from this typed class only; it
/// never carries a path, a provider field or an underlying error message,
/// so it is also the only repository detail that may cross the helper wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActiveRepositoryFailureClass {
    /// The invocation is inside a repository whose storage cannot be
    /// resolved (detached, migrating or corrupt linked worktree, or an
    /// unreadable invocation directory).
    StorageUnresolved,
    /// Repository storage resolved, but its database file is missing.
    DatabaseMissing,
    /// The repository database exists but could not be opened.
    DatabaseUnavailable,
    /// `core.objectformat` could not be read from the repository database.
    ObjectFormatUnreadable,
    /// `core.objectformat` names an object format this build does not support.
    ObjectFormatUnsupported,
}

impl ActiveRepositoryFailureClass {
    /// Fixed internal description; never rendered with any other detail.
    const fn description(self) -> &'static str {
        match self {
            Self::StorageUnresolved => "unable to resolve the active hook worktree",
            Self::DatabaseMissing => "the active repository database is missing",
            Self::DatabaseUnavailable => "the active repository database could not be opened",
            Self::ObjectFormatUnreadable => "the active repository object format could not be read",
            Self::ObjectFormatUnsupported => "the active repository object format is not supported",
        }
    }

    /// Classify a failed repository-database open by its I/O kind alone. A
    /// missing file is [`Self::DatabaseMissing`]; every other failure
    /// (permissions, corruption, a schema from a newer Libra, a busy lock,
    /// a non-UTF-8 path) is [`Self::DatabaseUnavailable`].
    pub(crate) fn for_database_open(kind: std::io::ErrorKind) -> Self {
        if kind == std::io::ErrorKind::NotFound {
            Self::DatabaseMissing
        } else {
            Self::DatabaseUnavailable
        }
    }
}

/// A trusted active-repository failure carrying only its closed class.
///
/// Scope resolution wraps it in [`HookTrustedScopeBindingFailure`] (so the
/// terminal/advisory policy is unchanged); post-binding database and object
/// format setup returns it directly, where the runtime already treats every
/// failure as trusted.
#[derive(Debug, Error)]
#[error("{}", .class.description())]
pub(crate) struct HookActiveRepositoryFailure {
    class: ActiveRepositoryFailureClass,
}

/// Construct the typed active-repository failure for one closed class.
pub(crate) fn active_repository_failure(class: ActiveRepositoryFailureClass) -> anyhow::Error {
    HookActiveRepositoryFailure { class }.into()
}

/// The closed active-repository class carried anywhere in a capture error
/// chain, if any. Like [`is_no_active_repository_error`], this is typed and
/// never derived from message text.
pub(crate) fn active_repository_failure_class(
    error: &anyhow::Error,
) -> Option<ActiveRepositoryFailureClass> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<HookActiveRepositoryFailure>()
            .map(|failure| failure.class)
    })
}

/// Whether a capture error is the one fixed platform-capability result that
/// may be rendered to an external hook host. All other helper failures can
/// contain local filesystem detail and must keep their generic classification.
pub(crate) fn is_capture_unsupported_platform_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<CaptureDedupUnsupportedPlatform>())
}

/// Bind a provider-reported cwd to this invocation's exact worktree identity.
///
/// Prefix containment alone is insufficient: an independent nested repository
/// or a sibling linked worktree can both be physically beneath/near the active
/// tree while owning a different local gitdir. Resolve both paths once and
/// require their local gitdir, common storage, root, and logical scope to
/// agree. Every filesystem failure maps to a stable generic validation error
/// so an attacker-controlled path never reaches stderr or tracing.
fn bind_capture_scope_cwd(scope_input: &CaptureScopeInput) -> Result<CaptureIngressBinding> {
    bind_capture_scope_cwd_with_mutation_deadline(scope_input, None)
}

/// Resolve a reported worktree and create/load its replay key. The optional
/// wall-clock deadline comes exclusively from the parent-created managed
/// capture deadline; it is rechecked immediately before the only local key
/// mutation, while the parent monotonic deadline remains liveness authority.
fn bind_capture_scope_cwd_with_mutation_deadline(
    scope_input: &CaptureScopeInput,
    mutation_deadline_millis: Option<i64>,
) -> Result<CaptureIngressBinding> {
    let verified_scope = resolve_capture_scope_cwd(scope_input)?;
    bind_verified_capture_scope(verified_scope, mutation_deadline_millis)
}

/// Scope facts established before the repository-private replay key is read
/// or created. Keeping this proof separate lets the helper publish its
/// trusted phase before it crosses the key I/O boundary.
struct VerifiedCaptureScope {
    verified_cwd: String,
    storage_path: PathBuf,
    worktree_root: PathBuf,
}

/// The fixed trusted failure for an active hook worktree that exists but
/// cannot be resolved (unreadable cwd, damaged `commondir`/worktree marker).
/// It carries the closed [`ActiveRepositoryFailureClass::StorageUnresolved`]
/// class so fail-closed surfaces can restore the shipped `LBR-REPO-003`
/// contract without rendering the underlying (path-bearing) discovery error.
fn active_worktree_unresolved() -> anyhow::Error {
    trusted_active_repository_failure(ActiveRepositoryFailureClass::StorageUnresolved)
}

/// A trusted scope-binding failure whose only detail is a closed
/// active-repository class. Shared by the in-process resolver and the
/// parent's decoding of the helper's classified trusted response.
pub(crate) fn trusted_active_repository_failure(
    class: ActiveRepositoryFailureClass,
) -> anyhow::Error {
    HookTrustedScopeBindingFailure {
        source: active_repository_failure(class),
    }
    .into()
}

/// Classify read-only repository discovery for the hook invocation. This is
/// the single platform-neutral mapping shared by the scope resolver and the
/// non-Unix capability path: discovery `Ok(None)` (no Libra repository at
/// all) is the unverified [`HookNoActiveRepository`], while a discovery error
/// is damage to an existing active worktree and stays trusted.
fn classify_active_request_scope(
    discovered: std::io::Result<Option<crate::internal::worktree_scope::RequestScope>>,
) -> Result<crate::internal::worktree_scope::RequestScope> {
    match discovered {
        Ok(Some(active)) => Ok(active),
        Ok(None) => Err(no_active_repository_scope_binding_failure()),
        Err(_) => Err(active_worktree_unresolved()),
    }
}

/// Resolve only the invocation's active repository from `workdir` (the
/// caller passes `std::env::current_dir()`), with no reported-path or key
/// I/O. Non-Unix managed capture uses this before its capability result.
#[cfg(any(not(unix), test))]
fn resolve_active_invocation_scope(
    workdir: std::io::Result<PathBuf>,
) -> Result<crate::internal::worktree_scope::RequestScope> {
    let workdir = workdir.map_err(|_| active_worktree_unresolved())?;
    classify_active_request_scope(crate::internal::worktree_scope::RequestScope::try_resolve(
        workdir,
    ))
}

/// The managed non-Unix scope-binding result. Non-Unix never spawns the
/// replay-key helper, but an invocation outside every Libra repository has
/// no scope for that capability to protect: it keeps the same unverified
/// no-repository outcome as Unix (Codex acknowledges it, including
/// `SessionEnd`; fail-closed surfaces report `LBR-REPO-001`). An invocation
/// that reaches a repository, or whose discovery fails, keeps the fixed
/// unsupported-platform result unchanged.
#[cfg(any(not(unix), test))]
fn non_unix_scope_binding_failure(
    active: Result<crate::internal::worktree_scope::RequestScope>,
) -> anyhow::Error {
    match active {
        Err(error) if is_no_active_repository_error(&error) => {
            no_active_repository_scope_binding_failure()
        }
        Ok(_) | Err(_) => unsupported_platform_scope_binding_failure(),
    }
}

/// Resolve the untrusted reported path and prove that it denotes the active
/// worktree. Errors derived from the reported path stay envelope-invalid.
/// An invocation outside any Libra repository (discovery `NotFound`) has no
/// active scope and is reported as the unverified [`HookNoActiveRepository`];
/// only damage to an existing active worktree (or an unreadable cwd) is a
/// trusted infrastructure failure. No key or other mutable state is touched.
fn resolve_capture_scope_cwd(scope_input: &CaptureScopeInput) -> Result<VerifiedCaptureScope> {
    let reported_cwd = scope_input.reported_cwd.as_str();
    let reported_path = Path::new(reported_cwd);
    if !reported_path.is_absolute() {
        return Err(HookEnvelopeInvalid(
            "hook cwd must be an absolute path within the active worktree".to_string(),
        )
        .into());
    }
    // Canonicalizing the provider-controlled spelling is the sole
    // pre-proof filesystem step. A deadline or failure here remains advisory;
    // no trusted identity has been proven yet.
    let canonical_reported = reported_path.canonicalize().map_err(|_| {
        HookEnvelopeInvalid("hook cwd must resolve within the active worktree".to_string())
    })?;
    // Resolve the actual invocation before consulting the reported identity.
    // A damaged active `.libra`/commondir is trusted local infrastructure, not
    // evidence that an otherwise valid provider frame may be acknowledged.
    // A cwd outside every Libra repository (`Ok(None)`) has no active scope to
    // damage: it is unverified, never a trusted terminal boundary.
    let active_workdir = std::env::current_dir().map_err(|_| active_worktree_unresolved())?;
    let active = classify_active_request_scope(
        crate::internal::worktree_scope::RequestScope::try_resolve(active_workdir),
    )?;
    let canonical_root =
        active
            .worktree_root
            .canonicalize()
            .map_err(|_| HookTrustedScopeBindingFailure {
                source: anyhow!("unable to canonicalize the active hook worktree"),
            })?;
    let active_gitdir =
        active
            .gitdir
            .canonicalize()
            .map_err(|_| HookTrustedScopeBindingFailure {
                source: anyhow!("unable to canonicalize the active hook worktree"),
            })?;
    let active_storage =
        active
            .storage
            .canonicalize()
            .map_err(|_| HookTrustedScopeBindingFailure {
                source: anyhow!("unable to canonicalize the active hook worktree"),
            })?;

    if !canonical_reported.starts_with(&canonical_root) {
        return Err(
            HookEnvelopeInvalid("hook cwd is outside the active worktree".to_string()).into(),
        );
    }
    let reported =
        crate::internal::worktree_scope::RequestScope::try_resolve(canonical_reported.clone())
            .map_err(|_| {
                HookEnvelopeInvalid("hook cwd does not resolve to the active worktree".to_string())
            })?
            .ok_or_else(|| {
                HookEnvelopeInvalid("hook cwd does not resolve to the active worktree".to_string())
            })?;
    let reported_gitdir = reported.gitdir.canonicalize().map_err(|_| {
        HookEnvelopeInvalid("hook cwd does not resolve to the active worktree".to_string())
    })?;
    let reported_storage = reported.storage.canonicalize().map_err(|_| {
        HookEnvelopeInvalid("hook cwd does not resolve to the active worktree".to_string())
    })?;
    let reported_root = reported.worktree_root.canonicalize().map_err(|_| {
        HookEnvelopeInvalid("hook cwd does not resolve to the active worktree".to_string())
    })?;
    if active.scope != reported.scope
        || active_gitdir != reported_gitdir
        || active_storage != reported_storage
        || canonical_root != reported_root
    {
        return Err(HookEnvelopeInvalid(
            "hook cwd does not resolve to the active worktree".to_string(),
        )
        .into());
    }
    let verified_cwd = canonical_reported.to_str().ok_or_else(|| {
        HookEnvelopeInvalid("hook cwd is not valid UTF-8 after canonicalization".to_string())
    })?;
    Ok(VerifiedCaptureScope {
        verified_cwd: verified_cwd.to_string(),
        storage_path: active_storage,
        worktree_root: canonical_root,
    })
}

/// Cross the replay-key boundary only after [`resolve_capture_scope_cwd`] has
/// proven the invocation scope. The child checks its wall-clock mutation gate
/// here; the parent remains responsible for the host-visible monotonic timer.
fn bind_verified_capture_scope(
    verified_scope: VerifiedCaptureScope,
    mutation_deadline_millis: Option<i64>,
) -> Result<CaptureIngressBinding> {
    // A non-Unix build has no descriptor-relative, no-replace key
    // publication implementation. Return that fixed capability result before
    // consulting a deadline: no key mutation is possible on this path, and
    // suppressing the capability behind an expired budget would turn Codex's
    // documented path-free remediation back into a silent advisory.
    #[cfg(not(unix))]
    {
        let _ = (verified_scope, mutation_deadline_millis);
        Err(CaptureDedupUnsupportedPlatform.into())
    }

    #[cfg(unix)]
    {
        if let Some(deadline_millis) = mutation_deadline_millis {
            ensure_scope_binding_mutation_deadline(deadline_millis)?;
        }
        let dedup_secret = load_capture_dedup_secret_with_mutation_deadline(
            &verified_scope.storage_path,
            mutation_deadline_millis,
        )
        .map_err(|source| HookTrustedScopeBindingFailure { source })?;
        Ok(CaptureIngressBinding::new(
            verified_scope.verified_cwd,
            dedup_secret,
            CaptureRuntimeScope::new(verified_scope.storage_path, verified_scope.worktree_root),
        ))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeBindingHelperRequest {
    reported_cwd: String,
    /// Generated from the host-owned capture deadline before ingress. This is
    /// not a provider frame field and acts only as the child's mutation gate.
    deadline_millis: i64,
    /// Fixed SHA-256 commitment to the total lifecycle action identity. It is
    /// always present, and the helper never receives raw session/timestamp/
    /// event components.
    event_identity_preimage:
        [u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
    /// Fixed SHA-256 commitment built by ingress from all native replay
    /// components. The helper must never receive their raw spellings.
    dedup_preimage:
        Option<[u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES]>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum ScopeBindingHelperResponse {
    Bound {
        verified_cwd: String,
        /// Short-lived HMAC proof for the opaque action UUID. The parent
        /// validates then discards `event_key`; it is never a receipt key.
        opaque_event: ScopeBindingOpaqueEvent,
        /// A one-way HMAC commitment, not the repository key. It is safe for
        /// the parent to persist as a receipt identity.
        opaque_dedup: Option<ScopeBindingOpaqueDedup>,
        storage_path: ScopeBindingWirePath,
        worktree_root: ScopeBindingWirePath,
    },
    InvalidScope,
    TrustedBindingFailure,
    /// A trusted failure of the active repository itself, reduced to one
    /// closed, content-free class (never a path or an error message) so the
    /// parent can restore the shipped repository stable code.
    TrustedRepositoryFailure {
        reason: ActiveRepositoryFailureClass,
    },
    /// A fixed platform capability result. Together with the closed
    /// repository class above, this is the only helper failure detail safe
    /// to return across the process boundary.
    UnsupportedPlatform,
}

/// The only replay-related helper response shape. It carries a commitment
/// produced in the child and intentionally has no secret/key-material field.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeBindingOpaqueDedup {
    event_id: Uuid,
    dedup_key: String,
}

/// The helper's opaque action identity. `event_key` is an irreversible HMAC
/// commitment used solely to bind the UUID to helper output; it never enters
/// a catalog receipt, tracing field, error chain, or persisted metadata.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeBindingOpaqueEvent {
    event_id: Uuid,
    event_key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeBindingWirePath {
    #[cfg(unix)]
    #[serde(with = "scope_binding_wire_path_bytes")]
    bytes: Vec<u8>,
    #[cfg(windows)]
    wide: Vec<u16>,
    #[cfg(not(any(unix, windows)))]
    text: String,
}

#[cfg(unix)]
mod scope_binding_wire_path_bytes {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(encoded)
            .map_err(|_| D::Error::custom("invalid scope-binding path frame"))
    }
}

impl ScopeBindingWirePath {
    fn from_path(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let bytes = path.as_os_str().as_bytes();
            if bytes.is_empty()
                || bytes.len() > MAX_SCOPE_BINDING_WIRE_PATH_BYTES
                || bytes.contains(&0)
            {
                bail!("scope-binding helper path is outside the supported wire bounds");
            }
            Ok(Self {
                bytes: bytes.to_vec(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;

            let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
            if wide.is_empty()
                || wide.len() > MAX_SCOPE_BINDING_WIRE_PATH_BYTES
                || wide.contains(&0)
            {
                bail!("scope-binding helper path is outside the supported wire bounds");
            }
            Ok(Self { wide })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let text = path
                .to_str()
                .ok_or_else(|| anyhow!("scope-binding helper path is not valid platform text"))?;
            if text.is_empty()
                || text.len() > MAX_SCOPE_BINDING_WIRE_PATH_BYTES
                || text.contains('\0')
            {
                bail!("scope-binding helper path is outside the supported wire bounds");
            }
            Ok(Self {
                text: text.to_string(),
            })
        }
    }

    fn into_path_buf(self) -> Result<PathBuf> {
        #[cfg(unix)]
        let path = {
            use std::{ffi::OsString, os::unix::ffi::OsStringExt};

            if self.bytes.is_empty()
                || self.bytes.len() > MAX_SCOPE_BINDING_WIRE_PATH_BYTES
                || self.bytes.contains(&0)
            {
                bail!("scope-binding helper returned an invalid path frame");
            }
            PathBuf::from(OsString::from_vec(self.bytes))
        };
        #[cfg(windows)]
        let path = {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};

            if self.wide.is_empty()
                || self.wide.len() > MAX_SCOPE_BINDING_WIRE_PATH_BYTES
                || self.wide.contains(&0)
            {
                bail!("scope-binding helper returned an invalid path frame");
            }
            PathBuf::from(OsString::from_wide(&self.wide))
        };
        #[cfg(not(any(unix, windows)))]
        let path = {
            if self.text.is_empty()
                || self.text.len() > MAX_SCOPE_BINDING_WIRE_PATH_BYTES
                || self.text.contains('\0')
            {
                bail!("scope-binding helper returned an invalid path frame");
            }
            PathBuf::from(self.text)
        };
        if !path.is_absolute() {
            bail!("scope-binding helper returned a non-absolute path");
        }
        Ok(path)
    }
}

enum ScopeBindingHelperOutcome {
    Bound(CaptureIngressBinding),
    InvalidScope,
    TrustedBindingFailure,
    TrustedRepositoryFailure(ActiveRepositoryFailureClass),
    UnsupportedPlatform,
}

fn encode_scope_binding_helper_request(
    scope_input: &CaptureScopeInput,
    deadline: HookExecutionDeadline,
) -> Result<Vec<u8>> {
    if scope_input.reported_cwd.len()
        > crate::internal::ai::capture::ingress::MAX_REPORTED_CWD_BYTES
    {
        bail!("scope-binding helper request exceeds the cwd limit");
    }
    let frame = serde_json::to_vec(&ScopeBindingHelperRequest {
        reported_cwd: scope_input.reported_cwd.clone(),
        deadline_millis: deadline.absolute_millis,
        event_identity_preimage: scope_input.event_identity_preimage,
        dedup_preimage: scope_input.dedup_preimage,
    })
    .context("encode scope-binding helper request")?;
    if frame.len() as u64 > CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP {
        bail!("scope-binding helper request exceeds the frame limit");
    }
    Ok(frame)
}

fn scope_binding_response_from_binding(
    binding: CaptureIngressBinding,
    event_identity_preimage: [
        u8;
        crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES
    ],
    dedup_preimage: Option<
        [u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
    >,
) -> Result<ScopeBindingHelperResponse> {
    let (verified_cwd, dedup_secret, runtime_scope) = binding.into_scope_binding_parts()?;
    if verified_cwd.is_empty()
        || verified_cwd.len() > MAX_SCOPE_BINDING_VERIFIED_CWD_BYTES
        || !Path::new(&verified_cwd).is_absolute()
    {
        bail!("scope-binding helper verified cwd is outside the supported wire bounds");
    }
    let opaque_dedup = dedup_preimage.map(|preimage| {
        let (event_id, dedup_key) =
            opaque_dedup_identity_from_preimage(&dedup_secret, &preimage).into_wire_parts();
        ScopeBindingOpaqueDedup {
            event_id,
            dedup_key,
        }
    });
    let opaque_event = match opaque_dedup.as_ref() {
        Some(dedup) => ScopeBindingOpaqueEvent {
            event_id: dedup.event_id,
            event_key: dedup.dedup_key.clone(),
        },
        None => {
            let (event_id, event_key) =
                opaque_event_identity_from_preimage(&dedup_secret, &event_identity_preimage);
            ScopeBindingOpaqueEvent {
                event_id,
                event_key,
            }
        }
    };
    Ok(ScopeBindingHelperResponse::Bound {
        verified_cwd,
        opaque_event,
        opaque_dedup,
        storage_path: ScopeBindingWirePath::from_path(&runtime_scope.storage_path)?,
        worktree_root: ScopeBindingWirePath::from_path(&runtime_scope.worktree_root)?,
    })
}

fn decode_scope_binding_helper_response(input: &[u8]) -> Result<ScopeBindingHelperOutcome> {
    if input.len() as u64 > CAPTURE_SCOPE_BINDING_HELPER_RESPONSE_CAP {
        bail!("scope-binding helper response exceeds the frame limit");
    }
    let response: ScopeBindingHelperResponse = serde_json::from_slice(input)
        .map_err(|_| anyhow!("scope-binding helper returned an invalid response frame"))?;
    match response {
        ScopeBindingHelperResponse::InvalidScope => Ok(ScopeBindingHelperOutcome::InvalidScope),
        ScopeBindingHelperResponse::TrustedBindingFailure => {
            Ok(ScopeBindingHelperOutcome::TrustedBindingFailure)
        }
        ScopeBindingHelperResponse::TrustedRepositoryFailure { reason } => {
            Ok(ScopeBindingHelperOutcome::TrustedRepositoryFailure(reason))
        }
        ScopeBindingHelperResponse::UnsupportedPlatform => {
            Ok(ScopeBindingHelperOutcome::UnsupportedPlatform)
        }
        ScopeBindingHelperResponse::Bound {
            verified_cwd,
            opaque_event,
            opaque_dedup,
            storage_path,
            worktree_root,
        } => {
            if verified_cwd.is_empty()
                || verified_cwd.len() > MAX_SCOPE_BINDING_VERIFIED_CWD_BYTES
                || !Path::new(&verified_cwd).is_absolute()
            {
                bail!("scope-binding helper returned an invalid verified cwd");
            }
            let event_id =
                opaque_event_id_from_wire_parts(opaque_event.event_id, opaque_event.event_key)?;
            let opaque_dedup = opaque_dedup
                .map(|opaque| {
                    OpaqueDedupIdentity::from_wire_parts(opaque.event_id, opaque.dedup_key)
                })
                .transpose()?;
            if opaque_dedup
                .as_ref()
                .is_some_and(|dedup| dedup.event_id() != event_id)
            {
                bail!("scope-binding helper returned mismatched event and receipt identities");
            }
            let storage_path = storage_path.into_path_buf()?;
            let worktree_root = worktree_root.into_path_buf()?;
            Ok(ScopeBindingHelperOutcome::Bound(
                CaptureIngressBinding::from_opaque_identity(
                    verified_cwd,
                    event_id,
                    opaque_dedup,
                    CaptureRuntimeScope::new(storage_path, worktree_root),
                ),
            ))
        }
    }
}

/// Execute the scope/key part of managed capture in a fresh helper process.
///
/// The wire begins with one flushed phase byte, followed only for a trusted
/// phase by one bounded JSON response. It never puts the repository key on
/// the wire, in argv, diagnostics, or tracing; the response carries only a
/// one-way opaque HMAC identity. The phase is a proof protocol rather than a
/// spawn heuristic: parent timeout/EOF before a phase, `U`, and `N` (no Libra
/// repository) are advisory; after `T` it is a trusted terminal failure.
pub fn run_capture_scope_binding_helper(input: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    run_capture_scope_binding_helper_to_writer(input, &mut output)?;
    Ok(output)
}

/// Streaming form used only by the private binary entrypoint. A phase is
/// flushed before any operation that may block on replay-key I/O, so the
/// parent can classify a kill/EOF without awaiting the child process.
pub fn run_capture_scope_binding_helper_to_writer<W: Write>(
    input: &[u8],
    output: &mut W,
) -> Result<()> {
    if input.len() as u64 > CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP {
        bail!("scope-binding helper input exceeds the frame limit");
    }
    let request: ScopeBindingHelperRequest = match serde_json::from_slice(input) {
        Ok(request) => request,
        Err(_) => {
            write_scope_binding_phase(output, SCOPE_BINDING_PHASE_UNVERIFIED)?;
            return Ok(());
        }
    };
    if request.reported_cwd.len() > crate::internal::ai::capture::ingress::MAX_REPORTED_CWD_BYTES {
        write_scope_binding_phase(output, SCOPE_BINDING_PHASE_UNVERIFIED)?;
        return Ok(());
    }
    let ScopeBindingHelperRequest {
        reported_cwd,
        deadline_millis,
        event_identity_preimage,
        dedup_preimage,
    } = request;

    // A lexical/canonical reported-path reject remains unverified. The shared
    // resolver next establishes active `.libra` infrastructure before it
    // compares the reported identity: an active failure emits `T` plus a
    // trusted final result, a cwd outside every Libra repository emits `N`,
    // while a clean mismatch emits `U`. A successful exact match flushes `T`
    // before replay-key I/O.
    let verified_scope = match resolve_capture_scope_cwd(&CaptureScopeInput {
        reported_cwd,
        event_identity_preimage,
        dedup_preimage: None,
    }) {
        Ok(verified_scope) => verified_scope,
        Err(error) if error.chain().any(|cause| cause.is::<HookEnvelopeInvalid>()) => {
            write_scope_binding_phase(output, SCOPE_BINDING_PHASE_UNVERIFIED)?;
            return Ok(());
        }
        Err(error) if is_no_active_repository_error(&error) => {
            write_scope_binding_phase(output, SCOPE_BINDING_PHASE_NO_REPOSITORY)?;
            return Ok(());
        }
        Err(error) => {
            // Only the closed active-repository class crosses the wire; the
            // underlying discovery error (which names local paths) does not.
            let response = match active_repository_failure_class(&error) {
                Some(reason) => ScopeBindingHelperResponse::TrustedRepositoryFailure { reason },
                None => ScopeBindingHelperResponse::TrustedBindingFailure,
            };
            write_scope_binding_phase(output, SCOPE_BINDING_PHASE_TRUSTED)?;
            return write_scope_binding_response(output, response);
        }
    };

    write_scope_binding_phase(output, SCOPE_BINDING_PHASE_TRUSTED)?;
    // The child-side wall-clock check is only a second mutation gate. The
    // host liveness authority remains the parent's monotonic deadline.
    let response = scope_binding_response_for_binding_result(
        bind_verified_capture_scope(verified_scope, Some(deadline_millis)),
        event_identity_preimage,
        dedup_preimage,
    );
    write_scope_binding_response(output, response)
}

/// Translate only fixed, non-sensitive helper failures into a cross-process
/// result. Filesystem, key, and scope errors may carry local path details, so
/// they intentionally remain the generic trusted failure outcome.
fn scope_binding_response_for_binding_result(
    binding: Result<CaptureIngressBinding>,
    event_identity_preimage: [
        u8;
        crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES
    ],
    dedup_preimage: Option<
        [u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
    >,
) -> ScopeBindingHelperResponse {
    match binding {
        Ok(binding) => {
            scope_binding_response_from_binding(binding, event_identity_preimage, dedup_preimage)
                .unwrap_or(ScopeBindingHelperResponse::TrustedBindingFailure)
        }
        Err(error) if is_capture_unsupported_platform_error(&error) => {
            ScopeBindingHelperResponse::UnsupportedPlatform
        }
        Err(_) => ScopeBindingHelperResponse::TrustedBindingFailure,
    }
}

fn write_scope_binding_phase<W: Write>(output: &mut W, phase: u8) -> Result<()> {
    output
        .write_all(&[phase])
        .context("write capture scope-binding helper phase")?;
    output
        .flush()
        .context("flush capture scope-binding helper phase")
}

fn write_scope_binding_response<W: Write>(
    output: &mut W,
    response: ScopeBindingHelperResponse,
) -> Result<()> {
    let encoded = serde_json::to_vec(&response).context("encode scope-binding helper response")?;
    let total_len = u64::try_from(encoded.len())
        .context("capture scope-binding helper response exceeds the persistent range")?
        .saturating_add(1);
    if total_len > CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP {
        bail!("scope-binding helper response exceeds the frame limit");
    }
    output
        .write_all(&encoded)
        .context("write capture scope-binding helper response")?;
    output
        .flush()
        .context("flush capture scope-binding helper response")
}

pub(crate) fn trusted_scope_binding_failure(reason: &'static str) -> anyhow::Error {
    HookTrustedScopeBindingFailure {
        source: anyhow!(reason),
    }
    .into()
}

pub(crate) fn unsupported_platform_scope_binding_failure() -> anyhow::Error {
    HookTrustedScopeBindingFailure {
        source: CaptureDedupUnsupportedPlatform.into(),
    }
    .into()
}

pub(crate) fn unverified_scope_binding_failure(reason: &'static str) -> anyhow::Error {
    HookUnverifiedScopeBinding {
        source: anyhow!(reason),
    }
    .into()
}

/// The unverified outcome for a hook invoked outside any Libra repository.
/// Both the in-process resolver and the parent's `N` phase use this one
/// constructor so the advisory classification cannot diverge between them.
pub(crate) fn no_active_repository_scope_binding_failure() -> anyhow::Error {
    HookUnverifiedScopeBinding {
        source: HookNoActiveRepository.into(),
    }
    .into()
}

/// Result of the first, one-byte scope-proof read.  A ready proof wins even
/// at the deadline because Tokio polls the read future before observing its
/// timer; no ready byte leaves the callback without trustworthy scope proof.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
enum ScopeBindingPhaseRead {
    Byte(u8),
    EofOrIo,
    TimedOut,
}

#[cfg(test)]
async fn read_scope_binding_phase_until<R>(
    reader: &mut R,
    deadline: Instant,
) -> ScopeBindingPhaseRead
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let read = async {
        let mut phase = [0_u8; 1];
        reader.read_exact(&mut phase).await.map(|_| phase[0])
    };
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), read).await {
        Ok(Ok(phase)) => ScopeBindingPhaseRead::Byte(phase),
        Ok(Err(_)) => ScopeBindingPhaseRead::EofOrIo,
        Err(_) => ScopeBindingPhaseRead::TimedOut,
    }
}

pub(crate) async fn bind_capture_scope_cwd_bounded(
    scope_input: CaptureScopeInput,
    deadline: Option<HookExecutionDeadline>,
) -> Result<CaptureIngressBinding> {
    #[cfg(not(unix))]
    {
        let _ = (scope_input, deadline);
        // The scope helper carries a trusted replay-key boundary. Without a
        // private process group there is no way to contain a forked helper
        // descendant that retains its response pipe, so do not spawn it.
        // Read-only repository discovery still runs first (as the shipped
        // CLI preflight did on every platform): a hook outside every Libra
        // repository stays unverified, never the trusted capability failure.
        return Err(non_unix_scope_binding_failure(
            resolve_active_invocation_scope(std::env::current_dir()),
        ));
    }

    #[cfg(unix)]
    {
        let Some(deadline) = deadline else {
            return bind_capture_scope_cwd(&scope_input);
        };
        if Instant::now() >= deadline.monotonic {
            return Err(unverified_scope_binding_failure(
                "capture scope binding exceeded its managed deadline before scope verification",
            ));
        }
        let program = helper_program().ok_or_else(|| {
            unverified_scope_binding_failure("killable capture scope-binding helper is unavailable")
        })?;
        let mut command = tokio::process::Command::new(program);
        command
            .arg(CAPTURE_SCOPE_BINDING_HELPER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        let spawned = command.spawn().map_err(|_| {
            unverified_scope_binding_failure("start killable capture scope-binding helper")
        })?;
        let mut child = CancellationSafeChild::new_process_group(spawned);
        let mut stdin = match child.child_mut().and_then(|child| child.stdin.take()) {
            Some(stdin) => stdin,
            None => {
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "capture scope-binding helper has no stdin pipe",
                ));
            }
        };
        let mut stdout = match child.child_mut().and_then(|child| child.stdout.take()) {
            Some(stdout) => stdout,
            None => {
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "capture scope-binding helper has no stdout pipe",
                ));
            }
        };
        #[cfg(test)]
        let deadline = test_support::deadline_after_scope_binding_helper_ready(deadline)
            .await
            .map_err(|_| {
                unverified_scope_binding_failure(
                    "scope-binding helper test fixture did not become ready",
                )
            })?;
        let frame = match encode_scope_binding_helper_request(&scope_input, deadline) {
            Ok(frame) => frame,
            Err(_) => {
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "unable to encode capture scope-binding request",
                ));
            }
        };
        if Instant::now() >= deadline.monotonic {
            child.terminate_and_reap();
            return Err(unverified_scope_binding_failure(
                "capture scope binding exceeded its managed deadline",
            ));
        }
        // Begin draining before writing the request. A helper may emit its
        // phase/response before consuming stdin; delaying this task could let a
        // full pipe deadlock the parent. It sends the first phase through a
        // one-shot channel while continuing to drain to EOF in the background.
        let (phase_tx, mut phase_rx) = tokio::sync::oneshot::channel();
        let mut stdout_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;

            let mut phase = [0_u8; 1];
            if stdout.read_exact(&mut phase).await.is_err() {
                let _ = phase_tx.send(Err(()));
                return Err(std::io::Error::other(
                    "capture scope-binding helper ended before scope proof",
                ));
            }
            let _ = phase_tx.send(Ok(phase[0]));
            read_async_strictly_bounded(&mut stdout, CAPTURE_SCOPE_BINDING_HELPER_RESPONSE_CAP)
                .await
        });
        child.register_abort_on_cancel(&stdout_task);
        let send_result =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic), async {
                use tokio::io::AsyncWriteExt;

                stdin.write_all(&frame).await?;
                stdin.shutdown().await
            })
            .await;
        match send_result {
            Ok(Ok(())) => drop(stdin),
            Ok(Err(_)) => {
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "send capture scope-binding helper request",
                ));
            }
            Err(_) => {
                drop(stdin);
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "capture scope binding exceeded its managed deadline",
                ));
            }
        }
        // The helper must flush a classification proof before the response/key
        // boundary. Do not infer trust merely because spawn/write succeeded: a
        // deadline or EOF before this byte remains advisory and kills the child.
        let phase = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic),
            &mut phase_rx,
        )
        .await
        {
            Ok(Ok(Ok(phase))) => phase,
            Ok(Ok(Err(()))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "capture scope-binding helper ended before scope proof",
                ));
            }
            Err(_) => {
                child.terminate_and_reap();
                return Err(unverified_scope_binding_failure(
                    "capture scope binding exceeded its managed deadline before scope proof",
                ));
            }
        };
        if phase == SCOPE_BINDING_PHASE_UNVERIFIED {
            child.terminate_and_reap();
            return Err(HookEnvelopeInvalid(
                "hook cwd does not resolve to the active worktree".to_string(),
            )
            .into());
        }
        if phase == SCOPE_BINDING_PHASE_NO_REPOSITORY {
            // No Libra repository at the invocation cwd: no scope exists, so
            // this stays advisory and never becomes a trusted terminal failure.
            child.terminate_and_reap();
            return Err(no_active_repository_scope_binding_failure());
        }
        if phase != SCOPE_BINDING_PHASE_TRUSTED {
            child.terminate_and_reap();
            return Err(unverified_scope_binding_failure(
                "capture scope-binding helper returned an invalid scope proof",
            ));
        }
        // Keep the leader unreaped until inherited stdout reaches EOF. A child
        // that exits after forking a pipe-holding descendant must still leave a
        // safe PGID for timeout/Drop to terminate the entire helper group.
        let response_bytes = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic),
            &mut stdout_task,
        )
        .await
        {
            Ok(Ok(Ok(response))) => response,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                return Err(trusted_scope_binding_failure(
                    "read capture scope-binding helper response after trusted scope proof",
                ));
            }
            Err(_) => {
                child.terminate_and_reap();
                return Err(trusted_scope_binding_failure(
                    "capture scope binding exceeded its managed deadline after trusted scope proof",
                ));
            }
        };
        let status = match child.child_mut() {
            Some(child_process) => match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline.monotonic),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(_)) => {
                    child.terminate_and_reap();
                    return Err(trusted_scope_binding_failure(
                        "wait for capture scope-binding helper after trusted scope proof",
                    ));
                }
                Err(_) => {
                    child.terminate_and_reap();
                    return Err(trusted_scope_binding_failure(
                        "capture scope binding exceeded its managed deadline after trusted scope proof",
                    ));
                }
            },
            None => {
                return Err(trusted_scope_binding_failure(
                    "capture scope-binding helper was unavailable after trusted scope proof",
                ));
            }
        };
        child.disarm_child_after_wait();
        child.finish();
        if !status.success() {
            return Err(trusted_scope_binding_failure(
                "capture scope-binding helper ended after trusted scope proof",
            ));
        }
        scope_binding_outcome_to_binding(decode_scope_binding_helper_response(&response_bytes))
    }
}

/// Finish the parent-side scope-proof protocol after the bounded child response
/// has arrived. Keeping the mapping pure makes the public terminal policy
/// auditable: only the fixed unsupported-platform capability result and the
/// closed active-repository class may carry an actionable detail; every other
/// trusted helper failure stays generic.
fn scope_binding_outcome_to_binding(
    outcome: Result<ScopeBindingHelperOutcome>,
) -> Result<CaptureIngressBinding> {
    match outcome {
        Ok(ScopeBindingHelperOutcome::Bound(binding)) => Ok(binding),
        Ok(ScopeBindingHelperOutcome::InvalidScope) => Err(trusted_scope_binding_failure(
            "capture scope-binding helper violated its trusted scope proof",
        )),
        Ok(ScopeBindingHelperOutcome::TrustedBindingFailure) => Err(trusted_scope_binding_failure(
            "capture scope-binding helper reported trusted failure",
        )),
        Ok(ScopeBindingHelperOutcome::TrustedRepositoryFailure(class)) => {
            Err(trusted_active_repository_failure(class))
        }
        Ok(ScopeBindingHelperOutcome::UnsupportedPlatform) => {
            Err(unsupported_platform_scope_binding_failure())
        }
        Err(_) => Err(trusted_scope_binding_failure(
            "capture scope-binding helper returned an invalid response",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use serde_json::json;
    #[cfg(unix)]
    use serial_test::serial;

    use super::*;
    use crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_BYTES;
    #[cfg(not(unix))]
    use crate::internal::ai::{
        capture::key::{CAPTURE_UNSUPPORTED_PLATFORM_REMEDY, load_capture_dedup_secret},
        hooks::{
            HookTarget, LifecycleEventKind, provider::ProviderHookCommand,
            runtime::classify_capture_ingress_for_target,
        },
    };
    #[cfg(unix)]
    use crate::internal::{
        ai::{
            capture::{
                ingress::{CaptureDeadline, CaptureIngressCommand, CaptureIngressOutcome},
                key::{
                    CAPTURE_DEDUP_SECRET_DIR, CAPTURE_DEDUP_SECRET_FILE,
                    CaptureSourceCommitmentDomain, derive_capture_source_commitment_in_scope_until,
                },
                live::hook_execution_deadline,
            },
            capture_scope::CaptureScope,
            hooks::{LifecycleEventKind, providers::claude_provider},
        },
        db,
    };
    #[cfg(unix)]
    use crate::utils::util;

    #[cfg(unix)]
    #[test]
    fn scope_binding_helper_wire_roundtrips_non_utf8_paths() {
        use std::{
            ffi::OsString,
            os::unix::ffi::{OsStrExt, OsStringExt},
            path::PathBuf,
        };

        let storage_path = PathBuf::from(OsString::from_vec(b"/tmp/libra-storage-\xff".to_vec()));
        let worktree_root = PathBuf::from(OsString::from_vec(b"/tmp/libra-root-\xfe".to_vec()));
        let dedup_secret = [42_u8; CAPTURE_DEDUP_SECRET_BYTES];
        let response = scope_binding_response_from_binding(
            CaptureIngressBinding::new(
                "/tmp/libra-verified".to_string(),
                dedup_secret,
                CaptureRuntimeScope::new(storage_path.clone(), worktree_root.clone()),
            ),
            [6_u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
            Some([7_u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES]),
        )
        .expect("encode non-UTF-8 scope binding response");
        let frame = serde_json::to_vec(&response).expect("serialize scope binding response");
        let rendered = String::from_utf8_lossy(&frame);
        assert!(
            !rendered.contains("dedup_secret") && !rendered.contains("secret_base64"),
            "a directly callable helper response must never serialize repository key material: {rendered}"
        );
        assert!(
            rendered.contains("capture-dedup-v2:"),
            "a native replay identity is represented only by the opaque v2 commitment"
        );
        let ScopeBindingHelperOutcome::Bound(binding) =
            decode_scope_binding_helper_response(&frame).expect("decode scope binding response")
        else {
            panic!("scope binding response must retain a bound scope");
        };
        let (verified_cwd, actual_scope) = binding.into_verified_scope_parts();
        assert_eq!(verified_cwd, "/tmp/libra-verified");
        assert_eq!(
            actual_scope.storage_path.as_os_str().as_bytes(),
            storage_path.as_os_str().as_bytes(),
            "the helper protocol must not lossy-convert a verified storage path"
        );
        assert_eq!(
            actual_scope.worktree_root.as_os_str().as_bytes(),
            worktree_root.as_os_str().as_bytes(),
            "the helper protocol must not lossy-convert a verified worktree root"
        );
    }

    #[test]
    fn scope_binding_helper_request_contains_only_cwd_deadline_and_fixed_preimage() {
        let deadline = HookExecutionDeadline {
            monotonic: Instant::now() + Duration::from_secs(1),
            absolute_millis: 1_700_000_000_000,
        };
        let frame = encode_scope_binding_helper_request(
            &CaptureScopeInput {
                reported_cwd: "/safe/worktree".to_string(),
                event_identity_preimage: [0x2C;
                    crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                dedup_preimage: Some(
                    [0x3C; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                ),
            },
            deadline,
        )
        .expect("encode helper request");
        let value: serde_json::Value =
            serde_json::from_slice(&frame).expect("decode helper request as JSON");
        let object = value.as_object().expect("helper request is a JSON object");
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "deadline_millis",
                "dedup_preimage",
                "event_identity_preimage",
                "reported_cwd",
            ],
            "the helper request must not carry provider/session/native scalar spellings"
        );
        assert_eq!(
            object
                .get("dedup_preimage")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES),
            "the only replay input is a fixed SHA-256 digest"
        );
        assert_eq!(
            object
                .get("event_identity_preimage")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES),
            "the helper receives only a fixed event-identity digest, never raw native fields"
        );
    }

    #[test]
    fn scope_binding_helper_opaque_replay_identity_is_stable() {
        fn response() -> ScopeBindingHelperResponse {
            scope_binding_response_from_binding(
                CaptureIngressBinding::new(
                    "/tmp/libra-verified".to_string(),
                    [42_u8; CAPTURE_DEDUP_SECRET_BYTES],
                    CaptureRuntimeScope::new(
                        PathBuf::from("/tmp/libra-storage"),
                        PathBuf::from("/tmp/libra-root"),
                    ),
                ),
                [6_u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                Some([7_u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES]),
            )
            .expect("encode opaque helper response")
        }

        let first = serde_json::to_vec(&response()).expect("serialize first helper response");
        let retry = serde_json::to_vec(&response()).expect("serialize retry helper response");
        assert_eq!(
            first, retry,
            "the same ingress digest must produce one stable opaque replay receipt across helper retries"
        );
    }

    #[test]
    fn scope_binding_helper_fallback_action_identity_is_opaque_without_a_receipt() {
        let response = scope_binding_response_from_binding(
            CaptureIngressBinding::new(
                "/tmp/libra-verified".to_string(),
                [42_u8; CAPTURE_DEDUP_SECRET_BYTES],
                CaptureRuntimeScope::new(
                    PathBuf::from("/tmp/libra-storage"),
                    PathBuf::from("/tmp/libra-root"),
                ),
            ),
            [9_u8; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
            None,
        )
        .expect("encode fallback opaque helper response");
        let ScopeBindingHelperResponse::Bound {
            opaque_event,
            opaque_dedup,
            ..
        } = response
        else {
            panic!("valid binding must encode a bound response")
        };
        assert!(
            opaque_dedup.is_none(),
            "an event without native identity must not mint a replay receipt"
        );
        assert!(
            opaque_event.event_key.starts_with("capture-event-v1:"),
            "fallback action UUID must be bound only to the domain-separated opaque HMAC"
        );
        assert_eq!(
            opaque_event_id_from_wire_parts(opaque_event.event_id, opaque_event.event_key)
                .expect("fallback opaque event wire validates"),
            opaque_event.event_id,
            "the parent retains only a validated opaque action UUID"
        );
    }

    /// The hidden binary entrypoint is deliberately callable without a
    /// parent-only capability: the OS cannot authenticate one same-user
    /// process to another. Its valid-scope output therefore must be safe even
    /// when an arbitrary local caller invokes it directly. Exercise the real
    /// helper path in an isolated repository rather than only its response
    /// encoder so a future wire change cannot re-export the private HMAC key.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[serial(cwd)]
    async fn scope_binding_helper_direct_valid_scope_hides_key_and_replays_opaque_identity() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let repo = tempfile::tempdir().expect("create isolated helper repository");
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        let reported_cwd = repo
            .path()
            .canonicalize()
            .expect("canonicalize isolated helper repository")
            .to_str()
            .expect("test repository path is UTF-8")
            .to_owned();
        let deadline_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is after Unix epoch")
            .as_millis()
            .checked_add(5_000)
            .and_then(|millis| i64::try_from(millis).ok())
            .expect("test helper deadline fits i64");
        let request = ScopeBindingHelperRequest {
            reported_cwd,
            deadline_millis,
            event_identity_preimage: [0xC4;
                crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
            dedup_preimage: Some(
                [0xD4; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
            ),
        };
        let frame = serde_json::to_vec(&request).expect("encode direct helper request");

        let first = run_capture_scope_binding_helper(&frame)
            .expect("a direct helper call must bind the isolated active repository");
        let retry = run_capture_scope_binding_helper(&frame)
            .expect("a duplicate direct helper call must retain the opaque replay receipt");
        assert_eq!(
            first, retry,
            "the same ingress digest must replay to the same opaque helper result"
        );
        let (phase, response) = first
            .split_first()
            .expect("direct helper output begins with its scope-proof phase");
        assert_eq!(
            *phase, SCOPE_BINDING_PHASE_TRUSTED,
            "a valid active worktree must reach the trusted helper phase"
        );
        let rendered = String::from_utf8_lossy(response);
        let secret = std::fs::read(
            repo.path()
                .join(".libra")
                .join(CAPTURE_DEDUP_SECRET_DIR)
                .join(CAPTURE_DEDUP_SECRET_FILE),
        )
        .expect("direct helper creates its repository-private replay key");
        assert!(
            !rendered.contains("dedup_secret")
                && !rendered.contains("secret_base64")
                && !rendered.contains(&STANDARD.encode(&secret))
                && !rendered.contains(&hex::encode(&secret)),
            "a directly callable helper response must never serialize its repository key: {rendered}"
        );
        let response_json: serde_json::Value =
            serde_json::from_slice(response).expect("direct helper output is JSON after phase");
        let opaque_event = response_json
            .get("opaque_event")
            .and_then(serde_json::Value::as_object)
            .expect("direct helper returns an opaque action identity");
        assert_eq!(
            opaque_event.len(),
            2,
            "the action identity wire shape is fixed and carries no secret field"
        );
        assert!(
            opaque_event.contains_key("event_id") && opaque_event.contains_key("event_key"),
            "the action identity contains only its opaque UUID and short-lived HMAC proof"
        );
        assert!(
            opaque_event
                .get("event_key")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|key| key.starts_with("capture-dedup-v2:")),
            "a native helper response preserves the existing v2 HMAC action identity"
        );
        let opaque = response_json
            .get("opaque_dedup")
            .and_then(serde_json::Value::as_object)
            .expect("direct helper returns an opaque native replay identity");
        assert_eq!(
            opaque.len(),
            2,
            "the response's replay material is limited to the opaque identity pair"
        );
        assert!(
            opaque.contains_key("dedup_key") && opaque.contains_key("event_id"),
            "the opaque identity pair has fixed names and no secret field"
        );
        assert!(
            opaque
                .get("dedup_key")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|key| key.starts_with("capture-dedup-v2:")),
            "the helper must return only the v2 HMAC commitment"
        );
        assert!(
            matches!(
                decode_scope_binding_helper_response(response),
                Ok(ScopeBindingHelperOutcome::Bound(_))
            ),
            "the parent accepts the direct helper's correlated opaque identity"
        );

        // Exercise the complete raw-ingress -> helper-request -> helper-output
        // handoff with a no-native Claude callback. The raw provider session
        // spelling is intentionally distinctive: it must be reduced to the
        // fixed SHA-256 request commitment before the helper boundary, and
        // the opaque action UUID is the only corresponding telemetry value.
        let raw_session_sentinel = "raw-session-must-not-enter-helper-wire";
        let raw_payload = serde_json::to_vec(&json!({
            "hook_event_name": "SessionStart",
            "session_id": raw_session_sentinel,
            "cwd": request.reported_cwd,
        }))
        .expect("serialize raw no-native ingress fixture");
        let helper_deadline = HookExecutionDeadline {
            monotonic: Instant::now() + Duration::from_secs(5),
            absolute_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock is after Unix epoch")
                .as_millis()
                .checked_add(5_000)
                .and_then(|millis| i64::try_from(millis).ok())
                .expect("test helper deadline fits i64"),
        };
        let mut forwarded_request = None;
        let mut forwarded_output = None;
        let outcome = CaptureIngressCommand::from_payload(
            &raw_payload,
            crate::internal::ai::hooks::provider::ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            None,
            |scope_input| {
                let helper_request =
                    encode_scope_binding_helper_request(scope_input, helper_deadline)?;
                let helper_output = run_capture_scope_binding_helper(&helper_request)?;
                let (phase, helper_response) = helper_output
                    .split_first()
                    .ok_or_else(|| anyhow!("scope helper output is missing its phase"))?;
                if *phase != SCOPE_BINDING_PHASE_TRUSTED {
                    bail!("valid raw ingress fixture did not reach trusted scope binding");
                }
                let binding = scope_binding_outcome_to_binding(
                    decode_scope_binding_helper_response(helper_response),
                )?;
                forwarded_request = Some(helper_request);
                forwarded_output = Some(helper_output);
                Ok(binding)
            },
        )
        .expect("raw no-native ingress must bind through the real helper");
        let CaptureIngressOutcome::Command(command) = outcome else {
            panic!("known raw no-native event must yield a capture command")
        };
        let forwarded_request = String::from_utf8(forwarded_request.expect("helper request"))
            .expect("helper request is JSON");
        let forwarded_output = String::from_utf8(forwarded_output.expect("helper output"))
            .expect("helper output is phase-prefixed JSON");
        assert!(
            !forwarded_request.contains(raw_session_sentinel)
                && !forwarded_output.contains(raw_session_sentinel),
            "raw provider session input must not cross the helper wire: request={forwarded_request}, output={forwarded_output}"
        );
        let helper_response: serde_json::Value =
            serde_json::from_str(&forwarded_output[1..]).expect("decode helper response");
        assert!(
            helper_response["opaque_dedup"].is_null(),
            "the real no-native ingress route must remain receipt-free"
        );
        let telemetry_rendering = format!(
            "event_id={} provider={} event_kind={}",
            command.event_id(),
            command.provider_kind(),
            command.event_kind(),
        );
        assert!(
            !telemetry_rendering.contains(raw_session_sentinel),
            "the action identifier recorded by hook telemetry must not be a raw session derivative: {telemetry_rendering}"
        );
        let malformed_helper_error = match scope_binding_outcome_to_binding(
            decode_scope_binding_helper_response(
                format!(
                    r#"{{"result":"trusted_binding_failure","unexpected":"{raw_session_sentinel}"}}"#
                )
                .as_bytes(),
            ),
        ) {
            Ok(_) => panic!("malformed helper output must fail closed"),
            Err(error) => error,
        };
        assert!(
            !format!("{malformed_helper_error:#}").contains(raw_session_sentinel),
            "helper errors must remain fixed-safe even when a response contains a raw session spelling"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[serial(cwd)]
    async fn expired_source_commitment_capability_cannot_create_a_private_key() {
        let repo = tempfile::tempdir().expect("create isolated commitment repository");
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let storage = repo.path().join(".libra");
        let database = storage.join(util::DATABASE);
        let conn = db::get_db_conn_instance_for_path(&database)
            .await
            .expect("open isolated commitment database");
        let scope = CaptureScope::resolve(&conn, repo.path())
            .await
            .expect("resolve isolated commitment scope");
        let private = storage.join(CAPTURE_DEDUP_SECRET_DIR);
        assert!(
            !private.exists(),
            "fixture must not create a capture commitment key before the capability is invoked"
        );

        let error = derive_capture_source_commitment_in_scope_until(
            &conn,
            &scope,
            &storage,
            repo.path(),
            CaptureSourceCommitmentDomain::ImportSourceV2,
            &[0xA5; 32],
            Instant::now(),
        )
        .await
        .expect_err("an expired source commitment capability must fail closed");
        assert!(
            format!("{error:#}").contains("deadline elapsed"),
            "expired source commitment must report its deadline rather than performing key I/O: {error:#}"
        );
        assert!(
            !private.exists(),
            "an expired commitment capability must not create a private key namespace"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[serial(cwd)]
    async fn source_commitment_domains_produce_distinct_opaque_v2_ids() {
        let repo = tempfile::tempdir().expect("create isolated commitment-domain repository");
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let storage = repo.path().join(".libra");
        let database = storage.join(util::DATABASE);
        let conn = db::get_db_conn_instance_for_path(&database)
            .await
            .expect("open isolated commitment-domain database");
        let scope = CaptureScope::resolve(&conn, repo.path())
            .await
            .expect("resolve isolated commitment-domain scope");
        let deadline = Instant::now() + Duration::from_secs(5);
        let snapshot = derive_capture_source_commitment_in_scope_until(
            &conn,
            &scope,
            &storage,
            repo.path(),
            CaptureSourceCommitmentDomain::SnapshotContentV2,
            &[0xB6; 32],
            deadline,
        )
        .await
        .expect("derive snapshot-content source commitment");
        let import = derive_capture_source_commitment_in_scope_until(
            &conn,
            &scope,
            &storage,
            repo.path(),
            CaptureSourceCommitmentDomain::ImportSourceV2,
            &[0xB6; 32],
            deadline,
        )
        .await
        .expect("derive import source commitment");
        let subagent = derive_capture_source_commitment_in_scope_until(
            &conn,
            &scope,
            &storage,
            repo.path(),
            CaptureSourceCommitmentDomain::SubagentSourceV2,
            &[0xB6; 32],
            deadline,
        )
        .await
        .expect("derive subagent source commitment");
        assert!(snapshot.starts_with("source/hmac-v2/"));
        assert!(import.starts_with("source/hmac-v2/"));
        assert!(subagent.starts_with("source/subagent-hmac-v2/"));
        assert_ne!(
            snapshot.strip_prefix("source/hmac-v2/"),
            import.strip_prefix("source/hmac-v2/"),
            "snapshot-content and import-ownership domains must not reuse the same HMAC output"
        );
        assert_ne!(
            snapshot.strip_prefix("source/hmac-v2/"),
            subagent.strip_prefix("source/subagent-hmac-v2/"),
            "snapshot-content and subagent domains must not reuse the same HMAC output"
        );
        assert_ne!(
            import.strip_prefix("source/hmac-v2/"),
            subagent.strip_prefix("source/subagent-hmac-v2/"),
            "import-ownership and subagent domains must not reuse the same HMAC output"
        );
    }

    /// Non-Unix platforms must not substitute path-only publication for the
    /// descriptor-relative Unix protocol. Refusing before any key namespace
    /// mutation is safer than trusting a locally swappable endpoint.
    #[cfg(not(unix))]
    #[test]
    fn capture_dedup_non_unix_initialization_fails_closed_without_namespace_mutation() {
        let root = tempfile::tempdir().expect("create non-Unix capture-key tempdir");
        let storage = root.path().join("storage");
        std::fs::create_dir(&storage).expect("create storage");
        let error = load_capture_dedup_secret(&storage)
            .expect_err("non-Unix key initialization must fail closed");
        assert!(
            format!("{error:#}").contains(CAPTURE_UNSUPPORTED_PLATFORM_REMEDY),
            "the diagnostic must explain why Session Capture cannot initialize a key: {error:#}"
        );
        assert_eq!(
            std::fs::read_dir(&storage)
                .expect("enumerate storage after refusal")
                .count(),
            0,
            "the fail-closed branch must not create private, staging, or temporary key entries"
        );

        let response = scope_binding_response_for_binding_result(
            Err(CaptureDedupUnsupportedPlatform.into()),
            [0; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
            None,
        );
        assert!(matches!(
            &response,
            &ScopeBindingHelperResponse::UnsupportedPlatform
        ));
        let wire = serde_json::to_vec(&response).expect("serialize fixed platform response");
        let error =
            match scope_binding_outcome_to_binding(decode_scope_binding_helper_response(&wire)) {
                Ok(_) => panic!("parent must surface the fixed non-Unix capability failure"),
                Err(error) => error,
            };
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(CAPTURE_UNSUPPORTED_PLATFORM_REMEDY),
            "the parent-visible helper failure must retain the actionable fixed platform diagnostic: {rendered}"
        );
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "the actionable platform result must remain a trusted scope failure"
        );
    }

    /// An elapsed parent budget must not hide the platform capability result:
    /// non-Unix has no key-mutation path to protect with that gate. Exercise
    /// the full helper response, parent classification, and Codex settlement
    /// chain so a nonterminal still receives its safe diagnostic/zero exit
    /// while SessionEnd remains a visible unsupported-platform failure.
    #[cfg(not(unix))]
    #[test]
    fn non_unix_expired_scope_binding_keeps_codex_platform_remedy() {
        fn expired_binding_response() -> ScopeBindingHelperResponse {
            let binding = bind_verified_capture_scope(
                VerifiedCaptureScope {
                    verified_cwd: "/libra-test-worktree".to_string(),
                    storage_path: PathBuf::from("/libra-test-worktree/.libra"),
                    worktree_root: PathBuf::from("/libra-test-worktree"),
                },
                Some(0),
            );
            scope_binding_response_for_binding_result(
                binding,
                [0; crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                None,
            )
        }

        fn parent_error_after_expired_binding() -> anyhow::Error {
            let response = expired_binding_response();
            assert!(matches!(
                response,
                ScopeBindingHelperResponse::UnsupportedPlatform
            ));
            let wire = serde_json::to_vec(&response)
                .expect("serialize expired non-Unix scope-binding response");
            match scope_binding_outcome_to_binding(decode_scope_binding_helper_response(&wire)) {
                Ok(_) => panic!("expired non-Unix binding must not return a scope"),
                Err(error) => error,
            }
        }

        let nonterminal = match classify_capture_ingress_for_target(
            Err(parent_error_after_expired_binding()),
            HookTarget::AgentTraces,
            LifecycleEventKind::SessionStart,
        ) {
            Ok(_) => panic!("expired non-Unix SessionStart must not form an ingress command"),
            Err(error) => error,
        };
        assert!(
            is_capture_unsupported_platform_error(&nonterminal),
            "the expired non-Unix SessionStart error must retain the fixed capability result"
        );
        assert!(
            crate::command::hooks::settle_codex_capture_result(
                ProviderHookCommand::SessionStart,
                Err(nonterminal),
            )
            .is_ok(),
            "Codex SessionStart must acknowledge only after emitting the fixed platform remedy"
        );

        let terminal = match classify_capture_ingress_for_target(
            Err(parent_error_after_expired_binding()),
            HookTarget::AgentTraces,
            LifecycleEventKind::SessionEnd,
        ) {
            Ok(_) => panic!("expired non-Unix SessionEnd must not form an ingress command"),
            Err(error) => error,
        };
        assert!(
            is_capture_unsupported_platform_error(&terminal),
            "the expired non-Unix SessionEnd error must retain the fixed capability result"
        );
        let terminal = crate::command::hooks::settle_codex_capture_result(
            ProviderHookCommand::SessionEnd,
            Err(terminal),
        )
        .expect_err("Codex SessionEnd must surface the fixed platform remedy");
        assert_eq!(
            terminal.stable_code(),
            crate::utils::error::StableErrorCode::Unsupported
        );
        assert!(
            terminal
                .render()
                .contains(CAPTURE_UNSUPPORTED_PLATFORM_REMEDY),
            "the expired non-Unix SessionEnd must retain the safe Unix-host remedy: {terminal}"
        );
    }

    #[test]
    fn scope_binding_helper_rejects_oversized_frames_before_decoding() {
        let oversized_input = vec![b'x'; CAPTURE_SCOPE_BINDING_HELPER_INPUT_CAP as usize + 1];
        assert!(
            run_capture_scope_binding_helper(&oversized_input).is_err(),
            "the private helper must reject an oversized request before JSON decoding"
        );
        let oversized_output = vec![b'x'; CAPTURE_SCOPE_BINDING_HELPER_OUTPUT_CAP as usize + 1];
        assert!(
            decode_scope_binding_helper_response(&oversized_output).is_err(),
            "the parent must reject an oversized response before JSON decoding"
        );
    }

    #[test]
    fn scope_binding_helper_expired_unbound_cwd_remains_advisory() {
        let frame = serde_json::to_vec(&ScopeBindingHelperRequest {
            // Deliberately invalid: a deadline must not upgrade an unbound
            // callback into a terminal-persistence failure.
            reported_cwd: "relative-path-must-not-be-resolved".to_string(),
            deadline_millis: 0,
            event_identity_preimage: [0;
                crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
            dedup_preimage: None,
        })
        .expect("encode expired scope-binding request");
        let output = run_capture_scope_binding_helper(&frame)
            .expect("expired helper request returns an opaque advisory response");
        assert_eq!(
            output,
            vec![SCOPE_BINDING_PHASE_UNVERIFIED],
            "an expired unbound scope must emit the advisory phase before any key I/O"
        );
    }

    #[cfg(unix)]
    fn write_scope_binding_transport_fixture(
        dir: &tempfile::TempDir,
        name: &str,
        script: &str,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let helper = dir.path().join(name);
        std::fs::write(&helper, script).expect("write scope-binding transport fixture");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read scope-binding transport fixture permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make scope-binding transport fixture executable");
        helper
    }

    #[cfg(unix)]
    fn scope_binding_fixture_deadline() -> HookExecutionDeadline {
        hook_execution_deadline(
            CaptureDeadline::from_budget_millis(5_000)
                .expect("construct scope-binding transport deadline"),
            false,
        )
        .expect("extend scope-binding transport deadline")
    }

    #[cfg(unix)]
    async fn bind_with_scope_binding_transport_fixture(
        helper: PathBuf,
    ) -> Result<CaptureIngressBinding> {
        crate::internal::ai::authorized_read::with_test_helper_program(
            helper,
            bind_capture_scope_cwd_bounded(
                CaptureScopeInput {
                    reported_cwd: "/scope-binding-transport-fixture".to_string(),
                    event_identity_preimage: [0;
                        crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                    dedup_preimage: None,
                },
                Some(scope_binding_fixture_deadline()),
            ),
        )
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_phase_deadline_edge_prefers_a_ready_proof() {
        use tokio::io::AsyncWriteExt;

        let expired_deadline = tokio::time::Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("construct an already-expired phase deadline")
            .into_std();
        for phase in [
            SCOPE_BINDING_PHASE_TRUSTED,
            SCOPE_BINDING_PHASE_UNVERIFIED,
            SCOPE_BINDING_PHASE_NO_REPOSITORY,
        ] {
            let (mut writer, mut reader) = tokio::io::duplex(1);
            writer
                .write_all(&[phase])
                .await
                .expect("preload one ready scope proof");
            assert_eq!(
                read_scope_binding_phase_until(&mut reader, expired_deadline).await,
                ScopeBindingPhaseRead::Byte(phase),
                "a proof ready when the timer is polled must retain its U/T classification"
            );
        }

        let (_writer, mut reader) = tokio::io::duplex(1);
        assert_eq!(
            read_scope_binding_phase_until(&mut reader, expired_deadline).await,
            ScopeBindingPhaseRead::TimedOut,
            "when the timer wins without a phase byte, the callback has no trusted scope proof"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_helper_clean_eof_before_phase_remains_unverified() {
        let dir = tempfile::tempdir().expect("create no-phase scope helper tempdir");
        let helper = write_scope_binding_transport_fixture(
            &dir,
            "no-phase-eof-scope-binding-helper.sh",
            "#!/bin/sh\ncat >/dev/null\nexit 0\n",
        );

        let error = match bind_with_scope_binding_transport_fixture(helper).await {
            Ok(_) => panic!("clean EOF before a proof must not bind a scope"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookUnverifiedScopeBinding>()),
            "clean EOF before a phase must remain advisory: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_helper_clean_eof_after_trusted_phase_is_terminal() {
        let dir = tempfile::tempdir().expect("create trusted EOF scope helper tempdir");
        let helper = write_scope_binding_transport_fixture(
            &dir,
            "trusted-eof-scope-binding-helper.sh",
            "#!/bin/sh\ncat >/dev/null\nprintf 'T'\nexit 0\n",
        );

        let error = match bind_with_scope_binding_transport_fixture(helper).await {
            Ok(_) => panic!("clean EOF after trusted proof must fail terminally"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "clean EOF after trusted proof must not downgrade to advisory: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_helper_nonzero_after_trusted_phase_is_terminal() {
        let dir = tempfile::tempdir().expect("create nonzero scope helper tempdir");
        let helper = write_scope_binding_transport_fixture(
            &dir,
            "trusted-nonzero-scope-binding-helper.sh",
            "#!/bin/sh\ncat >/dev/null\nprintf 'T'\nexit 7\n",
        );

        let error = match bind_with_scope_binding_transport_fixture(helper).await {
            Ok(_) => panic!("nonzero exit after trusted proof must fail terminally"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "nonzero exit after trusted proof must not downgrade to advisory: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_helper_invalid_phase_remains_unverified() {
        let dir = tempfile::tempdir().expect("create invalid-phase scope helper tempdir");
        let helper = write_scope_binding_transport_fixture(
            &dir,
            "invalid-phase-scope-binding-helper.sh",
            "#!/bin/sh\ncat >/dev/null\nprintf 'X'\nexit 0\n",
        );

        let error = match bind_with_scope_binding_transport_fixture(helper).await {
            Ok(_) => panic!("invalid scope proof must not bind a scope"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookUnverifiedScopeBinding>()),
            "invalid phase must remain advisory: {error:#}"
        );
    }

    /// R86 #4: a hook invoked outside every Libra repository has no active
    /// scope. The helper's `N` proof must stay advisory (never trusted) and
    /// keep its typed no-repository cause for the command-layer contract.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_helper_no_repository_phase_remains_unverified() {
        let dir = tempfile::tempdir().expect("create no-repository scope helper tempdir");
        let helper = write_scope_binding_transport_fixture(
            &dir,
            "no-repository-phase-scope-binding-helper.sh",
            "#!/bin/sh\ncat >/dev/null\nprintf 'N'\nexit 0\n",
        );

        let error = match bind_with_scope_binding_transport_fixture(helper).await {
            Ok(_) => panic!("a no-repository proof must not bind a scope"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookUnverifiedScopeBinding>()),
            "a no-repository proof must remain advisory: {error:#}"
        );
        assert!(
            is_no_active_repository_error(&error),
            "a no-repository proof must keep its typed cause: {error:#}"
        );
        assert!(
            !error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "a cwd outside every repository must never become a trusted terminal failure: {error:#}"
        );
        assert!(
            !error.chain().any(|cause| cause.is::<HookEnvelopeInvalid>()),
            "a well-formed frame outside a repository is not an envelope reject: {error:#}"
        );
    }

    #[test]
    fn no_active_repository_failure_is_unverified_and_never_trusted() {
        let error = no_active_repository_scope_binding_failure();
        assert!(is_no_active_repository_error(&error));
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookUnverifiedScopeBinding>())
                && !error
                    .chain()
                    .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "the no-repository outcome must be unverified only: {error:#}"
        );
        assert!(
            !is_no_active_repository_error(&trusted_scope_binding_failure(
                "unable to resolve the active hook worktree"
            )),
            "a damaged active worktree must not be mistaken for a missing repository"
        );
        assert!(
            !is_no_active_repository_error(&unverified_scope_binding_failure(
                "capture scope-binding helper ended before scope proof"
            )),
            "only the typed no-repository proof restores the repository-not-found contract"
        );
    }

    /// R88 #8: a damaged active repository is a trusted scope failure that
    /// carries only the closed `StorageUnresolved` class, and that class (and
    /// nothing else) survives the helper wire: every class round-trips as a
    /// fixed `trusted_repository_failure` frame back into the same trusted,
    /// typed failure, while an unknown reason or an extra field fails closed
    /// to the generic trusted failure without a class.
    #[test]
    fn active_repository_failure_class_crosses_scope_and_helper_wire() {
        let root = tempfile::tempdir().expect("create damaged-worktree tempdir");
        let damaged = root.path().join("damaged");
        std::fs::create_dir_all(damaged.join(".libra")).expect("create damaged gitdir");
        std::fs::write(damaged.join(".libra").join("worktree_id"), b"linked\n")
            .expect("mark damaged linked worktree");
        let resolved = match classify_active_request_scope(
            crate::internal::worktree_scope::RequestScope::try_resolve(damaged.clone()),
        ) {
            Ok(_) => panic!("a linked worktree without commondir must not resolve"),
            Err(error) => error,
        };
        assert!(
            resolved
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>())
                && active_repository_failure_class(&resolved)
                    == Some(ActiveRepositoryFailureClass::StorageUnresolved)
                && !is_no_active_repository_error(&resolved),
            "a damaged active worktree must be trusted and carry its closed class: {resolved:#}"
        );
        assert!(
            !format!("{resolved:#}").contains(&*damaged.to_string_lossy()),
            "the classified scope failure must not carry the discovery error's path: {resolved:#}"
        );

        for class in [
            ActiveRepositoryFailureClass::StorageUnresolved,
            ActiveRepositoryFailureClass::DatabaseMissing,
            ActiveRepositoryFailureClass::DatabaseUnavailable,
            ActiveRepositoryFailureClass::ObjectFormatUnreadable,
            ActiveRepositoryFailureClass::ObjectFormatUnsupported,
        ] {
            let frame = serde_json::to_vec(&ScopeBindingHelperResponse::TrustedRepositoryFailure {
                reason: class,
            })
            .expect("encode classified trusted helper response");
            let wire: serde_json::Value =
                serde_json::from_slice(&frame).expect("classified response is JSON");
            assert_eq!(
                wire.as_object().map(serde_json::Map::len),
                Some(2),
                "{class:?}: the classified response carries only its result and closed reason: {wire}"
            );
            assert_eq!(wire["result"], "trusted_repository_failure");
            let error = match scope_binding_outcome_to_binding(
                decode_scope_binding_helper_response(&frame),
            ) {
                Ok(_) => panic!("{class:?}: a classified trusted failure must not bind"),
                Err(error) => error,
            };
            assert!(
                error
                    .chain()
                    .any(|cause| cause.is::<HookTrustedScopeBindingFailure>())
                    && active_repository_failure_class(&error) == Some(class),
                "{class:?}: the parent must restore the trusted, typed class: {error:#}"
            );
        }

        for frame in [
            br#"{"result":"trusted_repository_failure","reason":"/tmp/secret-path"}"#.as_slice(),
            br#"{"result":"trusted_repository_failure","reason":"storage_unresolved","detail":"/tmp/secret-path"}"#
                .as_slice(),
            br#"{"result":"trusted_repository_failure"}"#.as_slice(),
        ] {
            let error = match scope_binding_outcome_to_binding(
                decode_scope_binding_helper_response(frame),
            ) {
                Ok(_) => panic!("a malformed classified frame must not bind"),
                Err(error) => error,
            };
            assert!(
                error
                    .chain()
                    .any(|cause| cause.is::<HookTrustedScopeBindingFailure>())
                    && active_repository_failure_class(&error).is_none()
                    && !format!("{error:#}").contains("/tmp/secret-path"),
                "a malformed classified frame must fail closed without a class: {error:#}"
            );
        }
    }

    /// R86 #4 on non-Unix: the managed non-Unix path never spawns the
    /// replay-key helper, but it runs read-only repository discovery before
    /// its capability result. This test is deliberately platform-neutral so
    /// the Unix suite drives the exact discovery and decision functions the
    /// `#[cfg(not(unix))]` branch of `bind_capture_scope_cwd_bounded` calls,
    /// through the real runtime classification and command settlement.
    #[test]
    fn non_unix_scope_binding_keeps_outside_repository_contract() {
        use crate::{
            command::hooks::{
                CaptureErrorSurface, map_capture_ingest_error, settle_codex_capture_result,
            },
            internal::ai::hooks::{
                HookTarget, provider::ProviderHookCommand,
                runtime::classify_capture_ingress_for_target,
            },
            utils::error::StableErrorCode,
        };

        fn classified(error: anyhow::Error, cmd: ProviderHookCommand) -> anyhow::Error {
            match classify_capture_ingress_for_target(
                Err(error),
                HookTarget::AgentTraces,
                cmd.lifecycle_event_kind(),
            ) {
                Ok(_) => panic!("{cmd}: a non-Unix scope failure must not bind"),
                Err(error) => error,
            }
        }

        let root = tempfile::tempdir().expect("create non-Unix discovery tempdir");
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).expect("create outside-repository dir");
        assert!(
            matches!(
                crate::internal::worktree_scope::RequestScope::try_resolve(outside.clone()),
                Ok(None)
            ),
            "fixture precondition: the temporary dir must not be inside any Libra repository"
        );

        // A minimal main repository: discovery stops at a `.libra` holding a
        // database, without any key or reported-path I/O.
        let repository = root.path().join("repository");
        std::fs::create_dir_all(repository.join(".libra")).expect("create repository gitdir");
        std::fs::write(repository.join(".libra").join("libra.db"), b"")
            .expect("mark repository storage");
        // A linked worktree that lost its `commondir`: an existing active
        // repository whose discovery fails, i.e. damage, not absence.
        let damaged = root.path().join("damaged");
        std::fs::create_dir_all(damaged.join(".libra")).expect("create damaged gitdir");
        std::fs::write(damaged.join(".libra").join("worktree_id"), b"linked\n")
            .expect("mark damaged linked worktree");

        // Outside every repository: unverified no-repository, never the
        // trusted capability failure.
        let outside_error =
            non_unix_scope_binding_failure(resolve_active_invocation_scope(Ok(outside.clone())));
        assert!(
            is_no_active_repository_error(&outside_error)
                && !is_capture_unsupported_platform_error(&outside_error)
                && outside_error
                    .chain()
                    .any(|cause| cause.is::<HookUnverifiedScopeBinding>())
                && !outside_error
                    .chain()
                    .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "outside every repository the non-Unix result must be the unverified no-repository outcome: {outside_error:#}"
        );
        assert!(
            settle_codex_capture_result(
                ProviderHookCommand::SessionEnd,
                Err(classified(
                    non_unix_scope_binding_failure(resolve_active_invocation_scope(Ok(
                        outside.clone()
                    ))),
                    ProviderHookCommand::SessionEnd,
                )),
            )
            .is_ok(),
            "Codex SessionEnd outside every repository must exit 0 on non-Unix too"
        );
        let fail_closed = map_capture_ingest_error(
            classified(outside_error, ProviderHookCommand::Stop),
            "hook ingestion failed",
            CaptureErrorSurface::FailClosed,
        );
        assert_eq!(
            fail_closed.stable_code(),
            StableErrorCode::RepoNotFound,
            "Claude and the hidden alias must keep LBR-REPO-001 outside a repository on non-Unix"
        );
        assert!(
            !fail_closed.render().contains(&*outside.to_string_lossy()),
            "the restored repository-not-found error must stay path-free"
        );

        // Inside a repository, and for a damaged active repository, the
        // fixed unsupported-platform policy is unchanged.
        for (label, workdir) in [("repository", &repository), ("damaged", &damaged)] {
            let resolved = resolve_active_invocation_scope(Ok(workdir.clone()));
            match (label, &resolved) {
                ("repository", Ok(_)) => {}
                ("damaged", Err(error))
                    if error
                        .chain()
                        .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()) => {}
                _ => panic!("{label}: unexpected active-repository discovery result"),
            }
            let error = non_unix_scope_binding_failure(resolved);
            assert!(
                is_capture_unsupported_platform_error(&error)
                    && !is_no_active_repository_error(&error)
                    && error
                        .chain()
                        .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
                "{label}: the non-Unix capability result must stay unchanged: {error:#}"
            );
            let terminal = match settle_codex_capture_result(
                ProviderHookCommand::SessionEnd,
                Err(classified(error, ProviderHookCommand::SessionEnd)),
            ) {
                Ok(()) => panic!("{label}: non-Unix Codex SessionEnd must surface its remedy"),
                Err(error) => error,
            };
            assert_eq!(terminal.stable_code(), StableErrorCode::Unsupported);
        }

        // An unreadable invocation cwd is local infrastructure failure, not
        // evidence that the hook ran outside a repository.
        let unreadable = non_unix_scope_binding_failure(resolve_active_invocation_scope(Err(
            std::io::Error::other("cwd unavailable"),
        )));
        assert!(
            is_capture_unsupported_platform_error(&unreadable)
                && !is_no_active_repository_error(&unreadable),
            "an unreadable cwd must keep the unsupported-platform result: {unreadable:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scope_binding_helper_oversized_tail_after_trusted_phase_is_terminal() {
        let dir = tempfile::tempdir().expect("create oversized scope helper tempdir");
        let helper = write_scope_binding_transport_fixture(
            &dir,
            "trusted-oversized-scope-binding-helper.sh",
            "#!/bin/sh\ncat >/dev/null\nprintf 'T'\nhead -c 65536 /dev/zero\n",
        );

        let error = match bind_with_scope_binding_transport_fixture(helper).await {
            Ok(_) => panic!("oversized trusted response must fail terminally"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "oversized response after trusted proof must not downgrade to advisory: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn managed_scope_binding_deadline_kills_and_reaps_blocked_helper() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create scope helper tempdir");
        let helper = dir.path().join("stalled-scope-binding-helper.sh");
        let pid_file = dir.path().join("scope-binding-helper.pid");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\ncat >/dev/null\nexec tail -f /dev/null\n",
                pid_file.display()
            ),
        )
        .expect("write stalled scope helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read stalled scope helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make stalled scope helper executable");

        let started = Instant::now();
        let initial_deadline = hook_execution_deadline(
            CaptureDeadline::from_budget_millis(5_000)
                .expect("construct scope helper startup deadline"),
            false,
        )
        .expect("extend scope helper startup deadline");
        let result = crate::internal::ai::authorized_read::with_test_helper_program(
            helper,
            test_support::with_scope_binding_helper_ready(
                pid_file.clone(),
                Duration::from_millis(100),
                bind_capture_scope_cwd_bounded(
                    CaptureScopeInput {
                        reported_cwd: "/scope-helper-never-binds".to_string(),
                        event_identity_preimage: [0;
                            crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                        dedup_preimage: None,
                    },
                    Some(initial_deadline),
                ),
            ),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("blocked scope helper must observe the absolute deadline"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookUnverifiedScopeBinding>()),
            "a helper that never emits a scope proof must remain advisory: {error:#}"
        );
        // The managed startup deadline is 5 s, then the post-ready budget is
        // only 100 ms. Parallel `cargo test` can spend most of that startup
        // window before the pid file appears. The cap stays well below a
        // blocked `tail -f` reap, which would not return inside this bound.
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the foreground callback must not wait for helper reaping"
        );
        assert!(
            !dir.path().join("private").exists(),
            "the blocked helper fixture must not create any local replay-key state"
        );

        let mut pid = None;
        for _ in 0..50 {
            if let Ok(value) = std::fs::read_to_string(&pid_file) {
                pid = value.trim().parse::<libc::pid_t>().ok();
                if pid.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = pid.unwrap_or_else(|| {
            panic!(
                "stalled helper recorded its process id before the deadline; helper result: {error:#}"
            )
        });
        let mut reaped = false;
        for _ in 0..100 {
            // SAFETY: `kill(pid, 0)` does not signal the process; it only
            // probes whether the exact process remains registered.
            let probe = unsafe { libc::kill(pid, 0) };
            if probe == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                reaped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(reaped, "blocked scope helper must be killed and reaped");
    }

    /// A helper that has flushed `T` crossed the active-scope/key boundary.
    /// Its later deadline is a trusted terminal failure, unlike the preceding
    /// no-phase fixture. This exercises the parent's proof-aware kill/reap
    /// branch rather than inferring trust from a successful spawn.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn managed_scope_binding_deadline_after_trusted_phase_is_terminal_and_reaped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create trusted scope helper tempdir");
        let helper = dir.path().join("trusted-stalled-scope-binding-helper.sh");
        let pid_file = dir.path().join("trusted-scope-binding-helper.pid");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\ncat >/dev/null\nprintf 'T'\nexec tail -f /dev/null\n",
                pid_file.display()
            ),
        )
        .expect("write trusted stalled scope helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read trusted stalled scope helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make trusted stalled scope helper executable");

        let started = Instant::now();
        let initial_deadline = hook_execution_deadline(
            CaptureDeadline::from_budget_millis(5_000)
                .expect("construct trusted scope helper startup deadline"),
            false,
        )
        .expect("extend trusted scope helper startup deadline");
        let result = crate::internal::ai::authorized_read::with_test_helper_program(
            helper,
            // The post-ready window must cover the request write, the
            // helper's stdin EOF and its `T` byte even under parallel test
            // load; the deadline still fires because `tail` never exits.
            test_support::with_scope_binding_helper_ready(
                pid_file.clone(),
                Duration::from_millis(1_000),
                bind_capture_scope_cwd_bounded(
                    CaptureScopeInput {
                        reported_cwd: "/scope-helper-proved-before-block".to_string(),
                        event_identity_preimage: [0;
                            crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                        dedup_preimage: None,
                    },
                    Some(initial_deadline),
                ),
            ),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("trusted blocked scope helper must observe the absolute deadline"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "a helper that proved trusted scope before blocking must stay terminal: {error:#}"
        );
        // The managed startup deadline is 5 s, then the post-ready budget is
        // 1 s. Parallel `cargo test` can spend most of that startup window
        // before the pid file appears. The cap stays well below a blocked
        // `tail -f` reap, which would not return inside this bound.
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the foreground callback must not wait for trusted helper reaping"
        );
        let pid = std::fs::read_to_string(&pid_file)
            .expect("ready trusted helper records its pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("parse trusted helper pid");
        let mut reaped = false;
        for _ in 0..100 {
            // SAFETY: `kill(pid, 0)` does not signal the process; it only
            // probes whether the exact process remains registered.
            let probe = unsafe { libc::kill(pid, 0) };
            if probe == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                reaped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            reaped,
            "trusted blocked scope helper must be killed and reaped"
        );
    }

    /// After the trusted phase, a leader can exit while a forked descendant
    /// retains the response pipe. The parent must keep the leader unreaped
    /// until EOF so the deadline path can still kill the entire process
    /// group, rather than waiting forever or signalling a recycled PGID.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn managed_scope_binding_deadline_kills_descendant_after_trusted_phase() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create scope helper tempdir");
        let helper = dir.path().join("forking-trusted-scope-binding-helper.sh");
        let leader_file = dir.path().join("forking-scope-binding-leader.pid");
        let descendant_file = dir.path().join("forking-scope-binding-descendant.pid");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\ncat >/dev/null\nprintf 'T'\n/bin/sleep 60 &\nprintf '%s' \"$!\" > '{}'\nexit 0\n",
                leader_file.display(),
                descendant_file.display(),
            ),
        )
        .expect("write forking scope helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make forking scope helper executable");

        let initial_deadline = hook_execution_deadline(
            CaptureDeadline::from_budget_millis(5_000)
                .expect("construct forking scope helper startup deadline"),
            false,
        )
        .expect("extend forking scope helper deadline");
        let result = crate::internal::ai::authorized_read::with_test_helper_program(
            helper,
            // See `managed_scope_binding_deadline_after_trusted_phase_is_terminal_and_reaped`:
            // the window must outlast the `T` byte under parallel load, while
            // the pipe-holding `sleep 60` descendant still exhausts it.
            test_support::with_scope_binding_helper_ready(
                leader_file,
                Duration::from_millis(1_000),
                bind_capture_scope_cwd_bounded(
                    CaptureScopeInput {
                        reported_cwd: "/scope-helper-forks-after-proof".to_string(),
                        event_identity_preimage: [0;
                            crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                        dedup_preimage: None,
                    },
                    Some(initial_deadline),
                ),
            ),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("pipe-holding descendant must exhaust deadline"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "the post-proof descendant failure must remain terminal: {error:#}"
        );

        let descendant = async {
            for _ in 0..100 {
                if let Ok(value) = std::fs::read_to_string(&descendant_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            None
        }
        .await
        .expect("forked scope helper descendant must publish its PID");
        let reaped = async {
            for _ in 0..100 {
                // SAFETY: signal zero only probes the exact fixture PID.
                if unsafe { libc::kill(descendant, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(
            reaped,
            "scope-binding deadline left a trusted pipe-holding descendant alive"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn malformed_scope_binding_response_after_trusted_phase_is_terminal() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create malformed scope helper tempdir");
        let helper = dir.path().join("malformed-trusted-scope-binding-helper.sh");
        std::fs::write(&helper, "#!/bin/sh\ncat >/dev/null\nprintf 'Tnot-json'\n")
            .expect("write malformed trusted scope helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read malformed trusted scope helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)
            .expect("make malformed trusted scope helper executable");

        let deadline = hook_execution_deadline(
            CaptureDeadline::from_budget_millis(5_000)
                .expect("construct malformed scope helper deadline"),
            false,
        )
        .expect("extend malformed scope helper deadline");
        let result = crate::internal::ai::authorized_read::with_test_helper_program(
            helper,
            bind_capture_scope_cwd_bounded(
                CaptureScopeInput {
                    reported_cwd: "/scope-helper-malformed-response".to_string(),
                    event_identity_preimage: [0;
                        crate::internal::ai::capture::ingress::CAPTURE_DEDUP_PREIMAGE_BYTES],
                    dedup_preimage: None,
                },
                Some(deadline),
            ),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("malformed response after trusted proof must fail terminally"),
            Err(error) => error,
        };
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<HookTrustedScopeBindingFailure>()),
            "malformed response after trusted proof must not downgrade to advisory: {error:#}"
        );
    }
}
