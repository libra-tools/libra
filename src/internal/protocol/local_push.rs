//! Local push target for `push` to a local Libra repository (issues/480 HP-07).
//!
//! Unlike the network transports, the target is opened BY PATH (no process cwd
//! switch, per GC-HP-03), objects are written to the target's own object store,
//! and each ref update is compare-and-swap guarded inside a single metadata
//! transaction. A rejected update (checked-out branch, non-fast-forward without
//! force) leaves the target untouched.

use std::{collections::HashSet, path::Path};

use git_internal::{
    hash::{HashKind, ObjectHash},
    internal::pack::entry::Entry,
};
#[allow(unused_imports)]
use sea_orm::ConnectionTrait;
#[allow(unused_imports)]
use sea_orm::entity::prelude::*;

use crate::{
    command::push::{PushRefUpdate, PushRefUpdateKind},
    internal::{
        branch::Branch,
        config::ConfigKv,
        db::write_transaction,
        model::reference,
        reflog::{Reflog, ReflogAction, ReflogContext},
    },
    utils::{client_storage::ClientStorage, util::try_get_storage_path},
};

/// Apply a local push update to a Libra target repository at `target_path`.
/// Writes any missing objects first, then updates each ref with compare-and-swap
/// inside one transaction (all-or-nothing). No cwd switch.
pub(crate) async fn apply_local_push_to_libra(
    target_path: &Path,
    hash_kind: HashKind,
    updates: &[PushRefUpdate],
    objs: &HashSet<Entry>,
    dry_run: bool,
    force_override: bool,
) -> Result<(), String> {
    let storage_dir = try_get_storage_path(Some(target_path.to_path_buf())).map_err(|e| {
        format!(
            "failed to open target repository '{}': {e}",
            target_path.display()
        )
    })?;
    let db_path = storage_dir.join("libra.db");
    let objects_dir = storage_dir.join("objects");

    // Object store for the target (path-addressed, never the caller's cwd).
    git_internal::hash::set_hash_kind(hash_kind);
    let storage = ClientStorage::init_local(objects_dir);
    if !dry_run {
        for entry in objs {
            storage
                .put(&entry.hash, &entry.data, entry.obj_type)
                .map_err(|e| format!("failed to write object {} to target: {e}", entry.hash))?;
        }
    }

    let db = crate::internal::db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|e| {
            format!(
                "failed to open target database '{}': {e}",
                db_path.display()
            )
        })?;
    let checked_out = target_checked_out_branch(&db).await?;
    let updates_owned = updates.to_vec();
    let checked_out_owned = checked_out.clone();

    write_transaction::<_, _, (), String>(&db, |txn| {
        let updates = updates_owned.clone();
        let checked_out = checked_out_owned.clone();
        Box::pin(async move {
            for update in &updates {
                apply_one_update(txn, update, &checked_out, hash_kind, force_override).await?;
            }
            Ok::<_, String>(())
        })
    })
    .await
    .map_err(|err| match err {
        sea_orm::TransactionError::Connection(e) => format!("target DB connection error: {e}"),
        sea_orm::TransactionError::Transaction(e) => e,
    })
}

async fn apply_one_update<C: ConnectionTrait>(
    txn: &C,
    update: &PushRefUpdate,
    checked_out: &Option<String>,
    hash_kind: HashKind,
    force_override: bool,
) -> Result<(), String> {
    let remote_ref = &update.remote_ref;
    let (storage_name, remote_scope) = target_branch_storage(remote_ref);

    let current = Branch::find_branch_result_with_conn(txn, &storage_name, remote_scope.as_deref())
        .await
        .map_err(|e| format!("failed to inspect target ref '{remote_ref}': {e}"))?
        .map(|b| b.commit.to_string());

    // Checked-out branch protection.
    if checked_out.as_deref().is_some_and(|b| b == storage_name) {
        return Err(format!(
            "refusing to update checked-out branch '{storage_name}' (target ref {remote_ref})"
        ));
    }

    match update.kind {
        PushRefUpdateKind::Delete => {
            if let (Some(expected), Some(found)) = (update.old_oid.as_deref(), current.as_deref())
                && expected != found
            {
                return Err(format!(
                    "failed to delete '{remote_ref}': expected {expected} but found {found}"
                ));
            }
            if let Some(found) = current.as_deref() {
                let zero = ObjectHash::zero_str(hash_kind).to_string();
                reflog_entry(txn, remote_ref, found, &zero, &storage_name).await?;
                Branch::delete_branch_result_with_conn(txn, &storage_name, None)
                    .await
                    .map_err(|e| format!("failed to delete '{remote_ref}': {e}"))?;
            }
        }
        PushRefUpdateKind::Update => {
            let is_new = current.is_none();
            let cas_mismatch = current.as_deref() != update.old_oid.as_deref() && !is_new;
            if cas_mismatch {
                return Err(format!(
                    "CAS mismatch on '{remote_ref}': expected {:?} but found {:?}",
                    update.old_oid, current
                ));
            }
            if update.forced && !force_override && !is_new {
                return Err(format!(
                    "non-fast-forward update to '{remote_ref}' requires '+' in the refspec or --force"
                ));
            }
            let old = current
                .clone()
                .unwrap_or_else(|| ObjectHash::zero_str(hash_kind).to_string());
            reflog_entry(txn, remote_ref, &old, &update.new_oid, &storage_name).await?;
            Branch::update_branch_with_conn(
                txn,
                &storage_name,
                &update.new_oid,
                remote_scope.as_deref(),
            )
            .await
            .map_err(|e| format!("failed to update '{remote_ref}': {e}"))?;
        }
    }
    Ok(())
}

async fn reflog_entry<C: ConnectionTrait>(
    txn: &C,
    remote_ref: &str,
    old_oid: &str,
    new_oid: &str,
    storage_name: &str,
) -> Result<(), String> {
    Reflog::insert_single_entry(
        txn,
        &ReflogContext {
            old_oid: old_oid.to_string(),
            new_oid: new_oid.to_string(),
            action: ReflogAction::Push,
        },
        storage_name,
    )
    .await
    .map_err(|e| format!("failed to record reflog for '{remote_ref}': {e}"))
}

/// Read the target's checked-out branch name (HEAD). We open the target DB by
/// path; the HEAD row carries the branch name (or a hash when detached).
async fn target_checked_out_branch(
    db: &sea_orm::DatabaseConnection,
) -> Result<Option<String>, String> {
    // A bare target has no checked-out branch to protect (Git parity).
    let bare = ConfigKv::get_with_conn(db, "core.bare")
        .await
        .map_err(|e| format!("failed to read target core.bare: {e}"))?
        .map(|e| {
            matches!(
                e.value.to_ascii_lowercase().as_str(),
                "true" | "yes" | "on" | "1"
            )
        })
        .unwrap_or(false);
    if bare {
        return Ok(None);
    }
    let head = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
        .one(db)
        .await
        .map_err(|e| format!("failed to read target HEAD: {e}"))?;
    Ok(head.and_then(|row| row.name))
}

fn target_branch_storage(remote_ref: &str) -> (String, Option<String>) {
    if let Some(branch) = remote_ref.strip_prefix("refs/heads/") {
        (branch.to_string(), None)
    } else {
        (remote_ref.to_string(), None)
    }
}
