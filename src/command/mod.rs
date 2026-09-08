//! Command module hub exporting all subcommands plus shared helpers for
//! loading/saving objects and prompting for authentication.
//!
//! Commenting convention for AI-maintained command code: public command entry
//! points should document their externally visible side effects and error
//! mapping intent. Prefer `# Side Effects` and `# Errors` sections on
//! `execute_safe`/equivalent structured handlers so future agents can modify
//! command flows without missing repository, index, worktree, network, or
//! rendering consequences.

pub mod account;
pub mod add;
pub mod agent;
pub mod alternates;
pub mod am;
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
pub mod deps;
pub mod describe;
pub mod diff;
pub mod diff_plumbing;
pub mod dirty;
pub mod editor;
pub mod fast_export;
pub mod fast_import;
pub mod fetch;
pub mod file;
pub mod for_each_ref;
pub mod format_patch;
pub mod fsck;
pub mod graph;
pub mod grep;
pub mod hash_object;
pub(crate) mod history_config;
pub mod hooks;
pub mod hydrate;
pub mod index_pack;
mod index_pack_support;
mod index_pack_v1;
mod index_pack_v2;
pub mod init;
pub mod layer;
pub mod lfs;
pub mod lfs_schema;
pub mod log;
pub mod logfile;
pub mod ls_files;
pub mod ls_remote;
pub mod ls_tree;
pub mod mailinfo;
pub mod maintenance;
#[cfg(feature = "fastcdc")]
pub mod media;
pub mod merge;
pub mod merge_base;
pub mod merge_file;
pub(crate) mod merge_message;
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
// The shared diffcore rename engine (§B.4). Public so the wave-0
// integration suite can pin engine-level contracts (evidence allow-list,
// budget independence) that no CLI path can construct today.
pub mod rename_detect;
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
pub mod sparse_view;
pub mod symbolic_ref;
pub mod tag;
pub(crate) mod unmerged;
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
pub mod status_io_worker;
pub(crate) mod status_probe;
pub(crate) mod status_untracked;
pub(crate) mod status_untracked_paths;
pub mod switch;
pub mod upgrade;
pub mod web_assets;
pub mod write_tree;

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::object::{ObjectTrait, blob::Blob},
    utils::HashAlgorithm,
};
use rpassword::read_password;

use crate::{
    internal::protocol::https_client::BasicAuth,
    utils,
    utils::{client_storage::ClientStorage, error::emit_warning, util},
};

// impl load for all objects
// NOTE (Part C ledger): the W0 `ensure_main_worktree`/`ensure_main_worktree_because`
// transition guards were RETIRED with the W2 stash slice — every formerly
// repository-global store (sequencers, dirty cache, layer, sparse view, the
// stash stack protocol) is worktree-aware now, and no command refuses to run
// in a linked worktree on those grounds.
//
// The remaining Code/Agent linked-worktree preflight
// (`require_main_worktree_for_code_agent`) was retired in W4-08 after the
// W4-06/W4-11/W4-12 resolver and W4-07/W4-13 approval ownership landed.
// Damaged/unreadable or unregistered linked scope still fail-closes; healthy
// registered linked worktrees launch `libra code` / `libra automation`
// through the resolver.

