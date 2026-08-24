//! `WorkspaceStore` — the unified Agent workspace association and lease
//! service (plan-20260714 Part C §C.8, task W4).
//!
//! One table (`workspace_record`, migration `2026072501`) makes linked
//! worktrees, temporary task worktrees, and future remote workspaces queryable
//! through a single association layer, and coordinates the *writers* of a
//! workspace between Agent runtimes.
//!
//! Three rules shape this module, all of them straight out of §C.8:
//!
//! 1. **The database is the arbiter, not the application.** A linked
//!    workspace's lease identity is `(repo_id, worktree_id)` — never a single
//!    `workspace_id` row — and the canonical path is unique across every live
//!    workspace. [`WorkspaceStore::acquire_with_conn`] therefore writes FIRST
//!    and maps the resulting unique violation onto a stable "lease held"
//!    error. It never SELECTs a winner: a read-then-write election has a race
//!    window in which two acquirers both publish a record for one directory.
//! 2. **Every lease mutation is an owner + monotonic-fence conditional
//!    write.** A stale owner (one whose lease has been reclaimed with a higher
//!    fence) cannot renew or release the new owner's lease; its conditional
//!    UPDATE matches zero rows and surfaces [`WorkspaceError::LeaseLost`].
//! 3. **No lease is ever stolen implicitly.** An expired lease stays with its
//!    owner until [`WorkspaceStore::reclaim_expired_with_conn`] — the
//!    doctor/scavenger entry point — takes it over with a new fence. Human use
//!    of a linked worktree never needs a record at all, so a human is never
//!    locked out by this table.
//!
//! The store holds only association IDs: no prompts, transcripts, tool
//! payloads, or file contents (§C.8). Callers pass `now_ms` explicitly rather
//! than the store reading the clock, so TTL/expiry behaviour is testable
//! without sleeping (§C.12 forbids sleep-timed concurrency tests).

use std::path::{Path, PathBuf};

use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, Value};
use thiserror::Error;

use crate::{
    internal::config::ConfigKv,
    utils::error::{CliError, StableErrorCode},
};

/// Default page size for workspace listings (§C.14).
pub const DEFAULT_LIST_LIMIT: u64 = 50;
/// Hard cap on a single workspace listing page (§C.14) — machine callers
/// paginate with a keyset cursor instead of asking for an unbounded page.
pub const MAX_LIST_LIMIT: u64 = 500;

/// What kind of working copy a record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
    /// A registered linked worktree (`libra worktree add`).
    Linked,
    /// A temporary copied task worktree owned by an Agent run.
    TaskCopy,
    /// A temporary FUSE-mounted task worktree owned by an Agent run.
    TaskFuse,
    /// A workspace materialized outside this machine.
    Remote,
}

impl WorkspaceKind {
    /// Storage spelling — must match the `kind` CHECK constraint of
    /// `2026072501_workspace_record.sql`.
    pub const fn as_db_value(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::TaskCopy => "task_copy",
            Self::TaskFuse => "task_fuse",
            Self::Remote => "remote",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, WorkspaceError> {
        match value {
            "linked" => Ok(Self::Linked),
            "task_copy" => Ok(Self::TaskCopy),
            "task_fuse" => Ok(Self::TaskFuse),
            "remote" => Ok(Self::Remote),
            other => Err(WorkspaceError::Corrupt(format!(
                "unknown workspace kind '{other}' in workspace_record"
            ))),
        }
    }
}

/// Who the workspace was provisioned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceOwnerKind {
    Human,
    Agent,
    Automation,
}

impl WorkspaceOwnerKind {
    /// Storage spelling — must match the `owner_kind` CHECK constraint.
    pub const fn as_db_value(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Automation => "automation",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, WorkspaceError> {
        match value {
            "human" => Ok(Self::Human),
            "agent" => Ok(Self::Agent),
            "automation" => Ok(Self::Automation),
            other => Err(WorkspaceError::Corrupt(format!(
                "unknown workspace owner kind '{other}' in workspace_record"
            ))),
        }
    }
}

/// Lifecycle state of a workspace record.
///
/// Note the two different "not finished" sets, which are easy to conflate:
/// [`Self::holds_identity`] is the LIVE set indexed by the two partial unique
/// indexes (`provisioning`/`active`/`releasing`), while the down-migration
/// guard additionally refuses `orphaned` rows — an orphan still carries
/// recovery state a rollback would destroy, even though it no longer reserves
/// its identity or path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Materialization in progress; nothing may treat it as usable yet.
    Provisioning,
    /// Published and usable.
    Active,
    /// Teardown started; the identity is still reserved until it settles.
    Releasing,
    /// Fully torn down. Terminal — frees the identity and the path.
    Released,
    /// Teardown failed or the owner vanished; needs scavenger/doctor
    /// attention. Frees the identity, but is NOT settled.
    Orphaned,
}

impl WorkspaceState {
    /// Storage spelling — must match the `state` CHECK constraint.
    pub const fn as_db_value(self) -> &'static str {
        match self {
            Self::Provisioning => "provisioning",
            Self::Active => "active",
            Self::Releasing => "releasing",
            Self::Released => "released",
            Self::Orphaned => "orphaned",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, WorkspaceError> {
        match value {
            "provisioning" => Ok(Self::Provisioning),
            "active" => Ok(Self::Active),
            "releasing" => Ok(Self::Releasing),
            "released" => Ok(Self::Released),
            "orphaned" => Ok(Self::Orphaned),
            other => Err(WorkspaceError::Corrupt(format!(
                "unknown workspace state '{other}' in workspace_record"
            ))),
        }
    }

    /// True while the row occupies its `(repo_id, worktree_id)` identity and
    /// its canonical path — i.e. exactly the states covered by the partial
    /// unique indexes.
    pub const fn holds_identity(self) -> bool {
        matches!(self, Self::Provisioning | Self::Active | Self::Releasing)
    }

    /// SQL fragment listing [`Self::holds_identity`] states. Kept next to the
    /// predicate so the Rust and SQL definitions cannot drift.
    const fn live_sql_set() -> &'static str {
        "('provisioning', 'active', 'releasing')"
    }
}

/// One `workspace_record` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub workspace_id: String,
    pub repo_id: String,
    pub kind: WorkspaceKind,
    /// Set for linked worktrees (their lease identity); `None` for task and
    /// remote workspaces, which own their own directory.
    pub worktree_id: Option<String>,
    /// Canonical absolute path — resolved by [`canonical_path_string`] before
    /// it is ever stored.
    pub path: String,
    pub owner_kind: WorkspaceOwnerKind,
    pub owner_id: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub base_commit: Option<String>,
    pub branch: Option<String>,
    pub state: WorkspaceState,
    pub lease_owner: Option<String>,
    pub lease_fence: i64,
    pub lease_expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The capability token returned by a successful acquire/renew/reclaim.
///
/// Carry it (not the bare `workspace_id`) into every later lease mutation: the
/// `fence` is what makes a stale owner's write fail instead of clobbering the
/// new owner (§C.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLease {
    pub workspace_id: String,
    pub owner: String,
    pub fence: i64,
    pub expires_at: Option<i64>,
}

impl WorkspaceLease {
    /// True when `now_ms` is at or past the lease deadline. An expired lease is
    /// still the owner's until a doctor/scavenger reclaim takes it over — this
    /// only reports the fact.
    pub fn is_expired(&self, now_ms: i64) -> bool {
        self.expires_at.is_some_and(|deadline| now_ms >= deadline)
    }
}

/// Everything needed to publish a workspace and take its first lease.
///
/// Deliberately carries NO `repo_id`: the repository identity is derived from
/// the connection inside [`WorkspaceStore::acquire_with_conn`] (§C.4.1.1 — one
/// canonicalization helper for both writes and queries). A caller-supplied
/// value could differ from the repository's own `libra.repoid`, and because
/// both partial unique indexes start with `repo_id`, two acquirers spelling it
/// differently would each get a live record for one directory.
#[derive(Debug, Clone)]
pub struct AcquireRequest {
    pub kind: WorkspaceKind,
    /// Required for [`WorkspaceKind::Linked`], rejected otherwise.
    pub worktree_id: Option<String>,
    /// Any spelling of the workspace directory; stored canonicalized.
    pub path: PathBuf,
    pub owner_kind: WorkspaceOwnerKind,
    pub owner_id: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub base_commit: Option<String>,
    pub branch: Option<String>,
    /// Lease holder identity (runtime/process token). Conditional writes match
    /// on it, so it must be stable for the life of the lease.
    pub lease_owner: String,
    /// Lease lifetime in milliseconds; must be positive.
    pub lease_ttl_ms: i64,
    /// [`WorkspaceState::Provisioning`] when the directory still has to be
    /// materialized (publish with [`WorkspaceStore::activate_with_conn`] only
    /// after it succeeds — §C.8 forbids a fake active record), or
    /// [`WorkspaceState::Active`] for an already-usable directory.
    pub initial_state: WorkspaceState,
}

impl AcquireRequest {
    /// Minimal linked-worktree request; fill the optional association IDs with
    /// the builder-style setters below.
    pub fn linked(
        worktree_id: impl Into<String>,
        path: impl Into<PathBuf>,
        lease_owner: impl Into<String>,
        lease_ttl_ms: i64,
    ) -> Self {
        Self {
            kind: WorkspaceKind::Linked,
            worktree_id: Some(worktree_id.into()),
            path: path.into(),
            owner_kind: WorkspaceOwnerKind::Agent,
            owner_id: None,
            task_id: None,
            session_id: None,
            base_commit: None,
            branch: None,
            lease_owner: lease_owner.into(),
            lease_ttl_ms,
            initial_state: WorkspaceState::Active,
        }
    }

