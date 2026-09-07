//! `libra media` — FastCDC LFS media chunking client (lore.md §6).
//!
//! The honest, feature-gated (`fastcdc`) v1 CLIENT surface: chunk a media file,
//! inspect/validate a manifest, reassemble+verify from the local chunk store,
//! and probe a remote's chunked-LFS capability with the §6.4 safe-fallback
//! decision. Feature-enabled LFS transfers can use Mega's authenticated media
//! endpoints, with standard LFS fallback. This module is only the CLI surface; all logic
//! lives in [`crate::utils::media`].

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::config::ConfigKv,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        media::{
            capability,
            chunk_store::{self, MediaChunkStore},
            manifest::MediaManifest,
            negotiate::{self, ProbeOutcome, TransferDecision},
        },
        output::{OutputConfig, emit_json_data},
        path,
    },
};

pub const MEDIA_EXAMPLES: &str = "\
EXAMPLES:
    libra media chunk big.psd                 FastCDC-chunk a file; print the manifest summary
    libra media chunk big.psd --store         Also persist chunks + manifest to the local media store
    libra media inspect .libra/media/manifests/<oid>.json   Validate a manifest file
    libra media verify big.psd                Reassemble from the store and verify the media_oid
    libra media probe                         Probe the remote's chunked-LFS capability (falls back to standard LFS)
    libra --json media chunk big.psd          Structured JSON output for agents

NOTES:
    FastCDC media chunking is a feature-gated Libra extension (lore.md §6). The
    media_oid is always SHA-256 of the full file (standard-LFS-compatible), and
    chunks live in a private .libra/media store outside the Git object graph.
    Cross-machine chunked transfer requires Mega built with --features fastcdc
    and a stored access token. Other remotes fall back to standard Git LFS.";

#[derive(Parser, Debug)]
#[command(after_help = MEDIA_EXAMPLES)]
pub struct MediaArgs {
    #[command(subcommand)]
    command: MediaCommand,
}

#[derive(Subcommand, Debug)]
enum MediaCommand {
    /// FastCDC-chunk a file and emit its media manifest.
    Chunk {
        /// The media file to chunk.
        path: String,
        /// Persist the chunks and the manifest to the local media store.
        #[clap(long)]
        store: bool,
    },
    /// Parse and validate a manifest JSON file.
    Inspect {
        /// Path to a `<media_oid>.json` manifest file.
        manifest: String,
    },
    /// Reassemble a media object from the local chunk store and verify its
    /// media_oid. Give a file path (its media_oid is computed) or `--media-oid`.
    Verify {
        /// The original media file whose media_oid keys its stored manifest.
        path: Option<String>,
        /// The media_oid (64-hex) directly, instead of a file.
        #[clap(long = "media-oid", conflicts_with = "path")]
        media_oid: Option<String>,
    },
    /// Probe a remote's media capability endpoint and report the transfer
    /// decision (chunked vs standard-LFS fallback).
    Probe {
        /// Remote name (default: the current branch's remote, else `origin`).
        #[clap(long)]
        remote: Option<String>,
    },
}

pub async fn execute_safe(args: MediaArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        MediaCommand::Chunk { path, store } => chunk(&path, store, output).await,
        MediaCommand::Inspect { manifest } => inspect(&manifest, output),
        MediaCommand::Verify { path, media_oid } => verify(path, media_oid, output).await,
        MediaCommand::Probe { remote } => probe(remote, output).await,
    }
}

#[derive(Serialize)]
struct ChunkSummary {
    media_oid: String,
    media_size: u64,
    chunk_count: usize,
    unique_chunks: usize,
    algorithm: String,
    stored: bool,
    manifest_path: Option<String>,
}

