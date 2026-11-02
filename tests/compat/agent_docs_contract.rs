//! Guard active external-agent documentation against stale implementation claims.
//!
//! This target deliberately has no dependency on the removed Code executor
//! module. It pins only the current external-agent contracts: retired
//! `claudecode`, public schema/retention/raw-export wording, PD-03 tombstone
//! propagation, macOS OpenCode export fail-closed disclosure, and the active internal-plan
//! source of truth, and the complete installed Codex hook-event map.

use std::path::Path;

const AGENT_DOC: &str = include_str!("../../docs/development/tracing/agent.md");
const AGENT_CMD_DOC_EN: &str = include_str!("../../docs/commands/agent.md");
const AGENT_CMD_DOC_ZH: &str = include_str!("../../docs/commands/zh-CN/agent.md");
const HOOKS_CMD_DOC_EN: &str = include_str!("../../docs/commands/hooks.md");
const HOOKS_CMD_DOC_ZH: &str = include_str!("../../docs/commands/zh-CN/hooks.md");
const AGENT_DEV_DOC: &str = include_str!("../../docs/development/commands/agent.md");
const COMPATIBILITY_DOC: &str = include_str!("../../COMPATIBILITY.md");
const ERROR_CODES_DOC: &str = include_str!("../../docs/error-codes.md");
const CLOUD_CMD_DOC_EN: &str = include_str!("../../docs/commands/cloud.md");
const CLOUD_CMD_DOC_ZH: &str = include_str!("../../docs/commands/zh-CN/cloud.md");

#[cfg(feature = "test-live-agent")]
#[test]
#[ignore = "local-only Codex association documentation; never run in CI"]
fn cx23_commit_association_docs_match_current_local_contract() {
    let commit_en = include_str!("../../docs/commands/commit.md");
    let commit_zh = include_str!("../../docs/commands/zh-CN/commit.md");
    let commit_dev = include_str!("../../docs/development/commands/commit.md");
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("agent zh-CN", AGENT_CMD_DOC_ZH),
        ("agent development", AGENT_DEV_DOC),
        ("tracing", AGENT_DOC),
        ("commit EN", commit_en),
        ("commit zh-CN", commit_zh),
        ("commit development", commit_dev),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        for required in [
            "agent.commit_trailer",
            "--no-agent-trailer",
            "Libra-Agent-Session",
            "64",
            "agent-commit-links.v1",
        ] {
            assert_contains(document, label, required);
        }
    }
    for (label, document) in [("commit EN", commit_en), ("commit zh-CN", commit_zh)] {
        for required in ["commit-msg", "lease", "amend", "cloud", "LBR-WARN-001"] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "defaults to `false`",
        "If this opt-in setting cannot be read",
        "unreadable opt-in configuration warning follows the same exit-code rule",
        "after the `commit-msg` hook",
        "before optional",
        "more than 64 eligible sessions aborts",
        "Dry runs neither link nor preview",
        "doctor --repair",
        "unknown",
        "does not back it up",
    ] {
        assert_contains(commit_en, "commit EN", required);
    }
    for required in [
        "默认 `false`",
        "若无法读取此 opt-in 配置",
        "读取 opt-in 配置失败产生的警告也遵循同一退出码规则",
        "`commit-msg` hook 结束后",
        "签名前",
        "超过 64 个",
        "预演既不写关联，也不预览",
        "doctor --repair",
        "不备份",
    ] {
        assert_contains(commit_zh, "commit zh-CN", required);
    }
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("agent zh-CN", AGENT_CMD_DOC_ZH),
        ("agent development", AGENT_DEV_DOC),
        ("tracing", AGENT_DOC),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        assert_contains(document, label, "--commit-link-page");
        assert_contains(document, label, "--commit-page");
        assert_contains(document, label, "--reset-commit-link-journal");
        assert_contains(document, label, "--acknowledge-commit-link-loss");
    }
    for (label, document) in [
        ("cloud EN", include_str!("../../docs/commands/cloud.md")),
        (
            "cloud zh-CN",
            include_str!("../../docs/commands/zh-CN/cloud.md"),
        ),
        (
            "cloud development",
            include_str!("../../docs/development/commands/cloud.md"),
        ),
    ] {
        assert_contains(document, label, "agent-commit-links.v1");
        assert_contains(document, label, "cloud");
    }
}

#[test]
fn native_attestation_launch_docs_disclose_isolation_and_limits() {
    for (label, document) in [
        ("EN", AGENT_CMD_DOC_EN),
        ("compatibility", COMPATIBILITY_DOC),
        ("tracing", AGENT_DOC),
        (
            "native-origin design",
            include_str!("../../docs/development/internal/opencode-native-origin.md"),
        ),
    ] {
        for required in [
            "caller `TMPDIR`",
            "mode 0700 and empty before the producer starts",
            "cleanup refuses registration",
            "cancellation cleanup is best effort",
            "while the leader PID is still reserved",
            "later challenges never reuse",
            "Libra-launched installation challenge",
            "real-field archive gates remain unproven",
        ] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "不继承调用者的 `TMPDIR`",
        "0700 且为空",
        "显式清理失败",
        "取消时尽力清理",
        "leader PID 仍保留",
        "后续挑战绝不重用",
        "仅覆盖 Libra 发起的安装挑战",
        "真实字段归档门保持未放行",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "zh-CN", required);
    }
}

#[test]
fn reasoning_artifact_lifecycle_docs_match_current_contract() {
    for (label, document) in [
        ("EN", AGENT_CMD_DOC_EN),
        ("zh-CN", AGENT_CMD_DOC_ZH),
        ("development", AGENT_DEV_DOC),
        ("tracing", AGENT_DOC),
    ] {
        for term in [
            "manifest role",
            "object_index",
            "byte_len",
            "SHA-256",
            "fenced generation",
            "agent clean --gc",
            "inconsistent_reasoning_artifact",
            "checkpoint_id",
            "agent enable",
            "agent import --yes",
            "encrypted_unavailable",
        ] {
            assert_contains(document, label, term);
        }
    }
    assert_contains(
        AGENT_CMD_DOC_EN,
        "EN",
        "Physical R2 payload deletion remains deferred",
    );
    assert_contains(
        AGENT_CMD_DOC_EN,
        "EN",
        "Native source proof remains required",
    );
    assert_contains(AGENT_CMD_DOC_ZH, "zh-CN", "native 来源证明要求继续保留");
    assert_contains(AGENT_CMD_DOC_ZH, "zh-CN", "不新增持久化 status 列");
}

fn assert_contains(document: &str, label: &str, required: &str) {
    assert!(
        document.contains(required),
        "{label} must retain the active external-agent contract: {required}",
    );
}

fn assert_absent(document: &str, label: &str, forbidden: &str) {
    assert!(
        !document.contains(forbidden),
        "{label} still carries a stale external-agent claim: {forbidden}",
    );
}

