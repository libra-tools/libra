//! Codex `$CODEX_HOME/hooks.json` + `config.toml` trust-state management for
//! installing and removing Libra hook entries (AG-19).
//!
//! Contract verified against codex-cli 0.142.4 (probed live 2026-07-05,
//! cross-checked byte-exact against source `rust-v0.142.4` @57d253ad); see
//! the module docs in `mod.rs` for the full upstream facts. Key points that
//! shape this installer:
//!
//! - **User level only.** A project-level `<repo>/.codex/hooks.json` is only
//!   loaded when the *user* config marks the project trusted
//!   (`[projects."<abs path>"] trust_level = "trusted"`), which Libra cannot
//!   arrange non-interactively for arbitrary repos. Libra therefore writes
//!   its entries into `$CODEX_HOME/hooks.json` and the matching
//!   `[hooks.state]` trust entries into `$CODEX_HOME/config.toml` — the
//!   proven fully non-interactive path.
//! - **Trust double gate.** A hook only runs when its `[hooks.state."<abs
//!   hooks.json path>:<event_snake>:<matcher_group_index>:<handler_index>"]`
//!   entry has `enabled != false` **and** a `trusted_hash` equal to
//!   `"sha256:" + sha256hex(<canonical identity JSON>)`. Untrusted hooks are
//!   skipped **silently** by `codex exec`, so install always (re)writes the
//!   trust entries and `codex_hook_trust_gaps` lets the dispatcher surface a
//!   SessionStart banner when captures would be dropped.
//! - **Positional-key hazard.** The state keys embed matcher-group/handler
//!   indices (upstream TODO: durable ids). Indices are recomputed from the
//!   final `hooks.json` on every (re)install, Libra's own group is updated
//!   in place whenever possible so its index never drifts, and stale
//!   Libra-managed state keys pointing at the wrong index are removed. When
//!   removal would empty a Libra-only group before a user group, it leaves an
//!   empty group as a positional placeholder. A matcher-less group that mixes
//!   Libra and user handlers is refused before either `hooks.json` or
//!   `config.toml` changes: removing only Libra's handler would silently
//!   invalidate the user's positional trust key.
//! - **Durable ownership marker.** Codex deserializes `hooks.json` with
//!   `deny_unknown_fields` (group fields: `matcher`/`hooks`; handler fields:
//!   `type`/`command`/`commandWindows`/`timeout`/`async`/`statusMessage`),
//!   so Libra cannot tag entries with an arbitrary key. Current canonical
//!   handlers use the modeled `statusMessage: "libra capture"` marker plus
//!   exact command grammar and timeout/budget arithmetic. This lets an
//!   explicitly chosen renamed canonical binary remain idempotent while
//!   leaving arbitrary user commands untouched. During an explicit enable
//!   only, a direct absolute unmarked legacy or canonical command whose
//!   executable exactly equals the selected binary may be adopted and
//!   rewritten with the marker. Disable and trust-state cleanup remain
//!   marker-only, so a user command is never removed or trusted merely
//!   because its grammar resembles Libra's.
//! - **`config.toml` is edited surgically.** No format-preserving TOML
//!   editor exists in the dependency tree (`toml` 0.8 does not round-trip
//!   comments), so Libra uses a minimal line-based section editor: it only
//!   removes/appends its own `[hooks.state."…"]` sections, each tagged with
//!   a `# libra-managed …` comment line, leaving every other byte untouched
//!   (pinned by tests). Reads (trust-gap checks) parse the whole file with
//!   the `toml` crate, so they tolerate any layout.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::super::super::setup::write_json_settings;
use super::super::super::{
    lifecycle::normalize_json_value,
    provider::{
        ProviderInstallOptions, capture_budget_millis_for_host_timeout,
        is_managed_capture_budget_millis,
    },
    setup::{load_json_settings, resolve_hook_binary_path},
};
use crate::utils::atomic_write;

/// Default `timeout` (seconds) Libra writes on its own handlers.
const DEFAULT_CODEX_HOOK_TIMEOUT_SECS: u64 = 30;
/// Codex permits only a short `SessionEnd` hook window. Keep the provider's
/// 1s cleanup margin inside that window rather than advertising a 30s capture
/// budget that the host will reject or terminate early.
const CODEX_SESSION_END_MAX_TIMEOUT_SECS: u64 = 3;
const CODEX_SESSION_END_CAPTURE_BUDGET_MILLIS: u64 = 2_000;
/// Codex's own handler-timeout default (seconds), used as the *effective*
/// timeout in the canonical trust-hash identity when a handler omits it.
const CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS: u64 = 600;
/// `statusMessage` shown by Codex while a Libra capture hook runs.
const LIBRA_CODEX_STATUS_MESSAGE: &str = "libra capture";
/// Comment line written immediately above every Libra-managed
/// `[hooks.state."…"]` section in `$CODEX_HOME/config.toml`. The line-based
/// editor identifies Libra's sections by this marker (plus the hooks.json
/// path prefix inside the key) and never touches unmarked sections.
const CODEX_STATE_MARKER: &str = "# libra-managed codex hook trust entry (AG-19); do not edit";

const CODEX_HOOKS_FILE: &str = "hooks.json";
const CODEX_CONFIG_FILE: &str = "config.toml";
/// Serializes Libra-initiated updates to Codex's coupled hook/trust files.
/// The lock is deliberately retained after release: it contains no settings
/// data and gives independently launched Libra processes one shared mutex.
const CODEX_SETTINGS_TRANSACTION_LOCK_FILE: &str = ".libra-codex-hooks.lock";
/// Durable recovery record for the two-file Codex settings transaction. It
/// stores an operation/phase plus domain-separated before/after fingerprints,
/// never hooks/config bytes, command paths, or other user-private settings.
/// Recovery re-derives state solely from current marker-owned hooks and only
/// resumes an exact known fingerprint transition.
const CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE: &str = ".libra-codex-hooks.transaction.json";

/// Every Codex lifecycle event is forwarded through the stable
/// `libra hooks codex <verb>` command surface. Events sharing a lifecycle kind
/// intentionally share a verb; the provider event name is retained only long
/// enough for validated ingress to lower it into a canonical lifecycle event.
const CODEX_HOOK_FORWARD_MAP: &[(&str, &str)] = &[
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

/// `$CODEX_HOME/hooks.json` — top-level key must be `"hooks"`
/// (`deny_unknown_fields` upstream); unknown keys found on disk are still
/// round-tripped rather than dropped.
#[derive(Debug, Serialize, Deserialize, Default)]
struct CodexHooksFile {
    #[serde(default)]
    hooks: BTreeMap<String, Vec<CodexHookMatcherGroup>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// One matcher group: `{"matcher": "<optional regex>", "hooks": [ … ]}`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct CodexHookMatcherGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matcher: Option<String>,
    hooks: Vec<CodexHookHandler>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// One handler. Codex accepts `type`/`command`/`commandWindows`/`timeout`/
/// `async`/`statusMessage`; the fields Libra does not write (`commandWindows`,
/// `async`) are preserved via `extra` when present on user handlers.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct CodexHookHandler {
    #[serde(rename = "type")]
    handler_type: String,
    command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
    #[serde(
        rename = "statusMessage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    status_message: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// One desired/current `[hooks.state]` entry: the positional key and the
/// canonical trust hash of the handler it gates.
#[derive(Debug, Clone, PartialEq)]
struct CodexStateEntry {
    key: String,
    trusted_hash: String,
}

/// A byte-exact settings-file snapshot. `None` means the file was absent;
/// `Some(Vec::new())` is an existing empty file. Do not derive `Debug`: the
/// Codex config snapshot may contain user-private settings.
#[derive(Clone, PartialEq, Eq)]
struct CodexSettingsSnapshot {
    bytes: Option<Vec<u8>>,
}

impl CodexSettingsSnapshot {
    fn absent() -> Self {
        Self { bytes: None }
    }

    fn present(bytes: Vec<u8>) -> Self {
        Self { bytes: Some(bytes) }
    }

    fn is_present(&self) -> bool {
        self.bytes.is_some()
    }

    /// A non-plaintext, domain-separated integrity fingerprint is safe to
    /// persist in a recovery journal: it distinguishes absent from empty files
    /// without retaining a hook command, an absolute path, or config bytes.
    fn fingerprint(&self, target: &[u8]) -> CodexSettingsFingerprint {
        let mut hasher = Sha256::new();
        hasher.update(b"libra:codex-settings-transaction:journal-v2\0");
        hasher.update(target);
        hasher.update(b"\0");
        match self.bytes.as_deref() {
            Some(bytes) => {
                hasher.update([1]);
                hasher.update(bytes);
                CodexSettingsFingerprint {
                    present: true,
                    digest: hex::encode(hasher.finalize()),
                }
            }
            None => {
                hasher.update([0]);
                CodexSettingsFingerprint {
                    present: false,
                    digest: hex::encode(hasher.finalize()),
                }
            }
        }
    }
}

/// Non-plaintext durable identity for one target file. It is deliberately a
/// digest rather than a serialized snapshot: a recovery record must not carry
/// user config contents on a shared-ACL platform.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexSettingsFingerprint {
    present: bool,
    digest: String,
}

impl CodexSettingsFingerprint {
    fn validate(&self, label: &str) -> Result<()> {
        if self.digest.len() == 64 && self.digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        bail!(
            "invalid {label} digest in the Codex hook recovery journal; leave the journal in place and resolve the paired settings manually"
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodexSettingsTransactionPhase {
    /// The journal is durable but no target-file write has been attempted.
    Prepared,
    /// A `hooks.json` write may be in flight or visible.
    PublishingHooks,
    /// `hooks.json` is known to match its desired snapshot; config is untouched.
    HooksPublished,
    /// A `config.toml` write may be in flight or visible.
    PublishingConfig,
}

/// The only persistent intent needed for forward recovery. `InstallOrRefresh`
/// deliberately carries no selected executable: a crash before hooks publish
/// is retried by the next requested command, while a crash after publish can
/// re-derive trust from the current marker-owned handlers.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodexSettingsTransactionOperation {
    InstallOrRefresh,
    Uninstall,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexSettingsTransactionJournal {
    version: u8,
    phase: CodexSettingsTransactionPhase,
    operation: CodexSettingsTransactionOperation,
    hooks_before: CodexSettingsFingerprint,
    hooks_after: CodexSettingsFingerprint,
    config_before: CodexSettingsFingerprint,
    config_after: CodexSettingsFingerprint,
    /// Once a byte comparison observes an uncooperative writer, recovery must
    /// not later forward-publish over that user's state merely because another
    /// Libra invocation happened to acquire the cooperative lock.
    manual_recovery: bool,
}

/// How one currently observed target relates to the exact before/after
/// fingerprints in the durable journal. Recovery only ever writes from the
/// one normal partial state (`hooks_after` plus `config_before`); every other
/// mixed or unknown state is a manual-recovery boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CodexSettingsJournalSnapshotState {
    Before,
    After,
    Unchanged,
    Unknown,
}

impl CodexSettingsJournalSnapshotState {
    fn is_before_or_unchanged(self) -> bool {
        matches!(self, Self::Before | Self::Unchanged)
    }

    fn is_after_or_unchanged(self) -> bool {
        matches!(self, Self::After | Self::Unchanged)
    }
}

fn codex_settings_journal_phase_allows_current_pair(
    phase: CodexSettingsTransactionPhase,
    hooks_state: CodexSettingsJournalSnapshotState,
    config_state: CodexSettingsJournalSnapshotState,
) -> bool {
    use CodexSettingsJournalSnapshotState as State;

    match phase {
        // Before a target write may start, both files must still be their
        // original snapshots (except an unchanged target, which is both).
        CodexSettingsTransactionPhase::Prepared => {
            hooks_state.is_before_or_unchanged() && config_state.is_before_or_unchanged()
        }
        // `hooks.json` may become visible immediately after this marker, but
        // config cannot legitimately move until the next durable phase.
        CodexSettingsTransactionPhase::PublishingHooks => {
            matches!(hooks_state, State::Before | State::After | State::Unchanged)
                && config_state.is_before_or_unchanged()
        }
        // Hooks are proven published before this phase is persisted. Config
        // may still be its original snapshot (or be an unchanged target).
        CodexSettingsTransactionPhase::HooksPublished => {
            hooks_state.is_after_or_unchanged() && config_state.is_before_or_unchanged()
        }
        // A config replacement may be visible even if the process dies before
        // the next verification/removal, so both known config states are
        // admissible here.
        CodexSettingsTransactionPhase::PublishingConfig => {
            hooks_state.is_after_or_unchanged()
                && matches!(
                    config_state,
                    State::Before | State::After | State::Unchanged
                )
        }
    }
}

struct CodexSettingsTransaction {
    hooks_path: PathBuf,
    config_path: PathBuf,
    hooks_before: CodexSettingsSnapshot,
    hooks_after: CodexSettingsSnapshot,
    config_before: CodexSettingsSnapshot,
    config_after: CodexSettingsSnapshot,
    operation: CodexSettingsTransactionOperation,
}

impl CodexSettingsTransaction {
    fn has_changes(&self) -> bool {
        self.hooks_before != self.hooks_after || self.config_before != self.config_after
    }

    fn journal(&self) -> CodexSettingsTransactionJournal {
        CodexSettingsTransactionJournal {
            version: 2,
            phase: CodexSettingsTransactionPhase::Prepared,
            operation: self.operation,
            hooks_before: self.hooks_before.fingerprint(b"hooks.json"),
            hooks_after: self.hooks_after.fingerprint(b"hooks.json"),
            config_before: self.config_before.fingerprint(b"config.toml"),
            config_after: self.config_after.fingerprint(b"config.toml"),
            manual_recovery: false,
        }
    }
}

fn codex_settings_journal_snapshot_state(
    snapshot: &CodexSettingsSnapshot,
    target: &[u8],
    before: &CodexSettingsFingerprint,
    after: &CodexSettingsFingerprint,
) -> CodexSettingsJournalSnapshotState {
    if before == after {
        return if snapshot.fingerprint(target) == *before {
            CodexSettingsJournalSnapshotState::Unchanged
        } else {
            CodexSettingsJournalSnapshotState::Unknown
        };
    }

    let actual = snapshot.fingerprint(target);
    if actual == *before {
        CodexSettingsJournalSnapshotState::Before
    } else if actual == *after {
        CodexSettingsJournalSnapshotState::After
    } else {
        CodexSettingsJournalSnapshotState::Unknown
    }
}

/// Cross-process lock for the coupled Codex settings transaction.
struct CodexSettingsTransactionLock {
    file: fs::File,
}

impl CodexSettingsTransactionLock {
    fn acquire(codex_home: &Path, create_home: bool) -> Result<Option<Self>> {
        if !codex_home.exists() {
            if !create_home {
                return Ok(None);
            }
            fs::create_dir_all(codex_home).with_context(|| {
                format!(
                    "failed to create Codex settings directory '{}' for hook installation",
                    codex_home.display()
                )
            })?;
        }
        if !codex_home.is_dir() {
            bail!(
                "invalid Codex settings directory '{}': expected a directory",
                codex_home.display()
            );
        }

        let path = codex_home.join(CODEX_SETTINGS_TRANSACTION_LOCK_FILE);
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| {
                format!(
                    "failed to open Codex hook settings transaction lock '{}'",
                    path.display()
                )
            })?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => bail!(
                "Codex hook settings transaction '{}' is already in progress; retry after the other Libra hook command finishes",
                path.display()
            ),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(anyhow!(
                    "failed to lock Codex hook settings transaction '{}': {error}",
                    path.display()
                ));
            }
        }
        Ok(Some(Self { file }))
    }
}

impl Drop for CodexSettingsTransactionLock {
    fn drop(&mut self) {
        // Closing releases this lock too. Make release prompt without making a
        // successful settings update fail because an unlock reports an OS race.
        let _ = self.file.unlock();
    }
}

pub(super) fn install_codex_hooks(options: &ProviderInstallOptions) -> Result<()> {
    let binary_path = resolve_hook_binary_path(options.binary_path.as_deref())?;
    let timeout = options
        .timeout_secs
        .unwrap_or(DEFAULT_CODEX_HOOK_TIMEOUT_SECS);
    let codex_home = resolve_codex_home()?;
    install_codex_hooks_at(&codex_home, &binary_path, timeout)
}

pub(super) fn uninstall_codex_hooks() -> Result<()> {
    let codex_home = resolve_codex_home()?;
    uninstall_codex_hooks_at(&codex_home)
}

pub(super) fn codex_hooks_are_installed() -> Result<bool> {
    let codex_home = resolve_codex_home()?;
    let binary_path = resolve_hook_binary_path(None)?;
    codex_hooks_are_installed_at(&codex_home, &binary_path)
}

/// Count Libra-managed handlers in `$CODEX_HOME/hooks.json` lacking a
/// matching + current `trusted_hash` state entry in `$CODEX_HOME/config.toml`
/// (AG-19 trust-gap banner support: Codex skips untrusted hooks *silently*,
/// so the dispatcher surfaces a SessionStart banner when this is non-zero).
///
/// An entry with a matching hash but an explicit `enabled = false` is a
/// deliberate user disable, not a trust gap, and is not counted.
pub(super) fn codex_hook_trust_gaps() -> Result<usize> {
    let codex_home = resolve_codex_home()?;
    codex_hook_trust_gaps_at(&codex_home)
}

/// Resolve `$CODEX_HOME`: the `CODEX_HOME` env var when set (must be
/// absolute — Codex trust-state keys embed the absolute hooks.json path),
/// else `<home>/.codex`, where `<home>` honours the crate's test override
/// (`LIBRA_TEST_HOME`, mirroring the vault and hook-runtime modules) before
/// falling back to [`dirs::home_dir`].
fn resolve_codex_home() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("CODEX_HOME")
        && !raw.is_empty()
    {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            bail!(
                "invalid CODEX_HOME '{}': must be an absolute path (Codex trust-state keys \
                 embed the absolute hooks.json path)",
                path.display()
            );
        }
        return Ok(path);
    }
    let home = std::env::var_os("LIBRA_TEST_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .context(
            "failed to resolve the home directory for Codex hook installation \
             (set CODEX_HOME to override)",
        )?;
    Ok(home.join(".codex"))
}