    /// Minimal task-worktree request (copy or FUSE), starting in
    /// `provisioning`.
    pub fn task(
        kind: WorkspaceKind,
        path: impl Into<PathBuf>,
        lease_owner: impl Into<String>,
        lease_ttl_ms: i64,
    ) -> Self {
        Self {
            kind,
            worktree_id: None,
            path: path.into(),
            owner_kind: WorkspaceOwnerKind::Agent,
            owner_id: None,
            task_id: None,
            session_id: None,
            base_commit: None,
            branch: None,
            lease_owner: lease_owner.into(),
            lease_ttl_ms,
            initial_state: WorkspaceState::Provisioning,
        }
    }

    /// Attach the owning agent/human/automation identity.
    pub fn with_owner(mut self, owner_kind: WorkspaceOwnerKind, owner_id: Option<String>) -> Self {
        self.owner_kind = owner_kind;
        self.owner_id = owner_id;
        self
    }

    /// Attach the task/session association IDs (never payloads).
    pub fn with_association(mut self, task_id: Option<String>, session_id: Option<String>) -> Self {
        self.task_id = task_id;
        self.session_id = session_id;
        self
    }

    /// Publish in an explicit initial state (only `provisioning` or `active`
    /// are legal, and `active` requires the directory to exist already).
    pub fn with_initial_state(mut self, state: WorkspaceState) -> Self {
        self.initial_state = state;
        self
    }

    /// Attach the base commit / branch this workspace started from.
    pub fn with_base(mut self, base_commit: Option<String>, branch: Option<String>) -> Self {
        self.base_commit = base_commit;
        self.branch = branch;
        self
    }
}

/// Filter + keyset page for [`WorkspaceStore::list_with_conn`].
///
/// Scoped to this repository implicitly — like every other entry point, the
/// listing resolves `repo_id` from the connection, so no caller can page
/// through another repository's workspaces.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceListQuery {
    /// `None` lists every state; `Some(states)` restricts to those states.
    pub states: Option<Vec<WorkspaceState>>,
    /// Page size, clamped to [`MAX_LIST_LIMIT`]; `None` uses
    /// [`DEFAULT_LIST_LIMIT`].
    pub limit: Option<u64>,
    /// Keyset cursor: the last `workspace_id` of the previous page.
    pub cursor: Option<String>,
}

impl WorkspaceListQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_states(mut self, states: Vec<WorkspaceState>) -> Self {
        self.states = Some(states);
        self
    }

    pub fn with_page(mut self, limit: Option<u64>, cursor: Option<String>) -> Self {
        self.limit = limit;
        self.cursor = cursor;
        self
    }

    fn effective_limit(&self) -> u64 {
        self.limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT)
    }
}

/// One page of workspace records plus the cursor for the next page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePage {
    pub items: Vec<WorkspaceRecord>,
    /// `Some(workspace_id)` when more rows may follow; `None` at the end.
    pub next_cursor: Option<String>,
}

/// Failures surfaced by the workspace store.
///
/// Lease refusals map onto the `LBR-AGENT-*` domain rather than the branch
/// conflict codes (§C.13): a held lease is an Agent-runtime coordination
/// failure, not a ref conflict, and agents branch on the distinction.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    /// Another live record already claims this linked identity or canonical
    /// path. The refusal is authoritative (it comes from a unique index); the
    /// holder details are a best-effort diagnostic read.
    #[error("{0}")]
    LeaseHeld(Box<LeaseHeldDetail>),
    /// The presented owner/fence no longer matches the stored lease — it was
    /// reclaimed by a newer owner, or the record already settled.
    #[error("workspace lease for '{workspace_id}' is no longer held by '{owner}' at fence {fence}")]
    LeaseLost {
        workspace_id: String,
        owner: String,
        fence: i64,
    },
    /// No such workspace record.
    #[error("workspace '{0}' does not exist in this repository")]
    NotFound(String),
    /// Caller-supplied arguments cannot describe a valid workspace.
    #[error("{0}")]
    Invalid(String),
    /// Stored data does not match the schema this binary understands.
    #[error("{0}")]
    Corrupt(String),
    /// The repository database could not be read.
    #[error("cannot read the workspace registry: {0}")]
    ReadFailed(String),
    /// The repository database could not be written.
    #[error("cannot update the workspace registry: {0}")]
    WriteFailed(String),
}

/// Details of a refused acquire — kept boxed so `WorkspaceError` stays small.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseHeldDetail {
    /// Human-readable identity that is already claimed.
    pub identity: String,
    /// The conflicting record, when the diagnostic read could resolve it.
    pub workspace_id: Option<String>,
    pub holder: Option<String>,
    pub state: Option<WorkspaceState>,
    pub expires_at: Option<i64>,
    /// True when the conflict came from the canonical-path index (an alias of
    /// an already-claimed directory) rather than the linked-identity index.
    pub path_conflict: bool,
}

impl std::fmt::Display for LeaseHeldDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subject = if self.path_conflict {
            format!("workspace path {}", self.identity)
        } else {
            format!("workspace {}", self.identity)
        };
        write!(f, "{subject} is already leased")?;
        if let Some(holder) = &self.holder {
            write!(f, " by '{holder}'")?;
        }
        if let Some(workspace_id) = &self.workspace_id {
            write!(f, " (workspace {workspace_id}")?;
            if let Some(state) = self.state {
                write!(f, ", state {}", state.as_db_value())?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl WorkspaceError {
    /// Stable machine code for this failure.
    pub fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::LeaseHeld(_) => StableErrorCode::AgentWorkspaceLeaseHeld,
            Self::LeaseLost { .. } => StableErrorCode::AgentWorkspaceLeaseLost,
            Self::NotFound(_) => StableErrorCode::CliInvalidTarget,
            Self::Invalid(_) => StableErrorCode::CliInvalidArguments,
            Self::Corrupt(_) => StableErrorCode::RepoCorrupt,
            Self::ReadFailed(_) => StableErrorCode::IoReadFailed,
            Self::WriteFailed(_) => StableErrorCode::IoWriteFailed,
        }
    }

    /// Actionable next step for the user, mirroring the stable code.
    ///
    /// Every `libra worktree doctor` mention here is INSPECT-ONLY (§C.11 W0,
    /// Codex R19/R20): the doctor's default invocation is strictly read-only,
    /// and its mutating grammar (reclaim/adopt/clear/repair) is not frozen yet.
    /// A hint must never name a recovery command that does not exist — pinned
    /// by `worktree_doctor_hints_are_inspect_only`.
    fn hint(&self) -> Option<String> {
        match self {
            // W0 (Codex R19/R20): `worktree doctor` is a READ-ONLY diagnostic
            // until W4 lands its explicit mutation subcommands. Hints must not
            // name `reclaim` / `adopt` / `release` actions that do not exist —
            // a user who follows one gets an unknown-subcommand error while
            // still holding the problem the hint promised to solve.
            Self::LeaseHeld(detail) => Some(if detail.path_conflict {
                "another live workspace already claims this directory; run \
                 `libra worktree doctor` to inspect which one, and have its owner finish \
                 with the directory first"
                    .to_string()
            } else {
                "wait for the holder to finish with this workspace; run \
                 `libra worktree doctor` to inspect the lease deadline and its owner"
                    .to_string()
            }),
            Self::LeaseLost { .. } => Some(
                "the lease was reclaimed by a newer owner — stop writing to this workspace and \
                 acquire it again before retrying"
                    .to_string(),
            ),
            Self::NotFound(_) => {
                Some("run `libra agent workspace list` to see live workspaces".to_string())
            }
            Self::Corrupt(_) => {
                Some("run `libra worktree doctor` to inspect the workspace registry".to_string())
            }
            _ => None,
        }
    }
}

impl From<WorkspaceError> for CliError {
    fn from(error: WorkspaceError) -> Self {
        let code = error.stable_code();
        let hint = error.hint();
        let mut cli = CliError::fatal(error.to_string()).with_stable_code(code);
        if let Some(hint) = hint {
            cli = cli.with_hint(hint);
        }
        cli
    }
}

type WorkspaceResult<T> = Result<T, WorkspaceError>;

/// This repository's canonical identity (`libra.repoid`), proven to have come
/// from a repository database.
///
/// §C.4.1.1 makes this the ONE source of `repo_id` for workspace records: it is
/// derived from the repository, never from the caller's cwd, worktree path, or
/// a string the caller made up. Because both partial unique indexes are
/// prefixed by `repo_id`, a caller-spelled value would let two acquirers each
/// land a live record for one directory — so the only way to obtain this type
/// is [`RepoIdentity::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoIdentity(String);