#[test]
fn agent_doc_keeps_claudecode_marked_removed_not_active() {
    for removed_path in [
        "src/internal/ai/claudecode",
        "src/internal/ai/claudecode.rs",
    ] {
        let removed_provider = Path::new(env!("CARGO_MANIFEST_DIR")).join(removed_path);
        assert!(
            !removed_provider.exists(),
            "{} must stay absent after the claudecode hard-delete",
            removed_provider.display(),
        );
    }

    for forbidden in [
        "`claudecode` provider 仍存在",
        "code.rs` 仍有 Claudecode provider",
    ] {
        assert_absent(AGENT_DOC, "docs/development/tracing/agent.md", forbidden);
    }
    assert_contains(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "claudecode 硬删除",
    );
    assert_contains(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "`src/internal/ai/claudecode/` 不存在",
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
        assert_contains(AGENT_DOC, "docs/development/tracing/agent.md", required);
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
        assert_absent(AGENT_DOC, "docs/development/tracing/agent.md", forbidden);
    }
}

#[test]
fn import_docs_pin_v2_source_commitment_and_legacy_read_boundary() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("docs/development/commands/agent.md", AGENT_DEV_DOC),
        ("docs/development/tracing/agent.md", AGENT_DOC),
    ] {
        assert_contains(document, label, "source/hmac-v2/<64 lower-hex>");
    }

    assert_contains(
        AGENT_CMD_DOC_EN,
        "docs/commands/agent.md",
        "A bare or tagged unkeyed\nSHA-256 is immutable legacy evidence only",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "bare 或带标签的未加钥 SHA-256 只可作为不可变\nlegacy evidence",
    );
    assert_contains(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "bare 或带标签的未加钥 SHA 只可作为不可变 legacy proof 读取",
    );
    assert_contains(
        AGENT_DEV_DOC,
        "docs/development/commands/agent.md",
        "tagged unkeyed SHA-256 remains read-only immutable V1 proof",
    );
    for (label, document, commitment_boundary) in [
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "helper's transient SHA-256 is never durable",
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "helper 的瞬态 SHA-256 绝不持久化",
        ),
        (
            "docs/development/commands/agent.md",
            AGENT_DEV_DOC,
            "helper's transient SHA-256 is never durable",
        ),
        (
            "docs/development/tracing/agent.md",
            AGENT_DOC,
            "helper 的瞬态 SHA-256 绝不持久化",
        ),
    ] {
        assert_contains(document, label, commitment_boundary);
    }
}

/// PI-05: the local capture-kind fence documents both recovery hints (the
/// refusal persists while local-only capture exists; external full backup
/// meanwhile) and never suggests deleting capture data to get past it.
#[test]
fn local_capture_kind_fence_docs_pin_recovery_hints() {
    for (label, document, required) in [
        (
            "docs/commands/cloud.md",
            CLOUD_CMD_DOC_EN,
            "keeps refusing until a later Libra release adds complete cloud support for that kind",
        ),
        (
            "docs/commands/cloud.md",
            CLOUD_CMD_DOC_EN,
            "back up the whole repository, including its `.libra` directory, with an external backup tool you choose",
        ),
        (
            "docs/commands/cloud.md",
            CLOUD_CMD_DOC_EN,
            "The allowlist is the same list the D1 `agent_kind` CHECK is built from",
        ),
        (
            "docs/commands/zh-CN/cloud.md",
            CLOUD_CMD_DOC_ZH,
            "会持续拒绝，直到后续 Libra 版本为该种类提供完整云支持",
        ),
        (
            "docs/commands/zh-CN/cloud.md",
            CLOUD_CMD_DOC_ZH,
            "请用你自己选择的外部备份工具备份整个仓库（包括 `.libra` 目录）",
        ),
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "the hint points to an external backup of the whole repository meanwhile",
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "期间提示改用外部工具备份整个仓库",
        ),
        (
            "docs/development/commands/agent.md",
            AGENT_DEV_DOC,
            "full Pi cloud mirror stays DEFER-PI-08",
        ),
        (
            "docs/development/tracing/agent.md",
            AGENT_DOC,
            "直到\n后续版本提供该 kind 的完整云支持（plan-20260911 DEFER-PI-08）",
        ),
        (
            "COMPATIBILITY.md",
            COMPATIBILITY_DOC,
            "the hints say the refusal persists while local-only\ncapture exists",
        ),
    ] {
        assert_contains(document, label, required);
    }
    for (label, document) in [
        ("docs/commands/cloud.md", CLOUD_CMD_DOC_EN),
        ("docs/commands/zh-CN/cloud.md", CLOUD_CMD_DOC_ZH),
    ] {
        for forbidden in [
            "delete the capture sessions",
            "清空数据库后重试",
            "删除会话后重试",
        ] {
            assert_absent(document, label, forbidden);
        }
    }
}

#[test]
fn agent_doc_declares_cloud_tombstone_deferred() {
    for required in [
        "session erasure tombstone 传播已随 plan-20260714 PD-03 落地",
        "sync_agent_import_tombstones_batch",
        "persist_local_import_tombstones",
        "双向 tombstone 优先",
        "R2 物理删除",
        "libra cloud sync",
        "libra cloud restore",
    ] {
        assert_contains(AGENT_DOC, "docs/development/tracing/agent.md", required);
    }

    for forbidden in [
        "仍待建的是 session erasure 的 cloud tombstone/catalog 删除",
        "session erasure tombstone/catalog 删除和 R2 物理删除仍未实现",
        "session erasure tombstone/catalog 删除与 R2 物理删除是待建强制面",
        "待 delete/tombstone 传播落地",
        "restore 仍会复活被 erase 的 session",
        "session erasure tombstone/catalog 删除与 R2 物理删除仍 deferred",
        "cloud restore 的 session 复活风险",
    ] {
        assert_absent(AGENT_DOC, "docs/development/tracing/agent.md", forbidden);
    }

    for (label, document, propagation, required) in [
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "D1 tombstone propagation",
            "does not bring the session back",
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "D1 tombstone 传播",
            "也不会把 session 复活",
        ),
    ] {
        assert_contains(document, label, propagation);
        assert_contains(document, label, "R2");
        assert_contains(document, label, required);
    }

    for (label, document, forbidden) in [
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "remote deletion propagation remains deferred",
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "远端删除传播仍 deferred",
        ),
    ] {
        assert_absent(document, label, forbidden);
    }
}

#[test]
fn macos_opencode_export_requires_fork_denying_seatbelt_pair() {
    for (label, document) in [
        ("tracing/agent.md", AGENT_DOC),
        ("commands/agent.md", AGENT_CMD_DOC_EN),
        ("commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("development/commands/agent.md", AGENT_DEV_DOC),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        assert!(
            document.contains("Required Seatbelt server/export pair")
                && document.contains("process-fork")
                && document.contains("loopback")
                && document.contains("metadata-only"),
            "{label} must disclose required fork denial, bounded loopback and fail-closed degradation"
        );
        for obsolete in [
            "content export is unsupported",
            "不支持 OpenCode 内容导出",
            "explicitly unsupported",
        ] {
            assert_absent(document, label, obsolete);
        }
    }
}

#[test]
fn linux_opencode_export_requires_descriptor_native_bwrap_probe() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("docs/development/commands/agent.md", AGENT_DEV_DOC),
        ("docs/development/tracing/agent.md", AGENT_DOC),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        assert!(
            document.contains("bwrap")
                && document.contains("--bind-fd")
                && document.contains("metadata-only"),
            "{label} must document the Linux descriptor-native bwrap probe and fail-closed metadata-only fallback",
        );
    }
}

