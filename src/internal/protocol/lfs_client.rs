//! LFS protocol client that negotiates batch/lock/verify endpoints, uploads or downloads objects in chunks with hashing, and caches auth endpoints.

use std::{collections::HashSet, path::Path};

use anyhow::{Context as _, anyhow};
use futures_util::StreamExt;
use git_internal::internal::{object::types::ObjectType, pack::entry::Entry};
use reqwest::{Client, StatusCode};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::OnceCell,
};
use url::Url;

use crate::{
    command,
    internal::{
        config::ConfigKv,
        protocol::{ProtocolClient, https_client::BasicAuth},
    },
    lfs_structs::{
        Action, BatchRequest, ChunkDownloadObject, FetchchunkResponse, LockList, LockListQuery,
        LockRequest, ObjectError, Operation, Ref, RequestObject, ResponseObject, UnlockRequest,
        VerifiableLockList, VerifiableLockRequest,
    },
    utils::{lfs, util},
};

/// Failure surface for the LFS push pipeline.
///
/// `Display` formats the error as `LFS push failed[ for <path>][ (oid <oid>)]: <detail>`,
/// so callers that propagate via `?` or call `.to_string()` get a meaningful
/// one-line message instead of having to manually destructure the fields.
/// Callers that need structured access (e.g., `src/command/push.rs` maps to
/// a typed `PushError::LfsUploadFailed`) continue to read the public fields
/// directly.
#[derive(Debug, Clone, thiserror::Error)]
pub struct LfsPushError {
    pub path: Option<String>,
    pub oid: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for LfsPushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LFS push failed")?;
        if let Some(path) = &self.path {
            write!(f, " for {path}")?;
        }
        if let Some(oid) = &self.oid {
            write!(f, " (oid {oid})")?;
        }
        write!(f, ": {}", self.detail)
    }
}

#[derive(Debug)]
pub struct LFSClient {
    pub batch_url: Url,
    pub lfs_url: Url,
    pub client: Client,
}

#[derive(Debug, thiserror::Error)]
pub enum LockListError {
    #[error("request failed: {0}")]
    Request(String),
    #[error("remote returned status {status}: {message}")]
    Http { status: StatusCode, message: String },
    #[error("failed to decode response: {0}")]
    Decode(String),
}

static LFS_CLIENT: OnceCell<LFSClient> = OnceCell::const_new();
impl LFSClient {
    #[cfg(feature = "fastcdc")]
    async fn media_client(
        &self,
        local_fallback: bool,
    ) -> anyhow::Result<Option<crate::utils::media::transfer::MediaClient>> {
        if crate::utils::util::try_get_storage_path(None).is_ok()
            && let Some(value) = ConfigKv::get_best_effort("lfs.fastcdc").await?
        {
            match value.value.to_ascii_lowercase().as_str() {
                "false" | "no" | "off" | "0" => return Ok(None),
                "true" | "yes" | "on" | "1" => (),
                _ => return Err(anyhow!("lfs.fastcdc must be a boolean")),
            }
        }
        crate::utils::media::transfer::MediaClient::discover(
            self.client.clone(),
            &self.lfs_url,
            local_fallback,
        )
        .await
    }

    #[cfg(feature = "fastcdc")]
    async fn try_media_upload(
        &self,
        oid: &str,
        size: i64,
        file: &Path,
        remote_has_full_object: bool,
    ) -> Result<bool, LfsPushError> {
        let result = async {
            let Some(media) = self.media_client(true).await? else {
                return Ok(false);
            };
            let size = u64::try_from(size).context("negative LFS object size")?;
            if remote_has_full_object {
                // A prior finalize may have persisted the standard object but
                // failed before publishing the manifest. Repair it only when
                // a complete local source is available; ordinary basic LFS
                // skips never require opening a local object.
                let Ok(metadata) = tokio::fs::metadata(file).await else {
                    return Ok(false);
                };
                if !metadata.is_file() || metadata.len() != size {
                    return Ok(false);
                }
            }
            media.upload(oid, size, file).await
        }
        .await;
        result.map_err(|error| LfsPushError {
            path: Some(file.display().to_string()),
            oid: Some(oid.to_owned()),
            detail: format!("FastCDC upload failed: {error:#}"),
        })
    }

    /// Get LFSClient instance
    /// - DO NOT use `async_static!`: No IDE Code Completion & lagging
    pub async fn get() -> anyhow::Result<&'static LFSClient> {
        LFS_CLIENT
            .get_or_try_init(|| async { LFSClient::new().await })
            .await
    }
}

/// see [successful-responses](https://github.com/git-lfs/git-lfs/blob/main/docs/api/batch.md#successful-responses)
#[derive(Serialize, Deserialize)]
pub struct LfsBatchResponse {
    pub transfer: Option<String>,
    pub objects: Vec<ResponseObject>,
    pub hash_algo: Option<String>,
}

impl ProtocolClient for LFSClient {
    /// Construct LFSClient from a given Repo URL.
    ///
    /// INVARIANT (trait contract): the `ProtocolClient::from_url` trait
    /// returns `Self`, not Result, so failures here must panic. Use
    /// `LFSClient::new()` (returns `anyhow::Result<Self>`) for the
    /// graceful path that also handles SCP-style SSH URLs and surfaces
    /// errors with context.
    fn from_url(repo_url: &Url) -> Self {
        // INVARIANT (trait contract): this constructor cannot return errors;
        // callers needing recovery use `from_remote_url` or `new` instead.
        Self::from_remote_url(repo_url.as_str()).expect(
            "LFSClient::from_url: failed to construct LFS client (use from_remote_url for errors)",
        )
    }
}

impl LFSClient {
    /// Construct LFSClient from current remote URL.
    pub async fn new() -> anyhow::Result<Self> {
        let url = ConfigKv::get_current_remote_url()
            .await
            .ok()
            .flatten()
            .ok_or_else(|| {
                anyhow!(
                    "no remote set for current branch, use \
                     `libra branch --set-upstream-to <remote>/<branch>`"
                )
            })?;
        Self::from_remote_url(&url)
    }

