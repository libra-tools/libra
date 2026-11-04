//! Pi coding agent adapter (plan-20260911). Source pin: `earendil-works/pi`
//! `v1.4.0` (`4a08025`).
//!
//! PI-01 registered the identity; PI-02 adds restricted provider-root
//! discovery of session files; PI-15 keeps the discovered sessions of one
//! repository, by header `cwd`, with the session id from the header `id`;
//! PI-03 normalizes one v3 session file into its tree, disk leaf, active
//! branch, and user turns, without keeping content; PI-11 projects that
//! branch's message content, keeping thinking and tool-call blocks exactly
//! as written; PI-16 totals a session's token and cost usage over every
//! entry and attributes each assistant message to `responseModel ?? model`.
//! PI-34 turns the branch into coverage turns, and PI-32 bounds a worker
//! frame of those turns with the branch's thinking blocks under a byte
//! budget and a deadline; PI-33 runs that frame on a `pi` capture snapshot
//! (`capture/snapshot.rs`) over bytes already read. Pi stays unsupported:
//! nothing here imports a session, installs hooks, or reads a transcript for
//! capture.
//!
//! Pi writes sessions under `<agent-dir>/sessions/--<encoded-cwd>--/` by
//! default (`session-manager.ts:585-601`, `<agent-dir>` defaulting to
//! `~/.pi/agent`) and directly into an explicit session directory otherwise
//! (`session-manager.ts:1752-1756`). Discovery only walks roots inside the
//! configured provider root (`~/.pi`); a project's `.pi/settings.json`
//! `sessionDir` is read by Pi before trust is decided, so it never grants a
//! root here, and neither does any path taken from a session context.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};

use super::super::adapter::{AgentKind, AgentSessionCtx, AgentStability, ObservedAgent};

/// One deadline check of a deadline-bound stage (PI-32): returns
/// `Err(PiStop::Cancelled)` from the enclosing function once the
/// [`PiDeadline`] has elapsed.
macro_rules! poll {
    ($deadline:expr) => {
        if $deadline.elapsed() {
            return Err(PiStop::Cancelled);
        }
    };
}

/// PI-01 identity stub. The db tag is accepted, but the registry row stays
/// unsupported: discovery is internal (PI-02), and capture, hook install, and
/// launch are closed until later cards.
pub struct PiObservedAgent;

impl ObservedAgent for PiObservedAgent {
    fn provider_kind(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn provider_name(&self) -> &'static str {
        "pi"
    }

    fn stability(&self) -> AgentStability {
        AgentStability::Stable
    }

    fn read_transcript(&self, _session: &AgentSessionCtx) -> Result<Option<Vec<u8>>> {
        Err(anyhow::anyhow!(
            "pi capture is not available; the identity is registered but discovery, capture, hook install, and launch stay disabled"
        ))
    }

    fn protected_dirs(&self) -> &'static [&'static str] {
        &[".pi"]
    }
}

/// Default bound on directory entries one discovery walk inspects.
pub const PI_DISCOVERY_MAX_ENTRIES: usize = 4096;
/// Default bound on the bytes read from one session file's header line.
pub const PI_DISCOVERY_MAX_HEADER_BYTES: u64 = 64 * 1024;
/// Default bound on the bytes one discovery walk reads in total.
pub const PI_DISCOVERY_MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

/// Bounds for one discovery walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PiDiscoveryLimits {
    /// Directory entries inspected across every level of the walk.
    pub max_entries: usize,
    /// Bytes read from one session file looking for its header line.
    pub max_header_bytes: u64,
    /// Bytes read from all session files together.
    pub max_total_bytes: u64,
}

impl Default for PiDiscoveryLimits {
    fn default() -> Self {
        Self {
            max_entries: PI_DISCOVERY_MAX_ENTRIES,
            max_header_bytes: PI_DISCOVERY_MAX_HEADER_BYTES,
            max_total_bytes: PI_DISCOVERY_MAX_TOTAL_BYTES,
        }
    }
}

/// How the session files sit under the discovery root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiSessionLayout {
    /// `<root>/--<encoded-cwd>--/<file>.jsonl`, Pi's default sessions root.
    Default,
    /// `<root>/<file>.jsonl`, an explicit session directory.
    Flat,
}

/// Why a discovery root was refused before anything under it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiRootRefusal {
    /// The root is not an absolute path.
    NotAbsolute,
    /// The root contains a `..` component.
    ParentComponent,
    /// The root is not inside the configured Pi provider root (`~/.pi`).
    OutsideProviderRoot,
    /// The root, or a directory between it and the provider root, is a
    /// symlink.
    Symlink,
    /// The root exists but is not a directory.
    NotADirectory,
}

/// Why a discovery result is partial.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PiPartialReason {
    /// More entries existed than [`PiDiscoveryLimits::max_entries`].
    EntryLimit,
    /// A header or the walk needed more bytes than the limits allow.
    ByteLimit,
    /// A symlinked entry was refused without being followed or read.
    SymlinkEntry,
    /// A FIFO, device, or other non-regular entry was refused unread.
    NonRegularEntry,
    /// A directory another user owns, that its group or others can write, or
    /// (macOS) that carries an extended ACL, was not walked: an entry there
    /// could be swapped between its type check and its open.
    UnsafeDirectory,
    /// An entry vanished or changed type between enumeration and its open,
    /// or vanished before its type could be read.
    ChangedEntry,
}

/// One session file found by discovery: its path and its first line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiSessionCandidate {
    pub path: PathBuf,
    /// The header line without its newline. [`attribute_pi_sessions`] takes
    /// the session identity and repository attribution from it.
    pub header: Vec<u8>,
}

/// The result of one discovery walk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiDiscovery {
    /// Session files found, sorted by path.
    pub candidates: Vec<PiSessionCandidate>,
    /// Set when the root was refused; nothing under it was read.
    pub refused: Option<PiRootRefusal>,
    pub partial_reasons: BTreeSet<PiPartialReason>,
    /// Directory entries inspected.
    pub entries_scanned: usize,
    /// Session files read: opened and confirmed to be regular files.
    pub source_reads: usize,
    /// Bytes read from session files.
    pub bytes_read: u64,
}

impl PiDiscovery {
    /// Whether some session files may be missing from [`Self::candidates`].
    pub fn partial(&self) -> bool {
        !self.partial_reasons.is_empty()
    }

    fn refuse(reason: PiRootRefusal) -> Self {
        Self {
            refused: Some(reason),
            ..Self::default()
        }
    }
}

/// Discover Pi session files under `root`, which must be an absolute path
/// inside the configured Pi provider root. Symlinked entries are never
/// followed and non-regular entries are never read; walks past a limit stop
/// and report [`PiPartialReason`]s. A missing root yields an empty result.
pub fn discover_pi_sessions(
    root: &Path,
    layout: PiSessionLayout,
    limits: PiDiscoveryLimits,
) -> Result<PiDiscovery> {
    discover_pi_sessions_observed(root, layout, limits, &mut |_| {})
}

/// [`discover_pi_sessions`] with `before_open` called on each session file
/// just before it is opened (tests use it to swap the entry).
fn discover_pi_sessions_observed(
    root: &Path,
    layout: PiSessionLayout,
    limits: PiDiscoveryLimits,
    before_open: &mut dyn FnMut(&Path),
) -> Result<PiDiscovery> {
    if !root.is_absolute() {
        return Ok(PiDiscovery::refuse(PiRootRefusal::NotAbsolute));
    }
    if root
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Ok(PiDiscovery::refuse(PiRootRefusal::ParentComponent));
    }
    let adapter = super::super::agent_for(AgentKind::Pi);
    let normalized = super::super::transcript_source::normalize_macos_system_directory_alias(root);
    let Some(provider_root) = super::super::transcript_source::configured_provider_roots(adapter)
        .into_iter()
        .find(|provider_root| normalized.starts_with(provider_root))
    else {
        return Ok(PiDiscovery::refuse(PiRootRefusal::OutsideProviderRoot));
    };
    discover_within_provider_root(
        adapter,
        &provider_root,
        &normalized,
        layout,
        limits,
        before_open,
    )
}

/// One session of the repository: the file, the session id from its header,
/// and the header `cwd` that placed it in the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiRepoSession {
    pub path: PathBuf,
    /// The header `id`. File names are never read for it: Pi names a file
    /// `<timestamp>_<id>.jsonl` (`session-manager.ts:1079-1080`), but the
    /// header is the record.
    pub session_id: String,
    pub cwd: PathBuf,
}

/// Why a discovered session file is not one of the repository's sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PiSkipReason {
    /// The first line is a JSON object but not a Pi session header (`type`
    /// `"session"` with a string `id`). Pi does not list the file either
    /// (`session-manager.ts:677-683`).
    NotASession,
    /// The first line is blank, not JSON, repeats a key, or is not an
    /// object. Pi skips such a line and keeps scanning for a header, so the
    /// file may still be a session.
    UnreadableHeader,
    /// The header `cwd` is missing, not a string, empty, relative, or has a
    /// `.` or `..` component, so the session cannot be placed.
    NoCwd,
    /// The header `cwd` is outside the repository.
    OutsideRepo,
    /// A session of this repository whose header `id` does not match Pi's
    /// session id grammar (`session-manager.ts:268-274`).
    InvalidSessionId,
    /// A session of this repository whose header `id` another file of this
    /// repository also has; none of them is returned.
    DuplicateSessionId,
}

impl PiSkipReason {
    /// Whether a file skipped for this reason may be a session of the
    /// repository that the result leaves out.
    pub fn may_hide_a_repo_session(self) -> bool {
        !matches!(self, Self::NotASession | Self::OutsideRepo)
    }
}

/// The sessions one discovery walk found for one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiRepoSessions {
    /// The repository's sessions, sorted by path.
    pub sessions: Vec<PiRepoSession>,
    /// Discovered files left out, counted by reason.
    pub skipped: BTreeMap<PiSkipReason, usize>,
    /// The discovery walk's root refusal, if any.
    pub refused: Option<PiRootRefusal>,
    /// The discovery walk's partial reasons.
    pub partial_reasons: BTreeSet<PiPartialReason>,
}

impl PiRepoSessions {
    /// Whether some of the repository's sessions may be missing from
    /// [`Self::sessions`]: the root was refused, the walk was partial, or a
    /// skipped file may be one.
    pub fn partial(&self) -> bool {
        self.refused.is_some()
            || !self.partial_reasons.is_empty()
            || self
                .skipped
                .keys()
                .any(|reason| reason.may_hide_a_repo_session())
    }

    fn skip(&mut self, reason: PiSkipReason) {
        *self.skipped.entry(reason).or_default() += 1;
    }
}

/// Keep the discovered sessions whose header `cwd` is `repo_root` or lies
/// beneath it, with the session id taken from the header `id`. This holds
/// for both layouts: the default layout's `--<encoded-cwd>--` directory name
/// is a lossy encoding of the cwd (`session-manager.ts:589-594`), so it never
/// places a session, and an explicit directory holds sessions of any cwd
/// (Pi filters those by header `cwd` too, `session-manager.ts:1874-1910`).
///
/// `repo_root` must be absolute. A header `cwd` matches when it equals, or
/// lies beneath, the canonical repository root or `repo_root` as given; it
/// is compared lexically, component by component, and never resolved, so no
/// path a header names is touched.
pub fn attribute_pi_sessions(discovery: PiDiscovery, repo_root: &Path) -> Result<PiRepoSessions> {
    if !repo_root.is_absolute()
        || repo_root
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!(
            "the repository root {} must be an absolute path without `..`",
            repo_root.display()
        );
    }
    let canonical = repo_root.canonicalize().with_context(|| {
        format!(
            "resolve the repository root {} for Pi session attribution",
            repo_root.display()
        )
    })?;
    let roots = [without_verbatim_prefix(canonical), repo_root.to_path_buf()];
    let mut result = PiRepoSessions {
        refused: discovery.refused,
        partial_reasons: discovery.partial_reasons,
        ..PiRepoSessions::default()
    };
    let mut attributed = Vec::new();
    for candidate in discovery.candidates {
        match attribute_candidate(&candidate.header, &roots) {
            Ok((session_id, cwd)) => attributed.push(PiRepoSession {
                path: candidate.path,
                session_id,
                cwd,
            }),
            Err(reason) => result.skip(reason),
        }
    }
    let mut claims = BTreeMap::<String, usize>::new();
    for session in &attributed {
        *claims.entry(session.session_id.clone()).or_default() += 1;
    }
    for session in attributed {
        if claims.get(&session.session_id).copied().unwrap_or_default() > 1 {
            result.skip(PiSkipReason::DuplicateSessionId);
        } else {
            result.sessions.push(session);
        }
    }
    Ok(result)
}

/// The session id and cwd of one header line, or why it is not one of the
/// repository's sessions.
fn attribute_candidate(
    header: &[u8],
    roots: &[PathBuf],
) -> std::result::Result<(String, PathBuf), PiSkipReason> {
    use super::super::coverage::{CanonValue, parse_canon_value};

    let value = match parse_canon_value(header) {
        Ok(value @ CanonValue::Object(_)) => value,
        _ => return Err(PiSkipReason::UnreadableHeader),
    };
    let (Some("session"), Some(session_id)) = (
        value.get("type").and_then(CanonValue::as_str),
        value.get("id").and_then(CanonValue::as_str),
    ) else {
        return Err(PiSkipReason::NotASession);
    };
    // The raw text is checked for `.` and `..` segments: `Path::components`
    // drops an interior `.`, which would let `/repo/./sub` through.
    let cwd = value
        .get("cwd")
        .and_then(CanonValue::as_str)
        .filter(|cwd| {
            !cwd.is_empty()
                && !cwd
                    .split(std::path::is_separator)
                    .any(|segment| segment == "." || segment == "..")
        })
        .map(PathBuf::from)
        .filter(|cwd| cwd.is_absolute())
        .ok_or(PiSkipReason::NoCwd)?;
    if !roots.iter().any(|root| cwd.starts_with(root)) {
        return Err(PiSkipReason::OutsideRepo);
    }
    if !is_pi_session_id(session_id) {
        return Err(PiSkipReason::InvalidSessionId);
    }
    Ok((session_id.to_owned(), cwd))
}

/// PI-35: open one Pi session directory through the same provider-root and
/// no-follow rules as PI-02 discovery. `Ok(None)` means the path is not
/// inside the configured Pi provider root.
pub(crate) fn open_pi_provider_directory_for_source(path: &Path) -> Result<Option<std::fs::File>> {
    super::super::transcript_source::open_provider_directory_for_discovery(
        super::super::agent_for(AgentKind::Pi),
        path,
    )
}

/// PI-35: Pi's session-id grammar, shared with PI-15.
pub(crate) fn pi_session_id_is_valid(id: &str) -> bool {
    is_pi_session_id(id)
}

/// Pi's session id grammar, `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`
/// (`assertValidSessionId`, `session-manager.ts:268-274`). Pi's own ids are
/// UUIDv7; `--session-id` accepts any id of this form.
fn is_pi_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `path` in the form Pi records a cwd. On Windows `canonicalize` returns a
/// verbatim path (`\\?\C:\…`, `\\?\UNC\server\share\…`) while Pi records the
/// plain one (`C:\…`, `\\server\share\…`), so the prefix is rewritten; any
/// other verbatim form is kept and so matches no header.
#[cfg(windows)]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    use std::{ffi::OsString, path::Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    let root = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => OsString::from(format!("{}:\\", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut plain = OsString::from(r"\\");
            plain.push(server);
            plain.push(r"\");
            plain.push(share);
            plain.push(r"\");
            plain
        }
        _ => return path,
    };
    let mut plain = PathBuf::from(root);
    for component in components {
        if let Component::Normal(part) = component {
            plain.push(part);
        }
    }
    plain
}