/// Install/refresh the Libra-managed entries under `codex_home`.
///
/// `hooks.json` and `config.toml` are a coupled trust contract, so this
/// computes and validates both final files from one locked snapshot before
/// publishing either. [`apply_codex_settings_transaction`] records a
/// non-plaintext integrity recovery journal before the first write and uses snapshot comparison plus
/// rollback for every *observed* external edit. The lock serializes Libra
/// processes; arbitrary editors that do not honor it still have an inherent
/// cross-platform check-to-rename race (documented at the publish helper).
fn install_codex_hooks_at(codex_home: &Path, binary_path: &str, timeout: u64) -> Result<()> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    let config_path = codex_home.join(CODEX_CONFIG_FILE);
    let capture_budget_millis = capture_budget_millis_for_host_timeout(timeout)?;

    let _lock = CodexSettingsTransactionLock::acquire(codex_home, true)?.ok_or_else(|| {
        anyhow!(
            "failed to create Codex hook settings transaction lock under '{}'",
            codex_home.display()
        )
    })?;
    recover_codex_settings_transaction(codex_home)?;

    let hooks_before = read_codex_settings_snapshot(&hooks_path, "hooks.json")?;
    let config_before = read_codex_settings_snapshot(&config_path, "config.toml")?;
    let mut file = parse_codex_hooks_snapshot(&hooks_before, &hooks_path)?;
    let changed = upsert_codex_hooks(&mut file, binary_path, timeout, capture_budget_millis)?;

    // Positional-key hazard: always recompute the state keys from the final
    // file and rewrite Libra's trust entries, dropping stale Libra keys that
    // point at outdated indices.
    let entries = libra_state_entries(&hooks_path, &file);
    let remove_exact: BTreeSet<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    let config_after = prepare_codex_config_snapshot(
        &config_before,
        &config_path,
        &hooks_path,
        &remove_exact,
        &entries,
    )?;
    let hooks_after = if changed {
        CodexSettingsSnapshot::present(serialize_codex_hooks_file(&file)?)
    } else {
        hooks_before.clone()
    };
    let state_changed = config_before != config_after;
    apply_codex_settings_transaction(
        codex_home,
        CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before,
            hooks_after,
            config_before,
            config_after,
            operation: CodexSettingsTransactionOperation::InstallOrRefresh,
        },
    )?;

    if changed {
        println!(
            "Installed Codex hook forwarding at {}",
            hooks_path.display()
        );
    } else {
        println!(
            "Codex hook forwarding is already up to date at {}",
            hooks_path.display()
        );
    }
    if state_changed {
        println!(
            "Updated Codex hook trust state at {}",
            config_path.display()
        );
    } else {
        println!(
            "Codex hook trust state is already up to date at {}",
            config_path.display()
        );
    }
    Ok(())
}

/// Remove Libra-managed handlers from `hooks.json` (never deleting the file)
/// and Libra's `[hooks.state]` entries from `config.toml`. Idempotent.
fn uninstall_codex_hooks_at(codex_home: &Path) -> Result<()> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    let config_path = codex_home.join(CODEX_CONFIG_FILE);

    let Some(_lock) = CodexSettingsTransactionLock::acquire(codex_home, false)? else {
        println!("Codex hook settings not found at {}", hooks_path.display());
        return Ok(());
    };
    recover_codex_settings_transaction(codex_home)?;

    let hooks_before = read_codex_settings_snapshot(&hooks_path, "hooks.json")?;
    let config_before = read_codex_settings_snapshot(&config_path, "config.toml")?;

    // Capture the pre-removal positions so exact state keys for durable,
    // marker-owned handlers can be cleaned from config.toml after their
    // positional hook entries disappear.
    let mut stale_keys = BTreeSet::new();
    let hooks_present = hooks_before.is_present();
    let mut file = None;
    let mut hooks_changed = false;
    if hooks_present {
        let mut parsed = parse_codex_hooks_snapshot(&hooks_before, &hooks_path)?;
        for entry in libra_state_entries(&hooks_path, &parsed) {
            stale_keys.insert(entry.key);
        }
        hooks_changed = remove_libra_codex_hooks(&mut parsed)?;
        file = Some(parsed);
    }

    let config_after =
        prepare_codex_config_snapshot(&config_before, &config_path, &hooks_path, &stale_keys, &[])?;
    let hooks_after = match (hooks_changed, file) {
        (true, Some(file)) => CodexSettingsSnapshot::present(serialize_codex_hooks_file(&file)?),
        _ => hooks_before.clone(),
    };
    let state_changed = config_before != config_after;
    apply_codex_settings_transaction(
        codex_home,
        CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before,
            hooks_after,
            config_before,
            config_after,
            operation: CodexSettingsTransactionOperation::Uninstall,
        },
    )?;

    if hooks_changed {
        println!("Removed Codex hook forwarding at {}", hooks_path.display());
    } else if hooks_present {
        println!(
            "No Libra-managed Codex hooks found at {}",
            hooks_path.display()
        );
    } else {
        println!("Codex hook settings not found at {}", hooks_path.display());
    }

    if state_changed {
        println!(
            "Removed Codex hook trust state at {}",
            config_path.display()
        );
    }
    Ok(())
}

/// All forwarded events carry the exact desired command **and** every
/// Libra-managed handler has a current trust entry (`codex exec` silently
/// skips untrusted hooks, so "installed but untrusted" must read as not
/// installed).
fn codex_hooks_are_installed_at(codex_home: &Path, binary_path: &str) -> Result<bool> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    if !hooks_path.exists() {
        return Ok(false);
    }
    let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex")?;
    let all_present = CODEX_HOOK_FORWARD_MAP.iter().all(|(event, verb)| {
        file.hooks.get(*event).is_some_and(|groups| {
            groups.iter().any(|group| {
                group.matcher.is_none()
                    && group.hooks.iter().any(|hook| {
                        let timeout = hook.timeout.unwrap_or(CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS);
                        let Ok(capture_budget_millis) =
                            capture_budget_millis_for_host_timeout(timeout)
                        else {
                            return false;
                        };
                        let (expected_timeout, expected_capture_budget_millis) =
                            codex_hook_timing_for_event(event, timeout, capture_budget_millis);
                        is_managed_codex_hook_for_verb(hook, verb)
                            && hook.timeout == Some(expected_timeout)
                            && hook.command
                                == codex_hook_command(
                                    binary_path,
                                    verb,
                                    expected_capture_budget_millis,
                                )
                    })
            })
        })
    });
    if !all_present {
        return Ok(false);
    }
    Ok(codex_hook_trust_gaps_at(codex_home)? == 0)
}

fn codex_hook_trust_gaps_at(codex_home: &Path) -> Result<usize> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    if !hooks_path.exists() {
        return Ok(0);
    }
    let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex")?;
    let entries = libra_state_entries(&hooks_path, &file);
    if entries.is_empty() {
        return Ok(0);
    }

    let config_path = codex_home.join(CODEX_CONFIG_FILE);
    let config = load_codex_config_value(&config_path)?;
    let gaps = entries
        .iter()
        .filter(|entry| {
            let current = config
                .as_ref()
                .and_then(|value| value.get("hooks"))
                .and_then(|hooks| hooks.get("state"))
                .and_then(|state| state.get(entry.key.as_str()));
            match current {
                Some(state) => {
                    state.get("trusted_hash").and_then(|hash| hash.as_str())
                        != Some(entry.trusted_hash.as_str())
                }
                None => true,
            }
        })
        .count();
    Ok(gaps)
}

/// Parse `config.toml` tolerantly for *reading* (any layout the `toml` crate
/// accepts); returns `None` when the file is missing or blank.
fn load_codex_config_value(config_path: &Path) -> Result<Option<toml::Value>> {
    if !config_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(config_path).with_context(|| {
        format!(
            "failed to read Codex config file '{}'",
            config_path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let value = toml::from_str(&content).map_err(|err| {
        anyhow!(
            "invalid Codex config TOML at '{}': {err}",
            config_path.display()
        )
    })?;
    Ok(Some(value))
}

fn codex_hook_command(binary_path: &str, verb: &str, capture_budget_millis: u64) -> String {
    // Stable installed surface per the Codex capture contract
    // (`docs/development/tracing/agent.md` item 4): the top-level
    // `libra hooks codex <verb>` entry, which routes to AgentTraces.
    format!("{binary_path} hooks codex {verb} --capture-budget-ms {capture_budget_millis}")
}

/// Return the provider-valid handler timing for one Codex event. The user
/// supplied timeout remains effective for every other event. `SessionEnd` is
/// capped only when it exceeds Codex's documented maximum; shorter explicit
/// timeouts retain their normal capture-budget derivation.
fn codex_hook_timing_for_event(
    event_name: &str,
    timeout: u64,
    capture_budget_millis: u64,
) -> (u64, u64) {
    if event_name == "SessionEnd" && timeout > CODEX_SESSION_END_MAX_TIMEOUT_SECS {
        (
            CODEX_SESSION_END_MAX_TIMEOUT_SECS,
            CODEX_SESSION_END_CAPTURE_BUDGET_MILLIS,
        )
    } else {
        (timeout, capture_budget_millis)
    }
}

/// The durable Libra-ownership rule (see module docs). A handler is owned
/// only when it carries the modeled `statusMessage` marker and its direct
/// command grammar, timeout, and capture budget agree exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ManagedCodexCommand {
    Legacy {
        executable: String,
    },
    Canonical {
        executable: String,
        capture_budget_millis: u64,
    },
}

fn is_managed_codex_hook_for_verb(hook: &CodexHookHandler, verb: &str) -> bool {
    if hook.handler_type != "command" {
        return false;
    }
    match is_managed_codex_command_for_verb(&hook.command, verb) {
        // Old marker-less forms are migration candidates only during an
        // explicit enable. They are never durable ownership proof.
        Some(ManagedCodexCommand::Legacy { .. }) => false,
        Some(ManagedCodexCommand::Canonical {
            executable: _,
            capture_budget_millis,
        }) => {
            hook.status_message.as_deref() == Some(LIBRA_CODEX_STATUS_MESSAGE)
                && capture_budget_millis_for_host_timeout(
                    hook.timeout.unwrap_or(CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS),
                )
                .is_ok_and(|expected| expected == capture_budget_millis)
        }
        None => false,
    }
}

/// Migrate an unmarked historical direct command only during an explicit
/// install for the exact selected binary path. Uninstall intentionally calls
/// only the durable matcher above, preserving every unmarked user command
/// even when it happens to look like a Libra hook.
fn is_replaced_managed_codex_hook_for_verb(
    hook: &CodexHookHandler,
    verb: &str,
    binary_path: &str,
) -> bool {
    is_managed_codex_hook_for_verb(hook, verb)
        || is_current_unmarked_codex_hook(hook, verb, binary_path)
}

fn is_current_unmarked_codex_hook(hook: &CodexHookHandler, verb: &str, binary_path: &str) -> bool {
    if hook.handler_type != "command" || hook.status_message.is_some() {
        return false;
    }
    match is_managed_codex_command_for_verb(&hook.command, verb) {
        Some(ManagedCodexCommand::Legacy { executable }) => executable == binary_path,
        Some(ManagedCodexCommand::Canonical {
            executable,
            capture_budget_millis,
        }) => {
            executable == binary_path
                && capture_budget_millis_for_host_timeout(
                    hook.timeout.unwrap_or(CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS),
                )
                .is_ok_and(|expected| expected == capture_budget_millis)
        }
        None => false,
    }
}

#[cfg(test)]
fn parse_managed_codex_command(command: &str) -> Option<ManagedCodexCommand> {
    CODEX_HOOK_FORWARD_MAP
        .iter()
        .find_map(|(_, verb)| is_managed_codex_command_for_verb(command, verb))
}

fn is_managed_codex_command_for_verb(command: &str, verb: &str) -> Option<ManagedCodexCommand> {
    // Check the legacy suffix first: otherwise its `agent` token would be
    // misclassified as part of the executable by the stable-form suffix.
    [
        format!(" agent hooks codex {verb}"),
        format!(" hooks codex {verb}"),
    ]
    .into_iter()
    .find_map(|suffix| {
        let (executable, tail) = command.rsplit_once(&suffix)?;
        match parse_managed_codex_capture_tail(tail)? {
            ManagedCaptureBudgetTail::Legacy => is_direct_absolute_command_executable(executable)
                .map(|executable| ManagedCodexCommand::Legacy { executable }),
            ManagedCaptureBudgetTail::Canonical {
                capture_budget_millis,
            } => is_direct_absolute_command_executable(executable).map(|executable| {
                ManagedCodexCommand::Canonical {
                    executable,
                    capture_budget_millis,
                }
            }),
        }
    })
}

/// Parse one direct command-path token without invoking a shell. This accepts
/// the exact quoting emitted by `setup::quote_command_path`, including the
/// Unix apostrophe escape, then requires an absolute lexical-normal path. A
/// marked `sh -c ...` wrapper must never become a managed hook.
fn is_direct_absolute_command_executable(executable: &str) -> Option<String> {
    let decoded = unquote_direct_command_path(executable)?;
    let path = Path::new(&decoded);
    (path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        }))
    .then_some(executable.to_string())
}

fn unquote_direct_command_path(executable: &str) -> Option<String> {
    if executable.is_empty() || executable.trim() != executable {
        return None;
    }
    match executable.chars().next()? {
        '\'' => unquote_single_quoted_command_path(executable),
        '"' => unquote_double_quoted_command_path(executable),
        _ if executable
            .chars()
            .all(is_conservative_command_path_character) =>
        {
            Some(executable.to_string())
        }
        _ => None,
    }
}

fn is_conservative_command_path_character(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '\\' | '.' | '_' | '-' | ':')
}

fn unquote_single_quoted_command_path(executable: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = executable;
        return None;
    }

    #[cfg(not(windows))]
    {
        let mut rest = executable.strip_prefix('\'')?;
        let mut decoded = String::new();
        loop {
            let (segment, after_quote) = rest.split_once('\'')?;
            decoded.push_str(segment);
            if after_quote.is_empty() {
                return Some(decoded);
            }
            let after_escape = after_quote.strip_prefix(r#"\''"#)?;
            decoded.push('\'');
            rest = after_escape;
        }
    }
}

fn unquote_double_quoted_command_path(executable: &str) -> Option<String> {
    #[cfg(not(windows))]
    {
        let _ = executable;
        None
    }

    #[cfg(windows)]
    {
        let inner = executable.strip_prefix('"')?.strip_suffix('"')?;
        let mut decoded = String::new();
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '"' {
                return None;
            }
            if ch == '\\' && chars.clone().next() == Some('"') {
                chars.next();
                decoded.push('"');
            } else {
                decoded.push(ch);
            }
        }
        Some(decoded)
    }
}

enum ManagedCaptureBudgetTail {
    Legacy,
    Canonical { capture_budget_millis: u64 },
}

fn parse_managed_codex_capture_tail(tail: &str) -> Option<ManagedCaptureBudgetTail> {
    if tail.is_empty() {
        // `agent hooks codex` and the original stable surface did not persist
        // a capture budget. Retain recognition only for migration cleanup.
        return Some(ManagedCaptureBudgetTail::Legacy);
    }

    let value = tail.strip_prefix(" --capture-budget-ms ")?;
    (!value.is_empty() && !value.contains(char::is_whitespace))
        .then(|| value.parse::<u64>().ok())
        .flatten()
        .filter(|budget| is_managed_capture_budget_millis(*budget))
        .map(
            |capture_budget_millis| ManagedCaptureBudgetTail::Canonical {
                capture_budget_millis,
            },
        )
}

fn desired_codex_handler(
    binary_path: &str,
    verb: &str,
    timeout: u64,
    capture_budget_millis: u64,
) -> CodexHookHandler {
    CodexHookHandler {
        handler_type: "command".to_string(),
        command: codex_hook_command(binary_path, verb, capture_budget_millis),
        timeout: Some(timeout),
        status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
        extra: BTreeMap::new(),
    }
}

/// Upsert the Libra-managed matcher groups, preserving every user entry.
///
/// Index-stability strategy (positional trust keys): an existing
/// matcher-less, all-Libra group is updated **in place** so its group index
/// never changes. Duplicate all-Libra groups are cleared but retained as
/// placeholders; a matcher-less mixed group is refused before mutation,
/// because removing only Libra's handler would shift a user's handler index.
/// Missing groups are appended at the end, so user group indices are
/// unaffected.
fn upsert_codex_hooks(
    file: &mut CodexHooksFile,
    binary_path: &str,
    timeout: u64,
    capture_budget_millis: u64,
) -> Result<bool> {
    ensure_no_mixed_managed_codex_groups(file, "refresh", |hook, verb| {
        is_replaced_managed_codex_hook_for_verb(hook, verb, binary_path)
    })?;
    let mut changed = false;

    let mut events: Vec<String> = file.hooks.keys().cloned().collect();
    for (event, _) in CODEX_HOOK_FORWARD_MAP {
        if !events.iter().any(|name| name == event) {
            events.push((*event).to_string());
        }
    }

    for event in events {
        let Some((_, verb)) = CODEX_HOOK_FORWARD_MAP
            .iter()
            .find(|(name, _)| *name == event)
        else {
            // Libra never installs unknown provider event names. Preserve a
            // user group there verbatim, even if its command resembles a
            // current Libra hook.
            continue;
        };
        let (event_timeout, event_capture_budget_millis) =
            codex_hook_timing_for_event(&event, timeout, capture_budget_millis);
        let desired = desired_codex_handler(
            binary_path,
            verb,
            event_timeout,
            event_capture_budget_millis,
        );

        let mut groups = file.hooks.remove(&event).unwrap_or_default();
        let mut satisfied = false;

        for group in &mut groups {
            let libra_owned = group.matcher.is_none()
                && !group.hooks.is_empty()
                && group
                    .hooks
                    .iter()
                    .all(|hook| is_replaced_managed_codex_hook_for_verb(hook, verb, binary_path));
            if libra_owned && !satisfied {
                if group.hooks.len() != 1 || group.hooks[0] != desired || !group.extra.is_empty() {
                    group.hooks = vec![desired.clone()];
                    group.extra.clear();
                    changed = true;
                }
                satisfied = true;
            } else if libra_owned {
                // Preserve this empty matcher group as a positional
                // placeholder. Dropping it would change the group index of
                // every later user hook and make Codex silently skip their
                // existing [hooks.state] approvals.
                group.hooks.clear();
                changed = true;
            }
        }

        if !satisfied {
            groups.push(CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![desired],
                extra: BTreeMap::new(),
            });
            changed = true;
        }

        if !groups.is_empty() {
            file.hooks.insert(event, groups);
        }
    }

    Ok(changed)
}

