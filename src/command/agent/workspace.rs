//! `libra agent workspace …` — the read-only machine interface over the
//! Part C W4 workspace registry (§C.8): keyset-paginated `list` and a
//! by-id `show`. Lease mutation stays internal to the AgentRuntime
//! services; this surface only OBSERVES `workspace_record`.

use clap::{Args, Subcommand};

#[cfg(feature = "storage-sqlite")]
use crate::internal::db;
use crate::{
    internal::workspace::{WorkspaceListQuery, WorkspaceRecord, WorkspaceState, WorkspaceStore},
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
    },
};

/// `schema_version` of the paged list / show JSON `data` payloads
/// (additive evolution only — mirrors the AG-20 pagination precedent).
const WORKSPACE_SCHEMA_VERSION: u32 = 1;

#[derive(Subcommand, Debug)]
pub enum WorkspaceSubcommand {
    /// List workspace records, keyset-paginated (`workspace_id` ASC).
    #[command(about = "List workspace records (keyset pagination)")]
    List(WorkspaceListArgs),
    /// Show one workspace record by id.
    #[command(about = "Show one workspace record")]
    Show(WorkspaceShowArgs),
}

#[derive(Args, Debug)]
pub struct WorkspaceListArgs {
    /// Maximum rows to return (default 50, capped at 500).
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    /// Keyset cursor: the `next_cursor` value from the previous page,
    /// round-tripped verbatim.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    /// Restrict to one or more states (repeatable):
    /// provisioning|active|releasing|released|orphaned.
    #[arg(long = "state", value_name = "STATE")]
    pub states: Vec<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceShowArgs {
    /// Workspace identifier from `agent workspace list`.
    #[arg(value_name = "WORKSPACE_ID")]
    pub workspace_id: String,
}

pub async fn execute_safe(cmd: WorkspaceSubcommand, output: &OutputConfig) -> CliResult<()> {
    match cmd {
        WorkspaceSubcommand::List(args) => list(args, output).await,
        WorkspaceSubcommand::Show(args) => show(args, output).await,
    }
}

fn parse_state(raw: &str) -> CliResult<WorkspaceState> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "provisioning" => Ok(WorkspaceState::Provisioning),
        "active" => Ok(WorkspaceState::Active),
        "releasing" => Ok(WorkspaceState::Releasing),
        "released" => Ok(WorkspaceState::Released),
        "orphaned" => Ok(WorkspaceState::Orphaned),
        other => Err(CliError::command_usage(format!(
            "invalid --state '{other}': expected provisioning, active, releasing, \
             released, or orphaned"
        ))),
    }
}