#[cfg(not(windows))]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

/// The session file version this normalizer reads (`CURRENT_SESSION_VERSION`,
/// `session-manager.ts:41`). Pi migrates v1/v2 files in place on open; Libra
/// never rewrites a file, so it refuses them.
pub const PI_SESSION_VERSION: i64 = 3;
/// Default bound on the session file bytes one normalization reads.
pub const PI_NORMALIZE_MAX_BYTES: u64 =
    super::super::transcript_source::TRANSCRIPT_READ_HARD_CAP_BYTES;
/// Default bound on the entries one normalization indexes.
pub const PI_NORMALIZE_MAX_ENTRIES: usize = 200_000;

/// Bounds for one normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PiNormalizeLimits {
    pub max_bytes: u64,
    pub max_entries: usize,
}

impl Default for PiNormalizeLimits {
    fn default() -> Self {
        Self {
            max_bytes: PI_NORMALIZE_MAX_BYTES,
            max_entries: PI_NORMALIZE_MAX_ENTRIES,
        }
    }
}

/// The role of a `message` entry: the five roles Pi persists as `message`
/// entries (PI-21 frozen table; `custom`, `branchSummary`, and
/// `compactionSummary` are derived or stored as other entry types).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiMessageRole {
    User,
    Assistant,
    ToolResult,
    System,
    BashExecution,
    /// A role outside the PI-21 frozen table, or none.
    Unknown,
}

/// The type of one session entry (PI-20 frozen table,
/// `session-manager.ts:183-194`); `message` roles follow PI-21.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiEntryKind {
    Message(PiMessageRole),
    CustomMessage,
    Custom,
    ModelChange,
    ThinkingLevelChange,
    Usage,
    Compaction,
    ContextEdit,
    BranchSummary,
    Label,
    SessionInfo,
    /// A type outside the PI-20 frozen table, or none.
    Unknown,
}

impl PiEntryKind {
    fn from_entry(entry: &serde_json::Value) -> Self {
        Self::from_names(
            entry.get("type").and_then(serde_json::Value::as_str),
            entry
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(serde_json::Value::as_str),
        )
    }

    /// The kind of an entry with this `type` and `message.role`, each
    /// `None` when absent or not a string.
    fn from_names(kind: Option<&str>, role: Option<&str>) -> Self {
        match kind {
            Some("message") => Self::Message(match role {
                Some("user") => PiMessageRole::User,
                Some("assistant") => PiMessageRole::Assistant,
                Some("toolResult") => PiMessageRole::ToolResult,
                Some("system") => PiMessageRole::System,
                Some("bashExecution") => PiMessageRole::BashExecution,
                _ => PiMessageRole::Unknown,
            }),
            Some("custom_message") => Self::CustomMessage,
            Some("custom") => Self::Custom,
            Some("model_change") => Self::ModelChange,
            Some("thinking_level_change") => Self::ThinkingLevelChange,
            Some("usage") => Self::Usage,
            Some("compaction") => Self::Compaction,
            Some("context_edit") => Self::ContextEdit,
            Some("branch_summary") => Self::BranchSummary,
            Some("label") => Self::Label,
            Some("session_info") => Self::SessionInfo,
            _ => Self::Unknown,
        }
    }
}

/// Why a session file was refused as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiNormalizeRefusal {
    /// The input is larger than [`PiNormalizeLimits`] allow.
    TooLarge,
    /// The first parsed line is not a session header (`type` `"session"`
    /// with a session id); Pi loads such a file as empty
    /// (`session-manager.ts:660-665`).
    NotASession,
    /// The header `version` is not [`PI_SESSION_VERSION`].
    UnsupportedVersion,
    /// Two entries have the same `id`.
    DuplicateEntryId,
    /// An entry's `parentId` names no entry in the file.
    MissingParent,
    /// Following `parentId` from some entry returns to it.
    Cycle,
    /// A parsed entry Pi would index under something other than a
    /// non-empty string `id`, a parsed line that is not an object, or a
    /// `parentId` that is neither absent, null, nor a non-empty string: its
    /// place in Pi's tree cannot be reproduced.
    UnrepresentableEntry,
}

/// Why a normalized branch may not show everything the file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PiBranchPartialReason {
    /// A line `JSON.parse` rejects, or a falsy JSON value, was skipped, as
    /// Pi's loader skips it (`session-manager.ts:616-648`).
    MalformedLine,
    /// A line repeats a key; like `JSON.parse`, the last value is used.
    DuplicateKey,
    /// A second session header was skipped.
    ExtraHeader,
    /// An entry type outside the PI-20 frozen table, or none.
    UnknownEntryType,
    /// A `message` role outside the PI-21 frozen table, or none.
    UnknownMessageRole,
    /// The latest compaction's `firstKeptEntryId` is missing, or names
    /// neither the compaction itself (retain-none) nor an entry earlier on
    /// the branch; the boundary is taken at the compaction, as Pi keeps
    /// nothing before it then (`session-manager.ts:497-509`).
    CompactionBoundaryNotOnBranch,
}

/// One entry on the active branch. No content is kept: `line` points back
/// at the entry's line (1-based) in the normalized bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiBranchEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub line: usize,
    pub kind: PiEntryKind,
}

/// One user turn on the active branch: the entries from a user message up
/// to the next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiTurn {
    pub ordinal: usize,
    /// The `id` of the user message that opens the turn.
    pub opening_entry_id: String,
    /// The turn's entries, as indices into [`PiActiveBranch::entries`].
    pub entries: std::ops::Range<usize>,
    /// `Incomplete` when any entry lies before a compaction boundary: from
    /// there on Pi replaced that history with the compaction summary in the
    /// model context (`session-manager.ts:468-515`), although the raw
    /// entries stay on disk.
    pub completeness: super::super::coverage::Completeness,
    /// A line skipped as [`PiBranchPartialReason::MalformedLine`] may hold
    /// this turn's content: the latest branch entry above it in the file
    /// is in this turn, or, for the first turn, opens no turn or does not
    /// exist.
    pub skipped_lines: bool,
}

/// The v3 tree of one session file and its active branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiActiveBranch {
    /// The header `id`.
    pub session_id: String,
    /// The disk leaf: the last entry in file order (`_buildIndex`,
    /// `session-manager.ts:1103-1121`). `None` for a file with only a
    /// header.
    pub leaf_id: Option<String>,
    /// The branch from its root to the leaf.
    pub entries: Vec<PiBranchEntry>,
    /// Entries before the first user message, as indices into `entries`.
    /// They open no turn.
    pub preamble: std::ops::Range<usize>,
    pub turns: Vec<PiTurn>,
    /// Indices into `entries` before the latest compaction boundary.
    pub before_compaction: std::ops::Range<usize>,
    /// Entries in the file that are not on the active branch.
    pub off_branch_entries: usize,
    pub partial_reasons: BTreeSet<PiBranchPartialReason>,
}

impl PiActiveBranch {
    pub fn partial(&self) -> bool {
        !self.partial_reasons.is_empty()
    }
}

/// One indexed entry while the tree is built.
struct IndexedEntry {
    id: String,
    parent_id: Option<String>,
    line: usize,
    kind: PiEntryKind,
    first_kept_entry_id: Option<String>,
}

/// Normalize one Pi v3 session file: index its tree, recover the disk leaf,
/// and segment the active branch into user turns. The bytes are only read;
/// nothing is written, resolved, or logged.
pub fn normalize_pi_session(
    bytes: &[u8],
    limits: PiNormalizeLimits,
) -> std::result::Result<PiActiveBranch, PiNormalizeRefusal> {
    // INVARIANT: without a deadline the normalizer is never cancelled, so the
    // fallback refusal is never produced.
    normalize_pi_session_until(bytes, limits, &NO_DEADLINE)
        .unwrap_or(Err(PiNormalizeRefusal::TooLarge))
}

/// [`normalize_pi_session`] under a deadline (PI-32), checked before the
/// work, at every step of each pass over its lines and entries (reading
/// lines, indexing ids, linking parents, every step and finishing step of
/// the cycle walk, the branch walk,
/// the compaction and kept-entry searches, building the branch and its
/// turns, and attributing skipped lines), and once more after the branch is
/// built. `None` means the deadline elapsed; no branch is returned after it.
pub fn normalize_pi_session_until(
    bytes: &[u8],
    limits: PiNormalizeLimits,
    deadline: &dyn PiDeadline,
) -> Option<std::result::Result<PiActiveBranch, PiNormalizeRefusal>> {
    settle(normalize_session(bytes, limits, deadline))
}

fn normalize_session(
    bytes: &[u8],
    limits: PiNormalizeLimits,
    deadline: &dyn PiDeadline,
) -> std::result::Result<PiActiveBranch, PiStop<PiNormalizeRefusal>> {
    use serde_json::Value;

    use super::super::coverage::{Completeness, parse_canon_value};

    poll!(deadline);
    if bytes.len() as u64 > limits.max_bytes {
        return Err(PiNormalizeRefusal::TooLarge.into());
    }
    let mut partial_reasons = BTreeSet::new();
    let mut header: Option<String> = None;
    let mut entries: Vec<IndexedEntry> = Vec::new();
    // The first line of each run of skipped lines between two indexed
    // entries; a run has one latest branch entry above it, and there are
    // at most one more runs than entries.
    let mut skipped_runs: Vec<usize> = Vec::new();
    let mut in_skipped_run = false;
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        poll!(deadline);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // Pi's loader keeps every line `JSON.parse` accepts except a falsy
        // value, and skips the rest, before the header too
        // (`session-manager.ts:616-648`). A repeated key keeps its last value,
        // as `JSON.parse` does.
        let value = match serde_json::from_slice::<Value>(line) {
            Ok(value) if !is_js_falsy(&value) => value,
            _ => {
                partial_reasons.insert(PiBranchPartialReason::MalformedLine);
                if !in_skipped_run {
                    skipped_runs.push(index + 1);
                    in_skipped_run = true;
                }
                continue;
            }
        };
        if parse_canon_value(line).is_err() {
            partial_reasons.insert(PiBranchPartialReason::DuplicateKey);
        }
        let kind = value.get("type").and_then(Value::as_str);
        if header.is_none() {
            let Some(session_id) = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|_| kind == Some("session"))
            else {
                return Err(PiNormalizeRefusal::NotASession.into());
            };
            if value.get("version").and_then(Value::as_i64) != Some(PI_SESSION_VERSION) {
                return Err(PiNormalizeRefusal::UnsupportedVersion.into());
            }
            header = Some(session_id.to_owned());
            continue;
        }
        // `_buildIndex` skips every session-typed entry.
        if kind == Some("session") {
            partial_reasons.insert(PiBranchPartialReason::ExtraHeader);
            continue;
        }
        // `_buildIndex` indexes every other entry under its `id` and makes it
        // the leaf. One whose `id` is not a non-empty string, or whose
        // `parentId` is neither absent, null, nor a non-empty string, has no
        // place in the tree that can be reproduced, so the file is refused
        // rather than given an earlier leaf.
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(PiNormalizeRefusal::UnrepresentableEntry)?;
        let parent_id = match value.get("parentId") {
            None | Some(Value::Null) => None,
            Some(Value::String(parent)) if !parent.is_empty() => Some(parent.clone()),
            Some(_) => return Err(PiNormalizeRefusal::UnrepresentableEntry.into()),
        };
        if entries.len() >= limits.max_entries {
            return Err(PiNormalizeRefusal::TooLarge.into());
        }
        let kind = PiEntryKind::from_entry(&value);
        match kind {
            PiEntryKind::Unknown => {
                partial_reasons.insert(PiBranchPartialReason::UnknownEntryType);
            }
            PiEntryKind::Message(PiMessageRole::Unknown) => {
                partial_reasons.insert(PiBranchPartialReason::UnknownMessageRole);
            }
            _ => {}
        }
        in_skipped_run = false;
        entries.push(IndexedEntry {
            id: id.to_owned(),
            parent_id,
            line: index + 1,
            kind,
            first_kept_entry_id: value
                .get("firstKeptEntryId")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    let Some(session_id) = header else {
        return Err(PiNormalizeRefusal::NotASession.into());
    };

    let mut by_id = std::collections::HashMap::with_capacity(entries.len());
    for (position, entry) in entries.iter().enumerate() {
        poll!(deadline);
        if by_id.insert(entry.id.as_str(), position).is_some() {
            return Err(PiNormalizeRefusal::DuplicateEntryId.into());
        }
    }
    let mut parents = Vec::with_capacity(entries.len());
    for entry in &entries {
        poll!(deadline);
        parents.push(match &entry.parent_id {
            None => None,
            Some(parent) => Some(
                *by_id
                    .get(parent.as_str())
                    .ok_or(PiNormalizeRefusal::MissingParent)?,
            ),
        });
    }
    reject_parent_cycles(&parents, deadline)?;

    let mut path = Vec::new();
    let mut current = entries.len().checked_sub(1);
    while let Some(position) = current {
        poll!(deadline);
        path.push(position);
        current = parents[position];
    }
    // Root first: reversed in place, one swap per check.
    let (mut low, mut high) = (0, path.len());
    while low + 1 < high {
        poll!(deadline);
        high -= 1;
        path.swap(low, high);
        low += 1;
    }
    let off_branch_entries = entries.len() - path.len();

    // Pi's context follows the latest compaction on the branch only: the
    // history before its kept entry (earlier on the branch) is replaced by
    // the summary, and without such an entry (retain-none names the
    // compaction itself) everything before the compaction is
    // (`buildContextEntries`, `session-manager.ts:468-515`).
    let mut latest_compaction = None;
    for (index, &position) in path.iter().enumerate().rev() {
        poll!(deadline);
        if entries[position].kind == PiEntryKind::Compaction {
            latest_compaction = Some(index);
            break;
        }
    }
    let boundary = match latest_compaction {
        None => 0,
        Some(compaction) => {
            let entry = &entries[path[compaction]];
            let kept = entry.first_kept_entry_id.as_deref();
            let mut found = None;
            if let Some(kept) = kept {
                for (index, &earlier) in path[..compaction].iter().enumerate() {
                    poll!(deadline);
                    if entries[earlier].id == kept {
                        found = Some(index);
                        break;
                    }
                }
            }
            if found.is_none() && kept != Some(entry.id.as_str()) {
                partial_reasons.insert(PiBranchPartialReason::CompactionBoundaryNotOnBranch);
            }
            found.unwrap_or(compaction)
        }
    };

    let mut branch = Vec::with_capacity(path.len());
    let mut openings = Vec::new();
    for &position in &path {
        poll!(deadline);
        let entry = &entries[position];
        if entry.kind == PiEntryKind::Message(PiMessageRole::User) {
            openings.push(branch.len());
        }
        branch.push(PiBranchEntry {
            id: entry.id.clone(),
            parent_id: entry.parent_id.clone(),
            line: entry.line,
            kind: entry.kind,
        });
    }
    let preamble = 0..openings.first().copied().unwrap_or(branch.len());
    let mut turns = Vec::with_capacity(openings.len());
    for (ordinal, &start) in openings.iter().enumerate() {
        poll!(deadline);
        let end = openings.get(ordinal + 1).copied().unwrap_or(branch.len());
        turns.push(PiTurn {
            ordinal,
            opening_entry_id: branch[start].id.clone(),
            entries: start..end,
            completeness: if start < boundary {
                Completeness::Incomplete
            } else {
                Completeness::Complete
            },
            skipped_lines: false,
        });
    }
    if !skipped_runs.is_empty() && !turns.is_empty() {
        // A child may precede its parent in the file, so the branch is
        // searched by line rather than walked. The indexed entries are in
        // file order already, so the branch entries are put in line order by
        // marking each one's position and taking them in that order, without
        // a sort and in memory bounded by the entries.
        let mut on_branch = vec![None; entries.len()];
        for (index, &position) in path.iter().enumerate() {
            poll!(deadline);
            on_branch[position] = Some(index);
        }
        let mut by_line = Vec::with_capacity(branch.len());
        for (entry, index) in entries.iter().zip(on_branch) {
            poll!(deadline);
            if let Some(index) = index {
                by_line.push((entry.line, index));
            }
        }
        for line in skipped_runs {
            poll!(deadline);
            let above = by_line.partition_point(|&(entry_line, _)| entry_line < line);
            let turn = above
                .checked_sub(1)
                .and_then(|position| by_line.get(position))
                .and_then(|&(_, index)| {
                    openings
                        .partition_point(|&start| start <= index)
                        .checked_sub(1)
                })
                .unwrap_or(0);
            if let Some(turn) = turns.get_mut(turn) {
                turn.skipped_lines = true;
            }
        }
    }
    let leaf_id = branch.last().map(|entry| entry.id.clone());
    let normalized = PiActiveBranch {
        session_id,
        leaf_id,
        entries: branch,
        preamble,
        turns,
        before_compaction: 0..boundary,
        off_branch_entries,
        partial_reasons,
    };
    // Last, after everything is built, so no branch is returned after the
    // deadline.
    poll!(deadline);
    Ok(normalized)
}

/// Whether `JSON.parse` would give a value Pi's loader drops (`if (entry)`).
fn is_js_falsy(value: &serde_json::Value) -> bool {
    use serde_json::Value;

    match value {
        Value::Null | Value::Bool(false) => true,
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

/// Refuse a parent graph with a cycle. Each entry is walked toward its root
/// once; meeting an entry still on the current walk is a cycle.
fn reject_parent_cycles(
    parents: &[Option<usize>],
    deadline: &dyn PiDeadline,
) -> std::result::Result<(), PiStop<PiNormalizeRefusal>> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Unvisited,
        OnWalk,
        Done,
    }
    let mut state = vec![State::Unvisited; parents.len()];
    let mut walk = Vec::new();
    for start in 0..parents.len() {
        poll!(deadline);
        let mut current = Some(start);
        while let Some(position) = current {
            poll!(deadline);
            match state[position] {
                State::Done => break,
                State::OnWalk => return Err(PiNormalizeRefusal::Cycle.into()),
                State::Unvisited => {
                    state[position] = State::OnWalk;
                    walk.push(position);
                    current = parents[position];
                }
            }
        }
        for position in walk.drain(..) {
            poll!(deadline);
            state[position] = State::Done;
        }
    }
    Ok(())
}

/// Bound on the calls kept from one tool result's `nestedCalls`, Pi's own
/// `NESTED_CALL_LIMITS.maxCalls` (`nested-tool-calls.ts:26-31`).
pub const PI_NESTED_CALLS_MAX_CALLS: usize = 256;
/// Bound on the raw bytes kept from one tool result's `nestedCalls`. This is
/// a Libra limit on top of Pi's: Pi caps arguments at 32 KiB in total but
/// each of up to 256 errors only at 500 characters, so a record Pi wrote can
/// exceed it; past it only the count is kept, and the result is partial.
pub const PI_NESTED_CALLS_MAX_BYTES: usize = 256 * 1024;

/// A JSON value kept exactly as the session file holds it. Its `Debug`
/// shows only the length: a thinking block or signature is not echoed.
#[derive(Clone)]
pub struct PiRawJson(Box<serde_json::value::RawValue>);

/// Two raw values are equal when their text is.
impl PartialEq for PiRawJson {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Eq for PiRawJson {}

impl PiRawJson {
    /// The value's JSON text, byte for byte as in the file.
    pub fn get(&self) -> &str {
        self.0.get()
    }
}

impl std::fmt::Debug for PiRawJson {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiRawJson")
            .field("len", &self.0.get().len())
            .finish()
    }
}

/// Readable thinking text. Its `Debug` shows only the length.
#[derive(Clone, PartialEq, Eq)]
pub struct PiReasoningText(String);

impl PiReasoningText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PiReasoningText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiReasoningText")
            .field("len", &self.0.len())
            .finish()
    }
}

