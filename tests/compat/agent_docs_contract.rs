//! Guard `docs/development/tracing/agent.md` against stale implementation claims.
//!
//! The Agent plan is an active backlog document. Several sections are historical
//! by design, but the current-risk table must not claim removed provider
//! surfaces still exist after the source and CLI migration tests have closed
//! them.

const AGENT_DOC: &str = include_str!("../../docs/development/tracing/agent.md");
/// The USER-facing pages. The tracing doc is not the only place the
/// tombstone story is told, and a guard that reads one file cannot stop the
/// other two from contradicting it — which is exactly how they came to say
/// `restore` still resurrects erased sessions after it no longer did.
const AGENT_CMD_DOC_EN: &str = include_str!("../../docs/commands/agent.md");
const AGENT_CMD_DOC_ZH: &str = include_str!("../../docs/commands/zh-CN/agent.md");
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
fn agent_doc_declares_cloud_tombstone_deferred() {
    // A0-10 doc-guard, FLIPPED with plan-20260714 PD-03: session-erasure
    // tombstones now PROPAGATE — `libra cloud sync` publishes
    // `agent_import_tombstone` to D1 under the generation fence and
    // cascade-deletes the erased session's mirror rows; `libra cloud
    // restore` is tombstone-first (an erased session never restores) and
    // persists the fences locally. The only remaining deferral is R2
    // physical payload deletion. The doc must state the new facts and
    // may no longer claim restore revives erased sessions.
    for required in [
        "cloud mirror tombstone propagation for agent capture data",
        "session erasure tombstone 传播已随 plan-20260714 PD-03 落地",
        "sync_agent_import_tombstones_batch",
        "persist_local_import_tombstones",
        "tombstone 优先",
        "R2 物理删除",
        // Positive truth: the mirror IS live via cloud sync/restore.
        "libra cloud sync",
        "libra cloud restore",
    ] {
        assert!(
            AGENT_DOC.contains(required),
            "agent.md must declare session-tombstone propagation live and R2 deletion deferred: {required}",
        );
    }

    // Every page that describes erasure must agree, and must agree on the
    // ONE remaining deferral.
    for (label, doc) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
    ] {
        assert!(
            doc.contains("R2"),
            "{label} must still name R2 physical deletion as the remaining deferral",
        );
        // Scoped to the claim that actually went stale: RESTORE (or a
        // cross-machine re-sync) bringing an erased session back. Matching
        // "resurrect" alone catches unrelated prose — the `--restore-erased`
        // flag description, a note about queue updates reviving a row — so
        // a line only counts when it ties the two ideas together. The old
        // guard pinned the exact string `复活已本地擦除` while the doc said
        // `复活已擦除`, which is how the contradiction survived.
        for line in doc.lines() {
            let claims_revival = ["复活", "resurrect", "revive"]
                .iter()
                .any(|claim| line.contains(claim));
            let about_restore = ["restore", "re-sync", "resync", "重新同步", "跨机"]
                .iter()
                .any(|word| line.contains(word));
            if !(claims_revival && about_restore) {
                continue;
            }
            let negated = [
                "不",
                "does not",
                "not bring",
                "no longer",
                // Naming the MECHANISM that prevents revival is the opposite
                // of claiming revival happens.
                "anti-resurrection",
                "防复活",
                "反复活",
            ]
            .iter()
            .any(|word| line.contains(word));
            assert!(
                negated,
                "{label} says restore can bring erased capture back, which PD-03 fixed: {line}",
            );
        }
    }

    for forbidden in [
        // The old, factually wrong claims that the mirror does not exist.
        "当前未启用 D1/R2 agent capture mirror",
        "未启用 D1/R2 mirror",
        "未启用 cloud mirror",
        "引入 cloud mirror 后",
        "引入并启用 D1/R2 mirror",
        "cloud mirror 启用后",
        "尚无 D1 mirror",
        "没有 D1 mirror",
        // Post-PD-03: restore can no longer revive an erased session, so
        // the old warning wording must not survive anywhere.
        "复活已本地擦除",
        "会复活已擦除",
        // The phrasing the old list missed. `restore` is tombstone-first on
        // both sides now, so no page may say otherwise in ANY spelling.
        "复活已擦除",
        "仍可复活",
        "会复活 session",
        "不传播 tombstone",
        "tombstone 也不上传",
    ] {
        assert!(
            !AGENT_DOC.contains(forbidden),
            "agent.md still carries a stale pre-PD-03 tombstone claim: {forbidden}",
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