#[test]
fn agent_doc_tracks_code_agent_runtime_source_of_truth() {
    assert_contains(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "../internal/code-agent-runtime.md",
    );

    for forbidden_link in [
        "](../agent.md)",
        "](../web-only.md)",
        "](../code-agent-runtime.md)",
        "](../../development/agent.md)",
        "](../../development/web-only.md)",
        "](../../development/code-agent-runtime.md)",
    ] {
        assert_absent(
            AGENT_DOC,
            "docs/development/tracing/agent.md",
            forbidden_link,
        );
    }
}

#[test]
fn hook_docs_pin_marker_ownership_and_malformed_ingress_exit_policy() {
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "statusMessage: \"libra capture\"",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "unmarked command — whether bare, standard-named, or renamed",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "never owned\nby disable",
    );
    assert_contains(HOOKS_CMD_DOC_EN, "docs/commands/hooks.md", "LBR-AGENT-008");
    assert_contains(HOOKS_CMD_DOC_EN, "docs/commands/hooks.md", "`128`");
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "`SessionEnd` is capped by its host at `3s → 2000`",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "supports Unix and native Windows",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "prints a path-free platform remedy to stderr and exits `0`",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "Codex records hook approval by matcher-group and handler position",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "refuse before changing either `hooks.json` or `config.toml`",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "portable atomic compare-and-replace",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "non-plaintext integrity\nrecovery-journal path",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "limited transaction metadata (schema version, operation,\nphase, and a recovery fence)",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "domain-separated file fingerprints, never\nraw settings snapshots",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "unknown fingerprint state",
    );
    assert_contains(HOOKS_CMD_DOC_EN, "docs/commands/hooks.md", "zero-write");
    // R86 #4: a callback outside any Libra repository is never a trusted
    // terminal boundary; Codex acknowledges it, Claude/alias keep LBR-REPO-001.
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "A callback invoked from a working directory outside any Libra repository has\nno scope to bind and is never a trusted terminal boundary.",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "was invoked outside a Libra repository (`LBR-REPO-001`)",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "在任何 Libra 仓库之外的工作目录触发的回调没有可绑定的 scope，绝不会成为可信终态边界",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "在 Libra 仓库之外被调用（`LBR-REPO-001`）",
    );
    // The unsupported-platform capability result is scoped to in-repository callbacks;
    // the outside-repository contract holds on every platform.
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "outside-repository contract below on every platform, Unix or not.",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "在所有平台（无论是否为 Unix）上都遵循下文的仓库外契约",
    );
    assert_contains(
        AGENT_CMD_DOC_EN,
        "docs/commands/agent.md",
        "contract above on every platform, Unix or not.",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "在所有平台（无论是否为 Unix）上都遵循上文的仓库外契约",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "statusMessage: \"libra capture\"",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "未标记 command（无论裸 `libra`、标准命名还是重命名）",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "在停用时一律不认领",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "LBR-AGENT-008",
    );
    assert_contains(HOOKS_CMD_DOC_ZH, "docs/commands/zh-CN/hooks.md", "`128`");
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "Codex `SessionEnd` 受宿主上限约束",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "支持 Unix 和原生 Windows",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "对 Codex 绝不静默",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "Codex 按 matcher-group 与 handler 的位置记录 hook trust",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "`hooks.json` 或 `config.toml` 前拒绝",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "可移植的原子“比较并替换”",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "非明文完整性恢复\n日志路径",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "有限的事务\n元数据（schema 版本、操作、阶段和恢复围栏）",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "域隔离的文件指纹，不含原始设置快照",
    );
    assert_contains(HOOKS_CMD_DOC_ZH, "docs/commands/zh-CN/hooks.md", "未知指纹");
    assert_contains(HOOKS_CMD_DOC_ZH, "docs/commands/zh-CN/hooks.md", "零写入");
    assert_contains(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "无标记 command（包括 `libra`/`libra.exe`、标准绝对路径或重命名路径）在 disable 时必须保留",
    );
    assert_contains(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "无标记 legacy/旧 canonical command（包括 `libra`/`libra.exe`）均保留且绝不获得 Libra trust",
    );

    assert_absent(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "非 Codex 的 stdin payload 未通过 schema 验证",
    );
    assert_contains(
        HOOKS_CMD_DOC_ZH,
        "docs/commands/zh-CN/hooks.md",
        "malformed hook input 不使用此退出码",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "malformed hook input does not use this code",
    );
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
    ] {
        assert_contains(document, label, "SessionEnd");
        assert_contains(document, label, "deduplication");
        assert_contains(document, label, "Unix-host remedy");
    }
    assert_contains(
        AGENT_CMD_DOC_EN,
        "docs/commands/agent.md",
        "cooperative Libra-process lock",
    );
    assert_contains(
        AGENT_CMD_DOC_EN,
        "docs/commands/agent.md",
        "non-plaintext integrity\nrecovery-journal path",
    );
    assert_contains(
        AGENT_CMD_DOC_EN,
        "docs/commands/agent.md",
        "never raw hook or config contents",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "协作式 Libra 进程锁",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "非明文完整性恢复日志路径",
    );
}

#[test]
fn codex_capture_doc_pins_all_eleven_installed_hook_events() {
    for event in [
        "| `SessionStart` | `session-start` |",
        "| `UserPromptSubmit` | `prompt` |",
        "| `PreToolUse` | `tool-use` |",
        "| `PostToolUse` | `tool-use` |",
        "| `PermissionRequest` | `permission-request` |",
        "| `PreCompact` | `compaction` |",
        "| `PostCompact` | `compaction` |",
        "| `Stop` | `stop` |",
        "| `SessionEnd` | `session-end` |",
        "| `SubagentStart` | `subagent-start` |",
        "| `SubagentStop` | `subagent-end` |",
    ] {
        assert_contains(AGENT_DOC, "docs/development/tracing/agent.md", event);
    }
    assert_absent(
        AGENT_DOC,
        "docs/development/tracing/agent.md",
        "但默认不安装转发",
    );
}

#[test]
fn scoped_checkpoint_resume_docs_keep_ordinary_revalidation() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("docs/development/commands/agent.md", AGENT_DEV_DOC),
        ("docs/development/tracing/agent.md", AGENT_DOC),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        assert_contains(document, label, "reasoning/encrypted/<64 hex>");
        assert!(
            document.contains("ordinary") || document.contains("普通"),
            "{label} must describe ordinary checkpoint leaves"
        );
        assert!(
            document.contains("not a blob") || document.contains("不是 blob"),
            "{label} must refuse a saved id that is not a blob"
        );
        assert!(
            document.to_ascii_lowercase().contains("full-list") && document.contains("manifest"),
            "{label} must explain the proven legacy full-list exception"
        );
        assert!(
            document.contains("without opening") || document.contains("不打开"),
            "{label} must exclude closed artifact payload reads"
        );
    }
    assert_contains(
        ERROR_CODES_DOC,
        "docs/error-codes.md",
        "do not add a stable code for scoped revalidation",
    );
    assert_contains(
        ERROR_CODES_DOC,
        "docs/error-codes.md",
        "`LBR-IO-002` is not reused for that refusal",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "docs/commands/zh-CN/agent.md",
        "不新增稳定错误码",
    );
}

