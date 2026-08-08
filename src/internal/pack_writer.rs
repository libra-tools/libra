//! The single on-disk pack writer.
//!
//! Encodes a set of objects into a **valid** `pack-<checksum>.pack` file (plus
//! its matching `pack-<checksum>.idx`) under `objects/pack/`, using
//! `git-internal`'s [`PackEncoder`]. Every on-disk pack Libra writes — the
//! `maintenance` gc / incremental-repack tasks, the `repack` command, and the
//! hidden `pack-objects` command — goes through here so there is exactly one
//! pack encoder rather than several hand-rolled ones.
//!
//! This deliberately mirrors the wire encoder in
//! [`crate::internal::protocol::local_client`]: both drive the same
//! `PackEncoder`, but that one frames the pack bytes into a sideband fetch
//! response while this one writes a file and generates the index. Keeping the
//! two separate is intentional — they have different output sinks — but neither
//! re-implements the pack format itself.
//!
//! # Correctness notes
//!
//! - The pack trailer is the checksum of the whole pack stream, computed by the
//!   encoder. Earlier hand-rolled writers hashed each object's *object id* into
//!   the trailer instead of the pack bytes, producing packs that failed
//!   `index-pack` verification; routing everything through `PackEncoder` fixes
//!   that.
//! - `PackEncoder::new` seeds its trailer hasher from the thread-local hash kind
//!   *at construction*, so it is built inside the spawned task **after**
//!   `set_hash_kind`, and the kind is threaded in explicitly rather than read
//!   from a thread-local that may not survive an `.await`.

use std::{
    io,
    path::{Path, PathBuf},
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

use crate::{command::index_pack::build_index_v2, utils::client_storage::ClientStorage};

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
/// a file can share the one encoder. `hash_kind` is passed explicitly because
/// `PackEncoder` reads the thread-local hash kind when it is constructed, and a
/// Tokio worker thread may not carry the kind set before this `.await`.
pub async fn encode_pack_bytes(entries: Vec<Entry>, hash_kind: HashKind) -> io::Result<Vec<u8>> {
    let (entry_tx, entry_rx) = tokio::sync::mpsc::channel::<MetaAttached<Entry, EntryMeta>>(1_000);
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(1_000);

    let total_objects = entries.len();
    let encode_handle = tokio::spawn(async move {
        // Seed the encoder's trailer hasher from the repository's hash kind
        // before constructing it — see the module docs.
        set_hash_kind(hash_kind);
        let mut encoder = PackEncoder::new(total_objects, 0, stream_tx);
        encoder.encode(entry_rx).await
    });

    // Feed entries from a dedicated task so the output channel below is drained
    // concurrently. If this function instead queued every entry before draining,
    // a large object set could fill the bounded output channel — blocking the
    // encoder mid-encode — while this side is still blocked sending into the
    // (also bounded) input channel: a deadlock.
    let feed_handle = tokio::spawn(async move {
        for entry in entries {
            let meta_entry = MetaAttached {
                inner: entry,
                meta: EntryMeta::default(),
            };
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
    if hashes.is_empty() {
        return Ok(None);
    }
    let mut entries = Vec::with_capacity(hashes.len());
    for hash in hashes {
        entries.push(entry_from_storage(storage, hash)?);
    }
    Ok(Some(encode_pack_bytes(entries, hash_kind).await?))
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

pub async fn write_pack_with_index(
    storage: &ClientStorage,
    hashes: &[ObjectHash],
    pack_dir: &Path,
    hash_kind: HashKind,
) -> io::Result<Option<PackPublication>> {
    let Some(pack_bytes) = encode_hashes_to_pack(storage, hashes, hash_kind).await? else {
        return Ok(None);
    };

    // The trailer is the last `hash_kind.size()` bytes of the stream.
    let checksum_len = hash_kind.size();
    if pack_bytes.len() < checksum_len {
        return Err(io::Error::other(
            "pack encoder produced a stream shorter than its trailer",
        ));
    }
    let checksum = &pack_bytes[pack_bytes.len() - checksum_len..];
    let name = format!("pack-{}", hex::encode(checksum));

    std::fs::create_dir_all(pack_dir)?;
    let pack_path = pack_dir.join(format!("{name}.pack"));
    let index_path = pack_dir.join(format!("{name}.idx"));
    std::fs::write(&pack_path, &pack_bytes)?;

    let pack_str = pack_path
        .to_str()
        .ok_or_else(|| io::Error::other("pack path is not valid UTF-8"))?;
    let index_str = index_path
        .to_str()
        .ok_or_else(|| io::Error::other("index path is not valid UTF-8"))?;
    build_index_v2(pack_str, index_str)
        .map_err(|error| io::Error::other(format!("failed to index new pack: {error}")))?;

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

    Ok(Some(PackPublication {
        path: pack_path,
        durable,
    }))
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
