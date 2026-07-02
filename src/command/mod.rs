//! Command module hub exporting all subcommands plus shared helpers for
//! loading/saving objects and prompting for authentication.
//!
//! Commenting convention for AI-maintained command code: public command entry
//! points should document their externally visible side effects and error
//! mapping intent. Prefer `# Side Effects` and `# Errors` sections on
//! `execute_safe`/equivalent structured handlers so future agents can modify
//! command flows without missing repository, index, worktree, network, or
//! rendering consequences.

pub mod add;
pub mod agent;
pub mod apply;
pub mod archive;
pub mod auth;
pub mod automation;
pub mod bisect;
pub mod blame;
pub mod branch;
pub mod bundle;
pub mod cache;
pub mod cat_file;
pub mod check_attr;
pub mod check_ignore;
pub mod check_mailmap;
pub mod checkout;
pub mod cherry_pick;
pub mod clean;
pub mod clone;
pub mod cloud;
pub mod code;
pub mod code_control;
pub mod code_control_files;
pub mod commit;
pub mod commit_tree;
pub mod completions;
pub mod config;
pub mod credential;
pub mod describe;
pub mod diff;
pub mod diff_plumbing;
pub mod dirty;
pub mod editor;
pub mod fast_export;
pub mod fast_import;
pub mod fetch;
pub mod for_each_ref;
pub mod format_patch;
pub mod fsck;
pub mod graph;
pub mod grep;
pub mod hash_object;
pub mod hooks;
pub mod index_pack;
mod index_pack_support;
mod index_pack_v1;
mod index_pack_v2;
pub mod init;
pub mod lfs;
pub mod lfs_schema;
pub mod log;
pub mod logfile;
pub mod ls_files;
pub mod ls_remote;
pub mod ls_tree;
pub mod maintenance;
pub mod merge;
pub mod merge_base;
pub mod merge_file;
pub mod metadata;
pub mod mv;
pub mod notes;
pub mod op;
pub mod open;
pub mod pack_objects;
pub mod package;
pub mod publish;
pub mod pull;
pub mod push;
pub mod read_tree;
pub mod rebase;
pub mod reflog;
pub mod remote;
pub mod remove;
pub mod repack;
pub mod replace;
pub mod rerere;
pub mod reset;
pub mod restore;
pub mod rev_list;
pub mod rev_parse;
pub mod revert;
pub mod revision;
pub mod sandbox;
pub mod service;
pub mod shortlog;
pub mod show;
pub mod show_ref;
mod show_ref_check;
mod show_ref_deref;
mod show_ref_exclude_existing;
mod show_ref_render;
pub mod symbolic_ref;
pub mod tag;
pub mod update_index;
pub mod update_ref;
pub mod usage;
pub mod verify_pack;
mod verify_pack_decode;
mod verify_pack_index;
mod verify_pack_index_common;
mod verify_pack_index_v2;
mod verify_pack_render;
mod verify_pack_support;
mod verify_pack_types;
#[cfg(all(unix, feature = "worktree-fuse"))]
#[path = "worktree-fuse.rs"]
pub mod worktree;
#[cfg(not(all(unix, feature = "worktree-fuse")))]
pub mod worktree;

pub mod stash;
pub mod status;
pub mod switch;
pub mod web_assets;
pub mod write_tree;

use std::{io, io::Write, path::Path};

use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::object::{ObjectTrait, blob::Blob},
};
use rpassword::read_password;

use crate::{
    internal::protocol::https_client::BasicAuth,
    utils,
    utils::{client_storage::ClientStorage, error::emit_warning, object_ext::BlobExt, util},
};

// impl load for all objects
pub fn load_object<T>(hash: &ObjectHash) -> Result<T, GitError>
where
    T: ObjectTrait,
{
    // Apply any `refs/replace/<oid>` substitution before reading, so `log`,
    // `show`, `rev-parse` peeling, etc. transparently see the replacement.
    // Cheap no-op when no replacements exist.
    let hash = replace::resolve(*hash);
    let storage = util::objects_storage();
    let data = storage.get(&hash)?;
    T::from_bytes(&data.to_vec(), hash)
}

// impl save for all objects
pub fn save_object<T>(object: &T, obj_id: &ObjectHash) -> Result<(), GitError>
where
    T: ObjectTrait,
{
    let storage = util::objects_storage();
    save_object_to_storage(&storage, object, obj_id)
}

pub fn save_object_to_storage<T>(
    storage: &ClientStorage,
    object: &T,
    obj_id: &ObjectHash,
) -> Result<(), GitError>
where
    T: ObjectTrait,
{
    let data = object.to_data()?;
    storage.put(obj_id, &data, object.get_type())?;
    Ok(())
}