    /// Build a client from an EXPLICIT remote URL (lore.md 2.8): the lock
    /// gate must work on branches with no upstream yet (falling back to
    /// `remote.origin.url`), where [`Self::new`]'s current-branch resolution
    /// would refuse.
    pub fn from_remote_url(url: &str) -> anyhow::Result<Self> {
        // Convert SCP-style SSH before parsing and append to the URL path,
        // never after the query string. Keep the query on both endpoints.
        let lfs_server = lfs::generate_lfs_server_url(url.to_string());
        let mut lfs_server = Url::parse(&lfs_server).with_context(|| {
            format!(
                "failed to derive LFS server URL from remote '{}'",
                crate::utils::redact::redact_url_credentials(url)
            )
        })?;
        lfs_server.set_path(&format!("{}/", lfs_server.path().trim_end_matches('/')));
        let mut batch_url = lfs_server.clone();
        batch_url.set_path(&format!("{}objects/batch", lfs_server.path()));
        let client = Client::builder()
            .redirect(super::https_client::no_downgrade_redirect_policy())
            .default_headers(lfs::LFS_HEADERS.clone())
            .build()?;
        Ok(Self {
            batch_url,
            lfs_url: lfs_server,
            client,
        })
    }

    /// push LFS objects to remote server
    pub async fn push_objects<'a, I>(&self, objs: I) -> Result<usize, LfsPushError>
    where
        I: IntoIterator<Item = &'a Entry>,
    {
        // filter pointer file within blobs
        let mut lfs_oids = Vec::new();
        for blob in objs.into_iter().filter(|e| e.obj_type == ObjectType::Blob) {
            let oid = lfs::parse_pointer_data(&blob.data);
            if let Some(oid) = oid {
                lfs_oids.push(oid);
            }
        }

        let mut lfs_objs = Vec::new();
        for (oid, _) in &lfs_oids {
            let path = lfs::lfs_object_path(oid);
            if !path.exists() {
                return Err(LfsPushError {
                    path: Some(path.display().to_string()),
                    oid: Some(oid.clone()),
                    detail: "local LFS object not found".to_string(),
                });
            }
            let size = path.metadata().map_err(|e| LfsPushError {
                path: Some(path.display().to_string()),
                oid: Some(oid.clone()),
                detail: format!("failed to read local LFS object metadata: {e}"),
            })?;
            let size = size.len() as i64;
            lfs_objs.push(RequestObject {
                oid: oid.to_owned(),
                size,
                ..Default::default()
            })
        }

        if lfs_objs.is_empty() {
            tracing::info!("No LFS objects to push.");
            return Ok(0);
        }

        {
            // verify locks
            let refspec = command::lfs::current_refspec()
                .await
                .ok_or_else(|| LfsPushError {
                    path: None,
                    oid: None,
                    detail:
                        "HEAD is detached; check out a branch before pushing LFS objects so the \
                     remote can verify locks against a refspec."
                            .to_string(),
                })?;
            let (code, locks) = self
                .verify_locks(VerifiableLockRequest {
                    refs: Ref { name: refspec },
                    ..Default::default()
                })
                .await
                .map_err(|e| LfsPushError {
                    path: None,
                    oid: None,
                    detail: format!("LFS verify locks request failed: {e}"),
                })?;

            if code == StatusCode::FORBIDDEN {
                return Err(LfsPushError {
                    path: None,
                    oid: None,
                    detail: "forbidden: you must have push access to verify locks".to_string(),
                });
            } else if code == StatusCode::NOT_FOUND {
                // By default, an LFS server that doesn't implement any locking endpoints should return 404.
                // This response will not halt any Git pushes.
            } else if !code.is_success() {
                return Err(LfsPushError {
                    path: None,
                    oid: None,
                    detail: format!("LFS verify locks failed with status {code}"),
                });
            } else {
                // success
                tracing::debug!("LFS verify locks response:\n {:?}", locks);
                let oids: HashSet<String> = lfs_oids.iter().map(|(oid, _)| oid.clone()).collect();
                let ours = locks
                    .ours
                    .iter()
                    .filter(|l| {
                        lfs::get_oid_by_path(&l.path).is_some_and(|oid| oids.contains(&oid))
                    })
                    .collect::<Vec<_>>();
                if !ours.is_empty() {
                    println!("The following files are locked by you, consider unlocking them:");
                    for lock in ours {
                        println!("  - {}", lock.path);
                    }
                }
                let theirs = locks
                    .theirs
                    .iter()
                    .filter(|l| {
                        lfs::get_oid_by_path(&l.path).is_some_and(|oid| oids.contains(&oid))
                    })
                    .collect::<Vec<_>>();
                if !theirs.is_empty() {
                    let locked_paths = theirs
                        .iter()
                        .map(|lock| lock.path.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(LfsPushError {
                        path: None,
                        oid: None,
                        detail: format!(
                            "the following files are locked by another user: {locked_paths}"
                        ),
                    });
                }
            }
        }

        let batch_request = BatchRequest {
            operation: Operation::Upload,
            transfers: vec![lfs::LFS_TRANSFER_API.to_string()],
            objects: lfs_objs,
            hash_algo: lfs::LFS_HASH_ALGO.to_string(),
        };

        let response = BasicAuth::send(|| async {
            self.client
                .post(self.batch_url.clone())
                .json(&batch_request)
                .headers(lfs::LFS_HEADERS.clone())
        })
        .await
        .map_err(|e| LfsPushError {
            path: None,
            oid: None,
            detail: format!("failed to request LFS batch upload: {e}"),
        })?;

        let resp = response
            .json::<LfsBatchResponse>()
            .await
            .map_err(|e| LfsPushError {
                path: None,
                oid: None,
                detail: format!("failed to decode LFS batch upload response: {e}"),
            })?;
        tracing::debug!(
            "LFS push response:\n {:#?}",
            serde_json::to_value(&resp).unwrap_or_default()
        );

        // TODO: parallel upload
        let mut uploaded = 0;
        for obj in resp.objects {
            let file_path = lfs::lfs_object_path(&obj.oid);
            if self.upload_object(obj, &file_path).await? {
                uploaded += 1;
            }
        }
        println!("LFS objects push completed.");
        Ok(uploaded)
    }

    /// push LFS object to remote server, didn't need local lfs storage
    pub async fn push_object(&self, oid: &str, file: &Path) -> Result<bool, LfsPushError> {
        let batch_request = BatchRequest {
            operation: Operation::Upload,
            transfers: vec![lfs::LFS_TRANSFER_API.to_string()],
            objects: vec![RequestObject {
                oid: oid.to_owned(),
                size: file
                    .metadata()
                    .map_err(|e| LfsPushError {
                        path: Some(file.display().to_string()),
                        oid: Some(oid.to_string()),
                        detail: format!("failed to read local LFS object metadata: {e}"),
                    })?
                    .len() as i64,
                ..Default::default()
            }],
            hash_algo: lfs::LFS_HASH_ALGO.to_string(),
        };

        let response = BasicAuth::send(|| async {
            self.client
                .post(self.batch_url.clone())
                .json(&batch_request)
                .headers(lfs::LFS_HEADERS.clone())
        })
        .await
        .map_err(|e| LfsPushError {
            path: Some(file.display().to_string()),
            oid: Some(oid.to_string()),
            detail: format!("failed to request LFS batch upload: {e}"),
        })?;

        let resp = response
            .json::<LfsBatchResponse>()
            .await
            .map_err(|e| LfsPushError {
                path: Some(file.display().to_string()),
                oid: Some(oid.to_string()),
                detail: format!("failed to decode LFS batch upload response: {e}"),
            })?;
        tracing::debug!(
            "LFS push response:\n {:#?}",
            serde_json::to_value(&resp).unwrap_or_default()
        );
        if resp.objects.len() != 1 {
            return Err(LfsPushError {
                path: Some(file.display().to_string()),
                oid: Some(oid.to_string()),
                detail: format!(
                    "LFS batch upload returned {} objects, expected exactly 1",
                    resp.objects.len()
                ),
            });
        }
        // INVARIANT: `resp.objects.len() != 1` was checked above and rejected
        // before reaching this branch, so `into_iter().next()` always yields
        // exactly one ResponseObject.
        let obj = resp
            .objects
            .into_iter()
            .next()
            .expect("LFS batch response had exactly one object (checked above)");
        let uploaded = self.upload_object(obj, file).await?;
        println!("LFS objects push completed.");
        Ok(uploaded)
    }

    /// upload (PUT) one LFS file to remote server
    pub async fn upload_object(
        &self,
        object: ResponseObject,
        file: &Path,
    ) -> Result<bool, LfsPushError> {
        let oid = object.oid.clone();
        if let Some(err) = object.error {
            return Err(LfsPushError {
                path: Some(file.display().to_string()),
                oid: Some(oid),
                detail: format!("remote reported error {}: {}", err.code, err.message),
            });
        }

        if let Some(actions) = object.actions {
            let link = actions.get(&Action::Upload).ok_or_else(|| LfsPushError {
                path: Some(file.display().to_string()),
                oid: Some(oid),
                detail: "remote did not provide an upload action".to_string(),
            })?;

            println!("Uploading LFS file: {}", object.oid);
            #[cfg(feature = "fastcdc")]
            if self
                .try_media_upload(&object.oid, object.size, file, false)
                .await?
            {
                return Ok(true);
            }
            let content_len = tokio::fs::metadata(file)
                .await
                .map_err(|e| LfsPushError {
                    path: Some(file.display().to_string()),
                    oid: Some(object.oid.clone()),
                    detail: format!("failed to read local LFS object metadata: {e}"),
                })?
                .len();

            let resp = BasicAuth::send(|| async {
                let mut request = self.client.put(&link.href);
                for (k, v) in &link.header {
                    request = request.header(k, v);
                }

                // INVARIANT: metadata was validated before entering the retry loop.
                // A subsequent failure here indicates the file disappeared mid-upload
                // (TOCTOU race with another process); panicking is the only recovery
                // because BasicAuth::send's closure return type is reqwest::RequestBuilder.
                let content = tokio::fs::File::open(file)
                    .await
                    .expect("LFS upload file disappeared between metadata check and File::open");
                let progress_bar = util::default_progress_bar(content_len);

                let stream = tokio_util::io::ReaderStream::new(content);
                let progress_stream = stream.map(move |chunk| {
                    if let Ok(ref data) = chunk {
                        progress_bar.inc(data.len() as u64);
                    }
                    chunk
                });
                request.body(reqwest::Body::wrap_stream(progress_stream))
            })
            .await
            .map_err(|e| LfsPushError {
                path: Some(file.display().to_string()),
                oid: Some(object.oid.clone()),
                detail: format!("failed to send LFS upload request: {e}"),
            })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let message = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<unavailable>".to_string());
                return Err(LfsPushError {
                    path: Some(file.display().to_string()),
                    oid: Some(object.oid.clone()),
                    detail: format!("upload request failed with status {status}: {message}"),
                });
            }
            println!("Uploaded.");
            Ok(true)
        } else {
            #[cfg(feature = "fastcdc")]
            if self
                .try_media_upload(&object.oid, object.size, file, true)
                .await?
            {
                return Ok(true);
            }
            tracing::debug!("LFS file {} already exists on remote server", object.oid);
            Ok(false)
        }
    }

