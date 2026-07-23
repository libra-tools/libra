//! `libra layer` — Lore's local-overlay primitive (lore.md 2.4).
//!
//! A layer is a named, purely-LOCAL overlay of files materialized onto the
//! working tree on explicit command; it NEVER enters a commit. Subcommands:
//! `add` / `list` / `remove` / `apply` / `unapply` / `status`. All state and
//! materialization is owned by [`crate::internal::layer`]; this module is the
//! thin CLI surface (arg parsing, output config, JSON envelopes).

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::layer::{self, LayerStore},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

pub const LAYER_EXAMPLES: &str = "\
EXAMPLES:
    libra layer add scratch --source ./overlays/scratch   Register a local overlay
    libra layer add ci --source ./ci --priority 10        Higher priority wins collisions
    libra layer list                                      Show registered layers
    libra layer apply                                     Materialize enabled overlays
    libra layer status                                    Show materialized paths
    libra layer unapply --layer scratch                   Remove one layer's files
    libra layer remove scratch                            Unregister (unapply first)";

/// Manage local, never-committed working-tree overlays (lore.md 2.4).
#[derive(Parser, Debug)]
#[command(after_help = LAYER_EXAMPLES)]
pub struct LayerArgs {
    #[command(subcommand)]
    pub command: LayerCommand,
}

#[derive(Subcommand, Debug)]
pub enum LayerCommand {
    /// Register a new local overlay from a source directory.
    Add {
        /// Unique layer name.
        name: String,
        /// Local source directory to overlay.
        #[arg(long)]
        source: String,
        /// Stack priority — higher wins a same-destination collision.
        #[arg(long, default_value_t = 0)]
        priority: i64,
        /// Register the layer disabled (excluded from `apply`).
        #[arg(long)]
        disabled: bool,
    },
    /// List registered layers.
    List,
    /// Enable a layer.
    Enable { name: String },
    /// Disable a layer (kept registered, excluded from `apply`).
    Disable { name: String },
    /// Unregister a layer (removes its materialized files first).
    Remove { name: String },
    /// Materialize all enabled overlays onto the working tree.
    Apply,
    /// Remove materialized overlay files (all, or one `--layer`), leaving
    /// user-edited files untouched.
    Unapply {
        /// Only unapply this layer.
        #[arg(long)]
        layer: Option<String>,
    },
    /// Show registered layers and currently-materialized paths.
    Status,
}

#[derive(Serialize)]
struct LayerRow {
    name: String,
    source: String,
    priority: i64,
    enabled: bool,
}

