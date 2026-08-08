//! `libra alternates` — manage object alternates (lore.md 2.3).
//!
//! Object alternates let this repo BORROW objects from a shared/parent object
//! store instead of copying them: reads resolve through the registered
//! alternate on a local miss. Registering a base ALSO records this repo as a
//! borrower of it, so the base's `gc` / `cache evict` refuse to prune the
//! borrowed objects (deletion safety).
//!
//! v1 scope: `add` / `list` / `remove`. It refuses to borrow from a TIERED
//! base (a plain-local alternate cannot reach the base's remote tier) and from
//! a base whose `core.objectformat` differs from this repo's hash kind.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::{
    internal::alternates,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

pub const ALTERNATES_EXAMPLES: &str = "\
EXAMPLES:
    libra alternates add /path/to/base/.libra/objects   Borrow objects from a shared store
    libra alternates list                                Show registered alternates
    libra alternates remove /path/to/base/.libra/objects Stop borrowing (unregisters borrower)
    libra alternates prune --dry-run                     Show borrower registrations whose repo is gone
    libra alternates prune                               Retire them (unblocks this store's gc)
    libra alternates prune /gone/.libra/objects           Retire one that cannot be checked at all

NOTE: reads borrow from the base on a local miss; the base is protected — while
this repo borrows, the base's 'gc'/'cache evict' refuse to prune the borrowed
objects. `git clone --reference` copy-avoidance is a separate follow-up.";

/// Manage object alternates — borrow objects from a shared store (lore.md 2.3).
#[derive(Parser, Debug)]
#[command(after_help = ALTERNATES_EXAMPLES)]
pub struct AlternatesArgs {
    #[command(subcommand)]
    pub command: AlternatesCommand,
}

#[derive(Subcommand, Debug)]
pub enum AlternatesCommand {
    /// Register an object directory to borrow objects from.
    Add {
        /// Path to the alternate's object directory (e.g.
        /// `<repo>/.libra/objects`) or a repo root (resolved to its objects).
        path: String,
    },
    /// List the registered alternates.
    List,
    /// Stop borrowing from an object directory.
    Remove { path: String },
    /// Retire borrower registrations whose repository is gone.
    ///
    /// A borrower is registered in THIS store so its objects are protected
    /// from `gc` / `repack -d` / `cache evict`. Nothing removes such a
    /// registration automatically: an absent path is indistinguishable from
    /// an unmounted one, and guessing wrong deletes objects a borrower still
    /// needs. This command is where the user supplies that judgement.
    Prune {
        /// Retire this exact registration, whatever the filesystem says about
        /// it. Use when a borrower is gone for good but its path cannot be
        /// checked — an unreachable mount, a permission-denied parent — so
        /// the automatic "absent" rule cannot see it.
        path: Option<String>,
        /// Report what would be retired and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Resolve a user-supplied path to an object directory: accept an objects dir
/// directly, or a repo root (append `.libra/objects` / `objects`).
fn looks_like_object_dir(p: &Path) -> bool {
    p.is_dir() && (p.join("info").is_dir() || p.join("pack").is_dir())
}

/// Resolve a user path to an OBJECT directory (Codex P1: a repo ROOT must map
/// to its `.libra/objects`, never register the root itself). Prefer the
/// repo-root layouts first; accept the raw path only when it is itself an
/// object dir (has `info`/`pack`).
fn resolve_objects_dir(input: &str) -> CliResult<PathBuf> {
    let p = Path::new(input);
    // Repo-root layouts win first.
    for candidate in [p.join(".libra").join("objects"), p.join("objects")] {
        if looks_like_object_dir(&candidate) {
            return std::fs::canonicalize(&candidate).or(Ok(candidate));
        }
    }
    // Otherwise the input must itself be an object dir.
    if looks_like_object_dir(p) {
        return std::fs::canonicalize(p).or_else(|_| Ok(p.to_path_buf()));
    }
    Err(CliError::fatal(format!(
        "'{input}' is not a readable object directory or repository (expected an \
         'objects' dir with info/ or pack/)"
    ))
    .with_stable_code(StableErrorCode::CliInvalidTarget))
}

fn this_object_format() -> String {
    match git_internal::hash::get_hash_kind() {
        git_internal::hash::HashKind::Sha1 => "sha1".to_string(),
        git_internal::hash::HashKind::Sha256 => "sha256".to_string(),
    }
}

/// The foreign repo's `libra.db` path (`<repo>/.libra/libra.db`), or None if
/// there is no `.libra` parent.
fn foreign_db_path(objects_dir: &Path) -> Option<PathBuf> {
    objects_dir.parent().map(|libra| libra.join("libra.db"))
}

/// The base repo's `core.objectformat` and `LIBRA_STORAGE_TYPE`, read from its
/// SQLite via a READ-ONLY sea-orm connection (Codex P1 — the project API, not a
/// shelled-out sqlite3 that fails OPEN when absent). Returns
/// `Err` when the config cannot be read (the caller FAILS CLOSED). `Ok(None)`
/// for a repo with no DB (a plain-git base — no tiering, sha1 by default).
async fn read_foreign_config(objects_dir: &Path) -> Result<Option<(String, bool)>, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let Some(db_path) = foreign_db_path(objects_dir) else {
        return Ok(None);
    };
    if !db_path.exists() {
        return Ok(None);
    }
    let url = format!("sqlite://{}?mode=ro", db_path.display());
    let conn = sea_orm::Database::connect(&url)
        .await
        .map_err(|e| format!("cannot open the base repo's config database: {e}"))?;
    // objectformat (config table; default sha1).
    let fmt_row = conn
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT value FROM config WHERE name = 'objectformat' AND configuration = 'core' \
             LIMIT 1"
                .to_string(),
        ))
        .await
        .map_err(|e| format!("cannot read the base repo's objectformat: {e}"))?;
    let objectformat = match fmt_row {
        Some(row) => row
            .try_get_by_index::<String>(0)
            .map(|v| {
                if v.trim().is_empty() {
                    "sha1".into()
                } else {
                    v
                }
            })
            .unwrap_or_else(|_| "sha1".to_string()),
        None => "sha1".to_string(),
    };
    // tiered? (config_kv LIBRA_STORAGE_TYPE = s3/r2).
    let tier_row = conn
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT value FROM config_kv WHERE key = 'LIBRA_STORAGE_TYPE' LIMIT 1".to_string(),
        ))
        .await
        .map_err(|e| format!("cannot read the base repo's storage type: {e}"))?;
    let tiered = tier_row
        .and_then(|row| row.try_get_by_index::<String>(0).ok())
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "s3" | "r2"))
        .unwrap_or(false);
    Ok(Some((objectformat, tiered)))
}

