//! Paged FastCDC upload/download over the authenticated Mega media extension.
//!
//! Ordinary LFS remains the fallback only before a transfer starts. Once a
//! manifest is selected, authentication, hash, and protocol failures fail
//! closed. Prepare submits a bounded summary; pages, seal, paged missing, and
//! a durable finalize task carry the layout. A full manifest body is retried
//! only when a server still requires it and the compact JSON fits in the 1 MiB
//! envelope.

use std::{
    collections::HashSet,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use url::Url;

use super::{
    capability,
    chunk_store::{self, DiskMediaIndex, MediaChunkStore},
    chunker, is_sha256_hex,
    manifest::{
        self, ChunkEntry, CursorGuard, MAX_ENVELOPE_SIZE, MAX_PAGE_ENTRIES, ManifestPage,
        ManifestSummary, MediaManifest, advance_coverage, non_final_page_is_longest,
        parse_page_envelope,
    },
    negotiate::{self, ProbeOutcome, TransferDecision},
    sha256_hex,
};

const NO_PROGRESS: Duration = Duration::from_secs(10 * 60);
const MAX_FINALIZE_REQUEUE: u32 = 3;
const MAX_QUEUE_RETRIES: u32 = 3;
/// Lower bound used to skip a full-manifest retry that cannot fit the envelope.
const MIN_ENTRY_WIRE: u64 = 96;

#[derive(Deserialize)]
struct PrepareResponse {
    manifest_id: String,
}

#[derive(Deserialize)]
struct SealBody {
    manifest_id: String,
    seal_generation: i64,
    page_count: u32,
}

#[derive(Deserialize)]
struct MissingBody {
    hashes: Vec<String>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct AcceptedBody {
    task_id: String,
    manifest_id: String,
    status_url: String,
}

#[derive(Debug, Deserialize)]
struct TaskBody {
    task_id: String,
    manifest_id: String,
    state: String,
    #[serde(default)]
    bytes_verified: u64,
    #[serde(default)]
    pages_verified: i32,
    #[serde(default)]
    retryable: bool,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize)]
struct FinalizedPageItem {
    page_no: u32,
    offset_start: u64,
    offset_end: u64,
    entries: Vec<ChunkEntry>,
}

#[derive(Deserialize)]
struct FinalizedPagesBody {
    manifest_id: String,
    #[serde(default)]
    pages: Vec<FinalizedPageItem>,
    #[serde(default)]
    next_cursor: Option<String>,
}

struct PublishedId {
    manifest_id: String,
    oid: String,
    size: u64,
}

#[derive(Debug)]
struct PlannedPut {
    hash: String,
    offset: u64,
    length: u64,
}

/// Trust boundary for range export (C-07): local bytes are trimmed covering
/// chunks from an authenticated finalized summary; they do not independently
/// recompute the full-object canonical id or SHA-256.
pub const TRUST_BOUNDARY_C07: &str = "authenticated finalized summary and covering chunks; local range bytes do not independently prove full-object oid/SHA-256";

/// Outcome of [`MediaClient::export_range`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct RangeExportReport {
    pub manifest_id: String,
    pub oid: String,
    pub size: u64,
    pub offset: u64,
    pub length: u64,
    pub covering_chunks: u64,
    pub downloaded_chunks: u64,
    pub trust_boundary: &'static str,
}

pub struct MediaClient {
    client: Client,
    base: Url,
    token: Option<String>,
    max_manifest: usize,
}

impl MediaClient {
    pub async fn discover(
        client: Client,
        lfs_url: &Url,
        local_fallback: bool,
    ) -> Result<Option<Self>> {
        let outcome = capability::probe_with_client(lfs_url.as_str(), client.clone()).await;
        // A chunk-only advertisement must not block a basic transfer the batch
        // endpoint already offered. Insufficient paging falls out of negotiate.
        if let ProbeOutcome::Ok(caps) = &outcome
            && (!caps.keeps_standard_fallback() || caps.max_manifest_size == 0)
        {
            return Ok(None);
        }
        match negotiate::negotiate(&outcome, true, local_fallback) {
            TransferDecision::StandardLfs { .. } => return Ok(None),
            TransferDecision::Block { reason } => {
                bail!("FastCDC transfer blocked: {}", reason.as_str())
            }
            TransferDecision::Chunked { .. } => (),
        }
        let ProbeOutcome::Ok(caps) = outcome else {
            return Ok(None);
        };
        let mut base = lfs_url.clone();
        base.set_path(&format!(
            "{}/libra/media/v1/",
            lfs_url.path().trim_end_matches('/')
        ));
        base.set_fragment(None);
        let token = match crate::internal::auth::HostScope::from_request_url(&base) {
            Some(scope) => match crate::internal::auth::lookup(&scope).await {
                crate::internal::auth::Lookup::Valid { token, .. } => Some(token),
                _ => None,
            },
            None => None,
        };
        Ok(Some(Self {
            client,
            base,
            token,
            max_manifest: usize::try_from(caps.max_manifest_size.min(MAX_ENVELOPE_SIZE as u64))
                .unwrap_or(MAX_ENVELOPE_SIZE),
        }))
    }