impl RepoIdentity {
    /// Read and validate `libra.repoid` from a repository database.
    ///
    /// The value is taken VERBATIM (the newest row wins, matching
    /// `ConfigKv::get_with_conn`) and must already be clean: empty, or padded
    /// with surrounding whitespace, is corrupt repository metadata rather than
    /// something to silently normalize. Normalizing here would also split the
    /// identity in two — the SQL guard compares the stored bytes, and SQLite's
    /// `TRIM` and Rust's `str::trim` do not agree on what whitespace is.
    pub async fn resolve<C: ConnectionTrait>(conn: &C) -> WorkspaceResult<Self> {
        let entry = ConfigKv::get_with_conn(conn, "libra.repoid")
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot read the repository identity: {error}"))
            })?;
        let value = entry.map(|entry| entry.value).ok_or_else(|| {
            WorkspaceError::Corrupt(
                "repository identity (libra.repoid) is missing; the repository metadata is \
                 incomplete — run `libra init` in a fresh clone or restore this repository's \
                 configuration"
                    .to_string(),
            )
        })?;
        if value.trim().is_empty() {
            return Err(WorkspaceError::Corrupt(
                "repository identity (libra.repoid) is empty; the repository metadata is \
                 incomplete"
                    .to_string(),
            ));
        }
        if value != value.trim() {
            return Err(WorkspaceError::Corrupt(format!(
                "repository identity (libra.repoid) has surrounding whitespace ({value:?}); \
                 rewrite it without padding so every subsystem keys the repository identically"
            )));
        }
        Ok(Self(value))
    }

    /// [`RepoIdentity::resolve`], but lazily mints `libra.repoid` for a legacy
    /// repository that never had one (pre-`init`-stamping histories).
    ///
    /// Minting happens ONLY when the key is entirely absent — an existing
    /// value, even the legacy `"unknown-repo"` placeholder, is returned via
    /// the strict [`RepoIdentity::resolve`] path so this helper can never
    /// fork the identity away from what the workspace store and the cloud
    /// prefix logic already observed. Callers that must not create state
    /// (read-only doctor paths) should keep calling [`RepoIdentity::resolve`].
    pub async fn resolve_or_init<C: ConnectionTrait>(conn: &C) -> WorkspaceResult<Self> {
        let existing = ConfigKv::get_with_conn(conn, "libra.repoid")
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot read the repository identity: {error}"))
            })?;
        if existing.is_none() {
            let minted = uuid::Uuid::new_v4().to_string();
            ConfigKv::set_with_conn(conn, "libra.repoid", &minted, false)
                .await
                .map_err(|error| {
                    WorkspaceError::WriteFailed(format!(
                        "cannot initialize the repository identity (libra.repoid): {error}"
                    ))
                })?;
        }
        Self::resolve(conn).await
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepoIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Synchronization seam for concurrency tests (§C.12: "并发测试不得依赖 sleep
/// 猜时序；使用 barrier/failpoint 确认两个 operation 已进入指定窗口").
///
/// The store's two contended writes — the acquire election and the reclaim
/// takeover — call `before_write` immediately before their statement and
/// `after_write` immediately after it, so a test can prove a second writer
/// entered that window, and can hold a writer between its statement and its
/// return value.
///
/// NOT COMPILED INTO RELEASE BUILDS: the module and both call sites are
/// `#[cfg(debug_assertions)]` — deliberately NOT a cargo feature, since any
/// feature can be switched on for a release build, and a seam that can park an
/// acquire forever must be impossible to ship. An optimized build compiles the
/// empty stub below instead. Installation is process-global, so tests that use
/// it must be `#[serial]` and clear it on the way out.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub mod test_hooks {
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex, OnceLock},
    };

    /// An async callback fired inside the store, around a contended write.
    pub type WriteHook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

    static BEFORE_WRITE: OnceLock<Mutex<Option<WriteHook>>> = OnceLock::new();
    static AFTER_WRITE: OnceLock<Mutex<Option<WriteHook>>> = OnceLock::new();

    fn before_slot() -> &'static Mutex<Option<WriteHook>> {
        BEFORE_WRITE.get_or_init(|| Mutex::new(None))
    }

    fn after_slot() -> &'static Mutex<Option<WriteHook>> {
        AFTER_WRITE.get_or_init(|| Mutex::new(None))
    }

    /// Install (or clear with `None`) the pre-write hook.
    pub fn set_before_write(hook: Option<WriteHook>) {
        if let Ok(mut guard) = before_slot().lock() {
            *guard = hook;
        }
    }

    /// Install (or clear with `None`) the post-write hook — the window between
    /// a takeover's statement and the value it hands back.
    pub fn set_after_write(hook: Option<WriteHook>) {
        if let Ok(mut guard) = after_slot().lock() {
            *guard = hook;
        }
    }

    /// Fire the pre-write hook, if any. A poisoned lock is treated as "no
    /// hook": a test seam must never fail a real operation.
    pub(crate) async fn before_write() {
        fire(before_slot()).await;
    }

    /// Fire the post-write hook, if any.
    pub(crate) async fn after_write() {
        fire(after_slot()).await;
    }

    async fn fire(slot: &'static Mutex<Option<WriteHook>>) {
        let hook = slot.lock().ok().and_then(|guard| guard.clone());
        if let Some(hook) = hook {
            hook().await;
        }
    }
}

/// Release builds compile the contended writes with no seam at all.
#[cfg(not(debug_assertions))]
mod test_hooks {
    pub(crate) async fn before_write() {}
    pub(crate) async fn after_write() {}
}

/// Wall-clock milliseconds, the unit every timestamp column in this table uses./// Wall-clock milliseconds, the unit every timestamp column in this table uses.
/// Callers pass the value in explicitly so tests can drive expiry without
/// sleeping.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Association + lease service over `workspace_record`.
///
/// Every method has a `*_with_conn` form taking any [`ConnectionTrait`] (a
/// pooled connection or a caller-owned transaction). Mutations are single
/// statements, so they are atomic on their own connection and compose inside a
/// larger caller transaction without changing meaning.
pub struct WorkspaceStore;

impl WorkspaceStore {
    /// Publish a workspace record and take its first lease (fence 1).
    ///
    /// The INSERT itself is the election: whichever writer lands first owns the
    /// identity, and the loser's unique violation becomes
    /// [`WorkspaceError::LeaseHeld`]. Nothing is stolen here — a held-but-
    /// expired lease still refuses, and only
    /// [`Self::reclaim_expired_with_conn`] can take it over.
    ///
    /// `identity` must come from [`RepoIdentity::resolve`] on this repository.
    /// Resolve it either BEFORE opening the transaction or after the
    /// transaction already holds its write lock — never as the first statement
    /// of a not-yet-writing transaction, which is exactly the read-then-write
    /// shape that makes two concurrent acquires deadlock on the SQLite lock
    /// upgrade. [`Self::acquire`] does that ordering for callers that do not
    /// own a transaction.
    pub async fn acquire_with_conn<C: ConnectionTrait>(
        conn: &C,
        identity: &RepoIdentity,
        request: &AcquireRequest,
        now_ms: i64,
    ) -> WorkspaceResult<WorkspaceLease> {
        let normalized = NormalizedAcquire::from_request(request)?;
        let workspace_id = uuid::Uuid::new_v4().to_string();
        let expires_at = now_ms
            .checked_add(normalized.lease_ttl_ms)
            .ok_or_else(|| WorkspaceError::Invalid("lease TTL overflows the clock".to_string()))?;

        // ONE write statement carries the whole election, so the transaction
        // takes its lock on the first statement. A SELECT first — to read the
        // identity, or to look for a conflicting row — would make this a
        // read-then-write transaction, and two of those racing deadlock on the
        // SQLite lock upgrade: the loser gets a transient "database is locked"
        // instead of the stable lease-held refusal §C.8 requires.
        //
        // The two guards in the WHERE clause are:
        //   1. the identity this call resolved must still be the repository's
        //      identity (byte-for-byte — see `REPO_IDENTITY_SUBQUERY`), and
        //   2. no workspace of ANOTHER identity may still be live or awaiting
        //      recovery. Both unique indexes are prefixed by `repo_id`, so a
        //      rewritten `libra.repoid` would otherwise reopen the namespace:
        //      the same worktree and directory could be claimed a second time
        //      while the old owner's rows became invisible to every
        //      repo-scoped listing and unreclaimable by doctor. The guard
        //      reads only `repo_id`/`state`, which `idx_workspace_state`
        //      covers, so it stays an index-only scan as settled rows pile up.
        let statement = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            format!(
                "INSERT INTO workspace_record (workspace_id, repo_id, kind, worktree_id, path, \
                 owner_kind, owner_id, task_id, session_id, base_commit, branch, state, \
                 lease_owner, lease_fence, lease_expires_at, created_at, updated_at) \
                 SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ? \
                 WHERE ? = {REPO_IDENTITY_SUBQUERY} \
                 AND NOT EXISTS (SELECT 1 FROM workspace_record \
                                 WHERE repo_id <> ? AND state <> 'released')"
            ),
            [
                workspace_id.clone().into(),
                identity.as_str().into(),
                request.kind.as_db_value().into(),
                normalized.worktree_id.clone().into(),
                normalized.path.clone().into(),
                request.owner_kind.as_db_value().into(),
                optional(&request.owner_id),
                optional(&request.task_id),
                optional(&request.session_id),
                optional(&request.base_commit),
                optional(&request.branch),
                normalized.initial_state.as_db_value().into(),
                normalized.lease_owner.clone().into(),
                expires_at.into(),
                now_ms.into(),
                now_ms.into(),
                identity.as_str().into(),
                identity.as_str().into(),
            ],
        );

