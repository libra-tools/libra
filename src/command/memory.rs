//! Read and repair repository-scoped Agent development-history Memory.
//!
//! This command module is an adapter over the Memory reader and diagnostics
//! interfaces. It never reads or writes Memory refs, objects, or projection
//! tables directly.

use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use git_internal::hash::ObjectHash;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    info_println,
    internal::{
        ai::{
            history::HistoryManager,
            keyed_digest::RepositoryKeyedDigest,
            memory::{
                AuthenticatedMemoryContext, CodeApplicability, CodeChangeStatus, CompletionStatus,
                EpisodePathFilter, EpisodeQueryV1, EpisodeReadItemV1, EpisodeReader,
                EpisodeReaderError, EpisodeReaderErrorKind, EpisodeRootKind,
                EvidenceOmissionReason, MAX_RESULT_LIMIT, MemoryDiagnostics, MemoryNoteV1,
                MemoryRebuildReport, MemoryStatusReport, MemoryWriterError, MemoryWriterErrorKind,
                validate_plain_text_query,
            },
        },
        db,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        storage::local::LocalStorage,
        util::{DATABASE, try_get_storage_path},
    },
};

pub const MEMORY_EXAMPLES: &str = "\
EXAMPLES:
    libra memory search \"authentication retry\"                 Search current, applicable Episodes
    libra memory search \"timeout\" --task task-42 --limit 5     Filter by related Task
    libra memory search \"parser\" --path-prefix episodic.tasks  Filter by Memory path prefix
    libra --json memory search \"root cause\"                    Structured output for agents
    libra memory show <note-id>                                  Show one current Episode revision
    libra memory show <note-id> --revision <oid> --evidence       Inspect a historical revision and evidence
    libra memory status                                          Inspect ref, projection, jobs, and FTS5
    libra memory rebuild --dry-run                               Validate replay without changing SQLite
    libra memory rebuild                                         Rebuild the repository Memory projection";

#[derive(Parser, Debug)]
#[command(after_help = MEMORY_EXAMPLES)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemorySubcommand,
}