    /// Re-hash the already-downloaded prefix of a resumed download so the running
    /// SHA-256 context matches the bytes on disk before more chunks are appended.
    async fn update_file_checksum(
        file: &mut tokio::fs::File,
        checksum: &mut Context,
    ) -> std::io::Result<()> {
        file.seek(tokio::io::SeekFrom::Start(0)).await?;
        let mut buf = [0u8; 8192];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            checksum.update(&buf[..n]);
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    /// download (GET) one LFS file from remote server
    pub async fn download_object(
        &self,
        oid: &str,
        size: u64,
        path: impl AsRef<Path>,
        mut reporter: Option<(
            &mut (dyn FnMut(f64) -> anyhow::Result<()> + Send), // progress callback
            f64,                                                // step
        )>,
    ) -> anyhow::Result<()> {
        let batch_request = BatchRequest {
            operation: Operation::Download,
            transfers: vec![lfs::LFS_TRANSFER_API.to_string()],
            objects: vec![RequestObject {
                oid: oid.to_owned(),
                size: size as i64,
                ..Default::default()
            }],
            hash_algo: lfs::LFS_HASH_ALGO.to_string(),
        };

        let response = BasicAuth::send(|| async {
            self.client
                .post(self.batch_url.clone())
                .json(&batch_request)
                .headers(lfs::LFS_HEADERS.clone())
        })
        .await?;

        let text = response.text().await?;
        // Pre-fix this debug! macro called `serde_json::from_str::<Value>(&text)?`
        // inline so the response was parsed twice (once for the pretty
        // debug snapshot, once for the typed `LfsBatchResponse` two
        // lines below). Worse, `?` inside a `debug!` macro is NOT
        // gated by log-level — the closure is always evaluated, so a
        // non-JSON body would fail the entire download with a "expected
        // value" serde error even when `RUST_LOG` was at info or above.
        // Log the raw text instead; the typed parse below is the real
        // source of truth and surfaces its own error.
        tracing::debug!("LFS download response: {}", text);
        let resp = serde_json::from_str::<LfsBatchResponse>(&text)?;
        let obj = resp.objects.first().ok_or_else(|| {
            anyhow!(
                "LFS batch download response contained no objects for oid {oid}; \
                 the remote returned an empty `objects` array"
            )
        })?;
        if obj.error.is_some() || obj.actions.is_none() {
            let unknown_err = ObjectError {
                code: 0,
                message: "Unknown error".to_string(),
            };
            let err = obj.error.as_ref().unwrap_or(&unknown_err);
            // 404 means the object doesn't exist on the LFS server.
            // Gracefully fall back to writing a pointer file, matching git behavior.
            if err.code == 404 {
                eprintln!(
                    "warning: LFS object {oid} not found on server, keeping pointer file. \
                     Run `libra lfs pull` to retry."
                );
                tracing::warn!("LFS object {oid} not found on server (404), keeping pointer file.");
                let pointer = lfs::format_pointer_string(oid, size);
                tokio::fs::write(path.as_ref(), pointer.as_bytes()).await?;
                return Ok(());
            }
            eprintln!(
                "fatal: LFS download failed (BatchRequest). Code: {}, Message: {}",
                err.code, err.message
            );
            return Err(anyhow!("LFS download failed."));
        }

        // INVARIANT: actions.is_none() already returned above, so as_ref().unwrap()
        // here is safe. The Download action, however, can legitimately be absent
        // (e.g. server only returns Upload), so handle that case explicitly.
        let actions = obj
            .actions
            .as_ref()
            .expect("actions.is_none() checked above");
        let link = actions.get(&Action::Download).ok_or_else(|| {
            anyhow!("LFS batch download response missing 'download' action for oid {oid}")
        })?;

        #[cfg(feature = "fastcdc")]
        if let Ok(storage) = util::try_get_storage_path(None)
            && let Some(media) = self.media_client(false).await?
        {
            // The public LFSClient also supports callers outside a repository;
            // those use basic LFS instead of a repository-only cache that panics.
            let store = crate::utils::media::chunk_store::MediaChunkStore::at(
                storage.join("media").join("chunks"),
            );
            if media.download(oid, size, path.as_ref(), &store).await? {
                if let Some((report, _)) = reporter.as_mut() {
                    report(100.0)?;
                }
                return Ok(());
            }
        }

        let mut is_chunked = false;
        // Chunk API — infer that all chunks share the same size, falling back to the
        // total object size when the server reports a single-chunk download.
        let chunk_size: i64;
        let links = match self.fetch_chunks(&link.href).await {
            Ok(chunks) if !chunks.is_empty() => {
                is_chunked = true;
                // INVARIANT: matched the `!chunks.is_empty()` guard above.
                chunk_size = chunks
                    .first()
                    .expect("LFS chunk list was non-empty (checked above)")
                    .size;
                tracing::info!("LFS Chunk API supported.");
                chunks.into_iter().map(|c| c.link).collect()
            }
            _ => {
                chunk_size = size as i64;
                vec![link.clone()]
            }
        };

        let mut checksum = Context::new(&SHA256);
        let mut got_parts = 0;
        let mut file = if links.len() <= 1 || lfs::parse_pointer_file(&path).is_ok() {
            // pointer file or Not Chunks, truncate
            tokio::fs::File::create(path).await?
        } else {
            // for Chunks, calc offset to resume download
            let mut file = tokio::fs::File::options()
                .write(true)
                .read(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .await?;
            let file_len = file.metadata().await?.len();
            if file_len > size {
                println!("Local file size is larger than remote, truncate to 0.");
                file.set_len(0).await?; // clear
                file.seek(tokio::io::SeekFrom::Start(0)).await?;
            } else if file_len > 0 {
                let chunk_size = chunk_size as u64;
                got_parts = file_len / chunk_size;
                let file_offset = got_parts * chunk_size;
                println!(
                    "Resume download from offset: {}, part: {}",
                    file_offset,
                    got_parts + 1
                );
                file.set_len(file_offset).await?; // truncate
                Self::update_file_checksum(&mut file, &mut checksum).await?; // resume checksum
                file.seek(tokio::io::SeekFrom::End(0)).await?;
            }
            file
        };

        println!("Downloading LFS file: {oid}");
        let parts = links.len();
        let mut downloaded: u64 = file.metadata().await?.len();
        let mut last_progress = 0.0;
        let start_part = got_parts as usize;
        for link in links.iter().skip(start_part) {
            got_parts += 1;
            if is_chunked {
                println!("- part: {got_parts}/{parts}");
            }

            let response = BasicAuth::send(|| async {
                let mut request = self.client.get(&link.href);
                for (k, v) in &link.header {
                    request = request.header(k, v);
                }
                request
            })
            .await?;
            if !response.status().is_success() {
                eprintln!(
                    "fatal: LFS download failed. Status: {}, Message: {}",
                    response.status(),
                    response.text().await?
                );
                return Err(anyhow!("LFS download failed."));
            }

            let cur_chunk_size = if (got_parts as usize) < parts {
                chunk_size as u64
            } else {
                // last part
                size - (parts as u64 - 1) * chunk_size as u64
            };
            let pb = util::default_progress_bar(cur_chunk_size);
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                // TODO: progress bar TODO: multi-thread or async
                let chunk = chunk?;
                file.write_all(&chunk).await?;
                checksum.update(&chunk);

                // report progress
                if let Some((ref mut report_fn, step)) = reporter {
                    downloaded += chunk.len() as u64;
                    let progress = (downloaded as f64 / size as f64) * 100.0;
                    if progress >= last_progress + step {
                        last_progress = progress;
                        report_fn(progress)?;
                    }
                } else {
                    // mutually exclusive with reporter
                    pb.inc(chunk.len() as u64);
                }
            }
            pb.finish_and_clear();
        }
        // Tokio may still be completing the last write on its blocking pool.
        // Do not report success until those bytes (and any write error) are visible.
        file.flush()
            .await
            .context("failed to finish writing downloaded LFS object")?;
        let checksum = hex::encode(checksum.finish().as_ref());
        if checksum == oid {
            println!("Downloaded.");
            Ok(())
        } else {
            eprintln!(
                "fatal: LFS download failed. Checksum mismatch: {checksum} != {oid}. Fallback to pointer file."
            );
            let pointer = lfs::format_pointer_string(oid, size);
            file.set_len(0).await?; // clear
            file.seek(tokio::io::SeekFrom::Start(0)).await?; // ensure
            file.write_all(pointer.as_bytes()).await?;
            file.flush()
                .await
                .context("failed to finish writing LFS fallback pointer")?;
            Err(anyhow!("Checksum mismatch, fallback to pointer file."))
        }
    }

    /// Only for MonoRepo (mega)
    ///
    /// Returns `Err(())` whenever the chunks endpoint isn't usable for any reason —
    /// invalid URL, network error, 404/403 ("server doesn't support Chunks API"),
    /// non-success status, or undecodable JSON body. The caller already treats this
    /// as a fallback to the non-chunked download path, so we just log and bail
    /// instead of panicking.
    async fn fetch_chunks(&self, obj_link: &str) -> Result<Vec<ChunkDownloadObject>, ()> {
        let mut url = match Url::parse(obj_link) {
            Ok(u) => u,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "failed to parse LFS chunk URL, falling back to non-chunked download"
                );
                return Err(());
            }
        };
        let path = url.path().trim_end_matches('/');
        url.set_path(&(path.to_owned() + "/chunks")); // reserve query params (for GitHub link)