    fn url_for(&self, path: &str, extra: &[(&str, &str)]) -> Result<Url> {
        let mut url = self.base.clone();
        let existing: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        url.set_path(&format!("{}{path}", self.base.path()));
        url.set_query(None);
        if existing.is_empty() && extra.is_empty() {
            return Ok(url);
        }
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in &existing {
                pairs.append_pair(key, value);
            }
            for (key, value) in extra {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        self.request_query(method, path, &[])
    }

    fn request_query(
        &self,
        method: Method,
        path: &str,
        extra: &[(&str, &str)],
    ) -> Result<RequestBuilder> {
        let url = self.url_for(path, extra)?;
        let mut request = self
            .client
            .request(method, url)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(120));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        Ok(request)
    }

    async fn read_limited(&self, response: Response, limit: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(part) = stream.next().await {
            let part = part.context("failed to read FastCDC response")?;
            if part.len() > limit.saturating_sub(bytes.len()) {
                bail!("FastCDC response exceeds size limit");
            }
            bytes.extend_from_slice(&part);
        }
        Ok(bytes)
    }

    async fn json<T: DeserializeOwned>(&self, response: Response) -> Result<T> {
        let bytes = self.read_limited(response, self.max_manifest).await?;
        serde_json::from_slice(&bytes).context("invalid FastCDC response JSON")
    }

    async fn post_json(&self, path: &str, body: Vec<u8>) -> Result<Response> {
        self.request(Method::POST, path)?
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .context("FastCDC request failed")
    }