#[derive(Subcommand, Debug)]
pub enum MemorySubcommand {
    /// Search repository development-history Episodes.
    Search(Box<MemorySearchArgs>),
    /// Show one current or historical Episode revision.
    Show {
        /// Stable Episode note UUID.
        #[arg(value_name = "NOTE_ID")]
        note_id: String,
        /// Historical revision object ID; defaults to the current confirmed revision.
        #[arg(long, value_name = "OID")]
        revision: Option<String>,
        /// Resolve and show authorized evidence fragments and omissions.
        #[arg(long)]
        evidence: bool,
    },
    /// Show repository Memory ref, projection, job, and FTS5 diagnostics.
    Status,
    /// Validate or rebuild the repository-scoped SQLite projection.
    Rebuild {
        /// Validate and report the replay plan without changing SQLite.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args, Debug)]
pub struct MemorySearchArgs {
    /// FTS5 query text.
    #[arg(value_name = "QUERY")]
    query: String,
    /// Maximum returned Episodes (1..=50).
    #[arg(long, default_value = "10", value_name = "N")]
    limit: String,
    /// Filter by root kind; requires --root-id.
    #[arg(long, value_name = "task|intent")]
    root_kind: Option<String>,
    /// Filter by root ID; requires --root-kind.
    #[arg(long)]
    root_id: Option<String>,
    /// Filter by a related Intent ID.
    #[arg(long, value_name = "INTENT_ID")]
    intent: Option<String>,
    /// Filter by a related Task ID.
    #[arg(long, value_name = "TASK_ID")]
    task: Option<String>,
    /// Include Episodes ending at or after this RFC3339 timestamp.
    #[arg(long, value_name = "RFC3339")]
    ended_from: Option<String>,
    /// Include Episodes ending at or before this RFC3339 timestamp.
    #[arg(long, value_name = "RFC3339")]
    ended_until: Option<String>,
    /// Filter by task/intent completion status.
    #[arg(long, value_name = "completed|failed|cancelled")]
    completion: Option<String>,
    /// Filter by whether the Episode changed code.
    #[arg(long, value_name = "changed|unchanged|unknown")]
    code_change: Option<String>,
    /// Match one exact Memory taxonomy path.
    #[arg(long)]
    path: Option<String>,
    /// Match a Memory taxonomy path prefix.
    #[arg(long)]
    path_prefix: Option<String>,
    /// Include non-injectable applicability states for diagnosis.
    #[arg(long)]
    include_diagnostics: bool,
}

impl MemoryArgs {
    pub(crate) const fn mutates_repository(&self) -> bool {
        matches!(self.command, MemorySubcommand::Rebuild { dry_run: false })
    }
}

/// Execute a Memory read or projection-recovery command.
///
/// # Side Effects
///
/// `search`, `show`, `status`, and `rebuild --dry-run` are read-only. A plain
/// `rebuild` replaces only the repository-scoped rebuildable projection and
/// never changes the authoritative Memory ref or objects.
pub async fn execute_safe(args: MemoryArgs, output: &OutputConfig) -> CliResult<()> {
    let repository = MemoryCommandRepository::open(args.mutates_repository()).await?;
    match args.command {
        MemorySubcommand::Search(search) => {
            let MemorySearchArgs {
                query,
                limit,
                root_kind,
                root_id,
                intent,
                task,
                ended_from,
                ended_until,
                completion,
                code_change,
                path,
                path_prefix,
                include_diagnostics,
            } = *search;
            let limit = parse_limit(&limit).map_err(invalid_filter_error)?;
            let root_kind = root_kind
                .as_deref()
                .map(parse_root_kind)
                .transpose()
                .map_err(invalid_filter_error)?;
            let ended_from = ended_from
                .as_deref()
                .map(parse_rfc3339)
                .transpose()
                .map_err(invalid_filter_error)?;
            let ended_until = ended_until
                .as_deref()
                .map(parse_rfc3339)
                .transpose()
                .map_err(invalid_filter_error)?;
            let completion = completion
                .as_deref()
                .map(parse_completion)
                .transpose()
                .map_err(invalid_filter_error)?;
            let code_change = code_change
                .as_deref()
                .map(parse_code_change)
                .transpose()
                .map_err(invalid_filter_error)?;
            let path = match (path, path_prefix) {
                (Some(_), Some(_)) => {
                    return Err(invalid_filter_error(
                        "--path and --path-prefix cannot be used together".to_string(),
                    ));
                }
                (Some(path), None) => Some(EpisodePathFilter::Exact(path)),
                (None, Some(prefix)) => Some(EpisodePathFilter::Prefix(prefix)),
                (None, None) => None,
            };
            let query = EpisodeQueryV1 {
                text: Some(query),
                root_kind,
                root_id,
                related_intent_id: intent,
                related_task_id: task,
                ended_from,
                ended_until,
                effective_at: Some(Utc::now()),
                completion_status: completion,
                code_change_status: code_change,
                path,
                include_diagnostics,
                expand_evidence: false,
                limit,
            };
            query
                .validate()
                .map_err(|error| invalid_query_error(error.to_string()))?;
            run_search(&repository, &query, output).await
        }
        MemorySubcommand::Show {
            note_id,
            revision,
            evidence,
        } => run_show(&repository, &note_id, revision.as_deref(), evidence, output).await,
        MemorySubcommand::Status => run_status(&repository, output).await,
        MemorySubcommand::Rebuild { dry_run } => {
            let report = MemoryDiagnostics::new(&repository.history, None)
                .rebuild(dry_run)
                .await
                .map_err(map_writer_error)?;
            render_rebuild(&report, output)
        }
    }
}

struct MemoryCommandRepository {
    history: HistoryManager,
    database_path: PathBuf,
    #[cfg(test)]
    digest_override: Option<Arc<RepositoryKeyedDigest>>,
}

impl MemoryCommandRepository {
    async fn open(upgrade_schema: bool) -> CliResult<Self> {
        let storage_root = try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
        let database_path = storage_root.join(DATABASE);
        let inspected_database = db::open_database_without_migrations(&database_path)
            .await
            .map_err(|error| memory_database_open_error(&database_path, error))?;
        match db::inspect_database_schema_for_connection(&inspected_database)
            .await
            .map_err(|error| memory_database_open_error(&database_path, error))?
        {
            db::SchemaCompatibility::UnsupportedFuture {
                current_version,
                latest_version,
            } => {
                return Err(CliError::fatal(format!(
                    "repository Memory schema version {current_version} is newer than this Libra build supports (latest supported: {})",
                    latest_version
                        .map(|version| version.to_string())
                        .unwrap_or_else(|| "none".to_string())
                ))
                .with_stable_code(StableErrorCode::MemoryContractViolation)
                .with_hint("upgrade Libra before reading or rebuilding this repository Memory"));
            }
            db::SchemaCompatibility::Compatible { .. }
            | db::SchemaCompatibility::UpgradeRequired { .. } => {}
        }
        let database = if upgrade_schema {
            drop(inspected_database);
            db::get_db_conn_instance_for_path(&database_path)
                .await
                .map_err(|error| memory_database_open_error(&database_path, error))?
        } else {
            inspected_database
        };
        let storage = Arc::new(LocalStorage::new(storage_root.join("objects")));
        Ok(Self {
            history: HistoryManager::new(storage, storage_root, Arc::new(database)),
            database_path,
            #[cfg(test)]
            digest_override: None,
        })
    }

    async fn digest(&self) -> CliResult<Arc<RepositoryKeyedDigest>> {
        #[cfg(test)]
        if let Some(digest) = &self.digest_override {
            return Ok(Arc::clone(digest));
        }
        let database = self.history.database_connection();
        RepositoryKeyedDigest::load_existing_with_connection(&self.database_path, &database)
            .await
            .map_err(|error| {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::MemoryDigestKeyUnavailable)
            })
    }
}

async fn run_search(
    repository: &MemoryCommandRepository,
    query: &EpisodeQueryV1,
    output: &OutputConfig,
) -> CliResult<()> {
    let output_value = search_output(repository, query).await?;
    render_search(&output_value, output)
}

async fn search_output(
    repository: &MemoryCommandRepository,
    query: &EpisodeQueryV1,
) -> CliResult<MemorySearchOutput> {
    if let Some(text) = query.text.as_deref() {
        validate_plain_text_query(text).map_err(|error| invalid_query_error(error.to_string()))?;
    }
    let preflight_context = AuthenticatedMemoryContext::repository_system(
        "memory-status-preflight",
        "libra-memory-cli",
    )
    .map_err(map_writer_error)?;
    let preflight = MemoryDiagnostics::new(&repository.history, None)
        .status(&preflight_context)
        .await
        .map_err(map_writer_error)?;
    ensure_fts5_available(preflight.fts5_enabled)?;
    if preflight.memory_ref.is_none() {
        return Ok(MemorySearchOutput::empty());
    }

    let digest = repository.digest().await?;
    let context = command_context(&digest)?;
    let reader = EpisodeReader::new(&repository.history, &digest).map_err(map_reader_error)?;
    let view = reader
        .freeze_view(&context)
        .await
        .map_err(map_reader_error)?;
    let result = reader
        .search(&context, &view, query)
        .await
        .map_err(map_reader_error)?;
    Ok(MemorySearchOutput {
        view_hash: Some(result.view_hash),
        selector_version: Some(result.selector_version.to_string()),
        candidates_examined: result.candidates_examined,
        relation_omissions: result.relation_omissions,
        omitted_by_applicability: result.omitted_by_applicability,
        selector_limit_omissions: result.selector_limit_omissions,
        items: result
            .items
            .iter()
            .map(SearchItemOutput::from)
            .collect::<CliResult<Vec<_>>>()?,
    })
}

async fn run_show(
    repository: &MemoryCommandRepository,
    note_id: &str,
    revision: Option<&str>,
    expand_evidence: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let output_value = show_output(repository, note_id, revision, expand_evidence).await?;
    render_show(&output_value, output)
}

async fn show_output(
    repository: &MemoryCommandRepository,
    note_id: &str,
    revision: Option<&str>,
    expand_evidence: bool,
) -> CliResult<ShowItemOutput> {
    let note_id =
        Uuid::parse_str(note_id).map_err(|_| invalid_query_error("note ID must be a UUID"))?;
    let revision = revision
        .map(str::parse::<ObjectHash>)
        .transpose()
        .map_err(|_| invalid_query_error("revision must be a hexadecimal object ID"))?;
    let preflight_context =
        AuthenticatedMemoryContext::repository_system("memory-show-preflight", "libra-memory-cli")
            .map_err(map_writer_error)?;
    let preflight = MemoryDiagnostics::new(&repository.history, None)
        .status(&preflight_context)
        .await
        .map_err(map_writer_error)?;
    if preflight.memory_ref.is_none() {
        return Err(memory_not_found_error(note_id));
    }
    let digest = repository.digest().await?;
    let context = command_context(&digest)?;
    let reader = EpisodeReader::new(&repository.history, &digest).map_err(map_reader_error)?;
    let view = reader
        .freeze_view(&context)
        .await
        .map_err(map_reader_error)?;
    let item = reader
        .show(&context, &view, note_id, revision, expand_evidence)
        .await
        .map_err(map_reader_error)?
        .ok_or_else(|| memory_not_found_error(note_id))?;
    ShowItemOutput::from_item(&item)
}

async fn run_status(repository: &MemoryCommandRepository, output: &OutputConfig) -> CliResult<()> {
    let output_value = status_output(repository).await?;
    render_status(&output_value, output)
}

async fn status_output(repository: &MemoryCommandRepository) -> CliResult<StatusOutput> {
    let digest = repository.digest().await.ok();
    let context = match digest.as_deref() {
        Some(digest) => command_context(digest)?,
        None => AuthenticatedMemoryContext::repository_system(
            "memory-status-unavailable",
            "libra-memory-cli",
        )
        .map_err(map_writer_error)?,
    };
    let report = MemoryDiagnostics::new(&repository.history, digest.as_deref())
        .status(&context)
        .await
        .map_err(map_writer_error)?;
    Ok(StatusOutput::from_report(&report, digest.is_some()))
}

fn memory_not_found_error(note_id: Uuid) -> CliError {
    CliError::fatal(format!("Memory note '{note_id}' was not found"))
        .with_stable_code(StableErrorCode::MemoryNotFound)
}

fn ensure_fts5_available(enabled: bool) -> CliResult<()> {
    if enabled {
        return Ok(());
    }
    Err(
        CliError::fatal("SQLite FTS5 is unavailable in this Libra build")
            .with_stable_code(StableErrorCode::MemoryFtsUnavailable)
            .with_hint("install a Libra release built with bundled SQLite FTS5 support"),
    )
}

fn command_context(digest: &RepositoryKeyedDigest) -> CliResult<AuthenticatedMemoryContext> {
    AuthenticatedMemoryContext::repository_system(digest.repository_id(), "libra-memory-cli")
        .map_err(map_writer_error)
}

#[derive(Serialize)]
struct MemorySearchOutput {
    view_hash: Option<String>,
    selector_version: Option<String>,
    candidates_examined: usize,
    relation_omissions: usize,
    omitted_by_applicability: usize,
    selector_limit_omissions: usize,
    items: Vec<SearchItemOutput>,
}

impl MemorySearchOutput {
    fn empty() -> Self {
        Self {
            view_hash: None,
            selector_version: None,
            candidates_examined: 0,
            relation_omissions: 0,
            omitted_by_applicability: 0,
            selector_limit_omissions: 0,
            items: Vec::new(),
        }
    }
}

#[derive(Serialize)]
struct SearchItemOutput {
    note_id: String,
    revision_oid: String,
    root_kind: &'static str,
    root_id: String,
    path: String,
    summary: String,
    completion_status: &'static str,
    code_change_status: &'static str,
    applicability: &'static str,
    evidence_count: usize,
    ended_at: Option<String>,
    bm25_score: f64,
}

impl SearchItemOutput {
    fn from(item: &EpisodeReadItemV1) -> CliResult<Self> {
        let episode = item
            .note
            .episode
            .as_ref()
            .ok_or_else(corrupt_output_error)?;
        Ok(Self {
            note_id: item.note.note_id.to_string(),
            revision_oid: item.revision_oid.to_string(),
            root_kind: root_kind_label(episode.root_kind),
            root_id: episode.root_id.clone(),
            path: item.note.path.clone(),
            summary: episode.summary.claim.clone(),
            completion_status: completion_label(episode.completion_status),
            code_change_status: code_change_label(episode.code_change_status),
            applicability: applicability_label(item.applicability),
            evidence_count: evidence_ref_count(&item.note),
            ended_at: episode.ended_at.map(|value| value.to_rfc3339()),
            bm25_score: item.bm25_score,
        })
    }
}

#[derive(Serialize)]
struct ShowItemOutput {
    note_id: String,
    revision_oid: String,
    path: String,
    applicability: &'static str,
    body: String,
    episode: Value,
    #[serde(skip)]
    human_episode: HumanEpisodeOutput,
    evidence_count: usize,
    evidence: Vec<ResolvedEvidenceOutput>,
    evidence_omissions: Vec<EvidenceOmissionOutput>,
    read_cost: ReadCostOutput,
}

impl ShowItemOutput {
    fn from_item(item: &EpisodeReadItemV1) -> CliResult<Self> {
        let episode = item
            .note
            .episode
            .as_ref()
            .ok_or_else(corrupt_output_error)?;
        Ok(Self {
            note_id: item.note.note_id.to_string(),
            revision_oid: item.revision_oid.to_string(),
            path: item.note.path.clone(),
            applicability: applicability_label(item.applicability),
            body: item.note.body.clone(),
            episode: serde_json::to_value(episode).map_err(|error| {
                CliError::internal(format!("failed to serialize Memory Episode: {error}"))
            })?,
            human_episode: HumanEpisodeOutput {
                root_kind: root_kind_label(episode.root_kind),
                root_id: episode.root_id.clone(),
                completion_status: completion_label(episode.completion_status),
                code_change_status: code_change_label(episode.code_change_status),
                started_at: episode.started_at.map(|value| value.to_rfc3339()),
                ended_at: episode.ended_at.map(|value| value.to_rfc3339()),
                goal: episode.goal.claim.clone(),
                summary: episode.summary.claim.clone(),
                observations: episode
                    .observations
                    .iter()
                    .map(|claim| claim.claim.clone())
                    .collect(),
                inferences: episode
                    .inferences
                    .iter()
                    .map(|claim| claim.claim.clone())
                    .collect(),
                decisions: episode
                    .decisions
                    .iter()
                    .map(|claim| claim.claim.clone())
                    .collect(),
                failed_attempts: episode
                    .failed_attempts
                    .iter()
                    .map(|claim| claim.claim.clone())
                    .collect(),
                unresolved: episode
                    .unresolved
                    .iter()
                    .map(|claim| claim.claim.clone())
                    .collect(),
                base_oid: episode.code.base_oid.clone(),
                result_oid: episode.code.result_oid.clone(),
                branch_ref: episode.code.branch_ref.clone(),
                code_paths: episode.code.paths.clone(),
            },
            evidence_count: evidence_ref_count(&item.note),
            evidence: item
                .evidence
                .resolved
                .iter()
                .map(|evidence| {
                    Ok(ResolvedEvidenceOutput {
                        reference: serde_json::to_value(&evidence.reference).map_err(|error| {
                            CliError::internal(format!(
                                "failed to serialize Memory evidence reference: {error}"
                            ))
                        })?,
                        redacted_text: evidence.redacted_text.clone(),
                    })
                })
                .collect::<CliResult<Vec<_>>>()?,
            evidence_omissions: item
                .evidence
                .omissions
                .iter()
                .map(|omission| EvidenceOmissionOutput {
                    object_id: omission.object_id.clone(),
                    reason: omission_reason_label(omission.reason),
                })
                .collect(),
            read_cost: ReadCostOutput {
                projection_rows: item.read_cost.projection_rows,
                note_objects: item.read_cost.note_objects,
                code_commits_visited: item.read_cost.code_commits_visited,
                code_paths_compared: item.read_cost.code_paths_compared,
                evidence_items: item.read_cost.evidence_items,
            },
        })
    }
}

struct HumanEpisodeOutput {
    root_kind: &'static str,
    root_id: String,
    completion_status: &'static str,
    code_change_status: &'static str,
    started_at: Option<String>,
    ended_at: Option<String>,
    goal: String,
    summary: String,
    observations: Vec<String>,
    inferences: Vec<String>,
    decisions: Vec<String>,
    failed_attempts: Vec<String>,
    unresolved: Vec<String>,
    base_oid: Option<String>,
    result_oid: Option<String>,
    branch_ref: Option<String>,
    code_paths: Vec<String>,
}

#[derive(Serialize)]
struct ResolvedEvidenceOutput {
    reference: Value,
    redacted_text: String,
}

#[derive(Serialize)]
struct EvidenceOmissionOutput {
    object_id: String,
    reason: &'static str,
}

#[derive(Serialize)]
struct ReadCostOutput {
    projection_rows: usize,
    note_objects: usize,
    code_commits_visited: usize,
    code_paths_compared: usize,
    evidence_items: usize,
}

#[derive(Serialize)]
struct StatusOutput {
    memory_ref: Option<String>,
    projection_state: &'static str,
    projection_head: Option<String>,
    projected_ref: Option<String>,
    last_event_seq: Option<u64>,
    jobs: crate::internal::ai::memory::MemoryJobStatus,
    fts5_enabled: bool,
    digest_key_available: bool,
    view_hash: Option<String>,
}

impl StatusOutput {
    fn from_report(report: &MemoryStatusReport, digest_key_available: bool) -> Self {
        Self {
            memory_ref: report.memory_ref.clone(),
            projection_state: report.projection.state,
            projection_head: report.projection.head.clone(),
            projected_ref: report.projection.projected.clone(),
            last_event_seq: report.projection.last_event_seq,
            jobs: report.jobs.clone(),
            fts5_enabled: report.fts5_enabled,
            digest_key_available,
            view_hash: report.view_hash.clone(),
        }
    }
}

fn render_search(result: &MemorySearchOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("memory.search", result, output);
    }
    if result.items.is_empty() {
        info_println!(output, "No matching Memory Episodes.");
        return Ok(());
    }
    for item in &result.items {
        info_println!(
            output,
            "{}\t{}\t{}:{}\t{}/{}\t{}\tevidence={}\tscore={:.4}",
            item.note_id,
            item.revision_oid,
            item.root_kind,
            item.root_id,
            item.completion_status,
            item.code_change_status,
            item.applicability,
            item.evidence_count,
            item.bm25_score,
        );
        info_println!(output, "  {}", item.summary);
    }
    Ok(())
}

