//! `libra repack` — consolidate objects into a single pack.
//!
//! Reachable objects are encoded into one new `pack-<checksum>.pack` (plus its
//! index) through the shared [`crate::internal::pack_writer`], the same writer
//! the `maintenance` tasks use — there is no separate pack encoder here.
//!
//! - default: pack the reachable objects that are currently **loose**; existing
//!   packs are left untouched (an incremental repack).
//! - `-a` / `--all`: pack **all** reachable objects, including ones already in a
//!   pack, into a single fresh pack.
//! - `-d` / `--delete`: after packing, remove the loose objects that now live in
//!   the new pack. Existing packs are never deleted, so an object is never left
//!   unreferenced.

use std::collections::HashSet;

use clap::Parser;
use git_internal::hash::{ObjectHash, get_hash_kind};
use serde::Serialize;

use crate::{
    command::maintenance::{collect_reachable_objects, list_loose_objects, parse_object_hash},
    internal::pack_writer,
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

pub const REPACK_EXAMPLES: &str = "\
EXAMPLES:
    libra repack               Pack loose reachable objects into a single pack
    libra repack -a            Pack all reachable objects (loose and already packed)
    libra repack -a -d         Repack everything, then drop the now-redundant loose objects
    libra repack -d            Pack loose objects and delete the ones now packed";

/// Combine repository objects into a single pack.
#[derive(Parser, Debug)]
#[command(after_help = REPACK_EXAMPLES)]
pub struct RepackArgs {
    /// Pack all reachable objects, including those already stored in a pack.
    #[arg(short = 'a', long = "all")]
    pub all: bool,
    /// Remove loose objects that end up in the new pack.
    #[arg(short = 'd', long = "delete")]
    pub delete: bool,
    /// Suppress informational output.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

#[derive(Serialize)]
struct RepackOutput {
    pack: String,
    objects_packed: usize,
    loose_removed: usize,
}

pub async fn execute(args: RepackArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: RepackArgs, output: &OutputConfig) -> CliResult<()> {
    let repo_path = util::try_get_storage_path(None)
        .map_err(|e| CliError::repo_not_found().with_hint(e.to_string()))?;
    let storage = ClientStorage::init(path::objects());
    let hash_kind = get_hash_kind();

    // §C.4.3 writer-vs-deleter: `repack` PUBLISHES (a new pack becomes the
    // only home of objects it consolidates) and, under `-d`, DELETES. It
    // takes the maintenance lock shared for the publication half, exactly
    // like every other publisher, so an obliteration cannot classify an
    // object as loose-only and then find it re-published inside a pack it
    // never looked at. `-d` upgrades to the exclusive hold below.
    let publish_lock = crate::internal::maintenance_lock::MaintenanceLock::shared(&repo_path)?;

    let reachable = collect_reachable_objects(&storage).await?;

    // Loose objects on disk, as (hash, path). Used to pick the default packing
    // set and to know which files `-d` may remove.
    let loose = list_loose_objects(&repo_path)
        .map_err(|e| CliError::fatal(format!("failed to list loose objects: {e}")))?;
    let loose_hashes: HashSet<ObjectHash> = loose
        .iter()
        .filter_map(|(hash_str, _)| parse_object_hash(hash_str))
        .collect();

    let to_pack: Vec<ObjectHash> = if args.all {
        reachable.iter().copied().collect()
    } else {
        reachable.intersection(&loose_hashes).copied().collect()
    };

    let pack_dir = path::objects().join("pack");
    let publication =
        match pack_writer::write_pack_with_index(&storage, &to_pack, &pack_dir, hash_kind).await {
            Ok(Some(publication)) => publication,
            Ok(None) => {
                if !output.quiet && !output.is_json() {
                    println!("Nothing new to pack.");
                }
                return Ok(());
            }
            Err(e) => return Err(CliError::fatal(format!("failed to write pack: {e}"))),
        };

    // `-d`: drop loose objects that are now in the new pack. Only files whose
    // hash is in `to_pack` are removed, so nothing that was left out of the pack
    // is ever deleted.
    let pack_path = publication.path.clone();
    let mut loose_removed = 0usize;
    if args.delete {
        // The pack is about to become these objects' only home, so its NAME
        // has to be durable, not just its bytes: a crash that loses the
        // directory entry after the loose copies are gone loses the objects.
        if !publication.durable {
            return Err(CliError::fatal(
                "refusing to delete loose objects: the new pack's directory entry could not be \
                 made durable, so a crash could leave the objects with no copy at all"
                    .to_string(),
            )
            .with_stable_code(crate::utils::error::StableErrorCode::IoWriteFailed)
            .with_hint("re-run 'libra repack' without -d to keep the loose objects"));
        }
        // lore.md 2.3 / plan-20260714 W0 deletion hard gate: `-d` is a
        // DELETION entry point, so it must refuse while another repository
        // borrows from this object store — this store's reachability does
        // not include the borrower's refs, so a loose object it still needs
        // could be dropped. The borrower must `alternates remove` (or
        // dissociate) first.
        crate::internal::alternates::ensure_no_live_borrowers(
            "delete loose objects after repacking",
            crate::utils::error::StableErrorCode::ConflictOperationBlocked,
        )
        .map_err(|error| {
            error.with_hint("or re-run 'libra repack' without -d to keep the loose objects")
        })?;
        // The publication is durable on disk now, so the shared hold is
        // released and the deletion phase takes the EXCLUSIVE one: no
        // publisher may be running while loose payloads are unlinked. A
        // shared hold cannot be upgraded in place (another process may hold
        // it too), which is why this is a release-then-acquire rather than a
        // nested acquisition.
        drop(publish_lock);
        let _deletion_lock =
            crate::internal::maintenance_lock::MaintenanceLock::exclusive_or_refuse(
                &repo_path,
                "delete loose objects after repacking",
            )
            .map_err(|error| {
                error.with_hint("or re-run 'libra repack' without -d to keep the loose objects")
            })?;
        // Re-checked UNDER the exclusive hold: the gate above ran before this
        // lock existed, and borrower registration is itself a publication, so
        // one that appeared in between is caught here (§C.4.3).
        crate::internal::alternates::ensure_no_live_borrowers(
            "delete loose objects after repacking",
            crate::utils::error::StableErrorCode::ConflictOperationBlocked,
        )
        .map_err(|error| {
            error.with_hint("or re-run 'libra repack' without -d to keep the loose objects")
        })?;
        let packed: HashSet<ObjectHash> = to_pack.iter().copied().collect();
        for (hash_str, obj_path) in &loose {
            if let Some(hash) = parse_object_hash(hash_str)
                && packed.contains(&hash)
            {
                match std::fs::remove_file(obj_path) {
                    Ok(()) => loose_removed += 1,
                    // Already gone. Two `repack -d` runs can each snapshot the
                    // same loose set under their shared holds; whichever
                    // reaches the exclusive phase second finds the files
                    // removed, and its own pack still contains every object,
                    // so the goal state is reached. Treating that as fatal
                    // failed a command that had done its job correctly.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(CliError::fatal(format!(
                            "failed to remove loose object {hash_str}: {e}"
                        )));
                    }
                }
            }
        }
    }

    // Either the `-d` branch released it, or it is released when this
    // function returns — the publication window ends with the pack on disk.
    let pack_name = pack_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    if output.is_json() {
        emit_json_data(
            "repack",
            &RepackOutput {
                pack: pack_name,
                objects_packed: to_pack.len(),
                loose_removed,
            },
            output,
        )?;
    } else if !output.quiet {
        println!("Packed {} objects into {pack_name}.", to_pack.len());
        if args.delete {
            println!("Removed {loose_removed} loose objects now stored in the pack.");
        }
    }

    Ok(())
}
