use std::path::Path;

use sea_orm::DatabaseConnection;

use crate::{
    internal::ai::{
        automation::{
            config::AutomationConfig,
            events::{AutomationError, AutomationRunResult, AutomationRuntimeEvent},
            executor::AutomationExecutor,
            history::AutomationHistory,
            scheduler::AutomationScheduler,
        },
        hooks::{HookEvent, LifecycleEventKind},
    },
    utils::util,
};

/// W4-08: healthy main and registered linked worktrees dispatch through the
/// resolver. A damaged/unreadable worktree or an unregistered `worktree_id`
/// still fail-closes — unknown scope must not run a rule set of unknown
/// ownership.
fn unknown_scope_dispatch_disabled(working_dir: &Path) -> bool {
    use crate::internal::worktree_scope::{RequestScope, WorktreeScope};

    let request = match RequestScope::try_resolve(working_dir.to_path_buf()) {
        Ok(Some(request)) => request,
        Ok(None) => return false,
        Err(_) => {
            eprintln!(
                "warning: automation dispatch is disabled because the worktree \
                 scope could not be resolved; refusing to run automations \
                 against an unknown configuration owner"
            );
            return true;
        }
    };
    let WorktreeScope::Linked(id) = &request.scope else {
        return false;
    };
    match crate::command::worktree::registry_knows_linked_worktree_in_storage(
        &request.storage,
        id,
        Some(&request.worktree_root),
    ) {
        Some(true) => false,
        _ => {
            eprintln!(
                "warning: automation dispatch is disabled because this linked \
                 worktree's identity is missing from the worktree registry; \
                 refusing to run automations against an unknown configuration \
                 owner"
            );
            true
        }
    }
}

/// Dispatch a normalized hook lifecycle event through automation rules and
/// persist every matched rule result.
pub async fn dispatch_hook_lifecycle_event_to_history(
    working_dir: &Path,
    conn: &DatabaseConnection,
    event_kind: LifecycleEventKind,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    let Some(hook_event) = automation_hook_event(event_kind) else {
        return Ok(Vec::new());
    };
    if unknown_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }

    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    dispatch_hook_event_with_config_to_history(working_dir, conn, config, hook_event).await
}

/// Repository-oriented hook bridge used by provider hook ingestion. It avoids
/// touching the database when there is no matching automation work to do.
pub async fn dispatch_repo_hook_lifecycle_event_to_history(
    working_dir: &Path,
    storage_path: &Path,
    event_kind: LifecycleEventKind,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    let Some(hook_event) = automation_hook_event(event_kind) else {
        return Ok(Vec::new());
    };
    if unknown_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }

    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    if !has_matching_hook_rule(&config, hook_event) {
        return Ok(Vec::new());
    }

    match crate::storage::runtime::detect_repo_backend(storage_path)
        .map_err(|error| AutomationError::Database(error.to_string()))?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_state_store_read_write(storage_path)
                    .await
                    .map_err(|error| AutomationError::Database(error.to_string()))?;
                dispatch_hook_event_with_config_to_history_with_store(
                    working_dir,
                    store.as_ref(),
                    config,
                    hook_event,
                )
                .await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = (working_dir, config, hook_event);
                Err(AutomationError::Database(
                    "this Libra binary was compiled without `storage-redb`".to_string(),
                ))
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = crate::internal::db::get_db_conn_instance_for_path(
                    &storage_path.join(util::DATABASE),
                )
                .await
                .map_err(|error| AutomationError::Database(error.to_string()))?;
                dispatch_hook_event_with_config_to_history(working_dir, &conn, config, hook_event)
                    .await
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = (working_dir, config, hook_event);
                Err(AutomationError::Database(
                    "this Libra binary was compiled without `storage-sqlite`".to_string(),
                ))
            }
        }
    }
}

/// Dispatch a normalized hook lifecycle event through automation rules and
/// persist every matched rule result via [`crate::storage::GitStateStore`].
pub async fn dispatch_hook_lifecycle_event_to_history_with_store(
    working_dir: &Path,
    store: &dyn crate::storage::GitStateStore,
    event_kind: LifecycleEventKind,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    let Some(hook_event) = automation_hook_event(event_kind) else {
        return Ok(Vec::new());
    };
    if unknown_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }

    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    dispatch_hook_event_with_config_to_history_with_store(working_dir, store, config, hook_event)
        .await
}