fn render_show(result: &ShowItemOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("memory.show", result, output);
    }
    info_println!(output, "note: {}", result.note_id);
    info_println!(output, "revision: {}", result.revision_oid);
    info_println!(output, "path: {}", result.path);
    info_println!(output, "applicability: {}", result.applicability);
    info_println!(
        output,
        "episode: {}:{}  outcome={}  code_change={}",
        result.human_episode.root_kind,
        result.human_episode.root_id,
        result.human_episode.completion_status,
        result.human_episode.code_change_status,
    );
    info_println!(
        output,
        "time: {} .. {}",
        result
            .human_episode
            .started_at
            .as_deref()
            .unwrap_or("<unknown>"),
        result
            .human_episode
            .ended_at
            .as_deref()
            .unwrap_or("<unknown>"),
    );
    info_println!(output, "goal: {}", result.human_episode.goal);
    info_println!(output, "summary: {}", result.human_episode.summary);
    render_claims(output, "observations", &result.human_episode.observations);
    render_claims(output, "inferences", &result.human_episode.inferences);
    render_claims(output, "decisions", &result.human_episode.decisions);
    render_claims(
        output,
        "failed attempts",
        &result.human_episode.failed_attempts,
    );
    render_claims(output, "unresolved", &result.human_episode.unresolved);
    info_println!(
        output,
        "code: base={} result={} branch={} paths={}",
        result.human_episode.base_oid.as_deref().unwrap_or("<none>"),
        result
            .human_episode
            .result_oid
            .as_deref()
            .unwrap_or("<none>"),
        result
            .human_episode
            .branch_ref
            .as_deref()
            .unwrap_or("<none>"),
        if result.human_episode.code_paths.is_empty() {
            "<none>".to_string()
        } else {
            result.human_episode.code_paths.join(",")
        },
    );
    info_println!(output, "evidence refs: {}", result.evidence_count);
    if !result.evidence.is_empty() {
        info_println!(output, "resolved evidence:");
        for evidence in &result.evidence {
            info_println!(output, "  {}", evidence.redacted_text);
        }
    }
    if !result.evidence_omissions.is_empty() {
        info_println!(output, "evidence omissions:");
        for omission in &result.evidence_omissions {
            info_println!(output, "  {} ({})", omission.object_id, omission.reason);
        }
    }
    Ok(())
}

