//! The single on-disk pack writer.
//!
//! Encodes a set of objects into a **valid** `pack-<checksum>.pack` file (plus
//! its matching `pack-<checksum>.idx`) under `objects/pack/`, using
//! `git-internal`'s [`PackEncoder`]. Every on-disk pack Libra writes — the
//! `maintenance` gc / incremental-repack tasks, the `repack` command, and the
//! hidden `pack-objects` command — goes through here so there is exactly one
//! pack encoder rather than several hand-rolled ones.
//!
//! The byte-level encoder here is also the single source of truth for the
//! local-path fetch path: [`crate::internal::protocol::local_client`] frames
//! these same bytes into a sideband fetch response (ADR-CLH-01 / issues/496),
//! so there is exactly one `PackEncoder` path and no hand-rolled body that can
//! drift or deadlock. The caller's sink differs (write a file + index vs frame
//! into a sideband stream), but no caller re-implements the pack format.
//!
//! # Correctness notes
//!
//! - The pack trailer is the checksum of the whole pack stream, computed by the
//!   encoder. Earlier hand-rolled writers hashed each object's *object id* into
//!   the trailer instead of the pack bytes, producing packs that failed
//!   `index-pack` verification; routing everything through `PackEncoder` fixes
//!   that.
//! - `PackEncoder::new_with_hash_kind` takes the repository hash kind explicitly
//!   so the trailer hasher does not depend on a Tokio worker's thread-local.

use std::{
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, set_hash_kind},
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::{
            ObjectTrait, blob::Blob, commit::Commit, tag::Tag, tree::Tree, types::ObjectType,
        },
        pack::{encode::PackEncoder, entry::Entry},
    },
};

use crate::{
    command::index_pack::{build_index_v1, build_index_v2},
    utils::client_storage::ClientStorage,
};

/// Git's default `pack.window` — the delta-compression window size, in objects
/// (0 disables delta compression entirely). Shared by `repack`, `gc`'s
/// aggressive mapping, and the `maintenance` pack writers (ADR-MX-09).
pub const DEFAULT_WINDOW: usize = 10;

/// Git's default `pack.depth` — the maximum delta chain depth. Validated and
/// reported by `repack`, but the upstream git-internal encoder applies its own
/// fixed chain cap (`MAX_CHAIN_LEN = 50`) regardless of the requested value
/// (ER-MX-03 upstream dependency, `docs/development/plan/issues/481.md`).
pub const DEFAULT_DEPTH: usize = 50;

/// Load one object from storage and wrap it as a pack [`Entry`].
///
/// Reads the object body and its type, then reconstructs the typed object so the
/// encoder can re-serialise it. An object whose type is not one of the four Git
/// object kinds (e.g. an OFS/REF delta placeholder) cannot be packed directly
/// and is reported as an error rather than silently dropped.
fn entry_from_storage(storage: &ClientStorage, hash: &ObjectHash) -> io::Result<Entry> {
    let data = storage
        .get(hash)
        .map_err(|error| io::Error::other(format!("read object {hash}: {error}")))?;
    let object_type = storage
        .get_object_type(hash)
        .map_err(|error| io::Error::other(format!("object type of {hash}: {error}")))?;
    let to_io = |error: GitError| io::Error::other(format!("decode object {hash}: {error}"));
    let entry = match object_type {
        ObjectType::Commit => Entry::from(Commit::from_bytes(&data, *hash).map_err(to_io)?),
        ObjectType::Tree => Entry::from(Tree::from_bytes(&data, *hash).map_err(to_io)?),
        ObjectType::Blob => Entry::from(Blob::from_bytes(&data, *hash).map_err(to_io)?),
        ObjectType::Tag => Entry::from(Tag::from_bytes(&data, *hash).map_err(to_io)?),
        other => {
            return Err(io::Error::other(format!(
                "cannot pack object {hash} of type {other:?}"
            )));
        }
    };
    Ok(entry)
}

/// Encode already-loaded entries into the raw bytes of a pack stream.
///
/// Public so both the disk path (below) and callers that need the bytes without
/// a file can share the one encoder. `hash_kind` is passed to
/// [`PackEncoder::new_with_hash_kind`] so a Tokio worker thread does not need a
/// matching thread-local.
pub async fn encode_pack_bytes(entries: Vec<Entry>, hash_kind: HashKind) -> io::Result<Vec<u8>> {
    encode_pack_bytes_with_window(entries, hash_kind, 0).await
}

