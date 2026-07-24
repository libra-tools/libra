//! `libra rerere` — REuse REcorded REsolution. Records how a merge conflict was
//! resolved and replays that resolution when the identical conflict reappears.
//!
//! Storage split (W2 §C.4.3):
//! - the REUSABLE resolution cache stays repository-scoped under
//!   `.libra/rerere/<id>/{preimage,postimage}` — any worktree may replay a
//!   resolution any other worktree recorded;
//! - `MERGE_RR` (`id<TAB>path` lines for the conflicts CURRENTLY tracked) is a
//!   per-worktree fact and lives in the worktree's LOCAL gitdir
//!   (`<local_gitdir>/MERGE_RR`) — one worktree's clear/auto-update can never
//!   drop, stage, or record another worktree's current conflicts. A legacy
//!   common `.libra/rerere/MERGE_RR` follows the ambiguous-sidecar rules: a
//!   linked scope NEVER reads it; main reads (and migrates on first write)
//!   only while no linked worktree exists; with linked evidence it is left
//!   untouched for the worktree doctor (W3) and a one-line notice is printed.
//!
//! `<id>` is the SHA-256 of the conflicted file's bytes. This version matches a
//! conflict only when the whole conflicted file is byte-identical to a recorded
//! preimage (Git's per-hunk normalisation / ours-theirs-swap independence remain
//! a documented follow-up).
//!
//! When `rerere.enabled` is set, [`auto_update`] is invoked automatically by the
//! merge / rebase / cherry-pick sequencers (at both conflict and resolution
//! time) so preimages are recorded, known resolutions replayed, and postimages
//! recorded without a manual `libra rerere`. With `rerere.enabled` unset
//! (the default) those hooks are complete no-ops.

use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use git_internal::internal::index::Index;
use sha2::{Digest, Sha256};

use crate::{
    internal::{config::ConfigKv, worktree_scope::WorktreeScope},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        path, util,
    },
};

const CONFLICT_START: &str = "<<<<<<<";
const CONFLICT_SEP: &str = "=======";
const CONFLICT_END: &str = ">>>>>>>";

pub const RERERE_EXAMPLES: &str = "\
EXAMPLES:
    libra rerere                  Record preimages / replay resolutions for current conflicts
    libra rerere status           List the conflicts being tracked
    libra rerere diff             Show what changed since each preimage was recorded
    libra rerere forget <path>    Drop the recorded resolution for a path
    libra rerere clear            Stop tracking the current conflicts
    libra rerere gc               Prune old recorded resolutions";

/// Reuse recorded conflict resolutions.
#[derive(Parser, Debug)]
#[command(after_help = RERERE_EXAMPLES)]
pub struct RerereArgs {
    #[command(subcommand)]
    pub command: Option<RerereSubcommand>,
}

#[derive(Subcommand, Debug)]
pub enum RerereSubcommand {
    /// List the paths whose conflicts are currently being tracked.
    Status,
    /// Show the diff between each recorded preimage and the current file.
    Diff,
    /// Drop the recorded resolution(s) for the given paths.
    Forget {
        #[clap(value_name = "PATHSPEC", required = true)]
        paths: Vec<String>,
    },
    /// Stop tracking the current conflicts (keeps recorded resolutions).
    Clear,
    /// Prune recorded resolutions older than the configured thresholds.
    Gc,
}

pub async fn execute(args: RerereArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: RerereArgs, _output: &OutputConfig) -> CliResult<()> {
    let rr_dir = rerere_dir()?;
    // W2 §C.4.3: MERGE_RR is per-worktree — resolve the scope ONCE for this
    // request and pass it down (the shared cache under `rr_dir` stays
    // repository-scoped).
    let scope = WorktreeScope::current();
    match args.command {
        // The bare `libra rerere` never auto-stages replayed resolutions — that
        // is what `rerere.autoUpdate` / `--rerere-autoupdate` control, and they
        // only apply to the automatic merge/rebase/cherry-pick integration.
        None => apply(&scope, &rr_dir, false).await,
        Some(RerereSubcommand::Status) => status(&scope, &rr_dir),
        Some(RerereSubcommand::Diff) => diff(&scope, &rr_dir),
        Some(RerereSubcommand::Forget { paths }) => forget(&scope, &rr_dir, &paths),
        Some(RerereSubcommand::Clear) => clear(&scope, &rr_dir),
        Some(RerereSubcommand::Gc) => gc(&rr_dir),
    }
}

