//! `libra cache info` — report the resolved tiered-storage / LRU cache tunables
//! (storage type, small/large threshold, LRU disk budget), exposing the existing
//! `LIBRA_STORAGE_*` knobs for inspection (lore.md §0.10). Pure inspection of the
//! resolved storage configuration; needs no repository.

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::utils::{
    client_storage::{CacheConfig, resolve_cache_config},
    error::{CliError, CliResult},
    output::{OutputConfig, emit_json_data},
};

pub const CACHE_EXAMPLES: &str = "\
EXAMPLES:
    libra cache evict --dry-run            Preview verified-durable evictions
    libra cache evict                      Trim the local cache to budget
    libra cache evict --max-size 0         Evict every verified-durable large object
    libra cache info                       Show the resolved storage/cache tunables
    LIBRA_STORAGE_TYPE=r2 LIBRA_STORAGE_CACHE_SIZE=536870912 libra cache info
    libra --json cache info                Structured { storage_type, tiered, threshold_bytes, cache_size_bytes }";

/// Inspect the tiered-storage / LRU cache configuration.
#[derive(Parser, Debug)]
#[command(after_help = CACHE_EXAMPLES)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// Show the resolved storage/cache tunables (type, threshold, LRU budget).
    Info,
    /// Evict verified-durable large objects from the local cache until under
    /// budget (lore.md 2.9). Every deletion is gated on an error-aware
    /// durability probe run immediately before the unlink.
    Evict {
        /// Report what would be evicted (probes still run); delete nothing.
        #[arg(long)]
        dry_run: bool,
        /// Override the budget (bytes of uncompressed large-object payload).
        #[arg(long, value_name = "BYTES")]
        max_size: Option<u64>,
        /// Skip objects materialized within the last N seconds (default 600;
        /// 0 disables the floor).
        #[arg(long, value_name = "SECS", default_value_t = 600)]
        min_age: u64,
    },
}

#[derive(Debug, Serialize)]
struct CacheInfo {
    /// The raw `LIBRA_STORAGE_TYPE` value (`local` only when unset), e.g. `s3`/`r2`.
    storage_type: String,
    /// Whether the config statically selects a durable tier (`s3`/`r2` + valid
    /// bucket/endpoint/keys) — the tunables only apply then; a local-only repo
    /// caches nothing. A real connection also needs valid credentials.
    tiered: bool,
    /// Small/large object threshold in bytes (`LIBRA_STORAGE_THRESHOLD`).
    threshold_bytes: usize,
    /// Local LRU disk budget in bytes (`LIBRA_STORAGE_CACHE_SIZE`).
    cache_size_bytes: usize,
}

async fn evict(
    dry_run: bool,
    max_size: Option<u64>,
    min_age: u64,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::utils::{error::StableErrorCode, storage::EvictRequest};
    crate::utils::util::require_repo()
        .map_err(|_| crate::utils::error::CliError::repo_not_found())?;
    // Offline read policy forbids the durability probes the safety contract
    // requires — refuse rather than delete unverified.
    if crate::utils::read_policy::read_policy() == crate::utils::read_policy::ReadPolicy::LocalOnly
    {
        return Err(crate::utils::error::CliError::failure(
            "cache eviction needs durable-tier probes; the offline/local read policy is active",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("drop --offline / LIBRA_READ_POLICY=local and retry"));
    }
    let budget = match max_size {
        Some(budget) => budget,
        None => {
            crate::utils::client_storage::resolve_cache_config()
                .map_err(|error| {
                    crate::utils::error::CliError::failure(format!(
                        "cannot resolve the cache budget: {error}"
                    ))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                })?
                .cache_size_bytes as u64
        }
    };
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
    let report = storage
        .evict_local(EvictRequest {
            budget_bytes: budget,
            min_age_secs: min_age,
            dry_run,
        })
        .await
        .map_err(|error| {
            crate::utils::error::CliError::failure(format!("cache eviction failed: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    let Some(report) = report else {
        if output.is_json() {
            return crate::utils::output::emit_json_data(
                "cache",
                &serde_json::json!({ "action": "evict", "tiered": false }),
                output,
            );
        }
        if !output.quiet {
            println!("no durable tier configured — nothing is evictable (local-only repository)");
        }
        return Ok(());
    };
    if output.is_json() {
        let mut value = serde_json::to_value(&report).unwrap_or_default();
        value["action"] = serde_json::json!("evict");
        value["tiered"] = serde_json::json!(true);
        value["dry_run"] = serde_json::json!(dry_run);
        value["budget_bytes"] = serde_json::json!(budget);
        return crate::utils::output::emit_json_data("cache", &value, output);
    }
    if !output.quiet {
        let verb = if dry_run { "would evict" } else { "evicted" };
        println!(
            "{verb} {} object(s), {} bytes (budget {budget}; scanned {}, candidates {}, \
             verified {})",
            report.evicted,
            report.reclaimed_bytes,
            report.scanned,
            report.candidate_count,
            report.verified
        );
        if report.skipped_absent > 0 {
            println!(
                "skipped {} object(s) NOT in the durable tier (push/backup to make them \
                 durable)",
                report.skipped_absent
            );
        }
        if report.skipped_probe_error > 0 {
            println!(
                "skipped {} object(s) whose durability probe errored (outage is never \
                 treated as absence)",
                report.skipped_probe_error
            );
        }
        if report.skipped_recent > 0 {
            println!(
                "skipped {} recently-materialized object(s) (--min-age {min_age})",
                report.skipped_recent
            );
        }
    }
    Ok(())
}

pub async fn execute_safe(args: CacheArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command {
        CacheCommand::Info => info(output),
        CacheCommand::Evict {
            dry_run,
            max_size,
            min_age,
        } => evict(dry_run, max_size, min_age, output).await,
    }
}

fn info(output: &OutputConfig) -> CliResult<()> {
    let CacheConfig {
        storage_type,
        tiered,
        threshold_bytes,
        cache_size_bytes,
    } = resolve_cache_config().map_err(|message| {
        CliError::fatal(format!(
            "failed to resolve storage/cache configuration: {message}"
        ))
    })?;
    let report = CacheInfo {
        storage_type,
        tiered,
        threshold_bytes,
        cache_size_bytes,
    };

    if output.is_json() {
        return emit_json_data("cache", &report, output);
    }

    println!("storage:   {}", report.storage_type);
    if report.tiered {
        println!("tier:      durable tier active (cache tunables apply)");
    } else {
        println!("tier:      local-only (no durable tier; cache tunables are inert)");
    }
    println!(
        "threshold: {} bytes (objects >= this size are LRU-cached, not stored permanently)",
        report.threshold_bytes
    );
    println!(
        "cache:     {} bytes (LRU disk budget for large cached objects)",
        report.cache_size_bytes
    );
    println!(
        "(configure via LIBRA_STORAGE_TYPE / LIBRA_STORAGE_THRESHOLD / LIBRA_STORAGE_CACHE_SIZE)"
    );
    Ok(())
}