#[test]
fn reasoning_type_docs_pin_metadata_only_and_five_states() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("docs/development/commands/agent.md", AGENT_DEV_DOC),
        ("docs/development/tracing/agent.md", AGENT_DOC),
    ] {
        for state in [
            "provider_visible",
            "encrypted_unavailable",
            "opaque_archived",
            "not_present",
            "unsupported_shape",
        ] {
            assert_contains(document, label, state);
        }
        assert_contains(document, label, "OpaqueEncryptedBytes");
        assert_contains(document, label, "RedactedBytes");
    }
    assert_contains(
        AGENT_CMD_DOC_EN,
        "reasoning contract",
        "not that capture failed",
    );
    assert_contains(AGENT_CMD_DOC_ZH, "reasoning contract", "不是采集失败");
    assert_contains(AGENT_DEV_DOC, "reasoning contract", "not a capture failure");
    assert_contains(AGENT_DOC, "reasoning contract", "不是采集失败");
    assert_contains(AGENT_CMD_DOC_EN, "RG-01 scope", "metadata-only");
    assert_contains(
        AGENT_CMD_DOC_EN,
        "runtime artifact capture scope",
        "No live provider adapter yet produces artifacts",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "runtime artifact capture scope",
        "尚无 live provider adapter 产生 artifact",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "RG-01 verified source scope",
        "OpenCode 密文采集仍待注册经验证的 provider 字段",
    );
    assert_contains(
        AGENT_DEV_DOC,
        "RG-01 scope",
        "does not yet capture or store",
    );
    assert_contains(AGENT_DOC, "RG-01 scope", "未接存储");
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("agent zh-CN", AGENT_CMD_DOC_ZH),
        ("developer EN", AGENT_DEV_DOC),
        ("tracing zh-CN", AGENT_DOC),
    ] {
        assert_contains(document, label, "thinking.signature");
        assert_contains(document, label, "redacted_thinking.data");
        assert_contains(document, label, "reasoning.state");
        assert_contains(document, label, "content index");
        assert_contains(document, label, "native artifact");
        assert_contains(document, label, "coverage revision");
    }
}

#[test]
fn reasoning_readable_projection_docs_pin_typed_redaction() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("agent zh-CN", AGENT_CMD_DOC_ZH),
        ("developer EN", AGENT_DEV_DOC),
        ("tracing zh-CN", AGENT_DOC),
    ] {
        for term in [
            "ProviderVisibleText",
            "serde",
            "canonical_turn_bytes",
            "safe_turn_projection",
        ] {
            assert_contains(document, label, term);
        }
    }

    // RG-04: readable reasoning projection — typed redaction before any
    // persistence; own `reasoning` record type; never in unredacted metadata.
    assert_contains(
        AGENT_CMD_DOC_EN,
        "RG-04 readable reasoning",
        "always passes typed redaction first",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "RG-04 可读 reasoning",
        "必先经过 typed redaction",
    );
    assert_contains(
        AGENT_DEV_DOC,
        "RG-04 adds the readable-reasoning projection path",
        "always passes typed redaction first",
    );
    assert_contains(
        AGENT_DOC,
        "RG-04 加入可读 reasoning",
        "必先经过 typed redaction",
    );
}

#[test]
fn rg02_artifact_docs_contract() {
    // RG-02: the artifact manifest role, dedup and fan-out budgets, and the
    // unchanged four-role content_hash coverage are pinned in all four docs.
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("agent zh-CN", AGENT_CMD_DOC_ZH),
        ("developer EN", AGENT_DEV_DOC),
        ("tracing zh-CN", AGENT_DOC),
    ] {
        assert_contains(document, label, "reasoning_artifacts[]");
        assert_contains(document, label, "reasoning/encrypted/<sha256>");
        assert_contains(document, label, "512");
        assert_contains(document, label, "256 KiB");
        assert_contains(document, label, "32 MiB");
    }
    assert_contains(
        AGENT_CMD_DOC_EN,
        "RG-02 archives verified ciphertext",
        "leaves the checkpoint tree byte-identical",
    );
    // Codex re-review P1: the docs must pin that content_hash coverage stays
    // the four plain roles (never the artifact role) in EN/zh/developer/tracing.
    assert_contains(
        AGENT_CMD_DOC_EN,
        "RG-02 archives verified ciphertext",
        "content_hash` still covers only the four plain roles",
    );
    assert_contains(
        AGENT_CMD_DOC_ZH,
        "RG-02 将已核验的密文归档",
        "`content_hash` 仍只覆盖四个普通角色",
    );
    assert_contains(
        AGENT_DEV_DOC,
        "RG-02 adds the artifact archive",
        "content_hash` coverage stays four-role",
    );
    assert_contains(
        AGENT_DOC,
        "RG-02 归档 opaque 密文 artifact",
        "`content_hash` 四角色不变",
    );
}

#[test]
fn rg06_artifact_atomicity_docs_contract() {
    // RG-06: artifact blobs ride the same attempt as the checkpoint and are
    // ref-reachable tree entries (GC-safe), pinned in the tracing doc.
    assert_contains(
        AGENT_DOC,
        "RG-06 同 attempt 原子性",
        "RG-06 保证 artifact 与所属 checkpoint 同 attempt 原子写入",
    );
    assert_contains(
        AGENT_DOC,
        "RG-06 immutable signature time",
        "append_checkpoint_commit 初次写入非空 artifact checkpoint，以及相同 payload、未变 parent 的重试",
    );
    assert_contains(
        AGENT_DOC,
        "RG-06 immutable signature time",
        "author/committer 使用已封存 metadata.created_at 的 Unix 秒和固定 UTC",
    );
    assert_contains(
        AGENT_DOC,
        "RG-06 commit graph timestamp bound",
        "创建时间限于 0..=17179869183 Unix 秒（Git commit-graph 的 34 位时间范围）",
    );
    assert_contains(
        AGENT_DOC,
        "RG-06 retention timestamp scope",
        "retention 重写仍使用既有提交时间策略",
    );
}

#[test]
fn scoped01_shared_reader_docs_pin_role_and_catalog_bounds() {
    let command = include_str!("../../docs/development/commands/agent.md");
    let tracing = include_str!("../../docs/development/tracing/agent.md");
    for text in [command, tracing] {
        assert!(text.contains("checkpoint_reader"));
        assert!(text.contains("4608"));
        assert!(text.contains("8431616"));
        assert!(text.contains("84-byte"));
        assert!(text.contains("200 ms"));
    }
    assert!(command.contains("closed whitelist"));
    assert!(command.contains("Raw export authorization and audit are unchanged"));
    assert!(AGENT_CMD_DOC_EN.contains("ordinary child trees are capped at 16 MiB"));
    assert!(AGENT_CMD_DOC_EN.contains("selected metadata blob at 16 MiB"));
    assert!(AGENT_CMD_DOC_ZH.contains("普通子树累计上限为 16 MiB"));
    assert!(AGENT_CMD_DOC_ZH.contains("metadata blob 上限为 16 MiB"));
}

