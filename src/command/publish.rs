//! `libra publish` — read-only Cloudflare publishing.
//!
//! Per `docs/development/commands/publish.md`, the publish CLI surface is
//! `init` / `sync` / `status` / `deploy` / `unpublish`. `init` now
//! materialises the embedded Worker template, `sync` plans and writes
//! local refs to publish D1/R2 storage, `status` reports local template
//! drift and can compare local refs with D1 `publish_refs`, `deploy`
//! validates/builds the Worker before optionally
//! applying D1 migrations and deploying with Wrangler, and
//! `unpublish --yes` disables a site through Wrangler D1 execute.
//!
//! Codex pass-7 P1 registered the CLI surface so the `clap` parser
//! would not reject `libra publish ...`. Later slices filled in local
//! template management, dry-run planning, Worker deployment,
//! unpublish orchestration, and the first D1/R2 publish sync path.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    io,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use chrono::Utc;
use clap::{Parser, Subcommand};
use git_internal::{
    hash::ObjectHash,
    internal::object::{blob::Blob, commit::Commit, tree::Tree, types::ObjectType},
};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

use crate::{
    command::{cloud, load_object, status},
    internal::{
        ai::{history::HistoryManager, projection::ProjectionRebuilder},
        branch::Branch,
        config::ConfigKv,
        db,
        head::Head,
        publish::{
            ai_export::{
                AiExportPlan, AiExportRequest, HistoryAiExportRequest, ProjectionAiExportRequest,
                build_ai_export_plan, build_publish_ai_projection_objects,
                collect_publish_ai_objects_from_history,
            },
            contract::{
                AiBundleAssociatedIds, PUBLISH_SCHEMA_VERSION, RedactionMode, SiteVisibility,
            },
            preflight::{DenyReason, Preflight, PreflightDecision},
            snapshot::{
                FileSnapshot, RefInput, RevisionArtifactPlan, RevisionFileInput, SnapshotConfig,
                build_revision_artifact_plan, build_snapshot_plan, detect_ambiguous_short_refs,
                validate_oid, validate_ref_name,
            },
            upload::{
                AiExportArtifactUploadSummary, AiExportD1Rows, RevisionArtifactUploadOptions,
                RevisionArtifactUploadSummary, RevisionD1Rows, SiteIndexArtifacts,
                build_ai_export_d1_rows, build_revision_d1_rows, build_site_index_artifacts,
                upload_ai_export_artifacts_with_options, upload_revision_artifacts_with_options,
                upload_site_index_artifacts,
            },
            worker_template::{MANIFEST, RenderPolicy, WorkerTemplate, embed_path_is_allowed},
        },
        tag::{self, TagObject},
    },
    utils::{
        d1_client::{
            D1Client, D1Error, PublishAiObjectRow, PublishAiVersionRow, PublishFileRow,
            PublishRefRow, PublishRevisionRow, PublishSiteLatestUpdate,
            PublishSiteLatestUpdateResult, PublishSiteRow, PublishSyncRunRow,
        },
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{self, CommandOutput, OutputConfig},
        storage::{local::LocalStorage, publish_storage::PublishStorage},
        util,
    },
};

/// `--help` examples shown in `libra publish --help` output.
///
/// `publish` exposes five sub-commands (init / sync / status / deploy /
/// unpublish) that together drive the read-only Cloudflare Worker
/// publishing path. The banner pins the canonical invocation per
/// sub-command plus a dry-run sync, a sensitive-path allowance, a
/// site-scoped status, a deploy that skips Cloudflare mutation, and a
/// JSON variant for agents so users can map intent to invocation
/// without reading the design doc. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
pub const PUBLISH_EXAMPLES: &str = "\
EXAMPLES:
    libra publish init --slug <slug> --clone-domain <domain>
                                              Materialise the local Worker template scaffold
    libra publish status                      Inspect local Worker template / D1 ref drift
    libra publish status --site-id <uuid>     Inspect a specific published site by UUID
    libra publish sync                        Sync default refs to D1/R2
    libra publish sync --dry-run              Plan the publish without writing to D1/R2
    libra publish sync --ref refs/heads/main  Sync a single named ref
    libra publish sync --force                Re-upload every file/object regardless of CAS
    libra publish sync --allow-sensitive-path <path>
                                              Allow a path the deny list normally blocks (private sites)
    libra publish deploy                      Build the Worker and deploy to Cloudflare
    libra publish deploy --skip-deploy        Build only; skip Cloudflare mutation
    libra publish unpublish --site-id <uuid> --yes
                                              Disable a published site without deleting D1/R2 data
    libra publish --json sync --dry-run       Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(about = "Manage read-only Cloudflare Worker publishing", after_help = PUBLISH_EXAMPLES)]
pub struct PublishArgs {
    #[command(subcommand)]
    pub command: PublishCommand,
}

#[derive(Subcommand, Debug)]
pub enum PublishCommand {
    /// Materialise the local Worker template scaffold.
    Init(InitArgs),
    /// Sync publish snapshots to D1/R2, or plan them with --dry-run.
    Sync(SyncArgs),
    /// Inspect local Worker template status and optionally compare D1 refs.
    Status(StatusArgs),
    /// Build and optionally deploy the Cloudflare Worker.
    Deploy(DeployArgs),
    /// Disable a published site without deleting D1/R2 data.
    Unpublish(UnpublishArgs),
}

#[derive(Parser, Debug)]
pub struct InitArgs {
    /// URL-safe slug; uniqueness scoped to `--clone-domain`.
    #[arg(long)]
    pub slug: Option<String>,

    /// Public clone domain (e.g. `code.example.com`)
    #[arg(long, value_name = "DOMAIN")]
    pub clone_domain: Option<String>,

    /// Browser-facing origin URL (e.g. `https://code.example.com`)
    #[arg(long, value_name = "URL")]
    pub display_origin: Option<String>,

    /// Display name shown in the Worker UI header
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Site visibility: `public` (browser-readable) or `private` (Cloudflare Access)
    #[arg(long, value_name = "MODE")]
    pub visibility: Option<String>,

    /// Cloudflare Worker name (default: `libra-publish`)
    #[arg(long, value_name = "NAME")]
    pub worker_name: Option<String>,

    /// Per-file preview cap in bytes. Files larger than this fall back to metadata-only. Must be > 0 — passing `0` is rejected because a zero cap defeats the purpose of code-preview publishing
    #[arg(long, value_name = "BYTES", value_parser = parse_max_preview_bytes)]
    pub max_preview_bytes: Option<u64>,
}

#[derive(Parser, Debug)]
pub struct SyncArgs {
    /// Sync only the named ref (e.g. `refs/heads/main` or `main`)
    #[arg(long, value_name = "REF")]
    pub r#ref: Option<String>,

    /// Print the plan without writing to D1/R2.
    #[arg(long)]
    pub dry_run: bool,

    /// Fail on dirty working tree instead of warning.
    #[arg(long)]
    pub fail_on_dirty: bool,

    /// AI snapshot redaction policy: `default` or `strict`
    #[arg(long, value_name = "POLICY", default_value = "default")]
    pub ai_redaction: String,

    /// Allow a path that the deny list would normally block. Only honored on `private` sites
    #[arg(long, value_name = "PATH")]
    pub allow_sensitive_path: Vec<String>,

    /// Force re-upload of every file/object even if `is_synced` is set
    #[arg(long)]
    pub force: bool,
}

#[derive(Parser, Debug)]
pub struct StatusArgs {
    /// Published site UUID. Defaults to `publish.site_id` config when present.
    #[arg(long)]
    pub site_id: Option<String>,
}

#[derive(Parser, Debug)]
pub struct DeployArgs {
    /// Skip Cloudflare mutation steps after the local Worker build.
    #[arg(long)]
    pub skip_deploy: bool,
}

#[derive(Parser, Debug)]
pub struct UnpublishArgs {
    /// Confirm the unpublish operation.
    #[arg(long)]
    pub yes: bool,

    /// Site UUID to disable. Defaults to `publish.site_id` config.
    #[arg(long)]
    pub site_id: Option<String>,
}

const WORKER_TEMPLATE_MANIFEST_SCHEMA_VERSION: u32 = 1;
/// The manifest location relative to COMMON STORAGE (the shared `.libra`).
/// Resolution goes through the common-storage resolver
/// ([`publish_manifest_path`]); output reporting derives from the resolved
/// path ([`reported_manifest_path`]).
const WORKER_TEMPLATE_MANIFEST_STORAGE_PATH: &str = "publish/worker-template-manifest.json";
const PUBLISH_D1_DATABASE_ID_PLACEHOLDER: &str = "REPLACE_WITH_D1_DATABASE_ID";
const PUBLISH_R2_BUCKET_NAME_PLACEHOLDER: &str = "REPLACE_WITH_R2_BUCKET_NAME";
const PUBLISH_REDACTION_RULES_VERSION: &str = "2026.05.13-1";

/// GitHub Issues URL surfaced on inline `InternalInvariant` bug paths in
/// `publish.rs` so users can report unexpected failures (D1 row build,
/// AI projection rebuild, refs_generation overflow, etc.). The framework's
/// `effective_hints()` already auto-injects this URL on the rendered
/// output, but the explicit hint keeps the contract callsite-stable —
/// mirrors push.rs / tag.rs / commit.rs / stash.rs / index_pack.rs's
/// hint pattern per Cross-Cutting G.
const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";
const PUBLISH_AI_PROJECTION_OBJECT_TYPES: &[&str] = &[
    "Thread",
    "Scheduler",
    "QueryIndex",
    "LiveContextWindow",
    "ReadyQueue",
    "ParallelGroup",
    "Checkpoint",
    "RetryRoute",
    "UiCurrentView",
];

/// clap value parser for `--max-preview-bytes`.
///
/// Codex pass-9 P2: enforce `> 0` at the parse layer so a zero value
/// is caught before the stub runs. The SQL schema currently allows
/// `>= 0`, but at the CLI level a zero cap publishes no file
/// previews — that is unambiguously a misuse.
fn parse_max_preview_bytes(raw: &str) -> Result<u64, String> {
    let parsed: u64 = raw
        .parse()
        .map_err(|_| format!("'{raw}' is not a valid byte count"))?;
    if parsed == 0 {
        // Codex pass-10 P3: include the offending input verbatim so
        // the error message reads naturally in scripts that pipe
        // user input through.
        return Err(format!(
            "'{raw}' is not a valid byte count: must be > 0; pass a positive byte count or \
             omit the flag",
        ));
    }
    Ok(parsed)
}

pub async fn execute(args: PublishArgs) -> CliResult<()> {
    execute_safe(args, &OutputConfig::default()).await
}

/// The manifest path as REPORTED in machine/human output: relative when the
/// resolved location sits under THIS invocation's worktree root (the main
/// worktree — byte-identical to the pre-W0 output contract), absolute
/// otherwise (a linked worktree, whose manifest lives in main's common
/// storage; a relative string resolved against the linked root would name a
/// file that does not exist — and a tool WRITING there would recreate the
/// per-worktree manifest fork at the reporting layer).
fn reported_manifest_path(repo_root: &Path, manifest_path: &Path) -> String {
    if let Ok(relative) = manifest_path.strip_prefix(repo_root) {
        return relative.display().to_string();
    }
    // The resolver canonicalizes; the walk's repo_root may be an alias
    // (macOS /var -> /private/var). Compare canonical-to-canonical before
    // concluding the manifest lives outside this worktree.
    let canonical = fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    manifest_path
        .strip_prefix(&canonical)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| manifest_path.display().to_string())
}