    async fn expect_success(response: Response, action: &str) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        bail!("FastCDC {action} rejected (HTTP {status})")
    }

    async fn read_by_media(&self, oid: &str) -> Result<Option<PublishedId>> {
        let response = self
            .request(Method::GET, &format!("manifests/by-media/{oid}"))?
            .send()
            .await
            .context("FastCDC request failed")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = Self::expect_success(response, "by-media read").await?;
        let bytes = self.read_limited(response, self.max_manifest).await?;
        Ok(Some(parse_published(&bytes)?))
    }

    /// Returns true when the object is published, or after a paged upload and
    /// a completed finalize task. Protocol failures after prepare are errors.
    ///
    /// Prefers a pre-saved local layout keyed by `oid` (from `media chunk
    /// --store`, optionally with `--prior-manifest`). Before prepare the source
    /// size, full-file oid, and per-chunk hashes are re-checked; a mismatch
    /// fails closed without contacting the remote. A missing/evicted cache
    /// falls back to a cold cut.
    pub async fn upload(&self, oid: &str, size: u64, path: &Path) -> Result<bool> {
        if !is_sha256_hex(oid) {
            bail!("invalid LFS object SHA-256");
        }
        if let Some(published) = self.read_by_media(oid).await? {
            if published.oid != oid
                || published.size != size
                || !is_sha256_hex(&published.manifest_id)
            {
                bail!("FastCDC manifest does not match requested LFS object");
            }
            return Ok(true);
        }
        let root = chunk_store::repo_media_root()?;
        let source = path.to_path_buf();
        let root_for_stream = root.clone();
        if chunk_store::verify_cached_layout(&root, oid, size, path)?.is_none() {
            let outcome = tokio::task::spawn_blocking(move || {
                chunk_store::stream_media_file(&source, &root_for_stream, true)
            })
            .await
            .context("FastCDC paging task failed")??;
            if outcome.summary.oid != oid || outcome.summary.size != size {
                bail!("local LFS object size or SHA-256 mismatch");
            }
        }
        // ADR-FL-04: re-check source size / oid / per-chunk hashes before prepare.
        let summary = chunk_store::verify_cached_layout(&root, oid, size, path)?
            .ok_or_else(|| anyhow::anyhow!("FastCDC layout missing before prepare"))?;
        self.prepare(&root, &summary).await?;
        self.put_pages(&root, &summary).await?;
        self.seal(&summary).await?;
        self.upload_missing(path, &root, &summary).await?;
        self.wait_finalize(&summary).await?;
        Ok(true)
    }

    async fn prepare(&self, root: &Path, summary: &ManifestSummary) -> Result<()> {
        let summary_body = serde_json::to_vec(summary).context("invalid FastCDC summary JSON")?;
        if summary_body.len() > MAX_ENVELOPE_SIZE {
            bail!("FastCDC summary exceeds size limit");
        }
        let response = self.post_json("manifests", summary_body).await?;
        if response.status() == StatusCode::BAD_REQUEST {
            let Some(body) = try_full_manifest_body(root, summary)? else {
                bail!(
                    "FastCDC prepare rejected the summary and the layout exceeds the 1 MiB envelope"
                );
            };
            let response = self.post_json("manifests", body).await?;
            return self.accept_prepare(response, summary).await;
        }
        self.accept_prepare(response, summary).await
    }

    async fn accept_prepare(&self, response: Response, summary: &ManifestSummary) -> Result<()> {
        let response = Self::expect_success(response, "prepare").await?;
        let prepared: PrepareResponse = self.json(response).await?;
        if prepared.manifest_id != summary.manifest_id {
            bail!("remote returned a manifest ID that does not match the uploaded manifest");
        }
        Ok(())
    }

    async fn put_pages(&self, root: &Path, summary: &ManifestSummary) -> Result<()> {
        let mut covered = 0u64;
        for page_no in 0..summary.page_count {
            let page =
                chunk_store::load_local_page(root, &summary.oid, &summary.manifest_id, page_no)?;
            if page.page_no != page_no {
                bail!("stored page_no {} does not match {page_no}", page.page_no);
            }
            if page_no + 1 < summary.page_count {
                let next = chunk_store::load_local_page(
                    root,
                    &summary.oid,
                    &summary.manifest_id,
                    page_no + 1,
                )?;
                let Some(first) = next.entries.first() else {
                    bail!("FastCDC page is empty");
                };
                non_final_page_is_longest(&page.entries, first)?;
            }
            for entry in &page.entries {
                covered = advance_coverage(covered, entry)?;
            }
            let bytes = serde_json::to_vec(&page).context("invalid FastCDC page JSON")?;
            parse_page_envelope(&bytes)?;
            let response = self
                .request(
                    Method::PUT,
                    &format!("manifests/{}/pages/{page_no}", summary.manifest_id),
                )?
                .header("Content-Type", "application/json")
                .body(bytes)
                .send()
                .await
                .context("FastCDC request failed")?;
            Self::expect_success(response, "page upload").await?;
        }
        if covered != summary.size {
            bail!("FastCDC pages do not cover the object");
        }
        Ok(())
    }

    async fn seal(&self, summary: &ManifestSummary) -> Result<()> {
        let response = self
            .request(
                Method::POST,
                &format!("manifests/{}/seal", summary.manifest_id),
            )?
            .send()
            .await
            .context("FastCDC request failed")?;
        let response = Self::expect_success(response, "seal").await?;
        let sealed: SealBody = self.json(response).await?;
        if sealed.manifest_id != summary.manifest_id
            || sealed.page_count != summary.page_count
            || sealed.seal_generation < 0
        {
            bail!("FastCDC seal does not match the local layout");
        }
        Ok(())
    }

    async fn upload_missing(
        &self,
        path: &Path,
        root: &Path,
        summary: &ManifestSummary,
    ) -> Result<()> {
        let mut index = chunk_store::open_index(root, &summary.manifest_id)?;
        let mut cursor: Option<String> = None;
        let mut guard = CursorGuard::new();
        let mut source = tokio::fs::File::open(path)
            .await
            .context("cannot reopen LFS source")?;
        loop {
            let extra: Vec<(&str, String)> = cursor
                .as_ref()
                .map(|value| vec![("cursor", value.clone())])
                .unwrap_or_default();
            let extra_ref: Vec<(&str, &str)> =
                extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let response = self
                .request_query(
                    Method::GET,
                    &format!("manifests/{}/missing", summary.manifest_id),
                    &extra_ref,
                )?
                .send()
                .await
                .context("FastCDC request failed")?;
            let response = Self::expect_success(response, "missing chunks").await?;
            let page: MissingBody = self.json(response).await?;
            if page.hashes.len() > MAX_PAGE_ENTRIES {
                bail!("missing page exceeds max_page_entries");
            }
            if page.hashes.is_empty() && page.next_cursor.is_some() {
                bail!("missing cursor did not advance");
            }
            guard.observe(page.next_cursor.as_deref())?;
            for put in plan_missing_puts(&mut index, &page.hashes)? {
                let bytes = read_span(&mut source, put.offset, put.length).await?;
                if sha256_hex(&bytes) != put.hash {
                    bail!("LFS source changed while uploading");
                }
                let response = self
                    .request(
                        Method::PUT,
                        &format!("manifests/{}/chunks/{}", summary.manifest_id, put.hash),
                    )?
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes)
                    .send()
                    .await
                    .context("FastCDC request failed")?;
                Self::expect_success(response, "chunk upload").await?;
            }
            match page.next_cursor {
                None => break,
                Some(next) => cursor = Some(next),
            }
        }
        Ok(())
    }

    async fn post_finalize(&self, manifest_id: &str) -> Result<AcceptedBody> {
        for attempt in 0..=MAX_QUEUE_RETRIES {
            let response = self
                .request(Method::POST, &format!("manifests/{manifest_id}/finalize"))?
                .send()
                .await
                .context("FastCDC request failed")?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                if attempt == MAX_QUEUE_RETRIES {
                    bail!("FastCDC finalize rejected (HTTP 429)");
                }
                tokio::time::sleep(retry_after(&response)).await;
                continue;
            }
            if response.status() != StatusCode::ACCEPTED {
                bail!("FastCDC finalize rejected (HTTP {})", response.status());
            }
            let accepted: AcceptedBody = self.json(response).await?;
            if accepted.manifest_id != manifest_id {
                bail!("finalize task manifest_id mismatch");
            }
            same_origin_task_path(&accepted.status_url, &accepted.task_id)?;
            return Ok(accepted);
        }
        bail!("FastCDC finalize rejected (HTTP 429)")
    }

    async fn wait_finalize(&self, summary: &ManifestSummary) -> Result<()> {
        let mut accepted = self.post_finalize(&summary.manifest_id).await?;
        let mut requeues = 0u32;
        let mut last_change = Instant::now();
        let mut last_bytes = 0u64;
        let mut last_pages = -1i32;
        loop {
            let path = same_origin_task_path(&accepted.status_url, &accepted.task_id)?;
            let response = self
                .request(Method::GET, &path)?
                .send()
                .await
                .context("FastCDC request failed")?;
            let response = Self::expect_success(response, "task status").await?;
            let task: TaskBody = self.json(response).await?;
            if task.task_id != accepted.task_id || task.manifest_id != summary.manifest_id {
                bail!("finalize task manifest_id mismatch");
            }
            match task.state.as_str() {
                "complete" => {
                    accept_finalize_complete(summary, &task)?;
                    return Ok(());
                }
                "failed" if task.retryable => {
                    requeues += 1;
                    if requeues > MAX_FINALIZE_REQUEUE {
                        bail!("finalize retries exhausted");
                    }
                    let again = self.post_finalize(&summary.manifest_id).await?;
                    if again.task_id != accepted.task_id {
                        bail!("finalize task_id changed");
                    }
                    accepted = again;
                    last_change = Instant::now();
                }
                "failed" => {
                    bail!(
                        "FastCDC finalize failed ({})",
                        task.error_code.as_deref().unwrap_or("failed")
                    );
                }
                "pending" | "running" => {
                    if task.bytes_verified != last_bytes || task.pages_verified != last_pages {
                        last_bytes = task.bytes_verified;
                        last_pages = task.pages_verified;
                        last_change = Instant::now();
                    } else if last_change.elapsed() > NO_PROGRESS {
                        bail!("FastCDC finalize made no progress");
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                _ => bail!("FastCDC finalize returned an unknown state"),
            }
        }
    }

    /// Returns false only when no finalized manifest exists. The download pins
    /// `manifest_id` and checks the reassembled SHA-256 before replacing `path`.
    pub async fn download(
        &self,
        oid: &str,
        size: u64,
        path: &Path,
        store: &MediaChunkStore,
    ) -> Result<bool> {
        if !is_sha256_hex(oid) {
            bail!("invalid LFS object SHA-256");
        }
        let Some(published) = self.read_by_media(oid).await? else {
            return Ok(false);
        };
        if published.oid != oid || published.size != size {
            bail!("FastCDC manifest does not match requested LFS object");
        }
        let summary = self.finalized_summary(&published.manifest_id).await?;
        if summary.manifest_id != published.manifest_id
            || summary.oid != oid
            || summary.size != size
        {
            bail!("FastCDC manifest does not match requested LFS object");
        }
        let (layout, chunks) = store.resolved_cache();
        self.fetch_pages(&layout, &summary).await?;
        self.fetch_chunks(&layout, &chunks, &summary).await?;
        chunk_store::write_summary(&layout, &summary)?;
        chunk_store::reassemble_paged(&layout, &summary, path)?;
        Ok(true)
    }

    /// Discover a client for explicit range export. Ordinary LFS fallback is a
    /// hard refusal (C-03 / ADR-FL-03).
    pub async fn discover_for_range(client: Client, lfs_url: &Url) -> Result<Self> {
        let outcome = capability::probe_with_client(lfs_url.as_str(), client.clone()).await;
        match negotiate::negotiate_range_export(&outcome, true) {
            TransferDecision::Chunked { .. } => {}
            TransferDecision::StandardLfs { reason } => {
                bail!("FastCDC range export blocked: {}", reason.as_str())
            }
            TransferDecision::Block { reason } => {
                bail!("FastCDC range export blocked: {}", reason.as_str())
            }
        }
        let ProbeOutcome::Ok(caps) = outcome else {
            bail!("FastCDC range export blocked: remote has no media capability");
        };
        let mut base = lfs_url.clone();
        base.set_path(&format!(
            "{}/libra/media/v1/",
            lfs_url.path().trim_end_matches('/')
        ));
        base.set_fragment(None);
        let token = match crate::internal::auth::HostScope::from_request_url(&base) {
            Some(scope) => match crate::internal::auth::lookup(&scope).await {
                crate::internal::auth::Lookup::Valid { token, .. } => Some(token),
                _ => None,
            },
            None => None,
        };
        Ok(Self {
            client,
            base,
            token,
            max_manifest: usize::try_from(caps.max_manifest_size.min(MAX_ENVELOPE_SIZE as u64))
                .unwrap_or(MAX_ENVELOPE_SIZE),
        })
    }

    /// Export `[offset, offset+length)` into a new file without hydrating the
    /// whole object. Pins `manifest_id`, fetches only covering pages, and GETs
    /// the unique covering hashes missing from a valid local cache.
    pub async fn export_range(
        &self,
        oid: &str,
        size: u64,
        offset: u64,
        length: u64,
        path: &Path,
        store: &MediaChunkStore,
    ) -> Result<RangeExportReport> {
        if !is_sha256_hex(oid) {
            bail!("invalid LFS object SHA-256");
        }
        chunk_store::validate_byte_range(size, offset, length)?;
        let Some(published) = self.read_by_media(oid).await? else {
            bail!("FastCDC range export requires a finalized Media object");
        };
        if published.oid != oid || published.size != size {
            bail!("FastCDC manifest does not match requested LFS object");
        }
        let summary = self.finalized_summary(&published.manifest_id).await?;
        if summary.manifest_id != published.manifest_id
            || summary.oid != oid
            || summary.size != size
        {
            bail!("FastCDC manifest does not match requested LFS object");
        }
        let (layout, chunks) = store.resolved_cache();
        let covering = if length == 0 {
            Vec::new()
        } else {
            self.fetch_covering_pages(&layout, &summary, offset, length)
                .await?
        };
        let mut unique = HashSet::new();
        let mut downloaded = 0u64;
        for entry in &covering {
            if !unique.insert(entry.chunk_hash.clone()) {
                continue;
            }
            if chunks
                .get_chunk(&entry.chunk_hash)
                .ok()
                .is_some_and(|bytes| bytes.len() as u64 == entry.length)
            {
                continue;
            }
            let response = self
                .request(
                    Method::GET,
                    &format!(
                        "finalized/{}/chunks/{}",
                        summary.manifest_id, entry.chunk_hash
                    ),
                )?
                .send()
                .await
                .context("FastCDC request failed")?;
            let response = Self::expect_success(response, "chunk download").await?;
            let bytes = self.read_limited(response, chunker::MAX_SIZE).await?;
            if bytes.len() as u64 != entry.length || sha256_hex(&bytes) != entry.chunk_hash {
                bail!("FastCDC chunk size or SHA-256 mismatch");
            }
            chunks.put_chunk(&bytes)?;
            downloaded += 1;
        }
        chunk_store::write_range_export(&chunks, &covering, offset, length, path)?;
        Ok(RangeExportReport {
            manifest_id: summary.manifest_id,
            oid: summary.oid,
            size: summary.size,
            offset,
            length,
            covering_chunks: unique.len() as u64,
            downloaded_chunks: downloaded,
            trust_boundary: TRUST_BOUNDARY_C07,
        })
    }

    async fn fetch_covering_pages(
        &self,
        layout: &Path,
        summary: &ManifestSummary,
        offset: u64,
        length: u64,
    ) -> Result<Vec<manifest::ChunkEntry>> {
        let mut covering = Vec::new();
        let mut cursor: Option<String> = None;
        let mut guard = CursorGuard::new();
        let end = offset
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("range offset+length overflow"))?;
        let offset_s = offset.to_string();
        let length_s = length.to_string();
        loop {
            let mut extras: Vec<(&str, String)> = Vec::new();
            if let Some(value) = cursor.as_ref() {
                extras.push(("cursor", value.clone()));
            } else {
                extras.push(("offset", offset_s.clone()));
                extras.push(("length", length_s.clone()));
            }
            let extra_ref: Vec<(&str, &str)> =
                extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let response = self
                .request_query(
                    Method::GET,
                    &format!("finalized/{}/pages", summary.manifest_id),
                    &extra_ref,
                )?
                .send()
                .await
                .context("FastCDC request failed")?;
            let response = Self::expect_success(response, "finalized covering pages").await?;
            let body: FinalizedPagesBody = self.json(response).await?;
            if body.manifest_id != summary.manifest_id {
                bail!("FastCDC page does not match the pinned manifest");
            }
            if body.pages.is_empty() {
                break;
            }
            guard.observe(body.next_cursor.as_deref())?;
            for item in body.pages {
                let page = ManifestPage {
                    page_no: item.page_no,
                    entries: item.entries,
                };
                let bytes = serde_json::to_vec(&page).context("invalid FastCDC page JSON")?;
                parse_page_envelope(&bytes)?;
                let mut covered = item.offset_start;
                for entry in &page.entries {
                    if entry.offset != covered {
                        bail!(
                            "finalized covering page offset {} breaks contiguity (expected {covered})",
                            entry.offset
                        );
                    }
                    covered = advance_coverage(covered, entry)?;
                    let chunk_end = entry
                        .offset
                        .checked_add(entry.length)
                        .ok_or_else(|| anyhow::anyhow!("chunk offset overflow"))?;
                    if entry.offset < end && chunk_end > offset {
                        covering.push(entry.clone());
                    }
                }
                if item.offset_end != covered {
                    bail!("finalized page offset range does not match its entries");
                }
                chunk_store::write_page(layout, &summary.oid, &page)?;
            }
            match body.next_cursor {
                None => break,
                Some(next) => cursor = Some(next),
            }
        }
        covering.sort_by_key(|entry| entry.offset);
        covering.dedup_by_key(|entry| entry.offset);
        if covering.is_empty() {
            bail!("FastCDC covering pages do not intersect the requested range");
        }
        let first = covering
            .first()
            .expect("INVARIANT: covering non-empty after is_empty check");
        let last = covering
            .last()
            .expect("INVARIANT: covering non-empty after is_empty check");
        let last_end = last
            .offset
            .checked_add(last.length)
            .ok_or_else(|| anyhow::anyhow!("chunk offset overflow"))?;
        if first.offset > offset || last_end < end {
            bail!("FastCDC covering pages do not fully cover the requested range");
        }
        Ok(covering)
    }

    async fn finalized_summary(&self, manifest_id: &str) -> Result<ManifestSummary> {
        let response = self
            .request(Method::GET, &format!("finalized/{manifest_id}"))?
            .send()
            .await
            .context("FastCDC request failed")?;
        let response = Self::expect_success(response, "finalized summary").await?;
        let bytes = self.read_limited(response, self.max_manifest).await?;
        let text = std::str::from_utf8(&bytes).context("invalid FastCDC response JSON")?;
        Ok(ManifestSummary::from_json(text)?)
    }

    async fn fetch_pages(&self, layout: &Path, summary: &ManifestSummary) -> Result<()> {
        if summary.page_count == 0 {
            if summary.size != 0 || summary.chunk_count != 0 {
                bail!("FastCDC manifest does not match requested LFS object");
            }
            return Ok(());
        }
        let mut expected = 0u32;
        let mut covered = 0u64;
        let mut cursor: Option<String> = None;
        let mut guard = CursorGuard::new();
        while expected < summary.page_count {
            let extra: Vec<(&str, String)> = cursor
                .as_ref()
                .map(|value| vec![("cursor", value.clone())])
                .unwrap_or_default();
            let extra_ref: Vec<(&str, &str)> =
                extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let response = self
                .request_query(
                    Method::GET,
                    &format!("finalized/{}/pages", summary.manifest_id),
                    &extra_ref,
                )?
                .send()
                .await
                .context("FastCDC request failed")?;
            let response = Self::expect_success(response, "finalized pages").await?;
            let body: FinalizedPagesBody = self.json(response).await?;
            if body.manifest_id != summary.manifest_id {
                bail!("FastCDC page does not match the pinned manifest");
            }
            if body.pages.is_empty() {
                bail!("finalized page stream stalled");
            }
            guard.observe(body.next_cursor.as_deref())?;
            for item in body.pages {
                if item.page_no != expected {
                    bail!(
                        "finalized page_no {} does not match {expected}",
                        item.page_no
                    );
                }
                let start = covered;
                let page = ManifestPage {
                    page_no: item.page_no,
                    entries: item.entries,
                };
                let bytes = serde_json::to_vec(&page).context("invalid FastCDC page JSON")?;
                parse_page_envelope(&bytes)?;
                for entry in &page.entries {
                    covered = advance_coverage(covered, entry)?;
                }
                if item.offset_start != start || item.offset_end != covered {
                    bail!("finalized page offset range does not match its entries");
                }
                chunk_store::write_page(layout, &summary.oid, &page)?;
                expected += 1;
            }
            match body.next_cursor {
                None => break,
                Some(next) => cursor = Some(next),
            }
        }
        if expected != summary.page_count || covered != summary.size {
            bail!("FastCDC pages do not cover the object");
        }
        Ok(())
    }

    async fn fetch_chunks(
        &self,
        layout: &Path,
        store: &MediaChunkStore,
        summary: &ManifestSummary,
    ) -> Result<()> {
        let mut index = chunk_store::prepare_index(layout, &summary.manifest_id)?;
        for page_no in 0..summary.page_count {
            let page =
                chunk_store::load_local_page(layout, &summary.oid, &summary.manifest_id, page_no)?;
            for entry in &page.entries {
                index.insert(&entry.chunk_hash, entry.length, entry.offset, page_no)?;
                if store
                    .get_chunk(&entry.chunk_hash)
                    .ok()
                    .is_some_and(|bytes| bytes.len() as u64 == entry.length)
                {
                    continue;
                }
                let response = self
                    .request(
                        Method::GET,
                        &format!(
                            "finalized/{}/chunks/{}",
                            summary.manifest_id, entry.chunk_hash
                        ),
                    )?
                    .send()
                    .await
                    .context("FastCDC request failed")?;
                let response = Self::expect_success(response, "chunk download").await?;
                let bytes = self.read_limited(response, chunker::MAX_SIZE).await?;
                if bytes.len() as u64 != entry.length || sha256_hex(&bytes) != entry.chunk_hash {
                    bail!("FastCDC chunk size or SHA-256 mismatch");
                }
                store.put_chunk(&bytes)?;
            }
        }
        index.finish()?;
        Ok(())
    }
}