fn render_claims(output: &OutputConfig, label: &str, claims: &[String]) {
    if claims.is_empty() {
        return;
    }
    info_println!(output, "{label}:");
    for claim in claims {
        info_println!(output, "  - {claim}");
    }
}

fn render_status(result: &StatusOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("memory.status", result, output);
    }
    info_println!(
        output,
        "memory ref: {}",
        result.memory_ref.as_deref().unwrap_or("<none>")
    );
    info_println!(output, "projection: {}", result.projection_state);
    info_println!(
        output,
        "projected ref: {}",
        result.projected_ref.as_deref().unwrap_or("<none>")
    );
    info_println!(
        output,
        "last event seq: {}",
        result
            .last_event_seq
            .map_or_else(|| "<unknown>".to_string(), |value| value.to_string())
    );
    info_println!(output, "FTS5: {}", enabled_label(result.fts5_enabled));
    info_println!(
        output,
        "digest key: {}",
        enabled_label(result.digest_key_available)
    );
    info_println!(
        output,
        "view hash: {}",
        result.view_hash.as_deref().unwrap_or("<unavailable>")
    );
    info_println!(
        output,
        "jobs: scanned={}/{} truncated={} idle={} dirty={} inflight={} failed={} active_leases={} expired_leases={} pending_generations={} retries={} errors={}",
        result.jobs.total,
        result.jobs.scan_limit,
        result.jobs.truncated,
        result.jobs.idle,
        result.jobs.dirty,
        result.jobs.inflight,
        result.jobs.failed,
        result.jobs.active_leases,
        result.jobs.expired_leases,
        result.jobs.pending_generations,
        result.jobs.retry_count,
        result.jobs.error_count,
    );
    Ok(())
}