/// W4-08: healthy registered linked worktrees may run Code/Agent surfaces.
/// A damaged scope, unreadable registry, or `worktree_id` that is missing
/// from `worktrees.json` (including a synthesized fallback id) still
/// fail-closes — unknown ownership must not start a session or automation.
pub(crate) fn require_registered_worktree_scope(
    surface: &str,
    workdir: &std::path::Path,
) -> Result<(), crate::utils::error::CliError> {
    use crate::internal::worktree_scope::{RequestScope, WorktreeScope};

    let request = match RequestScope::try_resolve(workdir.to_path_buf()) {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        Err(error) => {
            return Err(crate::utils::error::CliError::fatal(format!(
                "{surface} cannot run: the worktree scope at '{}' could not be \
                 resolved ({error})",
                workdir.display()
            ))
            .with_stable_code(crate::utils::error::StableErrorCode::RepoCorrupt)
            .with_hint(
                "run `libra worktree repair <this-worktree-path> --confirm` from \
                 the main worktree",
            ));
        }
    };
    let WorktreeScope::Linked(id) = &request.scope else {
        return Ok(());
    };
    match worktree::registry_knows_linked_worktree_in_storage(
        &request.storage,
        id,
        Some(&request.worktree_root),
    ) {
        Some(true) => Ok(()),
        Some(false) => Err(crate::utils::error::CliError::fatal(format!(
            "{surface} cannot run: this linked worktree's identity '{id}' is \
             not in the worktree registry"
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::RepoCorrupt)
        .with_hint(
            "run `libra worktree repair <this-worktree-path> --confirm` from \
             the main worktree to restore this worktree's identity",
        )),
        None => Err(crate::utils::error::CliError::fatal(format!(
            "{surface} cannot run: the worktree registry could not be read, so \
             this worktree's identity cannot be confirmed"
        ))
        .with_stable_code(crate::utils::error::StableErrorCode::RepoCorrupt)
        .with_hint(
            "fix or restore `.libra/worktrees.json` in the repository storage, \
             then retry",
        )),
    }
}

pub fn load_object<T>(hash: &ObjectHash) -> Result<T, GitError>
where
    T: ObjectTrait,
{
    // Apply any `refs/replace/<oid>` substitution before reading, so `log`,
    // `show`, `rev-parse` peeling, etc. transparently see the replacement.
    // Cheap no-op when no replacements exist.
    let hash = replace::resolve(*hash);
    let storage = util::try_objects_storage().map_err(GitError::IOError)?;
    let data = storage.get(&hash)?;
    T::from_bytes(&data.to_vec(), hash)
}

/// RAW load: NO `refs/replace` substitution (plan-20260714 §C.4.1.1 process
/// cache rules — the GC/repack reachability walk must traverse the ORIGINAL
/// graph byte-for-byte; resolving replacements would leave the replaced
/// object's own tree/parents unrooted and expose a corrupt history the
/// moment the replace ref is deleted).
pub fn load_object_raw<T>(hash: &ObjectHash) -> Result<T, GitError>
where
    T: ObjectTrait,
{
    let storage = util::try_objects_storage().map_err(GitError::IOError)?;
    let data = storage.get(hash)?;
    T::from_bytes(&data.to_vec(), *hash)
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
    let path = path.as_ref();
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Ok(Blob::from_content_bytes(read_symlink_blob_bytes(path)?).id);
    }
    if utils::lfs::is_lfs_tracked(path) {
        let (pointer, _) = utils::lfs::generate_pointer_file(path);
        return Ok(Blob::from_content(&pointer).id);
    }

    stream_file_blob_hash(path)
}

/// Read the bytes Git would store for a worktree path's blob.
///
/// Regular files use their file content (or the generated LFS pointer when the
/// path is LFS-tracked). Symlinks use the link target bytes and are never
/// followed.
pub fn read_worktree_blob_bytes(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return read_symlink_blob_bytes(path);
    }
    if utils::lfs::is_lfs_tracked(path) {
        let (pointer, _) = utils::lfs::generate_pointer_file(path);
        return Ok(pointer.into_bytes());
    }
    fs::read(path)
}

pub(crate) fn read_symlink_blob_bytes(path: &Path) -> io::Result<Vec<u8>> {
    Ok(symlink_target_blob_bytes(&fs::read_link(path)?))
}

/// Build an index entry whose recorded stat PROVABLY describes the hashed
/// content. The caller stats the file BEFORE reading/hashing it; this
/// helper re-stats after (via `new_from_file`) and, when the two disagree
/// — the racy window where an edit landed between the content read and
/// the stat — zeroes the volatile stat fields so every later `status`
/// content-compares the entry instead of trusting a post-edit stat paired
/// with a pre-edit hash. The index writers previously statted only AFTER
/// hashing, which produced exactly that poisoned pairing (2026-08-06
/// R0-8 review: plain status hid a concurrently-edited file, and a
/// non-`-a` commit would have built a stale tree from it).
pub(crate) fn verified_index_entry(
    name: &Path,
    hash: ObjectHash,
    workdir: &Path,
    pre_read: Option<&fs::Metadata>,
) -> io::Result<git_internal::internal::index::IndexEntry> {
    use git_internal::internal::index::Time;

    let mut entry = git_internal::internal::index::IndexEntry::new_from_file(name, hash, workdir)?;
    // No pre-read stat means no proof — smudge.
    if !pre_read.is_some_and(|pre_read| entry_stat_matches_metadata(&entry, pre_read)) {
        entry.ctime = Time::from_system_time(std::time::UNIX_EPOCH);
        entry.mtime = Time::from_system_time(std::time::UNIX_EPOCH);
        entry.dev = 0;
        entry.ino = 0;
        entry.uid = 0;
        entry.gid = 0;
    }
    Ok(entry)
}