/// Register `base_objects` as an alternate of `clone_objects` (lore.md 2.3),
/// applying the full guard set (self-reference, fail-closed objectformat, and
/// tiered-base refusal) and the dual alternates/borrowers write. Shared by the
/// `alternates add` command and the 2.11 clone auto-register hook.
pub(crate) async fn guarded_add(
    clone_objects: &Path,
    base_objects: &Path,
    this_object_format: &str,
) -> CliResult<()> {
    // Refuse a self-reference.
    let me = std::fs::canonicalize(clone_objects).unwrap_or_else(|_| clone_objects.to_path_buf());
    if std::fs::canonicalize(base_objects).unwrap_or_else(|_| base_objects.to_path_buf()) == me {
        return Err(CliError::fatal("a repository cannot borrow from itself")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    // Fail-CLOSED objectformat + tiered guards: an unreadable base config
    // REFUSES the borrow rather than allowing a cross-hash / tiered base.
    match read_foreign_config(base_objects).await {
        Ok(Some((base_fmt, tiered))) => {
            if base_fmt != this_object_format {
                return Err(CliError::fatal(format!(
                    "cannot borrow from an object store with core.objectformat '{base_fmt}' \
                     (this repo is '{this_object_format}')"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            if tiered {
                return Err(CliError::fatal(
                    "cannot borrow from a tiered (s3/r2) object store — a local alternate \
                     cannot reach its remote tier",
                )
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("clone/copy the base locally, or use a non-tiered base"));
            }
        }
        // No DB → a plain-git base, implicitly sha1, non-tiered.
        Ok(None) => {
            if this_object_format != "sha1" {
                return Err(CliError::fatal(format!(
                    "cannot borrow from a base with no Libra config (assumed sha1) while this \
                     repo is '{this_object_format}'"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
        }
        Err(e) => {
            return Err(CliError::fatal(format!(
                "cannot verify the base repository's config; refusing to borrow: {e}"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid));
        }
    }
    alternates::add(clone_objects, base_objects).map_err(|e| {
        CliError::fatal(format!("failed to register the alternate: {e}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })
}

pub async fn execute_safe(args: AlternatesArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let objects_dir = path::objects();
    match args.command {
        AlternatesCommand::Add { path: input } => {
            let alt = resolve_objects_dir(&input)?;
            guarded_add(&objects_dir, &alt, &this_object_format()).await?;
            if !output.quiet && !output.is_json() {
                println!("registered alternate {}", alt.display());
            }
            if output.is_json() {
                return emit_json_data(
                    "alternates",
                    &serde_json::json!({ "action": "add", "alternate": alt.to_string_lossy() }),
                    output,
                );
            }
            Ok(())
        }
        AlternatesCommand::List => {
            let alts = alternates::list(&objects_dir);
            if output.is_json() {
                let paths: Vec<String> = alts
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                return emit_json_data(
                    "alternates",
                    &serde_json::json!({ "alternates": paths }),
                    output,
                );
            }
            if alts.is_empty() && !output.quiet {
                println!("no alternates registered");
            } else {
                for alt in &alts {
                    println!("{}", alt.display());
                }
            }
            Ok(())
        }
        AlternatesCommand::Prune { path, dry_run } => {
            // Taken verbatim. Canonicalizing the user's path would resolve a
            // symlink onto a DIFFERENT, live borrower and retire that one
            // instead — the registration list is matched against this value,
            // not the other way round.
            let named = path.as_deref().map(PathBuf::from);
            let candidates = match named.as_deref() {
                Some(target) => vec![target.to_path_buf()],
                None => alternates::absent_borrowers(&objects_dir).map_err(|error| {
                    CliError::fatal(format!(
                        "cannot read this store's borrower registration \
                         ('objects/info/borrowers'): {error}"
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?,
            };
            let removed = if dry_run {
                Vec::new()
            } else {
                // Re-checked under the borrowers-file lock: a borrower that
                // came back and re-registered between the listing and here
                // keeps its fresh registration.
                alternates::prune_borrowers(&objects_dir, named.as_deref()).map_err(|error| {
                    CliError::fatal(format!(
                        "failed to update this store's borrower registration: {error}"
                    ))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
                })?
            };
            if output.is_json() {
                return emit_json_data(
                    "alternates",
                    &serde_json::json!({
                        "action": "prune",
                        "dry_run": dry_run,
                        "candidates": candidates
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect::<Vec<_>>(),
                        "removed": removed
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect::<Vec<_>>(),
                    }),
                    output,
                );
            }
            if !output.quiet {
                if dry_run {
                    if candidates.is_empty() {
                        println!("no borrower registrations point at a missing repository");
                    } else {
                        for path in &candidates {
                            println!("would retire borrower {}", path.display());
                        }
                    }
                } else if removed.is_empty() {
                    println!("no borrower registrations were retired");
                } else {
                    for path in &removed {
                        println!("retired borrower {}", path.display());
                    }
                }
            }
            Ok(())
        }
        AlternatesCommand::Remove { path: input } => {
            let alt = resolve_objects_dir(&input)?;
            let removed = alternates::remove(&objects_dir, &alt).map_err(|e| {
                CliError::fatal(format!("failed to remove the alternate: {e}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            if !removed {
                return Err(CliError::fatal(format!(
                    "'{}' is not a registered alternate",
                    alt.display()
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget));
            }
            if !output.quiet {
                println!("removed alternate {}", alt.display());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    /// Surface guard: every subcommand this command accepts is documented in
    /// BOTH user manuals and in the development reference.
    ///
    /// A new subcommand is public surface, and public surface that only
    /// exists in `--help` is surface nobody finds. This catches the omission
    /// at the one moment it is cheap to fix — `prune` was added and three
    /// documents kept describing `add|list|remove`.
    #[test]
    fn every_subcommand_is_documented() {
        // Each entry is a form the manuals must SHOW, not merely mention:
        // `prune` and `prune <path>` are different routes with different
        // preconditions, and documenting only the first leaves the one that
        // rescues an unreadable registration undiscoverable.
        const SUBCOMMANDS: &[&str] = &["add", "list", "remove", "prune", "prune /"];
        let docs: [(&str, &str); 3] = [
            (
                "docs/commands/alternates.md",
                include_str!("../../docs/commands/alternates.md"),
            ),
            (
                "docs/commands/zh-CN/alternates.md",
                include_str!("../../docs/commands/zh-CN/alternates.md"),
            ),
            (
                "docs/development/commands/alternates.md",
                include_str!("../../docs/development/commands/alternates.md"),
            ),
        ];
        for (name, body) in docs {
            for subcommand in SUBCOMMANDS {
                assert!(
                    body.contains(&format!("libra alternates {subcommand}")),
                    "{name} does not document `libra alternates {subcommand}`"
                );
            }
        }
    }
}