        test_hooks::before_write().await;
        match conn.execute_raw(statement).await {
            Ok(result) if result.rows_affected() == 1 => Ok(WorkspaceLease {
                workspace_id,
                owner: normalized.lease_owner,
                fence: 1,
                expires_at: Some(expires_at),
            }),
            // Guard rejected: the identity moved under us, or workspaces of a
            // previous identity are still unsettled. Diagnosing costs a read,
            // but only on this refusal path.
            Ok(_) => Err(Self::describe_identity_refusal(conn, identity).await),
            Err(error) if is_unique_violation(&error) => {
                Err(Self::describe_conflict(conn, identity, &normalized).await)
            }
            Err(error) => Err(WorkspaceError::WriteFailed(format!(
                "cannot publish the workspace record: {error}"
            ))),
        }
    }

    /// Explain a guard-rejected acquire (see `acquire_with_conn`): either the
    /// repository identity changed since it was resolved, or rows from a
    /// previous identity are still unsettled.
    async fn describe_identity_refusal<C: ConnectionTrait>(
        conn: &C,
        identity: &RepoIdentity,
    ) -> WorkspaceError {
        match RepoIdentity::resolve(conn).await {
            Ok(current) if current.as_str() != identity.as_str() => {
                WorkspaceError::Corrupt(format!(
                    "the repository identity changed from {} to {} while this workspace was \
                     being registered; retry the operation",
                    identity.as_str(),
                    current.as_str()
                ))
            }
            // INSPECT-ONLY wording (§C.11 W0, Codex R19/R20): the doctor's
            // adopt/release grammar is not frozen yet, so this must not name
            // an action the CLI cannot perform.
            Ok(current) => WorkspaceError::Corrupt(format!(
                "workspaces registered under a previous repository identity are still live or \
                 awaiting recovery, so no new workspace can be registered under {current}; run \
                 `libra worktree doctor` to inspect them"
            )),
            Err(error) => error,
        }
    }

    /// Publish a `provisioning` workspace as `active` after its directory has
    /// been materialized. Owner + fence conditional: a reclaimed lease cannot
    /// activate a workspace it no longer owns.
    pub async fn activate_with_conn<C: ConnectionTrait>(
        conn: &C,
        lease: &WorkspaceLease,
        now_ms: i64,
    ) -> WorkspaceResult<()> {
        let affected = Self::conditional_update(
            conn,
            "UPDATE workspace_record SET state = 'active', updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ? \
             AND state = 'provisioning'",
            [
                now_ms.into(),
                lease.workspace_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence.into(),
            ],
            "activate the workspace",
        )
        .await?;
        if affected == 0 {
            return Err(Self::lost(lease));
        }
        Ok(())
    }

    /// Extend the lease deadline. The fence is unchanged — renewing is not a
    /// takeover — so a stale owner cannot resurrect a reclaimed lease.
    ///
    /// An expired-but-unreclaimed lease may still be renewed: nobody else has
    /// taken it, and refusing here would strand a workspace whose owner merely
    /// paused. Once a reclaim bumps the fence, this returns
    /// [`WorkspaceError::LeaseLost`].
    pub async fn renew_with_conn<C: ConnectionTrait>(
        conn: &C,
        lease: &WorkspaceLease,
        lease_ttl_ms: i64,
        now_ms: i64,
    ) -> WorkspaceResult<WorkspaceLease> {
        if lease_ttl_ms <= 0 {
            return Err(WorkspaceError::Invalid(
                "lease TTL must be a positive number of milliseconds".to_string(),
            ));
        }
        let expires_at = now_ms
            .checked_add(lease_ttl_ms)
            .ok_or_else(|| WorkspaceError::Invalid("lease TTL overflows the clock".to_string()))?;
        let sql = format!(
            "UPDATE workspace_record SET lease_expires_at = ?, updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ? \
             AND state IN {}",
            WorkspaceState::live_sql_set()
        );
        let affected = Self::conditional_update(
            conn,
            &sql,
            [
                expires_at.into(),
                now_ms.into(),
                lease.workspace_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence.into(),
            ],
            "renew the workspace lease",
        )
        .await?;
        if affected == 0 {
            return Err(Self::lost(lease));
        }
        Ok(WorkspaceLease {
            workspace_id: lease.workspace_id.clone(),
            owner: lease.owner.clone(),
            fence: lease.fence,
            expires_at: Some(expires_at),
        })
    }

    /// Move a live workspace into `releasing` — teardown has started, but the
    /// identity and path stay reserved until it settles.
    pub async fn begin_release_with_conn<C: ConnectionTrait>(
        conn: &C,
        lease: &WorkspaceLease,
        now_ms: i64,
    ) -> WorkspaceResult<()> {
        let sql = format!(
            "UPDATE workspace_record SET state = 'releasing', updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ? \
             AND state IN {}",
            WorkspaceState::live_sql_set()
        );
        let affected = Self::conditional_update(
            conn,
            &sql,
            [
                now_ms.into(),
                lease.workspace_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence.into(),
            ],
            "start releasing the workspace",
        )
        .await?;
        if affected == 0 {
            return Err(Self::lost(lease));
        }
        Ok(())
    }

    /// Settle the workspace as `released`, freeing its identity and path.
    ///
    /// Owner + fence conditional, so a STALE owner — one whose lease was
    /// reclaimed at a higher fence — cannot release the new owner's lease
    /// (§C.8). The fence column keeps its value: fences are monotonic per
    /// record and a released row is never re-leased (a fresh acquire mints a
    /// new `workspace_id`).
    pub async fn release_with_conn<C: ConnectionTrait>(
        conn: &C,
        lease: &WorkspaceLease,
        now_ms: i64,
    ) -> WorkspaceResult<()> {
        let sql = format!(
            "UPDATE workspace_record SET state = 'released', lease_expires_at = NULL, \
             updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ? \
             AND state IN {}",
            WorkspaceState::live_sql_set()
        );
        let affected = Self::conditional_update(
            conn,
            &sql,
            [
                now_ms.into(),
                lease.workspace_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence.into(),
            ],
            "release the workspace lease",
        )
        .await?;
        if affected == 0 {
            return Err(Self::lost(lease));
        }
        Ok(())
    }

    /// The OWNER gives up on its own workspace: provisioning or cleanup failed
    /// and the directory may still exist, so the record becomes `orphaned` for
    /// the scavenger/doctor instead of a silent leak.
    ///
    /// Owner + fence conditional like every other lease mutation — a stale
    /// owner must not be able to orphan (and thereby free the identity of) a
    /// workspace that a newer owner is actively using. For the case where the
    /// owner itself is gone, use [`Self::orphan_expired_with_conn`].
    pub async fn abandon_with_conn<C: ConnectionTrait>(
        conn: &C,
        lease: &WorkspaceLease,
        now_ms: i64,
    ) -> WorkspaceResult<()> {
        let sql = format!(
            "UPDATE workspace_record SET state = 'orphaned', updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ? AND state IN {}",
            WorkspaceState::live_sql_set()
        );
        let affected = Self::conditional_update(
            conn,
            &sql,
            [
                now_ms.into(),
                lease.workspace_id.clone().into(),
                lease.owner.clone().into(),
                lease.fence.into(),
            ],
            "abandon the workspace",
        )
        .await?;
        if affected == 0 {
            // Already orphaned/settled by an earlier attempt is a no-op, not a
            // failure — abandoning is the cleanup path and must be idempotent.
            return match Self::get_with_conn(conn, &lease.workspace_id).await? {
                Some(record) if !record.state.holds_identity() => Ok(()),
                Some(_) => Err(Self::lost(lease)),
                None => Err(Self::lost(lease)),
            };
        }
        Ok(())
    }

    /// The SCAVENGER orphans a workspace whose owner is gone: only a live
    /// record whose lease deadline has already passed can be swept, so a
    /// healthy workspace can never be knocked out from under its owner.
    ///
    /// Not fence-conditional by design — the owner that should have released
    /// this record is exactly the one that died — but the expiry predicate is
    /// what keeps that safe. An unknown id is an error rather than a silent
    /// no-op so a typo in a doctor invocation cannot look like success.
    pub async fn orphan_expired_with_conn<C: ConnectionTrait>(
        conn: &C,
        workspace_id: &str,
        now_ms: i64,
    ) -> WorkspaceResult<()> {
        let sql = format!(
            "UPDATE workspace_record SET state = 'orphaned', updated_at = ? \
             WHERE workspace_id = ? AND state IN {} \
             AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?",
            WorkspaceState::live_sql_set()
        );
        let affected = Self::conditional_update(
            conn,
            &sql,
            [now_ms.into(), workspace_id.into(), now_ms.into()],
            "sweep the expired workspace",
        )
        .await?;
        if affected == 0 {
            let record = Self::get_with_conn(conn, workspace_id)
                .await?
                .ok_or_else(|| WorkspaceError::NotFound(workspace_id.to_string()))?;
            if !record.state.holds_identity() {
                // Already orphaned or settled — the sweep has nothing to do.
                return Ok(());
            }
            return Err(WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
                identity: workspace_identity(&record),
                workspace_id: Some(record.workspace_id.clone()),
                holder: record.lease_owner.clone(),
                state: Some(record.state),
                expires_at: record.lease_expires_at,
                path_conflict: false,
            })));
        }
        Ok(())
    }

    /// Doctor/scavenger takeover of an EXPIRED lease, with a new fence.
    ///
    /// This is the only path that changes `lease_owner` on a live record, and
    /// it refuses while the lease is still within its deadline. The fence
    /// increment is what invalidates the previous owner: its renew, release,
    /// and activate calls all stop matching (§C.8 "stale owner 不能释放新
    /// owner lease").
    ///
    /// An `orphaned` record can be reclaimed too — that is how a crashed task
    /// workspace is adopted for cleanup, and it re-enters as `releasing`, not
    /// `active`: adoption means "I will tear this down", never "this is usable
    /// again". If a NEW record has since claimed the same linked identity or
    /// path, the update hits the unique index and refuses rather than
    /// double-claiming the directory.
    ///
    /// A live record keeps its lifecycle state across the takeover: a
    /// half-materialized `provisioning` workspace must not be advertised as
    /// `active` just because its lease changed hands.
    pub async fn reclaim_expired_with_conn<C: ConnectionTrait>(
        conn: &C,
        identity: &RepoIdentity,
        workspace_id: &str,
        new_owner: &str,
        lease_ttl_ms: i64,
        now_ms: i64,
    ) -> WorkspaceResult<WorkspaceLease> {
        let new_owner = new_owner.trim();
        if new_owner.is_empty() {
            return Err(WorkspaceError::Invalid(
                "lease owner must be a non-empty identity".to_string(),
            ));
        }
        if lease_ttl_ms <= 0 {
            return Err(WorkspaceError::Invalid(
                "lease TTL must be a positive number of milliseconds".to_string(),
            ));
        }
        let expires_at = now_ms
            .checked_add(lease_ttl_ms)
            .ok_or_else(|| WorkspaceError::Invalid("lease TTL overflows the clock".to_string()))?;

        // ONE statement decides and reports the takeover. Reading the new
        // fence back with a second SELECT would be a race on a bare
        // connection: a competing reclaim committing in between would make
        // this caller return — and later act on — a fence it does not own,
        // which is exactly the stale-owner release this fence exists to stop.
        //
        // Like acquire, it also re-checks that the identity resolved for this
        // call is STILL the repository's identity. Without that, a rewrite
        // landing between resolve and update would let a takeover revive a
        // row under the old identity — live again, but invisible to every
        // repo-scoped read and unreachable by any later recovery.
        let sql = format!(
            "UPDATE workspace_record SET lease_owner = ?, lease_fence = lease_fence + 1, \
             lease_expires_at = ?, \
             state = CASE WHEN state = 'orphaned' THEN 'releasing' ELSE state END, \
             updated_at = ? \
             WHERE workspace_id = ? AND repo_id = ? AND ? = {REPO_IDENTITY_SUBQUERY} \
             AND (state IN {} OR state = 'orphaned') \
             AND lease_expires_at IS NOT NULL AND lease_expires_at <= ? \
             RETURNING workspace_id, lease_fence, lease_expires_at",
            WorkspaceState::live_sql_set()
        );
        test_hooks::before_write().await;
        let updated = match conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                &sql,
                [
                    new_owner.into(),
                    expires_at.into(),
                    now_ms.into(),
                    workspace_id.into(),
                    identity.as_str().into(),
                    identity.as_str().into(),
                    now_ms.into(),
                ],
            ))
            .await
        {
            Ok(updated) => updated,
            Err(error) if is_unique_violation(&error) => {
                // Another record already claims this identity or directory —
                // adopting the orphan would double-claim it.
                let record = Self::get_with_conn(conn, workspace_id).await?;
                return Err(WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
                    identity: record
                        .as_ref()
                        .map(|record| record.path.clone())
                        .unwrap_or_else(|| workspace_id.to_string()),
                    workspace_id: None,
                    holder: None,
                    state: None,
                    expires_at: None,
                    path_conflict: true,
                })));
            }
            Err(error) => {
                return Err(WorkspaceError::WriteFailed(format!(
                    "cannot reclaim the workspace lease: {error}"
                )));
            }
        };

        // The window a fence read-back would have lived in: a regressed
        // implementation that re-read the row here would see a competing
        // takeover's fence instead of its own.
        test_hooks::after_write().await;

        let Some(row) = updated else {
            // Identity drift is reported as such rather than as "not expired".
            match RepoIdentity::resolve(conn).await {
                Ok(current) if current.as_str() != identity.as_str() => {
                    return Err(WorkspaceError::Corrupt(format!(
                        "the repository identity changed from {} to {} while this lease was \
                         being reclaimed; retry the operation",
                        identity.as_str(),
                        current.as_str()
                    )));
                }
                Ok(_) => {}
                Err(error) => return Err(error),
            }
            let record = Self::get_with_conn(conn, workspace_id)
                .await?
                .ok_or_else(|| WorkspaceError::NotFound(workspace_id.to_string()))?;
            return Err(WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
                identity: workspace_identity(&record),
                workspace_id: Some(record.workspace_id.clone()),
                holder: record.lease_owner.clone(),
                state: Some(record.state),
                expires_at: record.lease_expires_at,
                path_conflict: false,
            })));
        };

        let fence: i64 = row.try_get_by_index(1).map_err(|error| {
            WorkspaceError::Corrupt(format!("reclaimed lease fence is unreadable: {error}"))
        })?;
        let expires_at: Option<i64> = row.try_get_by_index(2).map_err(|error| {
            WorkspaceError::Corrupt(format!("reclaimed lease deadline is unreadable: {error}"))
        })?;
        Ok(WorkspaceLease {
            workspace_id: workspace_id.to_string(),
            owner: new_owner.to_string(),
            fence,
            expires_at,
        })
    }

    /// Load one record of THIS repository by id.
    pub async fn get_with_conn<C: ConnectionTrait>(
        conn: &C,
        workspace_id: &str,
    ) -> WorkspaceResult<Option<WorkspaceRecord>> {
        let repo_id = RepoIdentity::resolve(conn).await?;
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT {SELECT_COLUMNS} FROM workspace_record \
                     WHERE workspace_id = ? AND repo_id = ?"
                ),
                [workspace_id.into(), repo_id.as_str().into()],
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot read the workspace record: {error}"))
            })?;
        row.as_ref().map(record_from_row).transpose()
    }

    /// The live record holding a linked worktree's lease identity, if any.
    pub async fn find_live_linked_with_conn<C: ConnectionTrait>(
        conn: &C,
        worktree_id: &str,
    ) -> WorkspaceResult<Option<WorkspaceRecord>> {
        let repo_id = RepoIdentity::resolve(conn).await?;
        Self::live_linked_row(conn, repo_id.as_str(), worktree_id).await
    }

    /// Repo-id-explicit form used inside acquire, where the identity has
    /// already been resolved once for the insert.
    async fn live_linked_row<C: ConnectionTrait>(
        conn: &C,
        repo_id: &str,
        worktree_id: &str,
    ) -> WorkspaceResult<Option<WorkspaceRecord>> {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT {SELECT_COLUMNS} FROM workspace_record \
                     WHERE repo_id = ? AND kind = 'linked' AND worktree_id = ? AND state IN {}",
                    WorkspaceState::live_sql_set()
                ),
                [repo_id.into(), worktree_id.into()],
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot read the workspace record: {error}"))
            })?;
        row.as_ref().map(record_from_row).transpose()
    }

    /// The live record claiming a canonical path, if any. The path is
    /// canonicalized first, so an alias resolves to the same record.
    pub async fn find_live_by_path_with_conn<C: ConnectionTrait>(
        conn: &C,
        path: &Path,
    ) -> WorkspaceResult<Option<WorkspaceRecord>> {
        let repo_id = RepoIdentity::resolve(conn).await?;
        let canonical = canonical_path_string(path)?;
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT {SELECT_COLUMNS} FROM workspace_record \
                     WHERE repo_id = ? AND path = ? AND state IN {}",
                    WorkspaceState::live_sql_set()
                ),
                [repo_id.as_str().into(), canonical.into()],
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot read the workspace record: {error}"))
            })?;
        row.as_ref().map(record_from_row).transpose()
    }

    /// Records left behind by a PREVIOUS repository identity — the recovery
    /// work list for a rewritten `libra.repoid` (§C.4.1.1).
    ///
    /// Every other read is scoped to the current identity, and `acquire` fails
    /// closed while any of these is unsettled, so without this entry point a
    /// rewrite would strand a crashed owner's record where nothing could see or
    /// recover it.
    ///
    /// Keyset-paginated like every other listing (§C.14): a record the operator
    /// defers must not pin the page and hide everything behind it, so `cursor`
    /// walks past it.
    pub async fn foreign_identity_records_with_conn<C: ConnectionTrait>(
        conn: &C,
        limit: Option<u64>,
        cursor: Option<&str>,
    ) -> WorkspaceResult<WorkspacePage> {
        let repo_id = RepoIdentity::resolve(conn).await?;
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);
        let mut sql = format!(
            "SELECT {SELECT_COLUMNS} FROM workspace_record \
             WHERE repo_id <> ? AND state <> 'released'"
        );
        let mut values: Vec<Value> = vec![repo_id.as_str().into()];
        if let Some(cursor) = cursor {
            sql.push_str(" AND workspace_id > ?");
            values.push(cursor.into());
        }
        sql.push_str(" ORDER BY workspace_id ASC LIMIT ?");
        values.push((limit + 1).into());

        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                sql,
                values,
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!(
                    "cannot list workspaces of a previous repository identity: {error}"
                ))
            })?;
        let has_more = rows.len() as u64 > limit;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        for row in rows.iter().take(limit as usize) {
            items.push(record_from_row(row)?);
        }
        let next_cursor = if has_more {
            items.last().map(|record| record.workspace_id.clone())
        } else {
            None
        };
        Ok(WorkspacePage { items, next_cursor })
    }

    /// Re-home one record from a previous repository identity onto the current
    /// one, so the normal machinery (list, reclaim, release) can finish it.
    ///
    /// This is the doctor's adopt action, and it is deliberately the ONLY way
    /// out of the fail-closed state a rewritten identity produces. The record
    /// keeps its state, owner, and fence — adoption changes WHERE it is
    /// visible, not WHO holds it — so a still-live owner is not silently
    /// dispossessed. If the current identity already claims that linked
    /// worktree or directory, the unique index refuses rather than
    /// double-claiming it.
    pub async fn adopt_foreign_identity_with_conn<C: ConnectionTrait>(
        conn: &C,
        identity: &RepoIdentity,
        workspace_id: &str,
        now_ms: i64,
    ) -> WorkspaceResult<()> {
        let sql = format!(
            "UPDATE workspace_record SET repo_id = ?, updated_at = ? \
             WHERE workspace_id = ? AND repo_id <> ? AND ? = {REPO_IDENTITY_SUBQUERY}"
        );
        let affected = match conn
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                &sql,
                [
                    identity.as_str().into(),
                    now_ms.into(),
                    workspace_id.into(),
                    identity.as_str().into(),
                    identity.as_str().into(),
                ],
            ))
            .await
        {
            Ok(result) => result.rows_affected(),
            Err(error) if is_unique_violation(&error) => {
                return Err(WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
                    identity: workspace_id.to_string(),
                    workspace_id: Some(workspace_id.to_string()),
                    holder: None,
                    state: None,
                    expires_at: None,
                    path_conflict: true,
                })));
            }
            Err(error) => {
                return Err(WorkspaceError::WriteFailed(format!(
                    "cannot adopt the workspace record: {error}"
                )));
            }
        };
        if affected == 0 {
            // Identity drift first: a rewrite between resolve and update makes
            // the guard refuse, and reporting "no such workspace" would send
            // recovery looking for a record that is still very much there.
            match RepoIdentity::resolve(conn).await {
                Ok(current) if current.as_str() != identity.as_str() => {
                    return Err(WorkspaceError::Corrupt(format!(
                        "the repository identity changed from {} to {} while this workspace was \
                         being adopted; re-run the adoption against the current identity",
                        identity.as_str(),
                        current.as_str()
                    )));
                }
                Ok(_) => {}
                Err(error) => return Err(error),
            }
            return match Self::get_with_conn(conn, workspace_id).await? {
                // Already under the current identity: adopting is idempotent.
                Some(_) => Ok(()),
                None => Err(WorkspaceError::NotFound(workspace_id.to_string())),
            };
        }
        Ok(())
    }

    /// One bounded, keyset-paginated page of workspace records ordered by
    /// `workspace_id` (§C.14 — never an unbounded history read).
    pub async fn list_with_conn<C: ConnectionTrait>(
        conn: &C,
        query: &WorkspaceListQuery,
    ) -> WorkspaceResult<WorkspacePage> {
        let repo_id = RepoIdentity::resolve(conn).await?;
        let limit = query.effective_limit();
        let mut sql = format!("SELECT {SELECT_COLUMNS} FROM workspace_record WHERE repo_id = ?");
        let mut values: Vec<Value> = vec![repo_id.as_str().into()];

        if let Some(states) = &query.states {
            if states.is_empty() {
                return Ok(WorkspacePage {
                    items: Vec::new(),
                    next_cursor: None,
                });
            }
            let placeholders = vec!["?"; states.len()].join(", ");
            sql.push_str(&format!(" AND state IN ({placeholders})"));
            for state in states {
                values.push(state.as_db_value().into());
            }
        }
        if let Some(cursor) = &query.cursor {
            sql.push_str(" AND workspace_id > ?");
            values.push(cursor.as_str().into());
        }
        // Fetch one extra row to decide whether a next page exists without a
        // second COUNT query.
        sql.push_str(" ORDER BY workspace_id ASC LIMIT ?");
        values.push((limit + 1).into());

        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                sql,
                values,
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot list workspace records: {error}"))
            })?;

        let has_more = rows.len() as u64 > limit;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        for row in rows.iter().take(limit as usize) {
            items.push(record_from_row(row)?);
        }
        let next_cursor = if has_more {
            items.last().map(|record| record.workspace_id.clone())
        } else {
            None
        };
        Ok(WorkspacePage { items, next_cursor })
    }

    /// One bounded, keyset-paginated page for `worktree doctor` (§C.8 W4).
    ///
    /// Deliberately NOT identity-scoped, unlike every other listing: the whole
    /// point of the doctor view is to surface rows the normal machinery hides,
    /// and a record left behind by a PREVIOUS repository identity is exactly
    /// such a row — invisible to [`Self::list_with_conn`], unreclaimable, and
    /// the reason `acquire` fails closed. The caller renders each row's own
    /// `repo_id` so a foreign row is diagnosable rather than merely absent.
    ///
    /// `released` rows are excluded: they are settled history with nothing left
    /// to diagnose, and letting them accumulate in the page would push live
    /// problems behind a cursor. Strictly read-only.
    pub async fn doctor_page_with_conn<C: ConnectionTrait>(
        conn: &C,
        limit: Option<u64>,
        after_id: Option<&str>,
    ) -> WorkspaceResult<WorkspacePage> {
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);
        let mut sql =
            format!("SELECT {SELECT_COLUMNS} FROM workspace_record WHERE state <> 'released'");
        let mut values: Vec<Value> = Vec::new();
        if let Some(after_id) = after_id {
            sql.push_str(" AND workspace_id > ?");
            values.push(after_id.into());
        }
        // One extra row decides `has_more` without a second COUNT query.
        sql.push_str(" ORDER BY workspace_id ASC LIMIT ?");
        values.push((limit + 1).into());

        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                sql,
                values,
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!(
                    "cannot list workspace records for diagnosis: {error}"
                ))
            })?;
        let has_more = rows.len() as u64 > limit;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        for row in rows.iter().take(limit as usize) {
            items.push(record_from_row(row)?);
        }
        let next_cursor = if has_more {
            items.last().map(|record| record.workspace_id.clone())
        } else {
            None
        };
        Ok(WorkspacePage { items, next_cursor })
    }

    /// Load one record by id for `worktree doctor`, across repository
    /// identities (see [`Self::doctor_page_with_conn`] for why the doctor is
    /// not identity-scoped). Strictly read-only.
    pub async fn doctor_record_with_conn<C: ConnectionTrait>(
        conn: &C,
        workspace_id: &str,
    ) -> WorkspaceResult<Option<WorkspaceRecord>> {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!("SELECT {SELECT_COLUMNS} FROM workspace_record WHERE workspace_id = ?"),
                [workspace_id.into()],
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!(
                    "cannot read the workspace record for diagnosis: {error}"
                ))
            })?;
        row.as_ref().map(record_from_row).transpose()
    }

    /// Live records whose lease deadline has passed — the scavenger's bounded
    /// work list. Ordered by deadline so the longest-dead owner is handled
    /// first.
    pub async fn expired_leases_with_conn<C: ConnectionTrait>(
        conn: &C,
        now_ms: i64,
        limit: u64,
    ) -> WorkspaceResult<Vec<WorkspaceRecord>> {
        let repo_id = RepoIdentity::resolve(conn).await?;
        let limit = limit.clamp(1, MAX_LIST_LIMIT);
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT {SELECT_COLUMNS} FROM workspace_record \
                     WHERE repo_id = ? AND state IN {} AND lease_expires_at IS NOT NULL \
                     AND lease_expires_at <= ? ORDER BY lease_expires_at ASC LIMIT ?",
                    WorkspaceState::live_sql_set()
                ),
                [repo_id.as_str().into(), now_ms.into(), limit.into()],
            ))
            .await
            .map_err(|error| {
                WorkspaceError::ReadFailed(format!("cannot list expired workspace leases: {error}"))
            })?;
        rows.iter().map(record_from_row).collect()
    }

    /// Shared conditional-write helper: runs the statement and reports how many
    /// rows it matched.
    async fn conditional_update<C: ConnectionTrait, I: IntoIterator<Item = Value>>(
        conn: &C,
        sql: &str,
        values: I,
        action: &str,
    ) -> WorkspaceResult<u64> {
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            sql,
            values,
        ))
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| WorkspaceError::WriteFailed(format!("cannot {action}: {error}")))
    }

    fn lost(lease: &WorkspaceLease) -> WorkspaceError {
        WorkspaceError::LeaseLost {
            workspace_id: lease.workspace_id.clone(),
            owner: lease.owner.clone(),
            fence: lease.fence,
        }
    }

    /// Turn a unique violation into a precise refusal.
    ///
    /// The refusal itself is already decided by the index; this read only
    /// enriches the message. If the conflicting row disappears between the two
    /// statements, the refusal still stands (with less detail) rather than
    /// being downgraded into a success.
    async fn describe_conflict<C: ConnectionTrait>(
        conn: &C,
        repo_id: &RepoIdentity,
        normalized: &NormalizedAcquire,
    ) -> WorkspaceError {
        // Error path only: the write already took its lock, so reading here
        // cannot deadlock on an upgrade.
        if let Some(worktree_id) = &normalized.worktree_id
            && let Ok(Some(record)) =
                Self::live_linked_row(conn, repo_id.as_str(), worktree_id).await
        {
            return WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
                identity: workspace_identity(&record),
                workspace_id: Some(record.workspace_id.clone()),
                holder: record.lease_owner.clone(),
                state: Some(record.state),
                expires_at: record.lease_expires_at,
                path_conflict: false,
            }));
        }

        let by_path = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!(
                    "SELECT {SELECT_COLUMNS} FROM workspace_record \
                     WHERE repo_id = ? AND path = ? AND state IN {}",
                    WorkspaceState::live_sql_set()
                ),
                [repo_id.as_str().into(), normalized.path.as_str().into()],
            ))
            .await;
        if let Ok(Some(row)) = by_path
            && let Ok(record) = record_from_row(&row)
        {
            return WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
                identity: record.path.clone(),
                workspace_id: Some(record.workspace_id.clone()),
                holder: record.lease_owner.clone(),
                state: Some(record.state),
                expires_at: record.lease_expires_at,
                path_conflict: true,
            }));
        }

        WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
            identity: normalized
                .worktree_id
                .clone()
                .unwrap_or_else(|| normalized.path.clone()),
            workspace_id: None,
            holder: None,
            state: None,
            expires_at: None,
            path_conflict: normalized.worktree_id.is_none(),
        }))
    }
}