/// Whether an entry's volatile stat triple equals `metadata`'s. Mirrors
/// the comparison `status` performs (`index_stat_differs`), so a pairing
/// this returns `true` for is exactly one status would trust.
fn entry_stat_matches_metadata(
    entry: &git_internal::internal::index::IndexEntry,
    metadata: &fs::Metadata,
) -> bool {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use git_internal::internal::index::Time;

    #[cfg(unix)]
    fn stat_times(metadata: &fs::Metadata) -> (SystemTime, SystemTime) {
        use std::os::unix::fs::MetadataExt;

        fn at(seconds: i64, nanos: i64) -> SystemTime {
            if seconds < 0 {
                return UNIX_EPOCH;
            }
            let nanos = u32::try_from(nanos)
                .ok()
                .filter(|nanos| *nanos < 1_000_000_000)
                .unwrap_or(0);
            UNIX_EPOCH + Duration::new(seconds as u64, nanos)
        }
        (
            at(metadata.ctime(), metadata.ctime_nsec()),
            at(metadata.mtime(), metadata.mtime_nsec()),
        )
    }
    #[cfg(not(unix))]
    fn stat_times(metadata: &fs::Metadata) -> (SystemTime, SystemTime) {
        let _ = Duration::from_secs(0);
        (
            metadata
                .created()
                .or_else(|_| metadata.modified())
                .unwrap_or(UNIX_EPOCH),
            metadata
                .modified()
                .or_else(|_| metadata.created())
                .unwrap_or(UNIX_EPOCH),
        )
    }

    let Ok(size) = u32::try_from(metadata.len()) else {
        return false;
    };
    let (ctime, mtime) = stat_times(metadata);
    entry.size == size
        && entry.ctime == Time::from_system_time(ctime)
        && entry.mtime == Time::from_system_time(mtime)
}