fn render_rebuild(result: &MemoryRebuildReport, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("memory.rebuild", result, output);
    }
    let verb = if result.dry_run {
        "validated"
    } else {
        "rebuilt"
    };
    info_println!(
        output,
        "Memory projection {verb}: head={} events={} notes={} revisions={} last_event_seq={} changed={}",
        result.head.as_deref().unwrap_or("<none>"),
        result.event_count,
        result.note_count,
        result.revision_count,
        result.last_event_seq,
        result.changed,
    );
    Ok(())
}

fn evidence_ref_count(note: &MemoryNoteV1) -> usize {
    let mut count = note.evidence_refs.len();
    count = count.saturating_add(
        note.links
            .iter()
            .map(|link| link.evidence_refs.len())
            .sum::<usize>(),
    );
    count = count.saturating_add(
        note.entities
            .iter()
            .map(|entity| entity.evidence_refs.len())
            .sum::<usize>(),
    );
    if let Some(episode) = &note.episode {
        count = count
            .saturating_add(episode.goal.evidence_refs.len())
            .saturating_add(episode.summary.evidence_refs.len());
        for claim in episode
            .observations
            .iter()
            .chain(&episode.inferences)
            .chain(&episode.decisions)
            .chain(&episode.failed_attempts)
            .chain(&episode.unresolved)
        {
            count = count.saturating_add(claim.evidence_refs.len());
        }
    }
    count
}

