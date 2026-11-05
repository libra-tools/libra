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
pub mod commit;
pub mod commit_tree;
pub mod completions;
pub mod config;
pub mod control_lock;
pub mod count_objects;
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
pub mod gc;
pub mod graph;
pub mod grep;
pub mod hash_object;
pub(crate) mod history_config;
pub mod hooks;
pub mod hydrate;
pub mod index_pack;
mod index_pack_support;
mod index_pack_v1;
pub mod index_pack_v2;
pub mod init;
pub mod init_conversion;
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
pub mod mega2; // plan-20260912 MB-03: the single public `libra mega2 browser` surface
pub mod mega2_browser; // plan-20260912 MB-02: mega2 browser TUI state + terminal lifecycle
pub mod mega2_traces; // plan-20261015: scoped traces query and explicit fetch
pub mod memory;
pub mod merge;
pub mod merge_base;
pub mod merge_file;
pub(crate) mod merge_message;
pub mod mergetool;
pub mod metadata;
pub mod mv;
pub mod notes;
pub mod op;
pub mod open;
pub mod pack_objects;
pub mod pull;
pub mod push;
pub mod read_tree;
pub mod rebase;
pub mod rebase_todo;
pub mod reflog;
pub mod remote;
pub mod remove;
// The shared diffcore rename engine (§B.4). Public so the wave-0
// integration suite can pin engine-level contracts (evidence allow-list,
// budget independence) that no CLI path can construct today.
pub mod rename_detect;
pub mod repack;
// Shared index stat-refresh and executable-bit helpers (issues/479 UI-02/03),
// used by both `add` and `update-index`.
pub mod refresh;
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
pub mod ui;
pub mod upgrade;
pub mod write_tree;