/// Convenience wrappers that own their transaction. Do NOT call these from
/// inside a `db.transaction(...)` block — use the `*_with_conn` form there.
impl WorkspaceStore {
    /// [`Self::acquire_with_conn`] in its own transaction.
    ///
    /// A refusal rolls the transaction back EXPLICITLY rather than leaving it
    /// to `Drop`: the diagnostic read that builds a lease-held message runs
    /// after the failed INSERT, and the write lock it may still hold must be
    /// released before the caller retries — not whenever the guard happens to
    /// be dropped.
    pub async fn acquire<
        C: ConnectionTrait + TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
    >(
        conn: &C,
        request: &AcquireRequest,
        now_ms: i64,
    ) -> WorkspaceResult<WorkspaceLease> {
        // Resolved BEFORE the transaction opens: reading inside it would make
        // the transaction read-then-write, which two concurrent acquirers
        // cannot resolve without one of them hitting SQLITE_BUSY.
        let identity = RepoIdentity::resolve(conn).await?;
        // Reads the existing lease before writing one, so the write lock is
        // taken up front (`db::begin_write_transaction`).
        let txn = crate::internal::db::begin_write_transaction(conn)
            .await
            .map_err(|error| {
                WorkspaceError::WriteFailed(format!("cannot open a workspace transaction: {error}"))
            })?;
        let lease = match Self::acquire_with_conn(&txn, &identity, request, now_ms).await {
            Ok(lease) => lease,
            Err(error) => return Err(rollback_after(txn, error).await),
        };
        txn.commit().await.map_err(|error| {
            WorkspaceError::WriteFailed(format!("cannot commit the workspace record: {error}"))
        })?;
        Ok(lease)
    }

