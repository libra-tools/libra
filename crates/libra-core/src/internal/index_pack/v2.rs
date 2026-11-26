use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::types::ObjectType,
        pack::{
            Pack,
            entry::Entry,
            pack_index::{IdxBuilder, IndexEntry},
        },
    },
};

use crate::{
    internal::index_pack::support::{
        PackCommitEdges, ReceivedObjects, index_write_error, lock_state, record_first_pack_error,
        take_arc_mutex,
    },
    utils::client_storage::parse_commit_header_refs,
};

async fn write_idx_v2_file(
    index_file: PathBuf,
    idx_entries: Vec<IndexEntry>,
    pack_hash: ObjectHash,
) -> Result<(), GitError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    let mut builder = IdxBuilder::new(idx_entries.len(), tx, pack_hash);
    let mut idx_file = tokio::fs::File::create(index_file)
        .await
        .map_err(|e| index_write_error("creating output file", e))?;
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;

        while let Some(chunk) = rx.recv().await {
            idx_file
                .write_all(&chunk)
                .await
                .map_err(|e| index_write_error("writing index data", e))?;
        }
        idx_file
            .flush()
            .await
            .map_err(|e| index_write_error("flushing index file", e))?;
        Ok::<(), GitError>(())
    });

    builder.write_idx(idx_entries).await?;
    let writer_result = writer
        .await
        .map_err(|e| GitError::PackEncodeError(format!("idx writer task join error: {e}")))?;
    writer_result?;
    Ok(())
}

fn write_idx_v2_sync(
    index_file: PathBuf,
    idx_entries: Vec<IndexEntry>,
    pack_hash: ObjectHash,
) -> Result<(), GitError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(write_idx_v2_file(index_file, idx_entries, pack_hash))
}

struct TempDirGuard {
    path: PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn build_index_v2(pack_file: &str, index_file: &str) -> Result<(), GitError> {
    build_index_v2_inner(pack_file, index_file, false, None).map(|_| ())
}

pub fn build_index_v2_inner(
    pack_file: &str,
    index_file: &str,
    collect_edges: bool,
    received: Option<Arc<ReceivedObjects>>,
) -> Result<Option<PackCommitEdges>, GitError> {
    let pack_path = PathBuf::from(pack_file);
    let parent = pack_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_dir_path = parent.join(format!(".tmp_idx_{}", timestamp));
    std::fs::create_dir_all(&tmp_dir_path)?;

    let _guard = TempDirGuard {
        path: tmp_dir_path.clone(),
    };

    let tmp_path = tmp_dir_path;
    let pack_file = std::fs::File::open(pack_file)?;
    let mut pack_reader = std::io::BufReader::new(pack_file);
    let idx_entries = Arc::new(Mutex::new(Vec::new()));
    let idx_entries_c = idx_entries.clone();
    let err = Arc::new(Mutex::new(None));
    let err_c = err.clone();
    let commit_edges = collect_edges
        .then(|| PackCommitEdges::new(parent, git_internal::hash::get_hash_kind()))
        .transpose()?
        .map(|spool| Arc::new(Mutex::new(spool)));
    let commit_edges_c = commit_edges.clone();

    let mut pack = Pack::new_with_hash_kind(
        git_internal::hash::get_hash_kind(),
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
            if let Some(spool) = commit_edges_c.as_ref()
                && entry.obj_type == ObjectType::Commit
            {
                let (_, parents) = match parse_commit_header_refs(&entry.data, entry.hash) {
                    Ok(refs) => refs,
                    Err(error) => {
                        record_first_pack_error(&err_c, error);
                        return;
                    }
                };
                match spool.lock() {
                    Ok(mut guard) => {
                        if let Err(error) = guard.record_parents(entry.hash, &parents) {
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
            match IndexEntry::try_from(&meta_entry) {
                Ok(entry) => match idx_entries_c.lock() {
                    Ok(mut guard) => guard.push(entry),
                    Err(_) => record_first_pack_error(
                        &err_c,
                        GitError::PackEncodeError("index entry buffer mutex poisoned".to_string()),
                    ),
                },
                Err(e) => record_first_pack_error(&err_c, e),
            };
        },
        None::<fn(ObjectHash)>,
    )?;

    if let Some(err) = lock_state(&err, "index-pack error slot")?.take() {
        return Err(err);
    }

    let idx_entries = take_arc_mutex(idx_entries, "index entry buffer")?;
    if idx_entries.len() != pack.number {
        return Err(GitError::ConversionError(format!(
            "decoded entries count {} != pack number {}",
            idx_entries.len(),
            pack.number
        )));
    }

    let index_path = PathBuf::from(index_file);
    let pack_hash = pack.signature;
    if tokio::runtime::Handle::try_current().is_ok() {
        let handle =
            std::thread::spawn(move || write_idx_v2_sync(index_path, idx_entries, pack_hash));
        handle
            .join()
            .map_err(|_| GitError::PackEncodeError("idx writer thread panicked".to_string()))??;
    } else {
        write_idx_v2_sync(index_path, idx_entries, pack_hash)?;
    }

    commit_edges
        .map(|spool| {
            let mut spool = take_arc_mutex(spool, "commit edge spool")?;
            spool.finish()?;
            Ok(spool)
        })
        .transpose()
}