fn parse_published(bytes: &[u8]) -> Result<PublishedId> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("invalid FastCDC response JSON")?;
    if let Some(manifest) = value.get("manifest") {
        let manifest_id = value
            .get("manifest_id")
            .and_then(|v| v.as_str())
            .context("invalid FastCDC response JSON")?
            .to_owned();
        let embedded: MediaManifest =
            serde_json::from_value(manifest.clone()).context("invalid FastCDC response JSON")?;
        embedded.validate()?;
        if embedded.id()? != manifest_id {
            bail!("FastCDC manifest does not match requested LFS object");
        }
        return Ok(PublishedId {
            manifest_id,
            oid: embedded.media_oid,
            size: embedded.media_size,
        });
    }
    let text = std::str::from_utf8(bytes).context("invalid FastCDC response JSON")?;
    let summary = ManifestSummary::from_json(text)?;
    Ok(PublishedId {
        manifest_id: summary.manifest_id,
        oid: summary.oid,
        size: summary.size,
    })
}

fn entries_fit_envelope(chunk_count: u64) -> bool {
    chunk_count.saturating_mul(MIN_ENTRY_WIRE) <= MAX_ENVELOPE_SIZE as u64
}

fn try_full_manifest_body(root: &Path, summary: &ManifestSummary) -> Result<Option<Vec<u8>>> {
    if !entries_fit_envelope(summary.chunk_count) {
        return Ok(None);
    }
    let mut chunks = Vec::new();
    let mut used = 512usize;
    for page_no in 0..summary.page_count {
        let page = chunk_store::load_local_page(root, &summary.oid, &summary.manifest_id, page_no)?;
        for entry in page.entries {
            let wire = serde_json::to_vec(&entry)
                .context("invalid FastCDC page JSON")?
                .len()
                .saturating_add(1);
            if used.saturating_add(wire) > MAX_ENVELOPE_SIZE.saturating_sub(256) {
                return Ok(None);
            }
            used += wire;
            chunks.push(entry);
        }
    }
    let manifest = MediaManifest {
        version: summary.version,
        algorithm: summary.algorithm.clone(),
        hash_algorithm: summary.hash_algorithm.clone(),
        media_oid: summary.oid.clone(),
        media_size: summary.size,
        chunks,
        created_by: summary
            .created_by
            .clone()
            .unwrap_or_else(manifest::local_created_by),
        fallback_oid: Some(summary.oid.clone()),
    };
    manifest.validate()?;
    if manifest.id()? != summary.manifest_id {
        bail!("local page layout does not match the summary id");
    }
    let body = serde_json::to_vec(&manifest).context("invalid FastCDC manifest JSON")?;
    if body.len() > MAX_ENVELOPE_SIZE {
        return Ok(None);
    }
    Ok(Some(body))
}

