//! Pure decision for AgentTraces ingest.
//!
//! `decide` maps a [`LifecycleEventKind`] to the SQLite `agent_session.state`
//! string and the checkpoint write class. It performs no I/O and does not
//! read paths, prompts, or transcripts.

use super::lifecycle::LifecycleEventKind;

/// Which checkpoint writer, if any, AgentTraces ingest should call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckpointWrite {
    /// Leave checkpoint tables untouched.
    None,
    /// Existing committed-branch writer (`write_committed_checkpoint`).
    Committed,
    /// Existing subagent-boundary writer (`write_subagent_checkpoint`).
    SubagentBoundary,
}

/// One row of the AgentTraces ingest decision table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CaptureDecision {
    /// Value stored in `agent_session.state`.
    pub session_state: &'static str,
    /// Checkpoint class. `repo_path == None` still writes nothing.
    pub checkpoint: CheckpointWrite,
}

/// Return the AgentTraces state string and checkpoint class for `kind`.
///
/// The match is exhaustive inside this crate. A new [`LifecycleEventKind`]
/// fails compilation here until this table is updated.
pub(crate) fn decide(kind: LifecycleEventKind) -> CaptureDecision {
    let (session_state, checkpoint) = match kind {
        LifecycleEventKind::SessionStart => ("active", CheckpointWrite::None),
        LifecycleEventKind::TurnStart => ("active", CheckpointWrite::None),
        LifecycleEventKind::ToolUse => ("active", CheckpointWrite::None),
        LifecycleEventKind::ModelUpdate => ("active", CheckpointWrite::None),
        LifecycleEventKind::Compaction => ("condensed", CheckpointWrite::None),
        LifecycleEventKind::CompactionCompleted => ("active", CheckpointWrite::None),
        LifecycleEventKind::PermissionRequest => ("active", CheckpointWrite::None),
        LifecycleEventKind::SourceEnabled => ("active", CheckpointWrite::None),
        LifecycleEventKind::SourceDisabled => ("active", CheckpointWrite::None),
        LifecycleEventKind::TurnEnd => ("active", CheckpointWrite::Committed),
        LifecycleEventKind::SessionEnd => ("stopped", CheckpointWrite::Committed),
        LifecycleEventKind::SubagentStart => ("active", CheckpointWrite::SubagentBoundary),
        LifecycleEventKind::SubagentEnd => ("active", CheckpointWrite::SubagentBoundary),
    };
    CaptureDecision {
        session_state,
        checkpoint,
    }
}

#[cfg(test)]
mod tests {
    use super::{super::lifecycle::LifecycleEventKind, CheckpointWrite, decide};

    #[test]
    fn decide_matches_adr_matrix() {
        let rows = [
            (
                LifecycleEventKind::SessionStart,
                "active",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::TurnStart,
                "active",
                CheckpointWrite::None,
            ),
            (LifecycleEventKind::ToolUse, "active", CheckpointWrite::None),
            (
                LifecycleEventKind::ModelUpdate,
                "active",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::Compaction,
                "condensed",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::CompactionCompleted,
                "active",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::PermissionRequest,
                "active",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::SourceEnabled,
                "active",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::SourceDisabled,
                "active",
                CheckpointWrite::None,
            ),
            (
                LifecycleEventKind::TurnEnd,
                "active",
                CheckpointWrite::Committed,
            ),
            (
                LifecycleEventKind::SessionEnd,
                "stopped",
                CheckpointWrite::Committed,
            ),
            (
                LifecycleEventKind::SubagentStart,
                "active",
                CheckpointWrite::SubagentBoundary,
            ),
            (
                LifecycleEventKind::SubagentEnd,
                "active",
                CheckpointWrite::SubagentBoundary,
            ),
        ];
        assert_eq!(rows.len(), 13);
        for (kind, state, checkpoint) in rows {
            let decision = decide(kind);
            assert_eq!(decision.session_state, state, "{kind}");
            assert_eq!(decision.checkpoint, checkpoint, "{kind}");
        }
    }

    #[test]
    fn runtime_checkpoint_kind_matches_are_gone() {
        let src = include_str!("runtime.rs");
        let four = "\
        LifecycleEventKind::SessionEnd
            | LifecycleEventKind::TurnEnd
            | LifecycleEventKind::SubagentStart
            | LifecycleEventKind::SubagentEnd";
        let subagent_line =
            "            LifecycleEventKind::SubagentStart | LifecycleEventKind::SubagentEnd";
        assert!(
            !src.contains(four),
            "checkpoint-kind matches! must be replaced by decide"
        );
        assert!(
            !src.contains(subagent_line),
            "subagent checkpoint split must use decide().checkpoint"
        );
        assert!(
            !src.contains("let new_state = match event.kind"),
            "state match must be replaced by decide().session_state"
        );
        assert!(src.contains("decide(event.kind).session_state"));
        assert!(src.contains("decide(event.kind).checkpoint"));
    }

    #[test]
    fn runtime_keeps_owner_exemption_and_stopped_at() {
        let src = include_str!("runtime.rs");
        let owner = concat!(
            "        LifecycleEventKind::SessionStart | ",
            "LifecycleEventKind::TurnStart"
        );
        let stopped_at = concat!(
            "        matches!(event.kind, ",
            "LifecycleEventKind::SessionEnd).then_some(now);"
        );
        assert!(
            src.contains(owner),
            "SessionStart/TurnStart owner exemption must stay in runtime.rs"
        );
        assert!(
            src.contains(stopped_at),
            "stopped_at assignment must stay in runtime.rs"
        );
    }

    #[test]
    fn stale_session_end_only_comment_is_gone() {
        let src = include_str!("runtime.rs");
        for stale in [
            "commit on `SessionEnd` (see [`ingest_agent_traces_payload`])",
            "On `SessionEnd` it also writes an E4-libra checkpoint",
            "checkpoint commit step on `SessionEnd` and only persists",
        ] {
            assert!(
                !src.contains(stale),
                "AgentTraces docs must not say checkpoints are SessionEnd-only: {stale}"
            );
        }
        for expected in [
            "commits on `TurnEnd`, `SessionEnd`, and subagent boundaries",
            "On `TurnEnd`, `SessionEnd`, and subagent boundaries it",
            "for `TurnEnd`, `SessionEnd`, and subagent-boundary\n/// events",
        ] {
            assert!(
                src.contains(expected),
                "AgentTraces docs must name every checkpoint trigger: {expected}"
            );
        }
    }

    #[tokio::test]
    async fn no_checkpoint_when_repo_path_missing() {
        use sea_orm::{ConnectionTrait, Statement};

        use super::super::{
            provider::ProviderHookCommand,
            providers::claude_provider,
            runtime::{self, tests as runtime_tests},
        };

        let (_dir, conn) = runtime_tests::ingest_fresh_conn().await;
        runtime::ingest_agent_traces_payload(
            &runtime_tests::ingest_envelope("SessionStart", "S-no-repo", serde_json::json!({})),
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("session start without a repo still upserts");
        runtime::ingest_agent_traces_payload(
            &runtime_tests::ingest_envelope("Stop", "S-no-repo", serde_json::json!({})),
            ProviderHookCommand::Stop,
            LifecycleEventKind::TurnEnd,
            claude_provider(),
            &conn,
            None,
        )
        .await
        .expect("turn end without a repo still upserts");

        let backend = conn.get_database_backend();
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS n FROM agent_checkpoint",
                [],
            ))
            .await
            .expect("checkpoint count")
            .expect("count row");
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
    }
}