fn record_json(record: &WorkspaceRecord) -> serde_json::Value {
    serde_json::json!({
        "workspace_id": record.workspace_id,
        "kind": record.kind.as_db_value(),
        "worktree_id": record.worktree_id,
        "path": record.path,
        "owner_kind": record.owner_kind.as_db_value(),
        "owner_id": record.owner_id,
        "task_id": record.task_id,
        "session_id": record.session_id,
        "base_commit": record.base_commit,
        "branch": record.branch,
        "state": record.state.as_db_value(),
        "lease_owner": record.lease_owner,
        "lease_fence": record.lease_fence,
        "lease_expires_at": record.lease_expires_at,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

#[cfg(feature = "storage-redb")]
fn record_from_store_row(row: crate::storage::WorkspaceRecordRow) -> WorkspaceRecord {
    use crate::{
        internal::workspace::{WorkspaceKind, WorkspaceOwnerKind},
        storage::{WorkspaceRecordKind, WorkspaceRecordOwnerKind, WorkspaceRecordState},
    };
    WorkspaceRecord {
        workspace_id: row.workspace_id,
        repo_id: row.repo_id,
        kind: match row.kind {
            WorkspaceRecordKind::Linked => WorkspaceKind::Linked,
            WorkspaceRecordKind::TaskCopy => WorkspaceKind::TaskCopy,
            WorkspaceRecordKind::TaskFuse => WorkspaceKind::TaskFuse,
            WorkspaceRecordKind::Remote => WorkspaceKind::Remote,
        },
        worktree_id: row.worktree_id,
        path: row.path,
        owner_kind: match row.owner_kind {
            WorkspaceRecordOwnerKind::Human => WorkspaceOwnerKind::Human,
            WorkspaceRecordOwnerKind::Agent => WorkspaceOwnerKind::Agent,
            WorkspaceRecordOwnerKind::Automation => WorkspaceOwnerKind::Automation,
        },
        owner_id: row.owner_id,
        task_id: row.task_id,
        session_id: row.session_id,
        base_commit: row.base_commit,
        branch: row.branch,
        state: match row.state {
            WorkspaceRecordState::Provisioning => WorkspaceState::Provisioning,
            WorkspaceRecordState::Active => WorkspaceState::Active,
            WorkspaceRecordState::Releasing => WorkspaceState::Releasing,
            WorkspaceRecordState::Released => WorkspaceState::Released,
            WorkspaceRecordState::Orphaned => WorkspaceState::Orphaned,
        },
        lease_owner: row.lease_owner,
        lease_fence: row.lease_fence,
        lease_expires_at: row.lease_expires_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

async fn list(args: WorkspaceListArgs, output: &OutputConfig) -> CliResult<()> {
    let states = if args.states.is_empty() {
        None
    } else {
        Some(
            args.states
                .iter()
                .map(|raw| parse_state(raw))
                .collect::<CliResult<Vec<_>>>()?,
        )
    };
    let query = WorkspaceListQuery {
        states,
        limit: args.limit,
        cursor: args.cursor,
    };
    let storage = crate::utils::util::try_get_storage_path(None).map_err(|error| {
        CliError::fatal(format!("failed to locate repository storage: {error}"))
    })?;
    let page = match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(CliError::from)?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let config = crate::storage::runtime::open_repo_config_store_read_only(&storage)
                    .await
                    .map_err(CliError::from)?;
                let state = crate::storage::runtime::open_git_state_store_read_only(&storage)
                    .await
                    .map_err(CliError::from)?;
                let repo_id =
                    crate::internal::workspace::RepoIdentity::resolve_with_store(config.as_ref())
                        .await
                        .map_err(|error| {
                            CliError::fatal(format!(
                                "failed to resolve repository identity: {error}"
                            ))
                        })?;
                let limit = query
                    .limit
                    .unwrap_or(crate::internal::workspace::DEFAULT_LIST_LIMIT)
                    .clamp(1, crate::internal::workspace::MAX_LIST_LIMIT);
                let mut rows: Vec<WorkspaceRecord> = if let Some(filter_states) = &query.states {
                    if filter_states.is_empty() {
                        Vec::new()
                    } else {
                        WorkspaceStore::list_with_store(state.as_ref(), repo_id.as_str(), None)
                            .await
                            .map_err(CliError::from)?
                            .into_iter()
                            .map(record_from_store_row)
                            .filter(|rec| filter_states.contains(&rec.state))
                            .collect()
                    }
                } else {
                    WorkspaceStore::list_with_store(state.as_ref(), repo_id.as_str(), None)
                        .await
                        .map_err(CliError::from)?
                        .into_iter()
                        .map(record_from_store_row)
                        .collect()
                };
                if let Some(cursor) = &query.cursor {
                    rows.retain(|rec| rec.workspace_id.as_str() > cursor.as_str());
                }
                rows.sort_by(|a, b| a.workspace_id.cmp(&b.workspace_id));
                let has_more = rows.len() as u64 > limit;
                rows.truncate(limit as usize);
                let next_cursor = if has_more {
                    rows.last().map(|record| record.workspace_id.clone())
                } else {
                    None
                };
                crate::internal::workspace::WorkspacePage {
                    items: rows,
                    next_cursor,
                }
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = query;
                return Err(
                    crate::storage::error::StorageBackendError::UnsupportedBackend {
                        requested: "redb".to_string(),
                        detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                    }
                    .into_cli_error(),
                );
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = db::open_cached_repository(&storage.join(crate::utils::util::DATABASE))
                    .await
                    .map_err(|error| {
                        CliError::fatal(format!("failed to open repository database: {error}"))
                    })?;
                WorkspaceStore::list_with_conn(&conn, &query)
                    .await
                    .map_err(|error| {
                        CliError::fatal(format!("failed to list workspace records: {error}"))
                    })?
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = query;
                return Err(
                    crate::storage::error::StorageBackendError::UnsupportedBackend {
                        requested: "sqlite".to_string(),
                        detail: "this Libra binary was compiled without `storage-sqlite`"
                            .to_string(),
                    }
                    .into_cli_error(),
                );
            }
        }
    };

    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": WORKSPACE_SCHEMA_VERSION,
            "workspaces": page.items.iter().map(record_json).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        });
        return emit_json_data("agent_workspaces", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no workspace records)");
        return Ok(());
    }
    for record in &page.items {
        println!(
            "{}  {:12} {:9} {}",
            record.workspace_id,
            record.state.as_db_value(),
            record.kind.as_db_value(),
            record.path
        );
    }
    if let Some(cursor) = &page.next_cursor {
        println!("(more rows: --cursor {cursor})");
    }
    Ok(())
}