/// [`encode_pack_bytes`] with an explicit delta-compression window.
///
/// `window == 0` selects independent zlib encoding (no delta objects); a
/// non-zero window enables the encoder's sliding-window delta search with the
/// given candidate-window size. The upstream encoder applies a fixed delta
/// chain cap of 50 (git-internal `MAX_CHAIN_LEN`) regardless of any depth
/// value, so depth capping is an upstream dependency (ER-MX-03 in
/// `docs/development/plan/issues/481.md`); packs stay valid either way.
pub async fn encode_pack_bytes_with_window(
    entries: Vec<Entry>,
    hash_kind: HashKind,
    window: usize,
) -> io::Result<Vec<u8>> {
    let (entry_tx, entry_rx) = tokio::sync::mpsc::channel::<MetaAttached<Entry, EntryMeta>>(1_000);
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(1_000);

    let total_objects = entries.len();
    let encode_handle = tokio::spawn(async move {
        let mut encoder =
            PackEncoder::new_with_hash_kind(hash_kind, total_objects, window, stream_tx);
        encoder.encode(entry_rx).await
    });

    // Feed entries from a dedicated task so the output channel below is drained
    // concurrently. If this function instead queued every entry before draining,
    // a large object set could fill the bounded output channel — blocking the
    // encoder mid-encode — while this side is still blocked sending into the
    // (also bounded) input channel: a deadlock.
    let feed_handle = tokio::spawn(async move {
        for (i, entry) in entries.into_iter().enumerate() {
            // Upstream git-internal's `magic_sort` computes `content_sort_key`
            // for path-less entries, shifting the first 8 bytes into a u64 by
            // `(8 - len) * 8` — 64 for a 0-byte payload, i.e. a left-shift
            // overflow panic in overflow-checked builds. Empty blobs and trees
            // are common, so with deltas enabled they get a synthetic path
            // that routes them into the path-aware ordering branch instead.
            // The path is sort metadata only — it is never written into the
            // pack — and empty payloads can neither serve as nor become delta
            // objects (the similarity pre-filter rejects size-0 pairs).
            let meta = if window > 0 && entry.data.is_empty() {
                EntryMeta {
                    file_path: Some(format!("empty/{i}")),
                    ..EntryMeta::default()
                }
            } else {
                EntryMeta::default()
            };
            let meta_entry = MetaAttached { inner: entry, meta };
            if entry_tx.send(meta_entry).await.is_err() {
                break; // the encoder went away; stop feeding
            }
        }
        // `entry_tx` is dropped here, signalling end-of-input to the encoder.
    });

    let mut pack_data = Vec::new();
    while let Some(chunk) = stream_rx.recv().await {
        pack_data.extend(chunk);
    }

    feed_handle
        .await
        .map_err(|error| io::Error::other(format!("pack feed task panicked: {error}")))?;
    encode_handle
        .await
        .map_err(|error| io::Error::other(format!("pack encode task panicked: {error}")))?
        .map_err(|error| io::Error::other(format!("pack encoding failed: {error}")))?;
    Ok(pack_data)
}

/// Encode the objects named by `hashes` into the raw bytes of a pack stream.
///
/// Returns `Ok(None)` when `hashes` is empty (`PackEncoder` cannot encode a
/// zero-object pack). This is the shared front door used by both the on-disk
/// writer below and callers that want the bytes directly (e.g. `pack-objects
/// --stdout`).
pub async fn encode_hashes_to_pack(
    storage: &ClientStorage,
    hashes: &[ObjectHash],
    hash_kind: HashKind,
) -> io::Result<Option<Vec<u8>>> {
    encode_hashes_to_pack_with_window(storage, hashes, hash_kind, 0).await
}

/// [`encode_hashes_to_pack`] with an explicit delta-compression window.
pub async fn encode_hashes_to_pack_with_window(
    storage: &ClientStorage,
    hashes: &[ObjectHash],
    hash_kind: HashKind,
    window: usize,
) -> io::Result<Option<Vec<u8>>> {
    if hashes.is_empty() {
        return Ok(None);
    }
    let mut entries = Vec::with_capacity(hashes.len());
    for hash in hashes {
        entries.push(entry_from_storage(storage, hash)?);
    }
    Ok(Some(
        encode_pack_bytes_with_window(entries, hash_kind, window).await?,
    ))
}