#[cfg(unix)]
pub fn symlink_target_blob_bytes(target: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    target.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
pub fn symlink_target_blob_bytes(target: &Path) -> Vec<u8> {
    target.to_string_lossy().as_bytes().to_vec()
}

/// Hash a worktree file as a Git blob under a hard byte cap.
///
/// Returns `Ok(None)` when the file exceeds `cap`, or when its size changes
/// under the read. The second case matters for correctness as much as for
/// budget: the blob header commits to a length before the body is read, so
/// a file that grows or shrinks mid-read would otherwise yield a WRONG
/// object id that silently claims to be the file's content. Callers that
/// enforce a read budget must use this instead of [`stream_file_blob_hash`],
/// whose size check can be defeated by a file that grows after the stat.
/// Returns `(oid, bytes_read)`. `bytes_read` is what was actually pulled off
/// the file, NOT the caller's pre-read `stat` length, and is reported on the
/// refusal paths too: a file that grows between the caller's stat and this
/// read must be charged for the bytes it really cost, or a growth race buys
/// unmetered I/O against the budget this function exists to enforce.
pub(crate) fn stream_file_blob_hash_bounded(
    path: impl AsRef<Path>,
    cap: u64,
) -> io::Result<(Option<ObjectHash>, u64)> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > cap {
        return Ok((None, 0));
    }
    // Read AT MOST `len` (<= cap) bytes: the cap is a hard ceiling, so growth
    // is detected by re-stating AFTER the read rather than by reading one
    // byte past it. Reading cap+1 would overrun the very bound this function
    // exists to enforce, exactly when the caller is at its limit.
    let mut reader = io::BufReader::new(file).take(len);
    let mut hasher = HashAlgorithm::new();

    hasher.update(b"blob ");
    hasher.update(len.to_string().as_bytes());
    hasher.update(b"\0");

    let mut buffer = [0_u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    if total != len {
        return Ok((None, total)); // shrank under the read
    }
    // Post-read length check: a file that grew would otherwise hash a prefix
    // and pass it off as the whole file.
    if reader.into_inner().into_inner().metadata()?.len() != len {
        return Ok((None, total)); // grew under the read
    }
    ObjectHash::from_bytes(&hasher.finalize())
        .map(|oid| (Some(oid), total))
        .map_err(io::Error::other)
}

fn stream_file_blob_hash(path: impl AsRef<Path>) -> io::Result<ObjectHash> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut reader = io::BufReader::new(file);
    let mut hasher = HashAlgorithm::new();

    hasher.update(b"blob ");
    hasher.update(len.to_string().as_bytes());
    hasher.update(b"\0");

    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    ObjectHash::from_bytes(&hasher.finalize()).map_err(io::Error::other)
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
    use super::stream_file_blob_hash_bounded;

    /// §B.3.4: `cap` is a HARD ceiling. The bounded hasher used to read
    /// `cap + 1` bytes to detect a growing file, which meant a caller sitting
    /// exactly at its 2 MiB / 64 MiB limit could still be made to read one
    /// byte past it. Growth is now caught by re-stating AFTER the read, so
    /// nothing beyond the cap is ever consumed.
    #[test]
    fn bounded_hash_never_reads_past_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("exactly-at-cap.bin");
        let cap = 4096_u64;
        std::fs::write(&path, vec![b'z'; cap as usize]).expect("write");

        // Exactly at the cap: accepted, and charged exactly the cap.
        let (oid, read) = stream_file_blob_hash_bounded(&path, cap).expect("read at the cap");
        assert!(oid.is_some(), "a file exactly at the cap is readable");
        assert_eq!(read, cap, "and costs exactly the cap, never cap + 1");

        // One byte over: refused before anything is read.
        std::fs::write(&path, vec![b'z'; cap as usize + 1]).expect("grow past the cap");
        let (oid, read) = stream_file_blob_hash_bounded(&path, cap).expect("read past the cap");
        assert!(oid.is_none(), "a file over the cap is refused");
        assert_eq!(read, 0, "and no byte past the cap is consumed to find out");
    }

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

#[cfg(test)]
mod verified_entry_tests {
    use std::path::Path;

    use git_internal::internal::index::Time;

    use super::verified_index_entry;

    /// 2026-08-06 R0-8 review: an edit landing between the pre-read stat
    /// and the post-hash stat smudges the entry (zeroed volatile stats →
    /// every later status content-compares it); an un-raced entry keeps
    /// its verified stat; no pre-read stat never earns trust.
    #[test]
    fn racy_edit_between_read_and_stat_smudges_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zero = Time::from_system_time(std::time::UNIX_EPOCH);

        let file = dir.path().join("racy.txt");
        std::fs::write(&file, "original").unwrap();
        let pre = std::fs::symlink_metadata(&file).unwrap();
        // (the content would be hashed here) — then the racy edit lands:
        std::fs::write(&file, "edited, longer than before").unwrap();
        let hash = git_internal::internal::object::blob::Blob::from_content("original").id;
        let entry = verified_index_entry(Path::new("racy.txt"), hash, dir.path(), Some(&pre))
            .expect("entry");
        assert_eq!(
            entry.mtime, zero,
            "smudged: the stat must not describe content the hash is not"
        );
        assert_eq!(entry.ctime, zero);

        let calm = dir.path().join("calm.txt");
        std::fs::write(&calm, "steady").unwrap();
        let pre = std::fs::symlink_metadata(&calm).unwrap();
        let hash = git_internal::internal::object::blob::Blob::from_content("steady").id;
        let entry = verified_index_entry(Path::new("calm.txt"), hash, dir.path(), Some(&pre))
            .expect("entry");
        assert_ne!(entry.mtime, zero, "an un-raced entry keeps its real stat");

        let entry =
            verified_index_entry(Path::new("calm.txt"), hash, dir.path(), None).expect("entry");
        assert_eq!(entry.mtime, zero, "no pre-read stat never earns trust");
    }
}