#[test]
fn opencode_docs_pin_current_parser_and_legacy_alias_policy() {
    for (label, document) in [
        ("tracing/agent.md", AGENT_DOC),
        ("commands/agent.md", AGENT_CMD_DOC_EN),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        for required in [
            "2.0.26",
            "requires a prompt string",
            "empty string",
            "omitted role",
            "libra agent enable --agent opencode",
            "session.status(idle)",
            "legacy_event_alias",
        ] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "2.0.26",
        "prompt 必须是字符串",
        "允许空字符串",
        "role 可以省略",
        "libra agent enable --agent opencode",
        "session.status(idle)",
        "legacy_event_alias",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "commands/zh-CN/agent.md", required);
    }
}

#[test]
fn opencode_docs_distinguish_private_acceptance_from_historical_source_and_native_proof() {
    for (label, document) in [
        ("tracing/agent.md", AGENT_DOC),
        ("commands/agent.md", AGENT_CMD_DOC_EN),
        ("commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        for required in [
            "2.0.26-libra-native.1",
            "bac21cc4027896b8d1836133c347d20f",
            "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196",
            "187635570",
            "9b4ec5714d481559990db0a816d5dec19541a814",
            "native-origin-build.json",
            "LIBRA_OPENCODE_NATIVE_BINARY",
            "SOURCE_READY_GO",
            "UNPROVEN",
            "volcengine-agent-plan/glm-5.3-flash",
        ] {
            assert_contains(document, label, required);
        }
    }
}

#[test]
fn opencode_docs_disclose_current_template_upgrade_and_bounds() {
    for (label, doc) in [
        ("tracing/agent.md", AGENT_DOC),
        ("commands/agent.md", AGENT_CMD_DOC_EN),
    ] {
        for required in [
            "setup(ctx)",
            "node:child_process",
            "shell:false",
            "SIGKILL",
            "not installed",
            "16,384",
            "256",
            "concurrent directory",
            "synthetic events",
            "OPENCODE_PURE=1",
        ] {
            assert_contains(doc, label, required);
        }
        assert!(!doc.contains("The current managed plugin still forwards the legacy event set"));
    }
    for required in [
        "setup(ctx)",
        "node:child_process",
        "shell:false",
        "SIGKILL",
        "16,384",
        "256",
        "目录命名空间稳定",
        "synthetic",
        "OPENCODE_PURE=1",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "commands/zh-CN/agent.md", required);
    }
    for required in [
        "exact current rendered template",
        "post-check relocation/leaf replacement remains deferred",
        "synthetic runtime conformance",
    ] {
        assert_contains(COMPATIBILITY_DOC, "COMPATIBILITY.md", required);
    }
}

#[test]
fn opencode_docs_disclose_export_projection_and_provenance_limits() {
    for (label, document) in [("tracing", AGENT_DOC), ("agent English", AGENT_CMD_DOC_EN)] {
        for required in [
            "assistant `model.id`",
            "input + output + reasoning + cache.read + cache.write",
            "with forward slashes",
            "partial metadata",
            "native reasoning origin",
        ] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "assistant `model.id`",
        "input + output + reasoning + cache.read + cache.write",
        "项目相对路径",
        "标记 partial",
        "native reasoning 来源",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "agent Chinese", required);
    }
    assert_contains(COMPATIBILITY_DOC, "compatibility", "five-component tokens");
    assert_contains(
        COMPATIBILITY_DOC,
        "compatibility",
        "invalid/missing provenance or counters stays partial",
    );
}

#[test]
fn opencode_docs_disclose_normalizer_completeness_and_attachment_limits() {
    for (label, document) in [("tracing", AGENT_DOC), ("agent English", AGENT_CMD_DOC_EN)] {
        for required in [
            "Each human message starts its own stable logical turn",
            "Complete describes captured representation, not task success",
            "Unknown/malformed",
            "they mark Incomplete without copying attachment bytes or URIs",
            "error.message, never response.body",
        ] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "每条 human message",
        "Complete 表示采集表示完整，不等于任务成功",
        "idle 不会洗白",
        "不向语义记录复制附件字节或 URI",
        "不投影 response.body",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "agent Chinese", required);
    }
    assert_contains(
        COMPATIBILITY_DOC,
        "compatibility",
        "Native error fallback projects error.message, never response.body",
    );
}

#[test]
fn opencode_docs_disclose_shared_literal_reminder_filter() {
    for (label, document) in [("tracing", AGENT_DOC), ("agent English", AGENT_CMD_DOC_EN)] {
        for required in [
            "including nested blocks",
            "text outside them stays byte-for-byte unchanged",
            "independent token metadata is retained",
            "unfinished block suppresses its tail and marks partial/Incomplete",
            "literal marker filter, not proof of producer origin",
        ] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "块外文本保持原字节",
        "独立token元数据仍保留",
        "idle不能洗白",
        "不证明producer来源",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "agent Chinese", required);
    }
    assert_contains(
        COMPATIBILITY_DOC,
        "compatibility",
        "malformed markers stay partial/Incomplete",
    );
}

#[test]
fn opencode_docs_disclose_extraction_failure_containment() {
    for (label, document) in [("tracing", AGENT_DOC), ("agent English", AGENT_CMD_DOC_EN)] {
        for required in [
            "empty input, invalid JSON, and illegal",
            "fixed payload-free warnings",
            "extraction errors do not block an otherwise admissible checkpoint",
            "exports do not replace an existing checkpoint",
            "native-origin gates remain required",
        ] {
            assert_contains(document, label, required);
        }
    }
    for required in [
        "不含payload的固定警告",
        "坏export不替换已有checkpoint",
        "native来源门仍然必须满足",
    ] {
        assert_contains(AGENT_CMD_DOC_ZH, "agent Chinese", required);
    }
    assert_contains(
        COMPATIBILITY_DOC,
        "compatibility",
        "OpenCode empty/invalid exports are partial with fixed payload-free warnings",
    );
}

#[test]
fn scoped_run_cleanup_docs_pin_terminal_and_foreign_protection() {
    for (label, doc) in [
        ("commands/agent.md", AGENT_CMD_DOC_EN),
        ("development/commands/agent.md", AGENT_DEV_DOC),
        ("development/tracing/agent.md", AGENT_DOC),
        ("error-codes.md", include_str!("../../docs/error-codes.md")),
    ] {
        assert_contains(doc, label, "terminal manifest");
        assert_contains(doc, label, "foreign");
        assert_contains(doc, label, "corrupt");
    }
    assert_contains(AGENT_CMD_DOC_EN, "EN", "removed");
    assert_contains(AGENT_CMD_DOC_EN, "EN", "successful deletions");
    assert_contains(
        AGENT_CMD_DOC_EN,
        "EN",
        "interrupted state-first finalization",
    );
    assert_contains(AGENT_CMD_DOC_ZH, "ZH", "manifest 尚无终态标记");
    assert_contains(AGENT_CMD_DOC_ZH, "ZH", "只统计实际成功删除");
    assert_contains(AGENT_CMD_DOC_ZH, "ZH", "保留其他类型");
    assert_contains(AGENT_CMD_DOC_ZH, "ZH", "终态 manifest");
    assert_contains(
        COMPATIBILITY_DOC,
        "COMPATIBILITY",
        "corrupt-state recovery requires a valid own terminal manifest",
    );
}