/// Reject a matcher-less group that would require removing a managed handler
/// while retaining a user handler. Codex keys approval by both group and
/// handler index, so such a retain would silently invalidate a user hook's
/// approval. This preflight runs before either installer path mutates hooks
/// or trust state.
fn ensure_no_mixed_managed_codex_groups<F>(
    file: &CodexHooksFile,
    operation: &str,
    is_managed: F,
) -> Result<()>
where
    F: Fn(&CodexHookHandler, &str) -> bool,
{
    for (event, groups) in &file.hooks {
        let Some((_, verb)) = CODEX_HOOK_FORWARD_MAP
            .iter()
            .find(|(event_name, _)| *event_name == event)
        else {
            continue;
        };
        for group in groups {
            if group.matcher.is_some() {
                continue;
            }
            let managed_count = group
                .hooks
                .iter()
                .filter(|hook| is_managed(hook, verb))
                .count();
            if managed_count > 0 && managed_count < group.hooks.len() {
                bail!(
                    "cannot safely {operation} Libra-managed Codex hooks under '{event}': a matcher-less group mixes Libra and user handlers, so removing Libra would change a user [hooks.state] positional trust key; first move the Libra handler into its own group (or remove it manually), then re-approve the remaining Codex hook"
                );
            }
        }
    }
    Ok(())
}

/// Strip every Libra-managed handler while preserving positional group keys.
/// A group emptied by this cleanup stays as an empty placeholder when a later
/// user group must retain its index; trailing placeholders are dropped so
/// repeated enable/disable cycles do not accumulate empty groups.
fn remove_libra_codex_hooks(file: &mut CodexHooksFile) -> Result<bool> {
    ensure_no_mixed_managed_codex_groups(file, "remove", is_managed_codex_hook_for_verb)?;
    let mut changed = false;
    let events: Vec<String> = file.hooks.keys().cloned().collect();

    for event in events {
        let Some((_, verb)) = CODEX_HOOK_FORWARD_MAP
            .iter()
            .find(|(name, _)| *name == event)
        else {
            // Unknown event names were never written by this installer.
            continue;
        };
        let Some(mut groups) = file.hooks.remove(&event) else {
            continue;
        };
        let mut emptied_by_cleanup = vec![false; groups.len()];
        for (index, group) in groups.iter_mut().enumerate() {
            if group.matcher.is_some() {
                continue;
            }
            if !group.hooks.is_empty()
                && group
                    .hooks
                    .iter()
                    .all(|hook| is_managed_codex_hook_for_verb(hook, verb))
            {
                group.hooks.clear();
                changed = true;
                emptied_by_cleanup[index] = true;
            }
        }

        // A trailing placeholder cannot affect any remaining handler's group
        // index, so remove only the run created by this call. An original
        // empty user group deliberately stops trimming: preserving the user's
        // byte-level group layout is safer than inferring it is disposable.
        let retained_len = emptied_by_cleanup
            .iter()
            .rposition(|emptied| !emptied)
            .map_or(0, |index| index + 1);
        if retained_len < groups.len() {
            groups.truncate(retained_len);
        }
        if !groups.is_empty() {
            file.hooks.insert(event, groups);
        }
    }

    Ok(changed)
}