        let resp = match BasicAuth::send(|| async { self.client.get(url.clone()) }).await {
            Ok(resp) => resp,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "LFS chunks request failed, falling back to non-chunked download"
                );
                return Err(());
            }
        };
        let code = resp.status();
        if code == StatusCode::NOT_FOUND || code == StatusCode::FORBIDDEN {
            // GitHub maybe return 403
            tracing::info!("Remote LFS Server not support Chunks API, or forbidden.");
            return Err(());
        } else if !code.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!(
                "fatal: LFS get chunk hrefs failed. Status: {}, Message: {}",
                code,
                body
            );
            return Err(());
        }
        let mut res = match resp.json::<FetchchunkResponse>().await {
            Ok(res) => res,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "failed to decode LFS chunks response, falling back to non-chunked download"
                );
                return Err(());
            }
        };
        // sort by offset
        res.chunks.sort_by_key(|a| a.offset);
        Ok(res.chunks)
    }
}

// LFS locks API
impl LFSClient {
    pub async fn get_locks(&self, query: LockListQuery) -> Result<LockList, LockListError> {
        // INVARIANT: `self.lfs_url` was parsed by `Url::parse` during client
        // construction; joining a static relative URL onto a valid base URL
        // cannot fail.
        let url = self
            .lfs_url
            .join("locks")
            .expect("'locks' is a valid relative URL");
        let query = [
            ("id", query.id),
            ("path", query.path),
            ("limit", query.limit),
            ("cursor", query.cursor),
            ("refspec", query.refspec),
        ];
        let response = BasicAuth::send(|| async { self.client.get(url.clone()).query(&query) })
            .await
            .map_err(|err| LockListError::Request(err.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let message = response
                .text()
                .await
                .unwrap_or_else(|err| format!("failed to read response body: {err}"));
            return Err(LockListError::Http { status, message });
        }

        response
            .json::<LockList>()
            .await
            .map_err(|err| LockListError::Decode(err.to_string()))
    }

    /// lock an LFS file
    /// - `refspec` is must in Mega Server, but optional in Git Doc
    pub async fn lock(&self, path: String, refspec: String) -> Result<StatusCode, reqwest::Error> {
        // INVARIANT: `self.lfs_url` was parsed by `Url::parse` during client
        // construction; joining a static relative URL onto a valid base URL
        // cannot fail.
        let url = self
            .lfs_url
            .join("locks")
            .expect("'locks' is a valid relative URL");
        let resp = BasicAuth::send(|| async {
            self.client.post(url.clone()).json(&LockRequest {
                path: path.clone(),
                refs: Ref {
                    name: refspec.clone(),
                },
            })
        })
        .await?;
        let code = resp.status();
        if !resp.status().is_success() && code != StatusCode::FORBIDDEN {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %code, body = %body, "LFS lock failed");
        }
        Ok(code)
    }