async fn chunk(path: &str, store: bool, output: &OutputConfig) -> CliResult<()> {
    let (manifest, chunks) = MediaManifest::build_from_file(path).map_err(|e| {
        CliError::fatal(format!("failed to chunk '{path}': {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;

    let mut unique = std::collections::HashSet::new();
    for c in &manifest.chunks {
        unique.insert(c.chunk_hash.clone());
    }

    let mut manifest_path = None;
    if store {
        let cs = MediaChunkStore::open();
        let mut file = std::fs::File::open(path).map_err(|source| {
            CliError::fatal(format!(
                "failed to reopen '{path}' for storing chunks: {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for c in &chunks {
            let bytes = chunk_store::read_span(&mut file, c.offset, c.length)
                .map_err(|e| media_store_err("store chunk", e))?;
            cs.put_chunk(&bytes)
                .map_err(|e| media_store_err("store chunk", e))?;
        }
        // Persist the manifest as a content-addressed file.
        let dir = path::media_manifests();
        std::fs::create_dir_all(&dir).map_err(|source| {
            CliError::fatal(format!("failed to create media manifest dir: {source}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        let mpath = dir.join(format!("{}.json", manifest.media_oid));
        let json = manifest
            .to_json()
            .map_err(|e| CliError::fatal(format!("failed to serialize manifest: {e}")))?;
        crate::utils::atomic_write::write_atomic(
            &mpath,
            json.as_bytes(),
            crate::utils::atomic_write::sync_data_enabled(),
        )
        .map_err(|source| {
            CliError::fatal(format!("failed to write manifest: {source}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        manifest_path = Some(mpath.display().to_string());
    }

    let summary = ChunkSummary {
        media_oid: manifest.media_oid.clone(),
        media_size: manifest.media_size,
        chunk_count: manifest.chunks.len(),
        unique_chunks: unique.len(),
        algorithm: manifest.algorithm.clone(),
        stored: store,
        manifest_path: manifest_path.clone(),
    };

    if output.is_json() {
        return emit_json_data("media.chunk", &summary, output);
    }
    if !output.quiet {
        println!("media_oid: {}", summary.media_oid);
        println!("size:      {} bytes", summary.media_size);
        println!(
            "chunks:    {} ({} unique, algorithm {})",
            summary.chunk_count, summary.unique_chunks, summary.algorithm
        );
        if let Some(p) = &manifest_path {
            println!("stored:    chunks + manifest at {p}");
        }
    }
    Ok(())
}

fn inspect(manifest_path: &str, output: &OutputConfig) -> CliResult<()> {
    let text = std::fs::read_to_string(manifest_path).map_err(|source| {
        CliError::fatal(format!(
            "failed to read manifest '{manifest_path}': {source}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let manifest = MediaManifest::from_json(&text).map_err(|e| {
        CliError::fatal(format!("invalid manifest '{manifest_path}': {e}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    if output.is_json() {
        return emit_json_data("media.inspect", &manifest, output);
    }
    if !output.quiet {
        println!("valid manifest (version {})", manifest.version);
        println!("media_oid: {}", manifest.media_oid);
        println!("size:      {} bytes", manifest.media_size);
        println!("chunks:    {}", manifest.chunks.len());
        println!("algorithm: {}", manifest.algorithm);
    }
    Ok(())
}

#[derive(Serialize)]
struct VerifyResult {
    media_oid: String,
    verified: bool,
}

async fn verify(
    path: Option<String>,
    media_oid: Option<String>,
    output: &OutputConfig,
) -> CliResult<()> {
    let oid = match (path, media_oid) {
        (Some(p), None) => crate::utils::lfs::calc_lfs_file_hash(&p).map_err(|source| {
            CliError::fatal(format!("failed to hash '{p}': {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?,
        (None, Some(oid)) => oid,
        _ => {
            return Err(CliError::command_usage(
                "provide exactly one of <path> or --media-oid",
            ));
        }
    };
    let mpath = path::media_manifests().join(format!("{oid}.json"));
    let text = std::fs::read_to_string(&mpath).map_err(|_| {
        CliError::fatal(format!(
            "no stored manifest for media_oid {oid} (expected {}); run 'libra media chunk --store' first",
            mpath.display()
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;
    let manifest = MediaManifest::from_json(&text)
        .map_err(|e| CliError::fatal(format!("stored manifest for {oid} is invalid: {e}")))?;

    // Reassemble to a temp path and verify the media_oid (verify-then-rename
    // inside reassemble guarantees no partial/corrupt file survives a mismatch).
    let store = MediaChunkStore::open();
    let tmp = std::env::temp_dir().join(format!("libra-media-verify-{oid}"));
    let result = chunk_store::reassemble(&manifest, &store, &tmp);
    let _ = std::fs::remove_file(&tmp);

    let verified = result.is_ok();
    let vr = VerifyResult {
        media_oid: oid.clone(),
        verified,
    };
    if output.is_json() {
        emit_json_data("media.verify", &vr, output)?;
        return if verified {
            Ok(())
        } else {
            Err(CliError::fatal("media verification failed"))
        };
    }
    match result {
        Ok(()) => {
            if !output.quiet {
                println!("verified: {oid}");
            }
            Ok(())
        }
        Err(e) => Err(CliError::fatal(format!("media verification failed: {e}"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)),
    }
}

#[derive(Serialize)]
struct ProbeReport {
    remote: String,
    base_url: String,
    probe: String,
    decision: String,
    reason: Option<String>,
    chunked: bool,
}

async fn probe(remote: Option<String>, output: &OutputConfig) -> CliResult<()> {
    // Resolve the remote URL: explicit --remote, else the current branch's
    // remote, else `origin`.
    let (remote_name, url) = match remote {
        Some(name) => {
            let u = ConfigKv::get_remote_url(&name).await.map_err(|_| {
                CliError::fatal(format!("remote '{name}' has no configured URL"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            })?;
            (name, u)
        }
        None => match ConfigKv::get_current_remote_url().await {
            Ok(Some(u)) => ("origin".to_string(), u),
            _ => {
                let u = ConfigKv::get_remote_url("origin").await.map_err(|_| {
                    CliError::fatal("no remote configured (pass --remote <name>)")
                        .with_stable_code(StableErrorCode::CliInvalidTarget)
                })?;
                ("origin".to_string(), u)
            }
        },
    };

    let outcome = capability::probe(&url).await;
    // Report what WOULD happen assuming the repo enabled chunked LFS and a local
    // fallback object is available — i.e. characterise the remote itself.
    let decision = negotiate::negotiate(&outcome, true, true);
    let (decision_str, reason, chunked) = describe(&decision);
    let probe_str = match &outcome {
        ProbeOutcome::Ok(_) => "ok",
        ProbeOutcome::NoEndpoint => "no-endpoint",
        ProbeOutcome::ServerErrorAfterBackoff => "server-error-after-backoff",
    }
    .to_string();

    let report = ProbeReport {
        remote: remote_name,
        base_url: crate::utils::redact::redact_url_credentials(&url),
        probe: probe_str,
        decision: decision_str,
        reason,
        chunked,
    };
    if output.is_json() {
        return emit_json_data("media.probe", &report, output);
    }
    if !output.quiet {
        println!("remote:   {} ({})", report.remote, report.base_url);
        println!("probe:    {}", report.probe);
        match &report.reason {
            Some(r) => println!("decision: {} ({r})", report.decision),
            None => println!("decision: {}", report.decision),
        }
    }
    Ok(())
}

fn describe(decision: &TransferDecision) -> (String, Option<String>, bool) {
    match decision {
        TransferDecision::Chunked { algorithm } => (format!("chunked ({algorithm})"), None, true),
        TransferDecision::StandardLfs { reason } => (
            "standard-lfs (fallback)".to_string(),
            Some(reason.as_str().to_string()),
            false,
        ),
        TransferDecision::Block { reason } => (
            "blocked".to_string(),
            Some(reason.as_str().to_string()),
            false,
        ),
    }
}

fn media_store_err(action: &str, e: chunk_store::MediaStoreError) -> CliError {
    CliError::fatal(format!("failed to {action}: {e}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}