use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
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
    let storage = util::objects_storage()?;
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
///
/// Every read failure — including an LFS-tracked file that is unreadable or
/// vanishes under the read — is returned as `Err`, never a panic, so racy
/// worktree scans (refresh, the stat-refresh smudge pass) can degrade to
/// "content unknown" (issues/479 FX-479-03 review R2-P1-1).
pub fn calc_file_blob_hash(path: impl AsRef<Path>) -> io::Result<ObjectHash> {
    let path = path.as_ref();
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Ok(Blob::from_content_bytes(read_symlink_blob_bytes(path)?).id);
    }
    if utils::lfs::is_lfs_tracked(path) {
        let (pointer, _) = utils::lfs::generate_pointer_file_result(path)?;
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

/// Failure from [`read_worktree_blob_bytes_bounded`].
#[derive(Debug)]
pub enum WorktreeBlobBoundError {
    /// The path could not be opened or read.
    Io(io::Error),
    /// The representation that would be returned is larger than the limit.
    TooLarge,
}

impl From<io::Error> for WorktreeBlobBoundError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

thread_local! {
    static BOUNDED_BLOB_HANDLE_BYTES: Cell<u64> = const { Cell::new(0) };
}

/// Bytes read from the file handle by the most recent
/// [`read_worktree_blob_bytes_bounded`] call on this thread.
///
/// Symlink targets are not included: they come from `read_link`, not from the
/// opened handle. The counter is the evidence that a regular file stops at
/// `limit + 1` bytes.
pub fn read_worktree_blob_bounded_handle_bytes() -> u64 {
    BOUNDED_BLOB_HANDLE_BYTES.with(Cell::get)
}

/// Read the bytes Git would store for a worktree path, stopping at `limit`.
///
/// The open does not follow a symlink. On Unix the descriptor is created with
/// `O_NOFOLLOW`; a symlink then yields its target bytes via `read_link`. On
/// Windows the open uses `FILE_FLAG_OPEN_REPARSE_POINT` and a reparse point is
/// likewise returned as the link target. A regular file is classified with
/// `fstat` on that same descriptor and its body is read from it, at most
/// `limit + 1` bytes. Replacing the path after the open cannot switch the
/// read onto another object.
pub fn read_worktree_blob_bytes_bounded(
    path: impl AsRef<Path>,
    limit: u64,
) -> Result<Vec<u8>, WorktreeBlobBoundError> {
    BOUNDED_BLOB_HANDLE_BYTES.with(|cell| cell.set(0));
    let path = path.as_ref();
    let file = match open_nofollow(path) {
        Ok(file) => file,
        Err(error) if is_nofollow_symlink(&error) => {
            return limited_bytes(&read_symlink_blob_bytes(path)?, limit);
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if metadata.file_type().is_symlink() {
        return limited_bytes(&read_symlink_blob_bytes(path)?, limit);
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file or symlink", path.display()),
        )
        .into());
    }
    maybe_bounded_read_pause();
    if utils::lfs::is_lfs_tracked(path) {
        // Pointer text is what Git stores. MC-39 moves the hash onto this
        // already-open handle; the limit still applies to the pointer bytes.
        let (pointer, _) = utils::lfs::generate_pointer_file(path);
        return limited_bytes(pointer.as_bytes(), limit);
    }
    read_handle_limited(file, limit)
}

fn open_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: do not follow a symlink or junction.
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

fn is_nofollow_symlink(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

fn limited_bytes(bytes: &[u8], limit: u64) -> Result<Vec<u8>, WorktreeBlobBoundError> {
    if bytes.len() as u64 > limit {
        Err(WorktreeBlobBoundError::TooLarge)
    } else {
        Ok(bytes.to_vec())
    }
}

fn read_handle_limited(mut file: File, limit: u64) -> Result<Vec<u8>, WorktreeBlobBoundError> {
    let max = limit.saturating_add(1);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    while (buf.len() as u64) < max {
        let room = usize::try_from(max - buf.len() as u64).unwrap_or(usize::MAX);
        let want = room.min(chunk.len());
        let read = file.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        BOUNDED_BLOB_HANDLE_BYTES.with(|cell| {
            cell.set(cell.get() + u64::try_from(read).unwrap_or(u64::MAX));
        });
    }
    if buf.len() as u64 > limit {
        Err(WorktreeBlobBoundError::TooLarge)
    } else {
        Ok(buf)
    }
}

fn maybe_bounded_read_pause() {
    #[cfg(debug_assertions)]
    if std::env::var("LIBRA_TEST").ok().as_deref() == Some("1")
        && let Ok(path) = std::env::var("LIBRA_MCP_TEST_BOUNDED_READ_PAUSE_FILE")
    {
        let _ = std::fs::write(&path, "paused");
        let go = format!("{path}.go");
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(5) {
            if std::path::Path::new(&go).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Bytes of zlib output `object_size` may inflate while reading a header.
const OBJECT_SIZE_INFLATE_LIMIT: u64 = 64 * 1024;

thread_local! {
    static OBJECT_SIZE_INFLATED: Cell<u64> = const { Cell::new(0) };
}

/// Inflated bytes consumed by the most recent [`object_size`] call on this thread.
///
/// The counter is the debug evidence that a size lookup stops at the object
/// header (or the delta target-size varint) instead of reconstructing the body.
pub fn object_size_inflated_bytes() -> u64 {
    OBJECT_SIZE_INFLATED.with(Cell::get)
}

/// Size of a stored object, without reconstructing its body.
///
/// Loose objects are inflated only through the `type size\0` header. A pack
/// entry that is not a delta reports the size in its entry header. An
/// `OFS_DELTA` or `REF_DELTA` entry inflates only the two leading size
/// varints and returns the target size. Inflation stops at 64 KiB.
pub fn object_size(oid: &ObjectHash) -> Result<u64, GitError> {
    OBJECT_SIZE_INFLATED.with(|cell| cell.set(0));
    let objects = utils::path::try_objects().map_err(GitError::IOError)?;
    if let Some(size) = loose_object_size(&objects, oid)? {
        return Ok(size);
    }
    pack_object_size(&objects, oid)?.ok_or_else(|| GitError::ObjectNotFound(oid.to_string()))
}

fn loose_object_size(objects: &Path, oid: &ObjectHash) -> Result<Option<u64>, GitError> {
    let hex = oid.to_string();
    let path = objects.join(&hex[..2]).join(&hex[2..]);
    if !path.is_file() {
        return Ok(None);
    }
    let file = File::open(&path)?;
    let decoder = flate2::read::ZlibDecoder::new(file);
    let mut limited = InflateLimit::new(decoder);
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = limited.read(&mut byte).map_err(|error| {
            GitError::InvalidObjectInfo(format!(
                "could not read the header of loose object {oid}: {error}"
            ))
        })?;
        if read == 0 {
            return Err(GitError::InvalidObjectInfo(format!(
                "loose object {oid} ended before its header"
            )));
        }
        if byte[0] == 0 {
            break;
        }
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).map_err(|_| {
        GitError::InvalidObjectInfo(format!("loose object {oid} has a non-UTF-8 header"))
    })?;
    let size = header
        .split_once(' ')
        .and_then(|(_, size)| size.parse::<u64>().ok())
        .ok_or_else(|| {
            GitError::InvalidObjectInfo(format!("loose object {oid} has no size in its header"))
        })?;
    Ok(Some(size))
}

fn pack_object_size(objects: &Path, oid: &ObjectHash) -> Result<Option<u64>, GitError> {
    let pack_dir = objects.join("pack");
    if !pack_dir.is_dir() {
        return Ok(None);
    }
    let mut indexes = Vec::new();
    for entry in fs::read_dir(&pack_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "idx") {
            indexes.push(path);
        }
    }
    indexes.sort();
    for index in indexes {
        let Some(offset) = lookup_pack_offset(&index, oid)? else {
            continue;
        };
        let pack_path = index.with_extension("pack");
        let mut pack = File::open(&pack_path)?;
        return Ok(Some(read_pack_object_size(&mut pack, offset, oid)?));
    }
    Ok(None)
}

fn lookup_pack_offset(index: &Path, oid: &ObjectHash) -> Result<Option<u64>, GitError> {
    let mut file = File::open(index)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    let version2 = magic == [0xff, 0x74, 0x4f, 0x63];
    if version2 {
        let version = read_be_u32(&mut file)?;
        if version != 2 {
            return Err(GitError::InvalidObjectInfo(format!(
                "unsupported pack index version {version} in {}",
                index.display()
            )));
        }
    } else {
        file.seek(SeekFrom::Start(0))?;
    }
    if !version2 && oid.as_ref().len() != 20 {
        return Ok(None);
    }
    let mut fanout = [0u32; 256];
    for value in &mut fanout {
        *value = read_be_u32(&mut file)?;
    }
    let first = oid.as_ref()[0] as usize;
    let start = if first == 0 {
        0
    } else {
        u64::from(fanout[first - 1])
    };
    let end = u64::from(fanout[first]);
    let hash_len = oid.as_ref().len() as u64;
    let names_at = if version2 { 8 + 256 * 4 } else { 256 * 4 };
    for position in start..end {
        let offset = if version2 {
            file.seek(SeekFrom::Start(names_at + hash_len * position))?;
            let mut name = vec![0u8; hash_len as usize];
            file.read_exact(&mut name)?;
            if name.as_slice() != oid.as_ref() {
                continue;
            }
            read_v2_offset(&mut file, fanout[255], hash_len, position)?
        } else {
            file.seek(SeekFrom::Start(names_at + 24 * position))?;
            let entry_offset = u64::from(read_be_u32(&mut file)?);
            let mut name = [0u8; 20];
            file.read_exact(&mut name)?;
            if name.as_slice() != oid.as_ref() {
                continue;
            }
            entry_offset
        };
        return Ok(Some(offset));
    }
    Ok(None)
}

fn read_v2_offset(
    file: &mut File,
    count: u32,
    hash_len: u64,
    position: u64,
) -> Result<u64, GitError> {
    let names = 8 + 256 * 4;
    let crc = names + u64::from(count) * hash_len;
    let offsets = crc + u64::from(count) * 4;
    file.seek(SeekFrom::Start(offsets + position * 4))?;
    let offset = read_be_u32(file)?;
    if offset & 0x8000_0000 == 0 {
        return Ok(u64::from(offset));
    }
    let large = u64::from(offset & 0x7fff_ffff);
    let large_at = offsets + u64::from(count) * 4 + large * 8;
    file.seek(SeekFrom::Start(large_at))?;
    let mut bytes = [0u8; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_pack_object_size(pack: &mut File, offset: u64, oid: &ObjectHash) -> Result<u64, GitError> {
    pack.seek(SeekFrom::Start(offset))?;
    let (kind, size) = read_pack_type_and_size(pack).map_err(|error| {
        GitError::InvalidObjectInfo(format!("could not read the pack entry for {oid}: {error}"))
    })?;
    match kind {
        1..=4 => Ok(size),
        6 => {
            skip_ofs_distance(pack).map_err(|error| {
                GitError::InvalidObjectInfo(format!(
                    "could not read the OFS_DELTA base of {oid}: {error}"
                ))
            })?;
            read_delta_target_size(pack, oid)
        }
        7 => {
            let hash_len = git_internal::hash::get_hash_kind().size();
            let mut base = vec![0u8; hash_len];
            pack.read_exact(&mut base).map_err(|error| {
                GitError::InvalidObjectInfo(format!(
                    "could not read the REF_DELTA base of {oid}: {error}"
                ))
            })?;
            read_delta_target_size(pack, oid)
        }
        other => Err(GitError::InvalidObjectInfo(format!(
            "pack entry for {oid} has unsupported type {other}"
        ))),
    }
}

fn read_delta_target_size(pack: &mut File, oid: &ObjectHash) -> Result<u64, GitError> {
    let decoder = flate2::read::ZlibDecoder::new(pack);
    let mut limited = InflateLimit::new(decoder);
    let _base = read_delta_varint(&mut limited).map_err(|error| {
        GitError::InvalidObjectInfo(format!(
            "could not read the delta base size of {oid}: {error}"
        ))
    })?;
    read_delta_varint(&mut limited).map_err(|error| {
        GitError::InvalidObjectInfo(format!(
            "could not read the delta target size of {oid}: {error}"
        ))
    })
}

fn read_delta_varint(reader: &mut impl Read) -> io::Result<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let mut byte = [0u8; 1];
        if reader.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "delta size varint ended early",
            ));
        }
        let part = u64::from(byte[0] & 0x7f);
        if shift >= 64 || part > (u64::MAX >> shift) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "delta size varint exceeds u64",
            ));
        }
        value |= part << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

