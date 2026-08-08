use std::path::Path;

use sea_orm::DatabaseConnection;

use crate::{
    internal::{
        ai::{
            automation::{
                config::AutomationConfig,
                events::{AutomationError, AutomationRunResult, AutomationRuntimeEvent},
                executor::AutomationExecutor,
                history::AutomationHistory,
                scheduler::AutomationScheduler,
            },
            hooks::{HookEvent, LifecycleEventKind},
        },
        db,
    },
    utils::util,
};

/// W0 §C.4.1.1 (plan-20260714): automation configuration is not
/// worktree-aware until the W4 unified resolver — in a linked worktree
/// `automations.toml` would be read from the local gitdir, where no
/// repository configuration lives, so every dispatch would silently no-op
/// against an empty rule set. That silent skip is expressly forbidden:
/// dispatch is DISABLED with one user-visible warning instead. Returns true
/// when the caller must skip dispatching — including when the worktree's
/// scope CANNOT be resolved (corrupt metadata), because an unknown scope
/// must not run a rule set of unknown ownership.
///
/// "One warning per command" holds by construction with NO process latch:
/// every guarded entry (the commit/add/branch/switch/push VCS handlers and
/// the `agent hooks` provider ingest) dispatches exactly ONE event per
/// command, so warning on every disabled dispatch is once-per-command in a
/// one-shot CLI *and* in a long-lived host process — a latch would make a
/// host's second linked command skip silently, the exact forbidden outcome.
fn linked_scope_dispatch_disabled(working_dir: &Path) -> bool {
    // TRI-STATE, fail-closed: `worktree_id_for_base` collapses every
    // resolution ERROR to `None`, which reads as "main" — so a corrupt or
    // dangling `commondir` would let a linked worktree run its LOCAL rule
    // set. Resolve the gitdir explicitly instead: only a definitive
    // not-a-repository answer permits dispatch; a resolution failure means
    // the scope is unknown, and unknown scope must not run automations.
    let linked_or_unknown = match util::try_get_worktree_gitdir(Some(working_dir.to_path_buf())) {
        Ok(gitdir) => util::worktree_id_for_gitdir(&gitdir).is_some(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    if !linked_or_unknown {
        return false;
    }
    eprintln!(
        "warning: automation dispatch is disabled in linked worktrees until the \
         unified Code/Agent config resolver lands; run from the main worktree to \
         trigger automations (plan-20260714 W0)"
    );
    true
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
    if linked_scope_dispatch_disabled(working_dir) {
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
    if linked_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }

    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    if !has_matching_hook_rule(&config, hook_event) {
        return Ok(Vec::new());
    }

    let conn = db::get_db_conn_instance_for_path(&storage_path.join(util::DATABASE))
        .await
        .map_err(|error| AutomationError::Database(error.to_string()))?;
    dispatch_hook_event_with_config_to_history(working_dir, &conn, config, hook_event).await
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
    if linked_scope_dispatch_disabled(&working_dir) {
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
    if linked_scope_dispatch_disabled(working_dir) {
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
    if linked_scope_dispatch_disabled(working_dir) {
        return Ok(Vec::new());
    }
    let config = AutomationConfig::load_from_working_dir(working_dir)?;
    config.validate()?;
    if !has_matching_vcs_rule(&config, event) {
        return Ok(Vec::new());
    }

    let conn = db::get_db_conn_instance_for_path(&storage_path.join(util::DATABASE))
        .await
        .map_err(|error| AutomationError::Database(error.to_string()))?;
    dispatch_vcs_event_with_config_to_history(working_dir, &conn, config, event).await
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
    /// matching post_commit rule in its LOCAL gitdir.
    fn linked_fixture_with_rule() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main_gitdir = tmp.path().join("main").join(".libra");
        std::fs::create_dir_all(&main_gitdir).expect("main gitdir");
        std::fs::write(main_gitdir.join("libra.db"), b"").expect("db marker");
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
            gitdir.join("automations.toml"),
            br#"
            [[rules]]
            id = "commit_summary"
            trigger = { kind = "vcs", event = "post_commit" }
            action = { kind = "prompt", prompt = "x" }
            "#,
        )
        .expect("rules");
        tmp
    }

    /// W0 §C.4.1.1: the DIRECT public dispatch APIs are guarded too — a
    /// linked working dir returns empty WITHOUT touching config or storage.
    /// The nonexistent storage path is the discriminator: an unguarded call
    /// would load the matching rule and then fail connecting to the
    /// database there.
    #[tokio::test]
    #[serial_test::serial]
    async fn direct_repo_vcs_dispatch_is_guarded_in_linked_scope() {
        let tmp = linked_fixture_with_rule();
        let results = dispatch_repo_vcs_event_to_history(
            &tmp.path().join("wt"),
            &tmp.path().join("no-such-storage"),
            "post_commit",
        )
        .await
        .expect("guarded dispatch returns empty instead of a storage error");
        assert!(
            results.is_empty(),
            "linked dispatch must run nothing: {results:?}"
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

    /// Same guard on the conn-taking direct API: nothing runs, nothing is
    /// appended (an unguarded call would run the matching prompt rule).
    #[tokio::test]
    #[serial_test::serial]
    async fn direct_vcs_dispatch_is_guarded_in_linked_scope() {
        let tmp = linked_fixture_with_rule();
        let db_path = tmp.path().join("test.db");
        std::fs::write(&db_path, b"").expect("touch db file");
        let conn = crate::internal::db::establish_connection(&db_path.to_string_lossy())
            .await
            .expect("temp db");
        let result = dispatch_vcs_event_to_history(&tmp.path().join("wt"), &conn, "post_commit")
            .await
            .expect("guarded dispatch returns empty");
        assert!(
            result.is_empty(),
            "linked dispatch must run nothing: {result:?}"
        );
    }
}
