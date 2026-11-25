use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use byteorder::{BigEndian, WriteBytesExt};
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::types::ObjectType,
        pack::{Pack, entry::Entry},
    },
};
use sha1::{Digest, Sha1};

use crate::{
    command::index_pack_support::{
        PackCommitEdges, ReceivedObjects, index_write_error, lock_state, record_first_pack_error,
        take_arc_mutex,
    },
    utils::client_storage::parse_commit_header_refs,
};

pub fn build_index_v1(pack_file: &str, index_file: &str) -> Result<(), GitError> {
    build_index_v1_inner(pack_file, index_file, false, None).map(|_| ())
}

pub(crate) fn build_index_v1_inner(
    pack_file: &str,
    index_file: &str,
    collect_edges: bool,
    received: Option<Arc<ReceivedObjects>>,
) -> Result<Option<PackCommitEdges>, GitError> {
    if git_internal::hash::get_hash_kind() != HashKind::Sha1 {
        return Err(GitError::InvalidPackFile(
            "Index version 1 only supports SHA-1 hash".to_string(),
        ));
    }
    let pack_path = PathBuf::from(pack_file);
    let tmp_path = pack_path.parent().ok_or_else(|| {
        GitError::InvalidArgument(format!("invalid pack file path: '{pack_file}'"))
    })?;
    let spool_dir = if tmp_path.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        tmp_path
    };
    let pack_file = std::fs::File::open(pack_file)?;
    let mut pack_reader = std::io::BufReader::new(pack_file);
    let obj_map = Arc::new(Mutex::new(BTreeMap::new()));
    let obj_map_c = obj_map.clone();
    let err = Arc::new(Mutex::new(None));
    let err_c = err.clone();
    let commit_edges = collect_edges
        .then(|| PackCommitEdges::new(spool_dir, HashKind::Sha1))
        .transpose()?
        .map(|spool| Arc::new(Mutex::new(spool)));
    let commit_edges_c = commit_edges.clone();
    let mut pack = Pack::new_with_hash_kind(
        HashKind::Sha1,
        Some(8),
        Some(1024 * 1024 * 1024),
        Some(tmp_path.to_path_buf()),
        true,
    );
    pack.decode(
        &mut pack_reader,
        move |meta_entry: MetaAttached<Entry, EntryMeta>| {
            let entry = &meta_entry.inner;
            if let Some(received) = &received
                && let Err(error) = received.record(entry)
            {
                record_first_pack_error(&err_c, error);
                return;
            }
            let hash_key = entry.hash;
            if let Some(spool) = commit_edges_c.as_ref()
                && entry.obj_type == ObjectType::Commit
            {
                let (_, parents) = match parse_commit_header_refs(&entry.data, hash_key) {
                    Ok(refs) => refs,
                    Err(error) => {
                        record_first_pack_error(&err_c, error);
                        return;
                    }
                };
                match spool.lock() {
                    Ok(mut guard) => {
                        if let Err(error) = guard.record_parents(hash_key, &parents) {
                            record_first_pack_error(&err_c, error);
                            return;
                        }
                    }
                    Err(_) => record_first_pack_error(
                        &err_c,
                        GitError::PackEncodeError("commit edge spool mutex poisoned".to_string()),
                    ),
                }
            }
            let Some(offset) = meta_entry.meta.pack_offset else {
                record_first_pack_error(
                    &err_c,
                    GitError::ConversionError(
                        "missing pack offset while building version 1 index".to_string(),
                    ),
                );
                return;
            };

            match obj_map_c.lock() {
                Ok(mut guard) => {
                    guard.insert(hash_key, offset);
                }
                Err(_) => record_first_pack_error(
                    &err_c,
                    GitError::PackEncodeError("index entry map mutex poisoned".to_string()),
                ),
            }
        },
        None::<fn(ObjectHash)>,
    )?;
    if let Some(err) = lock_state(&err, "index-pack error slot")?.take() {
        return Err(err);
    }

    let mut index_hash = Sha1::new();
    let mut index_file = std::fs::File::create(index_file)
        .map_err(|e| index_write_error("creating output file", e))?;
    let mut i: u8 = 0;
    let mut cnt: u32 = 0;
    let mut fan_out = Vec::with_capacity(256 * 4);
    let obj_map = take_arc_mutex(obj_map, "index entry map")?;
    for hash in obj_map.keys() {
        let first_byte = hash.as_ref()[0];
        while first_byte > i {
            fan_out.write_u32::<BigEndian>(cnt)?;
            i += 1;
        }
        cnt += 1;
    }
    loop {
        fan_out.write_u32::<BigEndian>(cnt)?;
        if i == 255 {
            break;
        }
        i += 1;
    }
    index_hash.update(&fan_out);
    index_file
        .write_all(&fan_out)
        .map_err(|e| index_write_error("writing fan-out table", e))?;

    for (hash, offset) in obj_map {
        let mut buf = Vec::with_capacity(24);
        buf.write_u32::<BigEndian>(offset as u32)?;
        buf.write_all(hash.as_ref())?;

        index_hash.update(&buf);
        index_file
            .write_all(&buf)
            .map_err(|e| index_write_error("writing object offsets", e))?;
    }

    index_hash.update(pack.signature.as_ref());
    index_file
        .write_all(pack.signature.as_ref())
        .map_err(|e| index_write_error("writing pack checksum", e))?;
    let index_hash: [u8; 20] = index_hash.finalize().into();
    index_file
        .write_all(&index_hash)
        .map_err(|e| index_write_error("writing index checksum", e))?;

    tracing::debug!("Index file is written to {:?}", index_file);
    commit_edges
        .map(|spool| {
            let mut spool = take_arc_mutex(spool, "commit edge spool")?;
            spool.finish()?;
            Ok(spool)
        })
        .transpose()
}