    /// [`Self::reclaim_expired_with_conn`] in its own transaction.
    pub async fn reclaim_expired<
        C: ConnectionTrait + TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
    >(
        conn: &C,
        workspace_id: &str,
        new_owner: &str,
        lease_ttl_ms: i64,
        now_ms: i64,
    ) -> WorkspaceResult<WorkspaceLease> {
        let identity = RepoIdentity::resolve(conn).await?;
        // Reads the existing lease before writing one, so the write lock is
        // taken up front (`db::begin_write_transaction`).
        let txn = crate::internal::db::begin_write_transaction(conn)
            .await
            .map_err(|error| {
                WorkspaceError::WriteFailed(format!("cannot open a workspace transaction: {error}"))
            })?;
        let lease = match Self::reclaim_expired_with_conn(
            &txn,
            &identity,
            workspace_id,
            new_owner,
            lease_ttl_ms,
            now_ms,
        )
        .await
        {
            Ok(lease) => lease,
            Err(error) => return Err(rollback_after(txn, error).await),
        };
        txn.commit().await.map_err(|error| {
            WorkspaceError::WriteFailed(format!("cannot commit the workspace lease: {error}"))
        })?;
        Ok(lease)
    }
}

/// Validated, normalized form of an [`AcquireRequest`].
struct NormalizedAcquire {
    worktree_id: Option<String>,
    path: String,
    lease_owner: String,
    lease_ttl_ms: i64,
    initial_state: WorkspaceState,
}