/// Whether reuse-recorded-resolution is turned on for this repository
/// (`rerere.enabled`, default off). The automatic merge/rebase/cherry-pick
/// integration is a no-op unless this returns `true`, so leaving the config
/// unset keeps those commands' behaviour byte-for-byte unchanged.
pub(crate) async fn is_enabled() -> bool {
    matches!(
        ConfigKv::get("rerere.enabled")
            .await
            .ok()
            .flatten()
            .map(|entry| entry.value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("true" | "1" | "yes" | "on")
    )
}

/// Whether replayed resolutions should also be staged (`rerere.autoUpdate`,
/// default off). The per-command `--rerere-autoupdate` flag ORs with this.
async fn autoupdate_configured() -> bool {
    matches!(
        ConfigKv::get("rerere.autoUpdate")
            .await
            .ok()
            .flatten()
            .map(|entry| entry.value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("true" | "1" | "yes" | "on")
    )
}

/// Automatic hook for the merge/rebase/cherry-pick sequencers.
///
/// A no-op unless `rerere.enabled` is set. When enabled it runs the same
/// record/replay pass as `libra rerere`, so calling it at both the moment a
/// conflict is written and the moment it is resolved (or `--continue`d)
/// records preimages, replays known resolutions, and records postimages —
/// whichever applies to the current working-tree state. `auto_update` (the
/// command's `--rerere-autoupdate` flag) ORed with `rerere.autoUpdate` decides
/// whether a replayed file is also staged. Errors are surfaced to the caller,
/// which should treat them as non-fatal to the underlying operation.
pub(crate) async fn auto_update(auto_update: bool) -> CliResult<()> {
    if !is_enabled().await {
        return Ok(());
    }
    let rr_dir = rerere_dir()?;
    let scope = WorktreeScope::current();
    let stage_replayed = auto_update || autoupdate_configured().await;
    apply(&scope, &rr_dir, stage_replayed).await
}

/// The default action: for every tracked file that currently contains conflict
/// markers, record its preimage (or replay a known resolution); for every
/// tracked conflict that has since been resolved, record its postimage. When
/// `stage_replayed` is set, a file resolved by replay is also staged.
async fn apply(scope: &WorktreeScope, rr_dir: &Path, stage_replayed: bool) -> CliResult<()> {
    let workdir = util::working_dir();
    let index = load_index()?;
    let mut merge_rr = read_merge_rr(scope, rr_dir)?;

    // 1. Record postimages for previously-tracked conflicts that are now resolved.
    let mut resolved_paths = Vec::new();
    for (path, id) in &merge_rr {
        let content = read_or_empty(&workdir.join(path))?;
        // An empty read means the file is gone or genuinely empty; either way it
        // is no longer a conflict, but we only record a non-empty resolution.
        if !content.is_empty() && !is_conflicted(&content) {
            write_entry(rr_dir, id, "postimage", &content)?;
            println!("Recorded resolution for '{path}'.");
            resolved_paths.push(path.clone());
        }
    }
    merge_rr.retain(|(path, _)| !resolved_paths.contains(path));

    // 2. Visit each tracked file that currently has conflict markers. A conflict
    // lives at index stages 1-3 (there is no stage-0 entry for it), so gather
    // distinct paths across every stage — iterating only stage 0 (as
    // `tracked_files()` does) would miss exactly the conflicted files that the
    // merge/rebase/cherry-pick sequencers leave behind.
    let mut seen_paths = std::collections::HashSet::new();
    let mut candidates: Vec<String> = Vec::new();
    for stage in 0..=3 {
        for entry in index.tracked_entries(stage) {
            if seen_paths.insert(entry.name.clone()) {
                candidates.push(entry.name.clone());
            }
        }
    }
    for path in &candidates {
        let path = path.as_str();
        let absolute = workdir.join(path);
        let Ok(content) = fs::read(&absolute) else {
            continue;
        };
        if !is_conflicted(&content) {
            continue;
        }
        let id = conflict_id(&content);
        let postimage = entry_path(rr_dir, &id, "postimage");
        // Replay only when BOTH the recorded preimage and postimage exist — a
        // defensive guard so a stray postimage can never overwrite a file.
        if postimage.exists() && entry_path(rr_dir, &id, "preimage").exists() {
            let resolution = fs::read(&postimage).map_err(read_err)?;
            fs::write(&absolute, &resolution).map_err(write_err)?;
            println!("Resolved '{path}' using a previously recorded resolution.");
            if stage_replayed {
                stage_path(path).await?;
            }
        } else {
            write_entry(rr_dir, &id, "preimage", &content)?;
            if !merge_rr.iter().any(|(p, _)| p == path) {
                merge_rr.push((path.to_string(), id));
            }
            println!("Recorded preimage for '{path}'.");
        }
    }

    // Only persist MERGE_RR when there is something to track, or a file already
    // exists that may need updating/clearing. This keeps an ordinary commit in a
    // `rerere.enabled` repo — where `auto_update` runs after every commit — from
    // creating a spurious empty MERGE_RR, so it stays a true no-op.
    if !merge_rr.is_empty() || merge_rr_file(scope)?.exists() || legacy_is_readable(scope, rr_dir) {
        write_merge_rr(scope, rr_dir, &merge_rr)?;
    }
    Ok(())
}

/// Stage a single resolved path (used when `--rerere-autoupdate` /
/// `rerere.autoUpdate` is in effect): stage the resolved content at stage 0 via
/// the normal `add` path, then drop any leftover conflict stages 1-3 so the
/// index reports the path fully resolved (`ls-files -u` empty). `add` alone
/// writes stage 0 but does not clear the unmerged stages a sequencer left.
async fn stage_path(path: &str) -> CliResult<()> {
    let args = crate::command::add::AddArgs {
        pathspec: vec![path.to_string()],
        all: false,
        update: false,
        refresh: false,
        verbose: false,
        force: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    };
    crate::command::add::run_add(&args).await?;

    let index_path = path::index();
    let mut index = Index::load(&index_path).map_err(|error| {
        CliError::fatal(format!("failed to load index: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    // Remove ALL three conflict stages (do not short-circuit), noting whether
    // any were present so we only rewrite the index when something changed.
    let mut cleared = false;
    for stage in [1u8, 2, 3] {
        if index.remove(path, stage).is_some() {
            cleared = true;
        }
    }
    if cleared {
        index.save(&index_path).map_err(|error| {
            CliError::fatal(format!("failed to save index: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
    }
    Ok(())
}

fn status(scope: &WorktreeScope, rr_dir: &Path) -> CliResult<()> {
    for (path, _) in read_merge_rr(scope, rr_dir)? {
        println!("{path}");
    }
    Ok(())
}

fn diff(scope: &WorktreeScope, rr_dir: &Path) -> CliResult<()> {
    let workdir = util::working_dir();
    for (path, id) in read_merge_rr(scope, rr_dir)? {
        let Ok(preimage) = fs::read_to_string(entry_path(rr_dir, &id, "preimage")) else {
            continue;
        };
        let current_bytes = read_or_empty(&workdir.join(&path))?;
        let current = String::from_utf8_lossy(&current_bytes);
        let patch = diffy::create_patch(&preimage, &current);
        println!("* {path}");
        print!("{patch}");
    }
    Ok(())
}

fn forget(scope: &WorktreeScope, rr_dir: &Path, paths: &[String]) -> CliResult<()> {
    let mut removed = false;
    let mut kept = Vec::new();
    for (path, id) in read_merge_rr(scope, rr_dir)? {
        if paths.iter().any(|p| p == &path) {
            remove_dir_all_ok(&rr_dir.join(&id))?;
            removed = true;
        } else {
            kept.push((path, id));
        }
    }
    write_merge_rr(scope, rr_dir, &kept)?;
    if !removed {
        return Err(CliError::command_usage(format!(
            "no recorded resolution for: {}",
            paths.join(", ")
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    Ok(())
}

fn clear(scope: &WorktreeScope, rr_dir: &Path) -> CliResult<()> {
    let merge_rr = merge_rr_file(scope)?;
    if merge_rr.exists() {
        fs::remove_file(&merge_rr).map_err(write_err)?;
    }
    // A single-worktree main also clears the legacy common file it may still
    // be reading from (re-checked under the registry lock — a concurrent
    // `worktree add` makes it ambiguous and it must then stay). A linked
    // scope never touches it (rule 2); main surfaces the ambiguity notice
    // when the file is left behind (rule 3).
    let legacy = legacy_merge_rr_file(rr_dir);
    if !scope.is_linked()
        && legacy.exists()
        && matches!(
            remove_legacy_if_unambiguous(rr_dir)?,
            LegacyRemoval::Ambiguous
        )
    {
        print_ambiguous_legacy_notice(&legacy);
    }
    Ok(())
}

/// Prune cache entries: a resolved entry (has a postimage) is kept for
/// `gc.rerereResolved` days, an unresolved one (preimage only) for
/// `gc.rerereUnresolved` days. Defaults: 60 / 15 days. Time is taken from the
/// preimage file's modification time.
fn gc(rr_dir: &Path) -> CliResult<()> {
    const RESOLVED_TTL_SECS: u64 = 60 * 24 * 60 * 60;
    const UNRESOLVED_TTL_SECS: u64 = 15 * 24 * 60 * 60;

    let now = std::time::SystemTime::now();
    let entries = match fs::read_dir(rr_dir) {
        Ok(entries) => entries,
        // No cache directory yet → nothing to prune.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(read_err(error)),
    };
    for entry in entries {
        let dir = entry.map_err(read_err)?.path();
        if !dir.is_dir() {
            continue;
        }
        let resolved = dir.join("postimage").exists();
        let ttl = if resolved {
            RESOLVED_TTL_SECS
        } else {
            UNRESOLVED_TTL_SECS
        };
        // Age the entry from the relevant file's mtime; a missing file just
        // skips it, but an unexpected stat error surfaces.
        let reference = if resolved {
            dir.join("postimage")
        } else {
            dir.join("preimage")
        };
        let mtime = match reference.metadata().and_then(|m| m.modified()) {
            Ok(mtime) => mtime,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(read_err(error)),
        };
        // A future mtime (clock skew) counts as age 0 — i.e. fresh, not pruned.
        let age = now.duration_since(mtime).map(|d| d.as_secs()).unwrap_or(0);
        if age > ttl {
            remove_dir_all_ok(&dir)?;
        }
    }
    Ok(())
}

// ── helpers ──

/// Whether `content` contains a conflict marker.
fn is_conflicted(content: &[u8]) -> bool {
    content
        .split(|&b| b == b'\n')
        .any(|line| starts_with(line, CONFLICT_START))
        && content
            .split(|&b| b == b'\n')
            .any(|line| starts_with(line, CONFLICT_SEP) || starts_with(line, CONFLICT_END))
}

fn starts_with(line: &[u8], prefix: &str) -> bool {
    line.starts_with(prefix.as_bytes())
}

/// The cache id for a conflicted file: the SHA-256 of its bytes.
fn conflict_id(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}

fn entry_path(rr_dir: &Path, id: &str, name: &str) -> PathBuf {
    rr_dir.join(id).join(name)
}

fn write_entry(rr_dir: &Path, id: &str, name: &str, content: &[u8]) -> CliResult<()> {
    let dir = rr_dir.join(id);
    fs::create_dir_all(&dir).map_err(write_err)?;
    fs::write(dir.join(name), content).map_err(write_err)
}

/// This worktree's MERGE_RR file: `<local_gitdir>/MERGE_RR` (W2 §C.4.3).
/// For the main worktree the local gitdir IS the common `.libra`, so main's
/// canonical location is `.libra/MERGE_RR`.
fn merge_rr_file(scope: &WorktreeScope) -> CliResult<PathBuf> {
    Ok(scope.local_gitdir()?.join("MERGE_RR"))
}

/// The pre-W2 repository-global location (inside the shared cache dir).
fn legacy_merge_rr_file(rr_dir: &Path) -> PathBuf {
    rr_dir.join("MERGE_RR")
}

/// Whether the worktree registry shows (or cannot rule out) linked
/// worktrees. A missing registry file is a single-worktree repository; an
/// unreadable or corrupt one counts as evidence (ambiguous → fail safe).
fn registry_has_linked_evidence() -> bool {
    let registry = util::storage_path().join("worktrees.json");
    match fs::read_to_string(&registry) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Ok(raw) => crate::command::worktree::WorktreeState::parse(raw.as_bytes())
            .map(|state| !state.is_single_main())
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// Whether the LEGACY common MERGE_RR is readable as this scope's active
/// file: only for the MAIN scope, only while no linked worktree exists (the
/// ambiguous-sidecar rules — a linked scope never reads it, and with linked
/// evidence nobody auto-consumes it).
fn legacy_is_readable(scope: &WorktreeScope, rr_dir: &Path) -> bool {
    !scope.is_linked() && legacy_merge_rr_file(rr_dir).exists() && !registry_has_linked_evidence()
}

/// Outcome of a guarded legacy-MERGE_RR deletion attempt. `Ambiguous` is the
/// only state that warrants the user-facing notice — `AlreadyGone` (another
/// process removed it first, e.g. two concurrent `clear`s) must not print a
/// false "linked worktrees exist" line.
enum LegacyRemoval {
    Removed,
    Ambiguous,
    AlreadyGone,
}

/// Delete the legacy common MERGE_RR — but only after RE-CHECKING the
/// registry UNDER the worktree registry lock, so a concurrent `worktree
/// add` cannot make the file ambiguous between the evidence probe and the
/// unlink (an ambiguous sidecar must be left for the W3 doctor).
fn remove_legacy_if_unambiguous(rr_dir: &Path) -> CliResult<LegacyRemoval> {
    let legacy = legacy_merge_rr_file(rr_dir);
    if !legacy.exists() {
        return Ok(LegacyRemoval::AlreadyGone);
    }
    let _registry_lock = crate::command::worktree::acquire_registry_lock()
        .map_err(crate::command::worktree::WorktreeError::into_cli_error)?;
    if registry_has_linked_evidence() {
        return Ok(LegacyRemoval::Ambiguous);
    }
    match fs::remove_file(&legacy) {
        Ok(()) => Ok(LegacyRemoval::Removed),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LegacyRemoval::AlreadyGone)
        }
        Err(error) => Err(write_err(error)),
    }
}

/// The one-line ambiguity notice (main scope only): the legacy file exists
/// but linked worktrees do too, so nobody auto-consumes it.
fn print_ambiguous_legacy_notice(legacy: &Path) {
    eprintln!(
        "note: ignoring legacy '{}' — linked worktrees exist, so its owner is \
         ambiguous; resolve it from the owning worktree (worktree doctor lands in W3)",
        legacy.display()
    );
}

fn read_merge_rr(scope: &WorktreeScope, rr_dir: &Path) -> CliResult<Vec<(String, String)>> {
    let canonical = merge_rr_file(scope)?;
    let source = if canonical.exists() {
        canonical
    } else {
        let legacy = legacy_merge_rr_file(rr_dir);
        if !legacy.exists() || scope.is_linked() {
            // Rule 2: a linked scope NEVER reads the common sidecar (its
            // presence cannot prove an owner, nor may it block this scope).
            return Ok(Vec::new());
        }
        if registry_has_linked_evidence() {
            // Rule 3: ambiguous — leave the file untouched for the worktree
            // doctor, surface a one-line notice, and act on an empty list.
            print_ambiguous_legacy_notice(&legacy);
            return Ok(Vec::new());
        }
        legacy
    };
    let text = match fs::read_to_string(&source) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(read_err(error)),
    };
    let mut entries = Vec::new();
    for line in text.lines() {
        if let Some((id, path)) = line.split_once('\t') {
            // Only trust a well-formed SHA-256 hex id — a corrupted or injected
            // id (e.g. `../..`) must never reach a filesystem path join.
            if is_valid_id(id) {
                entries.push((path.to_string(), id.to_string()));
            }
        }
    }
    Ok(entries)
}

/// A cache id is exactly a 64-character lowercase SHA-256 hex string (the form
/// `hex::encode` produces); anything else is rejected so a corrupted or injected
/// id can never reach a filesystem path join.
fn is_valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Remove a cache directory, treating "already gone" as success and surfacing
/// any other I/O error.
fn remove_dir_all_ok(dir: &Path) -> CliResult<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(write_err(error)),
    }
}

/// Read a possibly-absent file: missing → empty, other error → fatal.
fn read_or_empty(path: &Path) -> CliResult<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(read_err(error)),
    }
}

fn write_merge_rr(
    scope: &WorktreeScope,
    rr_dir: &Path,
    entries: &[(String, String)],
) -> CliResult<()> {
    let canonical = merge_rr_file(scope)?;
    let body: String = entries
        .iter()
        .map(|(path, id)| format!("{id}\t{path}\n"))
        .collect();
    // Atomic + fsynced: the migrate-on-first-write below deletes the ONLY
    // other copy, so the canonical file must be durably on disk first (a
    // crash between a plain write and the unlink could lose the list).
    crate::utils::atomic_write::write_atomic(&canonical, body.as_bytes(), true)
        .map_err(write_err)?;
    // Migrate-on-first-write: once the canonical file is durably written, a
    // single-worktree main removes the legacy common file it was reading —
    // re-checked under the registry lock so a concurrent `worktree add`
    // cannot make it ambiguous between probe and unlink. With linked
    // evidence the legacy file is never touched (rule 3 — doctor territory).
    if legacy_is_readable(scope, rr_dir) {
        remove_legacy_if_unambiguous(rr_dir)?;
    }
    Ok(())
}

fn rerere_dir() -> CliResult<PathBuf> {
    let storage = util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
    Ok(storage.join("rerere"))
}

fn load_index() -> CliResult<Index> {
    Index::load(path::index()).map_err(|error| {
        CliError::fatal(format!("failed to load index: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })
}

fn read_err(error: std::io::Error) -> CliError {
    CliError::fatal(format!("rerere: read error: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoReadFailed)
}

fn write_err(error: std::io::Error) -> CliError {
    CliError::fatal(format!("rerere: write error: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_conflict_markers() {
        let conflicted = b"a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> other\nd\n";
        assert!(is_conflicted(conflicted));
        assert!(!is_conflicted(b"a\nb\nc\n"));
        // A lone marker without a separator is not a conflict.
        assert!(!is_conflicted(b"<<<<<<< only\n"));
    }

    #[test]
    fn conflict_id_is_stable_and_content_addressed() {
        let a = conflict_id(b"<<<<<<<\nx\n=======\ny\n>>>>>>>\n");
        let b = conflict_id(b"<<<<<<<\nx\n=======\ny\n>>>>>>>\n");
        let c = conflict_id(b"<<<<<<<\nx\n=======\nz\n>>>>>>>\n");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }
}