#[test]
fn rg03_controlled_artifact_read_docs_contract() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("agent zh", AGENT_CMD_DOC_ZH),
        ("developer EN", AGENT_DEV_DOC),
        ("tracing zh", AGENT_DOC),
    ] {
        for term in [
            "--reasoning-artifact",
            "reasoning_artifact_export",
            "ReasoningDecryptor",
            "reasoning_artifacts[]",
            "32 MiB",
            "0600",
            "None",
            "Windows",
            "signature",
            "redactedData",
            "reasoningEncryptedContent",
        ] {
            assert_contains(document, label, term);
        }
    }
    for document in [AGENT_CMD_DOC_EN, AGENT_DEV_DOC] {
        assert_contains(
            document,
            "RG03 keys",
            "it neither owns nor guesses decryption keys",
        );
        assert_contains(
            document,
            "RG03 audit order",
            "before checkpoint lookup, artifact body access or file creation",
        );
        assert_contains(
            document,
            "RG03 complete bytes",
            "truncated prefixes are refused",
        );
    }
    for document in [AGENT_CMD_DOC_ZH, AGENT_DOC] {
        assert_contains(document, "RG03密钥", "不拥有也不猜测解密密钥");
        assert_contains(
            document,
            "RG03审计顺序",
            "先于 checkpoint 查询、正文读取和文件创建",
        );
        assert_contains(document, "RG03完整字节", "截断前缀不导出");
    }
}

#[test]
fn authorized_reasoning_carrier_docs_preserve_partial_and_source_gates() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("developer EN", AGENT_DEV_DOC),
    ] {
        for term in [
            "disabled by default",
            "partial with zero artifacts",
            "before object or index writes",
            "cannot expose payload bytes or mint native authority",
            "before writing an envelope",
            "does not advance sync_revision",
            "Source A remains UNPROVEN",
        ] {
            assert_contains(document, label, term);
        }
    }
    for (label, document) in [("agent ZH", AGENT_CMD_DOC_ZH), ("tracing", AGENT_DOC)] {
        for term in [
            "默认为 None",
            "即使零 artifact",
            "不暴露载荷、不签发 native 权限",
            "envelope 写入前拒绝",
            "不推进 sync_revision",
            "Source A 仍为 UNPROVEN",
        ] {
            assert_contains(document, label, term);
        }
    }
    for term in [
        "zero-artifact partials",
        "one sealed owner",
        "Source A UNPROVEN",
        "No new public metadata enum value",
    ] {
        assert_contains(COMPATIBILITY_DOC, "compatibility", term);
    }
}

#[test]
fn private_reasoning_import_transport_docs_preserve_native_gate() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("developer EN", AGENT_DEV_DOC),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        for term in [
            "authenticated private import transport",
            "one-request key",
            "complete native proof",
            "Source A remains UNPROVEN",
        ] {
            assert_contains(document, label, term);
        }
    }
    for (label, document) in [("agent ZH", AGENT_CMD_DOC_ZH), ("tracing", AGENT_DOC)] {
        for term in [
            "认证的私有导入传输",
            "单请求密钥",
            "完整 native proof",
            "Source A 仍为 UNPROVEN",
        ] {
            assert_contains(document, label, term);
        }
    }
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("developer EN", AGENT_DEV_DOC),
        ("agent ZH", AGENT_CMD_DOC_ZH),
        ("tracing", AGENT_DOC),
    ] {
        assert_absent(
            document,
            label,
            "until the authenticated reasoning import transport is ready",
        );
        assert_absent(
            document,
            label,
            "authenticated reasoning import transport 就绪前",
        );
    }
}

#[test]
fn pending_reasoning_v2_docs_keep_replay_and_native_gates() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("developer EN", AGENT_DEV_DOC),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        for term in [
            "Pending reasoning v2 codec (OG-17)",
            "Ordinary pending payloads remain byte-for-byte v1",
            "outer header and envelope remain v1",
            "64 MiB envelope",
            "original whole-envelope MAC",
            "current catalog, claim and marker fences",
            "only reserved-turn artifacts",
            "without reopening a provider source or minting a replacement key",
            "A decoded DTO or envelope MAC alone never proves native provider origin",
            "Production native issuers and real OpenCode Source A evidence remain disabled",
        ] {
            assert_contains(document, label, term);
        }
    }
    for (label, document) in [("agent ZH", AGENT_CMD_DOC_ZH), ("tracing", AGENT_DOC)] {
        for term in [
            "Pending reasoning v2 codec（OG-17）",
            "普通 pending 仍逐字节编码为 v1",
            "外层 header/envelope 保持 v1",
            "64 MiB 总包",
            "原完整 envelope MAC",
            "当前 catalog、claim、marker 围栏",
            "本次 reserved turn 的 artifact",
            "不重开 provider 来源，也不补造私钥",
            "DTO 或 envelope MAC 本身不证明 provider native 来源",
            "生产 native issuer 与真实 OpenCode Source A 证据仍未放行",
        ] {
            assert_contains(document, label, term);
        }
    }
}

#[test]
fn reasoning_identity_gc_docs_preserve_scope_and_source_boundaries() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("developer EN", AGENT_DEV_DOC),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        for term in [
            "agent_reasoning_identity",
            "including a damaged value containing a JSON object ID or a nested object ID",
            "This exclusion requires no private reasoning key",
            "capture pending parent roots retain their existing rules",
            "retain both the private rows and this exclusion",
            "does not create identity records, issue native provenance, or enable OpenCode reasoning archival",
        ] {
            assert_contains(document, label, term);
        }
    }
    for (label, document) in [("agent ZH", AGENT_CMD_DOC_ZH), ("tracing", AGENT_DOC)] {
        for term in [
            "agent_reasoning_identity",
            "或包含嵌套对象 ID",
            "该隔离不需要私有 reasoning 密钥",
            "capture pending 父提交根继续遵守原有规则",
            "同时保留私有记录与该隔离",
            "不创建 identity 记录、不签发 native 来源证明",
            "也不启用 OpenCode reasoning 归档",
        ] {
            assert_contains(document, label, term);
        }
    }
}

#[test]
fn reasoning_identity_reservation_errors_and_session_limits_are_documented() {
    for (label, document) in [("agent EN", AGENT_CMD_DOC_EN)] {
        for term in [
            "reasoning coverage is partial; run libra agent doctor before retrying capture",
            "reasoning_identity_changed",
            "4096 records",
            "1 MiB",
            "identity-v1",
            "identity-v2",
            "start a new capture session",
            "An absent map is the legacy state",
            "candidate-free ordinary turns retain their existing coverage behavior",
            "first eligible identity-bearing turn may create a map",
            "rejects the entire reservation",
            "private reasoning identity is invalid; start a new capture session",
            "before any claim write, including candidate-free turns",
            "an import receipt records this as failed",
            "Doctor does not repair an invalid identity map",
            "Unproved OpenCode reasoning remains partial and is not archived",
        ] {
            assert_contains(document, label, term);
        }
    }
    for (label, document) in [("agent ZH", AGENT_CMD_DOC_ZH)] {
        for term in [
            "reasoning coverage is partial; run libra agent doctor before retrying capture",
            "reasoning_identity_changed",
            "4096 条记录",
            "1 MiB",
            "identity-v1",
            "identity-v2",
            "start a new capture session",
            "map 缺失属于 legacy 状态",
            "没有 candidate 的普通 turn 保持原 coverage 行为",
            "首个符合准入条件且带 identity 的 turn 可以创建 map",
            "整个 reservation 在任何 claim 写入前",
            "private reasoning identity is invalid; start a new capture session",
            "包括不含 candidate 的 turn",
            "import receipt 会记录为 failed",
            "doctor 不会修复无效 identity map",
            "未获证明的 OpenCode reasoning 保持 partial，不会归档",
        ] {
            assert_contains(document, label, term);
        }
    }
}