impl NormalizedAcquire {
    fn from_request(request: &AcquireRequest) -> WorkspaceResult<Self> {
        let lease_owner = request.lease_owner.trim().to_string();
        if lease_owner.is_empty() {
            return Err(WorkspaceError::Invalid(
                "lease owner must be a non-empty identity".to_string(),
            ));
        }
        if request.lease_ttl_ms <= 0 {
            return Err(WorkspaceError::Invalid(
                "lease TTL must be a positive number of milliseconds".to_string(),
            ));
        }
        let worktree_id = request
            .worktree_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        match request.kind {
            WorkspaceKind::Linked if worktree_id.is_none() => {
                return Err(WorkspaceError::Invalid(
                    "a linked workspace must carry the worktree id that owns its lease identity"
                        .to_string(),
                ));
            }
            WorkspaceKind::Linked => {}
            _ if worktree_id.is_some() => {
                return Err(WorkspaceError::Invalid(format!(
                    "a {} workspace owns its own directory and must not claim a worktree id",
                    request.kind.as_db_value()
                )));
            }
            _ => {}
        }
        if !matches!(
            request.initial_state,
            WorkspaceState::Provisioning | WorkspaceState::Active
        ) {
            return Err(WorkspaceError::Invalid(format!(
                "a workspace can only be published as provisioning or active, not {}",
                request.initial_state.as_db_value()
            )));
        }

        let path = canonical_path_string(&request.path)?;
        // §C.8 forbids a fake `active` record. A workspace may be published as
        // `provisioning` before its directory exists — that is the whole point
        // of the state — but publishing it as `active` claims it is usable, so
        // the directory must be there. Otherwise a linked acquire could occupy
        // the lease identity of a worktree that does not exist and lock the
        // real one out.
        if request.initial_state == WorkspaceState::Active && !Path::new(&path).is_dir() {
            return Err(WorkspaceError::Invalid(format!(
                "workspace directory '{path}' does not exist; publish it as provisioning and \
                 activate it once it is materialized"
            )));
        }

        Ok(Self {
            worktree_id,
            path,
            lease_owner,
            lease_ttl_ms: request.lease_ttl_ms,
            initial_state: request.initial_state,
        })
    }
}

/// SQL that resolves this repository's canonical identity INSIDE a write
/// statement (same key, same ordering, same trim/empty rules as
/// [`canonical_repo_id_with_conn`]).
///
/// Embedding it is not a micro-optimization: reading the identity with a
/// separate SELECT first would make `acquire`/`reclaim` a read-then-write
/// transaction, and two of those racing each other deadlock on the SQLite
/// lock upgrade — SQLITE_BUSY is returned immediately rather than waiting, so
/// the loser would get a transient "database is locked" instead of the stable
/// lease-held refusal §C.8 requires. With the subquery inline, the write
/// statement takes its lock first and the loser blocks, retries, and then
/// meets the unique index.
/// The identity is compared BYTE-FOR-BYTE against the value
/// [`RepoIdentity::resolve`] read (newest row wins, exactly as
/// `ConfigKv::get_with_conn` orders it). No `TRIM`/`NULLIF` normalization
/// happens here on purpose: SQLite's `TRIM` strips only spaces while Rust's
/// `str::trim` strips all Unicode whitespace, so any SQL-side normalization
/// would eventually disagree with the Rust reader and let a write land under
/// an identity no read could ever match. Cleanliness is validated once, in
/// [`RepoIdentity::resolve`].
const REPO_IDENTITY_SUBQUERY: &str = "(SELECT value FROM config_kv \
                                      WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1)";

const SELECT_COLUMNS: &str = "workspace_id, repo_id, kind, worktree_id, path, owner_kind, \
                              owner_id, task_id, session_id, base_commit, branch, state, \
                              lease_owner, lease_fence, lease_expires_at, created_at, updated_at";

/// Canonical storage spelling of a workspace directory.
///
/// The canonical-path unique index can only reject an alias if every writer
/// spells one directory the same way, so resolution is left ENTIRELY to the
/// filesystem: no lexical `..` collapsing happens first. Collapsing would be
/// wrong wherever a symlink is involved — with `/r/a/link -> /r/b/sub`, the
/// kernel resolves `/r/a/link/../ws` to `/r/b/ws`, while a lexical pass pops
/// `link` and yields `/r/a/ws`, so the same directory could be claimed twice.
///
/// Rules:
/// - The input must be ABSOLUTE. A relative path would resolve against the
///   process cwd, which RAII directory guards make a moving target (see
///   [`crate::internal::worktree_scope`]): two callers in different
///   directories would mint two canonical strings for one workspace.
/// - An existing path is resolved with [`std::fs::canonicalize`], which
///   applies symlinks and `..` in the kernel's order.
/// - The leaf may be missing (a workspace is registered as `provisioning`
///   before it is materialized), but its PARENT must canonicalize. Otherwise
///   `/r/link/ws` and `/r/target/ws` — where `link` points at a not-yet-created
///   `target` — would store different strings and both claim the one directory
///   that materialization creates.
/// - A dangling symlink leaf is refused outright: its identity is unprovable
///   until the target exists.
///
/// What this is NOT: the stored path is a canonical STRING identity captured at
/// registration, so two spellings the filesystem itself treats as one directory
/// but `canonicalize` reports differently — a case-insensitive volume reached
/// through two casings, a bind mount, a second mount of the same device — can
/// still yield two records, and a leaf re-pointed at another directory AFTER
/// registration is not re-validated. That matches the worktree registry, which
/// compares the same canonical strings, and it is within §C.8's contract: the
/// lease coordinates Agent/runtime OWNERS and does not replace a filesystem
/// lock.
fn canonical_path_string(path: &Path) -> WorkspaceResult<String> {
    if path.as_os_str().is_empty() {
        return Err(WorkspaceError::Invalid(
            "workspace path must not be empty".to_string(),
        ));
    }
    if !path.is_absolute() {
        return Err(WorkspaceError::Invalid(format!(
            "workspace path '{}' must be absolute — a cwd-relative path would key the same \
             workspace differently for each caller",
            path.display()
        )));
    }
    let canonical = match std::fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(WorkspaceError::Invalid(format!(
                    "workspace path '{}' is a dangling symlink; create its target first so the \
                     workspace registry can key it by identity",
                    path.display()
                )));
            }
            let parent = path.parent().ok_or_else(|| {
                WorkspaceError::Invalid(format!(
                    "workspace path '{}' has no parent directory",
                    path.display()
                ))
            })?;
            let file_name = path.file_name().ok_or_else(|| {
                WorkspaceError::Invalid(format!(
                    "workspace path '{}' does not name a directory",
                    path.display()
                ))
            })?;
            let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
                WorkspaceError::Invalid(format!(
                    "cannot resolve the parent directory '{}' of workspace path '{}': {error}; \
                     the parent must exist before a workspace can be registered there",
                    parent.display(),
                    path.display()
                ))
            })?;
            canonical_parent.join(file_name)
        }
        Err(error) => {
            return Err(WorkspaceError::Invalid(format!(
                "cannot resolve workspace path '{}': {error}",
                path.display()
            )));
        }
    };
    canonical.to_str().map(str::to_string).ok_or_else(|| {
        WorkspaceError::Invalid(format!(
            "workspace path '{}' is not valid UTF-8",
            canonical.display()
        ))
    })
}

/// Roll a refused transaction back and return the refusal — unless the
/// rollback ITSELF fails, in which case that is the more urgent problem: the
/// connection may still hold a write lock, so it is surfaced (with the
/// original refusal preserved in the message) rather than hidden behind a
/// tidy-looking lease error.
async fn rollback_after(
    txn: sea_orm::DatabaseTransaction,
    error: WorkspaceError,
) -> WorkspaceError {
    match txn.rollback().await {
        Ok(()) => error,
        Err(rollback) => WorkspaceError::WriteFailed(format!(
            "cannot roll back the workspace transaction after '{error}': {rollback}"
        )),
    }
}

/// How a record is named in refusals: linked workspaces by their worktree id,
/// everything else by its directory.
fn workspace_identity(record: &WorkspaceRecord) -> String {
    match (&record.kind, &record.worktree_id) {
        (WorkspaceKind::Linked, Some(worktree_id)) => format!("worktree {worktree_id}"),
        _ => record.path.clone(),
    }
}

fn optional(value: &Option<String>) -> Value {
    value.as_deref().into()
}

/// SQLite reports both plain and PARTIAL unique-index violations through the
/// same `UNIQUE constraint failed` text; sea-orm only exposes the driver
/// message, so match on it rather than on a typed code.
fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    let message = error.to_string();
    message.contains("UNIQUE constraint failed") || message.contains("code: 2067")
}

