//! Guard `docs/development/tracing/agent.md` against stale implementation claims.
//!
//! The Agent plan is an active backlog document. Several sections are historical
//! by design, but the current-risk table must not claim removed provider
//! surfaces still exist after the source and CLI migration tests have closed
//! them.

const AGENT_DOC: &str = include_str!("../../docs/development/tracing/agent.md");
const CODE_COMMAND: &str = include_str!("../../src/command/code.rs");

#[test]
fn agent_doc_keeps_claudecode_marked_removed_not_active() {
    assert!(
        !CODE_COMMAND.contains("CodeProvider::Claudecode"),
        "src/command/code.rs must not reintroduce the removed claudecode provider variant",
    );

    for forbidden in [
        "`claudecode` provider 仍存在",
        "code.rs` 仍有 Claudecode provider",
    ] {
        assert!(
            !AGENT_DOC.contains(forbidden),
            "docs/development/tracing/agent.md must not keep stale claudecode-active claim: {forbidden}",
        );
    }

    assert!(
        AGENT_DOC.contains("claudecode 硬删除"),
        "agent.md should continue to describe claudecode as a completed hard-delete wave",
    );
    assert!(
        AGENT_DOC.contains("`src/internal/ai/claudecode/` 不存在"),
        "agent.md should keep the source-grounded removal evidence",
    );
}

#[test]
fn agent_doc_tracks_schema_versioning_and_retention_policy() {
    for required in [
        "schema_version",
        "agent.retention.transcript_days",
        "agent.retention.stderr_days",
        "agent.retention.findings_days",
        "agent.max_transcript_read_bytes",
        "agent_audit_log",
        "append-only",
        "--allow-raw --raw",
        "LBR-AGENT-013",
        "content_hash.txt",
    ] {
        assert!(
            AGENT_DOC.contains(required),
            "agent.md must keep the public schema/retention/raw-export contract visible: {required}",
        );
    }

    for forbidden in [
        "`agent_lifecycle_event_test`：规划 target",
        "`agent_review_workflow_test`：规划 target",
        "`agent_investigate_workflow_test`：规划 target",
        "`agent_audit_log_test`：规划 target",
        "当前命令层无 review/investigate",
        "Codex/OpenCode 尚无 HookProvider",
        "libra agent add codex --force",
    ] {
        assert!(
            !AGENT_DOC.contains(forbidden),
            "agent.md must not keep stale AG-24 closeout wording: {forbidden}",
        );
    }
}

#[test]
fn agent_doc_tracks_code_agent_runtime_source_of_truth() {
    assert!(
        AGENT_DOC.contains("../internal/code-agent-runtime.md"),
        "agent.md should link the current internal runtime plan source of truth",
    );

    for forbidden_link in [
        "](../agent.md)",
        "](../web-only.md)",
        "](../code-agent-runtime.md)",
        "](../../development/agent.md)",
        "](../../development/web-only.md)",
        "](../../development/code-agent-runtime.md)",
    ] {
        assert!(
            !AGENT_DOC.contains(forbidden_link),
            "agent.md must not reintroduce stale internal-plan markdown links: {forbidden_link}",
        );
    }
}