async fn show(args: WorkspaceShowArgs, output: &OutputConfig) -> CliResult<()> {
    let storage = crate::utils::util::try_get_storage_path(None).map_err(|error| {
        CliError::fatal(format!("failed to locate repository storage: {error}"))
    })?;
    let record = match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(CliError::from)?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let config = crate::storage::runtime::open_repo_config_store_read_only(&storage)
                    .await
                    .map_err(CliError::from)?;
                let state = crate::storage::runtime::open_git_state_store_read_only(&storage)
                    .await
                    .map_err(CliError::from)?;
                let repo_id =
                    crate::internal::workspace::RepoIdentity::resolve_with_store(config.as_ref())
                        .await
                        .map_err(|error| {
                            CliError::fatal(format!(
                                "failed to resolve repository identity: {error}"
                            ))
                        })?;
                WorkspaceStore::get_with_store(state.as_ref(), &args.workspace_id)
                    .await
                    .map_err(CliError::from)?
                    .filter(|row| row.repo_id == repo_id.as_str())
                    .map(record_from_store_row)
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                return Err(
                    crate::storage::error::StorageBackendError::UnsupportedBackend {
                        requested: "redb".to_string(),
                        detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                    }
                    .into_cli_error(),
                );
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = db::open_cached_repository(&storage.join(crate::utils::util::DATABASE))
                    .await
                    .map_err(|error| {
                        CliError::fatal(format!("failed to open repository database: {error}"))
                    })?;
                WorkspaceStore::get_with_conn(&conn, &args.workspace_id)
                    .await
                    .map_err(|error| {
                        CliError::fatal(format!("failed to read workspace record: {error}"))
                    })?
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                return Err(
                    crate::storage::error::StorageBackendError::UnsupportedBackend {
                        requested: "sqlite".to_string(),
                        detail: "this Libra binary was compiled without `storage-sqlite`"
                            .to_string(),
                    }
                    .into_cli_error(),
                );
            }
        }
    }
    .ok_or_else(|| {
        CliError::fatal(format!(
            "no workspace matches id '{}'; list workspaces with `libra agent workspace list`",
            args.workspace_id
        ))
    })?;

    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": WORKSPACE_SCHEMA_VERSION,
            "workspace": record_json(&record),
        });
        return emit_json_data("agent_workspace", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("workspace_id: {}", record.workspace_id);
    println!("kind:         {}", record.kind.as_db_value());
    println!("state:        {}", record.state.as_db_value());
    println!("path:         {}", record.path);
    if let Some(worktree_id) = &record.worktree_id {
        println!("worktree_id:  {worktree_id}");
    }
    println!("owner_kind:   {}", record.owner_kind.as_db_value());
    if let Some(owner_id) = &record.owner_id {
        println!("owner_id:     {owner_id}");
    }
    if let Some(task_id) = &record.task_id {
        println!("task_id:      {task_id}");
    }
    if let Some(session_id) = &record.session_id {
        println!("session_id:   {session_id}");
    }
    if let Some(branch) = &record.branch {
        println!("branch:       {branch}");
    }
    if let Some(base_commit) = &record.base_commit {
        println!("base_commit:  {base_commit}");
    }
    if let Some(lease_owner) = &record.lease_owner {
        println!("lease_owner:  {lease_owner} (fence {})", record.lease_fence);
    }
    if let Some(expires) = record.lease_expires_at {
        println!("lease_expires_at: {expires}");
    }
    println!("created_at:   {}", record.created_at);
    println!("updated_at:   {}", record.updated_at);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "storage-redb")]
    use super::*;

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn redb_agent_cli_readonly_rw_and_mega2_traces() {
        use std::{
            ffi::OsString,
            io::Write as _,
            path::{Path, PathBuf},
            str::FromStr,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };

        use flate2::{Compression, write::ZlibEncoder};
        use git_internal::{
            hash::{HashKind, ObjectHash, set_hash_kind},
            internal::object::types::ObjectType,
        };

        use crate::{
            internal::{config::ConfigKv, db::schema::DatabaseRole, head::Head},
            storage::{
                WorkspaceRecordKind, WorkspaceRecordOwnerKind, WorkspaceRecordRow,
                WorkspaceRecordState,
                capture_store::{
                    AgentCheckpointRecord, AgentSessionRecord, CaptureAgentKind,
                    CaptureCheckpointScope, CaptureScopeState, CaptureSessionState,
                },
                codec::encode_versioned,
                git_store::{RefKey, RefTarget},
                meta::{
                    MAX_SUPPORTED_READER_VERSION, META_SCHEMA_CODEC_VERSION,
                    META_SCHEMA_SINGLETON_KEY, META_SCHEMA_TABLE, MetaSchemaRecord, MetaSchemaRole,
                },
                opener::{RedbRepairContext, open_redb_writable, repair_redb_explicit},
                runtime::{
                    init_redb_repository_storage, open_capture_store_read_write,
                    open_git_state_store_read_write, open_git_store_read_only,
                    open_git_store_read_write, open_repo_config_store_read_write,
                },
            },
            utils::{error::StableErrorCode, output::JsonFormat},
        };

        struct ChangeDirGuard {
            prev: PathBuf,
        }
        impl ChangeDirGuard {
            fn enter(path: &Path) -> Self {
                let prev = std::env::current_dir().expect("read current dir");
                std::env::set_current_dir(path).expect("switch current dir");
                Self { prev }
            }
        }
        impl Drop for ChangeDirGuard {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.prev);
            }
        }

        struct EnvVarGuard {
            saved: Vec<(&'static str, Option<OsString>)>,
        }
        impl EnvVarGuard {
            fn set_all(vars: &[(&'static str, &Path)]) -> Self {
                let mut saved = Vec::with_capacity(vars.len());
                for (key, value) in vars {
                    saved.push((*key, std::env::var_os(key)));
                    // SAFETY: serialized under `#[serial_test::serial(cwd, env)]`.
                    unsafe {
                        std::env::set_var(key, value);
                    }
                }
                Self { saved }
            }
        }
        impl Drop for EnvVarGuard {
            fn drop(&mut self) {
                for (key, prev) in self.saved.iter().rev() {
                    // SAFETY: serialized under `#[serial_test::serial(cwd, env)]`.
                    unsafe {
                        match prev {
                            Some(v) => std::env::set_var(key, v),
                            None => std::env::remove_var(key),
                        }
                    }
                }
            }
        }

        #[derive(Debug)]
        struct TestCrashBackend {
            inner: redb::backends::FileBackend,
            dead: Arc<AtomicBool>,
        }
        impl redb::StorageBackend for TestCrashBackend {
            fn len(&self) -> Result<u64, redb::BackendError> {
                self.inner.len()
            }
            fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), redb::BackendError> {
                self.inner.read(offset, out)
            }
            fn set_len(&self, len: u64) -> Result<(), redb::BackendError> {
                if self.dead.load(Ordering::SeqCst) {
                    return Ok(());
                }
                self.inner.set_len(len)
            }
            fn sync_data(&self) -> Result<(), redb::BackendError> {
                if self.dead.load(Ordering::SeqCst) {
                    return Ok(());
                }
                self.inner.sync_data()
            }
            fn write(&self, offset: u64, data: &[u8]) -> Result<(), redb::BackendError> {
                if self.dead.load(Ordering::SeqCst) {
                    return Ok(());
                }
                self.inner.write(offset, data)
            }
        }

        fn make_redb_unrecovered(path: &Path) {
            const CRASH_TOUCH: redb::TableDefinition<&str, &str> =
                redb::TableDefinition::new("test_ws_crash_touch");
            let dead = Arc::new(AtomicBool::new(false));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .expect("open redb file for crash fixture");
            let db = redb::Builder::new()
                .create_with_backend(TestCrashBackend {
                    inner: redb::backends::FileBackend::new(file),
                    dead: Arc::clone(&dead),
                })
                .expect("open redb with crash backend");
            let wtx = db.begin_write().expect("begin crash touch write");
            {
                let mut table = wtx.open_table(CRASH_TOUCH).expect("open crash touch table");
                table.insert("touch", "1").expect("insert crash touch");
            }
            wtx.commit().expect("commit crash touch write");
            dead.store(true, Ordering::SeqCst);
            drop(db);
        }

        fn write_loose_blob(objects_dir: &Path, data: &[u8]) -> ObjectHash {
            let oid = ObjectHash::from_type_and_data(ObjectType::Blob, data);
            let hex = oid.to_string();
            let dir = objects_dir.join(&hex[..2]);
            std::fs::create_dir_all(&dir).expect("create loose object fanout dir");
            let path = dir.join(&hex[2..]);
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(format!("blob {}\0", data.len()).as_bytes())
                .expect("write loose header");
            encoder.write_all(data).expect("write loose payload");
            let compressed = encoder.finish().expect("finish zlib");
            std::fs::write(&path, compressed).expect("write loose object");
            oid
        }

        set_hash_kind(HashKind::Sha1);
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&repo_root).expect("mkdir repo");
        std::fs::create_dir_all(&fake_home).expect("mkdir fake home");
        let dot_libra = repo_root.join(".libra");
        let objects_dir = dot_libra.join("objects");
        std::fs::create_dir_all(&objects_dir).expect("mkdir objects");

        let paths = init_redb_repository_storage(&dot_libra)
            .await
            .expect("init pure-redb storage");
        let _cwd = ChangeDirGuard::enter(&repo_root);
        let _env = EnvVarGuard::set_all(&[
            ("HOME", &fake_home),
            ("CODEX_HOME", &fake_home),
            ("CLAUDE_CONFIG_DIR", &fake_home),
            ("XDG_DATA_HOME", &fake_home),
            ("XDG_CONFIG_HOME", &fake_home),
        ]);

        let repo_id = "repo-redb-agent-cli-test".to_string();
        let cfg = open_repo_config_store_read_write(&dot_libra)
            .await
            .expect("open redb config rw");
        ConfigKv::set_with_store(cfg.as_ref(), "libra.repoid", &repo_id, false)
            .await
            .expect("set libra.repoid");
        ConfigKv::set_with_store(cfg.as_ref(), "core.objectformat", "sha1", false)
            .await
            .expect("set objectformat");
        ConfigKv::set_with_store(cfg.as_ref(), "user.name", "Redb Tester", false)
            .await
            .expect("set user.name");
        ConfigKv::set_with_store(cfg.as_ref(), "user.email", "redb@example.test", false)
            .await
            .expect("set user.email");
        drop(cfg);

        let commit_oid =
            ObjectHash::from_str("2222222222222222222222222222222222222222").expect("valid sha1");
        let git_rw = open_git_store_read_write(&dot_libra)
            .await
            .expect("open redb git rw");
        git_rw
            .update_ref_cas(
                &RefKey::LocalBranch {
                    name: "main".to_string(),
                },
                None,
                Some(&RefTarget::Direct(commit_oid.to_string())),
                &[],
            )
            .await
            .expect("seed refs/heads/main");
        git_rw
            .update_ref_cas(
                &RefKey::LocalHead { worktree_id: None },
                None,
                Some(&RefTarget::Symbolic("main".to_string())),
                &[],
            )
            .await
            .expect("seed HEAD");
        drop(git_rw);

        let now_ms = 1_700_000_000_000i64;
        let ws_id = "ws-redb-cli-01".to_string();
        let state_rw = open_git_state_store_read_write(&dot_libra)
            .await
            .expect("open redb git_state rw");
        let _ = state_rw
            .acquire_workspace_lease(&WorkspaceRecordRow {
                workspace_id: ws_id.clone(),
                repo_id: repo_id.clone(),
                kind: WorkspaceRecordKind::TaskCopy,
                worktree_id: None,
                path: repo_root
                    .canonicalize()
                    .unwrap_or_else(|_| repo_root.clone())
                    .to_string_lossy()
                    .into_owned(),
                owner_kind: WorkspaceRecordOwnerKind::Agent,
                owner_id: Some("owner-01".to_string()),
                task_id: Some("task-01".to_string()),
                session_id: Some("sess-redb-cli-01".to_string()),
                base_commit: Some(commit_oid.to_string()),
                branch: Some("main".to_string()),
                state: WorkspaceRecordState::Active,
                lease_owner: Some("owner-01".to_string()),
                lease_fence: 1,
                lease_expires_at: None,
                created_at: now_ms,
                updated_at: now_ms,
            })
            .await
            .expect("seed workspace_record");
        drop(state_rw);

        let metadata_oid = write_loose_blob(&objects_dir, b"{}");
        let session_id = "sess-redb-cli-01".to_string();
        let checkpoint_id = "cp-redb-cli-01".to_string();
        let cap_rw = open_capture_store_read_write(&dot_libra, RedbRepairContext::WriteCommand)
            .await
            .expect("open redb capture rw");
        cap_rw
            .upsert_session(&AgentSessionRecord {
                session_id: session_id.clone(),
                agent_kind: CaptureAgentKind::ClaudeCode,
                provider_session_id: "prov-redb-cli-01".to_string(),
                thread_id: None,
                state: CaptureSessionState::Active,
                working_dir: repo_root.to_string_lossy().into_owned(),
                worktree_id: Some(String::new()),
                parent_commit: Some(commit_oid.to_string()),
                parent_session_id: None,
                metadata_json: "{}".to_string(),
                redaction_report: "{}".to_string(),
                started_at: now_ms,
                last_event_at: now_ms,
                stopped_at: None,
                schema_version: 2,
                sync_revision: 1,
                repo_id: Some(repo_id.clone()),
                workspace_id: None,
                workspace_fence: None,
                scope_state: CaptureScopeState::Scoped,
            })
            .await
            .expect("seed agent_session");
        cap_rw
            .upsert_checkpoint(&AgentCheckpointRecord {
                checkpoint_id: checkpoint_id.clone(),
                session_id: session_id.clone(),
                parent_checkpoint_id: None,
                scope: CaptureCheckpointScope::Temporary,
                parent_commit: Some(commit_oid.to_string()),
                tree_oid: commit_oid.to_string(),
                metadata_blob_oid: metadata_oid.to_string(),
                traces_commit: commit_oid.to_string(),
                tool_use_id: None,
                subagent_session_id: None,
                description: Some("seed checkpoint".to_string()),
                created_at: now_ms + 1_000,
                sync_revision: 1,
            })
            .await
            .expect("seed agent_checkpoint");
        drop(cap_rw);

        assert!(!dot_libra.join("libra.db").exists());
        assert!(!dot_libra.join("private").join("capture.db").exists());

        let quiet_output = OutputConfig {
            pager: false,
            quiet: true,
            ..OutputConfig::default()
        };
        let json_output = OutputConfig {
            json_format: Some(JsonFormat::Compact),
            pager: false,
            quiet: true,
            ..OutputConfig::default()
        };

        // AC1: Read-only commands succeed on healthy pure-Redb repo.
        crate::command::agent::status::execute_safe(
            crate::command::agent::status::StatusArgs {},
            &quiet_output,
        )
        .await
        .expect("agent status ro");
        crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::List(
                crate::command::agent::session::SessionListArgs {
                    agent: None,
                    state: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent session list ro");
        crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::Show(
                crate::command::agent::session::SessionShowArgs {
                    session_id: session_id.clone(),
                    commit_page: 1,
                    extract_transcript: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent session show ro");
        crate::command::agent::checkpoint::execute_safe(
            crate::command::agent::CheckpointSubcommand::List(
                crate::command::agent::CheckpointListArgs {
                    session: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent checkpoint list ro");
        crate::command::agent::checkpoint::execute_safe(
            crate::command::agent::CheckpointSubcommand::Show(
                crate::command::agent::CheckpointShowArgs {
                    checkpoint_id: checkpoint_id.clone(),
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent checkpoint show ro");
        crate::command::agent::graph::execute_safe(
            crate::command::agent::graph::GraphArgs {
                session: session_id.clone(),
                repo: None,
            },
            &json_output,
        )
        .await
        .expect("agent graph ro");
        crate::command::agent::skill::execute_safe(
            crate::command::agent::skill::SkillSubcommand::Search(
                crate::command::agent::skill::SkillSearchArgs {
                    skill: None,
                    provider: None,
                    session: Some(session_id.clone()),
                    since: None,
                    until: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent skill search ro");
        execute_safe(
            WorkspaceSubcommand::List(WorkspaceListArgs {
                limit: None,
                cursor: None,
                states: Vec::new(),
            }),
            &quiet_output,
        )
        .await
        .expect("agent workspace list ro");
        execute_safe(
            WorkspaceSubcommand::Show(WorkspaceShowArgs {
                workspace_id: ws_id.clone(),
            }),
            &quiet_output,
        )
        .await
        .expect("agent workspace show ro");

        // AC1: Unrecovered capture.redb causes read-only capture commands to fail closed
        // on RepairAborted without upgrading to write or mutating a single byte.
        make_redb_unrecovered(&paths.capture_db_path);
        let cap_unrecovered_bytes =
            std::fs::read(&paths.capture_db_path).expect("read unrecovered capture.redb");
        let status_err = crate::command::agent::status::execute_safe(
            crate::command::agent::status::StatusArgs {},
            &quiet_output,
        )
        .await
        .expect_err("status must refuse unrecovered capture.redb");
        assert_eq!(status_err.stable_code(), StableErrorCode::IoReadFailed);
        let sess_err = crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::List(
                crate::command::agent::session::SessionListArgs {
                    agent: None,
                    state: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect_err("session list must refuse unrecovered capture.redb");
        assert_eq!(sess_err.stable_code(), StableErrorCode::IoReadFailed);
        let cp_err = crate::command::agent::checkpoint::execute_safe(
            crate::command::agent::CheckpointSubcommand::List(
                crate::command::agent::CheckpointListArgs {
                    session: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect_err("checkpoint list must refuse unrecovered capture.redb");
        assert_eq!(cp_err.stable_code(), StableErrorCode::IoReadFailed);
        let graph_err = crate::command::agent::graph::execute_safe(
            crate::command::agent::graph::GraphArgs {
                session: session_id.clone(),
                repo: None,
            },
            &json_output,
        )
        .await
        .expect_err("graph must refuse unrecovered capture.redb");
        assert_eq!(graph_err.stable_code(), StableErrorCode::IoReadFailed);
        let skill_err = crate::command::agent::skill::execute_safe(
            crate::command::agent::skill::SkillSubcommand::Search(
                crate::command::agent::skill::SkillSearchArgs {
                    skill: None,
                    provider: None,
                    session: None,
                    since: None,
                    until: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect_err("skill search must refuse unrecovered capture.redb");
        assert_eq!(skill_err.stable_code(), StableErrorCode::IoReadFailed);
        assert_eq!(
            std::fs::read(&paths.capture_db_path).expect("re-read capture.redb"),
            cap_unrecovered_bytes,
            "read-only commands must never upgrade to write or mutate capture.redb"
        );
        repair_redb_explicit(
            &paths.capture_db_path,
            DatabaseRole::Capture,
            RedbRepairContext::AgentDoctorRepair,
        )
        .expect("repair capture.redb for subsequent checks");

        // AC1: Unrecovered git.redb causes workspace read-only commands to fail closed
        // without upgrading to write or mutating git.redb.
        make_redb_unrecovered(&paths.git_db_path);
        let git_unrecovered_bytes =
            std::fs::read(&paths.git_db_path).expect("read unrecovered git.redb");
        let ws_err = execute_safe(
            WorkspaceSubcommand::List(WorkspaceListArgs {
                limit: None,
                cursor: None,
                states: Vec::new(),
            }),
            &quiet_output,
        )
        .await
        .expect_err("workspace list must refuse unrecovered git.redb");
        assert_eq!(ws_err.stable_code(), StableErrorCode::IoReadFailed);
        assert_eq!(
            std::fs::read(&paths.git_db_path).expect("re-read git.redb"),
            git_unrecovered_bytes,
            "read-only workspace command must never mutate git.redb"
        );
        repair_redb_explicit(
            &paths.git_db_path,
            DatabaseRole::Repository,
            RedbRepairContext::AgentDoctorRepair,
        )
        .expect("repair git.redb for subsequent checks");

        // AC2: RW agent commands and mega2_traces work cleanly on pure-Redb without touching SQLite.
        crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::Stop(
                crate::command::agent::session::SessionStopArgs {
                    session_id: session_id.clone(),
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent session stop rw");
        crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::Resume(
                crate::command::agent::session::SessionResumeArgs {
                    session_id: session_id.clone(),
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent session resume rw");
        crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::Stop(
                crate::command::agent::session::SessionStopArgs {
                    session_id: session_id.clone(),
                },
            ),
            &quiet_output,
        )
        .await
        .expect("agent session stop again before clean");
        crate::command::agent::clean::execute_safe(
            crate::command::agent::CleanArgs {
                all: true,
                gc: false,
                retention_days: None,
                dry_run: false,
            },
            &quiet_output,
        )
        .await
        .expect("agent clean rw");
        crate::command::agent::import::execute_redb_import_empty_for_test(
            &crate::command::agent::import::ImportArgs {
                session: None,
                path: None,
                since: None,
                all: true,
                codex_index_backup: None,
                agent: None,
                limit: 20,
                cursor: None,
                yes: true,
                restore_erased: false,
            },
            &quiet_output,
            &dot_libra,
            &repo_root,
        )
        .await
        .expect("agent import rw on redb");
        let epoch_binding = crate::command::agent::mega2_stream::bind_source_epoch_at(
            &repo_root,
            crate::command::agent::mega2_stream::EpochIntent::Current,
        )
        .await
        .expect("mega2_stream bind_source_epoch_at rw on redb");
        assert!(!epoch_binding.source.is_empty());
        assert!(!epoch_binding.epoch.is_empty());

        crate::command::mega2_traces::publish_archive_ref(
            "mega2-archive-redb",
            &commit_oid.to_string(),
        )
        .await
        .expect("mega2_traces publish_archive_ref rw on redb");
        let git_ro = open_git_store_read_only(&dot_libra)
            .await
            .expect("open git ro after publish_archive_ref");
        assert_eq!(
            git_ro
                .get_ref(&RefKey::LocalBranch {
                    name: "mega2-archive-redb".to_string(),
                })
                .await
                .expect("read published archive ref"),
            Some(RefTarget::Direct(commit_oid.to_string()))
        );
        drop(git_ro);

        assert!(
            !dot_libra.join("libra.db").exists(),
            "RW agent commands and mega2_traces must never create libra.db on redb"
        );
        assert!(
            !dot_libra.join("private").join("capture.db").exists(),
            "RW agent commands must never create capture.db on redb"
        );

        // AC3: Future META_SCHEMA in capture.redb returns LBR-AGENT-048 on agent commands
        // while Git read paths remain healthy.
        let future_ver = MAX_SUPPORTED_READER_VERSION + 7;
        {
            let raw_db = open_redb_writable(
                &paths.capture_db_path,
                DatabaseRole::Capture,
                RedbRepairContext::WriteCommand,
            )
            .expect("raw open capture.redb for future schema");
            let wtx = raw_db.begin_write().expect("begin future schema write");
            {
                let mut meta = wtx
                    .open_table(META_SCHEMA_TABLE)
                    .expect("open META_SCHEMA_TABLE");
                let encoded = encode_versioned(
                    META_SCHEMA_CODEC_VERSION,
                    &MetaSchemaRecord {
                        role: MetaSchemaRole::Capture,
                        format_version: future_ver,
                        min_reader_version: future_ver,
                    },
                )
                .expect("encode future META_SCHEMA");
                meta.insert(META_SCHEMA_SINGLETON_KEY, encoded.as_slice())
                    .expect("write future META_SCHEMA");
            }
            wtx.commit().expect("commit future META_SCHEMA");
        }

        let future_status_err = crate::command::agent::status::execute_safe(
            crate::command::agent::status::StatusArgs {},
            &quiet_output,
        )
        .await
        .expect_err("status must fail closed on future capture META_SCHEMA");
        assert_eq!(
            future_status_err.stable_code(),
            StableErrorCode::CaptureSchemaTooNew
        );
        let future_session_err = crate::command::agent::session::execute_safe(
            crate::command::agent::session::SessionSubcommand::List(
                crate::command::agent::session::SessionListArgs {
                    agent: None,
                    state: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect_err("session list must fail closed on future capture META_SCHEMA");
        assert_eq!(
            future_session_err.stable_code(),
            StableErrorCode::CaptureSchemaTooNew
        );
        let future_cp_err = crate::command::agent::checkpoint::execute_safe(
            crate::command::agent::CheckpointSubcommand::List(
                crate::command::agent::CheckpointListArgs {
                    session: None,
                    limit: None,
                    cursor: None,
                },
            ),
            &quiet_output,
        )
        .await
        .expect_err("checkpoint list must fail closed on future capture META_SCHEMA");
        assert_eq!(
            future_cp_err.stable_code(),
            StableErrorCode::CaptureSchemaTooNew
        );
        let future_clean_err = crate::command::agent::clean::execute_safe(
            crate::command::agent::CleanArgs {
                all: true,
                gc: false,
                retention_days: None,
                dry_run: false,
            },
            &quiet_output,
        )
        .await
        .expect_err("clean must fail closed on future capture META_SCHEMA");
        assert_eq!(
            future_clean_err.stable_code(),
            StableErrorCode::CaptureSchemaTooNew
        );

        // Git read paths remain completely healthy despite future capture.redb schema!
        let head = Head::current().await;
        assert!(matches!(head, Head::Branch(ref branch) if branch == "main"));
        let git_ro = open_git_store_read_only(&dot_libra)
            .await
            .expect("git.redb read-only must remain healthy");
        assert_eq!(
            git_ro
                .get_ref(&RefKey::LocalBranch {
                    name: "main".to_string(),
                })
                .await
                .expect("read main ref"),
            Some(RefTarget::Direct(commit_oid.to_string()))
        );
        execute_safe(
            WorkspaceSubcommand::List(WorkspaceListArgs {
                limit: None,
                cursor: None,
                states: Vec::new(),
            }),
            &quiet_output,
        )
        .await
        .expect("workspace list (git.redb only) must remain healthy");
    }
}