/// Convert a PascalCase Codex event name to the snake_case label used in
/// `[hooks.state]` keys and canonical identities (matches the eleven upstream
/// labels: `session_start`, `user_prompt_submit`, `pre_tool_use`,
/// `post_tool_use`, `stop`, `subagent_start`, `subagent_stop`,
/// `pre_compact`, `post_compact`, `permission_request`, `session_end`).
fn event_snake_label(event_name: &str) -> String {
    let mut out = String::with_capacity(event_name.len() + 4);
    for (index, ch) in event_name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The exact canonical identity JSON Codex hashes for its trust gate:
/// compact JSON with recursively sorted keys of
/// `{"event_name": <snake>, "matcher": <if any>, "hooks": [{"type":
/// "command", "command": <cmd>, "timeout": <effective>, "async": false,
/// "statusMessage": <if present>}]}`. Libra handlers always run synchronous,
/// so `async` is fixed at `false`.
///
/// Verified against live-probe vectors (codex-cli 0.142.4); see
/// `tests::canonical_identity_hash_matches_live_probe_vectors`.
fn canonical_hook_identity_json(
    event_snake: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    status_message: Option<&str>,
) -> String {
    let mut handler = Map::new();
    handler.insert("type".to_string(), json!("command"));
    handler.insert("command".to_string(), json!(command));
    handler.insert("timeout".to_string(), json!(timeout));
    handler.insert("async".to_string(), json!(false));
    if let Some(message) = status_message {
        handler.insert("statusMessage".to_string(), json!(message));
    }

    let mut root = Map::new();
    root.insert("event_name".to_string(), json!(event_snake));
    if let Some(matcher) = matcher {
        root.insert("matcher".to_string(), json!(matcher));
    }
    root.insert(
        "hooks".to_string(),
        Value::Array(vec![Value::Object(handler)]),
    );

    normalize_json_value(Value::Object(root)).to_string()
}

fn codex_trusted_hash(
    event_snake: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    status_message: Option<&str>,
) -> String {
    let canonical =
        canonical_hook_identity_json(event_snake, matcher, command, timeout, status_message);
    format!("sha256:{}", hex::encode(Sha256::digest(canonical)))
}

/// Enumerate the positional trust entries for every Libra-managed handler in
/// `file` — key `<hooks.json path>:<event_snake>:<group_index>:<handler_index>`
/// plus the canonical hash Codex must find in `[hooks.state]` for the hook
/// to run.
fn libra_state_entries(hooks_path: &Path, file: &CodexHooksFile) -> Vec<CodexStateEntry> {
    let mut entries = Vec::new();
    for (event, groups) in &file.hooks {
        let Some((_, verb)) = CODEX_HOOK_FORWARD_MAP
            .iter()
            .find(|(event_name, _)| *event_name == event)
        else {
            continue;
        };
        let snake = event_snake_label(event);
        for (group_index, group) in groups.iter().enumerate() {
            if group.matcher.is_some() {
                continue;
            }
            for (handler_index, handler) in group.hooks.iter().enumerate() {
                if !is_managed_codex_hook_for_verb(handler, verb) {
                    continue;
                }
                entries.push(CodexStateEntry {
                    key: format!(
                        "{}:{snake}:{group_index}:{handler_index}",
                        hooks_path.display()
                    ),
                    trusted_hash: codex_trusted_hash(
                        &snake,
                        group.matcher.as_deref(),
                        &handler.command,
                        handler
                            .timeout
                            .unwrap_or(CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS),
                        handler.status_message.as_deref(),
                    ),
                });
            }
        }
    }
    entries
}

/// Snapshot both Codex settings files under the transaction lock. We compare
/// these exact bytes again immediately before each publish, so an external
/// edit observed before replacement is never overwritten from a stale parsed
/// representation.
fn read_codex_settings_snapshot(path: &Path, label: &str) -> Result<CodexSettingsSnapshot> {
    match fs::read(path) {
        Ok(bytes) => Ok(CodexSettingsSnapshot::present(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CodexSettingsSnapshot::absent())
        }
        Err(error) => Err(anyhow!(
            "failed to snapshot Codex {label} '{}': {error}",
            path.display()
        )),
    }
}

/// Parse a `hooks.json` snapshot without performing another filesystem read.
/// A missing or whitespace-only file has the same default semantics as the
/// shared JSON settings helper.
fn parse_codex_hooks_snapshot(
    snapshot: &CodexSettingsSnapshot,
    hooks_path: &Path,
) -> Result<CodexHooksFile> {
    let Some(bytes) = snapshot.bytes.as_deref() else {
        return Ok(CodexHooksFile::default());
    };
    let content = std::str::from_utf8(bytes).with_context(|| {
        format!(
            "invalid Codex settings JSON at '{}': file is not valid UTF-8",
            hooks_path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(CodexHooksFile::default());
    }
    serde_json::from_str(content).map_err(|err| {
        anyhow!(
            "invalid Codex settings JSON at '{}': {err}",
            hooks_path.display()
        )
    })
}

/// Render `hooks.json` with the historical pretty-printed trailing-newline
/// contract, but leave publication to the coupled transaction.
fn serialize_codex_hooks_file(file: &CodexHooksFile) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(file)
        .context("failed to serialize Codex settings to JSON for hook installation")?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Read, validate, and render a prospective `config.toml` rewrite from an
/// already-held raw snapshot. This prevents a second, unchecked read between
/// preparing trust state and publishing its paired `hooks.json` change.
fn prepare_codex_config_snapshot(
    config_before: &CodexSettingsSnapshot,
    config_path: &Path,
    hooks_path: &Path,
    remove_exact: &BTreeSet<String>,
    append: &[CodexStateEntry],
) -> Result<CodexSettingsSnapshot> {
    let original = match config_before.bytes.as_deref() {
        Some(bytes) => std::str::from_utf8(bytes).with_context(|| {
            format!(
                "failed to read Codex config file '{}': file is not valid UTF-8",
                config_path.display()
            )
        })?,
        None => "",
    };

    if !original.trim().is_empty() {
        toml::from_str::<toml::Value>(original).map_err(|err| {
            anyhow!(
                "invalid Codex config TOML at '{}': {err}; fix or move the file, then re-run",
                config_path.display()
            )
        })?;
    }

    let rewritten = rewrite_codex_state_sections(
        original,
        &hooks_path.display().to_string(),
        remove_exact,
        append,
    );
    if rewritten == original {
        return Ok(config_before.clone());
    }

    toml::from_str::<toml::Value>(&rewritten).map_err(|err| {
        anyhow!(
            "refusing to update Codex config at '{}': the rewrite would produce invalid TOML \
             ({err}); an existing [hooks.state] entry for a Libra key likely uses a different \
             layout — remove it manually and re-run",
            config_path.display()
        )
    })?;

    Ok(CodexSettingsSnapshot::present(rewritten.into_bytes()))
}

/// Execute a fully-prepared two-file Codex update. The caller must already
/// hold [`CodexSettingsTransactionLock`]. A non-plaintext integrity journal is
/// persisted before the first target-file write. It lets a later command
/// forward-reconcile a crash without storing a copy of user settings on disk;
/// in-process errors still use the memory-only snapshots for rollback.
fn apply_codex_settings_transaction(
    codex_home: &Path,
    transaction: CodexSettingsTransaction,
) -> Result<()> {
    if !transaction.has_changes() {
        return Ok(());
    }

    let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
    let mut journal = transaction.journal();
    write_codex_settings_transaction_journal(&journal_path, &journal)?;

    match publish_codex_settings_transaction(&transaction, &journal_path, &mut journal) {
        Ok(()) => remove_codex_settings_transaction_journal(&journal_path),
        Err(error) => {
            // A durable write can report an I/O error after replacement is
            // visible. If both final snapshots are already present, accept the
            // completed transaction rather than rolling it back needlessly.
            if codex_settings_transaction_matches_desired(&transaction)? {
                return remove_codex_settings_transaction_journal(&journal_path);
            }
            // A detected outside write is never auto-forwarded on a later
            // invocation. Try to preserve a sticky manual marker *before*
            // rollback, but do not return early if that persistence fails:
            // rollback can still restore a target we demonstrably own. If the
            // marker write itself fails, the foreign bytes necessarily fail
            // the journal's exact fingerprints on the next invocation, which
            // is independently fail-closed (see recovery below).
            let manual_marker_error = if error.observed_conflict {
                journal.manual_recovery = true;
                write_codex_settings_transaction_journal(&journal_path, &journal).err()
            } else {
                None
            };

            match rollback_codex_settings_transaction_from_memory(&transaction, &journal_path) {
                Ok(()) if error.observed_conflict => {
                    let marker_context = match manual_marker_error {
                        Some(marker_error) => format!(
                            " Libra could not persist the sticky manual marker ({marker_error:#}), \
                             but the externally changed bytes do not match this journal's fingerprints, \
                             so later recovery will still refuse automatic writes"
                        ),
                        None => " Future automatic writes are blocked by the journal's manual-recovery marker".to_string(),
                    };
                    Err(error.error.context(format!(
                        "Codex hook settings update observed a concurrent edit; the non-plaintext recovery journal '{}' was retained.{marker_context}",
                        journal_path.display()
                    )))
                }
                Ok(()) => {
                    remove_codex_settings_transaction_journal(&journal_path)?;
                    Err(error.error.context(
                        "Codex hook settings update was rolled back; no partial hook/trust update was left behind",
                    ))
                }
                Err(rollback_error) => {
                    let rollback_marker_error = if !journal.manual_recovery {
                        journal.manual_recovery = true;
                        write_codex_settings_transaction_journal(&journal_path, &journal).err()
                    } else {
                        manual_marker_error
                    };
                    let marker_context = match rollback_marker_error {
                        Some(marker_error) => format!(
                            " The manual-recovery marker could not be persisted ({marker_error:#}); \
                             any state outside the journal fingerprints remains fail-closed on the next invocation."
                        ),
                        None => {
                            " Future automatic writes are blocked by the journal's manual-recovery marker."
                                .to_string()
                        }
                    };
                    Err(anyhow!(
                        "Codex hook settings update failed: {:#}; automatic rollback could not safely finish: {rollback_error:#}. \
                         The non-plaintext recovery journal '{}' was retained.{marker_context}",
                        error.error,
                        journal_path.display()
                    ))
                }
            }
        }
    }
}

fn codex_settings_transaction_matches_desired(
    transaction: &CodexSettingsTransaction,
) -> Result<bool> {
    Ok(
        read_codex_settings_snapshot(&transaction.hooks_path, "hooks.json")?
            == transaction.hooks_after
            && read_codex_settings_snapshot(&transaction.config_path, "config.toml")?
                == transaction.config_after,
    )
}

/// A publish error carries whether the compare step observed a changed file.
/// That distinction decides whether the durable intent may be retried or must
/// be converted into a sticky manual-recovery record.
struct CodexSettingsPublishError {
    error: anyhow::Error,
    observed_conflict: bool,
}

impl CodexSettingsPublishError {
    fn other(error: anyhow::Error) -> Self {
        Self {
            error,
            observed_conflict: false,
        }
    }

    fn conflict(error: anyhow::Error) -> Self {
        Self {
            error,
            observed_conflict: true,
        }
    }
}

/// Publish an already prepared transaction while advancing its durable phase.
/// This is shared by the initial operation and forward recovery; only the
/// former has memory snapshots available for rollback.
fn publish_codex_settings_transaction(
    transaction: &CodexSettingsTransaction,
    journal_path: &Path,
    journal: &mut CodexSettingsTransactionJournal,
) -> std::result::Result<(), CodexSettingsPublishError> {
    if transaction.hooks_before != transaction.hooks_after {
        journal.phase = CodexSettingsTransactionPhase::PublishingHooks;
        write_codex_settings_transaction_journal(journal_path, journal)
            .map_err(CodexSettingsPublishError::other)?;
        publish_codex_settings_snapshot(
            &transaction.hooks_path,
            &transaction.hooks_before,
            &transaction.hooks_after,
            "hooks.json",
            journal_path,
            CodexSettingsTransactionPhase::PublishingHooks,
        )?;
        journal.phase = CodexSettingsTransactionPhase::HooksPublished;
        write_codex_settings_transaction_journal(journal_path, journal)
            .map_err(CodexSettingsPublishError::other)?;
    }

    if transaction.config_before != transaction.config_after {
        journal.phase = CodexSettingsTransactionPhase::PublishingConfig;
        write_codex_settings_transaction_journal(journal_path, journal)
            .map_err(CodexSettingsPublishError::other)?;
        publish_codex_settings_snapshot(
            &transaction.config_path,
            &transaction.config_before,
            &transaction.config_after,
            "config.toml",
            journal_path,
            CodexSettingsTransactionPhase::PublishingConfig,
        )?;
    }

    verify_codex_settings_snapshot(
        &transaction.hooks_path,
        &transaction.hooks_after,
        "hooks.json",
        journal_path,
    )?;
    verify_codex_settings_snapshot(
        &transaction.config_path,
        &transaction.config_after,
        "config.toml",
        journal_path,
    )?;
    Ok(())
}

/// Write one target only when it still byte-matches the snapshot used to
/// prepare the transaction. This compare-before-publish check is intentionally
/// repeated after the test-only phase seam, because an external editor may
/// race while Libra holds only its own cooperative lock.
///
/// The underlying portable rename APIs do not expose compare-and-replace for
/// arbitrary existing files. An uncooperative writer can still replace the
/// target after the byte comparison and before our rename. The `$CODEX_HOME`
/// lock prevents that among Libra processes; snapshot verification, rollback,
/// and the durable journal protect every conflict we can observe without
/// pretending to provide a stronger OS primitive than exists on Windows and
/// Unix alike.
fn publish_codex_settings_snapshot(
    path: &Path,
    expected: &CodexSettingsSnapshot,
    desired: &CodexSettingsSnapshot,
    label: &str,
    journal_path: &Path,
    phase: CodexSettingsTransactionPhase,
) -> std::result::Result<(), CodexSettingsPublishError> {
    run_codex_settings_transaction_test_phase(phase).map_err(CodexSettingsPublishError::other)?;
    verify_codex_settings_snapshot(path, expected, label, journal_path)?;
    write_codex_settings_snapshot(path, desired, label)
        .map_err(CodexSettingsPublishError::other)?;
    verify_codex_settings_snapshot(path, desired, label, journal_path)
}

fn verify_codex_settings_snapshot(
    path: &Path,
    expected: &CodexSettingsSnapshot,
    label: &str,
    journal_path: &Path,
) -> std::result::Result<(), CodexSettingsPublishError> {
    let actual =
        read_codex_settings_snapshot(path, label).map_err(CodexSettingsPublishError::other)?;
    if actual == *expected {
        return Ok(());
    }
    Err(CodexSettingsPublishError::conflict(anyhow!(
        "Codex {label} '{}' changed concurrently; Libra did not overwrite the observed edit. \
         The non-plaintext integrity recovery journal '{}' records the interrupted operation for safe recovery",
        path.display(),
        journal_path.display()
    )))
}

fn write_codex_settings_snapshot(
    path: &Path,
    desired: &CodexSettingsSnapshot,
    label: &str,
) -> Result<()> {
    match desired.bytes.as_deref() {
        Some(bytes) => atomic_write::write_atomic(path, bytes, true).with_context(|| {
            format!(
                "failed to atomically publish Codex {label} settings file '{}'",
                path.display()
            )
        }),
        None => atomic_write::remove_durably(path).with_context(|| {
            format!(
                "failed to remove Codex {label} settings file '{}'",
                path.display()
            )
        }),
    }
}

/// Recover an unfinished transaction at the beginning of every install and
/// uninstall while the same cooperative lock is held.
fn recover_codex_settings_transaction(codex_home: &Path) -> Result<()> {
    let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
    match fs::metadata(&journal_path) {
        Ok(_) => recover_codex_settings_transaction_from_journal(codex_home, &journal_path)
            .map_err(|error| {
                anyhow!(
                    "Codex hook settings recovery is required before continuing: {error:#}. \
                     The non-plaintext integrity recovery journal '{}' was left in place; resolve the external edit before retrying",
                    journal_path.display()
                )
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow!(
            "failed to inspect Codex hook recovery journal '{}': {error}",
            journal_path.display()
        )),
    }
}

fn recover_codex_settings_transaction_from_journal(
    codex_home: &Path,
    journal_path: &Path,
) -> Result<()> {
    let mut journal = read_codex_settings_transaction_journal(journal_path)?;
    if journal.manual_recovery {
        bail!(
            "the non-plaintext integrity Codex hook recovery journal '{}' records an observed concurrent edit; \
             do not automatically overwrite either settings file. Resolve the edit manually, then remove the journal and retry",
            journal_path.display()
        );
    }

    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    let config_path = codex_home.join(CODEX_CONFIG_FILE);
    let hooks_current = read_codex_settings_snapshot(&hooks_path, "hooks.json")?;
    let config_current = read_codex_settings_snapshot(&config_path, "config.toml")?;
    let hooks_state = codex_settings_journal_snapshot_state(
        &hooks_current,
        b"hooks.json",
        &journal.hooks_before,
        &journal.hooks_after,
    );
    let config_state = codex_settings_journal_snapshot_state(
        &config_current,
        b"config.toml",
        &journal.config_before,
        &journal.config_after,
    );

    // The phase is useful crash evidence, but it cannot be a mutation proof:
    // a process can die after an atomic replacement becomes visible and before
    // it advances the journal. The v2 before/after fingerprints are therefore
    // the authority for every recovery decision.
    if !codex_settings_journal_phase_allows_current_pair(journal.phase, hooks_state, config_state) {
        return codex_settings_recovery_requires_manual(
            journal_path,
            &mut journal,
            "the journal phase does not permit the currently observed hooks/config fingerprint pair",
        );
    }

    if hooks_state.is_after_or_unchanged() && config_state.is_after_or_unchanged() {
        return remove_codex_settings_transaction_journal(journal_path);
    }
    if hooks_state.is_before_or_unchanged() && config_state.is_before_or_unchanged() {
        // No target mutation survived. Do not replay an incomplete install:
        // the command that triggered recovery can prepare its own desired
        // handler set after this journal has been safely consumed.
        return remove_codex_settings_transaction_journal(journal_path);
    }

    // The normal ordering is hooks first, then config. Only the exact
    // hooks-after/config-before pair can be safely resumed without persisting
    // user settings bytes. Every other mixed or unknown pair is a potential
    // external edit (or a corrupt journal), so it must remain zero-write.
    if hooks_state != CodexSettingsJournalSnapshotState::After
        || config_state != CodexSettingsJournalSnapshotState::Before
    {
        return codex_settings_recovery_requires_manual(
            journal_path,
            &mut journal,
            "the current hooks/config fingerprints are not a known resumable transaction state",
        );
    }

    let transaction = prepare_codex_forward_recovery_transaction(codex_home, journal.operation)?;
    if let Err(error) = validate_codex_forward_recovery_transaction(&transaction, &journal) {
        return codex_settings_recovery_requires_manual(
            journal_path,
            &mut journal,
            &format!(
                "re-derived pending settings do not match the journal's intended fingerprints ({error:#})"
            ),
        );
    }

    match publish_codex_settings_transaction(&transaction, journal_path, &mut journal) {
        Ok(()) => remove_codex_settings_transaction_journal(journal_path),
        Err(error) if error.observed_conflict => codex_settings_recovery_requires_manual(
            journal_path,
            &mut journal,
            &format!(
                "the resumed config publication observed a concurrent edit ({:#})",
                error.error
            ),
        ),
        Err(error) => Err(error.error.context(
            "Codex hook settings recovery could not complete; the exact-fingerprint journal was retained for a later safe retry",
        )),
    }
}

/// Ensure a resumed write is exactly the post-image recorded by the original
/// operation. The journal never contains plaintext snapshots, so re-deriving
/// the output and checking its digest is the proof that permits the one safe
/// forward transition.
fn validate_codex_forward_recovery_transaction(
    transaction: &CodexSettingsTransaction,
    journal: &CodexSettingsTransactionJournal,
) -> Result<()> {
    if transaction.hooks_before.fingerprint(b"hooks.json") != journal.hooks_after
        || transaction.hooks_after.fingerprint(b"hooks.json") != journal.hooks_after
        || transaction.config_before.fingerprint(b"config.toml") != journal.config_before
        || transaction.config_after.fingerprint(b"config.toml") != journal.config_after
    {
        bail!(
            "the marker-owned hooks or re-derived config does not equal the durable transaction post-image"
        );
    }
    Ok(())
}

/// Fence an uncertain transaction before returning an error. If persisting the
/// fence itself fails, the current pair was already classified as outside the
/// journal's exact fingerprint transitions, so the next invocation remains
/// fail-closed even with the older on-disk journal bytes.
fn codex_settings_recovery_requires_manual(
    journal_path: &Path,
    journal: &mut CodexSettingsTransactionJournal,
    reason: &str,
) -> Result<()> {
    journal.manual_recovery = true;
    match write_codex_settings_transaction_journal(journal_path, journal) {
        Ok(()) => bail!(
            "{reason}; automatic recovery will not overwrite either settings file. Resolve the edit manually, then remove the non-plaintext recovery journal '{}' and retry",
            journal_path.display()
        ),
        Err(marker_error) => bail!(
            "{reason}; Libra also could not persist the manual-recovery fence ({marker_error:#}). The current bytes are outside this journal's fingerprints, so future recovery remains zero-write. Resolve the settings manually and remove the non-plaintext recovery journal '{}' before retrying",
            journal_path.display()
        ),
    }
}

/// Rebuild a safe forward-recovery transaction from the *current* marker-owned
/// handlers. This intentionally never parses or replays a persisted user
/// settings snapshot. Install recovery only restores matching trust entries;
/// the caller's subsequent install refresh adds missing handlers if a crash
/// happened before hooks publication. Uninstall recovery removes only durable
/// marker-owned handlers and their state entries.
fn prepare_codex_forward_recovery_transaction(
    codex_home: &Path,
    operation: CodexSettingsTransactionOperation,
) -> Result<CodexSettingsTransaction> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    let config_path = codex_home.join(CODEX_CONFIG_FILE);
    let hooks_before = read_codex_settings_snapshot(&hooks_path, "hooks.json")?;
    let config_before = read_codex_settings_snapshot(&config_path, "config.toml")?;

    match operation {
        CodexSettingsTransactionOperation::InstallOrRefresh => {
            let file = parse_codex_hooks_snapshot(&hooks_before, &hooks_path)?;
            let entries = libra_state_entries(&hooks_path, &file);
            let remove_exact: BTreeSet<String> =
                entries.iter().map(|entry| entry.key.clone()).collect();
            let config_after = prepare_codex_config_snapshot(
                &config_before,
                &config_path,
                &hooks_path,
                &remove_exact,
                &entries,
            )?;
            Ok(CodexSettingsTransaction {
                hooks_path,
                config_path,
                hooks_before: hooks_before.clone(),
                hooks_after: hooks_before,
                config_before,
                config_after,
                operation,
            })
        }
        CodexSettingsTransactionOperation::Uninstall => {
            let mut stale_keys = BTreeSet::new();
            let mut file = None;
            let mut hooks_changed = false;
            if hooks_before.is_present() {
                let mut parsed = parse_codex_hooks_snapshot(&hooks_before, &hooks_path)?;
                for entry in libra_state_entries(&hooks_path, &parsed) {
                    stale_keys.insert(entry.key);
                }
                hooks_changed = remove_libra_codex_hooks(&mut parsed)?;
                file = Some(parsed);
            }
            let config_after = prepare_codex_config_snapshot(
                &config_before,
                &config_path,
                &hooks_path,
                &stale_keys,
                &[],
            )?;
            let hooks_after = match (hooks_changed, file) {
                (true, Some(file)) => {
                    CodexSettingsSnapshot::present(serialize_codex_hooks_file(&file)?)
                }
                _ => hooks_before.clone(),
            };
            Ok(CodexSettingsTransaction {
                hooks_path,
                config_path,
                hooks_before,
                hooks_after,
                config_before,
                config_after,
                operation,
            })
        }
    }
}

fn rollback_codex_settings_transaction_from_memory(
    transaction: &CodexSettingsTransaction,
    journal_path: &Path,
) -> Result<()> {
    let hooks_current = read_codex_settings_snapshot(&transaction.hooks_path, "hooks.json")?;
    let config_current = read_codex_settings_snapshot(&transaction.config_path, "config.toml")?;
    let hooks_result = rollback_codex_settings_snapshot_if_owned(
        &transaction.hooks_path,
        &transaction.hooks_before,
        &transaction.hooks_after,
        &hooks_current,
        "hooks.json",
        journal_path,
    );
    let config_result = rollback_codex_settings_snapshot_if_owned(
        &transaction.config_path,
        &transaction.config_before,
        &transaction.config_after,
        &config_current,
        "config.toml",
        journal_path,
    );
    hooks_result?;
    config_result
}

/// Roll back a target only when its current bytes still exactly equal the
/// transaction's desired snapshot. A user edit that differs from both the old
/// and intended bytes is an actionable observed conflict, not permission to
/// overwrite. As with publication, no portable API can prove the file remains
/// unchanged in the instant between this comparison and a replacement.
fn rollback_codex_settings_snapshot_if_owned(
    path: &Path,
    before: &CodexSettingsSnapshot,
    after: &CodexSettingsSnapshot,
    current: &CodexSettingsSnapshot,
    label: &str,
    journal_path: &Path,
) -> Result<()> {
    if before == after || current == before {
        return Ok(());
    }
    if current != after {
        bail!(
            "Codex {label} '{}' was externally changed while the paired update was in progress; \
             refusing to overwrite it. Use the non-plaintext integrity recovery journal '{}' to reconcile the settings",
            path.display(),
            journal_path.display()
        );
    }
    write_codex_settings_snapshot(path, before, label)?;
    let restored = read_codex_settings_snapshot(path, label)?;
    if restored == *before {
        Ok(())
    } else {
        bail!(
            "Codex {label} '{}' changed while rollback was publishing; refusing to overwrite it. \
             Use the non-plaintext integrity recovery journal '{}' to reconcile the settings",
            path.display(),
            journal_path.display()
        );
    }
}

fn write_codex_settings_transaction_journal(
    journal_path: &Path,
    journal: &CodexSettingsTransactionJournal,
) -> Result<()> {
    run_codex_settings_journal_test_hook(journal)?;
    let bytes = serde_json::to_vec(journal)
        .context("failed to serialize non-plaintext Codex hook recovery journal")?;
    atomic_write::write_atomic(journal_path, &bytes, true).with_context(|| {
        format!(
            "failed to persist non-plaintext Codex hook recovery journal '{}'",
            journal_path.display()
        )
    })
}

fn read_codex_settings_transaction_journal(
    journal_path: &Path,
) -> Result<CodexSettingsTransactionJournal> {
    let bytes = fs::read(journal_path).with_context(|| {
        format!(
            "failed to read non-plaintext Codex hook recovery journal '{}'",
            journal_path.display()
        )
    })?;
    let journal: CodexSettingsTransactionJournal = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow!(
            "invalid Codex hook recovery journal '{}': {error}; leave it in place and repair the settings before retrying",
            journal_path.display()
        )
    })?;
    if journal.version != 2 {
        bail!(
            "unsupported Codex hook recovery journal version at '{}'; upgrade Libra or resolve the paired settings manually",
            journal_path.display()
        );
    }
    journal.hooks_before.validate("hooks-before")?;
    journal.hooks_after.validate("hooks-after")?;
    journal.config_before.validate("config-before")?;
    journal.config_after.validate("config-after")?;
    Ok(journal)
}

fn remove_codex_settings_transaction_journal(journal_path: &Path) -> Result<()> {
    atomic_write::remove_durably(journal_path).with_context(|| {
        format!(
            "failed to remove Codex hook recovery journal '{}' after completing recovery",
            journal_path.display()
        )
    })
}

/// Test-only deterministic seam. Production has no environment-variable or
/// user-controlled injection surface; tests can force a second-publication
/// error or mutate one target after the first publication has succeeded.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum CodexSettingsTransactionTestPhase {
    BeforeHooksPublish,
    BeforeConfigPublish,
}

#[cfg(test)]
type CodexSettingsTransactionTestHook =
    Box<dyn Fn(CodexSettingsTransactionTestPhase) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static CODEX_SETTINGS_TRANSACTION_TEST_HOOK: std::cell::RefCell<Option<CodexSettingsTransactionTestHook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct CodexSettingsTransactionTestHookGuard {
    previous: Option<CodexSettingsTransactionTestHook>,
}