/// The worker-template manifest lives in repository COMMON storage
/// (`<shared .libra>/publish/worker-template-manifest.json`), resolved through
/// the commondir chain — never derived from the invoking worktree's root.
/// Every worktree of a repository must read and write the SAME manifest
/// (plan-20260714 §C.4.1.1): deriving it from the worktree root would fork the
/// deploy drift gate per worktree, letting a linked worktree init/deploy
/// against a manifest the main worktree never sees. Fail-closed on a corrupt
/// `commondir` pointer (§C.4.1) rather than minting a phantom location.
fn publish_manifest_path(repo_root: &Path) -> CliResult<PathBuf> {
    let storage_root =
        util::try_get_storage_path(Some(repo_root.to_path_buf())).map_err(|source| {
            CliError::fatal(format!(
                "failed to resolve the repository storage root for '{}': {source}",
                repo_root.display()
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
    Ok(storage_root.join(WORKER_TEMPLATE_MANIFEST_STORAGE_PATH))
}

pub async fn execute_safe(args: PublishArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    match args.command {
        PublishCommand::Init(init_args) => {
            let repo_root = util::try_working_dir().map_err(|source| {
                CliError::fatal(format!("failed to resolve Libra repository root: {source}"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            })?;
            let manifest_path = publish_manifest_path(&repo_root)?;
            let result = run_publish_init_at_root(&repo_root, &manifest_path, &init_args)?;
            output::emit(&result, output)
        }
        PublishCommand::Sync(sync_args) => {
            let result = if sync_args.dry_run {
                run_publish_sync_dry_run(&sync_args).await?
            } else {
                run_publish_sync_non_dry_run(&sync_args).await?
            };
            output::emit(&result, output)
        }
        PublishCommand::Status(status_args) => {
            let repo_root = util::try_working_dir().map_err(|source| {
                CliError::fatal(format!("failed to resolve Libra repository root: {source}"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            })?;
            let manifest_path = publish_manifest_path(&repo_root)?;
            let result =
                run_publish_status_command_at_root(&repo_root, &manifest_path, &status_args)
                    .await?;
            output::emit(&result, output)
        }
        PublishCommand::Deploy(deploy_args) => {
            let repo_root = util::try_working_dir().map_err(|source| {
                CliError::fatal(format!("failed to resolve Libra repository root: {source}"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            })?;
            let manifest_path = publish_manifest_path(&repo_root)?;
            let mut runner = ProcessPublishWorkerCommandRunner;
            let result =
                run_publish_deploy_at_root(&repo_root, &manifest_path, &deploy_args, &mut runner)?;
            output::emit(&result, output)
        }
        PublishCommand::Unpublish(unpublish_args) => {
            if !unpublish_args.yes {
                return Err(CliError::command_usage(
                    "publish unpublish requires --yes to confirm disabling the site",
                ));
            }
            let repo_root = util::try_working_dir().map_err(|source| {
                CliError::fatal(format!("failed to resolve Libra repository root: {source}"))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            })?;
            let manifest_path = publish_manifest_path(&repo_root)?;
            let site_id = resolve_unpublish_site_id(&unpublish_args).await?;
            let mut runner = ProcessPublishWorkerCommandRunner;
            let result = run_publish_unpublish_at_root(
                &repo_root,
                &manifest_path,
                &unpublish_args,
                &site_id,
                &mut runner,
            )?;
            output::emit(&result, output)
        }
    }
}

#[derive(Debug)]
struct TemplateFile {
    path: String,
    bytes: Vec<u8>,
    sha256: String,
    render_policy: RenderPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerTemplateManifest {
    schema_version: u32,
    template_version: String,
    worker_dir: String,
    files: Vec<WorkerTemplateManifestFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerTemplateManifestFile {
    path: String,
    render_policy: String,
    sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishInitOutput {
    worker_dir: String,
    manifest_path: String,
    template_version: &'static str,
    files_written: usize,
    files_current: usize,
}

impl CommandOutput for PublishInitOutput {
    fn render_human(&self, writer: &mut dyn Write, output: &OutputConfig) -> io::Result<()> {
        if output.quiet {
            return Ok(());
        }
        writeln!(writer, "Initialized publish Worker template")?;
        writeln!(writer, "  worker: {}", self.worker_dir)?;
        writeln!(writer, "  manifest: {}", self.manifest_path)?;
        writeln!(writer, "  files written: {}", self.files_written)?;
        writeln!(writer, "  files current: {}", self.files_current)?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishSyncOutput {
    dry_run: bool,
    site_id: Option<String>,
    selected_ref: Option<String>,
    refs_count: usize,
    revision_count: usize,
    default_ref: Option<String>,
    latest_revision_oid: Option<String>,
    file_count: usize,
    ai_object_count: usize,
    ai_bundle_count: usize,
    updates_full_refs_generation: bool,
    refs: Vec<PublishSyncRefOutput>,
    revisions: Vec<PublishSyncRevisionOutput>,
    warnings: Vec<String>,
}

impl CommandOutput for PublishSyncOutput {
    fn render_human(&self, writer: &mut dyn Write, output: &OutputConfig) -> io::Result<()> {
        if output.quiet {
            return Ok(());
        }
        if self.dry_run {
            writeln!(writer, "Publish dry-run plan")?;
        } else {
            writeln!(writer, "Publish sync complete")?;
        }
        writeln!(writer, "  refs: {}", self.refs_count)?;
        writeln!(writer, "  revisions: {}", self.revision_count)?;
        writeln!(
            writer,
            "  default ref: {}",
            self.default_ref.as_deref().unwrap_or("<none>")
        )?;
        writeln!(
            writer,
            "  latest revision: {}",
            self.latest_revision_oid.as_deref().unwrap_or("<none>")
        )?;
        writeln!(writer, "  files: {}", self.file_count)?;
        writeln!(writer, "  AI objects: {}", self.ai_object_count)?;
        writeln!(writer, "  AI bundles: {}", self.ai_bundle_count)?;
        writeln!(
            writer,
            "  updates full refs generation: {}",
            self.updates_full_refs_generation
        )?;
        if !self.warnings.is_empty() {
            writeln!(writer, "  warnings:")?;
            for warning in &self.warnings {
                writeln!(writer, "    - {warning}")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublishDeployStepState {
    Completed,
    Skipped,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishDeployStepSummary {
    name: String,
    command: Vec<String>,
    state: PublishDeployStepState,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishDeployOutput {
    worker_dir: String,
    template_status: WorkerTemplateStatus,
    steps: Vec<PublishDeployStepSummary>,
    deploy_url: Option<String>,
}

impl CommandOutput for PublishDeployOutput {
    fn render_human(&self, writer: &mut dyn Write, output: &OutputConfig) -> io::Result<()> {
        if output.quiet {
            return Ok(());
        }
        writeln!(writer, "Publish Worker deploy")?;
        writeln!(writer, "  worker: {}", self.worker_dir)?;
        writeln!(
            writer,
            "  template status: {}",
            self.template_status.as_str()
        )?;
        for step in &self.steps {
            let status = match step.state {
                PublishDeployStepState::Completed => "completed",
                PublishDeployStepState::Skipped => "skipped",
            };
            writeln!(writer, "  {status}: {}", step.command.join(" "))?;
        }
        writeln!(
            writer,
            "  deploy URL: {}",
            self.deploy_url.as_deref().unwrap_or("<skipped>")
        )?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishUnpublishOutput {
    worker_dir: String,
    site_id: String,
    status: String,
    command: Vec<String>,
}

impl CommandOutput for PublishUnpublishOutput {
    fn render_human(&self, writer: &mut dyn Write, output: &OutputConfig) -> io::Result<()> {
        if output.quiet {
            return Ok(());
        }
        writeln!(writer, "Unpublished site {}", self.site_id)?;
        writeln!(writer, "  worker: {}", self.worker_dir)?;
        writeln!(writer, "  status: {}", self.status)?;
        writeln!(writer, "  command: {}", self.command.join(" "))?;
        Ok(())
    }
}

#[derive(Debug)]
struct PublishWorkerCommandOutput {
    success: bool,
    status_code: Option<i32>,
    stdout: String,
    stderr: String,
}

trait PublishWorkerCommandRunner {
    fn run(
        &mut self,
        worker_dir: &Path,
        program: &str,
        args: &[&str],
    ) -> io::Result<PublishWorkerCommandOutput>;
}

struct ProcessPublishWorkerCommandRunner;

impl PublishWorkerCommandRunner for ProcessPublishWorkerCommandRunner {
    fn run(
        &mut self,
        worker_dir: &Path,
        program: &str,
        args: &[&str],
    ) -> io::Result<PublishWorkerCommandOutput> {
        let output = Command::new(program)
            .args(args)
            .current_dir(worker_dir)
            .env("CI", "1")
            .output()?;
        Ok(PublishWorkerCommandOutput {
            success: output.status.success(),
            status_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_publish_deploy_at_root(
    repo_root: &Path,
    manifest_path: &Path,
    args: &DeployArgs,
    runner: &mut dyn PublishWorkerCommandRunner,
) -> CliResult<PublishDeployOutput> {
    let status = run_publish_status_at_root(repo_root, manifest_path)?;
    ensure_publish_deploy_template_ready(&status)?;
    let worker_dir = repo_root.join("worker");
    ensure_publish_deploy_files(&worker_dir)?;

    let mut steps = Vec::new();
    run_publish_deploy_step(runner, &worker_dir, "build", "pnpm", &["build"], &mut steps)?;

    let mut deploy_url = None;
    if args.skip_deploy {
        steps.push(PublishDeployStepSummary {
            name: "d1_migrations".to_string(),
            command: command_summary(
                "pnpm",
                &[
                    "exec",
                    "wrangler",
                    "d1",
                    "migrations",
                    "apply",
                    "LIBRA_PUBLISH_DB",
                    "--remote",
                ],
            ),
            state: PublishDeployStepState::Skipped,
        });
        steps.push(PublishDeployStepSummary {
            name: "deploy".to_string(),
            command: command_summary("pnpm", &["exec", "opennextjs-cloudflare", "deploy"]),
            state: PublishDeployStepState::Skipped,
        });
    } else {
        run_publish_deploy_step(
            runner,
            &worker_dir,
            "d1_migrations",
            "pnpm",
            &[
                "exec",
                "wrangler",
                "d1",
                "migrations",
                "apply",
                "LIBRA_PUBLISH_DB",
                "--remote",
            ],
            &mut steps,
        )?;
        let output = run_publish_deploy_step(
            runner,
            &worker_dir,
            "deploy",
            "pnpm",
            &["exec", "opennextjs-cloudflare", "deploy"],
            &mut steps,
        )?;
        let combined = format!("{}\n{}", output.stdout, output.stderr);
        deploy_url = extract_first_url(&combined);
        if deploy_url.is_none() {
            return Err(CliError::fatal(
                "publish deploy completed but no deployment URL was found in Wrangler output",
            )
            .with_stable_code(StableErrorCode::NetworkProtocol)
            .with_hint("inspect the deploy output and verify the Worker route/domain."));
        }
    }

    Ok(PublishDeployOutput {
        worker_dir: "worker".to_string(),
        template_status: status.status,
        steps,
        deploy_url,
    })
}

async fn resolve_unpublish_site_id(args: &UnpublishArgs) -> CliResult<String> {
    if let Some(site_id) = args.site_id.as_deref() {
        return validate_publish_site_id(site_id);
    }

    let entry = ConfigKv::get("publish.site_id").await.map_err(|source| {
        CliError::fatal(format!(
            "failed to read publish.site_id from repository config: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let Some(entry) = entry else {
        return Err(CliError::failure(
            "publish unpublish requires --site-id or publish.site_id config",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("pass '--site-id <uuid>' or configure publish.site_id before unpublishing."));
    };
    validate_publish_site_id(&entry.value)
}

async fn resolve_publish_status_site_id(args: &StatusArgs) -> CliResult<Option<String>> {
    if let Some(site_id) = args.site_id.as_deref() {
        return validate_publish_site_id(site_id).map(Some);
    }

    let entry = ConfigKv::get("publish.site_id").await.map_err(|source| {
        CliError::fatal(format!(
            "failed to read publish.site_id from repository config: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    validate_publish_site_id(&entry.value).map(Some)
}

fn validate_publish_site_id(site_id: &str) -> CliResult<String> {
    uuid::Uuid::parse_str(site_id)
        .map(|uuid| uuid.to_string())
        .map_err(|source| {
            CliError::failure(format!(
                "publish site id '{site_id}' is not a valid UUID: {source}"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("use the UUID stored in publish.site_id or the D1 publish_sites row.")
        })
}

fn publish_status_d1_error(operation: &str, source: D1Error) -> CliError {
    let stable_code = match source.code {
        1001..=1003 => StableErrorCode::AuthMissingCredentials,
        2001..=2003 | 2005..=2006 => StableErrorCode::NetworkUnavailable,
        _ => StableErrorCode::NetworkProtocol,
    };
    CliError::fatal(format!("{operation}: {}", source.message))
        .with_stable_code(stable_code)
        .with_hint(
            "set vault.env.LIBRA_D1_ACCOUNT_ID, vault.env.LIBRA_D1_API_TOKEN, and \
             vault.env.LIBRA_D1_DATABASE_ID with `libra config set`, or export the matching \
             variables for cloud comparison. Omit --site-id/publish.site_id to inspect only \
             the local template.",
        )
}

fn run_publish_unpublish_at_root(
    repo_root: &Path,
    manifest_path: &Path,
    args: &UnpublishArgs,
    site_id: &str,
    runner: &mut dyn PublishWorkerCommandRunner,
) -> CliResult<PublishUnpublishOutput> {
    if !args.yes {
        return Err(CliError::command_usage(
            "publish unpublish requires --yes to confirm disabling the site",
        ));
    }

    let status = run_publish_status_at_root(repo_root, manifest_path)?;
    ensure_publish_deploy_template_ready(&status)?;
    let worker_dir = repo_root.join("worker");
    ensure_publish_deploy_files(&worker_dir)?;

    let sql = format!(
        "UPDATE publish_sites SET status = 'disabled', updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE site_id = '{site_id}';"
    );
    let command = command_summary(
        "pnpm",
        &[
            "exec",
            "wrangler",
            "d1",
            "execute",
            "LIBRA_PUBLISH_DB",
            "--remote",
            "--yes",
            "--command",
            &sql,
        ],
    );
    let output = runner
        .run(
            &worker_dir,
            "pnpm",
            &[
                "exec",
                "wrangler",
                "d1",
                "execute",
                "LIBRA_PUBLISH_DB",
                "--remote",
                "--yes",
                "--command",
                &sql,
            ],
        )
        .map_err(|source| {
            CliError::fatal(format!(
                "failed to start publish unpublish command: {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    if !output.success {
        return Err(CliError::fatal(format!(
            "publish unpublish failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output
                .status_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string()),
            output.stdout.trim(),
            output.stderr.trim(),
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("fix the Wrangler D1 error and rerun 'libra publish unpublish --yes'."));
    }

    Ok(PublishUnpublishOutput {
        worker_dir: "worker".to_string(),
        site_id: site_id.to_string(),
        status: "disabled".to_string(),
        command,
    })
}

fn ensure_publish_deploy_template_ready(status: &PublishStatusOutput) -> CliResult<()> {
    match status.status {
        WorkerTemplateStatus::Current | WorkerTemplateStatus::Modified => Ok(()),
        WorkerTemplateStatus::Missing => Err(CliError::failure(
            "publish deploy requires a local Worker template, but it is missing",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("run 'libra publish init' before deploying.")),
        WorkerTemplateStatus::Outdated => Err(CliError::failure(
            "publish deploy requires the Worker template to be current or intentionally modified",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("rerun 'libra publish init' and review any Worker template changes.")),
        WorkerTemplateStatus::Conflicted => Err(CliError::conflict(
            "publish deploy cannot continue while Worker template paths are conflicted",
        )
        .with_hint("resolve symlinks or non-file paths under worker/, then rerun deploy.")),
    }
}

fn ensure_publish_deploy_files(worker_dir: &Path) -> CliResult<()> {
    for relative in [
        "package.json",
        "pnpm-lock.yaml",
        "wrangler.jsonc",
        "migrations/0001_publish.sql",
    ] {
        let path = worker_dir.join(relative);
        if !path.is_file() {
            return Err(
                CliError::failure(format!("publish deploy requires '{}'", path.display()))
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint("run 'libra publish init' to materialize the Worker template."),
            );
        }
    }

    let wrangler_path = worker_dir.join("wrangler.jsonc");
    let wrangler = fs::read_to_string(&wrangler_path).map_err(|source| {
        CliError::fatal(format!(
            "failed to read Worker Wrangler config '{}': {source}",
            wrangler_path.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for required in ["LIBRA_PUBLISH_DB", "LIBRA_PUBLISH_BUCKET", "ASSETS"] {
        if !wrangler.contains(required) {
            return Err(CliError::failure(format!(
                "publish deploy requires Worker binding '{required}' in '{}'",
                wrangler_path.display()
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("restore the Worker bindings generated by 'libra publish init'."));
        }
    }
    for (placeholder, resource, hint) in [
        (
            PUBLISH_D1_DATABASE_ID_PLACEHOLDER,
            "D1 database_id",
            "create a Cloudflare D1 database and replace REPLACE_WITH_D1_DATABASE_ID.",
        ),
        (
            PUBLISH_R2_BUCKET_NAME_PLACEHOLDER,
            "R2 bucket_name",
            "create a Cloudflare R2 bucket and replace REPLACE_WITH_R2_BUCKET_NAME.",
        ),
    ] {
        if wrangler.contains(placeholder) {
            return Err(CliError::failure(format!(
                "publish deploy requires a real {resource} in '{}' instead of {placeholder}",
                wrangler_path.display(),
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(hint));
        }
    }
    Ok(())
}

fn run_publish_deploy_step(
    runner: &mut dyn PublishWorkerCommandRunner,
    worker_dir: &Path,
    name: &str,
    program: &str,
    args: &[&str],
    steps: &mut Vec<PublishDeployStepSummary>,
) -> CliResult<PublishWorkerCommandOutput> {
    let output = runner.run(worker_dir, program, args).map_err(|source| {
        CliError::fatal(format!(
            "failed to start publish deploy step '{name}' ({}): {source}",
            command_summary(program, args).join(" ")
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if !output.success {
        return Err(CliError::fatal(format!(
            "publish deploy step '{name}' failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output
                .status_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string()),
            output.stdout.trim(),
            output.stderr.trim(),
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("fix the Worker build/deploy error and rerun 'libra publish deploy'."));
    }
    steps.push(PublishDeployStepSummary {
        name: name.to_string(),
        command: command_summary(program, args),
        state: PublishDeployStepState::Completed,
    });
    Ok(output)
}

fn command_summary(program: &str, args: &[&str]) -> Vec<String> {
    std::iter::once(program.to_string())
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .collect()
}

fn extract_first_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | ','
                )
            })
        })
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
        .map(|token| {
            token
                .trim_end_matches(['.', ',', ';', ')', ']', '>'])
                .to_string()
        })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishSyncRefOutput {
    ref_name: String,
    target_oid: String,
    revision_oid: String,
    is_default: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishSyncRevisionOutput {
    revision_oid: String,
    ref_count: usize,
    file_count: usize,
    preflight_denied_count: usize,
    ai_object_count: usize,
    ai_bundle_count: usize,
}

async fn run_publish_sync_dry_run(args: &SyncArgs) -> CliResult<PublishSyncOutput> {
    validate_publish_sync_args(args)?;

    let all_refs = collect_publish_refs().await?;
    if all_refs.is_empty() {
        return Err(
            CliError::failure("no local branch or tag refs are available to publish")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("create a commit on a local branch or tag a commit before publishing."),
        );
    }

    let selected_refs = select_publish_refs(&all_refs, args.r#ref.as_deref())?;
    let default_ref = resolve_publish_default_ref(&all_refs).await?;
    let selected_ref = if args.r#ref.is_some() {
        selected_refs
            .first()
            .map(|publish_ref| publish_ref.ref_name.clone())
    } else {
        None
    };
    let mut warnings = inspect_publish_dirty(args.fail_on_dirty).await?;
    if selected_ref.is_some() {
        warnings.push(
            "targeted --ref dry-run will not update the complete published refs generation"
                .to_string(),
        );
    }
    if args.force {
        warnings.push("--force has no effect during dry-run".to_string());
    }
    if !args.allow_sensitive_path.is_empty() {
        warnings.push(
            "--allow-sensitive-path is recorded for sync planning but dry-run does not evaluate \
             site visibility"
                .to_string(),
        );
    }

    let mut revision_ref_counts: BTreeMap<String, usize> = BTreeMap::new();
    for publish_ref in &selected_refs {
        *revision_ref_counts
            .entry(publish_ref.revision_oid.clone())
            .or_default() += 1;
    }

    let mut revisions = Vec::with_capacity(revision_ref_counts.len());
    for (revision_oid, ref_count) in revision_ref_counts {
        let scan = scan_revision_files(&revision_oid)?;
        for denied in &scan.denied_paths {
            warnings.push(format!(
                "publish preflight denied '{}' in revision {} ({})",
                denied.path,
                revision_oid,
                preflight_reason_label(denied.reason)
            ));
        }
        revisions.push(PublishSyncRevisionOutput {
            revision_oid,
            ref_count,
            file_count: scan.file_count,
            preflight_denied_count: scan.denied_paths.len(),
            ai_object_count: 0,
            ai_bundle_count: 0,
        });
    }

    let file_count = revisions.iter().map(|revision| revision.file_count).sum();
    let ai_object_count = revisions
        .iter()
        .map(|revision| revision.ai_object_count)
        .sum();
    let ai_bundle_count = revisions
        .iter()
        .map(|revision| revision.ai_bundle_count)
        .sum();
    let latest_revision_oid = default_ref
        .as_ref()
        .and_then(|name| {
            selected_refs
                .iter()
                .find(|publish_ref| &publish_ref.ref_name == name)
        })
        .or_else(|| selected_refs.first())
        .map(|publish_ref| publish_ref.revision_oid.clone());

    let refs = selected_refs
        .into_iter()
        .map(|publish_ref| {
            let is_default = default_ref
                .as_ref()
                .is_some_and(|name| name == &publish_ref.ref_name);
            PublishSyncRefOutput {
                ref_name: publish_ref.ref_name,
                target_oid: publish_ref.target_oid,
                revision_oid: publish_ref.revision_oid,
                is_default,
            }
        })
        .collect::<Vec<_>>();

    Ok(PublishSyncOutput {
        dry_run: true,
        site_id: None,
        selected_ref,
        refs_count: refs.len(),
        revision_count: revisions.len(),
        default_ref,
        latest_revision_oid,
        file_count,
        ai_object_count,
        ai_bundle_count,
        updates_full_refs_generation: args.r#ref.is_none(),
        refs,
        revisions,
        warnings,
    })
}

async fn run_publish_sync_non_dry_run(args: &SyncArgs) -> CliResult<PublishSyncOutput> {
    validate_publish_sync_args(args)?;

    let site_id = resolve_publish_sync_site_id().await?;
    let d1_client = D1Client::from_env()
        .await
        .map_err(|source| publish_sync_d1_error("failed to initialize D1 client", source))?;
    d1_client
        .ensure_publish_schema()
        .await
        .map_err(|source| publish_sync_d1_error("failed to ensure publish D1 schema", source))?;
    let site = d1_client
        .find_publish_site(&site_id)
        .await
        .map_err(|source| publish_sync_d1_error("failed to load D1 publish site", source))?
        .ok_or_else(|| {
            CliError::failure(format!("publish site '{site_id}' was not found in D1"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(
                    "create the publish_sites row for this site or configure publish.site_id to \
                     an existing site before running 'libra publish sync'.",
                )
        })?;
    let site = publish_sync_site_context_from_row(&site)?;
    let storage = cloud::create_publish_storage(&site.repo_id, &site.site_id)
        .await
        .map_err(|source| {
            CliError::fatal(format!("failed to initialize publish R2 storage: {source}"))
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint(
                    "set vault.env.LIBRA_STORAGE_ENDPOINT, vault.env.LIBRA_STORAGE_BUCKET, \
                     vault.env.LIBRA_STORAGE_ACCESS_KEY, and vault.env.LIBRA_STORAGE_SECRET_KEY \
                     with `libra config set`, or export the matching variables.",
                )
        })?;

    let all_refs = collect_publish_refs().await?;
    if all_refs.is_empty() {
        return Err(
            CliError::failure("no local branch or tag refs are available to publish")
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("create a commit on a local branch or tag a commit before publishing."),
        );
    }
    let selected_refs = select_publish_refs(&all_refs, args.r#ref.as_deref())?;
    let default_ref = resolve_publish_default_ref(&all_refs).await?;
    let warnings = inspect_publish_dirty(args.fail_on_dirty).await?;
    let mut sink = CloudPublishSyncSink {
        d1_client,
        storage,
        force_upload: args.force,
    };
    run_publish_sync_selected_refs_with_sink(
        args,
        &site,
        selected_refs,
        default_ref,
        warnings,
        &mut sink,
    )
    .await
}

#[derive(Clone, Debug)]
struct PublishSyncSiteContext {
    repo_id: String,
    site_id: String,
    visibility: SiteVisibility,
    max_preview_bytes: u64,
    refs_generation: i64,
}

struct PublishSiteLatestUpdateRequest<'a> {
    site_id: &'a str,
    default_ref: Option<&'a str>,
    latest_revision_oid: Option<&'a str>,
    next_refs_generation: i64,
    expected_refs_generation: i64,
    updated_at: &'a str,
    force: bool,
}

#[async_trait]
trait PublishSyncSink {
    async fn upsert_sync_run(&mut self, row: PublishSyncRunRow) -> CliResult<()>;
    async fn upload_revision_artifacts(
        &mut self,
        plan: &RevisionArtifactPlan,
    ) -> CliResult<RevisionArtifactUploadSummary>;
    async fn upload_ai_export_artifacts(
        &mut self,
        plan: &AiExportPlan,
    ) -> CliResult<AiExportArtifactUploadSummary>;
    async fn upsert_revision(&mut self, row: PublishRevisionRow) -> CliResult<()>;
    async fn upsert_file(&mut self, row: PublishFileRow) -> CliResult<()>;
    async fn upsert_ai_object(&mut self, row: PublishAiObjectRow) -> CliResult<()>;
    async fn upsert_ai_version(&mut self, row: PublishAiVersionRow) -> CliResult<()>;
    async fn upload_site_index_artifacts(
        &mut self,
        artifacts: &SiteIndexArtifacts,
    ) -> CliResult<()>;
    async fn upsert_ref(&mut self, row: PublishRefRow) -> CliResult<()>;
    async fn update_site_latest(
        &mut self,
        update: PublishSiteLatestUpdateRequest<'_>,
    ) -> CliResult<PublishSiteLatestUpdateResult>;
    async fn delete_stale_refs(
        &mut self,
        site_id: &str,
        current_sync_run_id: &str,
    ) -> CliResult<i64>;
}

struct CloudPublishSyncSink {
    d1_client: D1Client,
    storage: PublishStorage,
    force_upload: bool,
}

#[async_trait]
impl PublishSyncSink for CloudPublishSyncSink {
    async fn upsert_sync_run(&mut self, row: PublishSyncRunRow) -> CliResult<()> {
        self.d1_client
            .upsert_publish_sync_run(&row)
            .await
            .map_err(|source| publish_sync_d1_error("failed to upsert publish sync run", source))
    }

    async fn upload_revision_artifacts(
        &mut self,
        plan: &RevisionArtifactPlan,
    ) -> CliResult<RevisionArtifactUploadSummary> {
        upload_revision_artifacts_with_options(
            &self.storage,
            plan,
            RevisionArtifactUploadOptions {
                force: self.force_upload,
            },
        )
        .await
        .map_err(|source| {
            CliError::fatal(format!(
                "failed to upload publish revision artifacts: {source}"
            ))
            .with_stable_code(StableErrorCode::NetworkProtocol)
        })
    }

    async fn upsert_revision(&mut self, row: PublishRevisionRow) -> CliResult<()> {
        self.d1_client
            .upsert_publish_revision(&row)
            .await
            .map_err(|source| publish_sync_d1_error("failed to upsert publish revision", source))
    }

    async fn upsert_file(&mut self, row: PublishFileRow) -> CliResult<()> {
        self.d1_client
            .upsert_publish_file(&row)
            .await
            .map_err(|source| publish_sync_d1_error("failed to upsert publish file", source))
    }

    async fn upload_ai_export_artifacts(
        &mut self,
        plan: &AiExportPlan,
    ) -> CliResult<AiExportArtifactUploadSummary> {
        upload_ai_export_artifacts_with_options(
            &self.storage,
            plan,
            RevisionArtifactUploadOptions {
                force: self.force_upload,
            },
        )
        .await
        .map_err(|source| {
            CliError::fatal(format!("failed to upload publish AI artifacts: {source}"))
                .with_stable_code(StableErrorCode::NetworkProtocol)
        })
    }

    async fn upsert_ai_object(&mut self, row: PublishAiObjectRow) -> CliResult<()> {
        self.d1_client
            .upsert_publish_ai_object(&row)
            .await
            .map_err(|source| publish_sync_d1_error("failed to upsert publish AI object", source))
    }

    async fn upsert_ai_version(&mut self, row: PublishAiVersionRow) -> CliResult<()> {
        self.d1_client
            .upsert_publish_ai_version(&row)
            .await
            .map_err(|source| publish_sync_d1_error("failed to upsert publish AI version", source))
    }

    async fn upload_site_index_artifacts(
        &mut self,
        artifacts: &SiteIndexArtifacts,
    ) -> CliResult<()> {
        upload_site_index_artifacts(&self.storage, artifacts)
            .await
            .map_err(|source| {
                CliError::fatal(format!(
                    "failed to upload publish site index artifacts: {source}"
                ))
                .with_stable_code(StableErrorCode::NetworkProtocol)
            })
    }

    async fn upsert_ref(&mut self, row: PublishRefRow) -> CliResult<()> {
        self.d1_client
            .upsert_publish_ref(&row)
            .await
            .map_err(|source| publish_sync_d1_error("failed to upsert publish ref", source))
    }

    async fn update_site_latest(
        &mut self,
        update: PublishSiteLatestUpdateRequest<'_>,
    ) -> CliResult<PublishSiteLatestUpdateResult> {
        self.d1_client
            .update_publish_site_latest(PublishSiteLatestUpdate {
                site_id: update.site_id,
                default_ref: update.default_ref,
                latest_revision_oid: update.latest_revision_oid,
                next_refs_generation: update.next_refs_generation,
                expected_refs_generation: update.expected_refs_generation,
                updated_at: update.updated_at,
                force: update.force,
            })
            .await
            .map_err(|source| publish_sync_d1_error("failed to update publish site latest", source))
    }

    async fn delete_stale_refs(
        &mut self,
        site_id: &str,
        current_sync_run_id: &str,
    ) -> CliResult<i64> {
        self.d1_client
            .delete_publish_refs_for_other_sync_runs(site_id, current_sync_run_id)
            .await
            .map_err(|source| publish_sync_d1_error("failed to delete stale publish refs", source))
    }
}

struct PublishRevisionExecutionPlan {
    artifact: RevisionArtifactPlan,
    rows: RevisionD1Rows,
    ai_plan: AiExportPlan,
    ai_rows: AiExportD1Rows,
    ref_count: usize,
    preflight_denied_count: usize,
}

struct PublishAiExportPlanInput {
    repo_id: String,
    site_id: String,
    revision_oid: String,
    tree_oid: String,
    generated_at: chrono::DateTime<Utc>,
    redaction_mode: RedactionMode,
    redaction_rules_version: String,
}

#[async_trait]
trait PublishAiExportPlanner {
    async fn plan_revision_ai_export(
        &self,
        input: PublishAiExportPlanInput,
    ) -> CliResult<AiExportPlan>;
}

struct HistoryBackedPublishAiExportPlanner {
    history: HistoryManager,
    storage: Arc<LocalStorage>,
}

impl HistoryBackedPublishAiExportPlanner {
    async fn new() -> CliResult<Self> {
        let repo_path = util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
        let storage = Arc::new(LocalStorage::new(repo_path.join("objects")));
        let db_path = repo_path.join(util::DATABASE);
        let db_conn = db::get_db_conn_instance_for_path(&db_path)
            .await
            .map_err(|source| {
                CliError::fatal(format!(
                    "failed to open repository database '{}': {source}",
                    db_path.display()
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
            })?;
        let history = HistoryManager::new(storage.clone(), repo_path, Arc::new(db_conn));
        Ok(Self { history, storage })
    }
}

#[async_trait]
impl PublishAiExportPlanner for HistoryBackedPublishAiExportPlanner {
    async fn plan_revision_ai_export(
        &self,
        input: PublishAiExportPlanInput,
    ) -> CliResult<AiExportPlan> {
        let mut objects = collect_publish_ai_objects_from_history(
            &self.history,
            self.storage.as_ref(),
            HistoryAiExportRequest {
                site_id: input.site_id.clone(),
                revision_oid: input.revision_oid.clone(),
                source_ref: format!("revision/{}", input.revision_oid),
                redaction_mode: input.redaction_mode,
                redaction_rules_version: input.redaction_rules_version.clone(),
            },
        )
        .await
        .map_err(|source| {
            publish_internal_error(format!(
                "failed to collect publish AI history objects: {source}"
            ))
        })?;
        let projection_rebuilder = ProjectionRebuilder::new(self.storage.as_ref(), &self.history);
        let projection_rebuilds =
            projection_rebuilder
                .rebuild_all_threads()
                .await
                .map_err(|source| {
                    publish_internal_error(format!(
                        "failed to rebuild publish AI projection objects: {source:#}"
                    ))
                })?;
        if !objects.is_empty() && projection_rebuilds.is_empty() {
            return Err(publish_internal_error(format!(
                "failed to rebuild publish AI projection objects: missing projection object types \
                 {}; no rebuildable Intent, Task, or Run history was found",
                PUBLISH_AI_PROJECTION_OBJECT_TYPES.join(", ")
            )));
        }
        for rebuild in projection_rebuilds {
            objects.extend(
                build_publish_ai_projection_objects(
                    &rebuild,
                    ProjectionAiExportRequest {
                        site_id: input.site_id.clone(),
                        revision_oid: input.revision_oid.clone(),
                        source_ref: format!("revision/{}", input.revision_oid),
                        redaction_mode: input.redaction_mode,
                        redaction_rules_version: input.redaction_rules_version.clone(),
                    },
                )
                .map_err(|source| {
                    publish_internal_error(format!(
                        "failed to build publish AI projection objects: {source}"
                    ))
                })?,
            );
        }

        build_ai_export_plan(AiExportRequest {
            repo_id: input.repo_id,
            site_id: input.site_id,
            revision_oid: input.revision_oid.clone(),
            ai_version_id: format!("ai-{}", input.revision_oid),
            generated_at: input.generated_at,
            ai_object_model_reference: "docs/ai/object-model-reference.md".to_string(),
            redaction_mode: input.redaction_mode,
            redaction_rules_version: input.redaction_rules_version,
            associated_ids: AiBundleAssociatedIds {
                tree_oid: Some(input.tree_oid),
                ..AiBundleAssociatedIds::default()
            },
            objects,
        })
        .map_err(|source| {
            publish_internal_error(format!("failed to build publish AI export plan: {source}"))
        })
    }
}

async fn run_publish_sync_selected_refs_with_sink(
    args: &SyncArgs,
    site: &PublishSyncSiteContext,
    selected_refs: Vec<RefInput>,
    default_ref: Option<String>,
    warnings: Vec<String>,
    sink: &mut dyn PublishSyncSink,
) -> CliResult<PublishSyncOutput> {
    let ai_planner = HistoryBackedPublishAiExportPlanner::new().await?;
    run_publish_sync_selected_refs_with_sink_and_ai_planner(
        args,
        site,
        selected_refs,
        default_ref,
        warnings,
        sink,
        &ai_planner,
    )
    .await
}

async fn run_publish_sync_selected_refs_with_sink_and_ai_planner(
    args: &SyncArgs,
    site: &PublishSyncSiteContext,
    selected_refs: Vec<RefInput>,
    default_ref: Option<String>,
    mut warnings: Vec<String>,
    sink: &mut dyn PublishSyncSink,
    ai_planner: &dyn PublishAiExportPlanner,
) -> CliResult<PublishSyncOutput> {
    if selected_refs.is_empty() {
        return Err(
            CliError::failure("no local branch or tag refs are available to publish")
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }

    let updates_full_refs_generation = args.r#ref.is_none();
    let selected_ref = if updates_full_refs_generation {
        None
    } else {
        selected_refs
            .first()
            .map(|publish_ref| publish_ref.ref_name.clone())
    };
    if selected_ref.is_some() {
        warnings.push(
            "targeted --ref sync will not update the complete published refs generation"
                .to_string(),
        );
    }
    let generated_at = Utc::now();
    let sync_run_id = uuid::Uuid::new_v4().to_string();
    let redaction_mode = publish_redaction_mode(args)?;

    let revision_ref_counts = revision_ref_counts(&selected_refs);
    let mut revision_plans = Vec::with_capacity(revision_ref_counts.len());
    for (revision_oid, ref_count) in revision_ref_counts {
        let materialized = materialize_revision_files(&revision_oid)?;
        let preflight = preflight_for_revision_items_with_visibility(
            &materialized.tree_items,
            &revision_oid,
            site.visibility,
            args.allow_sensitive_path.clone(),
        )?;
        let config = SnapshotConfig {
            max_preview_bytes: site.max_preview_bytes,
            preflight,
        };
        let artifact = build_revision_artifact_plan(
            &site.repo_id,
            &site.site_id,
            &materialized.revision_oid,
            &materialized.commit_oid,
            &materialized.tree_oid,
            generated_at,
            materialized.files,
            &config,
        )
        .map_err(snapshot_ref_error)?;
        for file in &artifact.revision.files {
            if let FileSnapshot::Ignored { path, reason, .. } = file {
                warnings.push(format!(
                    "publish preflight kept '{}' as metadata-only in revision {} ({})",
                    path,
                    artifact.revision.revision_oid,
                    ignored_reason_label(*reason)
                ));
            }
        }
        let preflight_denied_count = artifact
            .revision
            .files
            .iter()
            .filter(|file| matches!(file, FileSnapshot::Ignored { .. }))
            .count();
        let mut rows = build_revision_d1_rows(
            &artifact,
            &sync_run_id,
            redaction_mode,
            PUBLISH_REDACTION_RULES_VERSION,
        )
        .map_err(|source| {
            publish_internal_error(format!("failed to build publish D1 rows: {source}"))
        })?;
        let ai_plan = ai_planner
            .plan_revision_ai_export(PublishAiExportPlanInput {
                repo_id: site.repo_id.clone(),
                site_id: site.site_id.clone(),
                revision_oid: materialized.revision_oid.clone(),
                tree_oid: materialized.tree_oid.clone(),
                generated_at,
                redaction_mode,
                redaction_rules_version: PUBLISH_REDACTION_RULES_VERSION.to_string(),
            })
            .await?;
        let ai_rows = build_ai_export_d1_rows(&ai_plan).map_err(|source| {
            publish_internal_error(format!("failed to build publish AI D1 rows: {source}"))
        })?;
        rows.revision.ai_index_key = Some(ai_plan.index_key.clone());
        rows.revision.ai_object_count =
            usize_to_i64(ai_rows.objects.len(), "publish sync AI object count")?;
        rows.revision.ai_bundle_count = 1;
        revision_plans.push(PublishRevisionExecutionPlan {
            artifact,
            rows,
            ai_plan,
            ai_rows,
            ref_count,
            preflight_denied_count,
        });
    }

    let file_count: usize = revision_plans
        .iter()
        .map(|plan| plan.rows.files.len())
        .sum();
    let ai_object_count: usize = revision_plans
        .iter()
        .map(|plan| plan.ai_rows.objects.len())
        .sum();
    let ai_bundle_count = revision_plans.len();
    let refs_count = selected_refs.len();
    let revision_count = revision_plans.len();
    let started_at = generated_at.to_rfc3339();
    let counts = PublishSyncRunCounts {
        refs: refs_count,
        revisions: revision_count,
        files: file_count,
        ai_objects: ai_object_count,
        ai_bundles: ai_bundle_count,
    };
    let running = publish_sync_run_row(PublishSyncRunRowInput {
        site_id: &site.site_id,
        sync_run_id: &sync_run_id,
        status: "running",
        started_at: &started_at,
        finished_at: None,
        counts,
        warnings: &warnings,
        error_message: None,
    })?;
    sink.upsert_sync_run(running).await?;

    let persist_result = persist_publish_sync_plan(
        PublishSyncPersistContext {
            args,
            site,
            selected_refs: &selected_refs,
            default_ref: default_ref.as_deref(),
            generated_at,
            sync_run_id: &sync_run_id,
            revision_plans: &revision_plans,
        },
        sink,
    )
    .await;
    if let Err(error) = persist_result {
        let finished_at = Utc::now().to_rfc3339();
        let failed = publish_sync_run_row(PublishSyncRunRowInput {
            site_id: &site.site_id,
            sync_run_id: &sync_run_id,
            status: "failed",
            started_at: &started_at,
            finished_at: Some(&finished_at),
            counts,
            warnings: &warnings,
            error_message: Some(error.message()),
        })?;
        let _ = sink.upsert_sync_run(failed).await;
        return Err(error);
    }

    let finished_at = Utc::now().to_rfc3339();
    let succeeded = publish_sync_run_row(PublishSyncRunRowInput {
        site_id: &site.site_id,
        sync_run_id: &sync_run_id,
        status: "succeeded",
        started_at: &started_at,
        finished_at: Some(&finished_at),
        counts,
        warnings: &warnings,
        error_message: None,
    })?;
    sink.upsert_sync_run(succeeded).await?;

    let latest_revision_oid =
        latest_revision_oid_for_selected_refs(default_ref.as_deref(), &selected_refs);
    let refs = selected_refs
        .into_iter()
        .map(|publish_ref| {
            let is_default = default_ref
                .as_ref()
                .is_some_and(|name| name == &publish_ref.ref_name);
            PublishSyncRefOutput {
                ref_name: publish_ref.ref_name,
                target_oid: publish_ref.target_oid,
                revision_oid: publish_ref.revision_oid,
                is_default,
            }
        })
        .collect::<Vec<_>>();
    let revisions = revision_plans
        .iter()
        .map(|plan| PublishSyncRevisionOutput {
            revision_oid: plan.artifact.revision.revision_oid.clone(),
            ref_count: plan.ref_count,
            file_count: plan.rows.files.len(),
            preflight_denied_count: plan.preflight_denied_count,
            ai_object_count: plan.ai_rows.objects.len(),
            ai_bundle_count: 1,
        })
        .collect::<Vec<_>>();

    Ok(PublishSyncOutput {
        dry_run: false,
        site_id: Some(site.site_id.clone()),
        selected_ref,
        refs_count: refs.len(),
        revision_count: revisions.len(),
        default_ref,
        latest_revision_oid,
        file_count,
        ai_object_count,
        ai_bundle_count,
        updates_full_refs_generation,
        refs,
        revisions,
        warnings,
    })
}

struct PublishSyncPersistContext<'a> {
    args: &'a SyncArgs,
    site: &'a PublishSyncSiteContext,
    selected_refs: &'a [RefInput],
    default_ref: Option<&'a str>,
    generated_at: chrono::DateTime<Utc>,
    sync_run_id: &'a str,
    revision_plans: &'a [PublishRevisionExecutionPlan],
}

async fn persist_publish_sync_plan(
    context: PublishSyncPersistContext<'_>,
    sink: &mut dyn PublishSyncSink,
) -> CliResult<()> {
    for plan in context.revision_plans {
        sink.upload_revision_artifacts(&plan.artifact).await?;
        sink.upload_ai_export_artifacts(&plan.ai_plan).await?;
        sink.upsert_revision(plan.rows.revision.clone()).await?;
        for file in &plan.rows.files {
            sink.upsert_file(file.clone()).await?;
        }
        for object in &plan.ai_rows.objects {
            sink.upsert_ai_object(object.clone()).await?;
        }
        sink.upsert_ai_version(plan.ai_rows.version.clone()).await?;
    }

    if context.args.r#ref.is_none() {
        let revision_snapshots = context
            .revision_plans
            .iter()
            .map(|plan| plan.artifact.revision.clone())
            .collect::<Vec<_>>();
        let snapshot_plan = build_snapshot_plan(
            context.selected_refs,
            revision_snapshots,
            context.default_ref,
        )
        .map_err(snapshot_ref_error)?;
        let next_refs_generation =
            context.site.refs_generation.checked_add(1).ok_or_else(|| {
                publish_internal_error("publish refs_generation overflowed while planning sync")
            })?;
        let artifacts = build_site_index_artifacts(
            &snapshot_plan,
            &context.site.site_id,
            context.sync_run_id,
            u64::try_from(next_refs_generation).map_err(|_| {
                publish_internal_error("publish refs_generation cannot be negative")
            })?,
            context.generated_at,
        )
        .map_err(|source| {
            publish_internal_error(format!(
                "failed to build publish refs/latest artifacts: {source}"
            ))
        })?;
        sink.upload_site_index_artifacts(&artifacts).await?;
        for row in artifacts.ref_rows {
            sink.upsert_ref(row).await?;
        }
        let updated_at = context.generated_at.to_rfc3339();
        let update = PublishSiteLatestUpdateRequest {
            site_id: &context.site.site_id,
            default_ref: Some(&artifacts.latest.default_ref),
            latest_revision_oid: Some(&artifacts.latest.latest_revision_oid),
            next_refs_generation,
            expected_refs_generation: context.site.refs_generation,
            updated_at: &updated_at,
            force: context.args.force,
        };
        match sink.update_site_latest(update).await? {
            PublishSiteLatestUpdateResult::Updated => {}
            PublishSiteLatestUpdateResult::Conflict => {
                return Err(CliError::conflict(
                    "publish site refs_generation changed while syncing",
                )
                .with_hint(
                    "rerun 'libra publish sync' to rebuild from the latest site row, or pass \
                     '--force' if you intentionally want to overwrite the pointer.",
                ));
            }
        }
        sink.delete_stale_refs(&context.site.site_id, context.sync_run_id)
            .await?;
    } else {
        let updated_at = context.generated_at.to_rfc3339();
        for publish_ref in context.selected_refs {
            sink.upsert_ref(build_publish_ref_row(
                &context.site.site_id,
                context.sync_run_id,
                &updated_at,
                context.default_ref,
                publish_ref,
            ))
            .await?;
        }
    }

    Ok(())
}

fn revision_ref_counts(selected_refs: &[RefInput]) -> BTreeMap<String, usize> {
    let mut revision_ref_counts = BTreeMap::new();
    for publish_ref in selected_refs {
        *revision_ref_counts
            .entry(publish_ref.revision_oid.clone())
            .or_default() += 1;
    }
    revision_ref_counts
}

fn latest_revision_oid_for_selected_refs(
    default_ref: Option<&str>,
    selected_refs: &[RefInput],
) -> Option<String> {
    default_ref
        .and_then(|name| {
            selected_refs
                .iter()
                .find(|publish_ref| publish_ref.ref_name == name)
        })
        .or_else(|| selected_refs.first())
        .map(|publish_ref| publish_ref.revision_oid.clone())
}

fn build_publish_ref_row(
    site_id: &str,
    sync_run_id: &str,
    updated_at: &str,
    default_ref: Option<&str>,
    publish_ref: &RefInput,
) -> PublishRefRow {
    PublishRefRow {
        site_id: site_id.to_string(),
        ref_name: publish_ref.ref_name.clone(),
        ref_type: if publish_ref.ref_name.starts_with("refs/tags/") {
            "tag".to_string()
        } else {
            "branch".to_string()
        },
        short_name: publish_short_ref_name(&publish_ref.ref_name)
            .unwrap_or(&publish_ref.ref_name)
            .to_string(),
        target_oid: publish_ref.target_oid.clone(),
        revision_oid: publish_ref.revision_oid.clone(),
        is_default: if default_ref.is_some_and(|name| name == publish_ref.ref_name) {
            1
        } else {
            0
        },
        sync_run_id: sync_run_id.to_string(),
        schema_version: i64::from(PUBLISH_SCHEMA_VERSION),
        updated_at: updated_at.to_string(),
    }
}

#[derive(Clone, Copy)]
struct PublishSyncRunCounts {
    refs: usize,
    revisions: usize,
    files: usize,
    ai_objects: usize,
    ai_bundles: usize,
}

struct PublishSyncRunRowInput<'a> {
    site_id: &'a str,
    sync_run_id: &'a str,
    status: &'a str,
    started_at: &'a str,
    finished_at: Option<&'a str>,
    counts: PublishSyncRunCounts,
    warnings: &'a [String],
    error_message: Option<&'a str>,
}

fn publish_sync_run_row(input: PublishSyncRunRowInput<'_>) -> CliResult<PublishSyncRunRow> {
    let warnings_json = serde_json::to_string(input.warnings).map_err(|source| {
        CliError::internal(format!("failed to encode publish sync warnings: {source}"))
    })?;
    Ok(PublishSyncRunRow {
        sync_run_id: input.sync_run_id.to_string(),
        site_id: input.site_id.to_string(),
        status: input.status.to_string(),
        started_at: input.started_at.to_string(),
        finished_at: input.finished_at.map(ToString::to_string),
        refs_count: usize_to_i64(input.counts.refs, "publish sync refs count")?,
        revision_count: usize_to_i64(input.counts.revisions, "publish sync revision count")?,
        file_count: usize_to_i64(input.counts.files, "publish sync file count")?,
        ai_object_count: usize_to_i64(input.counts.ai_objects, "publish sync AI object count")?,
        ai_bundle_count: usize_to_i64(input.counts.ai_bundles, "publish sync AI bundle count")?,
        warnings_json,
        error_message: input.error_message.map(ToString::to_string),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: i64::from(PUBLISH_SCHEMA_VERSION),
    })
}

fn usize_to_i64(value: usize, label: &str) -> CliResult<i64> {
    i64::try_from(value)
        .map_err(|_| publish_internal_error(format!("{label} exceeds D1 integer range")))
}

async fn resolve_publish_sync_site_id() -> CliResult<String> {
    let entry = ConfigKv::get("publish.site_id").await.map_err(|source| {
        CliError::fatal(format!(
            "failed to read publish.site_id from repository config: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let Some(entry) = entry else {
        return Err(
            CliError::failure("publish sync requires publish.site_id config")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("configure publish.site_id before running 'libra publish sync'."),
        );
    };
    validate_publish_site_id(&entry.value)
}

fn publish_sync_site_context_from_row(row: &PublishSiteRow) -> CliResult<PublishSyncSiteContext> {
    let visibility = match row.visibility.as_str() {
        "public" => SiteVisibility::Public,
        "private" => SiteVisibility::Private,
        value => {
            return Err(CliError::failure(format!(
                "publish site '{}' has invalid visibility '{value}'",
                row.site_id
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("set publish_sites.visibility to 'public' or 'private'."));
        }
    };
    let max_preview_bytes = u64::try_from(row.max_preview_bytes).map_err(|_| {
        CliError::failure(format!(
            "publish site '{}' has negative max_preview_bytes {}",
            row.site_id, row.max_preview_bytes
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    if max_preview_bytes == 0 {
        return Err(CliError::failure(format!(
            "publish site '{}' has max_preview_bytes 0",
            row.site_id
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("set publish_sites.max_preview_bytes to a positive byte count."));
    }
    Ok(PublishSyncSiteContext {
        repo_id: row.repo_id.clone(),
        site_id: row.site_id.clone(),
        visibility,
        max_preview_bytes,
        refs_generation: row.refs_generation,
    })
}

fn publish_redaction_mode(args: &SyncArgs) -> CliResult<RedactionMode> {
    match args.ai_redaction.as_str() {
        "default" => Ok(RedactionMode::Default),
        "strict" => Ok(RedactionMode::Strict),
        value => Err(CliError::command_usage(format!(
            "invalid --ai-redaction value '{value}'; expected 'default' or 'strict'"
        ))),
    }
}

fn publish_sync_d1_error(operation: &str, source: D1Error) -> CliError {
    let stable_code = match source.code {
        1001..=1003 => StableErrorCode::AuthMissingCredentials,
        2001..=2003 | 2005..=2006 => StableErrorCode::NetworkUnavailable,
        _ => StableErrorCode::NetworkProtocol,
    };
    CliError::fatal(format!("{operation}: {}", source.message))
        .with_stable_code(stable_code)
        .with_hint(
            "set vault.env.LIBRA_D1_ACCOUNT_ID, vault.env.LIBRA_D1_API_TOKEN, \
             vault.env.LIBRA_D1_DATABASE_ID with `libra config set`, or export the matching \
             variables, and set publish.site_id before running 'libra publish sync'.",
        )
}

fn validate_publish_sync_args(args: &SyncArgs) -> CliResult<()> {
    match args.ai_redaction.as_str() {
        "default" | "strict" => Ok(()),
        value => Err(CliError::command_usage(format!(
            "invalid --ai-redaction value '{value}'; expected 'default' or 'strict'"
        ))),
    }
}

async fn collect_publish_refs() -> CliResult<Vec<RefInput>> {
    let branches = Branch::list_branches_result(None).await.map_err(|source| {
        CliError::fatal(format!(
            "failed to list local branches for publish dry-run: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let mut refs = Vec::new();
    for branch in branches {
        let target_oid = branch.commit.to_string();
        refs.push(RefInput {
            ref_name: format!("refs/heads/{}", branch.name),
            target_oid: target_oid.clone(),
            revision_oid: target_oid,
        });
    }

    let tags = tag::list().await.map_err(|source| {
        CliError::fatal(format!(
            "failed to list local tags for publish dry-run: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    for publish_tag in tags {
        let ref_name = format!("refs/tags/{}", publish_tag.name);
        let (target_oid, revision_oid) = match publish_tag.object {
            TagObject::Commit(commit) => {
                let oid = commit.id.to_string();
                (oid.clone(), oid)
            }
            TagObject::Tag(tag_object) => {
                let revision_oid = match tag_object.object_type {
                    ObjectType::Commit => tag_object.object_hash,
                    ObjectType::Tag => util::get_commit_base_typed(&publish_tag.name)
                        .await
                        .map_err(|source| {
                            CliError::fatal(format!(
                                "failed to peel publish tag '{}' to a commit: {source}",
                                publish_tag.name
                            ))
                            .with_stable_code(StableErrorCode::RepoStateInvalid)
                        })?,
                    target_type => {
                        return Err(CliError::failure(format!(
                            "publish tag '{}' does not point to a commit; target type is \
                             {target_type}",
                            publish_tag.name
                        ))
                        .with_stable_code(StableErrorCode::CliInvalidTarget)
                        .with_hint("publish only branch and tag refs that resolve to commits."));
                    }
                };
                (tag_object.id.to_string(), revision_oid.to_string())
            }
            TagObject::Tree(_) | TagObject::Blob(_) => {
                return Err(CliError::failure(format!(
                    "publish tag '{}' does not point to a commit",
                    publish_tag.name
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("publish only branch and tag refs that resolve to commits."));
            }
        };
        refs.push(RefInput {
            ref_name,
            target_oid,
            revision_oid,
        });
    }

    refs.sort_by(|left, right| left.ref_name.cmp(&right.ref_name));
    for publish_ref in &refs {
        validate_ref_name(&publish_ref.ref_name).map_err(snapshot_ref_error)?;
        validate_oid(&publish_ref.target_oid).map_err(snapshot_ref_error)?;
        validate_oid(&publish_ref.revision_oid).map_err(snapshot_ref_error)?;
    }
    Ok(refs)
}

fn select_publish_refs(all_refs: &[RefInput], selected: Option<&str>) -> CliResult<Vec<RefInput>> {
    let Some(raw_ref) = selected else {
        return Ok(all_refs.to_vec());
    };
    let trimmed = raw_ref.trim();
    if trimmed.is_empty() || trimmed != raw_ref {
        return Err(CliError::command_usage(
            "--ref must be a non-empty branch, tag, or full refs/heads/* / refs/tags/* name",
        ));
    }

    let selected_full_ref = if raw_ref.starts_with("refs/") {
        validate_ref_name(raw_ref).map_err(snapshot_ref_error)?;
        raw_ref.to_string()
    } else {
        let ambiguous = detect_ambiguous_short_refs(all_refs);
        if ambiguous.iter().any(|short| short == raw_ref) {
            return Err(CliError::failure(format!(
                "ambiguous publish ref '{raw_ref}' matches both a branch and a tag"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint(format!(
                "use 'refs/heads/{raw_ref}' or 'refs/tags/{raw_ref}' to select one."
            )));
        }

        let matches = all_refs
            .iter()
            .filter(|publish_ref| publish_short_ref_name(&publish_ref.ref_name) == Some(raw_ref))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [publish_ref] => publish_ref.ref_name.clone(),
            [] => {
                return Err(CliError::failure(format!(
                    "publish ref '{raw_ref}' was not found among local branches or tags"
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("run 'libra show-ref --heads --tags' to inspect publishable refs."));
            }
            _ => {
                return Err(CliError::failure(format!(
                    "ambiguous publish ref '{raw_ref}' matches multiple refs"
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use a full refs/heads/* or refs/tags/* name to select one."));
            }
        }
    };

    all_refs
        .iter()
        .find(|publish_ref| publish_ref.ref_name == selected_full_ref)
        .cloned()
        .map(|publish_ref| vec![publish_ref])
        .ok_or_else(|| {
            CliError::failure(format!(
                "publish ref '{selected_full_ref}' was not found among local branches or tags"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("run 'libra show-ref --heads --tags' to inspect publishable refs.")
        })
}

fn publish_short_ref_name(full_ref: &str) -> Option<&str> {
    full_ref
        .strip_prefix("refs/heads/")
        .or_else(|| full_ref.strip_prefix("refs/tags/"))
}

async fn resolve_publish_default_ref(all_refs: &[RefInput]) -> CliResult<Option<String>> {
    let head = Head::current_result().await.map_err(|source| {
        CliError::fatal(format!(
            "failed to resolve HEAD while planning publish dry-run: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    if let Head::Branch(branch_name) = head {
        let full_ref = format!("refs/heads/{branch_name}");
        if all_refs
            .iter()
            .any(|publish_ref| publish_ref.ref_name == full_ref)
        {
            return Ok(Some(full_ref));
        }
    }

    Ok(all_refs
        .iter()
        .find(|publish_ref| publish_ref.ref_name == "refs/heads/main")
        .or_else(|| {
            all_refs
                .iter()
                .find(|publish_ref| publish_ref.ref_name.starts_with("refs/heads/"))
        })
        .or_else(|| all_refs.first())
        .map(|publish_ref| publish_ref.ref_name.clone()))
}

async fn inspect_publish_dirty(fail_on_dirty: bool) -> CliResult<Vec<String>> {
    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(CliError::from)?;
    let unstaged = status::changes_to_be_staged().map_err(CliError::from)?;
    let staged_count = staged.polymerization().len();
    let unstaged_count = unstaged.polymerization().len();
    if staged_count == 0 && unstaged_count == 0 {
        return Ok(Vec::new());
    }

    let message = format!(
        "dirty working tree has {staged_count} staged path(s) and {unstaged_count} unstaged or \
         untracked path(s); publish sync plans committed refs only"
    );
    if fail_on_dirty {
        Err(CliError::fatal(message)
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(
                "commit, stash, or discard local changes before running with --fail-on-dirty.",
            ))
    } else {
        Ok(vec![message])
    }
}

#[derive(Debug)]
struct RevisionDryRunScan {
    file_count: usize,
    denied_paths: Vec<PreflightDeniedPath>,
}

// Staged for the full non-dry-run sync orchestrator; unit-tested now
// so the next slice can wire it into D1/R2 without reworking tree IO.
#[allow(dead_code)]
#[derive(Debug)]
struct MaterializedRevisionFiles {
    revision_oid: String,
    commit_oid: String,
    tree_oid: String,
    tree_items: Vec<(PathBuf, ObjectHash)>,
    files: Vec<RevisionFileInput>,
}

#[derive(Debug)]
struct PreflightDeniedPath {
    path: String,
    reason: DenyReason,
}

fn scan_revision_files(revision_oid: &str) -> CliResult<RevisionDryRunScan> {
    let revision = load_revision_tree_items(revision_oid)?;
    let preflight = preflight_for_revision_items(&revision.tree_items, revision_oid)?;
    let mut denied_paths = Vec::new();
    for (path, _) in &revision.tree_items {
        if let PreflightDecision::Deny(reason) = preflight.evaluate(path, false) {
            denied_paths.push(PreflightDeniedPath {
                path: path.display().to_string(),
                reason,
            });
        }
    }

    Ok(RevisionDryRunScan {
        file_count: revision.tree_items.len(),
        denied_paths,
    })
}

#[allow(dead_code)]
fn materialize_revision_files(revision_oid: &str) -> CliResult<MaterializedRevisionFiles> {
    let revision = load_revision_tree_items(revision_oid)?;
    let mut files = Vec::with_capacity(revision.tree_items.len());
    for (path, blob_oid) in &revision.tree_items {
        let blob: Blob = load_object(blob_oid).map_err(|source| {
            CliError::fatal(format!(
                "failed to load publish blob '{}' at path '{}' for revision '{}': {source}",
                blob_oid,
                path.display(),
                revision_oid
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
        let path = path.to_str().ok_or_else(|| {
            CliError::failure(format!(
                "publish revision '{revision_oid}' contains a non-UTF-8 path: {}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("rename the path to valid UTF-8 before publishing.")
        })?;
        files.push(RevisionFileInput {
            path: path.to_string(),
            bytes: blob.data,
        });
    }

    Ok(MaterializedRevisionFiles {
        revision_oid: revision.revision_oid,
        commit_oid: revision.commit_oid,
        tree_oid: revision.tree_oid,
        tree_items: revision.tree_items,
        files,
    })
}

fn load_revision_tree_items(revision_oid: &str) -> CliResult<MaterializedRevisionFiles> {
    let commit_oid = ObjectHash::from_str(revision_oid).map_err(|source| {
        CliError::fatal(format!(
            "publish revision oid '{revision_oid}' is invalid: {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let commit: Commit = load_object(&commit_oid).map_err(|source| {
        CliError::fatal(format!(
            "failed to load publish revision commit '{revision_oid}': {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let tree: Tree = load_object(&commit.tree_id).map_err(|source| {
        CliError::fatal(format!(
            "failed to load publish revision tree '{}' for commit '{revision_oid}': {source}",
            commit.tree_id
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let items = tree.get_plain_items();

    Ok(MaterializedRevisionFiles {
        revision_oid: revision_oid.to_string(),
        commit_oid: commit_oid.to_string(),
        tree_oid: commit.tree_id.to_string(),
        tree_items: items,
        files: Vec::new(),
    })
}

fn preflight_for_revision_items(
    items: &[(PathBuf, ObjectHash)],
    revision_oid: &str,
) -> CliResult<Preflight> {
    let mut preflight = Preflight::new();
    extend_preflight_with_revision_ignore(&mut preflight, items, revision_oid)?;
    Ok(preflight)
}

fn preflight_for_revision_items_with_visibility(
    items: &[(PathBuf, ObjectHash)],
    revision_oid: &str,
    visibility: SiteVisibility,
    allow_sensitive_paths: Vec<String>,
) -> CliResult<Preflight> {
    let mut preflight =
        Preflight::for_visibility(visibility, allow_sensitive_paths).map_err(|source| {
            CliError::command_usage(format!("invalid publish preflight policy: {source}"))
        })?;
    extend_preflight_with_revision_ignore(&mut preflight, items, revision_oid)?;
    Ok(preflight)
}

fn extend_preflight_with_revision_ignore(
    preflight: &mut Preflight,
    items: &[(PathBuf, ObjectHash)],
    revision_oid: &str,
) -> CliResult<()> {
    let ignore_path = Path::new(".librapublishignore");
    let Some((_, ignore_oid)) = items.iter().find(|(path, _)| path == ignore_path) else {
        return Ok(());
    };

    let blob: Blob = load_object(ignore_oid).map_err(|source| {
        CliError::fatal(format!(
            "failed to load .librapublishignore for publish revision '{revision_oid}': {source}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let text = std::str::from_utf8(&blob.data).map_err(|source| {
        CliError::failure(format!(
            ".librapublishignore in publish revision '{revision_oid}' is not valid UTF-8: \
             {source}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("commit .librapublishignore as UTF-8 text before publishing.")
    })?;
    preflight.extend_with_ignore_text(text);
    Ok(())
}

fn preflight_reason_label(reason: DenyReason) -> &'static str {
    match reason {
        DenyReason::BuiltinCredential => "builtin_credential",
        DenyReason::UserIgnore => "user_ignore",
    }
}

fn ignored_reason_label(reason: crate::internal::publish::snapshot::IgnoredReason) -> &'static str {
    match reason {
        crate::internal::publish::snapshot::IgnoredReason::BuiltinCredential => {
            "builtin_credential"
        }
        crate::internal::publish::snapshot::IgnoredReason::UserIgnore => "user_ignore",
    }
}

fn snapshot_ref_error(source: impl std::error::Error) -> CliError {
    CliError::failure(format!("invalid publish ref plan: {source}"))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("publish only refs/heads/* and refs/tags/* entries with valid object ids.")
}

/// Build a `CliError::fatal(message)` with `StableErrorCode::InternalInvariant`
/// and the GitHub Issues URL hint, mirroring the per-command Cross-Cutting G
/// pattern (push.rs / tag.rs / commit.rs / stash.rs / index_pack.rs). All
/// inline `InternalInvariant` raise sites in `publish.rs` route through
/// this helper so the callsite contract is stable.
fn publish_internal_error(message: impl Into<String>) -> CliError {
    CliError::fatal(message)
        .with_stable_code(StableErrorCode::InternalInvariant)
        .with_hint(format!("this is a bug; please report it at {ISSUE_URL}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkerTemplateStatus {
    Missing,
    Current,
    Modified,
    Outdated,
    Conflicted,
}

impl WorkerTemplateStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Current => "current",
            Self::Modified => "modified",
            Self::Outdated => "outdated",
            Self::Conflicted => "conflicted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublishRefComparisonState {
    Unconfigured,
    Compared,
}

impl PublishRefComparisonState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::Compared => "compared",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PublishSnapshotIssueState {
    Missing,
    Unpublished,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishLocalRefOutput {
    ref_name: String,
    target_oid: String,
    revision_oid: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishPublishedRefOutput {
    ref_name: String,
    target_oid: String,
    revision_oid: String,
    updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishChangedRefOutput {
    ref_name: String,
    local_target_oid: String,
    published_target_oid: String,
    local_revision_oid: String,
    published_revision_oid: String,
    published_updated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishRefSnapshotIssueOutput {
    ref_name: String,
    revision_oid: String,
    state: PublishSnapshotIssueState,
    revision_status: Option<String>,
    revision_updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishRefComparisonOutput {
    state: PublishRefComparisonState,
    site_id: Option<String>,
    local_count: usize,
    published_count: usize,
    matching_count: usize,
    local_only_count: usize,
    published_only_count: usize,
    changed_count: usize,
    snapshot_issue_count: usize,
    snapshot_missing_count: usize,
    snapshot_unpublished_count: usize,
    local_only: Vec<PublishLocalRefOutput>,
    published_only: Vec<PublishPublishedRefOutput>,
    changed: Vec<PublishChangedRefOutput>,
    snapshot_issues: Vec<PublishRefSnapshotIssueOutput>,
}

impl PublishRefComparisonOutput {
    fn unconfigured() -> Self {
        Self {
            state: PublishRefComparisonState::Unconfigured,
            site_id: None,
            local_count: 0,
            published_count: 0,
            matching_count: 0,
            local_only_count: 0,
            published_only_count: 0,
            changed_count: 0,
            snapshot_issue_count: 0,
            snapshot_missing_count: 0,
            snapshot_unpublished_count: 0,
            local_only: Vec::new(),
            published_only: Vec::new(),
            changed: Vec::new(),
            snapshot_issues: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct PublishCloudStatusRows {
    refs: Vec<PublishRefRow>,
    revisions: BTreeMap<String, Option<PublishRevisionRow>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishStatusOutput {
    worker_dir: String,
    manifest_path: String,
    template_version: &'static str,
    status: WorkerTemplateStatus,
    files_total: usize,
    files_current: usize,
    files_missing: usize,
    files_modified: usize,
    files_outdated: usize,
    files_conflicted: usize,
    published_refs: PublishRefComparisonOutput,
}

impl CommandOutput for PublishStatusOutput {
    fn render_human(&self, writer: &mut dyn Write, output: &OutputConfig) -> io::Result<()> {
        if output.quiet {
            return Ok(());
        }
        writeln!(writer, "Publish Worker template status")?;
        writeln!(writer, "  status: {}", self.status.as_str())?;
        writeln!(writer, "  worker: {}", self.worker_dir)?;
        writeln!(writer, "  manifest: {}", self.manifest_path)?;
        writeln!(writer, "  template version: {}", self.template_version)?;
        writeln!(writer, "  files total: {}", self.files_total)?;
        writeln!(writer, "  files current: {}", self.files_current)?;
        writeln!(writer, "  files missing: {}", self.files_missing)?;
        writeln!(writer, "  files modified: {}", self.files_modified)?;
        writeln!(writer, "  files outdated: {}", self.files_outdated)?;
        writeln!(writer, "  files conflicted: {}", self.files_conflicted)?;
        writeln!(
            writer,
            "  published refs: {}",
            self.published_refs.state.as_str()
        )?;
        if self.published_refs.state == PublishRefComparisonState::Compared {
            writeln!(
                writer,
                "  local/published refs: {}/{}",
                self.published_refs.local_count, self.published_refs.published_count
            )?;
            writeln!(
                writer,
                "  matching refs: {}",
                self.published_refs.matching_count
            )?;
            writeln!(
                writer,
                "  changed refs: {}",
                self.published_refs.changed_count
            )?;
            writeln!(
                writer,
                "  local-only refs: {}",
                self.published_refs.local_only_count
            )?;
            writeln!(
                writer,
                "  published-only refs: {}",
                self.published_refs.published_only_count
            )?;
            writeln!(
                writer,
                "  snapshot issues: {}",
                self.published_refs.snapshot_issue_count
            )?;
        }
        Ok(())
    }
}

fn run_publish_init_at_root(
    repo_root: &Path,
    manifest_path: &Path,
    _args: &InitArgs,
) -> CliResult<PublishInitOutput> {
    let files = collect_worker_template_files()?;
    let worker_dir = repo_root.join("worker");

    let conflicts = find_worker_template_conflicts(&worker_dir, &files)?;
    if !conflicts.is_empty() {
        return Err(conflicting_worker_template_error(&conflicts));
    }

    let mut files_written = 0usize;
    let mut files_current = 0usize;
    for file in &files {
        let destination = worker_dir.join(&file.path);
        if destination.exists() {
            files_current += 1;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                CliError::fatal(format!(
                    "failed to create Worker template directory '{}': {source}",
                    parent.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
        }
        fs::write(&destination, &file.bytes).map_err(|source| {
            CliError::fatal(format!(
                "failed to write Worker template file '{}': {source}",
                destination.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        files_written += 1;
    }

    let manifest = WorkerTemplateManifest {
        schema_version: WORKER_TEMPLATE_MANIFEST_SCHEMA_VERSION,
        template_version: env!("CARGO_PKG_VERSION").to_string(),
        worker_dir: "worker".to_string(),
        files: files
            .iter()
            .map(|file| WorkerTemplateManifestFile {
                path: file.path.clone(),
                render_policy: render_policy_name(file.render_policy).to_string(),
                sha256: file.sha256.clone(),
            })
            .collect(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|source| {
        CliError::internal(format!(
            "failed to encode Worker template manifest: {source}"
        ))
    })?;
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            CliError::fatal(format!(
                "failed to create publish manifest directory '{}': {source}",
                parent.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    fs::write(manifest_path, manifest_bytes).map_err(|source| {
        CliError::fatal(format!(
            "failed to write Worker template manifest '{}': {source}",
            manifest_path.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    Ok(PublishInitOutput {
        worker_dir: "worker".to_string(),
        manifest_path: reported_manifest_path(repo_root, manifest_path),
        template_version: env!("CARGO_PKG_VERSION"),
        files_written,
        files_current,
    })
}

fn run_publish_status_at_root(
    repo_root: &Path,
    manifest_path: &Path,
) -> CliResult<PublishStatusOutput> {
    let files = collect_worker_template_files()?;
    let worker_dir = repo_root.join("worker");
    let manifest = read_worker_template_manifest(manifest_path)?;
    let manifest_hashes: BTreeMap<&str, &str> = manifest
        .as_ref()
        .map(|manifest| {
            manifest
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.sha256.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let mut files_current = 0usize;
    let mut files_missing = 0usize;
    let mut files_modified = 0usize;
    let mut files_outdated = 0usize;
    let mut files_conflicted = 0usize;

    for file in &files {
        if first_existing_symlink_path(&worker_dir, &file.path)?.is_some() {
            files_conflicted += 1;
            continue;
        }

        let destination = worker_dir.join(&file.path);
        let metadata = match fs::metadata(&destination) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                files_missing += 1;
                continue;
            }
            Err(source) => {
                return Err(CliError::io(format!(
                    "failed to inspect Worker template file '{}': {source}",
                    destination.display()
                )));
            }
        };
        if !metadata.is_file() {
            files_conflicted += 1;
            continue;
        }

        let existing = fs::read(&destination).map_err(|source| {
            CliError::io(format!(
                "failed to read existing Worker template file '{}': {source}",
                destination.display()
            ))
        })?;
        let existing_sha = hex::encode(digest(&SHA256, &existing).as_ref());
        if existing_sha == file.sha256 {
            files_current += 1;
        } else if manifest_hashes
            .get(file.path.as_str())
            .is_some_and(|hash| *hash == existing_sha)
        {
            files_outdated += 1;
        } else {
            files_modified += 1;
        }
    }

    let status = if files_conflicted > 0 {
        WorkerTemplateStatus::Conflicted
    } else if files_modified > 0 {
        WorkerTemplateStatus::Modified
    } else if files_outdated > 0 {
        WorkerTemplateStatus::Outdated
    } else if files_missing > 0 || manifest.is_none() {
        WorkerTemplateStatus::Missing
    } else {
        WorkerTemplateStatus::Current
    };

    Ok(PublishStatusOutput {
        worker_dir: "worker".to_string(),
        manifest_path: reported_manifest_path(repo_root, manifest_path),
        template_version: env!("CARGO_PKG_VERSION"),
        status,
        files_total: files.len(),
        files_current,
        files_missing,
        files_modified,
        files_outdated,
        files_conflicted,
        published_refs: PublishRefComparisonOutput::unconfigured(),
    })
}

async fn run_publish_status_command_at_root(
    repo_root: &Path,
    manifest_path: &Path,
    args: &StatusArgs,
) -> CliResult<PublishStatusOutput> {
    run_publish_status_command_at_root_with_loaders(
        repo_root,
        manifest_path,
        args,
        collect_publish_refs,
        |site_id| async move {
            let d1_client = D1Client::from_env().await.map_err(|source| {
                publish_status_d1_error(
                    "failed to initialize D1 client for publish status ref comparison",
                    source,
                )
            })?;
            let refs = d1_client
                .list_publish_refs(&site_id)
                .await
                .map_err(|source| {
                    publish_status_d1_error(
                        "failed to list D1 publish_refs for publish status",
                        source,
                    )
                })?;
            let revisions = load_publish_status_revisions(&d1_client, &site_id, &refs).await?;
            Ok(PublishCloudStatusRows { refs, revisions })
        },
    )
    .await
}

async fn run_publish_status_command_at_root_with_loaders<
    LocalRefs,
    LocalRefsFuture,
    CloudRows,
    CloudRowsFuture,
>(
    repo_root: &Path,
    manifest_path: &Path,
    args: &StatusArgs,
    load_local_refs: LocalRefs,
    load_cloud_rows: CloudRows,
) -> CliResult<PublishStatusOutput>
where
    LocalRefs: FnOnce() -> LocalRefsFuture,
    LocalRefsFuture: Future<Output = CliResult<Vec<RefInput>>>,
    CloudRows: FnOnce(String) -> CloudRowsFuture,
    CloudRowsFuture: Future<Output = CliResult<PublishCloudStatusRows>>,
{
    let mut output = run_publish_status_at_root(repo_root, manifest_path)?;
    let Some(site_id) = resolve_publish_status_site_id(args).await? else {
        return Ok(output);
    };

    let local_refs = load_local_refs().await?;
    let cloud_rows = load_cloud_rows(site_id.clone()).await?;
    output.published_refs = compare_publish_refs(
        &site_id,
        &local_refs,
        &cloud_rows.refs,
        &cloud_rows.revisions,
    );
    Ok(output)
}

async fn load_publish_status_revisions(
    d1_client: &D1Client,
    site_id: &str,
    refs: &[PublishRefRow],
) -> CliResult<BTreeMap<String, Option<PublishRevisionRow>>> {
    let mut revisions = BTreeMap::new();
    let revision_oids = refs
        .iter()
        .map(|publish_ref| publish_ref.revision_oid.as_str())
        .collect::<BTreeSet<_>>();
    for revision_oid in revision_oids {
        let revision = d1_client
            .find_publish_revision_any(site_id, revision_oid)
            .await
            .map_err(|source| {
                publish_status_d1_error(
                    "failed to read D1 publish_revisions for publish status",
                    source,
                )
            })?;
        revisions.insert(revision_oid.to_string(), revision);
    }
    Ok(revisions)
}

fn compare_publish_refs(
    site_id: &str,
    local_refs: &[RefInput],
    published_refs: &[PublishRefRow],
    published_revisions: &BTreeMap<String, Option<PublishRevisionRow>>,
) -> PublishRefComparisonOutput {
    let local_by_ref = local_refs
        .iter()
        .map(|publish_ref| (publish_ref.ref_name.as_str(), publish_ref))
        .collect::<BTreeMap<_, _>>();
    let mut published_by_ref = published_refs
        .iter()
        .map(|publish_ref| (publish_ref.ref_name.as_str(), publish_ref))
        .collect::<BTreeMap<_, _>>();

    let mut matching_count = 0usize;
    let mut local_only = Vec::new();
    let mut changed = Vec::new();
    let mut snapshot_issues = Vec::new();

    for published_ref in published_refs {
        match published_revisions.get(&published_ref.revision_oid) {
            Some(Some(revision)) if revision.status == "published" => {}
            Some(Some(revision)) => {
                snapshot_issues.push(PublishRefSnapshotIssueOutput {
                    ref_name: published_ref.ref_name.clone(),
                    revision_oid: published_ref.revision_oid.clone(),
                    state: PublishSnapshotIssueState::Unpublished,
                    revision_status: Some(revision.status.clone()),
                    revision_updated_at: Some(revision.updated_at.clone()),
                });
            }
            Some(None) | None => {
                snapshot_issues.push(PublishRefSnapshotIssueOutput {
                    ref_name: published_ref.ref_name.clone(),
                    revision_oid: published_ref.revision_oid.clone(),
                    state: PublishSnapshotIssueState::Missing,
                    revision_status: None,
                    revision_updated_at: None,
                });
            }
        }
    }

    for (ref_name, local_ref) in local_by_ref {
        let Some(published_ref) = published_by_ref.remove(ref_name) else {
            local_only.push(PublishLocalRefOutput {
                ref_name: local_ref.ref_name.clone(),
                target_oid: local_ref.target_oid.clone(),
                revision_oid: local_ref.revision_oid.clone(),
            });
            continue;
        };

        if local_ref.target_oid == published_ref.target_oid
            && local_ref.revision_oid == published_ref.revision_oid
        {
            matching_count += 1;
        } else {
            changed.push(PublishChangedRefOutput {
                ref_name: local_ref.ref_name.clone(),
                local_target_oid: local_ref.target_oid.clone(),
                published_target_oid: published_ref.target_oid.clone(),
                local_revision_oid: local_ref.revision_oid.clone(),
                published_revision_oid: published_ref.revision_oid.clone(),
                published_updated_at: published_ref.updated_at.clone(),
            });
        }
    }

    let published_only = published_by_ref
        .into_values()
        .map(|publish_ref| PublishPublishedRefOutput {
            ref_name: publish_ref.ref_name.clone(),
            target_oid: publish_ref.target_oid.clone(),
            revision_oid: publish_ref.revision_oid.clone(),
            updated_at: publish_ref.updated_at.clone(),
        })
        .collect::<Vec<_>>();

    PublishRefComparisonOutput {
        state: PublishRefComparisonState::Compared,
        site_id: Some(site_id.to_string()),
        local_count: local_refs.len(),
        published_count: published_refs.len(),
        matching_count,
        local_only_count: local_only.len(),
        published_only_count: published_only.len(),
        changed_count: changed.len(),
        snapshot_issue_count: snapshot_issues.len(),
        snapshot_missing_count: snapshot_issues
            .iter()
            .filter(|issue| issue.state == PublishSnapshotIssueState::Missing)
            .count(),
        snapshot_unpublished_count: snapshot_issues
            .iter()
            .filter(|issue| issue.state == PublishSnapshotIssueState::Unpublished)
            .count(),
        local_only,
        published_only,
        changed,
        snapshot_issues,
    }
}

fn read_worker_template_manifest(path: &Path) -> CliResult<Option<WorkerTemplateManifest>> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(CliError::io(format!(
                "failed to read Worker template manifest '{}': {source}",
                path.display()
            )));
        }
    };

    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(|source| {
            CliError::fatal(format!(
                "failed to parse Worker template manifest '{}': {source}",
                path.display()
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
        })
}

fn collect_worker_template_files() -> CliResult<Vec<TemplateFile>> {
    let policy_by_path: BTreeMap<&'static str, RenderPolicy> = MANIFEST
        .iter()
        .map(|entry| (entry.path, entry.render_policy))
        .collect();
    let mut paths: Vec<String> = WorkerTemplate::iter()
        .map(|path| path.to_string())
        .collect();
    paths.sort();

    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        validate_template_relative_path(&path)?;
        if !embed_path_is_allowed(&path) {
            return Err(CliError::internal(format!(
                "embedded Worker template path '{path}' is denied by publish safety rules"
            )));
        }
        let embedded = WorkerTemplate::get(&path).ok_or_else(|| {
            CliError::internal(format!(
                "embedded Worker template path '{path}' was listed but could not be read"
            ))
        })?;
        let bytes = embedded.data.into_owned();
        let sha256 = hex::encode(digest(&SHA256, &bytes).as_ref());
        let render_policy = policy_by_path
            .get(path.as_str())
            .copied()
            .unwrap_or(RenderPolicy::ManagedTemplate);
        files.push(TemplateFile {
            path,
            bytes,
            sha256,
            render_policy,
        });
    }

    Ok(files)
}

fn validate_template_relative_path(path: &str) -> CliResult<()> {
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err(CliError::internal(format!(
            "embedded Worker template path '{path}' must be relative"
        )));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) => {}
            _ => {
                return Err(CliError::internal(format!(
                    "embedded Worker template path '{path}' contains an invalid component"
                )));
            }
        }
    }
    Ok(())
}

fn find_worker_template_conflicts(
    worker_dir: &Path,
    files: &[TemplateFile],
) -> CliResult<Vec<String>> {
    let mut conflicts = Vec::new();
    for file in files {
        if let Some(symlink_path) = first_existing_symlink_path(worker_dir, &file.path)? {
            conflicts.push(symlink_path);
            continue;
        }

        let destination = worker_dir.join(&file.path);
        if !destination.exists() {
            continue;
        }
        let metadata = fs::metadata(&destination).map_err(|source| {
            CliError::io(format!(
                "failed to inspect Worker template file '{}': {source}",
                destination.display()
            ))
        })?;
        if !metadata.is_file() {
            conflicts.push(file.path.clone());
            continue;
        }
        let existing = fs::read(&destination).map_err(|source| {
            CliError::io(format!(
                "failed to read existing Worker template file '{}': {source}",
                destination.display()
            ))
        })?;
        if existing != file.bytes {
            conflicts.push(file.path.clone());
        }
    }
    conflicts.sort();
    conflicts.dedup();
    Ok(conflicts)
}

fn first_existing_symlink_path(
    worker_dir: &Path,
    relative_path: &str,
) -> CliResult<Option<String>> {
    if let Ok(metadata) = fs::symlink_metadata(worker_dir)
        && metadata.file_type().is_symlink()
    {
        return Ok(Some("worker".to_string()));
    }

    let mut current = PathBuf::from(worker_dir);
    let mut relative = PathBuf::new();
    for component in Path::new(relative_path).components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        current.push(segment);
        relative.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Ok(Some(format!("worker/{}", relative.display())));
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CliError::io(format!(
                    "failed to inspect Worker template path '{}': {source}",
                    current.display()
                )));
            }
        }
    }
    Ok(None)
}

fn conflicting_worker_template_error(conflicts: &[String]) -> CliError {
    let display = conflicts
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let suffix = if conflicts.len() > 5 {
        format!(" and {} more", conflicts.len() - 5)
    } else {
        String::new()
    };
    CliError::conflict(format!(
        "Worker template files would be overwritten: {display}{suffix}"
    ))
    .with_detail("operation", "publish init")
    .with_detail("conflicts", serde_json::json!(conflicts))
    .with_hint("merge or move the listed worker files, then rerun 'libra publish init'.")
}

fn render_policy_name(policy: RenderPolicy) -> &'static str {
    match policy {
        RenderPolicy::ManagedTemplate => "managed_template",
        RenderPolicy::RenderedConfig => "rendered_config",
        RenderPolicy::UserOwned => "user_owned",
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, fs};

    use git_internal::internal::object::{
        intent::Intent,
        tree::{TreeItem, TreeItemMode},
        types::ActorRef,
    };
    use serde_json::Value;

    use super::*;
    use crate::{
        command::save_object,
        internal::publish::contract::{AiObjectLayer, AiObjectRedaction, PublishAiObject},
        utils::{storage_ext::StorageExt, test},
    };

    fn default_init_args() -> InitArgs {
        InitArgs {
            slug: Some("demo".to_string()),
            clone_domain: Some("code.example.com".to_string()),
            display_origin: None,
            name: None,
            visibility: None,
            worker_name: None,
            max_preview_bytes: None,
        }
    }

    fn sync_args(targeted: bool) -> SyncArgs {
        SyncArgs {
            r#ref: targeted.then(|| "main".to_string()),
            dry_run: false,
            fail_on_dirty: false,
            ai_redaction: "default".to_string(),
            allow_sensitive_path: Vec::new(),
            force: false,
        }
    }

    /// The main-worktree-relative manifest location — the REPORTED form for
    /// main-worktree invocations, and the fixture expectation these tests
    /// assert against.
    const WORKER_TEMPLATE_MANIFEST_PATH: &str = ".libra/publish/worker-template-manifest.json";

    /// The manifest path a MAIN worktree resolves (storage == `<root>/.libra`).
    /// Unit tests exercise the `*_at_root` bodies hermetically; the
    /// linked-worktree resolution itself is covered by
    /// `publish_manifest_resolves_into_common_storage_for_linked_worktrees`.
    fn manifest_at(root: &Path) -> PathBuf {
        root.join(WORKER_TEMPLATE_MANIFEST_PATH)
    }

    /// W0 (§C.4.1.1): every worktree of a repository reads/writes ONE
    /// worker-template manifest — the one in common storage. Deriving it from
    /// the invoking worktree's root forked the deploy drift gate per worktree.
    #[test]
    fn publish_manifest_resolves_into_common_storage_for_linked_worktrees() {
        let temp = tempfile::tempdir().expect("tempdir");
        let main_gitdir = temp.path().join("main").join(".libra");
        fs::create_dir_all(&main_gitdir).expect("main gitdir");
        fs::write(main_gitdir.join("libra.db"), b"").expect("db marker");

        let linked_root = temp.path().join("wt");
        let linked_gitdir = linked_root.join(".libra");
        fs::create_dir_all(&linked_gitdir).expect("linked gitdir");
        fs::write(
            linked_gitdir.join("commondir"),
            main_gitdir.to_string_lossy().as_bytes(),
        )
        .expect("commondir pointer");

        let resolved = publish_manifest_path(&linked_root).expect("linked root resolves");
        let canonical_main = fs::canonicalize(&main_gitdir).unwrap_or(main_gitdir.clone());
        assert_eq!(
            resolved,
            canonical_main.join(WORKER_TEMPLATE_MANIFEST_STORAGE_PATH),
            "a linked worktree must resolve the manifest into COMMON storage, \
             not its local gitdir"
        );

        let main_resolved =
            publish_manifest_path(main_gitdir.parent().expect("main root")).expect("main root");
        assert_eq!(
            main_resolved, resolved,
            "main and linked worktrees must agree on ONE manifest"
        );

        // A corrupt commondir pointer refuses — it must never mint a
        // per-worktree manifest location (§C.4.1).
        fs::write(linked_gitdir.join("commondir"), b"").expect("corrupt pointer");
        let err = publish_manifest_path(&linked_root).expect_err("corrupt commondir refuses");
        assert_eq!(err.stable_code(), StableErrorCode::RepoStateInvalid);
    }

    fn publish_sync_site_context() -> PublishSyncSiteContext {
        PublishSyncSiteContext {
            repo_id: "repo-1".to_string(),
            site_id: "00000000-0000-0000-0000-000000000001".to_string(),
            visibility: SiteVisibility::Public,
            max_preview_bytes: 1024 * 1024,
            refs_generation: 7,
        }
    }

    fn commit_with_single_file(path: &str, body: &str, message: &str) -> Commit {
        let blob = Blob::from_content(body);
        save_object(&blob, &blob.id).expect("blob should save");
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            blob.id,
            path.to_string(),
        )])
        .expect("tree should build");
        save_object(&tree, &tree.id).expect("tree should save");
        let commit = Commit::from_tree_id(tree.id, Vec::new(), message);
        save_object(&commit, &commit.id).expect("commit should save");
        commit
    }

    #[derive(Default)]
    struct FakePublishWorkerCommandRunner {
        calls: Vec<Vec<String>>,
        outputs: VecDeque<PublishWorkerCommandOutput>,
    }

    impl FakePublishWorkerCommandRunner {
        fn with_outputs(outputs: impl IntoIterator<Item = PublishWorkerCommandOutput>) -> Self {
            Self {
                calls: Vec::new(),
                outputs: outputs.into_iter().collect(),
            }
        }
    }

    impl PublishWorkerCommandRunner for FakePublishWorkerCommandRunner {
        fn run(
            &mut self,
            worker_dir: &Path,
            program: &str,
            args: &[&str],
        ) -> io::Result<PublishWorkerCommandOutput> {
            assert!(
                worker_dir.ends_with("worker"),
                "deploy commands must run from worker/: {}",
                worker_dir.display()
            );
            self.calls.push(command_summary(program, args));
            Ok(self
                .outputs
                .pop_front()
                .unwrap_or_else(successful_deploy_command_output))
        }
    }

    struct SingleObjectAiExportPlanner;

    #[async_trait::async_trait]
    impl PublishAiExportPlanner for SingleObjectAiExportPlanner {
        async fn plan_revision_ai_export(
            &self,
            input: PublishAiExportPlanInput,
        ) -> CliResult<AiExportPlan> {
            build_ai_export_plan(AiExportRequest {
                repo_id: input.repo_id,
                site_id: input.site_id.clone(),
                revision_oid: input.revision_oid.clone(),
                ai_version_id: format!("ai-{}", input.revision_oid),
                generated_at: input.generated_at,
                ai_object_model_reference: "docs/ai/object-model-reference.md".to_string(),
                redaction_mode: input.redaction_mode,
                redaction_rules_version: input.redaction_rules_version.clone(),
                associated_ids: AiBundleAssociatedIds {
                    tree_oid: Some(input.tree_oid),
                    ..AiBundleAssociatedIds::default()
                },
                objects: vec![publish_ai_object(
                    &input.site_id,
                    &input.revision_oid,
                    input.redaction_mode,
                    &input.redaction_rules_version,
                )],
            })
            .map_err(|source| CliError::internal(format!("test AI export failed: {source}")))
        }
    }

    #[derive(Default)]
    struct FakePublishSyncSink {
        sync_runs: Vec<PublishSyncRunRow>,
        revision_uploads: Vec<String>,
        ai_uploads: Vec<String>,
        revisions: Vec<PublishRevisionRow>,
        files: Vec<PublishFileRow>,
        ai_objects: Vec<PublishAiObjectRow>,
        ai_versions: Vec<PublishAiVersionRow>,
        site_index_uploads: usize,
        refs: Vec<PublishRefRow>,
        latest_updates: Vec<FakeLatestUpdate>,
        stale_ref_deletes: Vec<(String, String)>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct FakeLatestUpdate {
        site_id: String,
        default_ref: Option<String>,
        latest_revision_oid: Option<String>,
        next_refs_generation: i64,
        expected_refs_generation: i64,
        force: bool,
    }

    #[async_trait::async_trait]
    impl PublishSyncSink for FakePublishSyncSink {
        async fn upsert_sync_run(&mut self, row: PublishSyncRunRow) -> CliResult<()> {
            self.sync_runs.push(row);
            Ok(())
        }

        async fn upload_revision_artifacts(
            &mut self,
            plan: &RevisionArtifactPlan,
        ) -> CliResult<RevisionArtifactUploadSummary> {
            self.revision_uploads
                .push(plan.revision.revision_oid.clone());
            Ok(RevisionArtifactUploadSummary {
                code_manifest_key: plan.code_manifest_key.clone(),
                code_manifest_uploaded: true,
                text_blob_count: plan.text_blobs.len(),
                text_blob_uploaded_count: plan.text_blobs.len(),
                text_blob_skipped_count: 0,
                text_blob_keys: plan
                    .text_blobs
                    .iter()
                    .map(|blob| blob.object_key.clone())
                    .collect(),
            })
        }

        async fn upload_ai_export_artifacts(
            &mut self,
            plan: &AiExportPlan,
        ) -> CliResult<AiExportArtifactUploadSummary> {
            self.ai_uploads.push(plan.bundle.revision_oid.clone());
            Ok(AiExportArtifactUploadSummary {
                index_uploaded: true,
                graph_uploaded: true,
                bundle_uploaded: true,
                object_count: plan.objects.len(),
                object_uploaded_count: plan.objects.len(),
                object_skipped_count: 0,
                object_keys: plan
                    .objects
                    .iter()
                    .map(|object| object.r2_key.clone())
                    .collect(),
            })
        }

        async fn upsert_revision(&mut self, row: PublishRevisionRow) -> CliResult<()> {
            self.revisions.push(row);
            Ok(())
        }

        async fn upsert_file(&mut self, row: PublishFileRow) -> CliResult<()> {
            self.files.push(row);
            Ok(())
        }

        async fn upsert_ai_object(&mut self, row: PublishAiObjectRow) -> CliResult<()> {
            self.ai_objects.push(row);
            Ok(())
        }

        async fn upsert_ai_version(&mut self, row: PublishAiVersionRow) -> CliResult<()> {
            self.ai_versions.push(row);
            Ok(())
        }

        async fn upload_site_index_artifacts(
            &mut self,
            _artifacts: &SiteIndexArtifacts,
        ) -> CliResult<()> {
            self.site_index_uploads += 1;
            Ok(())
        }

        async fn upsert_ref(&mut self, row: PublishRefRow) -> CliResult<()> {
            self.refs.push(row);
            Ok(())
        }

        async fn update_site_latest(
            &mut self,
            update: PublishSiteLatestUpdateRequest<'_>,
        ) -> CliResult<PublishSiteLatestUpdateResult> {
            self.latest_updates.push(FakeLatestUpdate {
                site_id: update.site_id.to_string(),
                default_ref: update.default_ref.map(ToString::to_string),
                latest_revision_oid: update.latest_revision_oid.map(ToString::to_string),
                next_refs_generation: update.next_refs_generation,
                expected_refs_generation: update.expected_refs_generation,
                force: update.force,
            });
            Ok(PublishSiteLatestUpdateResult::Updated)
        }

        async fn delete_stale_refs(
            &mut self,
            site_id: &str,
            current_sync_run_id: &str,
        ) -> CliResult<i64> {
            self.stale_ref_deletes
                .push((site_id.to_string(), current_sync_run_id.to_string()));
            Ok(1)
        }
    }

    fn successful_deploy_command_output() -> PublishWorkerCommandOutput {
        PublishWorkerCommandOutput {
            success: true,
            status_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn successful_deploy_command_output_with_stdout(stdout: &str) -> PublishWorkerCommandOutput {
        PublishWorkerCommandOutput {
            success: true,
            status_code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn replace_wrangler_string_field_for_test(wrangler: &str, field: &str, value: &str) -> String {
        let field_prefix = format!("\"{field}\":");
        wrangler
            .lines()
            .map(|line| {
                let trimmed = line.trim_start();
                if trimmed.starts_with(&field_prefix) {
                    let indent = &line[..line.len() - trimmed.len()];
                    format!("{indent}\"{field}\": \"{value}\",")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn replace_d1_database_id_for_test(wrangler: &str, database_id: &str) -> String {
        replace_wrangler_string_field_for_test(wrangler, "database_id", database_id)
    }

    fn replace_r2_bucket_name_for_test(wrangler: &str, bucket_name: &str) -> String {
        replace_wrangler_string_field_for_test(wrangler, "bucket_name", bucket_name)
    }

    fn make_wrangler_deployable_for_test(wrangler: &str) -> String {
        let wrangler =
            replace_d1_database_id_for_test(wrangler, "00000000-0000-0000-0000-000000000000");
        replace_r2_bucket_name_for_test(&wrangler, "libra-publish-test")
    }

    fn materialize_deployable_worker(repo_root: &Path) {
        run_publish_init_at_root(repo_root, &manifest_at(repo_root), &default_init_args())
            .expect("publish init must materialize the template");
        let wrangler_path = repo_root.join("worker/wrangler.jsonc");
        let wrangler =
            fs::read_to_string(&wrangler_path).expect("materialized wrangler config is readable");
        fs::write(&wrangler_path, make_wrangler_deployable_for_test(&wrangler))
            .expect("wrangler config placeholders should be replaceable");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_materializes_revision_file_inputs_from_commit_tree() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let readme_blob = Blob::from_content("# demo\n");
        let lib_blob = Blob::from_content("pub fn demo() {}\n");
        save_object(&readme_blob, &readme_blob.id).expect("readme blob should save");
        save_object(&lib_blob, &lib_blob.id).expect("lib blob should save");

        let src_tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            lib_blob.id,
            "lib.rs".to_string(),
        )])
        .expect("src tree should build");
        save_object(&src_tree, &src_tree.id).expect("src tree should save");

        let root_tree = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Blob, readme_blob.id, "README.md".to_string()),
            TreeItem::new(TreeItemMode::Tree, src_tree.id, "src".to_string()),
        ])
        .expect("root tree should build");
        save_object(&root_tree, &root_tree.id).expect("root tree should save");

        let commit = Commit::from_tree_id(root_tree.id, Vec::new(), "publish fixture");
        save_object(&commit, &commit.id).expect("commit should save");

        let materialized = materialize_revision_files(&commit.id.to_string())
            .expect("revision files should materialize from committed tree");

        assert_eq!(materialized.revision_oid, commit.id.to_string());
        assert_eq!(materialized.commit_oid, commit.id.to_string());
        assert_eq!(materialized.tree_oid, root_tree.id.to_string());
        assert_eq!(materialized.tree_items.len(), 2);

        let files = materialized
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.bytes.as_slice()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            files.get("README.md").copied(),
            Some(b"# demo\n".as_slice())
        );
        assert_eq!(
            files.get("src/lib.rs").copied(),
            Some(b"pub fn demo() {}\n".as_slice())
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_non_dry_run_all_refs_persists_revision_rows_and_site_index() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let readme_blob = Blob::from_content("# demo\n");
        let secret_blob = Blob::from_content("TOKEN=secret\n");
        save_object(&readme_blob, &readme_blob.id).expect("readme blob should save");
        save_object(&secret_blob, &secret_blob.id).expect("secret blob should save");

        let root_tree = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Blob, readme_blob.id, "README.md".to_string()),
            TreeItem::new(TreeItemMode::Blob, secret_blob.id, ".env.local".to_string()),
        ])
        .expect("root tree should build");
        save_object(&root_tree, &root_tree.id).expect("root tree should save");

        let commit = Commit::from_tree_id(root_tree.id, Vec::new(), "publish fixture");
        save_object(&commit, &commit.id).expect("commit should save");
        let revision_oid = commit.id.to_string();
        let refs = vec![
            publish_local_ref("refs/heads/main", &revision_oid, &revision_oid),
            publish_local_ref("refs/tags/v1", &revision_oid, &revision_oid),
        ];
        let site = publish_sync_site_context();
        let args = sync_args(false);
        let mut sink = FakePublishSyncSink::default();

        let output = run_publish_sync_selected_refs_with_sink(
            &args,
            &site,
            refs,
            Some("refs/heads/main".to_string()),
            Vec::new(),
            &mut sink,
        )
        .await
        .expect("non-dry-run sync should persist through the sink");

        assert!(!output.dry_run);
        assert_eq!(output.refs_count, 2);
        assert_eq!(output.revision_count, 1);
        assert_eq!(
            output.latest_revision_oid.as_deref(),
            Some(revision_oid.as_str())
        );
        assert!(output.updates_full_refs_generation);
        assert_eq!(sink.sync_runs.len(), 2);
        assert_eq!(sink.sync_runs[0].status, "running");
        assert_eq!(sink.sync_runs[1].status, "succeeded");
        assert_eq!(sink.revision_uploads, vec![revision_oid.clone()]);
        assert_eq!(sink.revisions.len(), 1);
        assert_eq!(sink.files.len(), 2);
        assert!(
            sink.files
                .iter()
                .any(|row| row.path == ".env.local" && row.display_mode == "ignored"),
            "built-in sensitive paths should become metadata-only rows",
        );
        assert_eq!(sink.site_index_uploads, 1);
        assert_eq!(sink.refs.len(), 2);
        assert_eq!(
            sink.stale_ref_deletes,
            vec![(site.site_id.clone(), sink.sync_runs[0].sync_run_id.clone())],
            "all-refs sync must remove publish_refs rows from older sync runs after latest CAS",
        );
        assert_eq!(
            sink.latest_updates,
            vec![FakeLatestUpdate {
                site_id: site.site_id.clone(),
                default_ref: Some("refs/heads/main".to_string()),
                latest_revision_oid: Some(revision_oid),
                next_refs_generation: 8,
                expected_refs_generation: 7,
                force: false,
            }]
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_non_dry_run_targeted_ref_does_not_advance_full_refs_generation() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let readme_blob = Blob::from_content("# targeted\n");
        save_object(&readme_blob, &readme_blob.id).expect("readme blob should save");
        let root_tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            readme_blob.id,
            "README.md".to_string(),
        )])
        .expect("root tree should build");
        save_object(&root_tree, &root_tree.id).expect("root tree should save");
        let commit = Commit::from_tree_id(root_tree.id, Vec::new(), "publish fixture");
        save_object(&commit, &commit.id).expect("commit should save");

        let revision_oid = commit.id.to_string();
        let site = publish_sync_site_context();
        let args = sync_args(true);
        let mut sink = FakePublishSyncSink::default();
        let output = run_publish_sync_selected_refs_with_sink(
            &args,
            &site,
            vec![publish_local_ref(
                "refs/heads/main",
                &revision_oid,
                &revision_oid,
            )],
            Some("refs/heads/main".to_string()),
            Vec::new(),
            &mut sink,
        )
        .await
        .expect("targeted sync should still persist the selected ref");

        assert!(!output.updates_full_refs_generation);
        assert_eq!(sink.site_index_uploads, 0);
        assert!(sink.latest_updates.is_empty());
        assert!(sink.stale_ref_deletes.is_empty());
        assert_eq!(sink.refs.len(), 1);
        assert_eq!(sink.refs[0].ref_name, "refs/heads/main");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_non_dry_run_latest_uses_default_ref_revision() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let main_commit = commit_with_single_file("README.md", "main\n", "main fixture");
        let tag_commit = commit_with_single_file("README.md", "tag\n", "tag fixture");
        let main_revision = main_commit.id.to_string();
        let tag_revision = tag_commit.id.to_string();
        let refs = vec![
            publish_local_ref("refs/heads/main", &main_revision, &main_revision),
            publish_local_ref("refs/tags/v2", &tag_revision, &tag_revision),
        ];
        let site = publish_sync_site_context();
        let args = sync_args(false);
        let mut sink = FakePublishSyncSink::default();

        let output = run_publish_sync_selected_refs_with_sink(
            &args,
            &site,
            refs,
            Some("refs/heads/main".to_string()),
            Vec::new(),
            &mut sink,
        )
        .await
        .expect("all-refs sync should persist both revisions");

        assert_eq!(output.revision_count, 2);
        assert_eq!(
            output.latest_revision_oid.as_deref(),
            Some(main_revision.as_str())
        );
        assert_eq!(sink.latest_updates.len(), 1);
        assert_eq!(
            sink.latest_updates[0].latest_revision_oid.as_deref(),
            Some(main_revision.as_str()),
            "site latest must follow the default ref revision, not the tag revision",
        );
        assert_ne!(main_revision, tag_revision);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_non_dry_run_persists_ai_artifacts_and_counts() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let commit = commit_with_single_file("README.md", "ai\n", "ai fixture");
        let revision_oid = commit.id.to_string();
        let site = publish_sync_site_context();
        let args = sync_args(false);
        let mut sink = FakePublishSyncSink::default();
        let ai_planner = SingleObjectAiExportPlanner;

        let output = run_publish_sync_selected_refs_with_sink_and_ai_planner(
            &args,
            &site,
            vec![publish_local_ref(
                "refs/heads/main",
                &revision_oid,
                &revision_oid,
            )],
            Some("refs/heads/main".to_string()),
            Vec::new(),
            &mut sink,
            &ai_planner,
        )
        .await
        .expect("non-dry-run sync should persist AI artifacts");

        assert_eq!(output.ai_object_count, 1);
        assert_eq!(output.ai_bundle_count, 1);
        assert_eq!(output.revisions[0].ai_object_count, 1);
        assert_eq!(output.revisions[0].ai_bundle_count, 1);
        assert_eq!(sink.ai_uploads, vec![revision_oid.clone()]);
        assert_eq!(sink.ai_objects.len(), 1);
        assert_eq!(sink.ai_versions.len(), 1);
        assert_eq!(sink.revisions[0].ai_object_count, 1);
        assert_eq!(sink.revisions[0].ai_bundle_count, 1);
        assert!(
            sink.revisions[0]
                .ai_index_key
                .as_deref()
                .is_some_and(|key| key.ends_with("/ai/index.json")),
            "revision row must point at the uploaded AI index"
        );
        assert_eq!(sink.sync_runs[0].ai_object_count, 1);
        assert_eq!(sink.sync_runs[0].ai_bundle_count, 1);
        assert_eq!(sink.sync_runs[1].ai_object_count, 1);
        assert_eq!(sink.sync_runs[1].ai_bundle_count, 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_non_dry_run_default_planner_exports_history_ai_objects() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let libra_dir = temp.path().join(".libra");
        let storage = Arc::new(LocalStorage::new(libra_dir.join("objects")));
        let db_conn = Arc::new(
            db::get_db_conn_instance_for_path(&libra_dir.join(util::DATABASE))
                .await
                .expect("db should open"),
        );
        let history = HistoryManager::new(storage.clone(), libra_dir, db_conn);
        let intent = Intent::new(
            ActorRef::human("publish-test").expect("actor"),
            "Publish history-backed AI objects",
        )
        .expect("intent");
        storage
            .put_tracked(&intent, &history)
            .await
            .expect("intent should be written to history");
        let second_intent = Intent::new(
            ActorRef::human("publish-test").expect("actor"),
            "Publish second history-backed thread",
        )
        .expect("second intent");
        storage
            .put_tracked(&second_intent, &history)
            .await
            .expect("second intent should be written to history");

        let commit = commit_with_single_file("README.md", "ai\n", "ai fixture");
        let revision_oid = commit.id.to_string();
        let site = publish_sync_site_context();
        let args = sync_args(false);
        let mut sink = FakePublishSyncSink::default();

        let output = run_publish_sync_selected_refs_with_sink(
            &args,
            &site,
            vec![publish_local_ref(
                "refs/heads/main",
                &revision_oid,
                &revision_oid,
            )],
            Some("refs/heads/main".to_string()),
            Vec::new(),
            &mut sink,
        )
        .await
        .expect("default planner should export AI history objects");

        let object_types = sink
            .ai_objects
            .iter()
            .map(|object| object.object_type.as_str())
            .collect::<BTreeSet<_>>();
        for expected in [
            "Intent",
            "Thread",
            "Scheduler",
            "QueryIndex",
            "LiveContextWindow",
            "ReadyQueue",
            "ParallelGroup",
            "Checkpoint",
            "RetryRoute",
            "UiCurrentView",
        ] {
            assert!(
                object_types.contains(expected),
                "default planner should export {expected}, got {object_types:?}"
            );
        }
        assert_eq!(
            sink.ai_objects
                .iter()
                .filter(|object| object.object_type == "Thread")
                .count(),
            2,
            "default planner must rebuild every independent thread component",
        );
        assert_eq!(output.ai_object_count, 20);
        assert_eq!(sink.ai_objects.len(), 20);
        assert_eq!(sink.revisions[0].ai_object_count, 20);
        assert_eq!(sink.sync_runs[0].ai_object_count, 20);
        assert_eq!(sink.sync_runs[1].ai_object_count, 20);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn publish_sync_non_dry_run_fails_when_ai_projection_cannot_rebuild() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        test::setup_with_new_libra_in(temp.path()).await;
        let _guard = test::ChangeDirGuard::new(temp.path());

        let libra_dir = temp.path().join(".libra");
        let storage = Arc::new(LocalStorage::new(libra_dir.join("objects")));
        let db_conn = Arc::new(
            db::get_db_conn_instance_for_path(&libra_dir.join(util::DATABASE))
                .await
                .expect("db should open"),
        );
        let history = HistoryManager::new(storage.clone(), libra_dir, db_conn);
        let usage_only = serde_json::json!({
            "runId": "00000000-0000-0000-0000-000000000001",
            "promptTokens": 10,
            "completionTokens": 5
        });
        let usage_hash = storage
            .put_json(&usage_only)
            .await
            .expect("usage fixture should store");
        history
            .append("run_usage", "usage-only", usage_hash)
            .await
            .expect("usage fixture should be tracked");

        let commit = commit_with_single_file("README.md", "ai\n", "ai fixture");
        let revision_oid = commit.id.to_string();
        let site = publish_sync_site_context();
        let args = sync_args(false);
        let mut sink = FakePublishSyncSink::default();

        let err = run_publish_sync_selected_refs_with_sink(
            &args,
            &site,
            vec![publish_local_ref(
                "refs/heads/main",
                &revision_oid,
                &revision_oid,
            )],
            Some("refs/heads/main".to_string()),
            Vec::new(),
            &mut sink,
        )
        .await
        .expect_err("projection-less AI history must fail");

        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
        assert!(
            err.message().contains("missing projection object types"),
            "{}",
            err.message()
        );
        assert!(err.message().contains("Thread"), "{}", err.message());
        assert!(
            err.message()
                .contains("no rebuildable Intent, Task, or Run history"),
            "{}",
            err.message()
        );
        // Cross-Cutting G: every internal-invariant raise site in
        // `publish.rs` must surface the GitHub Issues URL hint via
        // `publish_internal_error`.
        assert!(
            err.hints().iter().any(|h| h.as_str().contains("issues")),
            "publish AI projection-rebuild internal error must include the Issues URL hint, got hints: {:?}",
            err.hints()
        );
    }

    /// Cross-Cutting G unit test: the `publish_internal_error` helper
    /// produces a fatal `CliError` whose stable code is
    /// `InternalInvariant` and whose user-visible hint list contains
    /// the GitHub Issues URL. This pins the contract for the inline
    /// raise sites that route through this helper.
    #[test]
    fn publish_internal_error_helper_has_issue_url_hint() {
        let err = publish_internal_error("synthetic bug case");
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
        assert!(
            err.message().contains("synthetic bug case"),
            "message should be passed through verbatim, got: {}",
            err.message()
        );
        assert!(
            err.hints().iter().any(|h| h.as_str().contains("issues")),
            "publish_internal_error must include the Issues URL hint, got hints: {:?}",
            err.hints()
        );
    }

    /// Codex pass-10 P1: pin the `--max-preview-bytes` parser
    /// behaviour. The CLI surface must reject 0 (zero cap publishes
    /// no previews — pure misuse) and non-numeric input, and accept
    /// any positive `u64`.
    #[test]
    fn max_preview_bytes_rejects_zero() {
        let err = parse_max_preview_bytes("0").expect_err("zero must be rejected");
        assert!(
            err.contains("must be > 0"),
            "error must mention the positive-only constraint: {err}",
        );
        assert!(
            err.contains("'0'"),
            "error must include the offending input: {err}",
        );
    }

    #[test]
    fn max_preview_bytes_rejects_non_numeric() {
        let err = parse_max_preview_bytes("abc").expect_err("non-numeric must be rejected");
        assert!(
            err.contains("'abc'"),
            "error must include the offending input: {err}",
        );
    }

    #[test]
    fn max_preview_bytes_accepts_positive() {
        assert_eq!(parse_max_preview_bytes("1").unwrap(), 1);
        assert_eq!(
            parse_max_preview_bytes("1048576").unwrap(),
            1024 * 1024,
            "1 MiB byte count must round-trip",
        );
        assert_eq!(
            parse_max_preview_bytes("18446744073709551615").unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn max_preview_bytes_rejects_negative() {
        // u64 cannot represent negatives so parse fails as
        // "not a valid byte count" — pin the message shape.
        let err = parse_max_preview_bytes("-1").expect_err("negative must be rejected");
        assert!(
            err.contains("not a valid byte count"),
            "negative input must hit the type-parse error: {err}",
        );
    }

    /// Codex pass-11 P1: prove `--max-preview-bytes` is wired
    /// through clap end-to-end, not just through the standalone
    /// parser fn. `try_parse_from` exercises the actual derive macro
    /// output, so a future regression that drops the
    /// `value_parser = ...` attribute is caught.
    #[test]
    fn clap_init_max_preview_bytes_rejects_zero() {
        use clap::Parser;
        let err = PublishArgs::try_parse_from([
            "publish",
            "init",
            "--slug",
            "demo",
            "--clone-domain",
            "code.example.com",
            "--max-preview-bytes",
            "0",
        ])
        .expect_err("clap must reject --max-preview-bytes=0");
        let rendered = err.to_string();
        assert!(
            rendered.contains("must be > 0"),
            "clap error must surface the positive-only constraint: {rendered}",
        );
    }

    #[test]
    fn clap_init_max_preview_bytes_accepts_positive() {
        use clap::Parser;
        let parsed = PublishArgs::try_parse_from([
            "publish",
            "init",
            "--slug",
            "demo",
            "--clone-domain",
            "code.example.com",
            "--max-preview-bytes",
            "1048576",
        ])
        .expect("clap must accept a positive --max-preview-bytes");
        match parsed.command {
            PublishCommand::Init(args) => {
                assert_eq!(args.max_preview_bytes, Some(1024 * 1024));
            }
            _ => panic!("expected `init` subcommand"),
        }
    }

    #[test]
    fn clap_init_max_preview_bytes_rejects_non_numeric() {
        use clap::Parser;
        let err = PublishArgs::try_parse_from([
            "publish",
            "init",
            "--slug",
            "demo",
            "--clone-domain",
            "code.example.com",
            "--max-preview-bytes",
            "abc",
        ])
        .expect_err("clap must reject non-numeric --max-preview-bytes");
        let rendered = err.to_string();
        assert!(
            rendered.contains("not a valid byte count"),
            "clap error must surface the parse failure: {rendered}",
        );
    }

    #[test]
    fn clap_sync_accepts_force_and_allow_sensitive_path() {
        use clap::Parser;
        let parsed = PublishArgs::try_parse_from([
            "publish",
            "sync",
            "--ref",
            "main",
            "--force",
            "--allow-sensitive-path",
            ".env.local",
            "--allow-sensitive-path",
            "config/api-secret.json",
        ])
        .expect("clap must accept the documented sync flag set");
        match parsed.command {
            PublishCommand::Sync(args) => {
                assert!(args.force);
                assert_eq!(args.r#ref.as_deref(), Some("main"));
                assert_eq!(
                    args.allow_sensitive_path,
                    vec![
                        ".env.local".to_string(),
                        "config/api-secret.json".to_string()
                    ],
                );
            }
            _ => panic!("expected `sync` subcommand"),
        }
    }

    #[test]
    fn clap_status_accepts_site_id() {
        use clap::Parser;
        let parsed = PublishArgs::try_parse_from([
            "publish",
            "status",
            "--site-id",
            "00000000-0000-0000-0000-000000000001",
        ])
        .expect("clap must accept the status cloud comparison site id");
        match parsed.command {
            PublishCommand::Status(args) => {
                assert_eq!(
                    args.site_id.as_deref(),
                    Some("00000000-0000-0000-0000-000000000001")
                );
            }
            _ => panic!("expected `status` subcommand"),
        }
    }

    #[test]
    fn publish_init_materializes_worker_template_and_manifest() {
        let temp = tempfile::tempdir().expect("temp dir must be created");

        let output =
            run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
                .expect("publish init must materialize the embedded worker template");

        assert!(output.files_written > 0);
        assert_eq!(output.files_current, 0);

        let package_json = temp.path().join("worker/package.json");
        let expected_package_json = WorkerTemplate::get("package.json")
            .expect("embedded package.json must exist")
            .data
            .into_owned();
        assert_eq!(
            fs::read(&package_json).expect("materialized package.json must be readable"),
            expected_package_json
        );

        let wrangler = fs::read_to_string(temp.path().join("worker/wrangler.jsonc"))
            .expect("materialized wrangler config must be readable");
        assert!(
            wrangler.contains("REPLACE_WITH_D1_DATABASE_ID"),
            "publish init must leave an explicit D1 placeholder: {wrangler}"
        );
        assert!(
            wrangler.contains("REPLACE_WITH_R2_BUCKET_NAME"),
            "publish init must leave an explicit R2 placeholder: {wrangler}"
        );
        assert!(
            !wrangler.contains("8e067bd6-f12c-4462-a536-65f8acde59ce")
                && !wrangler.contains("libra-action"),
            "publish init must not materialize repo-specific Cloudflare resource ids: {wrangler}"
        );

        let manifest_path = temp.path().join(WORKER_TEMPLATE_MANIFEST_PATH);
        let manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("manifest must be readable"))
                .expect("manifest must be valid JSON");
        assert_eq!(
            manifest["schemaVersion"],
            WORKER_TEMPLATE_MANIFEST_SCHEMA_VERSION
        );
        assert_eq!(manifest["templateVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["workerDir"], "worker");

        let files = manifest["files"]
            .as_array()
            .expect("manifest files must be an array");
        assert!(
            files.iter().any(|file| {
                file["path"] == "package.json"
                    && file["renderPolicy"] == "managed_template"
                    && file["sha256"].as_str().is_some_and(|hash| hash.len() == 64)
            }),
            "manifest must record package.json with its template hash"
        );

        let rerun =
            run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
                .expect("publish init must be idempotent for byte-identical files");
        assert_eq!(rerun.files_written, 0);
        assert_eq!(rerun.files_current, output.files_written);
    }

    #[test]
    fn publish_status_reports_missing_before_init() {
        let temp = tempfile::tempdir().expect("temp dir must be created");

        let output = run_publish_status_at_root(temp.path(), &manifest_at(temp.path()))
            .expect("status should inspect missing template");

        assert_eq!(output.status, WorkerTemplateStatus::Missing);
        assert_eq!(output.files_current, 0);
        assert!(output.files_missing > 0);
        assert_eq!(
            output.published_refs.state,
            PublishRefComparisonState::Unconfigured
        );
    }

    #[test]
    fn publish_status_reports_current_after_init() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
            .expect("publish init must materialize the template");

        let output = run_publish_status_at_root(temp.path(), &manifest_at(temp.path()))
            .expect("status should inspect template");

        assert_eq!(output.status, WorkerTemplateStatus::Current);
        assert_eq!(output.files_missing, 0);
        assert_eq!(output.files_modified, 0);
        assert_eq!(output.files_outdated, 0);
        assert_eq!(output.files_conflicted, 0);
        assert_eq!(output.files_current, output.files_total);
    }

    #[test]
    fn publish_status_ref_comparison_reports_d1_drift() {
        let local_refs = vec![
            publish_local_ref(
                "refs/heads/main",
                "1111111111111111111111111111111111111111",
                "1111111111111111111111111111111111111111",
            ),
            publish_local_ref(
                "refs/heads/dev",
                "2222222222222222222222222222222222222222",
                "2222222222222222222222222222222222222222",
            ),
            publish_local_ref(
                "refs/tags/v1.0.0",
                "3333333333333333333333333333333333333333",
                "4444444444444444444444444444444444444444",
            ),
        ];
        let published_refs = vec![
            publish_ref_row(
                "refs/heads/main",
                "1111111111111111111111111111111111111111",
                "1111111111111111111111111111111111111111",
            ),
            publish_ref_row(
                "refs/tags/v1.0.0",
                "3333333333333333333333333333333333333333",
                "5555555555555555555555555555555555555555",
            ),
            publish_ref_row(
                "refs/tags/remote-only",
                "6666666666666666666666666666666666666666",
                "6666666666666666666666666666666666666666",
            ),
        ];

        let comparison = compare_publish_refs(
            "00000000-0000-0000-0000-000000000001",
            &local_refs,
            &published_refs,
            &publish_revision_map_for_refs(&published_refs),
        );

        assert_eq!(comparison.state, PublishRefComparisonState::Compared);
        assert_eq!(
            comparison.site_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(comparison.local_count, 3);
        assert_eq!(comparison.published_count, 3);
        assert_eq!(comparison.matching_count, 1);
        assert_eq!(comparison.local_only_count, 1);
        assert_eq!(comparison.local_only[0].ref_name, "refs/heads/dev");
        assert_eq!(comparison.changed_count, 1);
        assert_eq!(comparison.changed[0].ref_name, "refs/tags/v1.0.0");
        assert_eq!(
            comparison.changed[0].published_revision_oid,
            "5555555555555555555555555555555555555555"
        );
        assert_eq!(comparison.published_only_count, 1);
        assert_eq!(
            comparison.published_only[0].ref_name,
            "refs/tags/remote-only"
        );
        assert_eq!(comparison.snapshot_issue_count, 0);
    }

    #[test]
    fn publish_status_ref_comparison_reports_snapshot_issues() {
        let published_refs = vec![
            publish_ref_row(
                "refs/heads/main",
                "1111111111111111111111111111111111111111",
                "1111111111111111111111111111111111111111",
            ),
            publish_ref_row(
                "refs/heads/syncing",
                "2222222222222222222222222222222222222222",
                "2222222222222222222222222222222222222222",
            ),
        ];
        let revisions = publish_revision_map([(
            "2222222222222222222222222222222222222222",
            Some(publish_revision_row(
                "2222222222222222222222222222222222222222",
                "syncing",
            )),
        )]);

        let comparison = compare_publish_refs(
            "00000000-0000-0000-0000-000000000001",
            &[],
            &published_refs,
            &revisions,
        );

        assert_eq!(comparison.snapshot_issue_count, 2);
        assert_eq!(comparison.snapshot_missing_count, 1);
        assert_eq!(comparison.snapshot_unpublished_count, 1);
        assert_eq!(
            comparison.snapshot_issues[0].state,
            PublishSnapshotIssueState::Missing
        );
        assert_eq!(comparison.snapshot_issues[0].ref_name, "refs/heads/main");
        assert_eq!(
            comparison.snapshot_issues[1].state,
            PublishSnapshotIssueState::Unpublished
        );
        assert_eq!(
            comparison.snapshot_issues[1].revision_status.as_deref(),
            Some("syncing")
        );
    }

    #[tokio::test]
    async fn publish_status_command_outputs_compared_ref_json() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        let args = StatusArgs {
            site_id: Some("00000000-0000-0000-0000-000000000001".to_string()),
        };

        let output = run_publish_status_command_at_root_with_loaders(
            temp.path(),
            &manifest_at(temp.path()),
            &args,
            || async {
                Ok::<Vec<RefInput>, CliError>(vec![
                    publish_local_ref(
                        "refs/heads/main",
                        "1111111111111111111111111111111111111111",
                        "1111111111111111111111111111111111111111",
                    ),
                    publish_local_ref(
                        "refs/heads/dev",
                        "2222222222222222222222222222222222222222",
                        "2222222222222222222222222222222222222222",
                    ),
                ])
            },
            |site_id| async move {
                assert_eq!(site_id, "00000000-0000-0000-0000-000000000001");
                let refs = vec![
                    publish_ref_row(
                        "refs/heads/main",
                        "9999999999999999999999999999999999999999",
                        "9999999999999999999999999999999999999999",
                    ),
                    publish_ref_row(
                        "refs/tags/remote-only",
                        "3333333333333333333333333333333333333333",
                        "3333333333333333333333333333333333333333",
                    ),
                ];
                let revisions = publish_revision_map([(
                    "9999999999999999999999999999999999999999",
                    Some(publish_revision_row(
                        "9999999999999999999999999999999999999999",
                        "published",
                    )),
                )]);
                Ok::<PublishCloudStatusRows, CliError>(PublishCloudStatusRows { refs, revisions })
            },
        )
        .await
        .expect("status should compare injected D1 refs");

        let json = serde_json::to_value(&output).expect("status output must serialize");
        assert_eq!(json["publishedRefs"]["state"], "compared");
        assert_eq!(json["publishedRefs"]["siteId"], args.site_id.unwrap());
        assert_eq!(json["publishedRefs"]["localCount"], 2);
        assert_eq!(json["publishedRefs"]["publishedCount"], 2);
        assert_eq!(json["publishedRefs"]["changedCount"], 1);
        assert_eq!(
            json["publishedRefs"]["changed"][0]["refName"],
            "refs/heads/main"
        );
        assert_eq!(json["publishedRefs"]["localOnlyCount"], 1);
        assert_eq!(
            json["publishedRefs"]["localOnly"][0]["refName"],
            "refs/heads/dev"
        );
        assert_eq!(json["publishedRefs"]["publishedOnlyCount"], 1);
        assert_eq!(
            json["publishedRefs"]["publishedOnly"][0]["refName"],
            "refs/tags/remote-only"
        );
        assert_eq!(json["publishedRefs"]["snapshotIssueCount"], 1);
        assert_eq!(json["publishedRefs"]["snapshotMissingCount"], 1);
        assert_eq!(
            json["publishedRefs"]["snapshotIssues"][0]["refName"],
            "refs/tags/remote-only"
        );
    }

    #[tokio::test]
    async fn publish_status_command_fails_when_d1_comparison_is_unavailable() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        let args = StatusArgs {
            site_id: Some("00000000-0000-0000-0000-000000000001".to_string()),
        };

        let err = run_publish_status_command_at_root_with_loaders(
            temp.path(),
            &manifest_at(temp.path()),
            &args,
            || async { Ok::<Vec<RefInput>, CliError>(Vec::new()) },
            |site_id| async move {
                assert_eq!(site_id, "00000000-0000-0000-0000-000000000001");
                Err::<PublishCloudStatusRows, CliError>(publish_status_d1_error(
                    "failed to initialize D1 client for publish status ref comparison",
                    D1Error {
                        code: 1001,
                        message: "LIBRA_D1_ACCOUNT_ID is not configured".to_string(),
                    },
                ))
            },
        )
        .await
        .expect_err("configured cloud comparison must fail instead of returning stale state");

        assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
        assert!(
            err.message().contains("LIBRA_D1_ACCOUNT_ID"),
            "error should explain which D1 credential is missing: {}",
            err.message()
        );
    }

    #[test]
    fn publish_deploy_skip_deploy_builds_worker_and_skips_cloud_mutations() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        materialize_deployable_worker(temp.path());
        let args = DeployArgs { skip_deploy: true };
        let mut runner =
            FakePublishWorkerCommandRunner::with_outputs([successful_deploy_command_output()]);

        let output =
            run_publish_deploy_at_root(temp.path(), &manifest_at(temp.path()), &args, &mut runner)
                .expect("deploy --skip-deploy should build and skip cloud mutations");

        assert_eq!(runner.calls, vec![command_summary("pnpm", &["build"])]);
        assert_eq!(output.deploy_url, None);
        assert_eq!(output.steps.len(), 3);
        assert_eq!(output.steps[0].state, PublishDeployStepState::Completed);
        assert_eq!(output.steps[1].state, PublishDeployStepState::Skipped);
        assert_eq!(output.steps[2].state, PublishDeployStepState::Skipped);
        assert_eq!(
            output.steps[1].command,
            command_summary(
                "pnpm",
                &[
                    "exec",
                    "wrangler",
                    "d1",
                    "migrations",
                    "apply",
                    "LIBRA_PUBLISH_DB",
                    "--remote",
                ],
            )
        );
    }

    #[test]
    fn publish_deploy_applies_migrations_deploys_and_extracts_url() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        materialize_deployable_worker(temp.path());
        let args = DeployArgs { skip_deploy: false };
        let mut runner = FakePublishWorkerCommandRunner::with_outputs([
            successful_deploy_command_output(),
            successful_deploy_command_output(),
            successful_deploy_command_output_with_stdout(
                "Uploaded libra-publish\nPublished at https://libra-publish.example.workers.dev.",
            ),
        ]);

        let output =
            run_publish_deploy_at_root(temp.path(), &manifest_at(temp.path()), &args, &mut runner)
                .expect("deploy should run build, migrations, and Worker deploy");

        assert_eq!(
            runner.calls,
            vec![
                command_summary("pnpm", &["build"]),
                command_summary(
                    "pnpm",
                    &[
                        "exec",
                        "wrangler",
                        "d1",
                        "migrations",
                        "apply",
                        "LIBRA_PUBLISH_DB",
                        "--remote",
                    ],
                ),
                command_summary("pnpm", &["exec", "opennextjs-cloudflare", "deploy"]),
            ],
        );
        assert_eq!(
            output.deploy_url.as_deref(),
            Some("https://libra-publish.example.workers.dev")
        );
        assert!(
            output
                .steps
                .iter()
                .all(|step| step.state == PublishDeployStepState::Completed)
        );
    }

    #[test]
    fn publish_deploy_requires_configured_d1_database_id_before_commands() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
            .expect("publish init must materialize the template");
        let wrangler_path = temp.path().join("worker/wrangler.jsonc");
        let wrangler =
            fs::read_to_string(&wrangler_path).expect("materialized wrangler config is readable");
        fs::write(
            &wrangler_path,
            replace_d1_database_id_for_test(&wrangler, "REPLACE_WITH_D1_DATABASE_ID"),
        )
        .expect("wrangler config should be writable for placeholder validation test");
        let args = DeployArgs { skip_deploy: true };
        let mut runner = FakePublishWorkerCommandRunner::default();

        let err =
            run_publish_deploy_at_root(temp.path(), &manifest_at(temp.path()), &args, &mut runner)
                .expect_err("deploy must fail before running commands with placeholder D1 config");

        assert_eq!(err.stable_code(), StableErrorCode::RepoStateInvalid);
        assert!(
            err.message().contains("REPLACE_WITH_D1_DATABASE_ID"),
            "error must identify the placeholder config: {}",
            err.message()
        );
        assert!(
            runner.calls.is_empty(),
            "deploy must not run build or cloud commands before config validation"
        );
    }

    #[test]
    fn publish_deploy_requires_configured_r2_bucket_name_before_commands() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
            .expect("publish init must materialize the template");
        let wrangler_path = temp.path().join("worker/wrangler.jsonc");
        let wrangler =
            fs::read_to_string(&wrangler_path).expect("materialized wrangler config is readable");
        let wrangler =
            replace_d1_database_id_for_test(&wrangler, "00000000-0000-0000-0000-000000000000");
        fs::write(&wrangler_path, wrangler)
            .expect("wrangler config should be writable for R2 placeholder validation test");
        let args = DeployArgs { skip_deploy: true };
        let mut runner = FakePublishWorkerCommandRunner::default();

        let err =
            run_publish_deploy_at_root(temp.path(), &manifest_at(temp.path()), &args, &mut runner)
                .expect_err("deploy must fail before running commands with placeholder R2 config");

        assert_eq!(err.stable_code(), StableErrorCode::RepoStateInvalid);
        assert!(
            err.message().contains("REPLACE_WITH_R2_BUCKET_NAME"),
            "error must identify the R2 placeholder config: {}",
            err.message()
        );
        assert!(
            runner.calls.is_empty(),
            "deploy must not run build or cloud commands before config validation"
        );
    }

    #[test]
    fn publish_unpublish_requires_yes() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        materialize_deployable_worker(temp.path());
        let args = UnpublishArgs {
            yes: false,
            site_id: Some("00000000-0000-0000-0000-000000000001".to_string()),
        };
        let mut runner = FakePublishWorkerCommandRunner::default();

        let err = run_publish_unpublish_at_root(
            temp.path(),
            &manifest_at(temp.path()),
            &args,
            "00000000-0000-0000-0000-000000000001",
            &mut runner,
        )
        .expect_err("unpublish must require explicit confirmation");

        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
        assert!(
            runner.calls.is_empty(),
            "unpublish must not run D1 mutation before --yes confirmation"
        );
    }

    #[test]
    fn publish_unpublish_marks_site_disabled_with_wrangler_d1_execute() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        materialize_deployable_worker(temp.path());
        let args = UnpublishArgs {
            yes: true,
            site_id: Some("00000000-0000-0000-0000-000000000001".to_string()),
        };
        let mut runner =
            FakePublishWorkerCommandRunner::with_outputs([successful_deploy_command_output()]);

        let output = run_publish_unpublish_at_root(
            temp.path(),
            &manifest_at(temp.path()),
            &args,
            "00000000-0000-0000-0000-000000000001",
            &mut runner,
        )
        .expect("unpublish should execute a D1 update through Wrangler");

        assert_eq!(output.status, "disabled");
        assert_eq!(output.site_id, "00000000-0000-0000-0000-000000000001");
        assert_eq!(runner.calls.len(), 1);
        assert_eq!(
            &runner.calls[0][..7],
            &[
                "pnpm".to_string(),
                "exec".to_string(),
                "wrangler".to_string(),
                "d1".to_string(),
                "execute".to_string(),
                "LIBRA_PUBLISH_DB".to_string(),
                "--remote".to_string(),
            ]
        );
        assert!(
            runner.calls[0]
                .iter()
                .any(|arg| arg.contains("status = 'disabled'")
                    && arg.contains("00000000-0000-0000-0000-000000000001")),
            "D1 command must disable the selected site: {:?}",
            runner.calls[0],
        );
    }

    #[test]
    fn publish_site_id_validation_rejects_non_uuid() {
        let err =
            validate_publish_site_id("not-a-uuid").expect_err("site id must be a parseable UUID");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
        assert!(
            err.message().contains("not-a-uuid"),
            "error must echo the bad site id: {}",
            err.message()
        );
    }

    fn publish_local_ref(ref_name: &str, target_oid: &str, revision_oid: &str) -> RefInput {
        RefInput {
            ref_name: ref_name.to_string(),
            target_oid: target_oid.to_string(),
            revision_oid: revision_oid.to_string(),
        }
    }

    fn publish_ref_row(ref_name: &str, target_oid: &str, revision_oid: &str) -> PublishRefRow {
        let short_name = publish_short_ref_name(ref_name)
            .expect("test ref names must be publishable")
            .to_string();
        PublishRefRow {
            site_id: "00000000-0000-0000-0000-000000000001".to_string(),
            ref_name: ref_name.to_string(),
            ref_type: if ref_name.starts_with("refs/heads/") {
                "branch".to_string()
            } else {
                "tag".to_string()
            },
            short_name,
            target_oid: target_oid.to_string(),
            revision_oid: revision_oid.to_string(),
            is_default: 0,
            sync_run_id: "sync-1".to_string(),
            schema_version: 1,
            updated_at: "2026-05-13T00:00:00Z".to_string(),
        }
    }

    fn publish_ai_object(
        site_id: &str,
        revision_oid: &str,
        redaction_mode: RedactionMode,
        redaction_rules_version: &str,
    ) -> PublishAiObject {
        PublishAiObject {
            schema_version: PUBLISH_SCHEMA_VERSION,
            site_id: site_id.to_string(),
            revision_oid: revision_oid.to_string(),
            object_type: "Intent".to_string(),
            object_id: "intent-1".to_string(),
            layer: AiObjectLayer::Snapshot,
            source_refs: vec!["refs/heads/main".to_string()],
            relationships: Vec::new(),
            payload: serde_json::json!({ "title": "ship AI publish" }),
            redaction: AiObjectRedaction {
                mode: redaction_mode,
                rules_version: redaction_rules_version.to_string(),
            },
            removed_fields: Vec::new(),
        }
    }

    fn publish_revision_map_for_refs(
        refs: &[PublishRefRow],
    ) -> BTreeMap<String, Option<PublishRevisionRow>> {
        refs.iter()
            .map(|publish_ref| {
                (
                    publish_ref.revision_oid.clone(),
                    Some(publish_revision_row(&publish_ref.revision_oid, "published")),
                )
            })
            .collect()
    }

    fn publish_revision_map<const N: usize>(
        entries: [(&str, Option<PublishRevisionRow>); N],
    ) -> BTreeMap<String, Option<PublishRevisionRow>> {
        entries
            .into_iter()
            .map(|(revision_oid, row)| (revision_oid.to_string(), row))
            .collect()
    }

    fn publish_revision_row(revision_oid: &str, status: &str) -> PublishRevisionRow {
        PublishRevisionRow {
            site_id: "00000000-0000-0000-0000-000000000001".to_string(),
            revision_oid: revision_oid.to_string(),
            status: status.to_string(),
            code_manifest_key: Some(format!("repo/publish/revisions/{revision_oid}/code.json")),
            ai_index_key: None,
            file_count: 1,
            ai_object_count: 0,
            ai_bundle_count: 0,
            redaction_mode: "default".to_string(),
            redaction_rules_version: "2026-05-13".to_string(),
            sync_run_id: "sync-1".to_string(),
            schema_version: 1,
            created_at: "2026-05-13T00:00:00Z".to_string(),
            updated_at: "2026-05-13T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn publish_status_reports_modified_template_file() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
            .expect("publish init must materialize the template");
        fs::write(
            temp.path().join("worker/package.json"),
            b"{\"custom\":true}\n",
        )
        .expect("custom package.json must be writable");

        let output = run_publish_status_at_root(temp.path(), &manifest_at(temp.path()))
            .expect("status should inspect template");

        assert_eq!(output.status, WorkerTemplateStatus::Modified);
        assert_eq!(output.files_modified, 1);
    }

    #[test]
    fn publish_status_reports_outdated_template_file() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
            .expect("publish init must materialize the template");
        let old_package = b"{\"old\":true}\n";
        fs::write(temp.path().join("worker/package.json"), old_package)
            .expect("old package.json must be writable");

        let manifest_path = temp.path().join(WORKER_TEMPLATE_MANIFEST_PATH);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("manifest must be readable"))
                .expect("manifest must be valid JSON");
        let old_sha = hex::encode(digest(&SHA256, old_package).as_ref());
        let files = manifest["files"]
            .as_array_mut()
            .expect("manifest files must be an array");
        let package = files
            .iter_mut()
            .find(|file| file["path"] == "package.json")
            .expect("manifest must contain package.json");
        package["sha256"] = Value::String(old_sha);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("manifest must serialize"),
        )
        .expect("manifest must be writable");

        let output = run_publish_status_at_root(temp.path(), &manifest_at(temp.path()))
            .expect("status should inspect template");

        assert_eq!(output.status, WorkerTemplateStatus::Outdated);
        assert_eq!(output.files_outdated, 1);
    }

    #[test]
    fn publish_init_refuses_to_overwrite_modified_template_file() {
        let temp = tempfile::tempdir().expect("temp dir must be created");
        let worker_dir = temp.path().join("worker");
        fs::create_dir_all(&worker_dir).expect("worker dir must be created");
        fs::write(worker_dir.join("package.json"), b"{\"custom\":true}\n")
            .expect("custom package.json must be writable");

        let err =
            run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
                .expect_err("publish init must fail closed on modified template files");

        assert_eq!(err.stable_code(), StableErrorCode::ConflictOperationBlocked);
        assert!(
            err.message().contains("package.json"),
            "conflict error must identify the changed file: {}",
            err.message()
        );
        assert_eq!(
            fs::read_to_string(worker_dir.join("package.json"))
                .expect("custom package.json must remain readable"),
            "{\"custom\":true}\n"
        );
        assert!(
            !temp.path().join(WORKER_TEMPLATE_MANIFEST_PATH).exists(),
            "manifest must not be written after a template conflict"
        );
    }

    #[cfg(unix)]
    #[test]
    fn publish_init_refuses_worker_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir must be created");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).expect("outside dir must be created");
        symlink(&outside, temp.path().join("worker")).expect("worker symlink must be created");

        let err =
            run_publish_init_at_root(temp.path(), &manifest_at(temp.path()), &default_init_args())
                .expect_err("publish init must refuse symlinked worker roots");

        assert_eq!(err.stable_code(), StableErrorCode::ConflictOperationBlocked);
        assert!(
            err.message().contains("worker"),
            "conflict error must identify the symlinked worker root: {}",
            err.message()
        );
        assert!(
            !outside.join("package.json").exists(),
            "publish init must not write template files through a worker symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn publish_status_reports_worker_symlink_conflict() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir must be created");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).expect("outside dir must be created");
        symlink(&outside, temp.path().join("worker")).expect("worker symlink must be created");

        let output = run_publish_status_at_root(temp.path(), &manifest_at(temp.path()))
            .expect("status should inspect symlink");

        assert_eq!(output.status, WorkerTemplateStatus::Conflicted);
        assert!(output.files_conflicted > 0);
    }
}
