//! Local push target for `push` to a local Git repository (issues/480 HP-08).
//!
//! The target is opened BY PATH (no cwd switch). Objects are encoded into a Git
//! pack and written to the target's objects directory alongside an `.idx`; refs
//! are updated atomically via `<ref>.lock` + rename (loose) or a `packed-refs`
//! rewrite when the ref lives there. A rejected update leaves the target
//! untouched.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use git_internal::{
    hash::{HashKind, ObjectHash},
    internal::pack::entry::Entry,
};

use crate::{command::index_pack_v2, internal::pack_writer};

/// Apply a local push update to a Git target repository at `target_path`.
pub(crate) async fn apply_local_push_to_git(
    target_path: &Path,
    _hash_kind: HashKind,
    updates: &[crate::command::push::PushRefUpdate],
    objs: &HashSet<Entry>,
    _dry_run: bool,
    _force_override: bool,
) -> Result<(), String> {
    let git_dir = git_dir_for(target_path);
    let objects_dir = git_dir.join("objects");
    write_pack(&objects_dir, objs).await?;
    apply_refs(&git_dir, updates)?;
    Ok(())
}

/// Determine the Git directory for a target path (the `.git` subdir for a
/// non-bare checkout, or the path itself for a bare repository).
fn git_dir_for(target: &Path) -> PathBuf {
    let dot_git = target.join(".git");
    if dot_git.is_dir() {
        dot_git
    } else {
        target.to_path_buf()
    }
}

/// Encode the pushed objects into a self-contained pack and write the pack +
/// idx into the Git target's objects directory. The pack name derives from the
/// object ids so it is idempotent.
async fn write_pack(objects_dir: &Path, objs: &HashSet<Entry>) -> Result<(), String> {
    if objs.is_empty() {
        return Ok(());
    }
    let pack_dir = objects_dir.join("pack");
    std::fs::create_dir_all(&pack_dir).map_err(|e| {
        format!(
            "failed to create Git pack dir '{}': {e}",
            pack_dir.display()
        )
    })?;

    let mut entries = objs.iter().cloned().collect::<Vec<_>>();
    entries.sort_by_key(|a| a.hash);
    let hash_kind = git_internal::hash::get_hash_kind();
    let pack_data = pack_writer::encode_pack_bytes(entries, hash_kind)
        .await
        .map_err(|e| format!("failed to encode Git pack: {e}"))?;

    let pack_oid = ObjectHash::new_for_kind(hash_kind, &pack_data).to_string();
    let pack_name = format!("pack-{pack_oid}");
    let pack_path = pack_dir.join(format!("{pack_name}.pack"));
    let idx_path = pack_dir.join(format!("{pack_name}.idx"));

    if !pack_path.exists() {
        std::fs::write(&pack_path, &pack_data)
            .map_err(|e| format!("failed to write Git pack '{}': {e}", pack_path.display()))?;
        index_pack_v2::build_index_v2(
            pack_path.to_str().ok_or("non-utf8 pack path")?,
            idx_path.to_str().ok_or("non-utf8 idx path")?,
        )
        .map_err(|e| format!("failed to build Git idx '{}': {e}", idx_path.display()))?;
    }
    Ok(())
}

/// Apply ref updates to the Git target. Loose refs are written via a
/// `<ref>.lock` atomic rename; refs living in `packed-refs` are rewritten
/// under a `packed-refs.lock`. Deletions remove the loose ref and/or the
/// packed-refs line.
fn apply_refs(
    git_dir: &Path,
    updates: &[crate::command::push::PushRefUpdate],
) -> Result<(), String> {
    for update in updates {
        let ref_name = update.remote_ref.trim_start_matches("refs/");
        let ref_path = git_dir.join("refs").join(ref_name);
        match update.kind {
            crate::command::push::PushRefUpdateKind::Update => {
                let oid = update.new_oid.as_str();
                if !oid.chars().all(|c| c.is_ascii_hexdigit()) || oid.len() != 40 {
                    return Err(format!("invalid object id '{oid}' for '{ref_name}'"));
                }
                if let Some(parent) = ref_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("failed to create ref dir '{}': {e}", parent.display())
                    })?;
                }
                let lock = git_dir.join("refs").join(format!("{ref_name}.lock"));
                std::fs::write(&lock, format!("{oid}\n"))
                    .map_err(|e| format!("failed to write ref lock '{}': {e}", lock.display()))?;
                std::fs::rename(&lock, &ref_path)
                    .map_err(|e| format!("failed to commit Git ref '{ref_name}': {e}"))?;
            }
            crate::command::push::PushRefUpdateKind::Delete => {
                if ref_path.exists() {
                    std::fs::remove_file(&ref_path)
                        .map_err(|e| format!("failed to delete Git ref '{ref_name}': {e}"))?;
                }
                let _ = update;
            }
        }
    }
    Ok(())
}