/// A tool result's `nestedCalls`: calls a tool made while it ran. Kept only
/// as bounded metadata of that result; they never become tool calls of the
/// turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiNestedCalls {
    /// Entries in `calls`.
    pub calls: usize,
    /// `complete`, when it is a boolean.
    pub complete: Option<bool>,
    /// The value as written, when within [`PI_NESTED_CALLS_MAX_CALLS`] and
    /// [`PI_NESTED_CALLS_MAX_BYTES`].
    pub raw: Option<PiRawJson>,
}

/// One projected piece of message content (PI-21 frozen table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PiContentRecord {
    /// User text: a string `content` or a `text` block.
    User { text: String },
    /// An assistant `text` block: the answer. Thinking never joins it.
    Assistant { text: String },
    /// An assistant `thinking` block, kept whole as written. `readable` is
    /// its `thinking` text when that is non-empty and the block is not
    /// `redacted`.
    Reasoning {
        readable: Option<PiReasoningText>,
        block: PiRawJson,
    },
    /// An assistant `toolCall` block. `block` is kept whole, so
    /// `thoughtSignature` stays exactly as written; `thought_signature` is
    /// that field alone.
    ToolCall {
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<serde_json::Value>,
        thought_signature: Option<PiRawJson>,
        block: PiRawJson,
    },
    /// A `toolResult` message: its text blocks in order, `isError` (`None`
    /// when missing or not a boolean, which is partial), `details` as
    /// written, and `nestedCalls` as bounded metadata.
    ToolResult {
        call_id: Option<String>,
        tool_name: Option<String>,
        text: Vec<String>,
        is_error: Option<bool>,
        details: Option<PiRawJson>,
        nested_calls: Option<PiNestedCalls>,
    },
    /// Readable text that opens no turn (PI-20/PI-21): a system message's
    /// content and sections, a `!` bash execution's command and output, an
    /// extension message's content, a branch or compaction summary, or a
    /// compaction's `systemMessage`. `entry` is what the text comes from, as
    /// written: the whole entry, or the compaction's `systemMessage` object.
    Context {
        kind: PiContextKind,
        text: Vec<String>,
        entry: PiRawJson,
    },
    /// Kept as written but not projected: an `image` block (unsupported in
    /// PI-20/PI-21), a block type outside the table, a whole `content` of a
    /// JSON type the message does not allow, or a whole entry
    /// ([`PiContentPartialReason::UnprojectedEntry`]).
    Unprojected { block: PiRawJson },
}

/// The entry a [`PiContentRecord::Context`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiContextKind {
    /// A `system` role message (`ai/src/types.ts:554-570`).
    System,
    /// A `bashExecution` role message (`messages.ts:29-41`).
    BashExecution,
    /// A `custom_message` entry (`session-manager.ts:150-166`).
    CustomMessage,
    /// A `branch_summary` entry's `summary`.
    BranchSummary,
    /// A `compaction` entry's `summary`.
    CompactionSummary,
}

/// Why a projection may not show all of the branch's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PiContentPartialReason {
    /// A `toolResult` whose `toolCallId` matches no `toolCall` earlier on
    /// the branch.
    OrphanToolResult,
    /// A `nestedCalls` whose `complete` is `false` or not a boolean: Pi
    /// dropped calls, omitted arguments, or saw calls unfinished
    /// (`ai/src/types.ts:627-631`).
    NestedCallsIncomplete,
    /// A `nestedCalls` over [`PI_NESTED_CALLS_MAX_CALLS`] or
    /// [`PI_NESTED_CALLS_MAX_BYTES`], or without a `calls` array; only its
    /// count is kept.
    NestedCallsOverBound,
    /// An `image` block, unsupported in PI-20/PI-21; kept unprojected.
    UnsupportedBlock,
    /// A block type outside the table, a block that is not an object or
    /// has an ill-typed `text`, or a `content` of a JSON type the message
    /// does not allow; the block, or the whole `content`, is kept
    /// unprojected.
    UnknownBlock,
    /// A `message` entry with an unknown role or without a `message`
    /// object, or an entry type outside the PI-20 table: kept whole as
    /// [`PiContentRecord::Unprojected`].
    UnprojectedEntry,
    /// A field of a projected record is missing, `null`, or of the wrong JSON
    /// type:
    /// a thinking block's `thinking` or `redacted` (its text is then not
    /// readable), a tool call's `id`, `name`, or `arguments`, a tool
    /// result's `toolCallId`, `toolName`, or `isError`, a bash execution's
    /// `command` or `output`, a summary, system `sections`, a compaction's
    /// `systemMessage`, or a `null` `nestedCalls`.
    MalformedField,
}

/// The projected content of one turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiProjectedTurn {
    pub ordinal: usize,
    pub completeness: super::super::coverage::Completeness,
    pub records: Vec<PiContentRecord>,
    /// Each of the turn's entries that has records, in branch order: its
    /// index into [`PiActiveBranch::entries`] and its records as a range
    /// into `records`. The blocks of one message share one range.
    pub entry_records: Vec<(usize, std::ops::Range<usize>)>,
}

/// The projected content of an active branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiContentProjection {
    /// Content before the first user message, which opens no turn.
    pub preamble: Vec<PiContentRecord>,
    pub turns: Vec<PiProjectedTurn>,
    /// Reasons of this projection; the branch's own
    /// [`PiActiveBranch::partial_reasons`] apply too.
    pub partial_reasons: BTreeSet<PiContentPartialReason>,
}

impl PiContentProjection {
    pub fn partial(&self) -> bool {
        !self.partial_reasons.is_empty()
    }
}

/// Why [`normalize_pi_turns`] gives no turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiCoverageError {
    /// The normalizer refused the file.
    Refused(PiNormalizeRefusal),
    /// The file did not project over its own branch, which the normalizer
    /// built from the same bytes; not expected.
    Projection(PiProjectionError),
}

/// Why `bytes` cannot be projected over a branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiProjectionError {
    /// A branch entry's line is missing from `bytes` or holds another
    /// entry: the bytes are not the ones the branch was normalized from.
    BranchMismatch,
}

/// One JSON object's members as written: each key's last value (as
/// `JSON.parse` keeps it), borrowed byte for byte from the line.
type RawFields<'a> = BTreeMap<String, &'a serde_json::value::RawValue>;

fn raw_fields(text: &str) -> Option<RawFields<'_>> {
    serde_json::from_str(text).ok()
}

/// A member as written, `null` included.
fn member<'a>(fields: &RawFields<'a>, name: &str) -> Option<&'a serde_json::value::RawValue> {
    fields.get(name).copied()
}

/// A message's `content`. Pi reads a `null` content as empty
/// (`sessionEntryToContextMessages`, `session-manager.ts:439-453`).
fn content_member<'a>(fields: &RawFields<'a>) -> Option<&'a serde_json::value::RawValue> {
    member(fields, "content").filter(|raw| raw.get() != "null")
}

/// A member's value of type `T`: `Ok(None)` when absent, `Err(())` when it
/// is `null` or another JSON type.
fn typed_field<T: serde::de::DeserializeOwned>(
    fields: &RawFields<'_>,
    name: &str,
) -> std::result::Result<Option<T>, ()> {
    member(fields, name)
        .map(|raw| serde_json::from_str::<T>(raw.get()).map_err(|_| ()))
        .transpose()
}

/// A required string member; a missing or ill-typed one is partial.
fn required_string(
    fields: &RawFields<'_>,
    name: &str,
    partial: &mut BTreeSet<PiContentPartialReason>,
) -> Option<String> {
    match typed_field::<String>(fields, name) {
        Ok(Some(value)) => Some(value),
        Ok(None) | Err(()) => {
            partial.insert(PiContentPartialReason::MalformedField);
            None
        }
    }
}

/// Project the content of `branch`, read from the same `bytes` it was
/// normalized from. Messages: user text; assistant answers; each thinking
/// block as reasoning, kept whole and apart from the answer; tool calls,
/// kept whole; tool results with `nestedCalls` as bounded metadata. Readable
/// entries that open no turn (system and bash-execution messages, extension
/// messages, branch and compaction summaries) become
/// [`PiContentRecord::Context`]. Every kept value is the file's own bytes,
/// under a repeated key too (the last value, as `JSON.parse` keeps it).
/// `context_edit` is not applied: Pi's replacement changes only later model
/// context, never the target's recorded text
/// (`session-manager.ts:519-541,549-553`).
pub fn project_pi_content(
    bytes: &[u8],
    branch: &PiActiveBranch,
) -> std::result::Result<PiContentProjection, PiProjectionError> {
    // INVARIANT: without a deadline the projection is never cancelled, so
    // the fallback error is never produced.
    project_pi_content_until(bytes, branch, &NO_DEADLINE)
        .unwrap_or(Err(PiProjectionError::BranchMismatch))
}

/// [`project_pi_content`] under a deadline (PI-32), checked before the work,
/// at every branch entry and every line of the bytes as it indexes the
/// branch's lines, every turn and turn entry it sets up,
/// every branch entry it reads, every content block of an entry, and right
/// before it returns the projection. `None` means the deadline elapsed; no
/// projection is returned after it.
pub fn project_pi_content_until(
    bytes: &[u8],
    branch: &PiActiveBranch,
    deadline: &dyn PiDeadline,
) -> Option<std::result::Result<PiContentProjection, PiProjectionError>> {
    settle(project_content(bytes, branch, deadline))
}