fn plan_missing_puts(index: &mut DiskMediaIndex, hashes: &[String]) -> Result<Vec<PlannedPut>> {
    let mut seen = HashSet::new();
    let mut planned = Vec::new();
    for hash in hashes {
        if !is_sha256_hex(hash) || !seen.insert(hash.clone()) {
            if !is_sha256_hex(hash) {
                bail!("remote requested a chunk outside the manifest");
            }
            continue;
        }
        let Some((offset, length)) = index.lookup_span(hash)? else {
            bail!("remote requested a chunk outside the manifest");
        };
        planned.push(PlannedPut {
            hash: hash.clone(),
            offset,
            length,
        });
    }
    Ok(planned)
}

fn same_origin_task_path(status_url: &str, task_id: &str) -> Result<String> {
    if task_id.is_empty()
        || task_id.contains('/')
        || task_id.contains('\\')
        || task_id.contains("..")
        || status_url.contains("://")
        || status_url.starts_with("//")
        || status_url.contains("..")
    {
        bail!("finalize status_url is cross-origin");
    }
    let trimmed = status_url.trim_matches('/');
    let relative = format!("tasks/{task_id}");
    let prefixed = format!("libra/media/v1/{relative}");
    if trimmed == relative || trimmed == prefixed {
        return Ok(relative);
    }
    bail!("finalize status_url is not the media task path");
}

