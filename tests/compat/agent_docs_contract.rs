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
fn macos_opencode_export_is_explicitly_unsupported() {
    assert!(
        AGENT_DOC.contains("macOS")
            && AGENT_DOC.contains("macOS 不支持 OpenCode 内容导出")
            && AGENT_DOC.contains("启动 `sandbox-exec` 或 exporter **之前**")
            && AGENT_DOC.contains("metadata-only"),
        "tracing/agent.md must disclose the macOS no-spawn, metadata-only boundary",
    );
    assert!(
        AGENT_CMD_DOC_EN.contains("macOS")
            && AGENT_CMD_DOC_EN.contains("content export is unsupported")
            && AGENT_CMD_DOC_EN.contains("before it spawns `sandbox-exec` or the exporter")
            && AGENT_CMD_DOC_EN.contains("metadata-only"),
        "docs/commands/agent.md must disclose the macOS no-spawn, metadata-only boundary",
    );
    assert!(
        AGENT_CMD_DOC_ZH.contains("macOS")
            && AGENT_CMD_DOC_ZH.contains("不支持 OpenCode 内容导出")
            && AGENT_CMD_DOC_ZH.contains("启动 `sandbox-exec` 或 exporter **之前**")
            && AGENT_CMD_DOC_ZH.contains("metadata-only"),
        "docs/commands/zh-CN/agent.md must disclose the macOS no-spawn, metadata-only boundary",
    );
    assert!(
        AGENT_DEV_DOC.contains("macOS")
            && AGENT_DEV_DOC.contains("content export is unsupported")
            && AGENT_DEV_DOC.contains("before it")
            && AGENT_DEV_DOC.contains("metadata-only"),
        "docs/development/commands/agent.md must disclose the macOS no-spawn, metadata-only boundary",
    );
    assert!(
        COMPATIBILITY_DOC.contains("macOS")
            && COMPATIBILITY_DOC.contains("explicitly unsupported")
            && COMPATIBILITY_DOC.contains("before spawning `sandbox-exec` or the exporter")
            && COMPATIBILITY_DOC.contains("metadata-only"),
        "COMPATIBILITY.md must disclose the macOS no-spawn, metadata-only boundary",
    );

    for (label, document, stale) in [
        (
            "docs/development/tracing/agent.md",
            AGENT_DOC,
            "**macOS 支持**内容捕获",
        ),
        (
            "docs/commands/agent.md",
            AGENT_CMD_DOC_EN,
            "On macOS the sandboxed export uses seatbelt",
        ),
        (
            "docs/commands/zh-CN/agent.md",
            AGENT_CMD_DOC_ZH,
            "macOS 经 seatbelt（`sandbox-exec`）启用内容捕获",
        ),
        (
            "docs/development/commands/agent.md",
            AGENT_DEV_DOC,
            "OpenCode content capture on **macOS** is assembled through seatbelt",
        ),
        (
            "COMPATIBILITY.md",
            COMPATIBILITY_DOC,
            "OpenCode export on macOS uses seatbelt",
        ),
    ] {
        assert_absent(document, label, stale);
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
        "requires a Unix host to initialize",
    );
    assert_contains(
        HOOKS_CMD_DOC_EN,
        "docs/commands/hooks.md",
        "prints the path-free Unix-host remedy to stderr and exits `0`",
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
    // The non-Unix capability result is scoped to in-repository callbacks;
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
        "当前要求 Unix host",
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