fn project_content(
    bytes: &[u8],
    branch: &PiActiveBranch,
    deadline: &dyn PiDeadline,
) -> std::result::Result<PiContentProjection, PiStop<PiProjectionError>> {
    poll!(deadline);
    let lines = branch_lines(bytes, branch, deadline)?;
    let mut turns = Vec::with_capacity(branch.turns.len());
    for turn in &branch.turns {
        poll!(deadline);
        turns.push(PiProjectedTurn {
            ordinal: turn.ordinal,
            completeness: turn.completeness,
            records: Vec::new(),
            entry_records: Vec::new(),
        });
    }
    let mut projection = PiContentProjection {
        preamble: Vec::new(),
        turns,
        partial_reasons: BTreeSet::new(),
    };
    // The turn of each branch entry; `None` for the preamble.
    let mut owner = vec![None; branch.entries.len()];
    for (ordinal, turn) in branch.turns.iter().enumerate() {
        for slot in owner.get_mut(turn.entries.clone()).into_iter().flatten() {
            poll!(deadline);
            *slot = Some(ordinal);
        }
    }
    let mut call_ids = BTreeSet::new();
    for (index, entry) in branch.entries.iter().enumerate() {
        poll!(deadline);
        let (text, fields) = branch_entry_line(&lines, entry)?;
        let whole = || {
            serde_json::from_str::<Box<serde_json::value::RawValue>>(text)
                .map(PiRawJson)
                .map_err(|_| PiProjectionError::BranchMismatch)
        };
        let partial = &mut projection.partial_reasons;
        let mut records = Vec::new();
        match entry.kind {
            PiEntryKind::Message(role) => {
                let message = member(&fields, "message").and_then(|raw| raw_fields(raw.get()));
                match (role, message) {
                    // An unknown role, or no `message` object: kept whole and
                    // unprojected.
                    (PiMessageRole::Unknown, _) | (_, None) => {
                        partial.insert(PiContentPartialReason::UnprojectedEntry);
                        records.push(PiContentRecord::Unprojected { block: whole()? });
                    }
                    (PiMessageRole::User, Some(message)) => {
                        // Text and kept blocks stay in block order.
                        for piece in content_pieces(content_member(&message), partial, deadline)? {
                            records.push(match piece {
                                ContentPiece::Text(text) => PiContentRecord::User { text },
                                ContentPiece::Kept(record) => record,
                            });
                        }
                    }
                    (PiMessageRole::Assistant, Some(message)) => project_assistant(
                        content_member(&message),
                        &mut records,
                        &mut call_ids,
                        partial,
                        deadline,
                    )?,
                    (PiMessageRole::ToolResult, Some(message)) => {
                        project_tool_result(&message, &mut records, &call_ids, partial, deadline)?
                    }
                    (PiMessageRole::System, Some(message)) => {
                        system_context(&message, whole()?, &mut records, partial, deadline)?
                    }
                    (PiMessageRole::BashExecution, Some(message)) => {
                        let text = ["command", "output"]
                            .into_iter()
                            .filter_map(|name| required_string(&message, name, partial))
                            .collect();
                        records.push(PiContentRecord::Context {
                            kind: PiContextKind::BashExecution,
                            text,
                            entry: whole()?,
                        });
                    }
                }
            }
            PiEntryKind::CustomMessage => {
                let mut trailing = Vec::new();
                let text = content_text(content_member(&fields), &mut trailing, partial, deadline)?;
                records.push(PiContentRecord::Context {
                    kind: PiContextKind::CustomMessage,
                    text,
                    entry: whole()?,
                });
                records.extend(trailing);
            }
            PiEntryKind::BranchSummary | PiEntryKind::Compaction => {
                let kind = if entry.kind == PiEntryKind::Compaction {
                    // Pi puts a compaction's `systemMessage` before its summary
                    // (`session-manager.ts:461-464`).
                    match member(&fields, "systemMessage") {
                        None => {}
                        Some(system) => match raw_fields(system.get()) {
                            Some(system_fields) => system_context(
                                &system_fields,
                                PiRawJson(system.to_owned()),
                                &mut records,
                                partial,
                                deadline,
                            )?,
                            None => {
                                partial.insert(PiContentPartialReason::MalformedField);
                            }
                        },
                    }
                    PiContextKind::CompactionSummary
                } else {
                    PiContextKind::BranchSummary
                };
                let text = required_string(&fields, "summary", partial)
                    .into_iter()
                    .collect();
                records.push(PiContentRecord::Context {
                    kind,
                    text,
                    entry: whole()?,
                });
            }
            // A type outside the PI-20 table may hold content: kept whole.
            PiEntryKind::Unknown => {
                partial.insert(PiContentPartialReason::UnprojectedEntry);
                records.push(PiContentRecord::Unprojected { block: whole()? });
            }
            // Metadata entries (PI-20): no readable content.
            PiEntryKind::Custom
            | PiEntryKind::ModelChange
            | PiEntryKind::ThinkingLevelChange
            | PiEntryKind::Usage
            | PiEntryKind::ContextEdit
            | PiEntryKind::Label
            | PiEntryKind::SessionInfo => continue,
        }
        match owner.get(index).copied().flatten() {
            Some(turn) if !records.is_empty() => {
                let turn = &mut projection.turns[turn];
                let start = turn.records.len();
                turn.records.extend(records);
                turn.entry_records.push((index, start..turn.records.len()));
            }
            Some(_) => {}
            None => projection.preamble.extend(records),
        }
    }
    poll!(deadline);
    Ok(projection)
}

/// The lines of `bytes` that hold the branch's entries, by 1-based line
/// number; `None` for a line the bytes do not have.
type BranchLines<'a> = std::collections::HashMap<usize, Option<&'a [u8]>>;

/// Read the lines of `bytes` that hold the branch's entries under the
/// deadline (PI-32): one check per branch entry, then one per line of the
/// bytes. Only those lines are kept, so blank or skipped lines never grow
/// the index.
fn branch_lines<'a, E>(
    bytes: &'a [u8],
    branch: &PiActiveBranch,
    deadline: &dyn PiDeadline,
) -> std::result::Result<BranchLines<'a>, PiStop<E>> {
    let mut lines = BranchLines::with_capacity(branch.entries.len());
    for entry in &branch.entries {
        poll!(deadline);
        lines.insert(entry.line, None);
    }
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        poll!(deadline);
        if let Some(slot) = lines.get_mut(&(index + 1)) {
            *slot = Some(line);
        }
    }
    Ok(lines)
}

/// The line of a branch entry, with its fields, checked to hold that
/// entry: other bytes than the branch was normalized from fail.
fn branch_entry_line<'a>(
    lines: &BranchLines<'a>,
    entry: &PiBranchEntry,
) -> std::result::Result<(&'a str, RawFields<'a>), PiProjectionError> {
    let text = lines
        .get(&entry.line)
        .copied()
        .flatten()
        .and_then(|line| std::str::from_utf8(line).ok())
        .ok_or(PiProjectionError::BranchMismatch)?;
    let fields = raw_fields(text).ok_or(PiProjectionError::BranchMismatch)?;
    let string =
        |fields: &RawFields<'_>, name: &str| typed_field::<String>(fields, name).ok().flatten();
    let role = member(&fields, "message")
        .and_then(|message| raw_fields(message.get()))
        .and_then(|message| string(&message, "role"));
    if string(&fields, "id").as_deref() != Some(entry.id.as_str())
        || PiEntryKind::from_names(string(&fields, "type").as_deref(), role.as_deref())
            != entry.kind
    {
        return Err(PiProjectionError::BranchMismatch);
    }
    Ok((text, fields))
}

/// The Pi coverage normalizer (PI-34): one
/// [`NormalizedTurn`](super::super::coverage::NormalizedTurn) per turn of
/// the active branch, in PI-03 order with PI-03's ordinal, holding the
/// PI-11 records of the turn in coverage-v1 form (`coverage-v1.md` §2–§6).
///
/// The text pieces of one message join with `\n` into one user or
/// assistant record (§3), present whenever the message has a text piece,
/// even an empty one. An assistant message's joined text comes first,
/// then its readable thinking and tool calls in block order, as for Claude
/// Code. Readable thinking becomes a
/// [`SemanticRecord::Reasoning`](super::super::coverage::SemanticRecord::Reasoning) of
/// provider Pi, never assistant text; redacted or empty thinking has no
/// record. Context entries (system and bash execution messages, extension
/// messages, branch and compaction summaries) have none either.
///
/// A turn is `Complete` only when PI-03 says so, no skipped line may hold
/// its content ([`PiTurn::skipped_lines`], §6), and each of its user,
/// assistant, and tool result messages was read faithfully. Any of these
/// makes it `Incomplete`: a duplicate key in the message's line (§4.6),
/// whether or not the message has records, a block or entry kept
/// unprojected, a thinking block with an ill-typed `thinking` or
/// `redacted`, a tool call without a string `id` and `name` or an object
/// `arguments`, fractional or beyond-i64 numbers in `arguments` (which
/// become `null`, §4.5), or a tool result without a string `toolCallId` or
/// a boolean `isError`. Tool result metadata (`toolName`, `details`,
/// `nestedCalls`) and an unmatched `toolCallId` do not count.
///
/// The turn key is the opening user message's entry id when it is a valid
/// provider turn id, else the ordinal key. The branch, its turns, and
/// their records all come from `bytes` ([`normalize_pi_session`] under
/// `limits`, then [`project_pi_content`]); a file the normalizer refuses
/// gives its refusal.
pub fn normalize_pi_turns(
    bytes: &[u8],
    limits: PiNormalizeLimits,
) -> std::result::Result<Vec<super::super::coverage::NormalizedTurn>, PiCoverageError> {
    let branch = normalize_pi_session(bytes, limits).map_err(PiCoverageError::Refused)?;
    let projection = project_pi_content(bytes, &branch).map_err(PiCoverageError::Projection)?;
    // INVARIANT: without a deadline the turns are never cancelled, so the
    // fallback error is never produced.
    settle(coverage_turns(bytes, &branch, &projection, &NO_DEADLINE)).unwrap_or(Err(
        PiCoverageError::Projection(PiProjectionError::BranchMismatch),
    ))
}

/// The coverage turns of [`normalize_pi_turns`] from a branch and its
/// projection of the same bytes, under a deadline checked at every branch
/// entry and every line of the bytes as it indexes the branch's lines, at
/// every turn, every entry of a turn, every record of an entry, and right
/// before the turns are returned.
fn coverage_turns(
    bytes: &[u8],
    branch: &PiActiveBranch,
    projection: &PiContentProjection,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Vec<super::super::coverage::NormalizedTurn>, PiStop<PiCoverageError>> {
    use super::super::{
        coverage::{
            CanonValue, Completeness, NormalizedTurn, SemanticRecord, ordinal_key,
            parse_canon_value, valid_provider_turn_id,
        },
        reasoning::{ReasoningProvider, ReasoningSourceKind, classify_provider_visible_reasoning},
    };

    let lines = branch_lines(bytes, branch, deadline)?;
    let mut turns = Vec::with_capacity(branch.turns.len());
    for (turn, projected) in branch.turns.iter().zip(&projection.turns) {
        poll!(deadline);
        let mut complete = turn.completeness == Completeness::Complete && !turn.skipped_lines;
        let mut records = Vec::new();
        // Every entry of the turn, with its records (none for most
        // metadata entries and for an empty message).
        let mut spans = projected.entry_records.iter().peekable();
        for index in turn.entries.clone() {
            poll!(deadline);
            let range = spans
                .next_if(|(entry, _)| *entry == index)
                .map_or(0..0, |(_, range)| range.clone());
            let mismatch = PiCoverageError::Projection(PiProjectionError::BranchMismatch);
            let entry = branch.entries.get(index).ok_or(mismatch)?;
            let pieces = projected.records.get(range).ok_or(mismatch)?;
            match entry.kind {
                PiEntryKind::Message(
                    PiMessageRole::User | PiMessageRole::Assistant | PiMessageRole::ToolResult,
                ) => {}
                // An entry outside the tables may hold turn content.
                PiEntryKind::Message(PiMessageRole::Unknown) | PiEntryKind::Unknown => {
                    complete = false;
                    continue;
                }
                // Context: no coverage record.
                _ => continue,
            }
            let (line, _) =
                branch_entry_line(&lines, entry).map_err(PiCoverageError::Projection)?;
            // coverage-v1.md §4.6: a duplicate key is a parse rejection,
            // never "last one wins".
            if parse_canon_value(line.as_bytes()).is_err() {
                complete = false;
            }
            let mut user = Vec::new();
            let mut assistant = Vec::new();
            let mut ordered = Vec::new();
            for piece in pieces {
                poll!(deadline);
                match piece {
                    PiContentRecord::User { text } => user.push(text.as_str()),
                    PiContentRecord::Assistant { text } => assistant.push(text.as_str()),
                    PiContentRecord::Reasoning {
                        readable: Some(text),
                        ..
                    } => {
                        let (_, text) = classify_provider_visible_reasoning(
                            ReasoningProvider::Pi,
                            ReasoningSourceKind::Thinking,
                            text.as_str().to_owned(),
                        );
                        ordered.push(SemanticRecord::Reasoning {
                            provider: ReasoningProvider::Pi,
                            source_kind: Some(ReasoningSourceKind::Thinking),
                            text,
                        });
                    }
                    // Redacted or empty thinking has no record; an
                    // ill-typed one was not read faithfully.
                    PiContentRecord::Reasoning {
                        readable: None,
                        block,
                    } => {
                        if raw_fields(block.get())
                            .is_none_or(|fields| readable_thinking(&fields).is_err())
                        {
                            complete = false;
                        }
                    }
                    PiContentRecord::ToolCall {
                        call_id,
                        name,
                        block,
                        ..
                    } => {
                        let arguments =
                            parse_canon_value(block.get().as_bytes())
                                .ok()
                                .and_then(|block| match block {
                                    CanonValue::Object(mut fields) => fields.remove("arguments"),
                                    _ => None,
                                });
                        let input = match arguments {
                            Some(mut input @ CanonValue::Object(_)) => {
                                if sanitize_floats_until(&mut input, deadline)? {
                                    complete = false;
                                }
                                input
                            }
                            _ => {
                                complete = false;
                                CanonValue::Null
                            }
                        };
                        if call_id.is_none() || name.is_none() {
                            complete = false;
                        }
                        ordered.push(SemanticRecord::ToolCall {
                            call_id: call_id.clone(),
                            input,
                            name: name.clone().unwrap_or_default(),
                        });
                    }
                    PiContentRecord::ToolResult {
                        call_id,
                        text,
                        is_error,
                        ..
                    } => {
                        if call_id.is_none() || is_error.is_none() {
                            complete = false;
                        }
                        ordered.push(SemanticRecord::ToolResult {
                            call_id: call_id.clone(),
                            content: text.join("\n"),
                            is_error: is_error.unwrap_or(false),
                        });
                    }
                    // An image or unknown block: content the records do not
                    // hold. (A context record cannot come from these roles.)
                    PiContentRecord::Unprojected { .. } | PiContentRecord::Context { .. } => {
                        complete = false;
                    }
                }
            }
            if !user.is_empty() {
                records.push(SemanticRecord::User {
                    text: user.join("\n"),
                });
            }
            // A text block keeps its record even when empty, as a user's
            // does: it differs from a message without text.
            if !assistant.is_empty() {
                records.push(SemanticRecord::Assistant {
                    text: assistant.join("\n"),
                });
            }
            records.extend(ordered);
        }
        turns.push(NormalizedTurn {
            logical_turn_key: if valid_provider_turn_id(&turn.opening_entry_id) {
                turn.opening_entry_id.clone()
            } else {
                ordinal_key(turn.ordinal)
            },
            ordinal: turn.ordinal,
            completeness: if complete {
                Completeness::Complete
            } else {
                Completeness::Incomplete
            },
            started_at: None,
            ended_at: None,
            records,
        });
    }
    poll!(deadline);
    Ok(turns)
}

/// Replace every fractional or beyond-i64 number in `value` with `null` and
/// report whether there was one, as `coverage::contains_float` followed by
/// `coverage::sanitize_floats` do (coverage-v1.md §4.5), checking the
/// deadline at every node of the tree (PI-32). A container's children are
/// taken one at a time from its iterator, so no child is reached without a
/// check before it.
fn sanitize_floats_until(
    value: &mut super::super::coverage::CanonValue,
    deadline: &dyn PiDeadline,
) -> std::result::Result<bool, PiStop<PiCoverageError>> {
    use super::super::coverage::CanonValue;

    /// The children of a container not yet visited.
    enum Children<'a> {
        Items(std::slice::IterMut<'a, CanonValue>),
        Fields(std::collections::btree_map::ValuesMut<'a, String, CanonValue>),
    }

    /// Visit one node: replace a fractional number, or give a container's
    /// children.
    fn visit<'a>(node: &'a mut CanonValue, found: &mut bool) -> Option<Children<'a>> {
        match node {
            CanonValue::Float(_) => {
                *node = CanonValue::Null;
                *found = true;
                None
            }
            CanonValue::Array(items) => Some(Children::Items(items.iter_mut())),
            CanonValue::Object(fields) => Some(Children::Fields(fields.values_mut())),
            CanonValue::Null | CanonValue::Bool(_) | CanonValue::Int(_) | CanonValue::Str(_) => {
                None
            }
        }
    }

    let mut found = false;
    let mut open = Vec::new();
    poll!(deadline);
    open.extend(visit(value, &mut found));
    while let Some(children) = open.last_mut() {
        let next = match children {
            Children::Items(items) => items.next(),
            Children::Fields(fields) => fields.next(),
        };
        match next {
            None => {
                open.pop();
            }
            Some(child) => {
                poll!(deadline);
                open.extend(visit(child, &mut found));
            }
        }
    }
    Ok(found)
}