#[test]
fn opencode_verified_partial_live_capture_keeps_native_and_replay_closed() {
    for (label, document) in [
        ("agent EN", AGENT_CMD_DOC_EN),
        ("compatibility", COMPATIBILITY_DOC),
    ] {
        let (_, section) = document
            .split_once("### OpenCode ordinary partial live checkpoints")
            .expect("ordinary partial live capture must have its own contract section");
        for term in [
            "`Unproven` / `Partial`",
            "known non-reasoning text or message part",
            "only metadata or erased reasoning",
            "strict single-JSON parsing",
            "successful exporter",
            "exact session mapping",
            "repository-scoped",
            "source HMAC binding",
            "`info.id`",
            "provider session",
            "`repo_id`",
            "agent kind at bind time",
            "actual checkpoint",
            "HMAC is an opaque string",
            "Generic ordinary consumers stay closed to `Unproven`",
            "associated reasoning text and time",
            "original `originProof` key anywhere",
            "or a sealed candidate prevents this fallback",
            "metadata flag",
            "`source_authorization_mismatch`",
            "exporter authorization has already",
            "Source A remains UNPROVEN",
            "No reasoning artifacts, private",
            "Complete coverage claims or native ACK",
            "source input cap bounds each snapshot independently",
            "up to 16 MiB",
            "no per-session total cap",
            "idle count × source size",
            "legacy classic `info` / `parts` exports remain excluded",
            "Checkpoint/export readers in this build",
            "Older-binary compatibility requires a separate actual",
            "no cross-idle deduplication protocol",
            "generation, lease, scoped",
            "ref-CAS fences",
            "verified durable partial checkpoint write",
            "content-free pending intent",
            "ACK or generation advance",
            "no durable partial payload replay",
            "`LBR-AGENT-020`",
            "does not enable import or native admission",
            "single redacted JSON document",
            "no new",
            "public reason or metadata enum value",
            "V2 import rule requiring partial",
            "zero bytes and zero redaction metrics",
            "The official 0.30.49 release includes this ordinary live fix",
            "historical OpenCode imports, sessions containing unsupported reasoning state",
            "remain rejected with `LBR-AGENT-020`; their repair is deferred",
            "Release completion does not prove native source",
            "locally on macOS",
            "CI; synthetic tests do not prove native source",
        ] {
            assert_contains(section, label, term);
        }
        for opaque_hint in [
            "`reasoningField`",
            "`signature`",
            "`redactedData`",
            "`reasoningEncryptedContent`",
            "`thoughtSignature`",
            "`originProof`",
        ] {
            assert_contains(section, label, opaque_hint);
        }
    }
    for (label, document) in [("agent ZH", AGENT_CMD_DOC_ZH), ("tracing", AGENT_DOC)] {
        let (_, section) = document
            .split_once("### OpenCode 普通 partial live checkpoint")
            .expect("ordinary partial live capture must have its own contract section");
        for term in [
            "`Unproven` / `Partial`",
            "已知非 reasoning",
            "只有 metadata 或已擦除 reasoning 骨架的源仍拒绝",
            "严格单 JSON",
            "exporter 授权",
            "精确会话映射",
            "有界脱敏",
            "repository-scoped source HMAC",
            "`info.id`",
            "`repo_id`",
            "agent kind",
            "实际 checkpoint request 比较",
            "HMAC 是 opaque string",
            "generic ordinary consumer 继续拒绝",
            "关联 reasoning text/time",
            "原始解析树任意",
            "错型或未知嵌套值",
            "sealed candidate",
            "metadata flag 不能授此权限",
            "`source_authorization_mismatch`",
            "exporter 授权已经成功",
            "Source A 仍为 UNPROVEN",
            "不创建 reasoning artifact",
            "私有 reasoning identity、Complete coverage claim 或 native ACK",
            "source 输入单独受上限约束",
            "最多 16 MiB",
            "没有累计总量上限",
            "idle 次数 × source 大小",
            "旧 classic `info` / `parts` export 仍排除",
            "本构建的 checkpoint/export 读端",
            "旧二进制兼容必须另外实测读回",
            "重复 whole-source snapshot",
            "不新增跨 idle deduplication 协议",
            "generation、lease、scoped tombstone 和 ref-CAS",
            "partial checkpoint 写入后",
            "content-free pending intent",
            "不产生成功 ACK、不提前推进 generation",
            "不支持 durable partial payload replay",
            "`LBR-AGENT-020`",
            "live 修复不启用",
            "import 或 native admission",
            "单个已脱敏 JSON document",
            "无新增",
            "public reason/metadata enum value",
            "V2 import 对 partial projection 的 zero bytes/zero redaction metrics 规则不放宽",
            "正式 0.30.49 已包含普通 live 修复",
            "正式发布不证明 native 来源",
            "含 unsupported reasoning state 的历史 OpenCode import",
            "仍以 `LBR-AGENT-020` 拒绝，修复已延期",
            "macOS 本机运行，不进 CI",
            "synthetic 不证明",
        ] {
            assert_contains(section, label, term);
        }
        for opaque_hint in [
            "`reasoningField`",
            "`signature`",
            "`redactedData`",
            "`reasoningEncryptedContent`",
            "`thoughtSignature`",
            "`originProof`",
        ] {
            assert_contains(section, label, opaque_hint);
        }
    }
}

