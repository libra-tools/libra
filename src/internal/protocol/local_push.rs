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
) -> Result<Vec<String>, String> {
    let storage_dir = try_get_storage_path(Some(target_path.to_path_buf())).map_err(|e| {
        format!(
            "failed to open target repository '{}': {e}",
            target_path.display()
        )
    })?;
    let objects_dir = storage_dir.join("objects");

    // Object store for the target (path-addressed, never the caller's cwd).
    git_internal::hash::set_hash_kind(hash_kind);
    let storage = ClientStorage::try_init_local(objects_dir).map_err(|error| error.to_string())?;
    if !dry_run {
        for entry in objs {
            storage
                .put(&entry.hash, &entry.data, entry.obj_type)
                .map_err(|e| format!("failed to write object {} to target: {e}", entry.hash))?;
        }
    }

    let backend = crate::storage::runtime::detect_repo_backend(&storage_dir).map_err(|e| {
        format!(
            "failed to detect target repository backend '{}': {e}",
            storage_dir.display()
        )
    })?;
    match backend {
        crate::storage::StorageBackendKind::Redb => {
            let config_store =
                crate::storage::runtime::open_repo_config_store_read_only(&storage_dir)
                    .await
                    .map_err(|e| {
                        format!(
                            "failed to open target config store '{}': {e}",
                            storage_dir.display()
                        )
                    })?;
            let git_store = crate::storage::runtime::open_git_store_read_write(&storage_dir)
                .await
                .map_err(|e| {
                    format!(
                        "failed to open target git store '{}': {e}",
                        storage_dir.display()
                    )
                })?;
            let checked_out =
                target_checked_out_branch_with_store(&*config_store, &*git_store).await?;
            let deny = match ConfigKv::get_with_store(&*config_store, "receive.denyCurrentBranch")
                .await
                .ok()
                .flatten()
                .map(|entry| entry.value.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("warn") => DenyCurrentBranch::Warn,
                Some("ignore") => DenyCurrentBranch::Ignore,
                _ => DenyCurrentBranch::Refuse,
            };
            let mut warnings = Vec::new();
            for update in updates {
                if apply_one_update_with_store(
                    &*config_store,
                    &*git_store,
                    update,
                    &checked_out,
                    hash_kind,
                    force_override,
                    deny,
                    dry_run,
                )
                .await?
                {
                    warnings.push("updating the current branch".to_string());
                }
            }
            Ok(warnings)
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db_path = storage_dir.join("libra.db");
            let db = crate::internal::db::get_db_conn_instance_for_path(&db_path)
                .await
                .map_err(|e| {
                    format!(
                        "failed to open target database '{}': {e}",
                        db_path.display()
                    )
                })?;
            let checked_out = target_checked_out_branch(&db).await?;
            // Git's `receive.denyCurrentBranch`: `warn` updates the checked-out branch
            // and tells the pusher, `ignore` does it silently, anything else refuses.
            let deny = match ConfigKv::get_with_conn(&db, "receive.denyCurrentBranch")
                .await
                .ok()
                .flatten()
                .map(|entry| entry.value.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("warn") => DenyCurrentBranch::Warn,
                Some("ignore") => DenyCurrentBranch::Ignore,
                _ => DenyCurrentBranch::Refuse,
            };
            let updates_owned = updates.to_vec();
            let checked_out_owned = checked_out.clone();
            write_transaction::<_, _, Vec<String>, String>(&db, |txn| {
                let updates = updates_owned.clone();
                let checked_out = checked_out_owned.clone();
                Box::pin(async move {
                    let mut warnings = Vec::new();
                    for update in &updates {
                        if apply_one_update(
                            txn,
                            update,
                            &checked_out,
                            hash_kind,
                            force_override,
                            deny,
                        )
                        .await?
                        {
                            // The renderer prefixes `warning: `.
                            warnings.push("updating the current branch".to_string());
                        }
                    }
                    Ok::<_, String>(warnings)
                })
            })
            .await
            .map_err(|err| match err {
                sea_orm::TransactionError::Connection(e) => {
                    format!("target DB connection error: {e}")
                }
                sea_orm::TransactionError::Transaction(e) => e,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_one_update_with_store(
    config_store: &dyn crate::storage::config_store::ConfigStore,
    store: &dyn crate::storage::git_store::GitStore,
    update: &PushRefUpdate,
    checked_out: &Option<String>,
    hash_kind: HashKind,
    force_override: bool,
    deny: DenyCurrentBranch,
    dry_run: bool,
) -> Result<bool, String> {
    let remote_ref = &update.remote_ref;
    let (storage_name, remote_scope) = target_branch_storage(remote_ref);

    let current =
        Branch::find_branch_result_with_store(store, &storage_name, remote_scope.as_deref())
            .await
            .map_err(|e| format!("failed to inspect target ref '{remote_ref}': {e}"))?
            .map(|b| b.commit.to_string());

    let mut warned_checked_out = false;
    if checked_out.as_deref().is_some_and(|b| b == storage_name) {
        match deny {
            DenyCurrentBranch::Refuse => {
                return Err(format!(
                    "refusing to update checked-out branch '{storage_name}' (target ref \
                     {remote_ref})"
                ));
            }
            DenyCurrentBranch::Warn => warned_checked_out = true,
            DenyCurrentBranch::Ignore => {}
        }
    }

    let ref_key = match remote_scope {
        Some(remote) => crate::storage::git_store::RefKey::RemoteBranch {
            remote,
            name: storage_name.clone(),
        },
        None => crate::storage::git_store::RefKey::LocalBranch {
            name: storage_name.clone(),
        },
    };

    match update.kind {
        PushRefUpdateKind::Delete => {
            if let (Some(expected), Some(found)) = (update.old_oid.as_deref(), current.as_deref())
                && expected != found
            {
                return Err(format!(
                    "failed to delete '{remote_ref}': expected {expected} but found {found}"
                ));
            }
            if !dry_run && let Some(found) = current.as_deref() {
                let zero = ObjectHash::zero_str(hash_kind).to_string();
                let reflog_write = Reflog::build_reflog_write_with_store(
                    config_store,
                    &ReflogContext {
                        old_oid: found.to_string(),
                        new_oid: zero,
                        action: ReflogAction::Push,
                    },
                    &storage_name,
                )
                .await;
                let expected_target =
                    crate::storage::git_store::RefTarget::Direct(found.to_string());
                let outcome = store
                    .update_ref_cas(&ref_key, Some(&expected_target), None, &[reflog_write])
                    .await
                    .map_err(|e| format!("failed to delete '{remote_ref}': {e}"))?;
                if !matches!(
                    outcome,
                    crate::storage::git_store::RefCasOutcome::Applied { .. }
                ) {
                    return Err(format!(
                        "concurrent ref modification while deleting '{remote_ref}'"
                    ));
                }
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
            if !dry_run {
                let old = current
                    .clone()
                    .unwrap_or_else(|| ObjectHash::zero_str(hash_kind).to_string());
                let reflog_write = Reflog::build_reflog_write_with_store(
                    config_store,
                    &ReflogContext {
                        old_oid: old,
                        new_oid: update.new_oid.clone(),
                        action: ReflogAction::Push,
                    },
                    &storage_name,
                )
                .await;
                let expected_target = current
                    .as_ref()
                    .map(|oid| crate::storage::git_store::RefTarget::Direct(oid.clone()));
                let new_target =
                    crate::storage::git_store::RefTarget::Direct(update.new_oid.clone());
                let outcome = store
                    .update_ref_cas(
                        &ref_key,
                        expected_target.as_ref(),
                        Some(&new_target),
                        &[reflog_write],
                    )
                    .await
                    .map_err(|e| format!("failed to update '{remote_ref}': {e}"))?;
                if !matches!(
                    outcome,
                    crate::storage::git_store::RefCasOutcome::Applied { .. }
                ) {
                    return Err(format!(
                        "concurrent ref modification while updating '{remote_ref}'"
                    ));
                }
            }
        }
    }
    Ok(warned_checked_out)
}

async fn target_checked_out_branch_with_store(
    config_store: &dyn crate::storage::config_store::ConfigStore,
    git_store: &dyn crate::storage::git_store::GitStore,
) -> Result<Option<String>, String> {
    let bare = ConfigKv::get_with_store(config_store, "core.bare")
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
    let head = git_store
        .get_ref(&crate::storage::git_store::RefKey::LocalHead { worktree_id: None })
        .await
        .map_err(|e| format!("failed to read target HEAD: {e}"))?;
    Ok(match head {
        Some(crate::storage::git_store::RefTarget::Symbolic(name)) => Some(name),
        _ => None,
    })
}

/// Git's `receive.denyCurrentBranch` policy for the target repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenyCurrentBranch {
    Refuse,
    Warn,
    Ignore,
}

async fn apply_one_update<C: ConnectionTrait>(
    txn: &C,
    update: &PushRefUpdate,
    checked_out: &Option<String>,
    hash_kind: HashKind,
    force_override: bool,
    deny: DenyCurrentBranch,
) -> Result<bool, String> {
    let remote_ref = &update.remote_ref;
    let (storage_name, remote_scope) = target_branch_storage(remote_ref);

    let current = Branch::find_branch_result_with_conn(txn, &storage_name, remote_scope.as_deref())
        .await
        .map_err(|e| format!("failed to inspect target ref '{remote_ref}': {e}"))?
        .map(|b| b.commit.to_string());

    // Checked-out branch protection, following the target's policy: `warn`
    // allows the update and reports it, `ignore` allows it silently.
    let mut warned_checked_out = false;
    if checked_out.as_deref().is_some_and(|b| b == storage_name) {
        match deny {
            DenyCurrentBranch::Refuse => {
                return Err(format!(
                    "refusing to update checked-out branch '{storage_name}' (target ref \
                     {remote_ref})"
                ));
            }
            DenyCurrentBranch::Warn => warned_checked_out = true,
            DenyCurrentBranch::Ignore => {}
        }
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
    Ok(warned_checked_out)
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

#[cfg(all(test, feature = "storage-redb"))]
mod tests {
    use git_internal::internal::object::types::ObjectType;

    use super::*;
    use crate::{
        git_protocol::ServiceType,
        internal::{
            config::ConfigKv, head::Head, protocol::local_client::LocalClient,
            worktree_scope::WorktreeScope,
        },
        storage::runtime::{
            init_redb_repository_storage, open_git_store_read_only, open_git_store_read_write,
            open_repo_config_store_read_write,
        },
        utils::path_case::effective_ignore_case_for_dir_sync,
    };

    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn redb_local_client_push_object_index_and_path_case() {
        let tmp = tempfile::tempdir().expect("tmp");
        let target_dir = tmp.path().join("target-redb");
        let dot_libra = target_dir.join(".libra");
        let objects_dir = dot_libra.join("objects");
        std::fs::create_dir_all(&objects_dir).expect("mkdir objects");
        let _paths = init_redb_repository_storage(&dot_libra)
            .await
            .expect("init redb target");
        assert!(!dot_libra.join("libra.db").exists());

        let repo_id = "repo-rb16b-local";
        let config_store = open_repo_config_store_read_write(&dot_libra)
            .await
            .expect("open config store");
        ConfigKv::set_with_store(&*config_store, "libra.repoid", repo_id, false)
            .await
            .expect("set repoid");
        ConfigKv::set_with_store(&*config_store, "core.bare", "false", false)
            .await
            .expect("set core.bare");
        ConfigKv::set_with_store(&*config_store, "core.ignorecase", "true", false)
            .await
            .expect("set core.ignorecase");
        drop(config_store);

        let git_store = open_git_store_read_write(&dot_libra)
            .await
            .expect("open git store");
        Head::update_for_scope_result_with_store(
            &*git_store,
            Head::Branch("main".to_string()),
            None,
            &WorktreeScope::Main,
        )
        .await
        .expect("set HEAD");
        drop(git_store);

        // AC4: path_case.rs reads core.ignorecase from ConfigStore on Redb.
        assert!(
            effective_ignore_case_for_dir_sync(&target_dir).expect("effective_ignore_case"),
            "core.ignorecase=true must be read from redb ConfigStore"
        );

        // AC2 + AC3: local_push.rs pushes to redb target without libra.db, and
        // ClientStorage indexes pushed objects into GitStore object_index.
        let blob_bytes = b"hello rb16b redb local push\n".to_vec();
        let blob_oid = ObjectHash::from_type_and_data(ObjectType::Blob, &blob_bytes);
        let empty_tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
        let commit_bytes = format!(
            "tree {empty_tree_oid}\nauthor Test <t@example.com> 1700000000 +0000\ncommitter Test <t@example.com> 1700000000 +0000\n\nrb16b push\n"
        )
        .into_bytes();
        let commit_oid = ObjectHash::from_type_and_data(ObjectType::Commit, &commit_bytes);

        let mut objs = HashSet::new();
        objs.insert(Entry {
            hash: blob_oid,
            data: blob_bytes,
            obj_type: ObjectType::Blob,
            chain_len: 0,
        });
        objs.insert(Entry {
            hash: empty_tree_oid,
            data: Vec::new(),
            obj_type: ObjectType::Tree,
            chain_len: 0,
        });
        objs.insert(Entry {
            hash: commit_oid,
            data: commit_bytes,
            obj_type: ObjectType::Commit,
            chain_len: 0,
        });

        let updates = vec![PushRefUpdate {
            local_ref: "refs/heads/feature".to_string(),
            remote_ref: "refs/heads/feature".to_string(),
            old_oid: None,
            new_oid: commit_oid.to_string(),
            kind: PushRefUpdateKind::Update,
            forced: false,
        }];
        let warnings =
            apply_local_push_to_libra(&target_dir, HashKind::Sha1, &updates, &objs, false, false)
                .await
                .expect("local push to redb target");
        assert!(warnings.is_empty());
        ClientStorage::wait_for_background_tasks();

        // Verify object_index populated in redb GitStore!
        let ro_git = open_git_store_read_only(&dot_libra)
            .await
            .expect("open ro git store");
        let indexed = ro_git
            .list_object_index_by_repo(repo_id, None)
            .await
            .expect("list object_index");
        assert!(
            indexed.iter().any(|r| r.o_id == commit_oid.to_string()),
            "pushed commit must be indexed in redb object_index: {indexed:?}"
        );
        drop(ro_git);

        // AC1: LocalClient discovers the redb repo and its refs without libra.db!
        let client = LocalClient::from_path(&target_dir).expect("LocalClient::from_path redb");
        assert!(client.is_libra_source());
        let disc = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .expect("discover refs from redb target");
        assert!(
            disc.refs
                .iter()
                .any(|r| r._ref == "refs/heads/feature" && r._hash == commit_oid.to_string()),
            "LocalClient must discover pushed branch on redb repo: {:?}",
            disc.refs
        );
        assert!(
            !dot_libra.join("libra.db").exists(),
            "pure redb RB-16b must never create libra.db"
        );
    }
}