/// Bound for the bytes one Pi worker frame reads. The frame reads redacted
/// bytes, which can be longer than the raw file: this is the redacted
/// output bound of a transcript at the raw read cap (1.5×, the bound the
/// capture extraction helper also gives its parent).
pub const PI_WORKER_FRAME_MAX_BYTES: u64 = PI_NORMALIZE_MAX_BYTES / 2 * 3;

/// Closed readable tag for the Pi ordinary worker: the tag of Pi's readable
/// reasoning provider. It does not change any other provider's tag.
pub fn pi_readable_tag() -> &'static str {
    "pi"
}

/// One assistant `thinking` block of the active branch, carrying exactly
/// the fields the PI-21 frozen table gives a thinking block (AC10):
/// `thinking`, `thinkingSignature`, and `redacted`, each byte for byte as
/// written (the last value of a repeated key, as `JSON.parse` keeps it). No
/// other field of the block is carried. `Debug` shows only lengths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiRetainedThinking {
    /// The logical key of the ordinary turn that holds the block; `None`
    /// before the first user message, which opens no turn.
    pub turn_key: Option<String>,
    pub thinking: Option<PiRawJson>,
    pub thinking_signature: Option<PiRawJson>,
    pub redacted: Option<PiRawJson>,
    /// The readable text, as [`PiContentRecord::Reasoning`] gives it.
    pub readable: Option<PiReasoningText>,
}

/// One bounded ordinary frame (PI-32): the coverage turns of
/// [`normalize_pi_turns`] and the thinking blocks of the same active
/// branch, in source order. Thinking is carried beside the answer, never
/// in it. `acquisition_gate_passed` stays false until a real session
/// comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiWorkerFrame {
    pub tag: &'static str,
    pub source_state: &'static str,
    pub partial: bool,
    pub acquisition_gate_passed: bool,
    pub ordinary: Vec<super::super::coverage::NormalizedTurn>,
    pub thinking: Vec<PiRetainedThinking>,
}

/// Content-free worker failure. No byte of the input is copied into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PiWorkerError {
    /// The input is larger than [`PI_WORKER_FRAME_MAX_BYTES`], or has more
    /// entries than [`PI_NORMALIZE_MAX_ENTRIES`].
    Budget,
    /// The deadline elapsed before the frame was complete.
    Cancelled,
    /// The normalizer refused the bytes, or they did not project.
    BadFrame,
}

impl std::fmt::Display for PiWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Budget => "pi worker frame exceeded its budget",
            Self::Cancelled => "pi worker frame was cancelled",
            Self::BadFrame => "pi worker frame is unusable",
        })
    }
}

/// The worker frame of bytes that were already authorized (PI-32). The
/// deadline is checked before the work (so an elapsed deadline wins over the
/// budget), at every step of the normalizer's passes, every branch entry the
/// projection reads, every turn and entry the coverage records come from,
/// every record whose thinking may be carried, and once more after the frame
/// is built, so no frame is returned after it.
pub fn pi_worker_frame_authorized(
    bytes: &[u8],
    deadline: Option<std::time::Instant>,
) -> std::result::Result<PiWorkerFrame, PiWorkerError> {
    pi_worker_frame_with_deadline(bytes, &deadline)
}

/// [`pi_worker_frame_authorized`] under any [`PiDeadline`].
pub fn pi_worker_frame_with_deadline(
    bytes: &[u8],
    deadline: &dyn PiDeadline,
) -> std::result::Result<PiWorkerFrame, PiWorkerError> {
    if deadline.elapsed() {
        return Err(PiWorkerError::Cancelled);
    }
    if bytes.len() as u64 > PI_WORKER_FRAME_MAX_BYTES {
        return Err(PiWorkerError::Budget);
    }
    let limits = PiNormalizeLimits {
        max_bytes: PI_WORKER_FRAME_MAX_BYTES,
        ..PiNormalizeLimits::default()
    };
    let branch = match normalize_session(bytes, limits, deadline) {
        Ok(branch) => branch,
        Err(PiStop::Cancelled) => return Err(PiWorkerError::Cancelled),
        Err(PiStop::Failed(PiNormalizeRefusal::TooLarge)) => return Err(PiWorkerError::Budget),
        Err(PiStop::Failed(_)) => return Err(PiWorkerError::BadFrame),
    };
    let projection = project_content(bytes, &branch, deadline).map_err(worker_error)?;
    let ordinary = coverage_turns(bytes, &branch, &projection, deadline).map_err(worker_error)?;
    let mut thinking = Vec::new();
    let sources = std::iter::once((None, projection.preamble.as_slice())).chain(
        projection
            .turns
            .iter()
            .zip(&ordinary)
            .map(|(turn, covered)| {
                (
                    Some(covered.logical_turn_key.as_str()),
                    turn.records.as_slice(),
                )
            }),
    );
    for (turn_key, records) in sources {
        for record in records {
            if deadline.elapsed() {
                return Err(PiWorkerError::Cancelled);
            }
            let PiContentRecord::Reasoning { readable, block } = record else {
                continue;
            };
            // The block came from `raw_fields`, so it is an object.
            let fields = raw_fields(block.get()).ok_or(PiWorkerError::BadFrame)?;
            let carried = |name: &str| member(&fields, name).map(|raw| PiRawJson(raw.to_owned()));
            thinking.push(PiRetainedThinking {
                turn_key: turn_key.map(str::to_owned),
                thinking: carried("thinking"),
                thinking_signature: carried("thinkingSignature"),
                redacted: carried("redacted"),
                readable: readable.clone(),
            });
        }
    }
    let frame = PiWorkerFrame {
        tag: pi_readable_tag(),
        source_state: "unproven",
        partial: true,
        acquisition_gate_passed: false,
        ordinary,
        thinking,
    };
    if deadline.elapsed() {
        return Err(PiWorkerError::Cancelled);
    }
    Ok(frame)
}

/// A deadline the Pi stages check (PI-32): before their work, once per item
/// of every loop that reads, parses, or walks the input (lines, entries,
/// parent-chain steps, turns, content blocks, and records, and the items of
/// an array or object nested in an entry as they are parsed: content
/// blocks, system sections, and nested calls; and every node of a parsed
/// tree that is walked, such as a tool call's arguments), and once more
/// after the result is built. Parsing one JSON value read from an entry's
/// line (the line, or a member of it) is one step, as an entry is the unit
/// of the stages. A loop that only moves values a checked loop already
/// produced is not checked again. The production form is
/// `Option<Instant>`, where `None` never elapses; a test may implement it to
/// elapse at a chosen check. It must stay elapsed once it has elapsed.
pub trait PiDeadline {
    /// Whether the deadline has elapsed. Called once per check.
    fn elapsed(&self) -> bool;
}

impl PiDeadline for Option<std::time::Instant> {
    fn elapsed(&self) -> bool {
        self.is_some_and(|deadline| std::time::Instant::now() >= deadline)
    }
}

/// The deadline of the forms that take none.
const NO_DEADLINE: Option<std::time::Instant> = None;

/// Why a deadline-bound stage stopped: its own error, or the deadline.
enum PiStop<E> {
    Failed(E),
    Cancelled,
}

impl<E> From<E> for PiStop<E> {
    fn from(error: E) -> Self {
        Self::Failed(error)
    }
}

/// A projection or coverage stage's stop as a worker error: the bytes
/// already normalized, so any failure of these stages is a bad frame.
fn worker_error<E>(stop: PiStop<E>) -> PiWorkerError {
    match stop {
        PiStop::Cancelled => PiWorkerError::Cancelled,
        PiStop::Failed(_) => PiWorkerError::BadFrame,
    }
}

/// A stage's result for its public `_until` form: `None` when cancelled.
fn settle<T, E>(result: std::result::Result<T, PiStop<E>>) -> Option<std::result::Result<T, E>> {
    match result {
        Ok(value) => Some(Ok(value)),
        Err(PiStop::Failed(error)) => Some(Err(error)),
        Err(PiStop::Cancelled) => None,
    }
}

/// A system message's content and sections as one context record.
fn system_context(
    message: &RawFields<'_>,
    entry: PiRawJson,
    records: &mut Vec<PiContentRecord>,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<(), PiStop<PiProjectionError>> {
    let mut trailing = Vec::new();
    let mut text = content_text(content_member(message), &mut trailing, partial, deadline)?;
    text.extend(system_sections(
        member(message, "sections"),
        partial,
        deadline,
    )?);
    records.push(PiContentRecord::Context {
        kind: PiContextKind::System,
        text,
        entry,
    });
    records.extend(trailing);
    Ok(())
}

/// The blocks of a `content` array, each kept as written and parsed one at
/// a time under the deadline (PI-32); `Ok(None)` when the content is not an
/// array.
fn content_blocks<'a>(
    content: &'a serde_json::value::RawValue,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Option<Vec<&'a serde_json::value::RawValue>>, PiStop<PiProjectionError>> {
    struct Blocks<'c, 'd>(&'c ItemChecks<'d>);

    impl<'de> serde::de::Visitor<'de> for Blocks<'_, '_> {
        type Value = Vec<&'de serde_json::value::RawValue>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an array")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut blocks = Vec::new();
            loop {
                self.0.check()?;
                match seq.next_element()? {
                    Some(block) => blocks.push(block),
                    None => return Ok(blocks),
                }
            }
        }
    }

    let checks = ItemChecks::new(deadline);
    let mut parser = serde_json::Deserializer::from_str(content.get());
    let parsed = serde::Deserializer::deserialize_seq(&mut parser, Blocks(&checks))
        .and_then(|blocks| parser.end().map(|()| blocks));
    checks.outcome(parsed)
}

/// The deadline of a parser that consumes a nested array or object item by
/// item (PI-32). A JSON error cannot carry the cancellation, so it is kept
/// here and read back when the parse fails. (The name `settle` is kept for
/// `LiveExportRunner`, whose callers a guard counts by method name.)
struct ItemChecks<'d> {
    deadline: &'d dyn PiDeadline,
    cancelled: std::cell::Cell<bool>,
}

impl<'d> ItemChecks<'d> {
    fn new(deadline: &'d dyn PiDeadline) -> Self {
        Self {
            deadline,
            cancelled: std::cell::Cell::new(false),
        }
    }

    /// One check, before each item.
    fn check<E: serde::de::Error>(&self) -> std::result::Result<(), E> {
        if self.deadline.elapsed() {
            self.cancelled.set(true);
            return Err(E::custom("the deadline elapsed"));
        }
        Ok(())
    }

    /// A parse's result: `Cancelled` when a check stopped it, `Ok(None)`
    /// when the value has another shape.
    fn outcome<T>(
        &self,
        parsed: std::result::Result<T, serde_json::Error>,
    ) -> std::result::Result<Option<T>, PiStop<PiProjectionError>> {
        match parsed {
            Ok(value) => Ok(Some(value)),
            Err(_) if self.cancelled.get() => Err(PiStop::Cancelled),
            Err(_) => Ok(None),
        }
    }
}

/// A `content` of a JSON type the message does not allow, kept whole.
fn unsupported_content(
    content: &serde_json::value::RawValue,
    partial: &mut BTreeSet<PiContentPartialReason>,
) -> PiContentRecord {
    partial.insert(PiContentPartialReason::UnknownBlock);
    PiContentRecord::Unprojected {
        block: PiRawJson(content.to_owned()),
    }
}

fn block_type(block: &RawFields<'_>) -> Option<String> {
    typed_field::<String>(block, "type").ok().flatten()
}

/// Keep a block that is not projected, with the reason.
fn unprojected(
    block: &serde_json::value::RawValue,
    fields: Option<&RawFields<'_>>,
    records: &mut Vec<PiContentRecord>,
    partial: &mut BTreeSet<PiContentPartialReason>,
) {
    partial.insert(if fields.and_then(block_type).as_deref() == Some("image") {
        PiContentPartialReason::UnsupportedBlock
    } else {
        PiContentPartialReason::UnknownBlock
    });
    records.push(PiContentRecord::Unprojected {
        block: PiRawJson(block.to_owned()),
    });
}

/// One piece of a `string | (TextContent | ImageContent)[]` content, in
/// block order: a text, or a block kept unprojected.
enum ContentPiece {
    Text(String),
    Kept(PiContentRecord),
}

fn content_pieces(
    content: Option<&serde_json::value::RawValue>,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Vec<ContentPiece>, PiStop<PiProjectionError>> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    if let Ok(text) = serde_json::from_str::<String>(content.get()) {
        return Ok(vec![ContentPiece::Text(text)]);
    }
    let Some(blocks) = content_blocks(content, deadline)? else {
        return Ok(vec![ContentPiece::Kept(unsupported_content(
            content, partial,
        ))]);
    };
    let mut pieces = Vec::new();
    for block in blocks {
        poll!(deadline);
        let fields = raw_fields(block.get());
        match fields.as_ref().map(|fields| {
            (
                block_type(fields),
                typed_field::<String>(fields, "text").ok().flatten(),
            )
        }) {
            Some((Some(kind), Some(part))) if kind == "text" => {
                pieces.push(ContentPiece::Text(part))
            }
            _ => {
                let mut kept = Vec::new();
                unprojected(block, fields.as_ref(), &mut kept, partial);
                pieces.extend(kept.into_iter().map(ContentPiece::Kept));
            }
        }
    }
    Ok(pieces)
}