#[test]
fn opencode29_ordinary_extension_is_distinct_from_frozen_native_source() {
    for (label, document, heading, import_condition, import_limit, source_limit, sample_limit) in [
        (
            "commands/agent.md",
            AGENT_CMD_DOC_EN,
            "### Current official OpenCode ordinary source baseline",
            "historical OpenCode imports, sessions containing unsupported reasoning state",
            "remain rejected with `LBR-AGENT-020`; their repair is deferred",
            "Source A remains UNPROVEN",
            "does not establish native fields, complete reasoning",
        ),
        (
            "commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "### 当前官方 OpenCode 普通来源基线",
            "含 unsupported reasoning state 的历史 OpenCode import",
            "仍以 `LBR-AGENT-020` 拒绝，修复已延期",
            "Source A 仍为 UNPROVEN",
            "不证明 native 字段、完整 reasoning",
        ),
        (
            "tracing/agent.md",
            AGENT_DOC,
            "### Current official OpenCode ordinary source baseline",
            "historical OpenCode imports, sessions containing unsupported reasoning state",
            "remain rejected with `LBR-AGENT-020`; their repair is deferred",
            "Source A remains UNPROVEN",
            "does not establish native fields, complete reasoning",
        ),
        (
            "COMPATIBILITY.md",
            COMPATIBILITY_DOC,
            "### Current official OpenCode ordinary source baseline",
            "historical OpenCode imports, sessions containing unsupported reasoning state",
            "remain rejected with `LBR-AGENT-020`; their repair is deferred",
            "Source A remains UNPROVEN",
            "does not establish native fields, complete reasoning",
        ),
    ] {
        let heading_line = format!("\n{heading}\n");
        assert_eq!(
            document.matches(heading_line.as_str()).count(),
            1,
            "{label} must have one current ordinary source baseline section"
        );
        let (_, section) = document
            .split_once(heading_line.as_str())
            .unwrap_or_else(|| {
                panic!("{label} must disclose its current ordinary source baseline")
            });
        let section = section.split("\n## ").next().unwrap_or(section);
        let section = section.split("\n### ").next().unwrap_or(section);
        let readable = section.split_whitespace().collect::<Vec<_>>().join(" ");
        for required in [
            "2.0.29",
            "042d0832f5f3a73eaa039a58a7d565776f6d4290",
            "2.0.26",
            "100",
            "599",
            "partial",
            "automatic capture",
            "`Unproven` / `Partial`",
            "unsupported reasoning state",
            "`LBR-AGENT-020`",
            "extraction.model",
            "empty-files consistency",
            import_condition,
            import_limit,
            source_limit,
            sample_limit,
        ] {
            assert_contains(&readable, label, required);
        }
    }
}

#[test]
fn grok_docs_contract_mentions_thinking_retain() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("docs/development/commands/agent.md", AGENT_DEV_DOC),
        ("docs/development/tracing/agent.md", AGENT_DOC),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        for required in [
            "updates.jsonl",
            "Thinking",
            "encrypted_content",
            "support_wave",
            "grok",
            "checkpoint show",
        ] {
            assert_contains(document, label, required);
        }
        let lower = document.to_ascii_lowercase();
        assert!(
            lower.contains("paused") || document.contains("暂停"),
            "{label} must disclose that automatic Grok Thinking capture is paused"
        );
        assert!(
            lower.contains("scratch") || document.contains("临时状态"),
            "{label} must distinguish offline observer state from real checkpoints"
        );
        assert!(
            lower.contains("not decrypt")
                || lower.contains("不解密")
                || lower.contains("does not decrypt")
                || document.contains("不解密"),
            "{label} must say the ciphertext is not decrypted"
        );
        assert!(
            lower.contains("not a review")
                || lower.contains("not a `review`")
                || document.contains("不是 review")
                || document.contains("不是 `review`")
                || document.contains("not a review or investigate"),
            "{label} must say grok is not a review or investigate launcher"
        );
    }
}

#[cfg(feature = "test-live-agent")]
#[test]
#[ignore = "local-only Codex documentation contract; never run in CI"]
fn codex_ordinary_attached_and_legacy_repair_docs_stay_visible() {
    for (label, document) in [
        ("docs/commands/agent.md", AGENT_CMD_DOC_EN),
        ("docs/commands/zh-CN/agent.md", AGENT_CMD_DOC_ZH),
        ("docs/development/commands/agent.md", AGENT_DEV_DOC),
        ("docs/development/tracing/agent.md", AGENT_DOC),
        ("COMPATIBILITY.md", COMPATIBILITY_DOC),
    ] {
        for required in [
            "encrypted_content",
            "codex_legacy_reasoning_revision",
            "doctor --repair",
            "--codex-legacy-after",
        ] {
            assert_contains(document, label, required);
        }
    }
}

/// PI-10 AC8: every agent document that describes Pi discloses the explicit
/// import as local-only and its result as partial, and none still says the
/// import refuses `pi`.
#[test]
fn pi_observe_docs_disclose_trust_gate_and_no_install() {
    for (label, document, required) in [
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            &[
                "`libra agent hooks pi observe`",
                "only while Pi reports the project as trusted",
                "stores only lifecycle metadata",
                "no Pi content",
                "`agent enable` does not install that extension yet",
            ][..],
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            &[
                "`libra agent hooks pi observe`",
                "只在 Pi 报告项目受信时",
                "只为会话开始与 tool 事件保存生命周期元数据",
                "不保存任何 Pi 内容",
                "`agent enable` 暂不安装该 extension",
            ][..],
        ),
        (
            "COMPATIBILITY.md",
            COMPATIBILITY_DOC,
            &[
                "`agent hooks pi observe`",
                "while the project is trusted",
                "`agent enable` does not install the extension yet",
            ][..],
        ),
        (
            "docs/development/commands/agent.md",
            AGENT_DEV_DOC,
            &[
                "`HookProvider::lower_native_frame`",
                "`write_owned_pi_asset`",
            ][..],
        ),
        (
            "docs/development/tracing/agent.md",
            AGENT_DOC,
            &[
                "`agent hooks pi observe`",
                "`HookProvider::lower_native_frame`",
            ][..],
        ),
    ] {
        for text in required {
            assert!(
                document.contains(text),
                "{label} must disclose the Pi observe hook: {text}"
            );
        }
    }
    // The observe hook stores lifecycle metadata, so no page may still say
    // that only the explicit import stores Pi sessions at all.
    for (label, document, stale) in [
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "only an explicit import stores its sessions.",
        ),
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "The one way to store a `pi` session is",
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "只有显式 import 会写入其会话。",
        ),
        (
            "COMPATIBILITY.md",
            COMPATIBILITY_DOC,
            "The only writer is an explicit",
        ),
        (
            "docs/development/commands/agent.md",
            AGENT_DEV_DOC,
            "only the explicit import of PI-10 stores its sessions.",
        ),
    ] {
        assert!(
            !document.contains(stale),
            "{label} still says only the import stores Pi sessions: {stale}"
        );
    }
}

#[test]
fn pi_explicit_import_docs_disclose_local_only_and_partial() {
    for (label, document, required) in [
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            &[
                "`agent import --agent pi --path <timestamp>_<session-id>.jsonl --yes`",
                "local-only",
                "marked partial and unproven",
            ][..],
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            &[
                "`agent import --agent pi --path <timestamp>_<session-id>.jsonl --yes`",
                "仅本地",
                "标为 partial 且 unproven",
            ][..],
        ),
        (
            "COMPATIBILITY.md",
            COMPATIBILITY_DOC,
            &[
                "`agent import --agent pi --path <timestamp>_<session-id>.jsonl --yes`",
                "local-only",
                "as partial and unproven",
            ][..],
        ),
        (
            "docs/development/commands/agent.md",
            AGENT_DEV_DOC,
            &["`agent import --agent pi --path <timestamp>_<id>.jsonl --yes`"][..],
        ),
        (
            "docs/development/tracing/agent.md",
            AGENT_DOC,
            &["`agent import --agent pi --path <timestamp>_<id>.jsonl --yes`"][..],
        ),
    ] {
        for text in required {
            assert!(document.contains(text), "{label} must say {text:?}");
        }
        for stale in [
            "`agent import --agent pi` is rejected",
            "`agent import --agent pi` 被拒绝",
        ] {
            assert!(!document.contains(stale), "{label} still says {stale:?}");
        }
    }
}
