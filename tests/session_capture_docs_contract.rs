//! Pins the narrow public documentation contracts introduced by the Session
//! Capture Foundation.  Keep this target independent from the broader agent
//! documentation guard so this plan cannot accidentally claim unrelated work.

const AGENT_DOC_EN: &str = include_str!("../docs/commands/agent.md");
const AGENT_DOC_ZH: &str = include_str!("../docs/commands/zh-CN/agent.md");
const HOOKS_DOC_EN: &str = include_str!("../docs/commands/hooks.md");
const HOOKS_DOC_ZH: &str = include_str!("../docs/commands/zh-CN/hooks.md");
const TRACING_AGENT_DOC: &str = include_str!("../docs/development/tracing/agent.md");
const PLAN: &str = include_str!("../docs/development/plan/plan-20260924.md");

#[test]
fn pending_finalizer_diagnostics_are_bounded_and_preserve_superseded_evidence() {
    for (label, document) in [("agent EN", AGENT_DOC_EN), ("agent zh", AGENT_DOC_ZH)] {
        for text in [
            "keyset pagination",
            "32",
            "512",
            "128",
            "1 MiB",
            "pending_source",
            "superseded",
            "manual_required",
        ] {
            require(document, label, text);
        }
    }
    require(AGENT_DOC_EN, "agent EN", "report is incomplete");
    require(
        AGENT_DOC_EN,
        "agent EN",
        "quarantine is not successful capture",
    );
    require(AGENT_DOC_ZH, "agent zh", "报告不完整");
    require(AGENT_DOC_ZH, "agent zh", "quarantine 不代表成功捕获");
    require(
        AGENT_DOC_EN,
        "agent EN",
        "A permanently refused automatic replay parks",
    );
    require(
        AGENT_DOC_EN,
        "agent EN",
        "only the artifact header in quarantine",
    );
    require(
        AGENT_DOC_ZH,
        "agent zh",
        "永久拒绝的自动 replay 只把 artifact header 停放到 quarantine",
    );
    require(
        TRACING_AGENT_DOC,
        "agent tracing",
        "SessionStart detached worker 已接线",
    );
}

fn require(document: &str, label: &str, text: &str) {
    assert!(
        document.contains(text),
        "{label} must retain the Session Capture contract: {text}",
    );
}

#[test]
fn session_extract_transcript_contract_is_safe_and_private() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_DOC_ZH),
    ] {
        require(document, label, "--extract-transcript <path>");
        require(document, label, "16 MiB");
    }

    require(
        AGENT_DOC_EN,
        "docs/commands/agent.md",
        "never falls back to a captured metadata path",
    );
    require(
        AGENT_DOC_EN,
        "docs/commands/agent.md",
        "never overwrites an existing path",
    );
    require(
        AGENT_DOC_EN,
        "docs/commands/agent.md",
        "neither JSON nor human output reveals the provider source path",
    );
    require(
        AGENT_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "绝不回退使用已捕获的 metadata 路径",
    );
    require(
        AGENT_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "绝不覆盖已有路径",
    );
    require(
        AGENT_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "JSON 和人类输出都不暴露 provider 来源路径",
    );
}

#[test]
fn checkpoint_show_contract_is_a_fixed_safe_summary() {
    require(
        AGENT_DOC_EN,
        "docs/commands/agent.md",
        "`agent checkpoint show <id>` is intentionally not a metadata dump.",
    );
    for text in [
        "`checkpoint_id`",
        "a session identifier",
        "source locator or commitment",
        "redaction detail",
        "catalog object identifier",
    ] {
        require(AGENT_DOC_EN, "docs/commands/agent.md", text);
    }

    require(
        AGENT_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "`agent checkpoint show <id>` 刻意不是 metadata dump。",
    );
    for text in [
        "`checkpoint_id`",
        "session 标识",
        "source locator 或 commitment",
        "redaction 细节",
        "catalog 对象 ID",
    ] {
        require(AGENT_DOC_ZH, "docs/commands/zh-CN/agent.md", text);
    }
}