fn read_pack_type_and_size(reader: &mut impl Read) -> io::Result<(u8, u64)> {
    let mut byte = [0u8; 1];
    reader.read_exact(&mut byte)?;
    let kind = (byte[0] >> 4) & 0x07;
    let mut size = u64::from(byte[0] & 0x0f);
    let mut shift = 4u32;
    let mut current = byte[0];
    while current & 0x80 != 0 {
        reader.read_exact(&mut byte)?;
        current = byte[0];
        let part = u64::from(current & 0x7f);
        if shift >= 64 || part > (u64::MAX >> shift) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pack object size exceeds u64",
            ));
        }
        size |= part << shift;
        shift += 7;
    }
    Ok((kind, size))
}

fn skip_ofs_distance(reader: &mut impl Read) -> io::Result<()> {
    let mut byte = [0u8; 1];
    for _ in 0..10 {
        reader.read_exact(&mut byte)?;
        if byte[0] & 0x80 == 0 {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "overlong OFS_DELTA base distance",
    ))
}

fn read_be_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

struct InflateLimit<R> {
    inner: R,
    limit: u64,
}

impl<R> InflateLimit<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            limit: OBJECT_SIZE_INFLATE_LIMIT,
        }
    }
}

impl<R: Read> Read for InflateLimit<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let used = OBJECT_SIZE_INFLATED.with(Cell::get);
        if used >= self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "object inflate exceeded 64 KiB",
            ));
        }
        let room = usize::try_from(self.limit - used).unwrap_or(usize::MAX);
        let max = buf.len().min(room);
        let n = self.inner.read(&mut buf[..max])?;
        if n > 0 {
            OBJECT_SIZE_INFLATED.with(|cell| cell.set(used + u64::try_from(n).unwrap_or(u64::MAX)));
        }
        Ok(n)
    }
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