/// Encode `hashes` into a new pack under `pack_dir`, writing both the `.pack`
/// and its `.idx`.
///
/// The pack is named `pack-<trailer-checksum>` after its own trailing checksum,
/// matching Git's on-disk convention and guaranteeing the `.pack`/`.idx` pair
/// share a stable, content-derived name. Returns the written `.pack` path, or
/// `Ok(None)` when `hashes` is empty (`PackEncoder` cannot encode a zero-object
/// pack, and an empty pack would be pointless on disk).
/// A pack that was written, and whether its NAME is durable.
///
/// `durable` is `true` only when the directory entries naming the pack and
/// its index were successfully `fsync`ed. Callers that go on to delete the
/// objects' other copy (`repack -d`, the `loose-objects` maintenance task,
/// old-pack deletion) must refuse when it is `false`: file contents that
/// survive a crash under a name that does not are not a copy.
#[derive(Debug, Clone)]
pub struct PackPublication {
    pub path: PathBuf,
    pub durable: bool,
}

/// Serialize publication of one content-addressed pack with fetch installers.
/// The file stays in place after unlock so other users in a shared repository
/// can open it read-only even when they cannot create or write it.
struct PackInstallLock {
    _file: fs::File,
}

impl PackInstallLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match try_lock_pack_install(path)? {
                Some(file) => return Ok(Self { _file: file }),
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out waiting to install pack '{}'; retry after the other writer finishes",
                            path.display()
                        ),
                    ));
                }
            }
        }
    }
}

#[cfg(unix)]
fn try_lock_pack_install(path: &Path) -> io::Result<Option<fs::File>> {
    use std::os::fd::AsRawFd;

    let file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => fs::File::open(path)?,
        Err(error) => return Err(error),
    };
    // SAFETY: the descriptor remains open for the lifetime of PackInstallLock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(error),
    }
}

#[cfg(windows)]
fn try_lock_pack_install(path: &Path) -> io::Result<Option<fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;

    let open = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::OpenOptions::new().read(true).share_mode(0).open(path)
        }
        Err(error) => Err(error),
    };
    match open {
        Ok(file) => Ok(Some(file)),
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn try_lock_pack_install(_path: &Path) -> io::Result<Option<fs::File>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cross-process pack installation locking is unsupported on this platform",
    ))
}

fn file_matches_bytes(path: &Path, expected: &[u8]) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    if file.metadata()?.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut buffer = [0_u8; 64 * 1024];
    for chunk in expected.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])?;
        if buffer[..chunk.len()] != *chunk {
            return Ok(false);
        }
    }
    Ok(true)
}

fn files_are_identical(left: &Path, right: &Path) -> io::Result<bool> {
    let mut left_file = fs::File::open(left)?;
    let mut right_file = fs::File::open(right)?;
    let length = left_file.metadata()?.len();
    if length != right_file.metadata()?.len() {
        return Ok(false);
    }
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    let mut remaining = length;
    while remaining > 0 {
        let size = remaining.min(left_buffer.len() as u64) as usize;
        left_file.read_exact(&mut left_buffer[..size])?;
        right_file.read_exact(&mut right_buffer[..size])?;
        if left_buffer[..size] != right_buffer[..size] {
            return Ok(false);
        }
        remaining -= size as u64;
    }
    Ok(true)
}

fn existing_index_is_v1(path: &Path, hash_kind: HashKind) -> io::Result<bool> {
    let mut header = [0_u8; 8];
    fs::File::open(path)?.read_exact(&mut header)?;
    if header[..4] == [0xff, b't', b'O', b'c'] {
        if header[4..] != 2_u32.to_be_bytes() {
            return Err(io::Error::other(format!(
                "existing pack index '{}' uses an unsupported version",
                path.display()
            )));
        }
        return Ok(false);
    }
    if hash_kind != HashKind::Sha1 {
        return Err(io::Error::other(format!(
            "existing pack index '{}' has no v2 header for a non-SHA-1 repository",
            path.display()
        )));
    }
    Ok(true)
}

pub async fn write_pack_with_index(
    storage: &ClientStorage,
    hashes: &[ObjectHash],
    pack_dir: &Path,
    hash_kind: HashKind,
) -> io::Result<Option<PackPublication>> {
    write_pack_with_index_with_window(storage, hashes, pack_dir, hash_kind, 0).await
}