#[test]
fn installer_owned_deadline_and_terminal_contracts_remain_explicit() {
    for (label, document) in [
        ("docs/commands/hooks.md", HOOKS_DOC_EN),
        ("docs/commands/zh-CN/hooks.md", HOOKS_DOC_ZH),
    ] {
        require(document, label, "--capture-budget-ms");
        require(document, label, "statusMessage: \"libra capture\"");
    }

    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "installer appends a hidden, bounded",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "nonterminal capture failure",
    );
    require(HOOKS_DOC_EN, "docs/commands/hooks.md", "SessionEnd");
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "Only a trusted `SessionEnd` creates the durable pending-artifact handoff",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "eligible terminal snapshot",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "Transient database/deadline failures and a live writer",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "Candidates that cannot be authenticated or matched to their current catalog",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "The worker selects candidates from its current worktree",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "remain pending for a hint from that worktree.",
    );
    require(
        HOOKS_DOC_EN,
        "docs/commands/hooks.md",
        "non-zero rather than silently acknowledging a lost terminal boundary",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "安装器会为每条受管命令追加隐藏且受限的 `--capture-budget-ms` 参数",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "nonterminal capture failure",
    );
    require(HOOKS_DOC_ZH, "docs/commands/zh-CN/hooks.md", "SessionEnd");
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "只有可信的 `SessionEnd` 会创建用于 autonomous recovery 的 durable pending artifact handoff",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "符合条件的 terminal snapshot",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "临时数据库/截止时间故障以及仍有 live writer 的尝试会保留 pending",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "无法认证或无法匹配当前 catalog receipt",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "已耗尽原始重试预算的候选项会保留在 quarantine 中",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "属于其他 worktree 的 artifact 会保留 pending，等待该 worktree 的 hint",
    );
    require(
        HOOKS_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "而不会静默确认已丢失的终态边界",
    );
}

/// ACF-13: a retained terminal artifact owns its replay. Native redelivery and
/// a changed hook working directory are explicit non-zero outcomes, never a
/// silent acknowledgement or a second capture.
#[test]
fn terminal_artifact_redelivery_and_identity_conflict_are_explicit() {
    for text in [
        "a\nnative redelivery of the same `SessionEnd` exits non-zero with a content-free,\nretryable message",
        "Coverage claims held by a retained\nartifact are not taken over by later writers",
        "exits non-zero as explicitly\nincomplete: no artifact is retained and no recovery worker is hinted",
    ] {
        require(HOOKS_DOC_EN, "docs/commands/hooks.md", text);
    }
    for text in [
        "同一 `SessionEnd` 的原生重投会以不含敏感内容、可重试的消息非零退出",
        "所持有的 coverage claims 在该重放完成或 session 被显式 erase 前不会被后续 writer 接管",
        "会以显式 incomplete 非零退出：不保留 artifact，也不提示 recovery worker",
    ] {
        require(HOOKS_DOC_ZH, "docs/commands/zh-CN/hooks.md", text);
    }
}

#[test]
fn codex_hook_taxonomy_matches_its_provider_specific_command_surface() {
    let codex_synopsis_en = HOOKS_DOC_EN
        .lines()
        .find(|line| line.starts_with("libra hooks codex"))
        .expect("English Codex hook synopsis");
    assert!(
        codex_synopsis_en.contains("permission-request"),
        "Codex synopsis must expose its PermissionRequest verb: {codex_synopsis_en}"
    );
    assert!(
        !codex_synopsis_en.contains("model-update"),
        "Codex has no ModelUpdate hook and must not advertise one: {codex_synopsis_en}"
    );
    for text in [
        "Codex has eleven native event names, collapsed to nine command verbs",
        "no `ModelUpdate` event",
        "`PermissionRequest` | `permission-request`",
        "`SubagentStart` | `subagent-start`",
        "`SubagentStop` | `subagent-end`",
    ] {
        require(HOOKS_DOC_EN, "docs/commands/hooks.md", text);
    }

    let codex_synopsis_zh = HOOKS_DOC_ZH
        .lines()
        .find(|line| line.starts_with("libra hooks codex"))
        .expect("Chinese Codex hook synopsis");
    assert!(
        codex_synopsis_zh.contains("permission-request"),
        "Codex synopsis must expose its PermissionRequest verb: {codex_synopsis_zh}"
    );
    assert!(
        !codex_synopsis_zh.contains("model-update"),
        "Codex has no ModelUpdate hook and must not advertise one: {codex_synopsis_zh}"
    );
    for text in [
        "Codex 有 11 个原生事件名，并折叠为 9 个命令 verb",
        "没有 `ModelUpdate` 事件",
        "`PermissionRequest` | `permission-request`",
        "`SubagentStart` | `subagent-start`",
        "`SubagentStop` | `subagent-end`",
    ] {
        require(HOOKS_DOC_ZH, "docs/commands/zh-CN/hooks.md", text);
    }
}

#[test]
fn cap_raw_archive_work_remains_independently_blocked() {
    for card in ["CAP-04", "CAP-05", "CAP-06", "CAP-07"] {
        require(PLAN, "plan-20260924.md", card);
    }
    require(
        PLAN,
        "plan-20260924.md",
        "即使 ACF-09 complete 也持續 `blocked`",
    );
    require(
        PLAN,
        "plan-20260924.md",
        "獨立 security/privacy RFC 與新卡驗收完成",
    );
    require(PLAN, "plan-20260924.md", "必要而非充分條件");
}