    pub async fn unlock(
        &self,
        id: String,
        refspec: String,
        force: bool,
    ) -> Result<StatusCode, reqwest::Error> {
        // INVARIANT: `id` comes from a prior `get_locks` response or
        // user-supplied `--id` argument and is treated as a single path
        // segment; if it contains URL-special characters, `Url::join`
        // percent-encodes them via the relative-URL parser. The .expect()
        // names the dynamic-id contract.
        let url = self
            .lfs_url
            .join(&format!("locks/{id}/unlock"))
            .expect("LFS lock id failed to compose a valid relative URL segment");
        let resp = BasicAuth::send(|| async {
            self.client.post(url.clone()).json(&UnlockRequest {
                force: Some(force),
                refs: Ref {
                    name: refspec.clone(),
                },
            })
        })
        .await?;
        let code = resp.status();
        if !resp.status().is_success() && code != StatusCode::FORBIDDEN {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %code, body = %body, "LFS unlock failed");
        }
        Ok(code)
    }

    /// List Locks for Verification
    pub async fn verify_locks(
        &self,
        query: VerifiableLockRequest,
    ) -> Result<(StatusCode, VerifiableLockList), reqwest::Error> {
        // INVARIANT: `self.lfs_url` was parsed by `Url::parse` during client
        // construction; joining a static relative URL onto a valid base URL
        // cannot fail.
        let url = self
            .lfs_url
            .join("locks/verify")
            .expect("'locks/verify' is a valid relative URL");
        let resp = BasicAuth::send(|| async { self.client.post(url.clone()).json(&query) }).await?;
        let code = resp.status();
        // Only a 2xx response carries a lock list. Any non-success status —
        // 404 (server implements no locking endpoints; by default this must
        // NOT halt pushes), 403 (no push access), 5xx — is returned WITHOUT
        // decoding the body: a 404's body is typically empty or a plain
        // error object, so `resp.json::<VerifiableLockList>()` would fail and
        // (pre-fix) turn a benign no-locking-API 404 into a hard error,
        // bypassing the caller's explicit 404/403 handling. A genuinely
        // unexpected non-2xx (not 404/403) still prints a fatal note; the
        // caller decides whether an empty list on that status is acceptable.
        if !code.is_success() {
            if code != StatusCode::NOT_FOUND && code != StatusCode::FORBIDDEN {
                eprintln!(
                    "fatal: LFS verify locks failed. Status: {}, Message: {}",
                    code,
                    resp.text().await.unwrap_or_default()
                );
            }
            return Ok((
                code,
                VerifiableLockList {
                    ours: Vec::new(),
                    theirs: Vec::new(),
                    next_cursor: String::default(),
                },
            ));
        }
        let list = resp.json::<VerifiableLockList>().await?;
        Ok((code, list))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_append_endpoints_before_the_query() {
        let remote = "ssh://git@host.example:8443/project/demo.git/?tenant=one#ref";
        let client = LFSClient::from_remote_url(remote).unwrap();
        assert_eq!(
            client.lfs_url.as_str(),
            "https://host.example:8443/project/demo.git/info/lfs/?tenant=one"
        );
        assert_eq!(
            client.batch_url.as_str(),
            "https://host.example:8443/project/demo.git/info/lfs/objects/batch?tenant=one"
        );
        let trait_client = LFSClient::from_url(&Url::parse(remote).unwrap());
        assert_eq!(trait_client.lfs_url, client.lfs_url);
        assert_eq!(trait_client.batch_url, client.batch_url);
    }

    #[test]
    fn test_request_vars() {
        let vars = RequestObject {
            oid: "123".to_string(),
            size: 123,
            ..Default::default()
        };
        println!("{:?}", serde_json::to_string(&vars).unwrap());
    }

    /// Regression for v0.17.269: pin the `Display` format contract for
    /// `LfsPushError`. The format is
    /// `LFS push failed[ for <path>][ (oid <oid>)]: <detail>` — both
    /// optional fields are elided if `None`. Callers that propagate via
    /// `?` or call `.to_string()` rely on this exact one-line shape.
    #[test]
    fn lfs_push_error_display_formats_all_combinations() {
        // path + oid + detail (the common case from upload_object)
        let err = LfsPushError {
            path: Some("/tmp/large.bin".to_string()),
            oid: Some("abc123".to_string()),
            detail: "remote did not provide an upload action".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "LFS push failed for /tmp/large.bin (oid abc123): \
             remote did not provide an upload action",
        );

        // path only
        let err = LfsPushError {
            path: Some("/tmp/large.bin".to_string()),
            oid: None,
            detail: "local LFS object not found".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "LFS push failed for /tmp/large.bin: local LFS object not found",
        );

        // oid only
        let err = LfsPushError {
            path: None,
            oid: Some("abc123".to_string()),
            detail: "remote rejected upload".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "LFS push failed (oid abc123): remote rejected upload",
        );

        // detail only (e.g. lock-verification failures with no path/oid)
        let err = LfsPushError {
            path: None,
            oid: None,
            detail: "HEAD is detached".to_string(),
        };
        assert_eq!(err.to_string(), "LFS push failed: HEAD is detached");
    }

    /// Pin the `Display` format contract for [`LockListError`]. The
    /// variants are produced via `thiserror` `#[error(...)]` attributes:
    ///   - `Request(msg)`         -> `request failed: <msg>`
    ///   - `Http { status, msg }` -> `remote returned status <status>: <message>`
    ///   - `Decode(msg)`          -> `failed to decode response: <msg>`
    ///
    /// `src/command/lfs.rs::map_lock_list_error` and downstream
    /// `LBR-NET-*` / `LBR-AUTH-002` mappings depend on this exact shape
    /// to keep human and JSON stable-code surfaces consistent.
    #[test]
    fn lock_list_error_display_pins_each_variant() {
        let req = LockListError::Request("connection refused".to_string());
        assert_eq!(req.to_string(), "request failed: connection refused");

        let http = LockListError::Http {
            status: StatusCode::FORBIDDEN,
            message: "you must have push access to verify locks".to_string(),
        };
        assert_eq!(
            http.to_string(),
            "remote returned status 403 Forbidden: \
             you must have push access to verify locks",
        );

        let decode = LockListError::Decode("expected `objects` field".to_string());
        assert_eq!(
            decode.to_string(),
            "failed to decode response: expected `objects` field",
        );
    }

    #[tokio::test]
    async fn test_push_object() {
        if std::env::var("LIBRA_TEST_MEGA_SERVER").map_or(true, |v| v.is_empty()) {
            eprintln!("skipped (LIBRA_TEST_MEGA_SERVER not set)");
            return;
        }
        use tempfile::tempdir;

        use crate::utils::lfs;

        // Create a temporary directory and test file
        let temp_dir = tempdir().unwrap();
        let test_file = temp_dir
            .path()
            .join("git-2d187177923cd618a75da6c6db45bb89d92bd504.pack");

        // Write test content (simulating a pack file)
        let test_content = b"Sample pack file content for LFS push testing";
        std::fs::write(&test_file, test_content).unwrap();

        // Create client and calculate OID
        let client = LFSClient::from_url(&Url::parse("http://localhost:8000").unwrap());
        let oid = lfs::calc_lfs_file_hash(&test_file).unwrap();

        // Test push
        match client.push_object(&oid, &test_file).await {
            Ok(_) => println!("Pushed successfully."),
            Err(err) => eprintln!("Push failed: {err:?}"),
        }

        // temp_dir automatically cleans up when dropped
    }

    /// Verify that a batch response with a 404 object error triggers the
    /// pointer-file fallback and returns `Ok`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_download_object_404_writes_pointer_file() {
        use std::io::ErrorKind;

        use axum::{Router, routing::post};

        let test_oid = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let test_size: u64 = 1024;

        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == ErrorKind::PermissionDenied => return,
            Err(err) => panic!("failed to bind local LFS test server: {err}"),
        };
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/objects/batch",
            post(|| async {
                r#"{"objects":[{"oid":"abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890","size":1024,"error":{"code":404,"message":"Object does not exist on the server"}}]}"#
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{addr}/");
        let client = LFSClient {
            batch_url: Url::parse(&format!("{base_url}objects/batch")).unwrap(),
            lfs_url: Url::parse(&base_url).unwrap(),
            client: Client::builder().no_proxy().build().unwrap(),
        };

        let tmp_dir = tempfile::tempdir().unwrap();
        let out_path = tmp_dir.path().join("test_lfs_file");

        let result = client
            .download_object(test_oid, test_size, &out_path, None)
            .await;

        assert!(
            result.is_ok(),
            "download_object should return Ok on 404, got: {:?}",
            result.unwrap_err()
        );

        let contents = tokio::fs::read_to_string(&out_path).await.unwrap();
        let expected = lfs::format_pointer_string(test_oid, test_size);
        assert_eq!(contents, expected, "file should contain the LFS pointer");
    }

    fn test_lfs_client(base_url: &str) -> LFSClient {
        let mut client = LFSClient::from_remote_url(base_url).unwrap();
        client.client = Client::builder().no_proxy().build().unwrap();
        client
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_locks_returns_lock_list_from_mock_server() {
        use axum::{Json, Router, routing::get};
        use serde_json::json;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/locks",
            get(|| async {
                Json(json!({
                    "locks": [{
                        "id": "lock-1",
                        "path": "tracked.txt",
                        "locked_at": "2026-01-01T00:00:00Z",
                        "owner": { "name": "tester" }
                    }],
                    "next_cursor": ""
                }))
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let result = client
            .get_locks(LockListQuery {
                path: "tracked.txt".to_string(),
                id: String::new(),
                cursor: String::new(),
                limit: "10".to_string(),
                refspec: "refs/heads/main".to_string(),
            })
            .await
            .expect("get_locks should parse successful mock response");
        assert_eq!(result.locks.len(), 1);
        assert_eq!(result.locks[0].id, "lock-1");
        assert_eq!(result.locks[0].path, "tracked.txt");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_locks_maps_forbidden_to_http_error() {
        use axum::{Router, routing::get};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/locks", get(|| async { StatusCode::FORBIDDEN }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let err = client
            .get_locks(LockListQuery {
                path: String::new(),
                id: String::new(),
                cursor: String::new(),
                limit: String::new(),
                refspec: String::new(),
            })
            .await
            .expect_err("forbidden lock list should return an HTTP error");
        match err {
            LockListError::Http { status, .. } => assert_eq!(status, StatusCode::FORBIDDEN),
            other => panic!("expected LockListError::Http, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lock_returns_conflict_status_from_mock_server() {
        use axum::{Router, routing::post};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/locks", post(|| async { StatusCode::CONFLICT }));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let code = client
            .lock("tracked.txt".to_string(), "refs/heads/main".to_string())
            .await
            .expect("lock should reach mock server and return a status");
        assert_eq!(code, StatusCode::CONFLICT);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unlock_returns_unexpected_status_from_mock_server() {
        use axum::{Router, routing::post};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/locks/{id}/unlock",
            post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let code = client
            .unlock("lock-1".to_string(), "refs/heads/main".to_string(), false)
            .await
            .expect("unlock should reach mock server and return a status");
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Pre-v0.17.1063 `LFSClient::lock` / `::unlock` called
    /// `.expect("LFS … request failed after retries (...)")` on the
    /// `BasicAuth::send` result, which panicked on any network error.
    /// They now return `Result<StatusCode, reqwest::Error>` so callers
    /// can propagate a typed CliError instead. Pinning the new contract
    /// by pointing the client at a closed loopback port: the request
    /// must surface as an `Err`, not a panic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lock_returns_err_on_connection_refused_instead_of_panicking() {
        // Bind, capture the port, then drop the listener so the port
        // is closed before the request runs. This produces a clean
        // ECONNREFUSED that reqwest surfaces as a `reqwest::Error`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);

        let result = client
            .lock("tracked.txt".to_string(), "refs/heads/main".to_string())
            .await;
        assert!(
            result.is_err(),
            "expected Err on closed port, got Ok({:?})",
            result.ok()
        );

        let result = client
            .unlock("lock-1".to_string(), "refs/heads/main".to_string(), false)
            .await;
        assert!(
            result.is_err(),
            "expected Err on closed port, got Ok({:?})",
            result.ok()
        );
    }

    /// Same fix shape as `lock_returns_err_on_connection_refused_*` but
    /// for `verify_locks`. Pre-v0.17.1064 `verify_locks` returned
    /// `(StatusCode, VerifiableLockList)` and panicked on a network
    /// error or a malformed JSON body via two separate `.expect(...)`s.
    /// It now returns `Result<(...), reqwest::Error>` so `push_objects`
    /// can surface a `LfsPushError` instead of crashing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn verify_locks_returns_err_on_connection_refused_instead_of_panicking() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);

        let result = client
            .verify_locks(VerifiableLockRequest {
                refs: Ref {
                    name: "refs/heads/main".to_string(),
                },
                ..Default::default()
            })
            .await;
        assert!(
            result.is_err(),
            "expected Err on closed port, got Ok({:?})",
            result.ok()
        );
    }

    /// Regression for v0.17.194: `LFSClient::download_object` previously
    /// `obj.actions.as_ref().unwrap().get(&Action::Download).unwrap()` and
    /// would crash if the LFS server returned a valid batch response but
    /// omitted the `download` action (for example, an upload-only mirror,
    /// or a partial-permission token). After v0.17.194 the inner unwrap is
    /// `ok_or_else(|| anyhow!("…missing 'download' action for oid {oid}"))`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn download_object_rejects_batch_response_missing_download_action() {
        use axum::{Router, routing::post};

        let test_oid = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/objects/batch",
            post(|| async {
                // Object is present, no error, but `actions` only carries an upload
                // action — the download action is intentionally missing.
                r#"{"objects":[{"oid":"abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890","size":4,"actions":{"upload":{"href":"http://example.invalid/up","header":{},"expires_at":"2099-01-01T00:00:00Z"}}}]}"#
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let tmp_dir = tempfile::tempdir().unwrap();
        let out_path = tmp_dir.path().join("missing_download");
        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);

        let err = client
            .download_object(test_oid, 4, &out_path, None)
            .await
            .expect_err("missing download action should surface a typed error");
        let msg = err.to_string();
        assert!(
            msg.contains("missing 'download' action") && msg.contains(test_oid),
            "unexpected error: {msg}"
        );
    }

    /// Regression for v0.17.193: `LFSClient::push_object` previously
    /// `assert_eq!(resp.objects.len(), 1, ...)` and would crash the entire
    /// binary if the LFS server returned a batch response with the wrong
    /// number of objects. After v0.17.193 it returns a typed `LfsPushError`
    /// whose detail names the actual object count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn push_object_rejects_batch_response_with_zero_objects() {
        use axum::{Router, routing::post};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/objects/batch",
            post(|| async {
                // Server-side bug: returns an empty objects array.
                r#"{"transfer":"basic","objects":[],"hash_algo":"sha256"}"#
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let tmp_dir = tempfile::tempdir().unwrap();
        let file_path = tmp_dir.path().join("payload.bin");
        tokio::fs::write(&file_path, b"hello").await.unwrap();

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let err = client
            .push_object("deadbeef", &file_path)
            .await
            .expect_err("empty objects array should be rejected by push_object");
        assert!(
            err.detail.contains("LFS batch upload returned 0 objects"),
            "unexpected detail: {}",
            err.detail
        );
    }

    /// Regression for the upload-side missing-action branch in
    /// `LFSClient::upload_object` (src/internal/protocol/lfs_client.rs:365):
    /// when the LFS batch endpoint returns a well-formed response whose
    /// object lacks an `upload` action (for example, a download-only mirror
    /// or a partial-permission token), `push_object` must surface a typed
    /// `LfsPushError` whose detail names the missing action, not panic.
    /// Mirrors the analogous `download_object_rejects_batch_response_missing_download_action`
    /// regression test, inverting upload↔download.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn push_object_rejects_batch_response_missing_upload_action() {
        use axum::{Router, routing::post};

        let test_oid = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/objects/batch",
            post(|| async {
                // Object is present, no error, but `actions` only carries a download
                // action — the upload action is intentionally missing.
                r#"{"objects":[{"oid":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef","size":5,"actions":{"download":{"href":"http://example.invalid/dl","header":{},"expires_at":"2099-01-01T00:00:00Z"}}}]}"#
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let tmp_dir = tempfile::tempdir().unwrap();
        let file_path = tmp_dir.path().join("payload.bin");
        tokio::fs::write(&file_path, b"hello").await.unwrap();

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let err = client
            .push_object(test_oid, &file_path)
            .await
            .expect_err("missing upload action should surface a typed LfsPushError");
        assert!(
            err.detail
                .contains("remote did not provide an upload action"),
            "unexpected detail: {}",
            err.detail
        );
        assert_eq!(err.oid.as_deref(), Some(test_oid));
    }

    /// Batch protocol contract (lfs.md): when the `objects/batch` response
    /// carries an explicit `error` object for the requested oid, `push_object`
    /// must surface a typed [`LfsPushError`] whose detail echoes the remote
    /// code/message and whose `oid` is the requested object — never panic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn push_object_rejects_batch_response_with_error_object() {
        use axum::{Router, routing::post};

        let test_oid = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        // Restricted sandboxes may forbid binding a loopback port; skip
        // rather than fail, mirroring the guarded lock-API mock test above.
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipped (loopback bind not permitted in this environment)");
                return;
            }
            Err(err) => panic!("failed to bind mock LFS listener: {err}"),
        };
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/objects/batch",
            post(|| async {
                // The object is returned with an explicit error block (e.g. the
                // remote rejected the upload). No actions are provided.
                r#"{"objects":[{"oid":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef","size":5,"error":{"code":422,"message":"object is invalid"}}]}"#
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let tmp_dir = tempfile::tempdir().unwrap();
        let file_path = tmp_dir.path().join("payload.bin");
        tokio::fs::write(&file_path, b"hello").await.unwrap();

        let base_url = format!("http://{addr}/");
        let client = test_lfs_client(&base_url);
        let err = client.push_object(test_oid, &file_path).await.expect_err(
            "an error object in the batch response should surface a typed LfsPushError",
        );
        assert!(
            err.detail.contains("remote reported error 422")
                && err.detail.contains("object is invalid"),
            "unexpected detail: {}",
            err.detail
        );
        assert_eq!(err.oid.as_deref(), Some(test_oid));
    }
}