fn map_writer_error(error: MemoryWriterError) -> CliError {
    let code = match error.kind() {
        MemoryWriterErrorKind::DigestKeyUnavailable => StableErrorCode::MemoryDigestKeyUnavailable,
        MemoryWriterErrorKind::InvalidProposal | MemoryWriterErrorKind::SourceLimitExceeded => {
            StableErrorCode::MemoryContractViolation
        }
        MemoryWriterErrorKind::PolicyRejected
        | MemoryWriterErrorKind::SourceRejected
        | MemoryWriterErrorKind::UnknownDigestKey => StableErrorCode::MemoryPolicyRejected,
        MemoryWriterErrorKind::EvidenceMismatch
        | MemoryWriterErrorKind::CorruptHistory
        | MemoryWriterErrorKind::CorruptProjection => StableErrorCode::MemoryCorrupt,
        MemoryWriterErrorKind::ProjectionStale => StableErrorCode::MemoryProjectionStale,
        MemoryWriterErrorKind::StorageFailure | MemoryWriterErrorKind::ConflictExhausted => {
            StableErrorCode::MemoryStorageFailure
        }
    };
    let damage_point = error.damage_point().map(ToString::to_string);
    let message = match damage_point.as_deref() {
        Some(point) => format!("{error} (damage point: {point})"),
        None => error.to_string(),
    };
    let cli_error = CliError::fatal(message).with_stable_code(code);
    match damage_point {
        Some(point) => cli_error.with_detail("damage_point", point),
        None => cli_error,
    }
}

fn map_reader_error(error: EpisodeReaderError) -> CliError {
    let (code, message, hint) = match error.kind() {
        EpisodeReaderErrorKind::InvalidQuery => (
            StableErrorCode::MemoryQueryInvalid,
            "the Memory query or filter is invalid",
            "check `libra memory search --help` and retry with bounded filter values",
        ),
        EpisodeReaderErrorKind::InvalidConfiguration => (
            StableErrorCode::MemoryPolicyRejected,
            "the repository Memory reader configuration is invalid",
            "inspect the repository Memory configuration before retrying",
        ),
        EpisodeReaderErrorKind::InvalidCodeAnchor => (
            StableErrorCode::MemoryPolicyRejected,
            "the current branch, worktree, or code revision cannot anchor a Memory view",
            "repair the current repository HEAD/worktree identity, then retry",
        ),
        EpisodeReaderErrorKind::Unauthorized => (
            StableErrorCode::MemoryPolicyRejected,
            "the current repository principal cannot read this Memory item",
            "retry from the repository and principal that own the referenced Memory",
        ),
        EpisodeReaderErrorKind::StaleProjection => (
            StableErrorCode::MemoryProjectionStale,
            "the Memory projection does not match the authoritative Memory ref",
            "run `libra memory rebuild --dry-run`, then `libra memory rebuild` if validation succeeds",
        ),
        EpisodeReaderErrorKind::UnknownPolicy => (
            StableErrorCode::MemoryCorrupt,
            "the Memory history uses an unsupported policy version",
            "upgrade Libra to a version that supports this Memory policy",
        ),
        EpisodeReaderErrorKind::CorruptProjection => (
            StableErrorCode::MemoryCorrupt,
            "the Memory projection or referenced Episode is inconsistent",
            "run `libra memory rebuild --dry-run` to locate the damaged history or projection",
        ),
        EpisodeReaderErrorKind::StorageUnavailable => (
            StableErrorCode::MemoryStorageFailure,
            "the Memory database or object store could not be read",
            "check repository storage permissions and integrity, then retry",
        ),
    };
    CliError::fatal(message)
        .with_stable_code(code)
        .with_hint(hint)
}

fn invalid_query_error(reason: impl std::fmt::Display) -> CliError {
    CliError::command_usage(format!("invalid Memory query or filter: {reason}"))
        .with_stable_code(StableErrorCode::MemoryQueryInvalid)
        .with_hint("check `libra memory search --help` for accepted values")
}

fn memory_database_open_error(
    database_path: &std::path::Path,
    error: impl std::fmt::Display,
) -> CliError {
    CliError::fatal(format!(
        "failed to open or inspect repository Memory database '{}': {error}",
        database_path.display()
    ))
    .with_stable_code(StableErrorCode::MemoryStorageFailure)
    .with_hint(
        "check repository storage permissions and integrity; run `libra memory rebuild` when a known schema upgrade is pending",
    )
}

fn invalid_filter_error(reason: String) -> CliError {
    invalid_query_error(reason)
}

fn corrupt_output_error() -> CliError {
    CliError::fatal("Memory projection selected a note without an Episode payload")
        .with_stable_code(StableErrorCode::MemoryCorrupt)
}

fn parse_limit(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "limit must be an integer".to_string())?;
    if (1..=MAX_RESULT_LIMIT).contains(&parsed) {
        Ok(parsed)
    } else {
        Err(format!("limit must be between 1 and {MAX_RESULT_LIMIT}"))
    }
}

fn parse_rfc3339(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| "expected an RFC3339 timestamp".to_string())
}

fn parse_root_kind(value: &str) -> Result<EpisodeRootKind, String> {
    match value {
        "task" => Ok(EpisodeRootKind::Task),
        "intent" => Ok(EpisodeRootKind::Intent),
        _ => Err("--root-kind must be `task` or `intent`".to_string()),
    }
}

fn parse_completion(value: &str) -> Result<CompletionStatus, String> {
    match value {
        "completed" => Ok(CompletionStatus::Completed),
        "failed" => Ok(CompletionStatus::Failed),
        "cancelled" => Ok(CompletionStatus::Cancelled),
        _ => Err("--completion must be `completed`, `failed`, or `cancelled`".to_string()),
    }
}