/// [`write_pack_with_index`] with an explicit delta-compression window
/// (0 = no deltas). `repack` forwards the resolved `--window` value here.
pub async fn write_pack_with_index_with_window(
    storage: &ClientStorage,
    hashes: &[ObjectHash],
    pack_dir: &Path,
    hash_kind: HashKind,
    window: usize,
) -> io::Result<Option<PackPublication>> {
    let Some(pack_bytes) =
        encode_hashes_to_pack_with_window(storage, hashes, hash_kind, window).await?
    else {
        return Ok(None);
    };

    publish_pack_bytes(&pack_bytes, pack_dir, hash_kind).map(Some)
}

fn publish_pack_bytes(
    pack_bytes: &[u8],
    pack_dir: &Path,
    hash_kind: HashKind,
) -> io::Result<PackPublication> {
    // The trailer is the last `hash_kind.size()` bytes of the stream.
    let checksum_len = hash_kind.size();
    if pack_bytes.len() < checksum_len {
        return Err(io::Error::other(
            "pack encoder produced a stream shorter than its trailer",
        ));
    }
    let checksum = &pack_bytes[pack_bytes.len() - checksum_len..];
    let name = format!("pack-{}", hex::encode(checksum));

    fs::create_dir_all(pack_dir)?;
    let pack_path = pack_dir.join(format!("{name}.pack"));
    let index_path = pack_dir.join(format!("{name}.idx"));
    let lock_path = pack_dir.join(format!("{name}.install.lock"));
    // Acquire before the final pack name becomes visible. Readers treat a
    // pack without an index as pending only while this same lock is held.
    let _install_lock = PackInstallLock::acquire(&lock_path)?;
    let pack_exists = pack_path.try_exists()?;
    let index_exists = index_path.try_exists()?;
    if pack_exists != index_exists {
        let repair = if pack_exists {
            format!(
                "run 'libra index-pack {}' to rebuild its index",
                pack_path.display()
            )
        } else {
            "restore the missing pack or remove the orphan index".to_string()
        };
        return Err(io::Error::other(format!(
            "pack '{}' and index '{}' are incomplete; {repair}, then retry",
            pack_path.display(),
            index_path.display(),
        )));
    }
    if pack_exists && !file_matches_bytes(&pack_path, pack_bytes)? {
        return Err(io::Error::other(format!(
            "existing pack '{}' differs from encoded contents; repair the object store before retrying",
            pack_path.display()
        )));
    }

    // Stage both complete files in a private directory on the same filesystem.
    // create_new/File::create retain the repository's umask-derived file mode,
    // and hard_link publishes each name without replacing another writer's
    // already visible file. A failed second publication leaves a complete
    // orphan pack for explicit repair; it is never unlinked under readers.
    {
        let staging = tempfile::tempdir_in(pack_dir)?;
        let staged_pack = staging.path().join(format!("{name}.pack"));
        let staged_index = staging.path().join(format!("{name}.idx"));
        let source_pack = if pack_exists {
            &pack_path
        } else {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged_pack)?;
            file.write_all(pack_bytes)?;
            file.sync_all()?;
            &staged_pack
        };
        let pack_str = source_pack
            .to_str()
            .ok_or_else(|| io::Error::other("pack path is not valid UTF-8"))?;
        let index_str = staged_index
            .to_str()
            .ok_or_else(|| io::Error::other("index path is not valid UTF-8"))?;
        // The async encoder may resume on a different worker thread. Index
        // decoding also consults thread-local hash state on this thread.
        set_hash_kind(hash_kind);
        let use_v1 = index_exists && existing_index_is_v1(&index_path, hash_kind)?;
        if use_v1 {
            build_index_v1(pack_str, index_str)
        } else {
            build_index_v2(pack_str, index_str)
        }
        .map_err(|error| io::Error::other(format!("failed to index pack: {error}")))?;
        if index_exists {
            if !files_are_identical(&staged_index, &index_path)? {
                return Err(io::Error::other(format!(
                    "existing pack index '{}' does not match the pack; repair the object store before retrying",
                    index_path.display()
                )));
            }
        } else {
            fsync_path(&staged_index)?;
            fs::hard_link(&staged_pack, &pack_path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("failed to publish pack '{}': {error}", pack_path.display()),
                )
            })?;
            fs::hard_link(&staged_index, &index_path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "failed to publish index '{}' for pack '{}': {error}; run 'libra index-pack {}' to repair before retrying",
                        index_path.display(),
                        pack_path.display(),
                        pack_path.display()
                    ),
                )
            })?;
        }
    }

    // The pack becomes the ONLY home of the objects it holds: `repack -d` and
    // the `loose-objects` maintenance task unlink the loose copies right
    // after this returns. A pack that exists only in the page cache is not a
    // copy — a power loss between the write and the unlink would take
    // reachable objects with it. So the pack, its index, and the directory
    // entries naming them are made durable HERE, where the callers cannot
    // forget to, rather than at each deletion site.
    //
    // File contents first, then the directory: syncing the entry before the
    // bytes it names would make a name durable for data that is not.
    fsync_path(&pack_path)?;
    fsync_path(&index_path)?;

    // IN-06: Adjust pack file, index, and parent pack directory permissions in shared repositories
    if let Ok(Some(shared_mode)) = crate::utils::shared_perm::get_shared_mode_for_path(&pack_path) {
        let _ = crate::utils::shared_perm::adjust_shared_perm_for_pack(&pack_path, &shared_mode);
    }
    // A directory sync that cannot be PROVEN is reported, not swallowed. The
    // file data is durable either way, so creating a pack stays allowed; what
    // is not allowed is treating an unproven directory entry as licence to
    // delete the only other copy of those objects, which is the caller's
    // decision to make and now the caller's information to make it with.
    let durable = fsync_dir(pack_dir)?;
    // The PARENT is synced unconditionally, not only when this call created
    // `objects/pack`. `init` precreates that directory, so a "created here"
    // condition is false on every normal repository — and an entry that has
    // never been synced is exactly as losable whether this process made it or
    // an earlier one did. The cost is one more directory fsync per pack.
    let durable = durable
        && match pack_dir.parent() {
            Some(parent) => fsync_dir(parent)?,
            None => true,
        };

    Ok(PackPublication {
        path: pack_path,
        durable,
    })
}