/// Best-effort VCS event bridge for top-level Libra VCS commands.
///
/// Automation must never make a successful VCS command fail, so this helper logs
/// dispatch problems and returns `()`.
pub async fn dispatch_current_repo_vcs_event_to_history(event: &'static str) {
    let working_dir = match util::try_working_dir() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(
                target: "libra::ai::automation",
                event,
                error = %error,
                "failed to resolve working directory for automation VCS event"
            );
            return;
        }
    };
    if unknown_scope_dispatch_disabled(&working_dir) {
        return;
    }
    let storage_path = match util::try_get_storage_path(Some(working_dir.clone())) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(
                target: "libra::ai::automation",
                event,
                working_dir = %working_dir.display(),
                error = %error,
                "failed to resolve storage path for automation VCS event"
            );
            return;
        }
    };

    if let Err(error) = dispatch_repo_vcs_event_to_history(&working_dir, &storage_path, event).await
    {
        tracing::warn!(
            target: "libra::ai::automation",
            event,
            working_dir = %working_dir.display(),
            error = %error,
            "failed to dispatch automation VCS event"
        );
    }
}

pub async fn dispatch_vcs_event_to_history(
    working_dir: &Path,
    conn: &DatabaseConnection,
    event: &str,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if unknown_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }
    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    dispatch_vcs_event_with_config_to_history(working_dir, conn, config, event).await
}

pub async fn dispatch_repo_vcs_event_to_history(
    working_dir: &Path,
    storage_path: &Path,
    event: &str,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if unknown_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }
    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    if !has_matching_vcs_rule(&config, event) {
        return Ok(Vec::new());
    }

    match crate::storage::runtime::detect_repo_backend(storage_path)
        .map_err(|error| AutomationError::Database(error.to_string()))?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_git_state_store_read_write(storage_path)
                    .await
                    .map_err(|error| AutomationError::Database(error.to_string()))?;
                dispatch_vcs_event_with_config_to_history_with_store(
                    working_dir,
                    store.as_ref(),
                    config,
                    event,
                )
                .await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = (working_dir, config, event);
                Err(AutomationError::Database(
                    "this Libra binary was compiled without `storage-redb`".to_string(),
                ))
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = crate::internal::db::get_db_conn_instance_for_path(
                    &storage_path.join(util::DATABASE),
                )
                .await
                .map_err(|error| AutomationError::Database(error.to_string()))?;
                dispatch_vcs_event_with_config_to_history(working_dir, &conn, config, event).await
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = (working_dir, config, event);
                Err(AutomationError::Database(
                    "this Libra binary was compiled without `storage-sqlite`".to_string(),
                ))
            }
        }
    }
}

/// Dispatch a VCS event through automation rules and persist every matched rule
/// result via [`crate::storage::GitStateStore`].
pub async fn dispatch_vcs_event_to_history_with_store(
    working_dir: &Path,
    store: &dyn crate::storage::GitStateStore,
    event: &str,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if unknown_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }
    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    dispatch_vcs_event_with_config_to_history_with_store(working_dir, store, config, event).await
}

async fn dispatch_hook_event_with_config_to_history(
    working_dir: &Path,
    conn: &DatabaseConnection,
    config: AutomationConfig,
    hook_event: HookEvent,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if !has_matching_hook_rule(&config, hook_event) {
        return Ok(Vec::new());
    }

    let scheduler = AutomationScheduler::new(config);
    let executor = AutomationExecutor::live(working_dir);
    let results = scheduler
        .run_event(AutomationRuntimeEvent::hook(hook_event), &executor)
        .await?;
    for result in &results {
        AutomationHistory::append(conn, result).await?;
    }
    Ok(results)
}

async fn dispatch_hook_event_with_config_to_history_with_store(
    working_dir: &Path,
    store: &dyn crate::storage::GitStateStore,
    config: AutomationConfig,
    hook_event: HookEvent,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if !has_matching_hook_rule(&config, hook_event) {
        return Ok(Vec::new());
    }

    let scheduler = AutomationScheduler::new(config);
    let executor = AutomationExecutor::live(working_dir);
    let results = scheduler
        .run_event(AutomationRuntimeEvent::hook(hook_event), &executor)
        .await?;
    for result in &results {
        AutomationHistory::append_with_store(store, result).await?;
    }
    Ok(results)
}

