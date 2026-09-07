//! Real FastCDC upload/download over the authenticated Mega media extension.
//! Standard LFS remains the fallback before a transfer starts. Once a manifest
//! is selected, integrity/authentication failures fail closed.
use std::{collections::HashSet, path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use url::Url;

use super::{
    capability,
    chunk_store::{self, MediaChunkStore},
    chunker, is_sha256_hex,
    manifest::{MAX_MANIFEST_SIZE, MediaManifest},
    negotiate::{self, ProbeOutcome, TransferDecision},
    sha256_hex,
};

#[derive(Deserialize)]
struct PrepareResponse {
    manifest_id: String,
    missing_chunks: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestResponse {
    manifest_id: String,
    manifest: MediaManifest,
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
        // Live transfers require the server to retain a complete basic LFS
        // object. A chunk-only advertisement cannot disable a valid basic
        // download that the batch endpoint already offered.
        if let ProbeOutcome::Ok(caps) = &outcome
            && (!caps.supports_standard_lfs_fallback || caps.max_manifest_size == 0)
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
            max_manifest: caps.max_manifest_size.min(MAX_MANIFEST_SIZE as u64) as usize,
        }))
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let mut url = self.base.clone();
        url.set_path(&format!("{}{path}", self.base.path()));
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

    async fn bytes(response: Response, limit: usize) -> Result<Vec<u8>> {
        let response = response
            .error_for_status()
            .context("FastCDC request rejected by remote")?;
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
        serde_json::from_slice(&Self::bytes(response, self.max_manifest).await?)
            .context("invalid FastCDC response JSON")
    }

    async fn finalized_manifest(&self, oid: &str, size: u64) -> Result<Option<MediaManifest>> {
        if !is_sha256_hex(oid) {
            bail!("invalid LFS object SHA-256");
        }
        let response = self
            .request(Method::GET, &format!("manifests/by-media/{oid}"))?
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response: ManifestResponse = self.json(response).await?;
        let manifest = response.manifest;
        if response.manifest_id != manifest.id()?
            || manifest.media_oid != oid
            || manifest.media_size != size
        {
            bail!("FastCDC manifest does not match requested LFS object");
        }
        Ok(Some(manifest))
    }

    /// Returns false before sending a manifest if this object's layout exceeds
    /// the extension's limits; the caller may then use its basic upload action.
    pub async fn upload(&self, oid: &str, size: u64, path: &Path) -> Result<bool> {
        // Also repairs the crash window where a previous finalize wrote the
        // complete LFS fallback but failed before publishing the manifest.
        if self.finalized_manifest(oid, size).await?.is_some() {
            return Ok(true);
        }
        if size > (super::manifest::MAX_CHUNKS * chunker::MAX_SIZE) as u64 {
            return Ok(false);
        }
        let source = path.to_path_buf();
        let (mut manifest, _) =
            tokio::task::spawn_blocking(move || MediaManifest::build_from_file(source)).await??;
        if manifest.chunks.len() > super::manifest::MAX_CHUNKS {
            return Ok(false);
        }
        manifest.validate()?;
        if manifest.media_oid != oid || manifest.media_size != size {
            bail!("local LFS object size or SHA-256 mismatch");
        }
        manifest.fallback_oid = Some(oid.to_owned());
        let body = serde_json::to_vec(&manifest)?;
        if body.len() > self.max_manifest {
            return Ok(false);
        }
        let response = self
            .request(Method::POST, "manifests")?
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;
        let prepared: PrepareResponse = self.json(response).await?;
        if prepared.manifest_id != manifest.id()? {
            bail!("remote returned a manifest ID that does not match the uploaded manifest");
        }
        let mut seen = HashSet::new();
        let mut source = tokio::fs::File::open(path)
            .await
            .context("cannot reopen LFS source")?;
        for hash in prepared.missing_chunks {
            let chunk = manifest
                .chunks
                .iter()
                .find(|chunk| chunk.chunk_hash == hash)
                .ok_or_else(|| anyhow::anyhow!("remote requested a chunk outside the manifest"))?;
            if !seen.insert(hash.clone()) {
                continue;
            }
            source.seek(std::io::SeekFrom::Start(chunk.offset)).await?;
            let mut bytes = vec![0; chunk.length as usize];
            source.read_exact(&mut bytes).await?;
            if sha256_hex(&bytes) != hash {
                bail!("LFS source changed while uploading");
            }
            self.request(
                Method::PUT,
                &format!("manifests/{}/chunks/{hash}", prepared.manifest_id),
            )?
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?
            .error_for_status()
            .context("FastCDC chunk upload rejected")?;
        }
        self.request(
            Method::POST,
            &format!("manifests/{}/finalize", prepared.manifest_id),
        )?
        // Finalize reconstructs and checks the entire file, unlike a bounded
        // chunk request. Large LFS objects need a longer verification window.
        .timeout(Duration::from_secs(60 * 60))
        .send()
        .await?
        .error_for_status()
        .context("FastCDC finalize failed; rerun push to resume")?;
        Ok(true)
    }

    /// Returns false only when no finalized manifest exists. Valid local chunks
    /// are reused, so a failed/interrupted download resumes at variable boundaries.
    pub async fn download(
        &self,
        oid: &str,
        size: u64,
        path: &Path,
        store: &MediaChunkStore,
    ) -> Result<bool> {
        let Some(manifest) = self.finalized_manifest(oid, size).await? else {
            return Ok(false);
        };
        for chunk in &manifest.chunks {
            if let Ok(bytes) = store.get_chunk(&chunk.chunk_hash)
                && bytes.len() as u64 == chunk.length
            {
                continue;
            }
            let bytes = Self::bytes(
                self.request(
                    Method::GET,
                    &format!("manifests/by-media/{oid}/chunks/{}", chunk.chunk_hash),
                )?
                .send()
                .await?,
                chunker::MAX_SIZE,
            )
            .await?;
            if bytes.len() as u64 != chunk.length || sha256_hex(&bytes) != chunk.chunk_hash {
                bail!("FastCDC chunk size or SHA-256 mismatch");
            }
            store.put_chunk(&bytes)?;
        }
        chunk_store::reassemble(&manifest, store, path)?;
        store.put_manifest(&manifest)?;
        Ok(true)
    }
}