fn record_from_row(row: &sea_orm::QueryResult) -> WorkspaceResult<WorkspaceRecord> {
    fn column<T>(row: &sea_orm::QueryResult, index: usize, name: &str) -> WorkspaceResult<T>
    where
        T: sea_orm::TryGetable,
    {
        row.try_get_by_index(index).map_err(|error| {
            WorkspaceError::Corrupt(format!(
                "workspace_record column '{name}' is unreadable: {error}"
            ))
        })
    }

    let kind: String = column(row, 2, "kind")?;
    let owner_kind: String = column(row, 5, "owner_kind")?;
    let state: String = column(row, 11, "state")?;
    Ok(WorkspaceRecord {
        workspace_id: column(row, 0, "workspace_id")?,
        repo_id: column(row, 1, "repo_id")?,
        kind: WorkspaceKind::from_db_value(&kind)?,
        worktree_id: column(row, 3, "worktree_id")?,
        path: column(row, 4, "path")?,
        owner_kind: WorkspaceOwnerKind::from_db_value(&owner_kind)?,
        owner_id: column(row, 6, "owner_id")?,
        task_id: column(row, 7, "task_id")?,
        session_id: column(row, 8, "session_id")?,
        base_commit: column(row, 9, "base_commit")?,
        branch: column(row, 10, "branch")?,
        state: WorkspaceState::from_db_value(&state)?,
        lease_owner: column(row, 12, "lease_owner")?,
        lease_fence: column(row, 13, "lease_fence")?,
        lease_expires_at: column(row, 14, "lease_expires_at")?,
        created_at: column(row, 15, "created_at")?,
        updated_at: column(row, 16, "updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W0 (Codex R19/R20): no user-facing hint may direct the user at a bare
    /// `libra worktree doctor` MUTATION.
    ///
    /// Until W4 delivers the explicit `adopt` / `reclaim` / `release`
    /// subcommands, doctor is a read-only diagnostic. A hint that promises
    /// otherwise costs the user a second failure at the moment they are
    /// already stuck — and, once those subcommands DO land, this test is what
    /// forces the hints to be updated deliberately rather than by accident.
    fn lease_held_detail(path_conflict: bool) -> LeaseHeldDetail {
        LeaseHeldDetail {
            identity: "ws-identity".to_string(),
            workspace_id: None,
            holder: None,
            state: None,
            expires_at: None,
            path_conflict,
        }
    }

    #[test]
    fn doctor_hints_stay_inspect_only() {
        let forbidden = ["reclaim", "adopt", "release"];
        let mut checked = 0usize;
        for error in [
            WorkspaceError::LeaseHeld(Box::new(lease_held_detail(true))),
            WorkspaceError::LeaseHeld(Box::new(lease_held_detail(false))),
            WorkspaceError::Corrupt("workspace registry is unreadable".to_string()),
            WorkspaceError::NotFound("ws-1".to_string()),
        ] {
            let Some(hint) = error.hint() else { continue };
            checked += 1;
            if !hint.contains("worktree doctor") {
                continue;
            }
            for word in forbidden {
                assert!(
                    !hint.contains(word),
                    "hint points at a `worktree doctor` mutation that does not exist \
                     (found {word:?}): {hint}"
                );
            }
        }
        assert!(checked >= 3, "the hint surface was actually exercised");
    }

    #[test]
    fn db_spellings_round_trip() {
        for kind in [
            WorkspaceKind::Linked,
            WorkspaceKind::TaskCopy,
            WorkspaceKind::TaskFuse,
            WorkspaceKind::Remote,
        ] {
            assert_eq!(
                WorkspaceKind::from_db_value(kind.as_db_value()).expect("known kind"),
                kind
            );
        }
        for owner in [
            WorkspaceOwnerKind::Human,
            WorkspaceOwnerKind::Agent,
            WorkspaceOwnerKind::Automation,
        ] {
            assert_eq!(
                WorkspaceOwnerKind::from_db_value(owner.as_db_value()).expect("known owner kind"),
                owner
            );
        }
        for state in [
            WorkspaceState::Provisioning,
            WorkspaceState::Active,
            WorkspaceState::Releasing,
            WorkspaceState::Released,
            WorkspaceState::Orphaned,
        ] {
            assert_eq!(
                WorkspaceState::from_db_value(state.as_db_value()).expect("known state"),
                state
            );
        }
    }

    /// The Rust `holds_identity` predicate and the SQL fragment used by every
    /// conditional write must describe the same set — a drift would let a
    /// release skip a state the unique index still counts as live.
    #[test]
    fn live_state_set_matches_sql_fragment() {
        let sql = WorkspaceState::live_sql_set();
        for state in [
            WorkspaceState::Provisioning,
            WorkspaceState::Active,
            WorkspaceState::Releasing,
            WorkspaceState::Released,
            WorkspaceState::Orphaned,
        ] {
            let quoted = format!("'{}'", state.as_db_value());
            assert_eq!(
                sql.contains(&quoted),
                state.holds_identity(),
                "SQL live set disagrees with holds_identity() for {}",
                state.as_db_value()
            );
        }
    }

    /// A cwd-relative workspace path is refused outright: it would key one
    /// directory differently for every caller and slip past the active-path
    /// unique index.
    #[test]
    fn relative_and_empty_paths_are_refused() {
        assert!(matches!(
            canonical_path_string(Path::new("relative/workspace")),
            Err(WorkspaceError::Invalid(_))
        ));
        assert!(matches!(
            canonical_path_string(Path::new("")),
            Err(WorkspaceError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_db_values_are_corrupt_not_silent_defaults() {
        assert!(matches!(
            WorkspaceKind::from_db_value("bogus"),
            Err(WorkspaceError::Corrupt(_))
        ));
        assert!(matches!(
            WorkspaceState::from_db_value("bogus"),
            Err(WorkspaceError::Corrupt(_))
        ));
        assert!(matches!(
            WorkspaceOwnerKind::from_db_value("bogus"),
            Err(WorkspaceError::Corrupt(_))
        ));
    }

    #[test]
    fn linked_requests_require_a_worktree_id_and_task_requests_reject_one() {
        let mut linked = AcquireRequest::linked("wt-1", "/tmp/ws", "agent-a", 1_000);
        linked.worktree_id = None;
        assert!(matches!(
            NormalizedAcquire::from_request(&linked),
            Err(WorkspaceError::Invalid(_))
        ));

        let mut task = AcquireRequest::task(WorkspaceKind::TaskCopy, "/tmp/ws", "agent-a", 1_000);
        task.worktree_id = Some("wt-1".to_string());
        assert!(matches!(
            NormalizedAcquire::from_request(&task),
            Err(WorkspaceError::Invalid(_))
        ));
    }

    #[test]
    fn empty_identities_and_non_positive_ttl_are_refused() {
        let empty_owner = AcquireRequest::linked("wt-1", "/tmp/ws", "  ", 1_000);
        assert!(matches!(
            NormalizedAcquire::from_request(&empty_owner),
            Err(WorkspaceError::Invalid(_))
        ));

        let zero_ttl = AcquireRequest::linked("wt-1", "/tmp/ws", "agent-a", 0);
        assert!(matches!(
            NormalizedAcquire::from_request(&zero_ttl),
            Err(WorkspaceError::Invalid(_))
        ));
    }

    #[test]
    fn only_provisioning_or_active_can_be_published() {
        for state in [
            WorkspaceState::Releasing,
            WorkspaceState::Released,
            WorkspaceState::Orphaned,
        ] {
            let mut request = AcquireRequest::linked("wt-1", "/tmp/ws", "agent-a", 1_000);
            request.initial_state = state;
            assert!(
                matches!(
                    NormalizedAcquire::from_request(&request),
                    Err(WorkspaceError::Invalid(_))
                ),
                "publishing directly as {} must be refused",
                state.as_db_value()
            );
        }
    }

    /// Path aliases must collapse BEFORE the row is written — the unique index
    /// compares strings, so normalization is what makes it an alias guard.
    #[test]
    fn path_aliases_normalize_to_one_spelling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("ws");
        std::fs::create_dir(&workspace).expect("create workspace dir");

        let direct = canonical_path_string(&workspace).expect("canonical");
        let dotted = canonical_path_string(&dir.path().join("ws").join(".").join("..").join("ws"))
            .expect("canonical");
        let trailing = canonical_path_string(Path::new(&format!(
            "{}{}",
            workspace.display(),
            std::path::MAIN_SEPARATOR
        )))
        .expect("canonical");

        assert_eq!(direct, dotted);
        assert_eq!(direct, trailing);
    }

    #[test]
    fn expired_lease_is_reported_but_not_self_stealing() {
        let lease = WorkspaceLease {
            workspace_id: "ws-1".to_string(),
            owner: "agent-a".to_string(),
            fence: 3,
            expires_at: Some(1_000),
        };
        assert!(!lease.is_expired(999));
        assert!(lease.is_expired(1_000));
        assert!(lease.is_expired(1_001));
    }

    #[test]
    fn list_limit_is_clamped_to_the_documented_cap() {
        let query = WorkspaceListQuery::new();
        assert_eq!(query.effective_limit(), DEFAULT_LIST_LIMIT);
        assert_eq!(
            WorkspaceListQuery::new()
                .with_page(Some(10_000), None)
                .effective_limit(),
            MAX_LIST_LIMIT
        );
        assert_eq!(
            WorkspaceListQuery::new()
                .with_page(Some(0), None)
                .effective_limit(),
            1
        );
    }

    #[test]
    fn lease_refusals_use_agent_domain_codes() {
        let held = WorkspaceError::LeaseHeld(Box::new(LeaseHeldDetail {
            identity: "worktree wt-1".to_string(),
            workspace_id: Some("ws-1".to_string()),
            holder: Some("agent-a".to_string()),
            state: Some(WorkspaceState::Active),
            expires_at: Some(10),
            path_conflict: false,
        }));
        assert_eq!(held.stable_code(), StableErrorCode::AgentWorkspaceLeaseHeld);
        let lost = WorkspaceError::LeaseLost {
            workspace_id: "ws-1".to_string(),
            owner: "agent-a".to_string(),
            fence: 1,
        };
        assert_eq!(lost.stable_code(), StableErrorCode::AgentWorkspaceLeaseLost);
    }
}