/// `fsync` one file by path.
fn fsync_path(path: &std::path::Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// `fsync` a directory so the entries created inside it survive a crash.
///
/// Returns whether durability was actually PROVEN. Opening a directory for
/// reading and syncing it is the POSIX way to make its entries durable; where
/// the platform refuses (Windows, some network filesystems) the result is
/// `false`, not an error — the pack's contents are durable regardless, so
/// creating it stays allowed. Only deletion of the objects' other copy
/// requires the `true`.
fn fsync_dir(dir: &std::path::Path) -> io::Result<bool> {
    // Every "this platform or filesystem will not sync a directory" answer is
    // `false`, never an error: a pack whose CONTENTS are durable is still a
    // valid pack, and failing the write would break `pack-objects` and every
    // other non-deleting caller on a filesystem that simply does not support
    // the operation. Only deletion requires the `true`.
    match std::fs::File::open(dir) {
        Ok(handle) => match handle.sync_all() {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput
                        | io::ErrorKind::Unsupported
                        | io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        },
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied
                    | io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use git_internal::{
        hash::{HashKind, ObjectHash, set_hash_kind},
        internal::{object::blob::Blob, pack::entry::Entry},
    };

    use super::{
        PackInstallLock, encode_pack_bytes, encode_pack_bytes_with_window, publish_pack_bytes,
        try_lock_pack_install,
    };

    fn valid_pack_bytes() -> Vec<u8> {
        set_hash_kind(HashKind::Sha1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime
            .block_on(encode_pack_bytes(
                vec![Entry::from(Blob::from_content("pack publication"))],
                HashKind::Sha1,
            ))
            .expect("encode valid test pack")
    }

    #[test]
    fn pack_install_lock_serializes_writers() {
        let dir = tempfile::tempdir().expect("temporary pack directory");
        let path = dir.path().join("pack-test.install.lock");
        let first = PackInstallLock::acquire(&path).expect("first writer lock");
        assert!(
            try_lock_pack_install(&path)
                .expect("probe contended lock")
                .is_none()
        );
        drop(first);
        assert!(
            try_lock_pack_install(&path)
                .expect("probe released lock")
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn pack_install_lock_reopens_existing_read_only_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temporary pack directory");
        let path = dir.path().join("pack-test.install.lock");
        drop(PackInstallLock::acquire(&path).expect("create lock file"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
            .expect("make lock file read-only");
        let lock = PackInstallLock::acquire(&path).expect("lock existing read-only file");
        assert!(
            try_lock_pack_install(&path)
                .expect("probe contended lock")
                .is_none()
        );
        drop(lock);
    }

    #[test]
    fn existing_pack_and_v2_index_are_reused_without_overwrite() {
        let bytes = valid_pack_bytes();
        let dir = tempfile::tempdir().expect("temporary pack directory");
        let first =
            publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).expect("publish pack and index");
        let index = first.path.with_extension("idx");
        let index_bytes = std::fs::read(&index).expect("read published index");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&first.path, std::fs::Permissions::from_mode(0o444))
                .expect("make published pack read-only");
            std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o444))
                .expect("make published index read-only");
        }
        let second = publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1)
            .expect("reuse existing complete pair");
        assert_eq!(first.path, second.path);
        assert_eq!(std::fs::read(&first.path).expect("read pack"), bytes);
        assert_eq!(std::fs::read(&index).expect("read index"), index_bytes);
    }

    #[test]
    fn existing_sha1_v1_index_is_accepted_when_it_matches_pack() {
        let bytes = valid_pack_bytes();
        let dir = tempfile::tempdir().expect("temporary pack directory");
        let first =
            publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).expect("publish pack and index");
        let index = first.path.with_extension("idx");
        let v1 = dir.path().join("index-v1.tmp");
        crate::command::index_pack::build_index_v1(
            first.path.to_str().expect("pack path"),
            v1.to_str().expect("temporary index path"),
        )
        .expect("build v1 index");
        std::fs::remove_file(&index).expect("remove v2 index");
        std::fs::rename(&v1, &index).expect("replace index with valid v1");
        publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).expect("reuse valid v1 index");
    }

    #[test]
    fn incomplete_or_conflicting_published_pair_fails_closed() {
        let bytes = valid_pack_bytes();
        let dir = tempfile::tempdir().expect("temporary pack directory");
        let first =
            publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).expect("publish pack and index");
        let index = first.path.with_extension("idx");
        let original_index = std::fs::read(&index).expect("read index");

        std::fs::remove_file(&index).expect("make orphan pack");
        assert!(publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).is_err());
        assert_eq!(std::fs::read(&first.path).expect("read orphan"), bytes);

        std::fs::write(&index, &original_index).expect("restore index");
        std::fs::write(&first.path, b"corrupt pack").expect("corrupt pack");
        assert!(publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).is_err());
        assert_eq!(
            std::fs::read(&first.path).expect("read conflict"),
            b"corrupt pack"
        );

        std::fs::write(&first.path, &bytes).expect("restore pack");
        let mut corrupt_index = original_index;
        let last = corrupt_index.last_mut().expect("nonempty index");
        *last ^= 1;
        std::fs::write(&index, &corrupt_index).expect("corrupt index");
        assert!(publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).is_err());
        assert_eq!(
            std::fs::read(&index).expect("read index conflict"),
            corrupt_index
        );
    }

    #[test]
    fn index_failure_leaves_no_public_pack() {
        let mut bytes = valid_pack_bytes();
        bytes[..4].copy_from_slice(b"FAIL");
        let checksum = hex::encode(&bytes[bytes.len() - HashKind::Sha1.size()..]);
        let dir = tempfile::tempdir().expect("temporary pack directory");
        assert!(publish_pack_bytes(&bytes, dir.path(), HashKind::Sha1).is_err());
        assert!(!dir.path().join(format!("pack-{checksum}.pack")).exists());
        assert!(!dir.path().join(format!("pack-{checksum}.idx")).exists());
    }

    /// A pack entry for the canonical empty tree (0-byte payload).
    pub(super) fn empty_tree_entry() -> Entry {
        use git_internal::internal::object::{ObjectTrait, tree::Tree};
        let hash = ObjectHash::from_bytes(&[0u8; 20]).expect("zero placeholder sha1 hash");
        Entry::from(Tree::from_bytes(&[], hash).expect("empty tree parses"))
    }

    /// A non-zero window enables delta compression: several near-identical
    /// blobs encode to fewer bytes than the same set at window 0 (independent
    /// zlib). This pins the window forwarding contract (ADR-MX-09): the window
    /// value reaches the encoder rather than always selecting the no-delta
    /// path.
    #[test]
    fn nonzero_window_enables_delta_compression() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        // Long common prefix with a tiny per-object tail: a delta base can
        // represent ~99% of each sibling, so window > 0 must select deltas.
        let contents: Vec<String> = (0..6)
            .map(|i| {
                let mut body = String::new();
                for _ in 0..120 {
                    body.push_str("the quick brown fox jumps over the lazy dog\n");
                }
                body.push_str(&format!("unique tail marker {i}\n"));
                body
            })
            .collect();
        let build = |contents: &[String]| {
            contents
                .iter()
                .map(|content| Entry::from(Blob::from_content(content)))
                .collect::<Vec<_>>()
        };

        let no_delta = runtime
            .block_on(encode_pack_bytes_with_window(
                build(&contents),
                HashKind::Sha1,
                0,
            ))
            .expect("window 0 encode");
        let delta = runtime
            .block_on(encode_pack_bytes_with_window(
                build(&contents),
                HashKind::Sha1,
                10,
            ))
            .expect("window 10 encode");

        assert!(
            no_delta.len() > delta.len(),
            "window > 0 must delta-compress near-identical blobs (no-delta {} bytes vs delta {} bytes)",
            no_delta.len(),
            delta.len()
        );
        assert_eq!(&no_delta[..4], b"PACK");
        assert_eq!(&delta[..4], b"PACK");
    }

    /// Empty blobs/trees must not crash the windowed encoder. Upstream
    /// git-internal's `magic_sort` computes `content_sort_key` for path-less
    /// entries with a left shift of `(8 - len) * 8` — 64 for a 0-byte payload,
    /// a shift-overflow panic in overflow-checked builds. The writer gives
    /// empty payloads a synthetic path when deltas are enabled, so the pack
    /// encodes cleanly and still delta-compresses its non-empty neighbors.
    #[test]
    fn windowed_encoding_handles_empty_objects_without_panic() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        let mut contents: Vec<String> = (0..6)
            .map(|i| {
                let mut body = String::new();
                for _ in 0..120 {
                    body.push_str("the quick brown fox jumps over the lazy dog\n");
                }
                body.push_str(&format!("unique tail marker {i}\n"));
                body
            })
            .collect();
        // The canonical empty blob and an empty tree: common in real history.
        contents.push(String::new());
        contents.push(String::new());
        let mut entries: Vec<Entry> = contents
            .iter()
            .map(|content| Entry::from(Blob::from_content(content)))
            .collect();
        let empty_tree = crate::internal::pack_writer::tests::empty_tree_entry();
        entries.push(empty_tree);

        let no_delta = runtime
            .block_on(encode_pack_bytes_with_window(
                entries.clone(),
                HashKind::Sha1,
                0,
            ))
            .expect("window 0 encode with empty blobs");
        let delta = runtime
            .block_on(encode_pack_bytes_with_window(entries, HashKind::Sha1, 10))
            .expect("window 10 encode with empty blobs must not panic");

        assert!(
            no_delta.len() > delta.len(),
            "window > 0 must still delta-compress the non-empty blobs"
        );
        assert_eq!(&delta[..4], b"PACK");
    }

    /// Regression guard for ADR-CLH-01 / ADR-IG-01: encoding more entries than
    /// the sum of the two bounded channel capacities (1_000 + 1_000 = 2_000)
    /// must NOT deadlock. The pre-fix in-house copy fed every entry into the
    /// bounded input channel before draining the bounded output channel; past
    /// ~2 000 objects the encoder blocked writing to the full output channel
    /// while the feeder blocked sending into the full input channel — a circular
    /// wait that hung the local-path clone at "Fetching objects" (0% CPU).
    /// Wrap the encode in `tokio::time::timeout` so this test itself fails
    /// rather than hanging if the deadlock regresses.
    #[test]
    fn encode_pack_bytes_does_not_deadlock_above_channel_capacity() {
        use tokio::time::{Duration, timeout};

        set_hash_kind(HashKind::Sha1);
        // Distinct contents so the encoder sees unique objects (no spurious
        // dedup masking the channel behaviour).
        let entries: Vec<Entry> = (0..2_500)
            .map(|i| {
                let content = format!("blob-content-{i}");
                Entry::from(Blob::from_content(&content))
            })
            .collect();
        assert!(entries.len() > 2_000, "test must exceed channel capacities");

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let pack = timeout(
                Duration::from_secs(30),
                encode_pack_bytes(entries, HashKind::Sha1),
            )
            .await
            .expect("encode_pack_bytes must not deadlock")
            .expect("encode large pack set");
            // A real v2 pack starts with the PACK signature.
            assert!(pack.len() >= 12, "pack has signature + header");
            assert_eq!(&pack[..4], b"PACK", "pack signature");
        });
    }
}