/// Whether an entry's volatile stat triple equals `metadata`'s. Adapts
/// [`fs::Metadata`] into the shared [`crate::utils::stat_diff`] helper so a
/// pairing this returns `true` for is exactly one status would trust.
fn entry_stat_matches_metadata(
    entry: &git_internal::internal::index::IndexEntry,
    metadata: &fs::Metadata,
) -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    fn stat_times(metadata: &fs::Metadata) -> (SystemTime, SystemTime) {
        use std::{os::unix::fs::MetadataExt, time::Duration};

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

    let (ctime, mtime) = stat_times(metadata);
    crate::utils::stat_diff::entry_stat_matches(entry, ctime, mtime, metadata.len())
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
    let mut hasher = HashAlgorithm::new_for_kind(git_internal::hash::get_hash_kind());

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
    ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &hasher.finalize())
        .map(|oid| (Some(oid), total))
        .map_err(io::Error::other)
}

fn stream_file_blob_hash(path: impl AsRef<Path>) -> io::Result<ObjectHash> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut reader = io::BufReader::new(file);
    let mut hasher = HashAlgorithm::new_for_kind(git_internal::hash::get_hash_kind());

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

    ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &hasher.finalize())
        .map_err(io::Error::other)
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
    #[serial(cwd, env)]
    /// Test objects can be correctly saved to and loaded from storage.
    async fn test_save_load_object() {
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());
        let object = Commit::from_tree_id(
            ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &[1; 20]),
            vec![],
            "\nCommit_1",
        );
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

    const TWO_MIB: usize = 2 * 1024 * 1024;

    fn two_mib_blob() -> Blob {
        Blob::from_content_bytes(vec![0xA5; TWO_MIB])
    }

    fn write_be_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    fn pack_type_and_size(kind: u8, mut size: u64) -> Vec<u8> {
        let mut first = (kind << 4) | ((size as u8) & 0x0f);
        size >>= 4;
        if size > 0 {
            first |= 0x80;
        }
        let mut out = vec![first];
        while size > 0 {
            let mut byte = (size as u8) & 0x7f;
            size >>= 7;
            if size > 0 {
                byte |= 0x80;
            }
            out.push(byte);
        }
        out
    }

    fn zlib_bytes(data: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).expect("compress");
        encoder.finish().expect("finish")
    }

    fn install_v1_pack(entries: &[(ObjectHash, u32)], pack: &[u8]) {
        let objects = crate::utils::path::objects().join("pack");
        std::fs::create_dir_all(&objects).expect("pack dir");
        std::fs::write(objects.join("size-test.pack"), pack).expect("pack");
        let mut sorted = entries.to_vec();
        sorted.sort_by(|left, right| left.0.as_ref().cmp(right.0.as_ref()));
        let mut counts = [0u32; 256];
        for (oid, _) in &sorted {
            counts[oid.as_ref()[0] as usize] += 1;
        }
        let mut fanout = [0u32; 256];
        let mut running = 0u32;
        for index in 0..256 {
            running += counts[index];
            fanout[index] = running;
        }
        let mut index = Vec::new();
        for value in fanout {
            write_be_u32(&mut index, value);
        }
        for (oid, offset) in &sorted {
            write_be_u32(&mut index, *offset);
            index.extend_from_slice(oid.as_ref());
        }
        index.extend_from_slice(&[0u8; 40]);
        std::fs::write(objects.join("size-test.idx"), index).expect("idx");
    }

    #[tokio::test]
    #[serial(cwd, env, hash_kind)]
    async fn object_size_loose_blob() {
        let _kind = git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Sha1);
        let dir = tempdir().expect("tempdir");
        test::setup_with_new_libra_in(dir.path()).await;
        let _guard = test::ChangeDirGuard::new(dir.path());
        let blob = two_mib_blob();
        save_object(&blob, &blob.id).expect("save");
        assert_eq!(object_size(&blob.id).expect("size"), TWO_MIB as u64);
    }

    #[tokio::test]
    #[serial(cwd, env, hash_kind)]
    async fn object_size_loose_inflates_header_only() {
        let _kind = git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Sha1);
        let dir = tempdir().expect("tempdir");
        test::setup_with_new_libra_in(dir.path()).await;
        let _guard = test::ChangeDirGuard::new(dir.path());
        let blob = two_mib_blob();
        save_object(&blob, &blob.id).expect("save");
        let _ = object_size(&blob.id).expect("size");
        assert!(
            object_size_inflated_bytes() <= 65_536,
            "inflated {}",
            object_size_inflated_bytes()
        );
    }

    #[tokio::test]
    #[serial(cwd, env, hash_kind)]
    async fn object_size_packed_full() {
        let _kind = git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Sha1);
        let dir = tempdir().expect("tempdir");
        test::setup_with_new_libra_in(dir.path()).await;
        let _guard = test::ChangeDirGuard::new(dir.path());
        let blob = two_mib_blob();
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        write_be_u32(&mut pack, 2);
        write_be_u32(&mut pack, 1);
        let offset = pack.len() as u32;
        pack.extend(pack_type_and_size(3, TWO_MIB as u64));
        pack.extend(zlib_bytes(&vec![0xA5; TWO_MIB]));
        pack.extend_from_slice(&[0u8; 20]);
        install_v1_pack(&[(blob.id, offset)], &pack);
        assert_eq!(object_size(&blob.id).expect("size"), TWO_MIB as u64);
    }

    fn delta_pack() -> (ObjectHash, Vec<u8>, u32) {
        let mut pack = Vec::new();
        pack.extend_from_slice(b"PACK");
        write_be_u32(&mut pack, 2);
        write_be_u32(&mut pack, 2);
        let base_at = pack.len() as u64;
        pack.extend(pack_type_and_size(3, 1));
        pack.extend(zlib_bytes(b"a"));
        let delta_at = pack.len() as u64;
        let mut delta = Vec::new();
        delta.push(0x01);
        let mut target = TWO_MIB as u64;
        loop {
            let mut byte = (target as u8) & 0x7f;
            target >>= 7;
            if target > 0 {
                byte |= 0x80;
            }
            delta.push(byte);
            if target == 0 {
                break;
            }
        }
        delta.extend_from_slice(&[0x01, b'x']);
        let distance = delta_at - base_at;
        pack.extend(pack_type_and_size(6, delta.len() as u64));
        pack.push(u8::try_from(distance).expect("distance fits in one byte"));
        pack.extend(zlib_bytes(&delta));
        pack.extend_from_slice(&[0u8; 20]);
        let oid = ObjectHash::from_bytes_for_kind(git_internal::hash::HashKind::Sha1, &[0x42; 20])
            .expect("oid");
        (oid, pack, delta_at as u32)
    }

    #[tokio::test]
    #[serial(cwd, env, hash_kind)]
    async fn object_size_packed_delta_reports_target_size() {
        let _kind = git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Sha1);
        let dir = tempdir().expect("tempdir");
        test::setup_with_new_libra_in(dir.path()).await;
        let _guard = test::ChangeDirGuard::new(dir.path());
        let (oid, pack, offset) = delta_pack();
        install_v1_pack(&[(oid, offset)], &pack);
        assert_eq!(object_size(&oid).expect("size"), TWO_MIB as u64);
    }

    #[tokio::test]
    #[serial(cwd, env, hash_kind)]
    async fn object_size_packed_delta_does_not_reconstruct() {
        let _kind = git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Sha1);
        let dir = tempdir().expect("tempdir");
        test::setup_with_new_libra_in(dir.path()).await;
        let _guard = test::ChangeDirGuard::new(dir.path());
        let (oid, pack, offset) = delta_pack();
        install_v1_pack(&[(oid, offset)], &pack);
        let _ = object_size(&oid).expect("size");
        assert!(
            object_size_inflated_bytes() <= 65_536,
            "inflated {}",
            object_size_inflated_bytes()
        );
    }

    #[test]
    fn bounded_read_rejects_large_file() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("big");
        std::fs::write(&file, vec![0xA5; 2 * 1024 * 1024]).expect("write");
        let error = read_worktree_blob_bytes_bounded(&file, 1024 * 1024).expect_err("too large");
        assert!(matches!(error, WorktreeBlobBoundError::TooLarge));
    }

    #[test]
    fn bounded_read_large_file_reads_at_most_limit() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("big");
        std::fs::write(&file, vec![0xA5; 2 * 1024 * 1024]).expect("write");
        let _ = read_worktree_blob_bytes_bounded(&file, 1024 * 1024);
        assert!(read_worktree_blob_bounded_handle_bytes() <= 1_048_577);
    }

    #[test]
    #[serial(env)]
    fn bounded_read_rejects_concurrent_growth() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("grow");
        std::fs::write(&file, vec![0u8; 512 * 1024]).expect("write");
        let pause = dir.path().join("pause");
        let _env = EnvGuard::set(
            "LIBRA_MCP_TEST_BOUNDED_READ_PAUSE_FILE",
            pause.to_string_lossy().as_ref(),
        );
        let _test_env = EnvGuard::set("LIBRA_TEST", "1");
        use std::io::Write as _;
        let reading = file.clone();
        let worker =
            std::thread::spawn(move || read_worktree_blob_bytes_bounded(&reading, 1024 * 1024));
        let started = std::time::Instant::now();
        while !pause.exists() {
            if started.elapsed() > std::time::Duration::from_secs(5) {
                panic!("bounded read did not reach the pause");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut grown = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("append");
        grown
            .write_all(&vec![0u8; 3 * 1024 * 1024 - 512 * 1024])
            .expect("grow");
        drop(grown);
        std::fs::write(format!("{}.go", pause.display()), b"go").expect("go");
        let error = worker.join().expect("join").expect_err("too large");
        assert!(matches!(error, WorktreeBlobBoundError::TooLarge));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_read_symlink_returns_target_bytes() {
        let dir = tempdir().expect("tempdir");
        let outside = dir.path().join("outside");
        std::fs::write(&outside, vec![0x11; 2 * 1024 * 1024]).expect("outside");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        let bounded = read_worktree_blob_bytes_bounded(&link, 1024 * 1024).expect("link bytes");
        let plain = read_worktree_blob_bytes(&link).expect("plain");
        assert_eq!(bounded, plain);
        assert_ne!(bounded, vec![0x11; 2 * 1024 * 1024]);
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: the test serializes env changes and restores the previous value.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                unsafe { std::env::set_var(self.key, previous) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
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