fn accept_finalize_complete(summary: &ManifestSummary, task: &TaskBody) -> Result<()> {
    if task.state != "complete"
        || task.manifest_id != summary.manifest_id
        || task.oid.as_deref() != Some(summary.oid.as_str())
        || task.size != Some(summary.size)
    {
        bail!("finalize completion does not match the requested object");
    }
    Ok(())
}

fn retry_after(response: &Response) -> Duration {
    let secs = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(1)
        .clamp(1, 30);
    Duration::from_secs(secs)
}

async fn read_span(file: &mut tokio::fs::File, offset: u64, length: u64) -> Result<Vec<u8>> {
    if length > chunker::MAX_SIZE as u64 {
        bail!("FastCDC chunk exceeds size limit");
    }
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; length as usize];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_puts_use_the_disk_index() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blob");
        std::fs::write(&file, b"indexed-missing-plan").unwrap();
        let root = dir.path().join("media");
        let outcome = chunk_store::stream_media_file(&file, &root, false).unwrap();
        let mut index = chunk_store::open_index(&root, &outcome.summary.manifest_id).unwrap();
        let page = chunk_store::load_local_page(
            &root,
            &outcome.summary.oid,
            &outcome.summary.manifest_id,
            0,
        )
        .unwrap();
        let hash = page.entries[0].chunk_hash.clone();
        let puts = plan_missing_puts(&mut index, &[hash.clone(), hash]).unwrap();
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].length, page.entries[0].length);
        let unknown = "ab".repeat(32);
        let err = plan_missing_puts(&mut index, &[unknown]).unwrap_err();
        assert!(err.to_string().contains("outside the manifest"), "{err:#}");
    }

    #[test]
    fn full_manifest_retry_is_bounded_by_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blob");
        std::fs::write(&file, b"compact-layout").unwrap();
        let root = dir.path().join("media");
        let outcome = chunk_store::stream_media_file(&file, &root, false).unwrap();
        let body = try_full_manifest_body(&root, &outcome.summary)
            .unwrap()
            .unwrap();
        assert!(body.len() <= MAX_ENVELOPE_SIZE);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value.get("chunks").is_some());
        assert!(value.get("media_oid").is_some());
        let mut huge = outcome.summary.clone();
        huge.chunk_count = 100_000;
        assert!(try_full_manifest_body(&root, &huge).unwrap().is_none());
    }

    #[test]
    fn finalize_completion_and_status_url_are_pinned() {
        let summary = ManifestSummary {
            version: 1,
            algorithm: "fastcdc-v2020-32k".to_string(),
            hash_algorithm: "sha256".to_string(),
            oid: "a".repeat(64),
            size: 4,
            chunk_count: 1,
            page_count: 1,
            manifest_id: "b".repeat(64),
            created_by: None,
        };
        let task = TaskBody {
            task_id: "task-1".to_string(),
            manifest_id: summary.manifest_id.clone(),
            state: "complete".to_string(),
            bytes_verified: 4,
            pages_verified: 1,
            retryable: false,
            error_code: None,
            oid: Some(summary.oid.clone()),
            size: Some(summary.size),
        };
        accept_finalize_complete(&summary, &task).unwrap();
        let mut wrong = task;
        wrong.oid = Some("c".repeat(64));
        assert!(accept_finalize_complete(&summary, &wrong).is_err());
        assert_eq!(
            same_origin_task_path("libra/media/v1/tasks/task-1", "task-1").unwrap(),
            "tasks/task-1"
        );
        assert!(same_origin_task_path("https://evil.example/tasks/task-1", "task-1").is_err());
        assert!(same_origin_task_path("//evil.example/tasks/task-1", "task-1").is_err());
        assert!(same_origin_task_path("libra/media/v1/tasks/other", "task-1").is_err());
    }
}