async fn dispatch_vcs_event_with_config_to_history(
    working_dir: &Path,
    conn: &DatabaseConnection,
    config: AutomationConfig,
    event: &str,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if !has_matching_vcs_rule(&config, event) {
        return Ok(Vec::new());
    }

    let scheduler = AutomationScheduler::new(config);
    let executor = AutomationExecutor::live(working_dir);
    let results = scheduler
        .run_event(AutomationRuntimeEvent::vcs(event), &executor)
        .await?;
    for result in &results {
        AutomationHistory::append(conn, result).await?;
    }
    Ok(results)
}

async fn dispatch_vcs_event_with_config_to_history_with_store(
    working_dir: &Path,
    store: &dyn crate::storage::GitStateStore,
    config: AutomationConfig,
    event: &str,
) -> Result<Vec<AutomationRunResult>, AutomationError> {
    if !has_matching_vcs_rule(&config, event) {
        return Ok(Vec::new());
    }

    let scheduler = AutomationScheduler::new(config);
    let executor = AutomationExecutor::live(working_dir);
    let results = scheduler
        .run_event(AutomationRuntimeEvent::vcs(event), &executor)
        .await?;
    for result in &results {
        AutomationHistory::append_with_store(store, result).await?;
    }
    Ok(results)
}

fn has_matching_hook_rule(config: &AutomationConfig, hook_event: HookEvent) -> bool {
    config.rules.iter().any(|rule| {
        rule.enabled
            && matches!(
                &rule.trigger,
                crate::internal::ai::automation::config::AutomationTrigger::Hook { event }
                    if *event == hook_event
            )
    })
}

fn has_matching_vcs_rule(config: &AutomationConfig, vcs_event: &str) -> bool {
    config.rules.iter().any(|rule| {
        rule.enabled
            && matches!(
                &rule.trigger,
                crate::internal::ai::automation::config::AutomationTrigger::Vcs { event }
                    if event == vcs_event
            )
    })
}