fn parse_code_change(value: &str) -> Result<CodeChangeStatus, String> {
    match value {
        "changed" => Ok(CodeChangeStatus::Changed),
        "unchanged" => Ok(CodeChangeStatus::Unchanged),
        "unknown" => Ok(CodeChangeStatus::Unknown),
        _ => Err("--code-change must be `changed`, `unchanged`, or `unknown`".to_string()),
    }
}

const fn root_kind_label(value: EpisodeRootKind) -> &'static str {
    match value {
        EpisodeRootKind::Task => "task",
        EpisodeRootKind::Intent => "intent",
    }
}

const fn completion_label(value: CompletionStatus) -> &'static str {
    match value {
        CompletionStatus::Completed => "completed",
        CompletionStatus::Failed => "failed",
        CompletionStatus::Cancelled => "cancelled",
    }
}

const fn code_change_label(value: CodeChangeStatus) -> &'static str {
    match value {
        CodeChangeStatus::Changed => "changed",
        CodeChangeStatus::Unchanged => "unchanged",
        CodeChangeStatus::Unknown => "unknown",
    }
}

const fn applicability_label(value: CodeApplicability) -> &'static str {
    match value {
        CodeApplicability::Exact => "exact",
        CodeApplicability::DescendantUnchanged => "descendant_unchanged",
        CodeApplicability::DescendantPathChanged => "descendant_path_changed",
        CodeApplicability::Diverged => "diverged",
        CodeApplicability::Unknown => "unknown",
    }
}

const fn omission_reason_label(value: EvidenceOmissionReason) -> &'static str {
    match value {
        EvidenceOmissionReason::LimitExceeded => "limit_exceeded",
        EvidenceOmissionReason::Unauthorized => "unauthorized",
        EvidenceOmissionReason::SourceUnreachable => "source_unreachable",
        EvidenceOmissionReason::SourceCorrupt => "source_corrupt",
        EvidenceOmissionReason::DigestMismatch => "digest_mismatch",
        EvidenceOmissionReason::UnsupportedLocator => "unsupported_locator",
    }
}