#[cfg(test)]
impl CodexSettingsTransactionTestHookGuard {
    fn install(hook: impl Fn(CodexSettingsTransactionTestPhase) -> Result<()> + 'static) -> Self {
        let previous =
            CODEX_SETTINGS_TRANSACTION_TEST_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for CodexSettingsTransactionTestHookGuard {
    fn drop(&mut self) {
        CODEX_SETTINGS_TRANSACTION_TEST_HOOK.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

// Test-only seam for a failed durable manual-recovery fence. It deliberately
// takes the fully structured journal rather than exposing a production
// environment variable or a path/bytes injection surface.
#[cfg(test)]
type CodexSettingsJournalTestHook = Box<dyn Fn(&CodexSettingsTransactionJournal) -> Result<()>>;

#[cfg(test)]
thread_local! {
    static CODEX_SETTINGS_JOURNAL_TEST_HOOK: std::cell::RefCell<Option<CodexSettingsJournalTestHook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct CodexSettingsJournalTestHookGuard {
    previous: Option<CodexSettingsJournalTestHook>,
}

#[cfg(test)]
impl CodexSettingsJournalTestHookGuard {
    fn install(hook: impl Fn(&CodexSettingsTransactionJournal) -> Result<()> + 'static) -> Self {
        let previous =
            CODEX_SETTINGS_JOURNAL_TEST_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for CodexSettingsJournalTestHookGuard {
    fn drop(&mut self) {
        CODEX_SETTINGS_JOURNAL_TEST_HOOK.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
fn run_codex_settings_journal_test_hook(journal: &CodexSettingsTransactionJournal) -> Result<()> {
    CODEX_SETTINGS_JOURNAL_TEST_HOOK.with(|hook| {
        let hook = hook.borrow();
        match hook.as_ref() {
            Some(hook) => hook(journal),
            None => Ok(()),
        }
    })
}

#[cfg(not(test))]
fn run_codex_settings_journal_test_hook(_journal: &CodexSettingsTransactionJournal) -> Result<()> {
    Ok(())
}

#[cfg(test)]
fn run_codex_settings_transaction_test_phase(phase: CodexSettingsTransactionPhase) -> Result<()> {
    let Some(test_phase) = (match phase {
        CodexSettingsTransactionPhase::PublishingHooks => {
            Some(CodexSettingsTransactionTestPhase::BeforeHooksPublish)
        }
        CodexSettingsTransactionPhase::PublishingConfig => {
            Some(CodexSettingsTransactionTestPhase::BeforeConfigPublish)
        }
        CodexSettingsTransactionPhase::Prepared | CodexSettingsTransactionPhase::HooksPublished => {
            None
        }
    }) else {
        return Ok(());
    };
    CODEX_SETTINGS_TRANSACTION_TEST_HOOK.with(|hook| {
        let hook = hook.borrow();
        match hook.as_ref() {
            Some(hook) => hook(test_phase),
            None => Ok(()),
        }
    })
}

#[cfg(not(test))]
fn run_codex_settings_transaction_test_phase(_phase: CodexSettingsTransactionPhase) -> Result<()> {
    Ok(())
}

/// The minimal line-based `[hooks.state]` section editor (see module docs).
///
/// Removes (a) sections whose exact key is in `remove_exact` and (b)
/// marker-tagged sections whose key starts with `<hooks.json path>:` (stale
/// Libra keys with outdated indices), then appends `append` as fresh
/// marker-tagged sections. Every other byte of the file is preserved; the
/// round trip install→reinstall and install→uninstall is byte-stable (pinned
/// by tests).
fn rewrite_codex_state_sections(
    content: &str,
    hooks_json_path: &str,
    remove_exact: &BTreeSet<String>,
    append: &[CodexStateEntry],
) -> String {
    let our_prefix = format!("{hooks_json_path}:");
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut removed_any = false;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if let Some(key) = parse_state_section_key(line) {
            let marked = out
                .last()
                .is_some_and(|previous| previous.trim() == CODEX_STATE_MARKER);
            if remove_exact.contains(&key) || (marked && key.starts_with(&our_prefix)) {
                if marked {
                    out.pop();
                }
                removed_any = true;
                index += 1;
                // Consume the section body: everything up to the next
                // section header or the next Libra marker (which belongs to
                // the following section). The final empty element of the
                // `split('\n')` encodes the file's trailing newline, not a
                // body line — consuming it would eat a trailing blank line
                // the user's config carried before install and break the
                // byte-restore contract (uninstall must reproduce the
                // pre-install bytes exactly).
                while index < lines.len() {
                    let body = lines[index];
                    if body.trim_start().starts_with('[') || body.trim() == CODEX_STATE_MARKER {
                        break;
                    }
                    if body.is_empty() && index == lines.len() - 1 {
                        break;
                    }
                    index += 1;
                }
                continue;
            }
        }
        out.push(line);
        index += 1;
    }

    let mut result = out.join("\n");
    if (removed_any || !append.is_empty()) && !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    for entry in append {
        result.push_str(&format!(
            "{CODEX_STATE_MARKER}\n[hooks.state.\"{}\"]\nenabled = true\ntrusted_hash = \"{}\"\n",
            escape_toml_basic_string(&entry.key),
            entry.trusted_hash,
        ));
    }
    result
}

/// Parse a `[hooks.state."<key>"]` / `[hooks.state.'<key>']` header line into
/// its unescaped key. Returns `None` for anything else (including layouts we
/// do not own, which are then left untouched).
fn parse_state_section_key(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let body = trimmed
        .strip_prefix("[hooks.state.")?
        .strip_suffix(']')?
        .trim();
    if let Some(inner) = body
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        unescape_toml_basic_string(inner)
    } else {
        body.strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .map(str::to_string)
    }
}

/// Escape a raw string for use inside a TOML basic (double-quoted) string.
fn escape_toml_basic_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ch if (ch as u32) < 0x20 || ch == '\u{7F}' => {
                out.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out
}

/// Minimal inverse of [`escape_toml_basic_string`]; returns `None` on any
/// escape sequence it does not understand so the caller treats the section
/// as foreign and leaves it untouched.
fn unescape_toml_basic_string(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'b' => out.push('\u{0008}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'f' => out.push('\u{000C}'),
            'r' => out.push('\r'),
            'u' => {
                let digits: String = chars.by_ref().take(4).collect();
                if digits.len() != 4 {
                    return None;
                }
                let code = u32::from_str_radix(&digits, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            'U' => {
                let digits: String = chars.by_ref().take(8).collect();
                if digits.len() != 8 {
                    return None;
                }
                let code = u32::from_str_radix(&digits, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Fixture-only trust-state writer for migration tests. Production installation
/// and removal always use the coupled transaction above; this helper exists so
/// a test can seed an older trusted handler before exercising the migration.
#[cfg(test)]
fn sync_codex_trust_state(
    config_path: &Path,
    hooks_path: &Path,
    desired: &[CodexStateEntry],
) -> Result<bool> {
    let before = read_codex_settings_snapshot(config_path, "config.toml")?;
    let remove_exact: BTreeSet<String> = desired.iter().map(|entry| entry.key.clone()).collect();
    let after =
        prepare_codex_config_snapshot(&before, config_path, hooks_path, &remove_exact, desired)?;
    if before == after {
        return Ok(false);
    }
    write_codex_settings_snapshot(config_path, &after, "config.toml")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn codex_hook_forward_map_installs_and_parses_subagent_boundaries() {
        assert!(CODEX_HOOK_FORWARD_MAP.contains(&("SubagentStart", "subagent-start")));
        assert!(CODEX_HOOK_FORWARD_MAP.contains(&("SubagentStop", "subagent-end")));
        let mut hooks = CodexHooksFile::default();
        assert!(
            upsert_codex_hooks(&mut hooks, BINARY, 30, 29_000)
                .expect("install subagent forwarding hooks")
        );
        assert!(hooks.hooks.contains_key("SubagentStart"));
        assert!(hooks.hooks.contains_key("SubagentStop"));

        let envelope = crate::internal::ai::hooks::SessionHookEnvelope {
            hook_event_name: "SubagentStart".to_string(),
            session_id: "session-1".to_string(),
            cwd: "/tmp".to_string(),
            transcript_path: None,
            extra: serde_json::Map::new(),
        };
        let start = super::super::parser::parse_codex_hook_event("SubagentStart", &envelope)
            .expect("parse installed start boundary");
        let stop = super::super::parser::parse_codex_hook_event("SubagentStop", &envelope)
            .expect("parse installed stop boundary");
        assert_eq!(
            start.kind,
            crate::internal::ai::hooks::LifecycleEventKind::SubagentStart
        );
        assert_eq!(
            stop.kind,
            crate::internal::ai::hooks::LifecycleEventKind::SubagentEnd
        );
    }

    const BINARY: &str = "/opt/libra";

    fn hooks_path_of(codex_home: &Path) -> PathBuf {
        codex_home.join(CODEX_HOOKS_FILE)
    }

    fn config_path_of(codex_home: &Path) -> PathBuf {
        codex_home.join(CODEX_CONFIG_FILE)
    }

    /// Live-probe ground truth (codex-cli 0.142.4, 2026-07-05): the
    /// canonical-identity hash must reproduce the byte-exact `trusted_hash`
    /// values Codex itself wrote for these two hooks.
    #[test]
    fn canonical_identity_hash_matches_live_probe_vectors() {
        assert_eq!(
            codex_trusted_hash(
                "session_start",
                None,
                "/tmp/claude-1000/codex-hooks-probe/hook.sh user-session-start",
                600,
                None,
            ),
            "sha256:11a16641ac6ee4381e5ae3674660428e467257b8cc7f18c68637c64725ef6195",
        );
        assert_eq!(
            codex_trusted_hash(
                "pre_tool_use",
                None,
                "/tmp/claude-1000/codex-hooks-probe/hook.sh user-pre-tool-use",
                600,
                None,
            ),
            "sha256:eddbdb39ec91f713977b8fef8f233e9e78aa8512013c4806a859862660217688",
        );
    }

    /// The canonical identity JSON is compact with recursively sorted keys;
    /// `matcher` and `statusMessage` slot into their sorted positions.
    #[test]
    fn canonical_identity_json_is_compact_and_sorted() {
        assert_eq!(
            canonical_hook_identity_json(
                "session_start",
                None,
                "/opt/libra hooks codex session-start",
                30,
                None,
            ),
            r#"{"event_name":"session_start","hooks":[{"async":false,"command":"/opt/libra hooks codex session-start","timeout":30,"type":"command"}]}"#,
        );
        assert_eq!(
            canonical_hook_identity_json(
                "pre_tool_use",
                Some("Bash.*"),
                "/opt/libra hooks codex tool-use",
                30,
                Some("libra capture"),
            ),
            r#"{"event_name":"pre_tool_use","hooks":[{"async":false,"command":"/opt/libra hooks codex tool-use","statusMessage":"libra capture","timeout":30,"type":"command"}],"matcher":"Bash.*"}"#,
        );
    }

    /// The generic PascalCase→snake conversion reproduces all eleven upstream
    /// event labels used in `[hooks.state]` keys.
    #[test]
    fn event_snake_label_matches_upstream_labels() {
        let cases = [
            ("SessionStart", "session_start"),
            ("UserPromptSubmit", "user_prompt_submit"),
            ("PreToolUse", "pre_tool_use"),
            ("PostToolUse", "post_tool_use"),
            ("Stop", "stop"),
            ("SubagentStart", "subagent_start"),
            ("SubagentStop", "subagent_stop"),
            ("PreCompact", "pre_compact"),
            ("PostCompact", "post_compact"),
            ("PermissionRequest", "permission_request"),
            ("SessionEnd", "session_end"),
        ];
        for (event, expected) in cases {
            assert_eq!(event_snake_label(event), expected, "event {event}");
        }
    }

    #[test]
    fn upsert_codex_hooks_is_idempotent() {
        let mut file = CodexHooksFile::default();
        assert!(upsert_codex_hooks(&mut file, BINARY, 30, 29_000).expect("install Codex hooks"));
        assert!(!upsert_codex_hooks(&mut file, BINARY, 30, 29_000).expect("refresh Codex hooks"));
        assert_eq!(file.hooks.len(), CODEX_HOOK_FORWARD_MAP.len());
        for (event, verb) in CODEX_HOOK_FORWARD_MAP {
            let (expected_timeout, expected_capture_budget_millis) =
                codex_hook_timing_for_event(event, 30, 29_000);
            let groups = file.hooks.get(*event).expect("event installed");
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].hooks.len(), 1);
            assert_eq!(
                groups[0].hooks[0].command,
                codex_hook_command(BINARY, verb, expected_capture_budget_millis)
            );
            assert_eq!(groups[0].hooks[0].timeout, Some(expected_timeout));
            assert_eq!(
                groups[0].hooks[0].status_message.as_deref(),
                Some(LIBRA_CODEX_STATUS_MESSAGE)
            );
        }
    }

    #[test]
    fn renamed_binary_install_is_idempotent_and_uninstallable() {
        let binary = "'/opt/Libra Install/capture-hook-custom-name'";
        let mut file = CodexHooksFile::default();

        assert!(
            upsert_codex_hooks(&mut file, binary, 30, 29_000).expect("install renamed Codex hooks")
        );
        assert!(
            !upsert_codex_hooks(&mut file, binary, 30, 29_000)
                .expect("refresh renamed Codex hooks")
        );
        for (event, verb) in CODEX_HOOK_FORWARD_MAP {
            let (expected_timeout, expected_capture_budget_millis) =
                codex_hook_timing_for_event(event, 30, 29_000);
            let group = &file.hooks[*event][0];
            assert_eq!(group.hooks.len(), 1, "{event}");
            assert_eq!(
                group.hooks[0].command,
                codex_hook_command(binary, verb, expected_capture_budget_millis)
            );
            assert_eq!(group.hooks[0].timeout, Some(expected_timeout));
            assert_eq!(
                group.hooks[0].status_message.as_deref(),
                Some(LIBRA_CODEX_STATUS_MESSAGE)
            );
        }

        assert!(remove_libra_codex_hooks(&mut file).expect("remove Codex hooks"));
        assert!(file.hooks.is_empty());
    }

    #[test]
    fn custom_command_path_parser_accepts_only_installer_quoting() {
        let escaped_apostrophe = r#"'/opt/Libra'\''s/capture-hook'"#;
        assert_eq!(
            unquote_direct_command_path(escaped_apostrophe).as_deref(),
            Some("/opt/Libra's/capture-hook")
        );
        assert!(is_direct_absolute_command_executable(escaped_apostrophe).is_some());
        assert!(is_direct_absolute_command_executable("/opt/$(not-a-binary)").is_none());
        assert!(is_direct_absolute_command_executable("sh -c /opt/libra").is_none());

        #[cfg(not(windows))]
        assert!(is_direct_absolute_command_executable("\"/opt/$(not-a-binary)\"").is_none());
    }

    #[test]
    fn install_migrates_only_the_exact_unmarked_renamed_binary() {
        let binary = "/opt/capture-hook-custom-name";
        let old = CodexHookHandler {
            handler_type: "command".to_string(),
            command: codex_hook_command(binary, "session-start", 29_000),
            timeout: Some(30),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![old],
                extra: BTreeMap::new(),
            }],
        );

        assert!(
            upsert_codex_hooks(&mut file, binary, 30, 29_000).expect("migrate renamed Codex hook")
        );
        assert!(
            !upsert_codex_hooks(&mut file, binary, 30, 29_000)
                .expect("refresh migrated renamed Codex hook")
        );
        let session_start = &file.hooks["SessionStart"];
        assert_eq!(session_start.len(), 1);
        assert_eq!(session_start[0].hooks.len(), 1);
        assert_eq!(
            session_start[0].hooks[0].status_message.as_deref(),
            Some(LIBRA_CODEX_STATUS_MESSAGE)
        );
        assert!(remove_libra_codex_hooks(&mut file).expect("remove Codex hooks"));
        assert!(file.hooks.is_empty());
    }

    #[test]
    fn unmarked_standard_canonical_hook_migrates_and_uninstalls() {
        let old = CodexHookHandler {
            handler_type: "command".to_string(),
            command: codex_hook_command(BINARY, "session-start", 29_000),
            timeout: Some(30),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![old],
                extra: BTreeMap::new(),
            }],
        );

        assert!(
            upsert_codex_hooks(&mut file, BINARY, 30, 29_000).expect("migrate standard Codex hook")
        );
        assert!(
            !upsert_codex_hooks(&mut file, BINARY, 30, 29_000)
                .expect("refresh migrated standard Codex hook")
        );
        assert_eq!(
            file.hooks["SessionStart"][0].hooks[0]
                .status_message
                .as_deref(),
            Some(LIBRA_CODEX_STATUS_MESSAGE)
        );
        assert!(remove_libra_codex_hooks(&mut file).expect("remove Codex hooks"));
        assert!(file.hooks.is_empty());
    }

    #[test]
    fn explicit_enable_adopts_only_exact_current_unmarked_codex_handlers() {
        let handler = |command: &str, timeout: Option<u64>| CodexHookHandler {
            handler_type: "command".to_string(),
            command: command.to_string(),
            timeout,
            status_message: None,
            extra: BTreeMap::new(),
        };
        let bare_legacy = handler("libra agent hooks codex session-start", Some(30));
        let bare_canonical = handler(
            "libra hooks codex session-start --capture-budget-ms 29000",
            Some(30),
        );
        let other_legacy = handler("/other/libra agent hooks codex session-start", Some(30));
        let other_canonical = handler(
            "/other/libra hooks codex session-start --capture-budget-ms 29000",
            Some(30),
        );
        let exact_legacy = handler("/opt/libra agent hooks codex session-start", Some(30));
        let exact_canonical = handler(
            "/opt/libra hooks codex stop --capture-budget-ms 29000",
            Some(30),
        );
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![
                CodexHookMatcherGroup {
                    matcher: None,
                    hooks: vec![
                        bare_legacy.clone(),
                        bare_canonical.clone(),
                        other_legacy.clone(),
                        other_canonical.clone(),
                    ],
                    extra: BTreeMap::new(),
                },
                // Explicit enable may adopt the exact selected binary, but
                // only from an exclusively owned group: changing a mixed
                // group's handler layout would invalidate user trust keys.
                CodexHookMatcherGroup {
                    matcher: None,
                    hooks: vec![exact_legacy],
                    extra: BTreeMap::new(),
                },
            ],
        );
        file.hooks.insert(
            "Stop".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![exact_canonical],
                extra: BTreeMap::new(),
            }],
        );

        assert!(
            !remove_libra_codex_hooks(&mut file).expect("inspect unmarked Codex hooks for removal"),
            "disable must not claim any unmarked handler, including exact-current history"
        );
        assert!(
            upsert_codex_hooks(&mut file, BINARY, 30, 29_000)
                .expect("adopt exact current Codex hooks")
        );
        let session_start = file
            .hooks
            .get("SessionStart")
            .expect("SessionStart groups")
            .iter()
            .flat_map(|group| group.hooks.iter())
            .collect::<Vec<_>>();
        for user_handler in [
            &bare_legacy,
            &bare_canonical,
            &other_legacy,
            &other_canonical,
        ] {
            assert!(
                session_start.contains(&user_handler),
                "enable must preserve unmarked user handler: {user_handler:?}"
            );
        }
        assert!(session_start.iter().any(|handler| {
            handler.status_message.as_deref() == Some(LIBRA_CODEX_STATUS_MESSAGE)
                && handler.command == codex_hook_command(BINARY, "session-start", 29_000)
        }));
        assert!(
            !session_start
                .iter()
                .any(|handler| handler.command == "/opt/libra agent hooks codex session-start")
        );
        let stop = file
            .hooks
            .get("Stop")
            .expect("Stop groups")
            .iter()
            .flat_map(|group| group.hooks.iter())
            .collect::<Vec<_>>();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].command, codex_hook_command(BINARY, "stop", 29_000));
        assert_eq!(
            stop[0].status_message.as_deref(),
            Some(LIBRA_CODEX_STATUS_MESSAGE)
        );

        let trusted = libra_state_entries(Path::new("/tmp/hooks.json"), &file);
        assert_eq!(trusted.len(), CODEX_HOOK_FORWARD_MAP.len());
        assert!(
            trusted
                .iter()
                .any(|entry| entry.key.ends_with(":session_start:1:0"))
        );
        assert!(
            !trusted
                .iter()
                .any(|entry| entry.key.ends_with(":session_start:0:0")),
            "unmarked user handlers in the first group must not receive a Libra trust entry"
        );

        assert!(remove_libra_codex_hooks(&mut file).expect("remove Codex hooks"));
        let after_disable = file
            .hooks
            .get("SessionStart")
            .expect("user SessionStart handlers remain")
            .iter()
            .flat_map(|group| group.hooks.iter())
            .collect::<Vec<_>>();
        assert_eq!(after_disable.len(), 4);
        for user_handler in [
            &bare_legacy,
            &bare_canonical,
            &other_legacy,
            &other_canonical,
        ] {
            assert!(
                after_disable.contains(&user_handler),
                "disable must preserve unmarked user handler: {user_handler:?}"
            );
        }
        assert!(!file.hooks.contains_key("Stop"));
        assert!(libra_state_entries(Path::new("/tmp/hooks.json"), &file).is_empty());
    }

    /// A marked stale handler from an older install (different binary path)
    /// is replaced *in place*, keeping its group index — positional trust
    /// keys must not drift on binary upgrades.
    #[test]
    fn upsert_replaces_stale_binary_in_place() {
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![
                CodexHookMatcherGroup {
                    matcher: None,
                    hooks: vec![CodexHookHandler {
                        handler_type: "command".to_string(),
                        command:
                            "/old/path/libra hooks codex session-start --capture-budget-ms 9000"
                                .to_string(),
                        timeout: Some(10),
                        status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
                        extra: BTreeMap::new(),
                    }],
                    extra: BTreeMap::new(),
                },
                CodexHookMatcherGroup {
                    matcher: Some("user".to_string()),
                    hooks: vec![CodexHookHandler {
                        handler_type: "command".to_string(),
                        command: "echo user".to_string(),
                        timeout: None,
                        status_message: None,
                        extra: BTreeMap::new(),
                    }],
                    extra: BTreeMap::new(),
                },
            ],
        );

        assert!(
            upsert_codex_hooks(&mut file, BINARY, 30, 29_000).expect("replace stale Codex hook")
        );
        let groups = file.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(groups.len(), 2, "no group added or removed");
        assert_eq!(
            groups[0].hooks[0].command,
            codex_hook_command(BINARY, "session-start", 29_000),
            "Libra group updated in place at index 0",
        );
        assert_eq!(groups[1].hooks[0].command, "echo user");
    }

    #[test]
    fn codex_command_parser_requires_exact_direct_path_grammar() {
        for command in [
            "/old/path/libra agent hooks codex session-start",
            "/opt/libra hooks codex session-start",
            "'/opt/Libra Install/libra' hooks codex stop --capture-budget-ms 29000",
            "/opt/libra.exe hooks codex subagent-end --capture-budget-ms 500",
            "/usr/local/bin/user-wrapper hooks codex session-start",
        ] {
            assert!(
                parse_managed_codex_command(command).is_some(),
                "should parse: {command}"
            );
        }

        for command in [
            "echo hooks codex session-start",
            "/opt/libra hooks codex unknown-event",
            "/opt/libra hooks codex session-start --capture-budget-ms 1",
            "/opt/libra hooks codex session-start --extra",
            "sh -c '/opt/libra hooks codex session-start'",
            "\" hooks codex session-start",
        ] {
            assert!(
                parse_managed_codex_command(command).is_none(),
                "must reject unsupported command grammar: {command}"
            );
        }

        assert!(!is_managed_codex_hook_for_verb(
            &CodexHookHandler {
                handler_type: "prompt".to_string(),
                command: "/opt/libra hooks codex session-start --capture-budget-ms 29000"
                    .to_string(),
                timeout: Some(30),
                status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_codex_hook_for_verb(
            &CodexHookHandler {
                handler_type: "command".to_string(),
                command: "/usr/local/bin/user-wrapper hooks codex session-start".to_string(),
                timeout: Some(30),
                status_message: None,
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_codex_hook_for_verb(
            &CodexHookHandler {
                handler_type: "command".to_string(),
                command: "/opt/libra hooks codex session-start --capture-budget-ms 500".to_string(),
                timeout: Some(30),
                status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        // A marker-less public command is never durable ownership proof,
        // even when it uses the standard basename and exact budget.
        assert!(!is_managed_codex_hook_for_verb(
            &CodexHookHandler {
                handler_type: "command".to_string(),
                command: "/opt/libra hooks codex session-start --capture-budget-ms 29000"
                    .to_string(),
                timeout: Some(30),
                status_message: None,
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(is_managed_codex_hook_for_verb(
            &CodexHookHandler {
                handler_type: "command".to_string(),
                command: "/opt/capture-hook-custom-name hooks codex session-start --capture-budget-ms 29000"
                    .to_string(),
                timeout: Some(30),
                status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_codex_hook_for_verb(
            &CodexHookHandler {
                handler_type: "command".to_string(),
                command: "/opt/capture-hook-custom-name hooks codex session-start --capture-budget-ms 29000"
                    .to_string(),
                timeout: Some(30),
                status_message: None,
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
    }

    #[test]
    fn install_and_uninstall_preserve_user_command_with_codex_substring() {
        let user_command = "/usr/local/bin/user-wrapper hooks codex session-start";
        let mismatched_budget_command =
            "/opt/libra hooks codex session-start --capture-budget-ms 500";
        let unmarked_canonical_command =
            "/opt/custom-capture-command hooks codex session-start --capture-budget-ms 29000";
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![
                    CodexHookHandler {
                        handler_type: "command".to_string(),
                        command: user_command.to_string(),
                        timeout: Some(3),
                        status_message: None,
                        extra: BTreeMap::new(),
                    },
                    CodexHookHandler {
                        handler_type: "command".to_string(),
                        command: mismatched_budget_command.to_string(),
                        timeout: Some(30),
                        status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
                        extra: BTreeMap::new(),
                    },
                    CodexHookHandler {
                        handler_type: "command".to_string(),
                        command: unmarked_canonical_command.to_string(),
                        timeout: Some(30),
                        status_message: None,
                        extra: BTreeMap::new(),
                    },
                ],
                extra: BTreeMap::new(),
            }],
        );

        assert!(
            upsert_codex_hooks(&mut file, BINARY, 30, 29_000)
                .expect("install alongside user Codex commands")
        );
        for expected in [
            user_command,
            mismatched_budget_command,
            unmarked_canonical_command,
        ] {
            assert!(
                file.hooks
                    .get("SessionStart")
                    .expect("SessionStart groups")
                    .iter()
                    .flat_map(|group| &group.hooks)
                    .any(|hook| hook.command == expected),
                "install must retain user command {expected:?}"
            );
        }
        assert_eq!(
            libra_state_entries(Path::new("/tmp/hooks.json"), &file).len(),
            CODEX_HOOK_FORWARD_MAP.len(),
            "a non-Libra command must not receive a Libra trust entry"
        );

        assert!(remove_libra_codex_hooks(&mut file).expect("remove Codex hooks"));
        assert!(
            file.hooks
                .get("SessionStart")
                .expect("SessionStart user group")
                .iter()
                .flat_map(|group| &group.hooks)
                .map(|hook| hook.command.as_str())
                .eq([
                    user_command,
                    mismatched_budget_command,
                    unmarked_canonical_command,
                ]),
            "uninstall must not delete user commands that merely resemble Libra hooks"
        );
    }

    #[test]
    fn installed_status_rejects_an_unmarked_canonical_user_handler() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let mut hooks: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let session_start = hooks
            .hooks
            .get_mut("SessionStart")
            .expect("installed SessionStart group");
        session_start[0].hooks[0].status_message = None;
        write_json_settings(&hooks_path, &hooks, "Codex").expect("seed unmarked user handler");

        assert!(
            !codex_hooks_are_installed_at(&codex_home, BINARY).expect("installed status"),
            "a canonical-looking handler without Libra's status marker must not suppress install"
        );
    }

    #[test]
    fn installed_status_rejects_session_end_above_codex_provider_cap() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let mut hooks: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let session_end = hooks
            .hooks
            .get_mut("SessionEnd")
            .expect("installed SessionEnd group");
        let handler = &mut session_end[0].hooks[0];
        handler.timeout = Some(30);
        handler.command = codex_hook_command(BINARY, "session-end", 29_000);
        write_json_settings(&hooks_path, &hooks, "Codex").expect("seed invalid SessionEnd");

        assert!(
            !codex_hooks_are_installed_at(&codex_home, BINARY).expect("installed status"),
            "a SessionEnd handler above Codex's provider cap must be refreshed"
        );
    }

    #[test]
    fn enable_migrates_marked_stale_session_end_timing_and_trust_in_place() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let stale_command = codex_hook_command(BINARY, "session-end", 29_000);
        let stale_hash = codex_trusted_hash(
            "session_end",
            None,
            &stale_command,
            30,
            Some(LIBRA_CODEX_STATUS_MESSAGE),
        );
        let mut stale_file: CodexHooksFile =
            load_json_settings(&hooks_path, "Codex").expect("load installed hooks");
        let stale_handler =
            &mut stale_file.hooks.get_mut("SessionEnd").expect("SessionEnd")[0].hooks[0];
        stale_handler.timeout = Some(30);
        stale_handler.command = stale_command;
        write_json_settings(&hooks_path, &stale_file, "Codex").expect("write stale SessionEnd");
        let stale_entries = libra_state_entries(&hooks_path, &stale_file);
        assert!(
            sync_codex_trust_state(&config_path, &hooks_path, &stale_entries)
                .expect("trust stale SessionEnd"),
            "the old SessionEnd identity must be represented in the seeded trust state"
        );
        assert!(
            fs::read_to_string(&config_path)
                .expect("read stale trust state")
                .contains(&stale_hash),
            "fixture must start with a trusted old 30s SessionEnd handler"
        );

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("refresh SessionEnd timing");
        let refreshed: CodexHooksFile =
            load_json_settings(&hooks_path, "Codex").expect("load refreshed hooks");
        let handler = &refreshed.hooks["SessionEnd"][0].hooks[0];
        assert_eq!(handler.timeout, Some(3));
        assert_eq!(
            handler.command,
            codex_hook_command(BINARY, "session-end", 2_000)
        );
        assert_eq!(
            handler.status_message.as_deref(),
            Some(LIBRA_CODEX_STATUS_MESSAGE)
        );
        let refreshed_config =
            fs::read_to_string(&config_path).expect("read refreshed trust state");
        let refreshed_hash = codex_trusted_hash(
            "session_end",
            None,
            &codex_hook_command(BINARY, "session-end", 2_000),
            3,
            Some(LIBRA_CODEX_STATUS_MESSAGE),
        );
        assert!(!refreshed_config.contains(&stale_hash));
        assert!(refreshed_config.contains(&refreshed_hash));

        let hooks_before = fs::read_to_string(&hooks_path).expect("read refreshed hooks");
        let config_before = refreshed_config;
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("idempotent refresh");
        assert_eq!(
            fs::read_to_string(&hooks_path).expect("read hooks"),
            hooks_before
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            config_before
        );

        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        let after_uninstall: CodexHooksFile =
            load_json_settings(&hooks_path, "Codex").expect("load hooks after uninstall");
        assert!(
            !after_uninstall.hooks.contains_key("SessionEnd"),
            "uninstall must remove the migrated managed SessionEnd handler"
        );
        assert!(
            !fs::read_to_string(&config_path)
                .expect("read config after uninstall")
                .contains("session_end"),
            "uninstall must remove the migrated SessionEnd trust state"
        );
    }

    #[test]
    fn session_end_cap_applies_to_long_requested_timeout() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        install_codex_hooks_at(&codex_home, BINARY, 601).expect("install long timeout");
        let hooks: CodexHooksFile =
            load_json_settings(&hooks_path_of(&codex_home), "Codex").expect("load hooks");

        let session_end = &hooks.hooks["SessionEnd"][0].hooks[0];
        assert_eq!(session_end.timeout, Some(3));
        assert_eq!(
            session_end.command,
            codex_hook_command(BINARY, "session-end", 2_000)
        );
        let ordinary = &hooks.hooks["SessionStart"][0].hooks[0];
        assert_eq!(ordinary.timeout, Some(601));
        assert_eq!(
            ordinary.command,
            codex_hook_command(BINARY, "session-start", 600_000)
        );
        assert!(codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
    }

    #[test]
    fn install_and_uninstall_preserve_scoped_wrong_and_unknown_event_commands() {
        let canonical = codex_hook_command(BINARY, "session-start", 29_000);
        let scoped_user_hook = CodexHookHandler {
            handler_type: "command".to_string(),
            command: canonical,
            timeout: Some(30),
            status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
            extra: BTreeMap::new(),
        };
        let wrong_event_legacy_hook = CodexHookHandler {
            handler_type: "command".to_string(),
            command: "/old/libra hooks codex session-start".to_string(),
            timeout: None,
            status_message: None,
            extra: BTreeMap::new(),
        };
        let unknown_event_legacy_hook = wrong_event_legacy_hook.clone();
        let known_event_legacy_hook = CodexHookHandler {
            handler_type: "command".to_string(),
            command: "/old/libra hooks codex session-end".to_string(),
            timeout: None,
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: Some("Bash".to_string()),
                hooks: vec![scoped_user_hook.clone()],
                extra: BTreeMap::new(),
            }],
        );
        file.hooks.insert(
            "Stop".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![wrong_event_legacy_hook.clone()],
                extra: BTreeMap::new(),
            }],
        );
        file.hooks.insert(
            "UserCustomEvent".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![unknown_event_legacy_hook.clone()],
                extra: BTreeMap::new(),
            }],
        );
        file.hooks.insert(
            "SessionEnd".to_string(),
            vec![CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![known_event_legacy_hook],
                extra: BTreeMap::new(),
            }],
        );

        assert!(
            upsert_codex_hooks(&mut file, BINARY, 30, 29_000)
                .expect("install alongside scoped Codex commands")
        );
        for (event, expected) in [
            ("SessionStart", &scoped_user_hook),
            ("Stop", &wrong_event_legacy_hook),
            ("UserCustomEvent", &unknown_event_legacy_hook),
        ] {
            assert!(
                file.hooks
                    .get(event)
                    .into_iter()
                    .flatten()
                    .flat_map(|group| &group.hooks)
                    .any(|hook| hook == expected),
                "install must preserve the user-owned {event} hook: {file:?}"
            );
        }
        assert_eq!(
            libra_state_entries(Path::new("/tmp/hooks.json"), &file).len(),
            CODEX_HOOK_FORWARD_MAP.len(),
            "scoped, wrong-event and unknown-event user hooks must not get Libra trust entries"
        );

        assert!(remove_libra_codex_hooks(&mut file).expect("remove Codex hooks"));
        for (event, expected) in [
            ("SessionStart", &scoped_user_hook),
            ("Stop", &wrong_event_legacy_hook),
            ("UserCustomEvent", &unknown_event_legacy_hook),
        ] {
            assert!(
                file.hooks
                    .get(event)
                    .into_iter()
                    .flatten()
                    .flat_map(|group| &group.hooks)
                    .any(|hook| hook == expected),
                "uninstall must preserve the user-owned {event} hook: {file:?}"
            );
        }
        assert!(
            file.hooks
                .get("SessionEnd")
                .into_iter()
                .flatten()
                .flat_map(|group| &group.hooks)
                .any(|hook| hook.command == "/old/libra hooks codex session-end"),
            "unmarked legacy command must remain user-owned after uninstall: {file:?}"
        );
    }

    /// Full round trip against a tempdir CODEX_HOME: user entries in both
    /// files survive byte-for-byte (config.toml) / structurally (hooks.json),
    /// reinstall is byte-stable, and uninstall restores the user's
    /// config.toml exactly.
    #[test]
    fn install_round_trip_preserves_user_content() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);

        fs::create_dir_all(&codex_home).expect("create codex home");
        let user_hooks = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"matcher": "startup", "hooks": [
                        {"type": "command", "command": "echo keep", "timeout": 3}
                    ]}
                ],
                "PreToolUse": [
                    {"hooks": [{"type": "command", "command": "echo pre"}]}
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&user_hooks).expect("render"),
        )
        .expect("seed hooks.json");
        let user_config = "# user config\nmodel = \"gpt-5.4\"\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n\n[hooks.state.\"/elsewhere/hooks.json:stop:0:0\"]\nenabled = true\ntrusted_hash = \"sha256:userhash\"\n";
        fs::write(&config_path, user_config).expect("seed config.toml");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        // hooks.json: user groups intact, Libra appended after the user's
        // SessionStart group; the user's PreToolUse group is preserved and
        // Libra adds its own forwarding group alongside it.
        let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let session_start = file.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(session_start.len(), 2);
        assert_eq!(session_start[0].hooks[0].command, "echo keep");
        assert_eq!(
            session_start[1].hooks[0].command,
            codex_hook_command(BINARY, "session-start", 29_000),
        );
        assert_eq!(
            file.hooks.get("PreToolUse").expect("PreToolUse")[0].hooks[0].command,
            "echo pre",
        );
        assert_eq!(file.hooks.get("PreToolUse").expect("PreToolUse").len(), 2);

        // config.toml: user bytes are an exact prefix; our sections appended
        // with the user's SessionStart group shifting ours to index 1.
        let config_after = fs::read_to_string(&config_path).expect("read config");
        assert!(
            config_after.starts_with(user_config),
            "user config bytes must be preserved as an exact prefix; got:\n{config_after}",
        );
        let session_start_key = format!("{}:session_start:1:0", hooks_path.display());
        assert!(
            config_after.contains(&format!(
                "[hooks.state.\"{}\"]",
                escape_toml_basic_string(&session_start_key)
            )),
            "our SessionStart trust key must use group index 1; got:\n{config_after}",
        );
        let expected_hash = codex_trusted_hash(
            "session_start",
            None,
            &codex_hook_command(BINARY, "session-start", 29_000),
            30,
            Some(LIBRA_CODEX_STATUS_MESSAGE),
        );
        assert!(config_after.contains(&expected_hash));

        assert!(codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Reinstall: both files byte-stable.
        let hooks_before = fs::read_to_string(&hooks_path).expect("read hooks");
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("re-install");
        assert_eq!(
            fs::read_to_string(&hooks_path).expect("read hooks"),
            hooks_before
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            config_after,
        );

        // Uninstall: user config restored byte-for-byte; user hooks intact.
        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
            "uninstall must restore the user's config.toml byte-for-byte",
        );
        let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let session_start = file.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(session_start.len(), 1);
        assert_eq!(session_start[0].hooks[0].command, "echo keep");
        assert!(hooks_path.exists(), "hooks.json is never deleted");
        assert!(!codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Idempotent second uninstall.
        uninstall_codex_hooks_at(&codex_home).expect("second uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
        );
    }

    /// Codex keys hook approval by matcher-group and handler position. A
    /// Libra-only group before a user group must therefore remain as an empty
    /// placeholder on uninstall, or the user hook would move from `1:0` to
    /// `0:0` and Codex would silently stop running its existing approval.
    #[test]
    fn uninstall_preserves_user_positional_trust_after_pure_libra_group() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let user_handler = CodexHookHandler {
            handler_type: "command".to_string(),
            command: "/usr/local/bin/user-codex-hook".to_string(),
            timeout: Some(30),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut hooks: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        hooks
            .hooks
            .get_mut("SessionStart")
            .expect("installed SessionStart")
            .push(CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![user_handler.clone()],
                extra: BTreeMap::new(),
            });
        write_json_settings(&hooks_path, &hooks, "Codex").expect("add user group");

        let user_key = format!("{}:session_start:1:0", hooks_path.display());
        let user_hash = codex_trusted_hash(
            "session_start",
            None,
            &user_handler.command,
            user_handler.timeout.expect("test user handler timeout"),
            user_handler.status_message.as_deref(),
        );
        let mut config = fs::read_to_string(&config_path).expect("read managed config");
        config.push_str(&format!(
            "\n[hooks.state.\"{}\"]\nenabled = true\ntrusted_hash = \"{}\"\n",
            escape_toml_basic_string(&user_key),
            user_hash,
        ));
        fs::write(&config_path, &config).expect("add user trust state");

        uninstall_codex_hooks_at(&codex_home).expect("uninstall");

        let after: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load after");
        let groups = after
            .hooks
            .get("SessionStart")
            .expect("SessionStart remains");
        assert_eq!(groups.len(), 2, "the former Libra group keeps index zero");
        assert!(groups[0].hooks.is_empty(), "index-zero placeholder remains");
        assert_eq!(groups[1].hooks, vec![user_handler]);

        let config_after = fs::read_to_string(&config_path).expect("read config after uninstall");
        assert!(
            config_after.contains(&format!(
                "[hooks.state.\"{}\"]\nenabled = true\ntrusted_hash = \"{}\"",
                escape_toml_basic_string(&user_key),
                user_hash,
            )),
            "the user approval must retain its original positional key: {config_after}",
        );
        assert!(
            !config_after.contains(CODEX_STATE_MARKER),
            "only Libra-marked state is removed: {config_after}",
        );
    }

    /// Removing one handler from a mixed matcher-less group would shift each
    /// following handler's Codex trust key. Both enable and disable must
    /// reject the layout before either hooks.json or config.toml is written.
    #[test]
    fn mixed_codex_group_refuses_enable_and_disable_without_mutation() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let mut hooks: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        hooks
            .hooks
            .get_mut("SessionStart")
            .expect("installed SessionStart")[0]
            .hooks
            .push(CodexHookHandler {
                handler_type: "command".to_string(),
                command: "/usr/local/bin/user-codex-hook".to_string(),
                timeout: Some(30),
                status_message: None,
                extra: BTreeMap::new(),
            });
        write_json_settings(&hooks_path, &hooks, "Codex").expect("make mixed group");

        let user_key = format!("{}:session_start:0:1", hooks_path.display());
        let mut config = fs::read_to_string(&config_path).expect("read managed config");
        config.push_str(&format!(
            "\n[hooks.state.\"{}\"]\nenabled = true\ntrusted_hash = \"sha256:user-approved\"\n",
            escape_toml_basic_string(&user_key),
        ));
        fs::write(&config_path, &config).expect("add user trust state");
        let hooks_before = fs::read_to_string(&hooks_path).expect("read mixed hooks");
        let config_before = fs::read_to_string(&config_path).expect("read mixed config");

        let enable_error = install_codex_hooks_at(&codex_home, BINARY, 30)
            .expect_err("enable must reject a mixed matcher-less group");
        assert!(
            enable_error
                .to_string()
                .contains("mixes Libra and user handlers"),
            "unexpected enable error: {enable_error:#}",
        );
        assert_eq!(
            fs::read_to_string(&hooks_path).expect("read hooks"),
            hooks_before
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            config_before
        );

        let disable_error = uninstall_codex_hooks_at(&codex_home)
            .expect_err("disable must reject a mixed matcher-less group");
        assert!(
            disable_error
                .to_string()
                .contains("mixes Libra and user handlers"),
            "unexpected disable error: {disable_error:#}",
        );
        assert_eq!(
            fs::read_to_string(&hooks_path).expect("read hooks"),
            hooks_before
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            config_before
        );
    }

    /// A user config.toml with CRLF line endings (including a user-owned
    /// `[hooks.state."…"]` section) survives install → uninstall
    /// byte-for-byte: `split('\n')` keeps each `\r` attached to its line
    /// and `join("\n")` reassembles them unchanged, while
    /// `parse_state_section_key` trims the `\r` before matching keys —
    /// refutation evidence for the review claim that the line-based
    /// editor corrupts CRLF files.
    #[test]
    fn crlf_user_config_survives_install_and_uninstall_byte_for_byte() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let config_path = config_path_of(&codex_home);
        let user_config = "# user config\r\nmodel = \"gpt-5.4\"\r\n\r\n[hooks.state.\"/elsewhere/hooks.json:stop:0:0\"]\r\nenabled = true\r\ntrusted_hash = \"sha256:userhash\"\r\n";
        fs::write(&config_path, user_config).expect("seed CRLF config.toml");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let after_install = fs::read_to_string(&config_path).expect("read config");
        assert!(
            after_install.starts_with(user_config),
            "user CRLF bytes must survive install as an exact prefix; got:\n{after_install:?}",
        );
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Reinstall is byte-stable on the CRLF file too.
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("re-install");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            after_install,
            "reinstall over a CRLF user config must be byte-stable",
        );

        // Uninstall restores the CRLF user bytes exactly — user section,
        // `\r` terminators and all.
        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
            "uninstall must restore the user's CRLF config byte-for-byte",
        );
    }

    /// A user config.toml ending with a trailing blank line (here the
    /// real-world shape that broke the A6.5 local smoke on 2026-07-12: a
    /// bare user-owned `[hooks.state]` header followed by a blank line at
    /// EOF) survives install → uninstall byte-for-byte. Regression: the
    /// section-body consumer used to swallow the final empty `split('\n')`
    /// element — the encoding of the file's trailing newline — so uninstall
    /// returned the file one byte short (`…[hooks.state]\n` instead of
    /// `…[hooks.state]\n\n`).
    #[test]
    fn trailing_blank_line_user_config_survives_install_and_uninstall() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let config_path = config_path_of(&codex_home);
        let user_config =
            "# user config\nmodel = \"gpt-5.4\"\n\n[hooks]\nenabled = false\n\n[hooks.state]\n\n";
        fs::write(&config_path, user_config).expect("seed config.toml");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let after_install = fs::read_to_string(&config_path).expect("read config");
        assert!(
            after_install.starts_with(user_config),
            "user bytes (incl. the trailing blank line) must survive install as an exact \
             prefix; got:\n{after_install:?}",
        );

        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
            "uninstall must restore the trailing blank line byte-for-byte",
        );

        // Idempotent second uninstall.
        uninstall_codex_hooks_at(&codex_home).expect("second uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
        );
    }

    /// Fresh install into an empty CODEX_HOME creates both files and reports
    /// installed with zero trust gaps.
    #[test]
    fn fresh_install_creates_files_and_trusts_all_entries() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        assert!(hooks_path_of(&codex_home).exists());
        assert!(config_path_of(&codex_home).exists());
        assert!(codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        let config = fs::read_to_string(config_path_of(&codex_home)).expect("read config");
        assert_eq!(
            config.matches(CODEX_STATE_MARKER).count(),
            CODEX_HOOK_FORWARD_MAP.len(),
            "one marked trust section per forwarded event",
        );
    }

    /// The provider timeout belongs to Codex. The installer must preserve a
    /// viable capture budget for one-second hooks and keep a valid timeout
    /// above Codex's 600-second default for ordinary events, while capping
    /// `SessionEnd` at Codex's documented three-second maximum.
    #[test]
    fn installer_emits_exact_short_and_long_capture_budgets() {
        for (timeout_secs, capture_budget_millis) in [(1, 500), (601, 600_000)] {
            let tmp = TempDir::new().expect("tmp dir");
            let codex_home = tmp.path().join(".codex");

            install_codex_hooks_at(&codex_home, BINARY, timeout_secs).expect("install Codex hooks");

            let hooks: CodexHooksFile =
                load_json_settings(&hooks_path_of(&codex_home), "Codex").expect("read hooks");
            for (event_name, verb) in CODEX_HOOK_FORWARD_MAP {
                let (expected_timeout, expected_capture_budget_millis) =
                    if *event_name == "SessionEnd" && timeout_secs > 3 {
                        (3, 2_000)
                    } else {
                        (timeout_secs, capture_budget_millis)
                    };
                let expected = codex_hook_command(BINARY, verb, expected_capture_budget_millis);
                let installed = hooks.hooks.get(*event_name).is_some_and(|groups| {
                    groups.iter().any(|group| {
                        group.matcher.is_none()
                            && group.hooks.iter().any(|hook| {
                                hook.handler_type == "command"
                                    && hook.command == expected
                                    && hook.timeout == Some(expected_timeout)
                            })
                    })
                });
                assert!(
                    installed,
                    "{event_name} must retain its provider-valid timing and emit '{expected}': \
                     {hooks:?}"
                );
            }
            assert!(
                codex_hooks_are_installed_at(&codex_home, BINARY).expect("installed status"),
                "all exact emitted commands must be trusted"
            );
        }
    }

    /// Positional-key recomputation: when the on-disk group order changes,
    /// reinstall rewrites our state key to the new index and drops the stale
    /// marked key pointing at the old index.
    #[test]
    fn reinstall_recomputes_positional_state_keys() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);

        fs::create_dir_all(&codex_home).expect("create codex home");
        let seeded = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"matcher": "user", "hooks": [{"type": "command", "command": "echo user"}]}
                ]
            }
        });
        fs::write(&hooks_path, serde_json::to_string(&seeded).expect("render"))
            .expect("seed hooks.json");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let old_key = format!("{}:session_start:1:0", hooks_path.display());
        assert!(
            fs::read_to_string(&config_path)
                .expect("read")
                .contains(&old_key)
        );

        // The user deletes their group: our group is now index 0.
        let mut file: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let groups = file.hooks.get_mut("SessionStart").expect("SessionStart");
        groups.remove(0);
        write_json_settings(&hooks_path, &file, "Codex").expect("rewrite");

        assert_eq!(
            codex_hook_trust_gaps_at(&codex_home).expect("gaps"),
            1,
            "the shifted handler is untrusted until reinstall",
        );

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("re-install");
        let config = fs::read_to_string(&config_path).expect("read");
        let new_key = format!("{}:session_start:0:0", hooks_path.display());
        assert!(
            config.contains(&new_key),
            "recomputed key missing:\n{config}"
        );
        assert!(
            !config.contains(&old_key),
            "stale positional key must be removed:\n{config}"
        );
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);
    }

    /// Trust-gap counting: missing config, tampered hash, and explicit
    /// disable are classified per the documented semantics.
    #[test]
    fn trust_gaps_count_missing_and_stale_entries() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let config_path = config_path_of(&codex_home);

        // All trusted after install.
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Tamper one hash: exactly one gap.
        let config = fs::read_to_string(&config_path).expect("read");
        let stop_hash = codex_trusted_hash(
            "stop",
            None,
            &codex_hook_command(BINARY, "stop", 29_000),
            30,
            Some(LIBRA_CODEX_STATUS_MESSAGE),
        );
        let tampered = config.replace(&stop_hash, "sha256:0000");
        assert_ne!(tampered, config, "stop hash must be present to tamper");
        fs::write(&config_path, &tampered).expect("tamper");
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 1);

        // `enabled = false` with a matching hash is a deliberate disable,
        // not a trust gap.
        let disabled = config.replacen("enabled = true", "enabled = false", 1);
        fs::write(&config_path, &disabled).expect("disable");
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Missing config.toml: every managed handler is a gap.
        fs::remove_file(&config_path).expect("remove config");
        assert_eq!(
            codex_hook_trust_gaps_at(&codex_home).expect("gaps"),
            CODEX_HOOK_FORWARD_MAP.len(),
        );
        assert!(!codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));

        // No hooks.json at all: nothing to gap.
        fs::remove_file(hooks_path_of(&codex_home)).expect("remove hooks");
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);
    }

    /// An invalid config.toml is a hard, actionable error — never silently
    /// overwritten.
    #[test]
    fn install_rejects_invalid_config_toml() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let user_hooks = b"{\n  \"hooks\": {\n    \"SessionStart\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo user\"}]}]\n  }\n}\n";
        fs::write(&hooks_path, user_hooks).expect("seed user hooks.json");
        fs::write(&config_path, "this is [not toml").expect("seed broken config");

        let err = install_codex_hooks_at(&codex_home, BINARY, 30).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("invalid Codex config TOML"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains(&config_path.display().to_string()),
            "error must name the file; got: {rendered}",
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read back"),
            "this is [not toml",
            "the broken file must be left untouched",
        );
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks back"),
            user_hooks,
            "install must validate config.toml before it publishes hooks.json",
        );
    }

    /// A deterministic failure immediately before the second publication must
    /// roll back the first file byte-for-byte. This uses a thread-local test
    /// seam rather than a production environment variable so the externally
    /// visible command surface has no fault-injection switch.
    #[test]
    fn uninstall_rolls_back_hooks_when_config_publication_fails() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let hooks_before = fs::read(&hooks_path).expect("snapshot installed hooks");
        let config_before = fs::read(&config_path).expect("snapshot installed config");

        let _hook = CodexSettingsTransactionTestHookGuard::install(|phase| {
            if phase == CodexSettingsTransactionTestPhase::BeforeConfigPublish {
                return Err(anyhow::anyhow!("injected config publish failure"));
            }
            Ok(())
        });
        let error = uninstall_codex_hooks_at(&codex_home)
            .expect_err("second publication failure must make uninstall fail");
        drop(_hook);

        assert!(
            error.to_string().contains("rolled back"),
            "the caller must learn that no half-uninstall remains: {error:#}",
        );
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks after rollback"),
            hooks_before,
            "hooks.json must be restored after config publication fails",
        );
        assert_eq!(
            fs::read(&config_path).expect("read config after rollback"),
            config_before,
            "config.toml must remain at its original bytes after injected failure",
        );
        assert!(
            !journal_path.exists(),
            "a successful rollback must consume its recovery journal",
        );
    }

    /// An uncooperative external editor can change config.toml while Libra is
    /// between the two target files. The second compare must refuse to
    /// overwrite the user bytes and still roll back the already-published
    /// hooks.json; the unresolved journal remains as deliberate recovery
    /// evidence rather than silently guessing which config contents to keep.
    #[test]
    fn install_refuses_external_config_race_and_rolls_back_hooks() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        fs::create_dir_all(&codex_home).expect("create codex home");
        let user_hooks = b"{\n  \"hooks\": {\n    \"SessionStart\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo user\"}]}]\n  }\n}\n";
        let user_config = b"model = \"initial\"\n";
        let externally_edited_config = b"model = \"saved-by-user\"\n";
        fs::write(&hooks_path, user_hooks).expect("seed hooks.json");
        fs::write(&config_path, user_config).expect("seed config.toml");

        let config_for_hook = config_path.clone();
        let externally_edited_config_for_hook = externally_edited_config.to_vec();
        let _hook = CodexSettingsTransactionTestHookGuard::install(move |phase| {
            if phase == CodexSettingsTransactionTestPhase::BeforeConfigPublish {
                fs::write(&config_for_hook, &externally_edited_config_for_hook)
                    .context("inject external Codex config edit")?;
            }
            Ok(())
        });
        let error = install_codex_hooks_at(&codex_home, BINARY, 30)
            .expect_err("concurrent config edit must prevent blind replacement");
        drop(_hook);

        assert!(
            error.to_string().contains("changed concurrently"),
            "the error must name the CAS conflict: {error:#}",
        );
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks after conflict"),
            user_hooks,
            "the first file must be rolled back after config conflict",
        );
        assert_eq!(
            fs::read(&config_path).expect("read external config"),
            externally_edited_config,
            "the external config bytes must never be overwritten",
        );
        assert!(
            journal_path.exists(),
            "an externally ambiguous config result must retain recovery evidence",
        );
        let journal = read_codex_settings_transaction_journal(&journal_path)
            .expect("read sticky conflict journal");
        assert!(
            journal.manual_recovery,
            "an observed external edit must persist a sticky no-auto-write fence",
        );
        let hooks_before_recovery = fs::read(&hooks_path).expect("snapshot hooks");
        let config_before_recovery = fs::read(&config_path).expect("snapshot config");
        let recovery_error = recover_codex_settings_transaction(&codex_home)
            .expect_err("an externally edited config must remain an actionable recovery conflict");
        assert!(
            recovery_error.to_string().contains("recovery journal"),
            "the next operation must identify the retained journal: {recovery_error:#}",
        );
        assert!(
            journal_path.exists(),
            "an unresolved external edit must not silently discard recovery evidence",
        );
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks after rejected recovery"),
            hooks_before_recovery,
            "sticky recovery must not rewrite hooks.json",
        );
        assert_eq!(
            fs::read(&config_path).expect("read config after rejected recovery"),
            config_before_recovery,
            "sticky recovery must not rewrite config.toml",
        );
    }

    /// Even if persisting the sticky manual fence itself fails, the next
    /// invocation must not mistake the old `manual_recovery = false` journal
    /// for replayable intent. The external config bytes are outside its v2
    /// fingerprint pair, which is an independent zero-write fence.
    #[test]
    fn failed_manual_fence_persistence_still_makes_next_recovery_zero_write() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        let user_hooks = b"{\n  \"hooks\": {}\n}\n";
        let user_config = b"model = \"initial\"\n";
        let externally_edited_config = b"model = \"external\"\n";
        fs::write(&hooks_path, user_hooks).expect("seed hooks");
        fs::write(&config_path, user_config).expect("seed config");

        let config_for_phase = config_path.clone();
        let external_for_phase = externally_edited_config.to_vec();
        let _phase_hook = CodexSettingsTransactionTestHookGuard::install(move |phase| {
            if phase == CodexSettingsTransactionTestPhase::BeforeConfigPublish {
                fs::write(&config_for_phase, &external_for_phase)
                    .context("inject external config edit")?;
            }
            Ok(())
        });
        let _journal_hook = CodexSettingsJournalTestHookGuard::install(|journal| {
            if journal.manual_recovery {
                bail!("injected durable manual-fence write failure");
            }
            Ok(())
        });
        let error = install_codex_hooks_at(&codex_home, BINARY, 30)
            .expect_err("observed conflict must not succeed when fence persistence fails");
        assert!(
            error.to_string().contains("could not be persisted"),
            "error must disclose the failed fence: {error:#}",
        );
        drop(_journal_hook);
        drop(_phase_hook);

        let journal = read_codex_settings_transaction_journal(&journal_path)
            .expect("the original journal must remain after injected marker failure");
        assert!(
            !journal.manual_recovery,
            "test seam must prove recovery cannot rely solely on the marker",
        );
        let hooks_before_recovery = fs::read(&hooks_path).expect("snapshot hooks");
        let config_before_recovery = fs::read(&config_path).expect("snapshot config");
        let recovery_error = recover_codex_settings_transaction(&codex_home)
            .expect_err("fingerprint-unknown recovery must refuse all target writes");
        assert!(
            recovery_error.to_string().contains("fingerprint"),
            "recovery must identify the independent fingerprint fence: {recovery_error:#}",
        );
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks after recovery"),
            hooks_before_recovery,
            "failed manual marker must not permit a later hooks rewrite",
        );
        assert_eq!(
            fs::read(&config_path).expect("read config after recovery"),
            config_before_recovery,
            "failed manual marker must not permit a later config rewrite",
        );
    }

    /// A process can terminate after marker-owned hooks become visible but
    /// before the paired trust rewrite is attempted. The v2 journal contains
    /// only exact fingerprints, so recovery re-derives the config candidate
    /// from the current hooks and publishes it only when it hashes to the
    /// recorded post-image.
    #[test]
    fn fresh_install_crash_between_files_recovers_fingerprinted_config() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let hooks_after =
            read_codex_settings_snapshot(&hooks_path, "hooks.json").expect("read installed hooks");
        let config_after = read_codex_settings_snapshot(&config_path, "config.toml")
            .expect("read installed config");
        write_codex_settings_snapshot(
            &config_path,
            &CodexSettingsSnapshot::absent(),
            "config.toml",
        )
        .expect("simulate crash before config publish");
        let transaction = CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before: CodexSettingsSnapshot::absent(),
            hooks_after: hooks_after.clone(),
            config_before: CodexSettingsSnapshot::absent(),
            config_after: config_after.clone(),
            operation: CodexSettingsTransactionOperation::InstallOrRefresh,
        };
        let mut journal = transaction.journal();
        journal.phase = CodexSettingsTransactionPhase::HooksPublished;
        write_codex_settings_transaction_journal(&journal_path, &journal)
            .expect("persist interrupted v2 transaction journal");

        recover_codex_settings_transaction(&codex_home)
            .expect("next operation must forward the exact known partial state");
        assert!(
            read_codex_settings_snapshot(&hooks_path, "hooks.json").expect("read hooks")
                == hooks_after,
            "recovery must preserve the already-published marker-owned hooks",
        );
        assert!(
            read_codex_settings_snapshot(&config_path, "config.toml").expect("read config")
                == config_after,
            "recovery must recreate exactly the fingerprinted trust post-image",
        );
        assert!(
            !journal_path.exists(),
            "successful recovery must consume the v2 journal",
        );
    }

    /// Uninstall has the same crash window in the opposite direction: marked
    /// handlers may already be gone while their trust entries still remain.
    /// Recovery must reconstruct the marker-only cleanup from the current
    /// hooks and prove the resulting config bytes match the v2 post-image;
    /// unrelated user state must survive unchanged.
    #[test]
    fn uninstall_crash_between_files_recovers_fingerprinted_trust_cleanup() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        let user_state = "[hooks.state.\"/foreign/hooks.json:stop:0:0\"]\nenabled = true\ntrusted_hash = \"sha256:user\"\n";
        fs::write(&config_path, format!("model = \"user\"\n\n{user_state}"))
            .expect("seed user config");
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let hooks_before =
            read_codex_settings_snapshot(&hooks_path, "hooks.json").expect("read installed hooks");
        let config_before = read_codex_settings_snapshot(&config_path, "config.toml")
            .expect("read installed config");

        // Use the ordinary uninstall once to obtain its exact intended
        // post-images, then restore the pre-images and simulate a process
        // death after only hooks.json became visible.
        uninstall_codex_hooks_at(&codex_home).expect("derive normal uninstall post-images");
        let hooks_after =
            read_codex_settings_snapshot(&hooks_path, "hooks.json").expect("read cleaned hooks");
        let config_after =
            read_codex_settings_snapshot(&config_path, "config.toml").expect("read cleaned config");
        write_codex_settings_snapshot(&hooks_path, &hooks_before, "hooks.json")
            .expect("restore installed hooks");
        write_codex_settings_snapshot(&config_path, &config_before, "config.toml")
            .expect("restore installed config");
        write_codex_settings_snapshot(&hooks_path, &hooks_after, "hooks.json")
            .expect("simulate first uninstall publication");

        let transaction = CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before,
            hooks_after: hooks_after.clone(),
            config_before,
            config_after: config_after.clone(),
            operation: CodexSettingsTransactionOperation::Uninstall,
        };
        let mut journal = transaction.journal();
        journal.phase = CodexSettingsTransactionPhase::HooksPublished;
        write_codex_settings_transaction_journal(&journal_path, &journal)
            .expect("persist interrupted uninstall journal");

        recover_codex_settings_transaction(&codex_home)
            .expect("recovery must finish the exact known uninstall state");
        assert!(
            read_codex_settings_snapshot(&hooks_path, "hooks.json").expect("read hooks")
                == hooks_after,
            "recovery must not restore deleted marker-owned handlers",
        );
        assert!(
            read_codex_settings_snapshot(&config_path, "config.toml").expect("read config")
                == config_after,
            "recovery must remove only the fingerprinted marker-owned trust entries",
        );
        let recovered_config = fs::read_to_string(&config_path).expect("read recovered config");
        assert!(
            recovered_config.contains("model = \"user\"") && recovered_config.contains(user_state),
            "unrelated user config must survive recovery: {recovered_config}",
        );
        assert!(
            !journal_path.exists(),
            "successful uninstall recovery must consume its journal",
        );
    }

    /// A crash may happen after config.toml becomes visible but before the
    /// journal is removed. `PublishingConfig` plus both post-image digests is
    /// completion evidence, so recovery only removes the journal and leaves
    /// both target files byte-stable.
    #[test]
    fn publishing_config_journal_with_visible_targets_is_consumed_without_rewrite() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        let hooks_after =
            read_codex_settings_snapshot(&hooks_path, "hooks.json").expect("read installed hooks");
        let config_after = read_codex_settings_snapshot(&config_path, "config.toml")
            .expect("read installed config");
        let transaction = CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before: CodexSettingsSnapshot::absent(),
            hooks_after: hooks_after.clone(),
            config_before: CodexSettingsSnapshot::absent(),
            config_after: config_after.clone(),
            operation: CodexSettingsTransactionOperation::InstallOrRefresh,
        };
        let mut journal = transaction.journal();
        journal.phase = CodexSettingsTransactionPhase::PublishingConfig;
        write_codex_settings_transaction_journal(&journal_path, &journal)
            .expect("persist completed-but-unremoved v2 journal");

        let hooks_before_recovery = fs::read(&hooks_path).expect("snapshot hooks");
        let config_before_recovery = fs::read(&config_path).expect("snapshot config");
        recover_codex_settings_transaction(&codex_home)
            .expect("visible post-images prove completion");
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks after recovery"),
            hooks_before_recovery,
            "completion recovery must not rewrite hooks.json",
        );
        assert_eq!(
            fs::read(&config_path).expect("read config after recovery"),
            config_before_recovery,
            "completion recovery must not rewrite config.toml",
        );
        assert!(!journal_path.exists(), "completion journal must be removed");
    }

    /// Fingerprint journals are safe to keep on every supported platform:
    /// they must not serialize raw config bytes, an absolute hook path, or a
    /// sentinel that could be a credential.
    #[test]
    fn codex_settings_transaction_journal_contains_only_integrity_fingerprints() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let secret = "super-secret-config-token";
        let absolute_path = "/very/private/profile/libra-custom";
        let transaction = CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before: CodexSettingsSnapshot::present(
                format!("{{\"command\":\"{absolute_path}\"}}\n").into_bytes(),
            ),
            hooks_after: CodexSettingsSnapshot::present(
                format!("{{\"command\":\"{absolute_path} newer\"}}\n").into_bytes(),
            ),
            config_before: CodexSettingsSnapshot::present(
                format!("token = \"{secret}\"\n").into_bytes(),
            ),
            config_after: CodexSettingsSnapshot::present(
                format!("token = \"{secret}-rotated\"\n").into_bytes(),
            ),
            operation: CodexSettingsTransactionOperation::InstallOrRefresh,
        };

        write_codex_settings_transaction_journal(&journal_path, &transaction.journal())
            .expect("write non-plaintext journal");
        let journal_text = fs::read_to_string(&journal_path).expect("read journal");
        assert!(
            journal_text.contains("\"version\":2"),
            "journal: {journal_text}"
        );
        assert!(
            journal_text.contains("\"digest\":"),
            "journal: {journal_text}"
        );
        assert!(
            !journal_text.contains(secret)
                && !journal_text.contains(absolute_path)
                && !journal_text.contains(&hooks_path.display().to_string())
                && !journal_text.contains(&config_path.display().to_string()),
            "journal must not contain raw user settings or target paths: {journal_text}",
        );
    }

    /// Malformed, old-schema, and unknown-operation records are never treated
    /// as replayable intent. Recovery must leave both target files untouched
    /// until a user resolves the journal manually.
    #[test]
    fn malformed_or_legacy_journal_is_zero_write_and_actionable() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        let hooks = b"{\"hooks\":{}}\n";
        let config = b"model = \"user\"\n";
        fs::write(&hooks_path, hooks).expect("seed hooks");
        fs::write(&config_path, config).expect("seed config");
        let fingerprint = serde_json::json!({"present": true, "digest": "00"});
        let old_v1 = serde_json::json!({
            "version": 1,
            "phase": "prepared",
            "operation": "install_or_refresh",
            "hooks_before": fingerprint,
            "hooks_after": {"present": true, "digest": "00"},
            "config_before": {"present": true, "digest": "00"},
            "config_after": {"present": true, "digest": "00"},
            "manual_recovery": false,
        });
        let unknown_operation = serde_json::json!({
            "version": 2,
            "phase": "prepared",
            "operation": "unknown_operation",
            "hooks_before": {"present": true, "digest": "00"},
            "hooks_after": {"present": true, "digest": "00"},
            "config_before": {"present": true, "digest": "00"},
            "config_after": {"present": true, "digest": "00"},
            "manual_recovery": false,
        });
        let missing_fingerprint = serde_json::json!({
            "version": 2,
            "phase": "prepared",
            "operation": "install_or_refresh",
            "hooks_before": {"present": true, "digest": "00"},
            "hooks_after": {"present": true, "digest": "00"},
            "config_before": {"present": true, "digest": "00"},
            "manual_recovery": false,
        });
        let invalid_digest = serde_json::json!({
            "version": 2,
            "phase": "prepared",
            "operation": "install_or_refresh",
            "hooks_before": {"present": true, "digest": "not-a-sha256"},
            "hooks_after": {"present": true, "digest": "not-a-sha256"},
            "config_before": {"present": true, "digest": "not-a-sha256"},
            "config_after": {"present": true, "digest": "not-a-sha256"},
            "manual_recovery": false,
        });

        for record in [
            b"{not json".to_vec(),
            serde_json::to_vec(&old_v1).expect("render v1 journal"),
            serde_json::to_vec(&unknown_operation).expect("render unknown operation"),
            serde_json::to_vec(&missing_fingerprint).expect("render missing fingerprint"),
            serde_json::to_vec(&invalid_digest).expect("render invalid digest"),
        ] {
            fs::write(&journal_path, record).expect("seed invalid journal");
            let error = recover_codex_settings_transaction(&codex_home)
                .expect_err("invalid journal must refuse recovery");
            assert!(
                error.to_string().contains("recovery journal"),
                "error must name the repair artifact: {error:#}",
            );
            assert_eq!(fs::read(&hooks_path).expect("read hooks"), hooks);
            assert_eq!(fs::read(&config_path).expect("read config"), config);
            assert!(
                journal_path.exists(),
                "invalid journal must remain actionable"
            );
        }
    }

    /// The only resumable order is hooks-after/config-before. A known but
    /// reversed pair could be a user edit or a damaged journal, so recovery
    /// must mark it manual without trying to reconstruct either file.
    #[test]
    fn mixed_fingerprint_pair_is_zero_write_and_becomes_manual() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);
        let journal_path = codex_home.join(CODEX_SETTINGS_TRANSACTION_JOURNAL_FILE);
        let hooks_before = CodexSettingsSnapshot::present(b"{\"hooks\":{}}\n".to_vec());
        let hooks_after =
            CodexSettingsSnapshot::present(b"{\"hooks\":{\"SessionStart\":[]}}\n".to_vec());
        let config_before = CodexSettingsSnapshot::present(b"model = \"before\"\n".to_vec());
        let config_after = CodexSettingsSnapshot::present(b"model = \"after\"\n".to_vec());
        write_codex_settings_snapshot(&hooks_path, &hooks_before, "hooks.json")
            .expect("seed reversed hooks");
        write_codex_settings_snapshot(&config_path, &config_after, "config.toml")
            .expect("seed reversed config");
        let transaction = CodexSettingsTransaction {
            hooks_path: hooks_path.clone(),
            config_path: config_path.clone(),
            hooks_before,
            hooks_after,
            config_before,
            config_after,
            operation: CodexSettingsTransactionOperation::InstallOrRefresh,
        };
        let mut journal = transaction.journal();
        journal.phase = CodexSettingsTransactionPhase::PublishingConfig;
        write_codex_settings_transaction_journal(&journal_path, &journal)
            .expect("seed v2 mixed journal");

        let hooks_before_recovery = fs::read(&hooks_path).expect("snapshot hooks");
        let config_before_recovery = fs::read(&config_path).expect("snapshot config");
        let error = recover_codex_settings_transaction(&codex_home)
            .expect_err("mixed pair must not be replayed");
        assert!(
            error.to_string().contains("phase") || error.to_string().contains("fingerprint"),
            "mixed pair error must be actionable: {error:#}",
        );
        assert_eq!(
            fs::read(&hooks_path).expect("read hooks after rejection"),
            hooks_before_recovery,
        );
        assert_eq!(
            fs::read(&config_path).expect("read config after rejection"),
            config_before_recovery,
        );
        assert!(
            read_codex_settings_transaction_journal(&journal_path)
                .expect("read manual journal")
                .manual_recovery,
            "mixed state must become sticky manual recovery",
        );
    }

    /// The section editor only touches Libra's marked sections and exact
    /// keys; a user's own hooks.state section for the *same* hooks.json (from
    /// their own manual entry at a non-Libra position) survives.
    #[test]
    fn state_section_editor_leaves_foreign_sections_alone() {
        let hooks_json = "/home/u/.codex/hooks.json";
        let user_key = format!("{hooks_json}:stop:0:0");
        let content = format!(
            "[hooks.state.\"{user_key}\"]\nenabled = true\ntrusted_hash = \"sha256:user\"\n"
        );
        let desired = [CodexStateEntry {
            key: format!("{hooks_json}:stop:1:0"),
            trusted_hash: "sha256:ours".to_string(),
        }];
        let remove_exact: BTreeSet<String> =
            desired.iter().map(|entry| entry.key.clone()).collect();

        let rewritten = rewrite_codex_state_sections(&content, hooks_json, &remove_exact, &desired);
        assert!(
            rewritten.starts_with(&content),
            "unmarked user section (same hooks.json, different position) must survive:\n{rewritten}",
        );
        assert!(rewritten.contains("sha256:ours"));

        // Removing our marked section again restores the user bytes exactly.
        let cleaned = rewrite_codex_state_sections(&rewritten, hooks_json, &BTreeSet::new(), &[]);
        assert_eq!(cleaned, content);
    }

    /// Keys containing TOML-special characters (quotes, backslashes) escape
    /// and re-parse losslessly.
    #[test]
    fn state_key_escaping_round_trips() {
        let key = r#"C:\Users\dev\.codex\hooks.json:stop:0:0"#;
        let header = format!("[hooks.state.\"{}\"]", escape_toml_basic_string(key));
        assert_eq!(parse_state_section_key(&header).as_deref(), Some(key));

        let quoted = "with\"quote and\ttab";
        let header = format!("[hooks.state.\"{}\"]", escape_toml_basic_string(quoted));
        assert_eq!(parse_state_section_key(&header).as_deref(), Some(quoted));
    }

    /// Install rejects a zero timeout with an actionable message.
    #[test]
    fn install_rejects_zero_timeout() {
        let options = ProviderInstallOptions {
            binary_path: None,
            timeout_secs: Some(0),
        };
        let err = install_codex_hooks(&options).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid hook timeout"),
            "got: {err:#}",
        );
    }
}
