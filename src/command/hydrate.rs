//! `libra hydrate` — on-demand whole-object hydration (lore.md 3.3).
//!
//! Materializes the content of one or more repo paths (and, by default, their
//! transitive forward dependencies) into the working tree, fetching each blob
//! from the local store → an alternate → the remote durable tier. This is the
//! platform-portable, honest v1 of Lore's "hydrating VFS": an EXPLICIT command,
//! NOT a transparent FUSE-on-access filesystem (that remains a `worktree-fuse`
//! follow-up). Whole-object only — no FastCDC byte-range hydration.
//!
//! Failure-recovery contract (the row's 可靠失败恢复): every blob is fetched
//! and (for a borrowed/remote hit) OID-verified BEFORE it is published, then
//! written via an atomic temp-file + rename. A hydration that fails for ANY
//! reason — object missing everywhere, remote unreachable under `--offline`,
//! transport error, verify mismatch, interruption — leaves the pre-existing
//! worktree file UNTOUCHED and never a truncated/half-written file.

use std::{collections::BTreeSet, path::PathBuf};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        commit::Commit,
        tree::{Tree, TreeItemMode},
        types::ObjectType,
    },
};
use serde::Serialize;

use crate::{
    command::{get_target_commit, load_object},
    internal::{
        deps::{DependencyStore, Direction, normalize_edge_path},
        sparse::SparseView,
    },
    utils::{
        atomic_write,
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        lfs,
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

pub const HYDRATE_EXAMPLES: &str = "\
EXAMPLES:
    libra hydrate scene.usd                 Materialize scene.usd + its deps on demand
    libra hydrate scene.usd --no-deps       Just this file, no transitive deps
    libra hydrate assets/ --depth-limit 2   Bound the dependency closure depth
    libra hydrate big.bin --verify          Re-hash the payload against its OID before landing
    libra hydrate a b --dry-run             Report what would hydrate, write nothing

NOTE: whole-object hydration only (no FastCDC range). Content resolves
local -> alternate -> remote (the read policy is honored: --offline/--local
refuse a remote fetch). Out-of-view paths (an active sparse view) are refused
unless --ignore-sparse. Transparent FUSE on-access hydration is a separate
follow-up (lore.md 3.3 deferred).";

/// Hydrate working-tree content on demand (lore.md 3.3, Libra extension).
#[derive(Parser, Debug)]
#[command(after_help = HYDRATE_EXAMPLES)]
pub struct HydrateArgs {
    /// Repo-relative path(s) to hydrate.
    #[clap(required = true)]
    pub pathspec: Vec<String>,
    /// Target revision (default HEAD).
    #[clap(long, default_value = "HEAD")]
    pub revision: String,
    /// Do not pull transitive forward dependencies.
    #[clap(long)]
    pub no_deps: bool,
    /// Bound the dependency-closure depth.
    #[clap(long)]
    pub depth_limit: Option<usize>,
    /// Hydrate even out-of-view paths when a sparse view is active.
    #[clap(long)]
    pub ignore_sparse: bool,
    /// Re-hash the fetched payload against its OID before landing.
    #[clap(long)]
    pub verify: bool,
    /// Report what would hydrate; write nothing.
    #[clap(long)]
    pub dry_run: bool,
    /// Stop at the first failure (default: best-effort, non-zero exit if any failed).
    #[clap(long)]
    pub fail_fast: bool,
}

#[derive(Serialize)]
struct PathReport {
    path: String,
    oid: Option<String>,
    status: &'static str, // hydrated | already-present | skipped-sparse | skipped-unsupported | failed
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub async fn execute_safe(args: HydrateArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Resolve the revision's tree to a path -> (oid, mode) map (Codex: the
    // commit TREE, never the index, so a non-HEAD --revision is honored).
    let commit_oid = get_target_commit(&args.revision).await.map_err(|e| {
        CliError::fatal(format!(
            "revision '{}' could not be resolved: {e}",
            args.revision
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let commit = load_object::<Commit>(&commit_oid).map_err(|e| {
        CliError::fatal(format!("failed to load commit {commit_oid}: {e}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let tree = load_object::<Tree>(&commit.tree_id).map_err(|e| {
        CliError::fatal(format!("failed to load tree {}: {e}", commit.tree_id))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let tree_map: std::collections::HashMap<String, (ObjectHash, TreeItemMode)> = tree
        .get_plain_items_with_mode()
        .into_iter()
        .map(|(p, oid, mode)| (p.to_string_lossy().replace('\\', "/"), (oid, mode)))
        .collect();

    // Normalize + validate the requested roots (reuse deps path normalization).
    let mut roots: Vec<String> = Vec::new();
    for raw in &args.pathspec {
        let norm = normalize_edge_path(raw).map_err(|e| {
            CliError::fatal(format!("invalid path '{raw}': {e}"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        roots.push(norm);
    }

    // Expand transitive forward deps (absence-tolerant: empty graph → roots).
    let mut targets: BTreeSet<String> = roots.iter().cloned().collect();
    if !args.no_deps {
        let closure = DependencyStore::transitive_closure(
            &args.revision,
            &roots,
            Direction::Forward,
            args.depth_limit,
        )
        .await
        .map_err(|e| {
            CliError::fatal(format!("failed to expand the dependency closure: {e}"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
        targets.extend(closure.reachable);
    }

    // Sparse gating over the FULL set (roots AND deps) — Codex: a sparse view
    // set to avoid materializing large out-of-view assets must not be bypassed
    // by a dependency edge. Gate on is_active() FIRST (a no-op view returns
    // contains()==true for everything).
    // W1 §C.4.1.1: hydrate MATERIALIZES files, so an unreadable view must
    // fail closed instead of degrading to "everything in view" (which would
    // bypass the sparse gate on a probe failure).
    let scope = crate::internal::worktree_scope::WorktreeScope::current();
    let sparse = SparseView::try_load(&scope).await.map_err(|e| {
        CliError::fatal(format!("cannot load the sparse view before hydrating: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let sparse_active = sparse.is_active() && !args.ignore_sparse;

    let workdir = util::working_dir();
    let storage = ClientStorage::init(path::objects());

    let mut reports: Vec<PathReport> = Vec::new();
    let mut any_failed = false;

    for target in &targets {
        if sparse_active && !sparse.contains_str(target) {
            reports.push(PathReport {
                path: target.clone(),
                oid: None,
                status: "skipped-sparse",
                bytes: None,
                error: None,
            });
            continue;
        }
        let report = hydrate_one(target, &tree_map, &storage, &workdir, &args).await;
        if report.status == "failed" {
            any_failed = true;
            if args.fail_fast {
                reports.push(report);
                break;
            }
        }
        reports.push(report);
    }

    render(&reports, output, &args)?;
    if any_failed {
        return Err(CliError::failure("one or more paths failed to hydrate")
            .with_stable_code(StableErrorCode::RepoStateInvalid));
    }
    Ok(())
}

async fn hydrate_one(
    target: &str,
    tree_map: &std::collections::HashMap<String, (ObjectHash, TreeItemMode)>,
    storage: &ClientStorage,
    workdir: &std::path::Path,
    args: &HydrateArgs,
) -> PathReport {
    let fail = |oid: Option<String>, msg: String| PathReport {
        path: target.to_string(),
        oid,
        status: "failed",
        bytes: None,
        error: Some(msg),
    };

    let Some((oid, mode)) = tree_map.get(target) else {
        return fail(None, format!("'{target}' does not exist at this revision"));
    };
    let oid_str = oid.to_string();

    // v1 handles regular file blobs only; symlink/gitlink/subtree entries are
    // skipped honestly (never a wrong-content or partial write).
    if !matches!(mode, TreeItemMode::Blob | TreeItemMode::BlobExecutable) {
        return PathReport {
            path: target.to_string(),
            oid: Some(oid_str),
            status: "skipped-unsupported",
            bytes: None,
            error: Some(format!(
                "entry kind {mode:?} is not supported by hydrate v1"
            )),
        };
    }

    // Fetch bytes through the full resolution chain (local -> alternate ->
    // remote); the read policy is honored inside `get`.
    let bytes = match storage.get(oid) {
        Ok(b) => b,
        Err(e) => {
            return fail(
                Some(oid_str),
                format!(
                    "could not fetch object (not present locally, or the offline/local read \
                     policy forbids a remote fetch): {e}"
                ),
            );
        }
    };

    // LFS-pointer blobs are DEFERRED in v1 (Codex: their download path is not
    // atomic/verified) — skipped cleanly, never a truncated media file.
    if lfs::parse_pointer_data(&bytes).is_some() {
        return PathReport {
            path: target.to_string(),
            oid: Some(oid_str),
            status: "skipped-unsupported",
            bytes: None,
            error: Some(
                "LFS-pointer blob; hydrate v1 handles plain blobs only (use libra lfs)".into(),
            ),
        };
    }

    // Optional whole-object integrity gate (belt-and-suspenders for the local
    // loose/pack path, which `get` does not re-verify). On mismatch, heal from
    // the durable tier and retry once.
    if args.verify {
        let mut verified_bytes = bytes;
        if crate::utils::storage::tiered::verify_fetched_object(
            oid,
            ObjectType::Blob,
            &verified_bytes,
        )
        .is_err()
        {
            match storage.heal(oid) {
                Ok(true) => match storage.get(oid) {
                    Ok(healed) => {
                        if crate::utils::storage::tiered::verify_fetched_object(
                            oid,
                            ObjectType::Blob,
                            &healed,
                        )
                        .is_err()
                        {
                            return fail(
                                Some(oid_str),
                                "object failed OID verification after heal".into(),
                            );
                        }
                        verified_bytes = healed;
                    }
                    Err(e) => {
                        return fail(Some(oid_str), format!("re-fetch after heal failed: {e}"));
                    }
                },
                _ => {
                    return fail(
                        Some(oid_str),
                        "object failed OID verification and could not be healed".into(),
                    );
                }
            }
        }
        return land(target, &oid_str, verified_bytes, *mode, workdir, args);
    }

    land(target, &oid_str, bytes, *mode, workdir, args)
}

/// Publish the verified bytes to the worktree path, atomically. Already-present
/// (byte-identical) content is a no-op skip; a dry run writes nothing.
fn land(
    target: &str,
    oid_str: &str,
    bytes: Vec<u8>,
    mode: TreeItemMode,
    workdir: &std::path::Path,
    args: &HydrateArgs,
) -> PathReport {
    let abs: PathBuf = workdir.join(target);

    // Already-present: the worktree file's content hashes to the same OID.
    if abs.exists()
        && let Ok(existing) = crate::command::calc_file_blob_hash(&abs)
        && existing.to_string() == oid_str
    {
        return PathReport {
            path: target.to_string(),
            oid: Some(oid_str.to_string()),
            status: "already-present",
            bytes: Some(bytes.len()),
            error: None,
        };
    }

    if args.dry_run {
        return PathReport {
            path: target.to_string(),
            oid: Some(oid_str.to_string()),
            status: "hydrated", // would-hydrate; dry run makes no change
            bytes: Some(bytes.len()),
            error: None,
        };
    }

    // Ensure the parent dir exists, then atomic temp-write + rename. A crash
    // mid-write discards the temp file — the final path is never truncated.
    if let Some(parent) = abs.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return PathReport {
            path: target.to_string(),
            oid: Some(oid_str.to_string()),
            status: "failed",
            bytes: None,
            error: Some(format!("could not create parent directory: {e}")),
        };
    }
    if let Err(e) = atomic_write::write_atomic(&abs, &bytes, atomic_write::sync_data_enabled()) {
        return PathReport {
            path: target.to_string(),
            oid: Some(oid_str.to_string()),
            status: "failed",
            bytes: None,
            error: Some(format!("atomic write failed: {e}")),
        };
    }
    // Executable bit. The content already landed atomically (never a corrupt
    // file), but a chmod failure must be reported LOUDLY (Codex P1) — a file
    // with the wrong mode can break scripts — not silently reported as
    // "hydrated". write_atomic does not expose the temp handle, so the bit is
    // set after the rename; a failure fails this path with an actionable note.
    #[cfg(unix)]
    if mode == TreeItemMode::BlobExecutable {
        use std::os::unix::fs::PermissionsExt;
        let set = std::fs::metadata(&abs).and_then(|meta| {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            std::fs::set_permissions(&abs, perms)
        });
        if let Err(e) = set {
            return PathReport {
                path: target.to_string(),
                oid: Some(oid_str.to_string()),
                status: "failed",
                bytes: None,
                error: Some(format!(
                    "content hydrated but the executable bit could not be set (the file mode \
                     is wrong): {e}"
                )),
            };
        }
    }
    #[cfg(not(unix))]
    let _ = mode;

    PathReport {
        path: target.to_string(),
        oid: Some(oid_str.to_string()),
        status: "hydrated",
        bytes: Some(bytes.len()),
        error: None,
    }
}

fn render(reports: &[PathReport], output: &OutputConfig, args: &HydrateArgs) -> CliResult<()> {
    if output.is_json() {
        let hydrated = reports.iter().filter(|r| r.status == "hydrated").count();
        let skipped = reports
            .iter()
            .filter(|r| r.status.starts_with("skipped") || r.status == "already-present")
            .count();
        let failed = reports.iter().filter(|r| r.status == "failed").count();
        return emit_json_data(
            "hydrate",
            &serde_json::json!({
                "dry_run": args.dry_run,
                "paths": reports,
                "summary": { "requested": reports.len(), "hydrated": hydrated, "skipped": skipped, "failed": failed },
            }),
            output,
        );
    }
    if output.quiet {
        return Ok(());
    }
    for r in reports {
        match r.status {
            "hydrated" => println!(
                "{} {}",
                if args.dry_run {
                    "would hydrate"
                } else {
                    "hydrated"
                },
                r.path
            ),
            "already-present" => println!("up to date  {}", r.path),
            "skipped-sparse" => println!("skipped (out of sparse view)  {}", r.path),
            "skipped-unsupported" => println!(
                "skipped ({})  {}",
                r.error.as_deref().unwrap_or("unsupported"),
                r.path
            ),
            "failed" => eprintln!(
                "error: {}: {}",
                r.path,
                r.error.as_deref().unwrap_or("failed")
            ),
            _ => {}
        }
    }
    Ok(())
}