const fn enabled_label(value: bool) -> &'static str {
    if value { "enabled" } else { "unavailable" }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::TimeZone;
    use sea_orm::ConnectionTrait;

    use super::*;
    use crate::internal::ai::memory::{
        MemoryDamagePoint, MemorySensitivity, memory_test_commit_injectable_episode,
        memory_test_fixture, memory_test_history, memory_test_seed_code_head,
    };

    #[test]
    fn limit_parser_enforces_reader_bound() {
        assert_eq!(parse_limit("1"), Ok(1));
        assert_eq!(parse_limit("50"), Ok(50));
        assert!(parse_limit("0").is_err());
        assert!(parse_limit("51").is_err());
    }

    #[test]
    fn mutability_classification_only_marks_real_rebuild() {
        assert!(
            !MemoryArgs {
                command: MemorySubcommand::Status,
            }
            .mutates_repository()
        );
        assert!(
            !MemoryArgs {
                command: MemorySubcommand::Rebuild { dry_run: true },
            }
            .mutates_repository()
        );
        assert!(
            MemoryArgs {
                command: MemorySubcommand::Rebuild { dry_run: false },
            }
            .mutates_repository()
        );
    }

    #[test]
    fn memory_error_kinds_map_to_stable_cli_codes() {
        for (kind, code) in [
            (
                MemoryWriterErrorKind::DigestKeyUnavailable,
                StableErrorCode::MemoryDigestKeyUnavailable,
            ),
            (
                MemoryWriterErrorKind::InvalidProposal,
                StableErrorCode::MemoryContractViolation,
            ),
            (
                MemoryWriterErrorKind::PolicyRejected,
                StableErrorCode::MemoryPolicyRejected,
            ),
            (
                MemoryWriterErrorKind::CorruptHistory,
                StableErrorCode::MemoryCorrupt,
            ),
            (
                MemoryWriterErrorKind::ProjectionStale,
                StableErrorCode::MemoryProjectionStale,
            ),
            (
                MemoryWriterErrorKind::StorageFailure,
                StableErrorCode::MemoryStorageFailure,
            ),
        ] {
            let error = MemoryWriterError::new(kind, "redacted test summary");
            assert_eq!(map_writer_error(error).stable_code(), code);
        }

        let damaged = map_writer_error(
            MemoryWriterError::new(
                MemoryWriterErrorKind::CorruptHistory,
                "redacted corruption summary",
            )
            .with_damage_point(MemoryDamagePoint::EventIdentity {
                event_seq: 7,
                event_id: "550e8400-e29b-41d4-a716-446655440000"
                    .parse()
                    .expect("valid test event ID"),
            }),
        );
        assert_eq!(
            damaged.details().get("damage_point"),
            Some(&serde_json::json!(
                "event_seq=7,event_id=550e8400-e29b-41d4-a716-446655440000"
            ))
        );

        for (kind, code) in [
            (
                EpisodeReaderErrorKind::InvalidQuery,
                StableErrorCode::MemoryQueryInvalid,
            ),
            (
                EpisodeReaderErrorKind::Unauthorized,
                StableErrorCode::MemoryPolicyRejected,
            ),
            (
                EpisodeReaderErrorKind::StaleProjection,
                StableErrorCode::MemoryProjectionStale,
            ),
            (
                EpisodeReaderErrorKind::UnknownPolicy,
                StableErrorCode::MemoryCorrupt,
            ),
            (
                EpisodeReaderErrorKind::StorageUnavailable,
                StableErrorCode::MemoryStorageFailure,
            ),
        ] {
            let error = EpisodeReaderError::for_tests(kind);
            assert_eq!(map_reader_error(error).stable_code(), code);
        }
    }

    #[test]
    fn unavailable_fts5_uses_the_stable_memory_error() {
        ensure_fts5_available(true).expect("available FTS5 should pass preflight");
        let error = ensure_fts5_available(false).expect_err("missing FTS5 must fail closed");
        assert_eq!(error.stable_code(), StableErrorCode::MemoryFtsUnavailable);
        assert!(
            error
                .hints()
                .iter()
                .any(|hint| hint.as_str().contains("bundled SQLite FTS5")),
            "missing FTS5 should explain how to obtain a supported build"
        );
    }

    #[tokio::test]
    async fn memory_search_show_status_rebuild() {
        let fixture = memory_test_fixture().await;
        let code_commit = memory_test_seed_code_head(&fixture).await;
        let target = memory_test_commit_injectable_episode(
            &fixture,
            code_commit,
            "task-memory-cli-e2e",
            1,
            MemorySensitivity::Internal,
            "authentication retry failed after the request timeout",
            "A bounded retry fixed the authentication timeout.",
        )
        .await;
        let repository = MemoryCommandRepository {
            history: memory_test_history(&fixture),
            database_path: fixture._temp.path().join(DATABASE),
            digest_override: Some(Arc::clone(&fixture.digest)),
        };
        let effective_at = Utc
            .with_ymd_and_hms(2026, 8, 25, 12, 0, 0)
            .single()
            .expect("fixed effective time");
        let query = EpisodeQueryV1 {
            text: Some("authentication retry".to_string()),
            effective_at: Some(effective_at),
            ..EpisodeQueryV1::default()
        };
        query.validate().expect("valid CLI search query");

        let initial_search = search_output(&repository, &query)
            .await
            .expect("search current Memory");
        assert_eq!(initial_search.items.len(), 1);
        assert_eq!(
            initial_search.items[0].note_id,
            target.root().note_id().to_string()
        );
        let historical_revision = initial_search.items[0].revision_oid.clone();

        let revised_target = memory_test_commit_injectable_episode(
            &fixture,
            code_commit,
            "task-memory-cli-e2e",
            2,
            MemorySensitivity::Internal,
            "authentication retry failed after the request timeout",
            "A second revision records the bounded authentication retry.",
        )
        .await;
        assert_eq!(revised_target.root().note_id(), target.root().note_id());

        let search = search_output(&repository, &query)
            .await
            .expect("search latest Memory revision");
        assert_eq!(search.items.len(), 1);
        assert_eq!(search.items[0].note_id, target.root().note_id().to_string());
        assert_eq!(search.items[0].root_id, "task-memory-cli-e2e");
        assert_ne!(search.items[0].revision_oid, historical_revision);
        assert!(search.view_hash.is_some());

        let show = tokio::time::timeout(
            Duration::from_secs(5),
            show_output(&repository, &search.items[0].note_id, None, true),
        )
        .await
        .expect("evidence expansion must not wait on the single SQLite connection")
        .expect("show selected Memory note");
        assert_eq!(show.note_id, search.items[0].note_id);
        assert!(show.evidence_count > 0);
        let expanded_evidence = show.evidence.len() + show.evidence_omissions.len();
        assert!(expanded_evidence > 0);
        assert!(expanded_evidence <= show.evidence_count);

        let historical = show_output(
            &repository,
            &search.items[0].note_id,
            Some(&historical_revision),
            false,
        )
        .await
        .expect("show historical Memory revision");
        assert_eq!(historical.note_id, show.note_id);
        assert_eq!(historical.revision_oid, historical_revision);
        assert_ne!(historical.revision_oid, show.revision_oid);
        assert_ne!(historical.episode, show.episode);

        let current = status_output(&repository)
            .await
            .expect("inspect current projection");
        assert_eq!(current.projection_state, "current");
        assert!(current.memory_ref.is_some());
        assert!(current.digest_key_available);

        fixture
            .database
            .execute_unprepared("DELETE FROM memory_projection_state")
            .await
            .expect("remove only the rebuildable projection watermark");

        let dry_run = MemoryDiagnostics::new(&repository.history, None)
            .rebuild(true)
            .await
            .expect("plan projection rebuild");
        assert!(dry_run.dry_run);
        assert!(!dry_run.changed);
        let state_rows = fixture
            .database
            .query_one_raw(sea_orm::Statement::from_string(
                fixture.database.get_database_backend(),
                "SELECT COUNT(*) AS count FROM memory_projection_state".to_string(),
            ))
            .await
            .expect("count projection state after dry-run")
            .expect("projection count row")
            .try_get::<i64>("", "count")
            .expect("decode projection count");
        assert_eq!(state_rows, 0, "dry-run must perform zero writes");

        let stale = match search_output(&repository, &query).await {
            Ok(_) => panic!("search must reject a stale projection"),
            Err(error) => error,
        };
        assert_eq!(stale.stable_code(), StableErrorCode::MemoryProjectionStale);

        let rebuilt = MemoryDiagnostics::new(&repository.history, None)
            .rebuild(false)
            .await
            .expect("rebuild projection from Memory history");
        assert!(!rebuilt.dry_run);
        assert!(rebuilt.changed);
        assert!(rebuilt.event_count > 0);

        let repaired = status_output(&repository)
            .await
            .expect("inspect rebuilt projection");
        assert_eq!(repaired.projection_state, "current");
        let search_after_rebuild = search_output(&repository, &query)
            .await
            .expect("search rebuilt Memory projection");
        assert_eq!(search_after_rebuild.items.len(), 1);
    }
}