pub async fn execute_safe(args: LayerArgs, output: &OutputConfig) -> CliResult<()> {
    // W1 §C.4.1.1: the layer registry is worktree-scoped — resolve the scope
    // ONCE for this request and pass it down the whole LayerStore call chain
    // (the former W0 linked-worktree guard is lifted).
    let scope = crate::internal::worktree_scope::WorktreeScope::current();
    match args.command {
        LayerCommand::Add {
            name,
            source,
            priority,
            disabled,
        } => {
            // Validate the source directory up front (a clear error beats a
            // deferred apply-time failure).
            let src = std::path::Path::new(&source);
            if !src.is_dir() {
                return Err(
                    CliError::fatal(format!("layer source '{source}' is not a directory"))
                        .with_stable_code(StableErrorCode::IoReadFailed),
                );
            }
            LayerStore::add(&scope, &name, &source, priority, !disabled)
                .await
                .map_err(|e| {
                    CliError::fatal(e).with_stable_code(StableErrorCode::CliInvalidArguments)
                })?;
            if !output.quiet {
                println!(
                    "registered layer '{name}' (source {source}, priority {priority}{})",
                    if disabled { ", disabled" } else { "" }
                );
            }
            Ok(())
        }
        LayerCommand::List => {
            let layers = load_rows(&scope).await?;
            if output.is_json() {
                return emit_json_data("layer", &serde_json::json!({ "layers": layers }), output);
            }
            if layers.is_empty() && !output.quiet {
                println!("no layers registered");
            } else {
                for row in &layers {
                    println!(
                        "{}\t{}\tpriority={}\t{}",
                        row.name,
                        row.source,
                        row.priority,
                        if row.enabled { "enabled" } else { "disabled" }
                    );
                }
            }
            Ok(())
        }
        LayerCommand::Enable { name } => set_enabled(&scope, &name, true, output).await,
        LayerCommand::Disable { name } => set_enabled(&scope, &name, false, output).await,
        LayerCommand::Remove { name } => {
            // Remove the materialized files first (skipping user-edited ones),
            // then unregister.
            let (_removed, _skipped) = layer::unapply(&scope, Some(&name)).await?;
            let existed = LayerStore::remove(&scope, &name)
                .await
                .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoWriteFailed))?;
            if !existed {
                return Err(CliError::fatal(format!("no layer named '{name}'"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget));
            }
            // Keep the exclusion snapshot fresh for subsequent commands.
            layer::refresh_exclusion_snapshot(&scope).await;
            if !output.quiet {
                println!("removed layer '{name}'");
            }
            Ok(())
        }
        LayerCommand::Apply => {
            let report = layer::apply(&scope).await?;
            layer::refresh_exclusion_snapshot(&scope).await;
            if output.is_json() {
                return emit_json_data(
                    "layer",
                    &serde_json::json!({
                        "action": "apply",
                        "layers": report.layers,
                        "written": report.written,
                        "pruned": report.pruned,
                    }),
                    output,
                );
            }
            if !output.quiet {
                println!(
                    "applied {} layer(s): {} file(s) materialized, {} pruned",
                    report.layers, report.written, report.pruned
                );
            }
            Ok(())
        }
        LayerCommand::Unapply { layer: filter } => {
            let (removed, skipped) = layer::unapply(&scope, filter.as_deref()).await?;
            layer::refresh_exclusion_snapshot(&scope).await;
            if output.is_json() {
                return emit_json_data(
                    "layer",
                    &serde_json::json!({
                        "action": "unapply",
                        "removed": removed,
                        "skipped": skipped,
                    }),
                    output,
                );
            }
            if !output.quiet {
                println!("unapplied: {removed} file(s) removed, {skipped} kept (edited)");
            }
            Ok(())
        }
        LayerCommand::Status => {
            let layers = load_rows(&scope).await?;
            let paths = LayerStore::materialized_paths(&scope)
                .await
                .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoReadFailed))?;
            if output.is_json() {
                let materialized: Vec<_> = paths
                    .iter()
                    .map(|p| serde_json::json!({ "layer": p.layer_name, "path": p.path }))
                    .collect();
                return emit_json_data(
                    "layer",
                    &serde_json::json!({ "layers": layers, "materialized": materialized }),
                    output,
                );
            }
            if !output.quiet {
                println!(
                    "{} layer(s) registered, {} path(s) materialized",
                    layers.len(),
                    paths.len()
                );
                for p in &paths {
                    println!("  {}\t({})", p.path, p.layer_name);
                }
            }
            Ok(())
        }
    }
}

async fn load_rows(
    scope: &crate::internal::worktree_scope::WorktreeScope,
) -> CliResult<Vec<LayerRow>> {
    Ok(LayerStore::list(scope)
        .await
        .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoReadFailed))?
        .into_iter()
        .map(|l| LayerRow {
            name: l.name,
            source: l.source,
            priority: l.priority,
            enabled: l.enabled,
        })
        .collect())
}

async fn set_enabled(
    scope: &crate::internal::worktree_scope::WorktreeScope,
    name: &str,
    enabled: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let changed = LayerStore::set_enabled(scope, name, enabled)
        .await
        .map_err(|e| CliError::fatal(e).with_stable_code(StableErrorCode::IoWriteFailed))?;
    if !changed {
        return Err(CliError::fatal(format!("no layer named '{name}'"))
            .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    if !output.quiet {
        println!(
            "{} layer '{name}'",
            if enabled { "enabled" } else { "disabled" }
        );
    }
    Ok(())
}