/// The texts of a content, in order, for a record that holds them all;
/// the blocks kept unprojected go to `trailing`, which the caller appends
/// after that record.
fn content_text(
    content: Option<&serde_json::value::RawValue>,
    trailing: &mut Vec<PiContentRecord>,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Vec<String>, PiStop<PiProjectionError>> {
    let mut text = Vec::new();
    for piece in content_pieces(content, partial, deadline)? {
        match piece {
            ContentPiece::Text(part) => text.push(part),
            ContentPiece::Kept(record) => trailing.push(record),
        }
    }
    Ok(text)
}

/// A system message's section texts, in JavaScript enumeration order; a
/// section set to `null` (removed) contributes none.
fn system_sections(
    sections: Option<&serde_json::value::RawValue>,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Vec<String>, PiStop<PiProjectionError>> {
    let Some(sections) = sections else {
        return Ok(Vec::new());
    };
    if sections.get() == "null" {
        partial.insert(PiContentPartialReason::MalformedField);
        return Ok(Vec::new());
    }
    match ordered_sections(sections.get(), deadline)? {
        Some(values) => Ok(values.into_iter().flatten().collect()),
        None => {
            partial.insert(PiContentPartialReason::MalformedField);
            Ok(Vec::new())
        }
    }
}

/// A name JavaScript enumerates as an array index: a canonical decimal
/// below `2^32 - 1`.
fn js_array_index(name: &str) -> Option<u32> {
    if name != "0" && (name.starts_with('0') || name.is_empty()) {
        return None;
    }
    if !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    name.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

/// The values of a `Record<string, string | null>` in the order
/// `JSON.parse` then enumerates it: array-index names ascending, then the
/// others in file order, a repeated name keeping its first position and last
/// value. Each entry is parsed under the deadline (PI-32); `Ok(None)` for
/// another shape.
fn ordered_sections(
    text: &str,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Option<Vec<Option<String>>>, PiStop<PiProjectionError>> {
    struct Sections<'c, 'd>(&'c ItemChecks<'d>);

    impl<'de> serde::de::Visitor<'de> for Sections<'_, '_> {
        type Value = Vec<Option<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object of string or null sections")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut names: Vec<String> = Vec::new();
            let mut values: Vec<Option<String>> = Vec::new();
            let mut positions = std::collections::HashMap::new();
            loop {
                self.0.check()?;
                let Some((name, value)) = map.next_entry::<String, Option<String>>()? else {
                    break;
                };
                match positions.get(&name) {
                    Some(&position) => values[position] = value,
                    None => {
                        positions.insert(name.clone(), names.len());
                        names.push(name);
                        values.push(value);
                    }
                }
            }
            // JavaScript enumerates array-index names first, ascending, then
            // the other names in insertion order. Each index name enters an
            // ordered map with a check (two names never share an index), the
            // others keep their order, and each value is taken with a check.
            let mut indexed = BTreeMap::new();
            let mut others = Vec::new();
            for (position, name) in names.iter().enumerate() {
                self.0.check()?;
                match js_array_index(name) {
                    Some(index) => {
                        indexed.insert(index, position);
                    }
                    None => others.push(position),
                }
            }
            let mut ordered = Vec::with_capacity(names.len());
            for position in indexed.into_values().chain(others) {
                self.0.check()?;
                ordered.push(values[position].take());
            }
            Ok(ordered)
        }
    }

    let checks = ItemChecks::new(deadline);
    let mut parser = serde_json::Deserializer::from_str(text);
    let parsed = serde::Deserializer::deserialize_map(&mut parser, Sections(&checks))
        .and_then(|values| parser.end().map(|()| values));
    checks.outcome(parsed)
}

/// A thinking block's readable text: its `thinking` when that is a
/// non-empty string and `redacted` is a valid `false` or absent. `Err` when
/// `thinking` is not a string or `redacted` not a boolean.
fn readable_thinking(fields: &RawFields<'_>) -> std::result::Result<Option<PiReasoningText>, ()> {
    match (
        typed_field::<String>(fields, "thinking"),
        typed_field::<bool>(fields, "redacted"),
    ) {
        (Ok(Some(text)), Ok(redacted)) => {
            Ok((!text.is_empty() && redacted != Some(true)).then_some(PiReasoningText(text)))
        }
        _ => Err(()),
    }
}

fn project_assistant(
    content: Option<&serde_json::value::RawValue>,
    records: &mut Vec<PiContentRecord>,
    call_ids: &mut BTreeSet<String>,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<(), PiStop<PiProjectionError>> {
    let Some(content) = content else {
        return Ok(());
    };
    let Some(blocks) = content_blocks(content, deadline)? else {
        records.push(unsupported_content(content, partial));
        return Ok(());
    };
    for block in blocks {
        poll!(deadline);
        let Some(fields) = raw_fields(block.get()) else {
            unprojected(block, None, records, partial);
            continue;
        };
        match block_type(&fields).as_deref() {
            Some("text") => match typed_field::<String>(&fields, "text") {
                Ok(Some(text)) => records.push(PiContentRecord::Assistant { text }),
                _ => unprojected(block, Some(&fields), records, partial),
            },
            Some("thinking") => {
                let readable = readable_thinking(&fields).unwrap_or_else(|()| {
                    partial.insert(PiContentPartialReason::MalformedField);
                    None
                });
                records.push(PiContentRecord::Reasoning {
                    readable,
                    block: PiRawJson(block.to_owned()),
                });
            }
            Some("toolCall") => {
                let call_id = required_string(&fields, "id", partial);
                if let Some(id) = &call_id {
                    call_ids.insert(id.clone());
                }
                let name = required_string(&fields, "name", partial);
                let arguments = match typed_field::<serde_json::Map<String, serde_json::Value>>(
                    &fields,
                    "arguments",
                ) {
                    Ok(Some(arguments)) => Some(serde_json::Value::Object(arguments)),
                    Ok(None) | Err(()) => {
                        partial.insert(PiContentPartialReason::MalformedField);
                        None
                    }
                };
                records.push(PiContentRecord::ToolCall {
                    call_id,
                    name,
                    arguments,
                    thought_signature: member(&fields, "thoughtSignature")
                        .map(|raw| PiRawJson(raw.to_owned())),
                    block: PiRawJson(block.to_owned()),
                });
            }
            _ => unprojected(block, Some(&fields), records, partial),
        }
    }
    Ok(())
}

/// A `toolResult` message: the result record first, then any of its blocks
/// that are kept unprojected.
fn project_tool_result(
    message: &RawFields<'_>,
    records: &mut Vec<PiContentRecord>,
    call_ids: &BTreeSet<String>,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<(), PiStop<PiProjectionError>> {
    let call_id = match typed_field::<String>(message, "toolCallId") {
        Ok(call_id) => call_id,
        Err(()) => {
            partial.insert(PiContentPartialReason::MalformedField);
            None
        }
    };
    if !call_id.as_ref().is_some_and(|id| call_ids.contains(id)) {
        partial.insert(PiContentPartialReason::OrphanToolResult);
    }
    let tool_name = required_string(message, "toolName", partial);
    let is_error = match typed_field::<bool>(message, "isError") {
        Ok(Some(is_error)) => Some(is_error),
        Ok(None) | Err(()) => {
            partial.insert(PiContentPartialReason::MalformedField);
            None
        }
    };
    let mut blocks = Vec::new();
    let text = content_text(content_member(message), &mut blocks, partial, deadline)?;
    let nested_calls = match member(message, "nestedCalls") {
        Some(nested) if nested.get() == "null" => {
            partial.insert(PiContentPartialReason::MalformedField);
            None
        }
        Some(nested) => Some(bound_nested_calls(nested, partial, deadline)?),
        None => None,
    };
    records.push(PiContentRecord::ToolResult {
        call_id,
        tool_name,
        text,
        is_error,
        details: member(message, "details").map(|raw| PiRawJson(raw.to_owned())),
        nested_calls,
    });
    records.extend(blocks);
    Ok(())
}

/// The number of items in a JSON array, counted in constant memory with a
/// check before each item (PI-32); `Ok(None)` for another shape.
fn count_array(
    text: &str,
    deadline: &dyn PiDeadline,
) -> std::result::Result<Option<usize>, PiStop<PiProjectionError>> {
    struct Count<'c, 'd>(&'c ItemChecks<'d>);

    impl<'de> serde::de::Visitor<'de> for Count<'_, '_> {
        type Value = usize;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an array")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut count = 0usize;
            loop {
                self.0.check()?;
                match seq.next_element::<serde::de::IgnoredAny>()? {
                    Some(_) => count += 1,
                    None => return Ok(count),
                }
            }
        }
    }

    let checks = ItemChecks::new(deadline);
    let mut parser = serde_json::Deserializer::from_str(text);
    let parsed = serde::Deserializer::deserialize_seq(&mut parser, Count(&checks))
        .and_then(|count| parser.end().map(|()| count));
    checks.outcome(parsed)
}

/// Keep `nestedCalls` as bounded metadata of its tool result. Its calls are
/// counted in constant memory, and its raw value is kept only within both
/// bounds.
fn bound_nested_calls(
    nested: &serde_json::value::RawValue,
    partial: &mut BTreeSet<PiContentPartialReason>,
    deadline: &dyn PiDeadline,
) -> std::result::Result<PiNestedCalls, PiStop<PiProjectionError>> {
    let fields = raw_fields(nested.get());
    let complete = fields
        .as_ref()
        .and_then(|fields| typed_field::<bool>(fields, "complete").ok().flatten());
    if complete != Some(true) {
        partial.insert(PiContentPartialReason::NestedCallsIncomplete);
    }
    let calls = match fields.as_ref().and_then(|fields| member(fields, "calls")) {
        Some(calls) => count_array(calls.get(), deadline)?,
        None => None,
    };
    let within = nested.get().len() <= PI_NESTED_CALLS_MAX_BYTES
        && calls.is_some_and(|count| count <= PI_NESTED_CALLS_MAX_CALLS);
    if !within {
        partial.insert(PiContentPartialReason::NestedCallsOverBound);
    }
    Ok(PiNestedCalls {
        calls: calls.unwrap_or(0),
        complete,
        raw: within.then(|| PiRawJson(nested.to_owned())),
    })
}

/// Session token and cost totals, as Pi's `UsageTotals`
/// (`usage-totals.ts:4-28`): the four token counters and `cost.total`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PiUsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Sum of each usage's `cost.total`.
    pub cost: f64,
}

impl PiUsageTotals {
    /// All tokens, as Pi's breakdown counts them (`usage-totals.ts:90-94`),
    /// saturating at `u64::MAX` (then [`PiMetadataPartialReason::TokenOverflow`]).
    pub fn tokens(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

/// The model one assistant message is attributed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiModelAttribution {
    /// The assistant message entry's `id`.
    pub entry_id: String,
    pub provider: Option<String>,
    /// `responseModel` when the message has one, else `model`
    /// (`usage-totals.ts:66`); `None` when that value is not a string.
    pub model: Option<String>,
    /// Whether the message has a `responseModel` (absent and `null` fall
    /// back to `model`, as `??` does).
    pub from_response_model: bool,
}

/// Why session metadata may be incomplete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PiMetadataPartialReason {
    /// A usage source whose `usage` is missing or not Pi's shape
    /// (non-negative integer token counters and a finite `cost.total`): it
    /// is left out of the totals.
    MalformedUsage,
    /// An assistant message whose `responseModel` is not a string, that has
    /// no `responseModel` and no string `model`, or whose required
    /// `provider` is missing or not a string.
    MalformedModel,
    /// A token counter, or their sum, would overflow; it stops at
    /// `u64::MAX`.
    TokenOverflow,
    /// The cost total is no longer finite.
    CostOverflow,
    /// A line the normalizer skipped (unparseable or a falsy JSON value) may
    /// have been a usage source or an assistant message.
    SkippedLine,
}

/// Session usage totals and model attribution of one session file.
#[derive(Clone, Debug, PartialEq)]
pub struct PiSessionMetadata {
    pub totals: PiUsageTotals,
    /// Usage sources added to the totals.
    pub usage_sources: usize,
    /// Every assistant message in the file, in file order.
    pub models: Vec<PiModelAttribution>,
    pub partial_reasons: BTreeSet<PiMetadataPartialReason>,
}

impl PiSessionMetadata {
    pub fn partial(&self) -> bool {
        !self.partial_reasons.is_empty()
    }
}

/// One usage value in Pi's shape.
struct ParsedUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
}

fn parse_usage(raw: &serde_json::value::RawValue) -> Option<ParsedUsage> {
    let fields = raw_fields(raw.get())?;
    let counter = |name: &str| typed_field::<u64>(&fields, name).ok().flatten();
    let cost = member(&fields, "cost")
        .and_then(|cost| raw_fields(cost.get()))
        .and_then(|cost| typed_field::<f64>(&cost, "total").ok().flatten())
        .filter(|total| total.is_finite())?;
    Some(ParsedUsage {
        input: counter("input")?,
        output: counter("output")?,
        cache_read: counter("cacheRead")?,
        cache_write: counter("cacheWrite")?,
        cost,
    })
}

/// Project a session file's token and cost totals and the model of each
/// assistant message. As Pi's `getSessionStats`
/// (`agent-session.ts:4243-4275`), the totals cover every entry in the
/// file, on the active branch or not, compacted or not: `usage` entries of
/// any `kind`, the `usage` of `branch_summary` and `compaction` entries and
/// of tool results when present, and each assistant message's `usage`. A
/// model is the message's `responseModel ?? model`; `model_change`
/// entries, whose `modelId` can name a virtual model, are never used. The
/// file is normalized first, so it is refused exactly when
/// [`normalize_pi_session`] refuses it.
pub fn project_pi_session_metadata(
    bytes: &[u8],
    limits: PiNormalizeLimits,
) -> std::result::Result<PiSessionMetadata, PiNormalizeRefusal> {
    // The same refusals as the normalizer. A line it skipped is skipped
    // here too, and may have held usage.
    let branch = normalize_pi_session(bytes, limits)?;
    let mut metadata = PiSessionMetadata {
        totals: PiUsageTotals::default(),
        usage_sources: 0,
        models: Vec::new(),
        partial_reasons: BTreeSet::new(),
    };
    if branch
        .partial_reasons
        .contains(&PiBranchPartialReason::MalformedLine)
    {
        metadata
            .partial_reasons
            .insert(PiMetadataPartialReason::SkippedLine);
    }
    let mut header_seen = false;
    for line in bytes.split(|byte| *byte == b'\n') {
        let Some(fields) = std::str::from_utf8(line).ok().and_then(raw_fields) else {
            continue;
        };
        let kind = typed_field::<String>(&fields, "type").ok().flatten();
        if !header_seen {
            // The normalizer has proved the first parsed object is the header.
            header_seen = true;
            continue;
        }
        let message = member(&fields, "message").and_then(|raw| raw_fields(raw.get()));
        let role = message
            .as_ref()
            .and_then(|message| typed_field::<String>(message, "role").ok().flatten());
        // Each usage source, and whether Pi adds it unconditionally (a
        // missing one is then malformed) or only when present.
        let source = match (kind.as_deref(), role.as_deref()) {
            (Some("usage"), _) => Some((member(&fields, "usage"), true)),
            (Some("branch_summary" | "compaction"), _) => Some((member(&fields, "usage"), false)),
            (Some("message"), Some("toolResult")) => message
                .as_ref()
                .map(|message| (member(message, "usage"), false)),
            (Some("message"), Some("assistant")) => message
                .as_ref()
                .map(|message| (member(message, "usage"), true)),
            _ => None,
        };
        if let Some((usage, required)) = source {
            match usage.filter(|raw| raw.get() != "null") {
                Some(raw) => match parse_usage(raw) {
                    Some(usage) => add_usage(&mut metadata, usage),
                    None => {
                        metadata
                            .partial_reasons
                            .insert(PiMetadataPartialReason::MalformedUsage);
                    }
                },
                None if required => {
                    metadata
                        .partial_reasons
                        .insert(PiMetadataPartialReason::MalformedUsage);
                }
                None => {}
            }
        }
        if let (Some("message"), Some("assistant"), Some(message)) =
            (kind.as_deref(), role.as_deref(), message.as_ref())
        {
            // `responseModel ?? model`: `??` falls back on absent and `null`.
            let (model, from_response_model) =
                match member(message, "responseModel").filter(|raw| raw.get() != "null") {
                    None => (
                        typed_field::<String>(message, "model").ok().flatten(),
                        false,
                    ),
                    Some(raw) => (serde_json::from_str::<String>(raw.get()).ok(), true),
                };
            let provider = typed_field::<String>(message, "provider").ok().flatten();
            if model.is_none() || provider.is_none() {
                metadata
                    .partial_reasons
                    .insert(PiMetadataPartialReason::MalformedModel);
            }
            metadata.models.push(PiModelAttribution {
                entry_id: typed_field::<String>(&fields, "id")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                provider,
                model,
                from_response_model,
            });
        }
    }
    Ok(metadata)
}

fn add_usage(metadata: &mut PiSessionMetadata, usage: ParsedUsage) {
    let totals = &mut metadata.totals;
    let mut overflow = false;
    for (total, value) in [
        (&mut totals.input, usage.input),
        (&mut totals.output, usage.output),
        (&mut totals.cache_read, usage.cache_read),
        (&mut totals.cache_write, usage.cache_write),
    ] {
        match total.checked_add(value) {
            Some(sum) => *total = sum,
            None => {
                *total = u64::MAX;
                overflow = true;
            }
        }
    }
    // The four-way sum `tokens()` reports must not overflow either.
    overflow |= totals
        .input
        .checked_add(totals.output)
        .and_then(|sum| sum.checked_add(totals.cache_read))
        .and_then(|sum| sum.checked_add(totals.cache_write))
        .is_none();
    totals.cost += usage.cost;
    let cost_overflow = !totals.cost.is_finite();
    metadata.usage_sources += 1;
    if overflow {
        metadata
            .partial_reasons
            .insert(PiMetadataPartialReason::TokenOverflow);
    }
    if cost_overflow {
        metadata
            .partial_reasons
            .insert(PiMetadataPartialReason::CostOverflow);
    }
}

#[cfg(any(unix, windows))]
fn discover_within_provider_root(
    adapter: &dyn ObservedAgent,
    provider_root: &Path,
    root: &Path,
    layout: PiSessionLayout,
    limits: PiDiscoveryLimits,
    before_open: &mut dyn FnMut(&Path),
) -> Result<PiDiscovery> {
    let directory =
        match super::super::transcript_source::open_provider_directory_for_discovery(adapter, root)
        {
            Ok(Some(directory)) => directory,
            Ok(None) => return Ok(PiDiscovery::default()),
            Err(error) => {
                return match root_refusal(provider_root, root) {
                    Some(reason) => Ok(PiDiscovery::refuse(reason)),
                    None => Err(error).context("open the Pi session root"),
                };
            }
        };
    let mut discovery = PiDiscovery::default();
    let mut walk = Walk {
        limits,
        discovery: &mut discovery,
        before_open,
    };
    walk.directory(&directory, root, layout)?;
    discovery
        .candidates
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(discovery)
}

#[cfg(not(any(unix, windows)))]
fn discover_within_provider_root(
    _adapter: &dyn ObservedAgent,
    _provider_root: &Path,
    _root: &Path,
    _layout: PiSessionLayout,
    _limits: PiDiscoveryLimits,
    _before_open: &mut dyn FnMut(&Path),
) -> Result<PiDiscovery> {
    anyhow::bail!("Pi session discovery is unavailable on this platform; it fails closed")
}

/// Classify a root the no-follow open refused: a symlink at or above it
/// (below the provider root), or a non-directory. `None` leaves the open
/// error as it was.
#[cfg(any(unix, windows))]
fn root_refusal(provider_root: &Path, root: &Path) -> Option<PiRootRefusal> {
    let relative = root.strip_prefix(provider_root).ok()?;
    let mut current = provider_root.to_path_buf();
    for path in std::iter::once(current.clone()).chain(relative.components().map(|component| {
        current.push(component);
        current.clone()
    })) {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Some(PiRootRefusal::Symlink);
            }
            Ok(metadata) if path == root && !metadata.is_dir() => {
                return Some(PiRootRefusal::NotADirectory);
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Bytes requested per read while looking for a header's newline. Up to
/// `PI_HEADER_READ_CHUNK - 1` bytes after the newline may be read into the
/// transient read buffer; they are dropped at once, never kept, parsed, or
/// logged, and are charged to the byte limits.
#[cfg(any(unix, windows))]
const PI_HEADER_READ_CHUNK: usize = 512;

/// One bounded walk. Returns `false` from a step once a limit stopped it.
#[cfg(any(unix, windows))]
struct Walk<'a> {
    limits: PiDiscoveryLimits,
    discovery: &'a mut PiDiscovery,
    before_open: &'a mut dyn FnMut(&Path),
}

#[cfg(any(unix, windows))]
impl Walk<'_> {
    fn directory(
        &mut self,
        directory: &std::fs::File,
        logical: &Path,
        layout: PiSessionLayout,
    ) -> Result<bool> {
        use super::super::transcript_source::PinnedEntryType;

        if !directory_is_private(directory)
            .with_context(|| format!("inspect Pi session directory {}", logical.display()))?
        {
            self.discovery
                .partial_reasons
                .insert(PiPartialReason::UnsafeDirectory);
            return Ok(true);
        }
        let mut entries =
            super::super::transcript_source::read_dir_pinned_provider_directory_reporting_vanished(
                directory,
            )
            .with_context(|| format!("read Pi session directory {}", logical.display()))?;
        loop {
            // Checked before the next entry is fetched, so the walk never
            // inspects more than `max_entries`; at the limit it is partial
            // whether or not more entries exist.
            if self.discovery.entries_scanned >= self.limits.max_entries {
                self.discovery
                    .partial_reasons
                    .insert(PiPartialReason::EntryLimit);
                return Ok(false);
            }
            let Some(entry) = entries.next() else {
                return Ok(true);
            };
            self.discovery.entries_scanned += 1;
            let entry = entry.with_context(|| {
                format!("read Pi session directory entry in {}", logical.display())
            })?;
            let name = entry.file_name;
            match entry.file_type {
                PinnedEntryType::Symlink => {
                    self.discovery
                        .partial_reasons
                        .insert(PiPartialReason::SymlinkEntry);
                }
                PinnedEntryType::Other => {
                    self.discovery
                        .partial_reasons
                        .insert(PiPartialReason::NonRegularEntry);
                }
                // Listed, then gone before its type could be read; counted
                // against `max_entries` like any other entry.
                PinnedEntryType::Vanished => {
                    self.discovery
                        .partial_reasons
                        .insert(PiPartialReason::ChangedEntry);
                }
                PinnedEntryType::Directory => {
                    if layout == PiSessionLayout::Default && is_encoded_cwd_dir(&name) {
                        (self.before_open)(&logical.join(&name));
                        let opened =
                            super::super::transcript_source::open_directory_beneath_pinned_directory(
                                directory,
                                Path::new(&name),
                            );
                        let subdirectory = match opened {
                            Ok(subdirectory) => subdirectory,
                            Err(error) => match changed_entry_reason(&error) {
                                Some(reason) => {
                                    self.discovery.partial_reasons.insert(reason);
                                    continue;
                                }
                                None => {
                                    return Err(error).with_context(|| {
                                        format!(
                                            "open Pi session directory {}",
                                            logical.join(&name).display()
                                        )
                                    });
                                }
                            },
                        };
                        if !self.directory(
                            &subdirectory,
                            &logical.join(&name),
                            PiSessionLayout::Flat,
                        )? {
                            return Ok(false);
                        }
                    }
                }
                PinnedEntryType::File => {
                    if layout == PiSessionLayout::Flat
                        && Path::new(&name)
                            .extension()
                            .is_some_and(|extension| extension == "jsonl")
                        && !self.session_file(directory, logical, &name)?
                    {
                        return Ok(false);
                    }
                }
            }
        }
    }

    /// Read one session file's header line within the limits.
    fn session_file(
        &mut self,
        directory: &std::fs::File,
        logical: &Path,
        name: &std::ffi::OsStr,
    ) -> Result<bool> {
        use std::io::Read;

        let remaining = self
            .limits
            .max_total_bytes
            .saturating_sub(self.discovery.bytes_read);
        if remaining == 0 {
            self.discovery
                .partial_reasons
                .insert(PiPartialReason::ByteLimit);
            return Ok(false);
        }
        let path = logical.join(name);
        (self.before_open)(&path);
        let opened = match open_regular_session_file(directory, name) {
            Ok(opened) => opened,
            Err(error) => match changed_entry_reason(&error) {
                Some(reason) => {
                    self.discovery.partial_reasons.insert(reason);
                    return Ok(true);
                }
                None => {
                    return Err(error)
                        .with_context(|| format!("open Pi session file {}", path.display()));
                }
            },
        };
        let Some(file) = opened else {
            // Replaced by a non-regular entry after its type was read.
            self.discovery
                .partial_reasons
                .insert(PiPartialReason::NonRegularEntry);
            return Ok(true);
        };
        self.discovery.source_reads += 1;
        let cap = self.limits.max_header_bytes.min(remaining);
        let mut header = Vec::new();
        let mut reader = file.take(cap);
        let mut chunk = [0u8; PI_HEADER_READ_CHUNK];
        let mut ended_line = false;
        loop {
            let read = reader
                .read(&mut chunk)
                .with_context(|| format!("read Pi session file {}", path.display()))?;
            if read == 0 {
                break;
            }
            self.discovery.bytes_read += read as u64;
            if let Some(index) = chunk[..read].iter().position(|byte| *byte == b'\n') {
                header.extend_from_slice(&chunk[..index]);
                ended_line = true;
                break;
            }
            header.extend_from_slice(&chunk[..read]);
        }
        let truncated = !ended_line && header.len() as u64 == cap;
        if truncated {
            self.discovery
                .partial_reasons
                .insert(PiPartialReason::ByteLimit);
            // A header longer than one file's cap drops that file only; a
            // walk out of total budget stops.
            return Ok(cap == self.limits.max_header_bytes);
        }
        self.discovery
            .candidates
            .push(PiSessionCandidate { path, header });
        Ok(true)
    }
}

/// Whether only the current user can change `directory`'s entries: it is
/// theirs and neither its group nor others can write it. Then no other user
/// can swap an entry between its type check and its open.
#[cfg(unix)]
fn directory_is_private(directory: &std::fs::File) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    // On Linux a POSIX ACL entry that grants write is capped by the ACL mask,
    // which the group mode bits report, so the mode check covers it.
    Ok(metadata.uid() == euid
        && metadata.mode() & 0o022 == 0
        && !directory_has_extended_acl(directory)?)
}

/// Whether a macOS directory carries an extended ACL. Such an ACL can let
/// another user add or delete entries whatever the mode bits say, so any
/// extended ACL object, whatever its entries, makes the directory unsafe.
#[cfg(target_os = "macos")]
fn directory_has_extended_acl(directory: &std::fs::File) -> Result<bool> {
    use std::os::fd::AsRawFd;

    // <sys/acl.h>: `acl_type_t` is a C enum (int).
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
        fn acl_free(object: *mut libc::c_void) -> libc::c_int;
    }

    // SAFETY: the descriptor stays open for the call; a non-null result is
    // an ACL this function owns and frees at once.
    let acl = unsafe { acl_get_fd_np(directory.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if !acl.is_null() {
        // SAFETY: `acl` is the live ACL obtained above, freed exactly once.
        unsafe { acl_free(acl) };
        return Ok(true);
    }
    extended_acl_lookup_failure(std::io::Error::last_os_error())
}

/// The verdict for an extended-ACL lookup that returned no ACL: none
/// (`ENOENT`) or none possible on this filesystem (`ENOTSUP`, `EOPNOTSUPP`)
/// is safe; any other failure could hide an ACL and is an error, so the walk
/// fails closed.
#[cfg(target_os = "macos")]
fn extended_acl_lookup_failure(error: std::io::Error) -> Result<bool> {
    match error.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP) => Ok(false),
        _ => Err(error).context("read the directory's extended ACL"),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn directory_has_extended_acl(_directory: &std::fs::File) -> Result<bool> {
    Ok(false)
}

/// The partial reason for an open that failed because the entry vanished
/// (`ChangedEntry`), became a symlink (`SymlinkEntry`), or changed type
/// (`ChangedEntry`) after enumeration. `None` for any other failure.
#[cfg(any(unix, windows))]
fn changed_entry_reason(error: &anyhow::Error) -> Option<PiPartialReason> {
    let io = error.downcast_ref::<std::io::Error>()?;
    if io.kind() == std::io::ErrorKind::NotFound {
        return Some(PiPartialReason::ChangedEntry);
    }
    #[cfg(unix)]
    if io.raw_os_error() == Some(libc::ELOOP) {
        return Some(PiPartialReason::SymlinkEntry);
    }
    #[cfg(unix)]
    if io.raw_os_error() == Some(libc::ENOTDIR) {
        return Some(PiPartialReason::ChangedEntry);
    }
    // The Windows openers report an entry that is now a reparse point, the
    // other kind (file or directory), or not a disk object as `InvalidData`.
    #[cfg(windows)]
    if io.kind() == std::io::ErrorKind::InvalidData {
        return Some(PiPartialReason::ChangedEntry);
    }
    None
}

/// Whether only the current user (with SYSTEM and Administrators) can change
/// `directory`'s entries: it is owned by one of them and no allowed ACE that
/// applies to it gives write rights to anyone else. Windows directories hold
/// no FIFOs or device nodes, and reparse points are refused by the no-follow
/// walk. A security lookup that fails is an error.
#[cfg(windows)]
fn directory_is_private(directory: &std::fs::File) -> Result<bool> {
    Ok(crate::utils::windows_fs::directory_entries_private(
        directory,
    )?)
}

/// Open `name` beneath `directory` without following a symlink and without
/// blocking, and return it only if the opened object is a regular file. A
/// FIFO or device swapped in after the entry's type was read is closed
/// unread (`None`).
#[cfg(unix)]
fn open_regular_session_file(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<Option<std::fs::File>> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    let name = CString::new(name.as_bytes()).context("session file name contains NUL")?;
    // SAFETY: the pinned directory descriptor and the NUL-terminated basename
    // stay alive for the call; O_NOFOLLOW refuses a symlink and O_NONBLOCK
    // keeps a FIFO from blocking the open.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open the session file (no-follow)");
    }
    // SAFETY: `fd` is a fresh descriptor from `openat`, owned from here on.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file
        .metadata()
        .context("inspect the session file")?
        .is_file()
    {
        return Ok(None);
    }
    Ok(Some(file))
}

#[cfg(windows)]
fn open_regular_session_file(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
) -> Result<Option<std::fs::File>> {
    super::super::open_file_beneath_pinned_provider_directory(directory, Path::new(name)).map(Some)
}

/// Pi names a default-layout directory `--<encoded-cwd>--`.
#[cfg(any(unix, windows))]
fn is_encoded_cwd_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.len() > 4 && name.starts_with("--") && name.ends_with("--"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PI-02 AC4: a session file replaced by a FIFO after its directory entry
    /// was read as a regular file is closed unread, without blocking.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(env)]
    fn pi_discovery_refuses_a_fifo_swapped_in_before_open() {
        let home = tempfile::tempdir().expect("pi home");
        let _home = crate::utils::test::ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let sessions = home.path().join(".pi").join("agent").join("sessions");
        let cwd_dir = sessions.join("--work--");
        std::fs::create_dir_all(&cwd_dir).expect("session dir");
        std::fs::write(cwd_dir.join("swapped.jsonl"), "{\"type\":\"session\"}\n")
            .expect("session file");
        std::fs::write(cwd_dir.join("kept.jsonl"), "{\"type\":\"session\"}\n")
            .expect("session file");
        let mut swapped = 0;
        let discovery = discover_pi_sessions_observed(
            &sessions,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
            &mut |path| {
                if path.file_name().is_some_and(|name| name == "swapped.jsonl") {
                    std::fs::remove_file(path).expect("remove the regular file");
                    let made = std::process::Command::new("mkfifo")
                        .arg(path)
                        .status()
                        .expect("run mkfifo");
                    assert!(made.success(), "mkfifo");
                    swapped += 1;
                }
            },
        )
        .expect("discovery");
        assert_eq!(swapped, 1, "the hook ran before the swapped file's open");
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::NonRegularEntry)
        );
        assert_eq!(discovery.source_reads, 1, "only the kept file was read");
        assert_eq!(
            discovery
                .candidates
                .iter()
                .map(|candidate| candidate.path.file_name().expect("name").to_owned())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from("kept.jsonl")]
        );
    }
    /// PI-02 AC3/AC4: an entry that vanishes, becomes a symlink, or (for a
    /// cwd directory) disappears between enumeration and its open is recorded
    /// as partial; the walk goes on and reads the other sessions.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial(env)]
    fn pi_discovery_survives_a_removed_or_swapped_entry() {
        let home = tempfile::tempdir().expect("pi home");
        let _home = crate::utils::test::ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let sessions = home.path().join(".pi").join("agent").join("sessions");
        let cwd_dir = sessions.join("--work--");
        let gone_dir = sessions.join("--gone--");
        std::fs::create_dir_all(&cwd_dir).expect("session dir");
        std::fs::create_dir_all(&gone_dir).expect("vanishing dir");
        for name in ["kept.jsonl", "gone.jsonl", "linked.jsonl"] {
            std::fs::write(cwd_dir.join(name), "{\"type\":\"session\"}\n").expect("session file");
        }
        std::fs::write(gone_dir.join("inside.jsonl"), "{\"type\":\"session\"}\n")
            .expect("session file");
        let target = home.path().join("target.jsonl");
        std::fs::write(&target, "{\"type\":\"session\"}\n").expect("symlink target");
        let discovery = discover_pi_sessions_observed(
            &sessions,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
            &mut |path| match path.file_name().and_then(|name| name.to_str()) {
                Some("gone.jsonl") => std::fs::remove_file(path).expect("remove the file"),
                Some("linked.jsonl") => {
                    std::fs::remove_file(path).expect("remove the file");
                    std::os::unix::fs::symlink(&target, path).expect("swap in a symlink");
                }
                Some("--gone--") => std::fs::remove_dir_all(path).expect("remove the dir"),
                _ => {}
            },
        )
        .expect("a changed entry does not abort the walk");
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::ChangedEntry),
            "{:?}",
            discovery.partial_reasons
        );
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::SymlinkEntry),
            "{:?}",
            discovery.partial_reasons
        );
        assert_eq!(discovery.source_reads, 1);
        assert_eq!(
            discovery
                .candidates
                .iter()
                .map(|candidate| candidate.path.file_name().expect("name").to_owned())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from("kept.jsonl")]
        );
    }

    /// PI-02 AC3 on Windows: a symlinked (reparse-point) root refuses the
    /// walk, and a symlinked entry is refused unread. Creating a symlink
    /// needs Developer Mode or elevation; without it the test prints
    /// `skipped`, unless `LIBRA_REQUIRE_WINDOWS_SYMLINKS=1` (set by the
    /// native Windows CI job) makes that a failure.
    #[cfg(windows)]
    #[test]
    #[serial_test::serial(env)]
    fn pi_discovery_refuses_reparse_points_on_windows() {
        let home = tempfile::tempdir().expect("pi home");
        let _home = crate::utils::test::ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let agent = home.path().join(".pi").join("agent");
        let sessions = agent.join("sessions");
        let cwd_dir = sessions.join("--work--");
        std::fs::create_dir_all(&cwd_dir).expect("session dir");
        std::fs::write(cwd_dir.join("real.jsonl"), "{\"type\":\"session\"}\n")
            .expect("session file");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        std::fs::write(
            elsewhere.path().join("target.jsonl"),
            "{\"type\":\"session\"}\n",
        )
        .expect("symlink target");
        if let Err(error) =
            std::os::windows::fs::symlink_dir(elsewhere.path(), agent.join("linked-sessions"))
        {
            assert!(
                std::env::var_os("LIBRA_REQUIRE_WINDOWS_SYMLINKS").is_none(),
                "LIBRA_REQUIRE_WINDOWS_SYMLINKS is set but a symlink cannot be created: {error}"
            );
            eprintln!("skipped (creating a symlink needs Developer Mode or elevation: {error})");
            return;
        }
        let discovery = discover_pi_sessions(
            &agent.join("linked-sessions"),
            PiSessionLayout::Flat,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery");
        assert_eq!(discovery.refused, Some(PiRootRefusal::Symlink));
        assert_eq!(discovery.source_reads, 0);
        std::os::windows::fs::symlink_file(
            elsewhere.path().join("target.jsonl"),
            cwd_dir.join("linked.jsonl"),
        )
        .expect("file symlink");
        let discovery = discover_pi_sessions(
            &sessions,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery");
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::SymlinkEntry)
        );
        assert_eq!(discovery.source_reads, 1);
        assert_eq!(discovery.candidates.len(), 1);

        // A regular file swapped for a reparse point after enumeration is
        // recorded as changed; the walk still reads the other session.
        std::fs::remove_file(cwd_dir.join("linked.jsonl")).expect("remove the file symlink");
        std::fs::write(cwd_dir.join("swapped.jsonl"), "{\"type\":\"session\"}\n")
            .expect("session file");
        let target = elsewhere.path().join("target.jsonl");
        let discovery = discover_pi_sessions_observed(
            &sessions,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
            &mut |path| {
                if path.file_name().is_some_and(|name| name == "swapped.jsonl") {
                    std::fs::remove_file(path).expect("remove the regular file");
                    std::os::windows::fs::symlink_file(&target, path)
                        .expect("swap in a file symlink");
                }
            },
        )
        .expect("a swapped entry does not abort the walk");
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::ChangedEntry),
            "{:?}",
            discovery.partial_reasons
        );
        assert_eq!(discovery.source_reads, 1);
        assert_eq!(discovery.candidates.len(), 1);
    }

    /// PI-02 AC4 on Windows: a session directory whose DACL lets another
    /// principal (here Everyone) write is not walked, so nothing in it is read.
    #[cfg(windows)]
    #[test]
    #[serial_test::serial(env)]
    fn pi_discovery_refuses_a_directory_others_can_write_on_windows() {
        let home = tempfile::tempdir().expect("pi home");
        let _home = crate::utils::test::ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let sessions = home.path().join(".pi").join("agent").join("sessions");
        let private_dir = sessions.join("--work--");
        let shared_dir = sessions.join("--shared--");
        for dir in [&private_dir, &shared_dir] {
            std::fs::create_dir_all(dir).expect("session dir");
            std::fs::write(dir.join("s.jsonl"), "{\"type\":\"session\"}\n").expect("session file");
        }
        let granted = std::process::Command::new("icacls")
            .arg(&shared_dir)
            .args(["/grant", "*S-1-1-0:(W)"])
            .output()
            .expect("run icacls");
        assert!(
            granted.status.success(),
            "icacls: {}",
            String::from_utf8_lossy(&granted.stderr)
        );
        let discovery = discover_pi_sessions(
            &sessions,
            PiSessionLayout::Default,
            PiDiscoveryLimits::default(),
        )
        .expect("discovery");
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::UnsafeDirectory),
            "{:?}",
            discovery.partial_reasons
        );
        assert_eq!(
            discovery.source_reads, 1,
            "only the private directory was read"
        );
        assert!(
            discovery
                .candidates
                .iter()
                .all(|candidate| !candidate.path.starts_with(&shared_dir))
        );
    }

    /// PI-02 AC3/AC4 on macOS: an extended-ACL lookup that fails for any
    /// reason but "no ACL" or "unsupported here" is an error, so a directory
    /// whose ACL cannot be read is never walked.
    #[cfg(target_os = "macos")]
    #[test]
    fn pi_discovery_treats_an_unreadable_acl_as_an_error() {
        for errno in [libc::ENOENT, libc::ENOTSUP, libc::EOPNOTSUPP] {
            assert!(
                !extended_acl_lookup_failure(std::io::Error::from_raw_os_error(errno))
                    .expect("no ACL is safe"),
                "{errno}"
            );
        }
        for errno in [libc::EACCES, libc::EBADF, libc::EIO, libc::ENOMEM] {
            assert!(
                extended_acl_lookup_failure(std::io::Error::from_raw_os_error(errno)).is_err(),
                "{errno}"
            );
        }
    }
    /// The shared pinned iterator, given an entry that vanishes between
    /// enumeration and its type lookup: the default mode skips it (Unix) or
    /// fails (Windows) as before, and the reporting mode yields it as
    /// `Vanished`.
    #[test]
    fn pi_discovery_pinned_iterator_reports_an_entry_vanishing_before_type_lookup() {
        use super::super::super::transcript_source::{
            PinnedEntryType, open_absolute_directory_no_follow, read_dir_pinned_provider_directory,
            read_dir_pinned_provider_directory_reporting_vanished, type_lookup_hook,
        };

        let names = |reporting: bool| -> Vec<Result<(String, PinnedEntryType), String>> {
            let dir = tempfile::tempdir().expect("dir");
            for name in ["a.jsonl", "gone.jsonl"] {
                std::fs::write(dir.path().join(name), "x").expect("file");
            }
            let directory = open_absolute_directory_no_follow(
                &super::super::super::transcript_source::normalize_macos_system_directory_alias(
                    dir.path(),
                ),
            )
            .expect("pin the directory");
            let gone = dir.path().join("gone.jsonl");
            type_lookup_hook::with(
                move |name| {
                    if name == "gone.jsonl" {
                        let _ = std::fs::remove_file(&gone);
                    }
                },
                || {
                    let entries = if reporting {
                        read_dir_pinned_provider_directory_reporting_vanished(&directory)
                    } else {
                        read_dir_pinned_provider_directory(&directory)
                    }
                    .expect("read the directory");
                    let mut seen: Vec<_> = entries
                        .map(|entry| {
                            entry
                                .map(|entry| {
                                    (
                                        entry.file_name.to_string_lossy().into_owned(),
                                        entry.file_type,
                                    )
                                })
                                .map_err(|error| error.to_string())
                        })
                        .collect();
                    seen.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
                    seen
                },
            )
        };
        let reported = names(true);
        assert!(
            reported.contains(&Ok(("gone.jsonl".to_string(), PinnedEntryType::Vanished))),
            "{reported:?}"
        );
        assert!(
            reported.contains(&Ok(("a.jsonl".to_string(), PinnedEntryType::File))),
            "{reported:?}"
        );
        let default = names(false);
        assert!(
            default.contains(&Ok(("a.jsonl".to_string(), PinnedEntryType::File))),
            "{default:?}"
        );
        assert!(
            !default
                .iter()
                .any(|entry| matches!(entry, Ok((_, PinnedEntryType::Vanished)))),
            "the default mode never yields Vanished: {default:?}"
        );
        #[cfg(unix)]
        assert_eq!(
            default.len(),
            1,
            "Unix skips the vanished entry: {default:?}"
        );
        #[cfg(windows)]
        assert!(
            default.iter().any(Result::is_err),
            "Windows fails on the vanished entry: {default:?}"
        );
    }

    /// PI-02: an entry that vanishes before its type is read is recorded as
    /// `ChangedEntry` and counted against `max_entries`, and the walk reads
    /// the remaining session.
    #[test]
    #[serial_test::serial(env)]
    fn pi_discovery_counts_an_entry_vanishing_before_type_lookup() {
        use super::super::super::transcript_source::type_lookup_hook;

        let home = tempfile::tempdir().expect("pi home");
        let _home = crate::utils::test::ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let sessions = home.path().join(".pi").join("agent").join("sessions");
        let cwd_dir = sessions.join("--work--");
        std::fs::create_dir_all(&cwd_dir).expect("session dir");
        for name in ["kept.jsonl", "gone.jsonl"] {
            std::fs::write(cwd_dir.join(name), "{\"type\":\"session\"}\n").expect("session file");
        }
        let gone = cwd_dir.join("gone.jsonl");
        let discover = |limits: PiDiscoveryLimits| {
            let gone = gone.clone();
            let _ = std::fs::write(&gone, "{\"type\":\"session\"}\n");
            type_lookup_hook::with(
                move |name| {
                    if name == "gone.jsonl" {
                        let _ = std::fs::remove_file(&gone);
                    }
                },
                || discover_pi_sessions(&sessions, PiSessionLayout::Default, limits),
            )
            .expect("discovery")
        };
        let discovery = discover(PiDiscoveryLimits::default());
        assert!(
            discovery
                .partial_reasons
                .contains(&PiPartialReason::ChangedEntry),
            "{:?}",
            discovery.partial_reasons
        );
        // `--work--`, `kept.jsonl`, and the vanished `gone.jsonl`.
        assert_eq!(discovery.entries_scanned, 3);
        assert_eq!(discovery.source_reads, 1);
        assert_eq!(discovery.candidates.len(), 1);
        // The vanished entry uses up the bound like any other entry. Only
        // the cwd directory and the vanishing file exist here, so the walk
        // cannot reach the bound without visiting it.
        std::fs::remove_file(cwd_dir.join("kept.jsonl")).expect("remove the kept session");
        std::fs::write(&gone, "{\"type\":\"session\"}\n").expect("session file");
        let visited = std::rc::Rc::new(std::cell::Cell::new(false));
        let bounded = {
            let gone = gone.clone();
            let visited = visited.clone();
            type_lookup_hook::with(
                move |name| {
                    if name == "gone.jsonl" {
                        visited.set(true);
                        let _ = std::fs::remove_file(&gone);
                    }
                },
                || {
                    discover_pi_sessions(
                        &sessions,
                        PiSessionLayout::Default,
                        PiDiscoveryLimits {
                            max_entries: 2,
                            ..PiDiscoveryLimits::default()
                        },
                    )
                },
            )
            .expect("discovery")
        };
        assert!(visited.get(), "the vanishing entry was visited");
        assert_eq!(bounded.entries_scanned, 2);
        assert!(
            bounded
                .partial_reasons
                .contains(&PiPartialReason::EntryLimit)
        );
        assert!(
            bounded
                .partial_reasons
                .contains(&PiPartialReason::ChangedEntry)
        );
        assert!(bounded.candidates.is_empty());
    }

    /// PI-15 AC2/AC3: on Windows `canonicalize` returns a verbatim path while
    /// Pi records a plain cwd; a plain header `cwd` beneath the repository
    /// still matches when the root is given in verbatim form.
    #[cfg(windows)]
    #[test]
    fn pi_attribution_matches_a_plain_windows_cwd_against_a_verbatim_root() {
        let repo = tempfile::tempdir().expect("repo");
        let canonical = repo.path().canonicalize().expect("canonical repo root");
        let verbatim = canonical.to_str().expect("utf-8 root");
        assert!(verbatim.starts_with(r"\\?\"), "{verbatim}");
        let plain = without_verbatim_prefix(canonical.clone());
        assert_eq!(plain.to_str().expect("utf-8 root"), &verbatim[4..]);
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\GLOBALROOT\repo")),
            PathBuf::from(r"\\?\GLOBALROOT\repo"),
            "other verbatim forms are kept"
        );

        let cwd = plain.join("src");
        let header = serde_json::json!({
            "type": "session",
            "id": "w1",
            "cwd": cwd.to_str().expect("utf-8 cwd"),
        })
        .to_string()
        .into_bytes();
        let discovery = PiDiscovery {
            candidates: vec![PiSessionCandidate {
                path: PathBuf::from(r"C:\sessions\w1.jsonl"),
                header,
            }],
            ..PiDiscovery::default()
        };
        let sessions = attribute_pi_sessions(discovery, &canonical).expect("attribution");
        assert!(!sessions.partial(), "{sessions:?}");
        assert_eq!(sessions.sessions.len(), 1, "{sessions:?}");
        assert_eq!(sessions.sessions[0].session_id, "w1");
        assert_eq!(sessions.sessions[0].cwd, cwd);
    }
}