fn automation_hook_event(event_kind: LifecycleEventKind) -> Option<HookEvent> {
    match event_kind {
        LifecycleEventKind::SessionStart => Some(HookEvent::SessionStart),
        LifecycleEventKind::SessionEnd => Some(HookEvent::SessionEnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A linked-worktree fixture whose commondir points at a REAL main
    /// storage (the resolver fail-closes on dangling targets), carrying a
    /// matching post_commit rule in the repository layer.
    fn linked_fixture_with_rule() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main_gitdir = tmp.path().join("main").join(".libra");
        std::fs::create_dir_all(&main_gitdir).expect("main gitdir");
        std::fs::write(main_gitdir.join("libra.db"), b"").expect("db marker");
        std::fs::write(
            main_gitdir.join("automations.toml"),
            br#"
            [[rules]]
            id = "commit_summary"
            trigger = { kind = "vcs", event = "post_commit" }
            action = { kind = "prompt", prompt = "x" }
            "#,
        )
        .expect("repository rules");
        let wt = tmp.path().join("wt");
        let gitdir = wt.join(".libra");
        std::fs::create_dir_all(&gitdir).expect("linked gitdir");
        std::fs::write(
            gitdir.join("commondir"),
            main_gitdir.to_string_lossy().as_bytes(),
        )
        .expect("commondir");
        std::fs::write(gitdir.join("worktree_id"), b"wt-test-1234").expect("worktree id");
        std::fs::write(
            main_gitdir.join("worktrees.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 3,
                "linked_history": "existed",
                "epoch_counter": 1,
                "entries": [
                    {
                        "path": tmp.path().join("main").to_string_lossy(),
                        "is_main": true,
                        "locked": false
                    },
                    {
                        "path": wt.to_string_lossy(),
                        "is_main": false,
                        "locked": false,
                        "worktree_id": "wt-test-1234",
                        "epoch": 1
                    }
                ]
            }))
            .expect("registry json"),
        )
        .expect("worktrees.json");
        tmp
    }

    /// An unregistered / forged linked identity must not dispatch even when
    /// the gitdir itself is readable.
    #[tokio::test]
    #[serial_test::serial]
    async fn direct_repo_vcs_dispatch_is_disabled_for_unregistered_identity() {
        let tmp = linked_fixture_with_rule();
        let wt = tmp.path().join("wt");
        std::fs::write(wt.join(".libra").join("worktree_id"), b"forged-id\n")
            .expect("forge worktree id");
        let results = dispatch_repo_vcs_event_to_history(
            &wt,
            &tmp.path().join("no-such-storage"),
            "post_commit",
        )
        .await
        .expect("unregistered identity returns empty instead of running");
        assert!(
            results.is_empty(),
            "unregistered linked dispatch must run nothing: {results:?}"
        );
    }

    /// W4-08: a healthy linked working dir reaches config + storage instead
    /// of returning empty. The nonexistent storage path is the discriminator.
    #[tokio::test]
    #[serial_test::serial]
    async fn direct_repo_vcs_dispatch_runs_in_linked_scope() {
        let tmp = linked_fixture_with_rule();
        let err = dispatch_repo_vcs_event_to_history(
            &tmp.path().join("wt"),
            &tmp.path().join("no-such-storage"),
            "post_commit",
        )
        .await
        .expect_err("linked dispatch must reach storage via the resolver");
        let message = err.to_string();
        assert!(
            message.to_lowercase().contains("database")
                || message.to_lowercase().contains("storage")
                || message.to_lowercase().contains("no such")
                || message.to_lowercase().contains("not found"),
            "expected a storage/database failure after enablement, got: {message}"
        );
    }

    /// A CORRUPT linked worktree (commondir pointing nowhere) must also be
    /// refused by both direct APIs: scope resolution fails, and an unknown
    /// scope must never run the local rule set. Before the tri-state check,
    /// the error collapsed to `None` == "main" and the rules ran.
    #[tokio::test]
    #[serial_test::serial]
    async fn direct_dispatch_is_guarded_when_scope_resolution_fails() {
        let tmp = linked_fixture_with_rule();
        let wt = tmp.path().join("wt");
        // Break the pointer: the target no longer exists.
        std::fs::write(
            wt.join(".libra").join("commondir"),
            tmp.path().join("gone").to_string_lossy().as_bytes(),
        )
        .expect("corrupt the commondir");

        let repo_form = dispatch_repo_vcs_event_to_history(
            &wt,
            &tmp.path().join("no-such-storage"),
            "post_commit",
        )
        .await
        .expect("corrupt scope is refused, not run");
        assert!(repo_form.is_empty(), "corrupt scope must run nothing");

        // The other corruption shape: the pointer FILE is deleted while the
        // linked worktree's other files remain. Repository discovery must
        // still stop at this gitdir (its `worktree_id` marks it) instead of
        // answering "no repository" and letting the local rule set run.
        std::fs::remove_file(wt.join(".libra").join("commondir")).expect("delete the pointer");
        let deleted_pointer = dispatch_repo_vcs_event_to_history(
            &wt,
            &tmp.path().join("no-such-storage"),
            "post_commit",
        )
        .await
        .expect("a pointer-less linked gitdir is refused, not run");
        assert!(
            deleted_pointer.is_empty(),
            "a linked worktree whose commondir was deleted must run nothing"
        );

        let db_path = tmp.path().join("corrupt.db");
        std::fs::write(&db_path, b"").expect("touch db file");
        let conn = crate::internal::db::establish_connection(&db_path.to_string_lossy())
            .await
            .expect("temp db");
        let conn_form = dispatch_vcs_event_to_history(&wt, &conn, "post_commit")
            .await
            .expect("corrupt scope is refused, not run");
        assert!(conn_form.is_empty(), "corrupt scope must run nothing");
    }

    /// W4-08: the conn-taking direct API runs the matching prompt rule in a
    /// healthy linked scope (corrupt scope stays fail-closed above).
    #[tokio::test]
    #[serial_test::serial]
    async fn direct_vcs_dispatch_runs_in_linked_scope() {
        let tmp = linked_fixture_with_rule();
        let db_path = tmp.path().join("test.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
        let conn = sea_orm::Database::connect(&db_url).await.expect("temp db");
        crate::internal::db::migration::run_builtin_migrations(&conn)
            .await
            .expect("migrations");
        let result = dispatch_vcs_event_to_history(&tmp.path().join("wt"), &conn, "post_commit")
            .await
            .expect("linked dispatch runs");
        assert!(
            result.iter().any(|row| row.rule_id == "commit_summary"),
            "linked dispatch must run the repository rule: {result:?}"
        );
    }
}