/// Ask for username and password (CLI interaction)
fn ask_username_password() -> (String, String) {
    let read_prompt = |prompt: &str| -> String {
        print!("{prompt}");
        // Normally your OS will buffer output by line when it's connected to a terminal,
        // which is why it usually flushes when a newline is written to stdout.
        if let Err(err) = io::stdout().flush() {
            emit_warning(format!("failed to flush stdout: {err}"));
        }

        let mut value = String::new();
        if let Err(err) = io::stdin().read_line(&mut value) {
            eprintln!("error: failed to read input: {err}");
            return String::new();
        }
        value.trim().to_string()
    };

    let username = read_prompt("username: ");
    tracing::debug!("username: {}", username);

    print!("password: ");
    if let Err(err) = io::stdout().flush() {
        emit_warning(format!("failed to flush stdout: {err}"));
    }

    let password = if std::env::var("LIBRA_NO_HIDE_PASSWORD").is_ok() {
        // for test
        read_prompt("")
    } else {
        // In non-tty environments, hidden input can fail (for example: "No such device or address").
        match read_password() {
            Ok(password) => password.trim().to_string(),
            Err(err) => {
                eprintln!(
                    "warning: failed to read hidden password ({err}); falling back to plain input."
                );
                read_prompt("")
            }
        }
    };
    (username, password)
}

/// same as ask_username_password, but return BasicAuth
pub fn ask_basic_auth() -> BasicAuth {
    let (username, password) = ask_username_password();
    BasicAuth { username, password }
}

/// Calculate the hash of a file blob
/// - for `lfs` file: calculate hash of the pointer data
pub fn calc_file_blob_hash(path: impl AsRef<Path>) -> io::Result<ObjectHash> {
    let blob = if utils::lfs::is_lfs_tracked(&path) {
        let (pointer, _) = utils::lfs::generate_pointer_file(&path);
        Blob::from_content(&pointer)
    } else {
        Blob::from_file(&path)
    };
    Ok(blob.id)
}

/// Get the commit hash from branch name or commit hash, support remote branch
pub async fn get_target_commit(
    branch_or_commit: &str,
) -> Result<ObjectHash, Box<dyn std::error::Error>> {
    util::get_commit_base(branch_or_commit)
        .await
        .map_err(|e| e.into())
}

#[cfg(test)]
mod tests {
    use git_internal::internal::object::commit::Commit;
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        common_utils::{format_commit_msg, parse_commit_msg},
        utils::test,
    };
    #[tokio::test]
    #[serial]
    /// Test objects can be correctly saved to and loaded from storage.
    async fn test_save_load_object() {
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());
        let object = Commit::from_tree_id(ObjectHash::new(&[1; 20]), vec![], "\nCommit_1");
        save_object(&object, &object.id).unwrap();
        let _ = load_object::<Commit>(&object.id).unwrap();
    }

    #[test]
    /// Tests commit message formatting and parsing with signatures.
    /// Verifies correct handling of GPG/SSH signatures and proper message extraction.
    fn test_format_and_parse_commit_msg() {
        {
            let msg = "commit message";
            let gpg_sig =
                "gpgsig -----BEGIN PGP SIGNATURE-----\ncontent\n-----END PGP SIGNATURE-----";
            let ssh_sig =
                "gpgsig -----BEGIN SSH SIGNATURE-----\ncontent1\n-----END SSH SIGNATURE-----";
            let msg_gpg = format_commit_msg(msg, Some(gpg_sig));
            let msg_ssh = format_commit_msg(msg, Some(ssh_sig));
            let gpg_sig_val = &gpg_sig[7..];
            let ssh_sig_val = &ssh_sig[7..];
            let (msg_, gpg_sig_) = parse_commit_msg(&msg_gpg);
            let (msg__, ssh_sig__) = parse_commit_msg(&msg_ssh);
            assert_eq!(msg, msg_);
            assert_eq!(msg, msg__);
            assert_eq!(gpg_sig_val, gpg_sig_.unwrap());
            assert_eq!(ssh_sig_val, ssh_sig__.unwrap());

            let msg_none = format_commit_msg(msg, None);
            let (msg_, sig_) = parse_commit_msg(&msg_none);
            assert_eq!(msg, msg_);
            assert_eq!(None, sig_);
        }

        {
            let msg = "commit message";
            let gpg_sig = "gpgsig -----BEGIN PGP SIGNATURE-----\ncontent\n-----END PGP SIGNATURE-----\n \n \n";
            let msg_gpg = format_commit_msg(msg, Some(gpg_sig));
            let (msg_, _) = parse_commit_msg(&msg_gpg);
            assert_eq!(msg, msg_);
        }
    }
}
