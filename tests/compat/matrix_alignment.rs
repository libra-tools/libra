mod matrix_alignment_support;

use std::{fs, process::Command};

use matrix_alignment_support::{
    assert_contains, cli_commands, code_router_routes, command_development_public_commands,
    command_development_unpublished_docs, compatibility_commands, declared_cargo_targets,
    declared_features, plan_features, plan_test_targets, quarantine_tests, read_repo_file,
    repo_root,
};

#[test]
fn compatibility_matrix_matches_cli_commands() {
    let cli = cli_commands();
    let compat = compatibility_commands();
    let missing = cli.difference(&compat).cloned().collect::<Vec<_>>();
    let extra = compat.difference(&cli).cloned().collect::<Vec<_>>();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "COMPATIBILITY.md top-level command matrix is out of sync with src/cli.rs::Commands.\nmissing from COMPATIBILITY.md: {missing:?}\nlisted in COMPATIBILITY.md but absent from src/cli.rs::Commands: {extra:?}"
    );
}

#[test]
fn send_email_policy_is_explicit_and_non_sending() {
    assert!(
        !cli_commands().contains("send-email"),
        "P2-04 / D19 requires send-email to remain absent until a reviewed transport RFC lands"
    );

    let outside_repo = tempfile::tempdir().expect("create no-repository send-email test directory");
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(["send-email", "--dry-run", "0001-example.patch"])
        .current_dir(outside_repo.path())
        .output()
        .expect("run libra send-email negative-path guard");
    assert_eq!(
        output.status.code(),
        Some(129),
        "an absent send-email command must use the stable CLI-error exit"
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_contains(
        &diagnostic,
        "'send-email' is not a libra command",
        "send-email runtime rejection",
    );
    assert_contains(
        &diagnostic,
        "LBR-CLI-001",
        "send-email stable runtime error code",
    );

    let compat = read_repo_file("COMPATIBILITY.md");
    assert_contains(
        &compat,
        "| send-email | unsupported | P2-04 / D19 policy:",
        "COMPATIBILITY.md absent-command matrix",
    );
    assert_contains(
        &compat,
        "never reads `sendemail.*` configuration, SMTP credentials, or contacts a mail server",
        "COMPATIBILITY.md no-network boundary",
    );

    let user_doc = read_repo_file("docs/commands/send-email.md");
    for needle in [
        "Libra does not implement SMTP",
        "git send-email --dry-run",
        "LBR-CLI-001",
    ] {
        assert_contains(&user_doc, needle, "docs/commands/send-email.md");
    }

    let zh_user_doc = read_repo_file("docs/commands/zh-CN/send-email.md");
    for needle in [
        "Libra 不实现 SMTP 投递",
        "git send-email --dry-run",
        "LBR-CLI-001",
    ] {
        assert_contains(&zh_user_doc, needle, "docs/commands/zh-CN/send-email.md");
    }

    let development_doc = read_repo_file("docs/development/commands/send-email.md");
    assert_contains(
        &development_doc,
        "No `Commands::SendEmail` variant",
        "docs/development/commands/send-email.md",
    );
    assert_contains(
        &read_repo_file("docs/development/commands/_compatibility.md"),
        "### D19：`send-email` SMTP 传输",
        "send-email governance decision",
    );
}

#[test]
fn command_development_readme_matches_public_cli_surface() {
    let cli = cli_commands();
    let compat = compatibility_commands();
    let public_docs = command_development_public_commands();
    let unpublished_docs = command_development_unpublished_docs();

    let missing_from_docs = cli.difference(&public_docs).cloned().collect::<Vec<_>>();
    let extra_in_docs = public_docs.difference(&cli).cloned().collect::<Vec<_>>();
    let unpublished_but_public = unpublished_docs
        .intersection(&cli)
        .cloned()
        .collect::<Vec<_>>();
    let unpublished_but_compatible = unpublished_docs
        .intersection(&compat)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        missing_from_docs.is_empty()
            && extra_in_docs.is_empty()
            && unpublished_but_public.is_empty()
            && unpublished_but_compatible.is_empty(),
        "docs/development/commands/README.md command tables are out of sync with src/cli.rs::Commands and COMPATIBILITY.md.\nmissing public docs: {missing_from_docs:?}\nextra public docs: {extra_in_docs:?}\nunpublished docs exposed in CLI: {unpublished_but_public:?}\nunpublished docs exposed in COMPATIBILITY.md: {unpublished_but_compatible:?}"
    );

    for command in public_docs.union(&unpublished_docs) {
        // agent/code development docs live under docs/development/tracing/ since the
        // 932c3a0 reorganization; their README rows link there instead of this directory.
        let relative = if command == "agent" || command == "code" {
            format!("docs/development/tracing/{command}.md")
        } else {
            format!("docs/development/commands/{command}.md")
        };
        let path = repo_root().join(&relative);
        assert!(
            path.is_file(),
            "command development README links to missing document: {}",
            path.display()
        );
    }
}

#[test]
fn integration_test_plan_references_existing_targets_and_features() {
    let repo = repo_root();
    let cargo_targets = declared_cargo_targets();
    let missing_targets = plan_test_targets()
        .into_iter()
        .filter(|target| {
            !repo.join(format!("tests/{target}.rs")).is_file() && !cargo_targets.contains(target)
        })
        .collect::<Vec<_>>();
    let features = declared_features();
    let missing_features = plan_features()
        .into_iter()
        .filter(|feature| !features.contains(feature))
        .collect::<Vec<_>>();

    assert!(
        missing_targets.is_empty() && missing_features.is_empty(),
        "integration-test-plan.md references unknown targets or features\nunknown targets: {missing_targets:?}\nunknown features: {missing_features:?}"
    );

    for test in quarantine_tests() {
        let (target, test_fn) = test
            .split_once("::")
            .unwrap_or_else(|| panic!("quarantine test must use target::fn: {test}"));
        let path = repo.join(format!("tests/{target}.rs"));
        assert!(path.is_file(), "quarantine target file is missing: {test}");
        let body = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("read {}: {error}", path.display());
        });
        assert!(
            body.contains(&format!("fn {test_fn}")),
            "quarantine test function is missing: {test}"
        );
    }
}

#[test]
fn docs_consistency_covers_code_command_router_contracts() {
    let web_mod = read_repo_file("src/internal/ai/web/mod.rs");
    let code_doc = read_repo_file("docs/commands/code.md");
    let code_control_doc = read_repo_file("docs/commands/code-control.md");
    let integration_plan = read_repo_file("docs/development/integration/integration-test-plan.md");
    let agent_doc = read_repo_file("docs/development/tracing/agent.md");
    let workflow = read_repo_file(".github/workflows/base.yml");
    let compatibility = read_repo_file("COMPATIBILITY.md");
    let source_and_docs = [
        web_mod.as_str(),
        read_repo_file("src/internal/ai/web/code_ui.rs").as_str(),
        code_doc.as_str(),
        code_control_doc.as_str(),
    ]
    .join("\n");

    let routes = code_router_routes(&web_mod);
    assert!(
        !routes.is_empty(),
        "expected to extract /api/code routes from src/internal/ai/web/mod.rs"
    );
    for route in routes {
        assert_contains(&code_doc, &route, "docs/commands/code.md");
    }

    for header in [
        "X-Libra-Control-Token",
        "X-Code-Controller-Token",
        "X-Libra-Browser-Bootstrap",
    ] {
        assert_contains(&code_doc, header, "docs/commands/code.md");
        assert_contains(&source_and_docs, header, "source/docs control contract");
    }

    for code in [
        "CONTROL_DISABLED",
        "LOOPBACK_REQUIRED",
        "MISSING_CONTROL_TOKEN",
        "INVALID_CONTROL_TOKEN",
        "MISSING_BROWSER_BOOTSTRAP",
        "INVALID_BROWSER_BOOTSTRAP",
        "MISSING_CONTROLLER_TOKEN",
        "INVALID_CONTROLLER_TOKEN",
        "CONTROLLER_CONFLICT",
        "SESSION_BUSY",
        "INTERACTION_NOT_ACTIVE",
        "PLAN_REPAIR_RETRY_LIMIT_REACHED",
    ] {
        assert_contains(&source_and_docs, code, "source/docs control error contract");
    }

    for flag in [
        "--control",
        "--control-token-file",
        "--control-info-file",
        "--control-url",
        "--control stdio",
    ] {
        assert_contains(&code_doc, flag, "docs/commands/code.md");
    }

    // W3-13: keep the public mode/provider split for env-file/approval-ttl
    // pinned in the compatibility matrix (non-Codex web accept; Codex/--stdio reject).
    let code_row = compatibility
        .lines()
        .find(|line| line.starts_with("| code |"))
        .expect("COMPATIBILITY.md must include a `code` row");
    for needle in [
        "--env-file",
        "--approval-ttl",
        "non-Codex",
        "--web",
        "codex",
        "--stdio",
    ] {
        assert!(
            code_row.contains(needle),
            "COMPATIBILITY.md code row must document W3-13 web flag parity ({needle}); row={code_row}"
        );
    }

    for (body, needle, context) in [
        // W5-01: the shim is removed; its doc is now a migration note pointing
        // at the canonical client, and the JSON-RPC method reference moved to
        // docs/commands/code.md.
        (
            code_control_doc.as_str(),
            "libra code --control stdio",
            "docs/commands/code-control.md",
        ),
        (
            code_doc.as_str(),
            "diagnostics.get",
            "docs/commands/code.md",
        ),
        (
            integration_plan.as_str(),
            "test-provider",
            "docs/development/integration/integration-test-plan.md",
        ),
        (
            integration_plan.as_str(),
            "code_ui_scenarios",
            "docs/development/integration/integration-test-plan.md",
        ),
        (
            agent_doc.as_str(),
            "diagnostics_redaction_test",
            "docs/development/tracing/agent.md",
        ),
    ] {
        assert_contains(body, needle, context);
    }
    assert_contains(
        &workflow,
        "Run Code UI automation scenarios",
        ".github/workflows/base.yml",
    );
    assert_contains(
        &workflow,
        "Check Rustdoc intra-doc links",
        ".github/workflows/base.yml",
    );
    assert_contains(
        &workflow,
        "RUSTDOCFLAGS=\"-D rustdoc::broken_intra_doc_links\" cargo doc --no-deps --all-features",
        ".github/workflows/base.yml",
    );
    assert!(
        !workflow.contains("paths-ignore:"),
        ".github/workflows/base.yml must run the compatibility guards for workflow and command-documentation-only pull requests",
    );
    assert!(
        !workflow.contains("RUST_LOG:"),
        "Run Code UI automation scenarios must not set global RUST_LOG in CI"
    );
    for target in [
        "--test code_codex_default_web_test",
        "--test ai_code_ui_headless_test",
        "--test code_codex_runtime_test",
    ] {
        assert_contains(&workflow, target, ".github/workflows/base.yml");
    }

    for path in [
        "tests/harness/scenario.rs",
        "tests/diagnostics_redaction_test.rs",
        "tests/code_codex_default_web_test.rs",
        "tests/ai_code_ui_headless_test.rs",
        "tests/code_codex_runtime_test.rs",
    ] {
        assert!(
            repo_root().join(path).exists(),
            "required path is missing: {path}"
        );
    }
}

#[test]
fn w203_revision_receipt_and_network_boundary_stay_aligned() {
    let code_doc = read_repo_file("docs/commands/code.md");
    let zh_code_doc = read_repo_file("docs/commands/zh-CN/code.md");
    let error_codes = read_repo_file("docs/error-codes.md");
    let changelog = read_repo_file("CHANGELOG.md");
    let changelog_words = changelog.split_whitespace().collect::<Vec<_>>().join(" ");
    let plan = read_repo_file("docs/development/plan/plan-20260715.md");
    let test_index = read_repo_file("tests/INDEX.md");
    let compatibility = read_repo_file("COMPATIBILITY.md");
    let development_compatibility = read_repo_file("docs/development/commands/_compatibility.md");
    let tracing_doc = read_repo_file("docs/development/tracing/code.md");
    let jsonl = read_repo_file("src/internal/ai/session/jsonl.rs");
    let phase1 = read_repo_file("src/internal/ai/runtime/phase1.rs");
    let workspace_snapshot = read_repo_file("src/internal/ai/workspace_snapshot.rs");
    let code_command = read_repo_file("src/command/code.rs");
    let sse_wire = read_repo_file("src/internal/ai/web/sse_wire.rs");
    let web_admission = read_repo_file("src/internal/ai/web/web_admission.rs");
    let code_ui = read_repo_file("src/internal/ai/web/code_ui.rs");
    let headless = read_repo_file("src/internal/ai/web/headless.rs");
    let wire_tests = read_repo_file("tests/ai_code_ui_wire_test.rs");
    let jsonl_tests = read_repo_file("tests/ai_session_jsonl_test.rs");
    let scenario_tests = read_repo_file("tests/code_ui_scenarios.rs");
    let headless_tests = read_repo_file("tests/ai_code_ui_headless_test.rs");
    let compat_tests = read_repo_file("tests/compat/matrix_alignment.rs");

    for (body, context) in [
        (&code_doc, "docs/commands/code.md"),
        (&zh_code_doc, "docs/commands/zh-CN/code.md"),
    ] {
        for needle in [
            "intent_revision",
            "interaction_id",
            "sidecar_digest",
            "intent_revision_consumption",
            "kind: \"intent_revision_consumed\"",
            "PHASE1_WORKSPACE_CHANGED",
            "PLAN_EXECUTION_NOT_AVAILABLE",
            "PLAN_REVISION_NOTE_REQUIRED",
            "workspaceDrifted",
            "workspaceWarning",
            "metadata-v1:<sha256>",
            "Claiming",
        ] {
            assert_contains(body, needle, context);
        }
    }
    for needle in [
        "but never the raw note",
        "That exclusion applies to the workflow terminal and consumption",
        "It does not redact ordinary",
        "determinate pre-write validation/retry signals",
        "30-second cooperative work budget",
        "128 MiB",
        "Paths are streamed and counted",
        "blocking filesystem operation",
        "final EOF step",
        "Each new binding is",
        "metadata-only signal is advisory",
        "fall back to content comparison using",
        "explicit direct",
        "exactly one successful",
        "Padded or otherwise non-exact spellings",
        "one shared, linear\n`ValidatedIntentRevisionReceiptIndex`",
        "exactly 5,000 events and 700 receipts",
        "Path-name\nenumeration remains an ignore-aware walk",
        "`openat`/`fstatat`/`readlinkat`",
        "`FILE_FLAG_OPEN_REPARSE_POINT`",
        "`FSCTL_GET_REPARSE_POINT`",
        "Other platforms fail closed",
        "process-lifetime Phase 1 writer lease",
        "different lock from the\nshort-lived workflow append lock",
        "only one\nindependently constructed persistence graph",
    ] {
        assert_contains(&code_doc, needle, "docs/commands/code.md privacy boundary");
    }
    for needle in [
        "绝不写入原始 note 或 HMAC key",
        "这个排除",
        "它不会清除普通 transcript",
        "pre-write\n校验/重试信号",
        "30 秒 cooperative work budget",
        "累计 128 MiB 编码路径名",
        "路径会先流式计数并受限",
        "单次阻塞式文件系统",
        "包括最终 EOF",
        "每个新\nbinding 都在同一稳定区间内",
        "仅 metadata 变化时该信号只是提示",
        "回退到精确 content",
        "恰好一次成功调用",
        "不精确写法的 `/intent cancel`",
        "共享、线性的 `ValidatedIntentRevisionReceiptIndex`",
        "5,000 个 events 与 700 张 receipts",
        "路径名枚举仍由 ignore-aware\nwalker 完成",
        "`openat`/`fstatat`/`readlinkat`",
        "`FILE_FLAG_OPEN_REPARSE_POINT`",
        "`FSCTL_GET_REPARSE_POINT`",
        "其他平台因为不支持安全",
        "进程生命周期的\nPhase 1 writer lease",
        "只能构造一个独立 persistence graph",
    ] {
        assert_contains(
            &zh_code_doc,
            needle,
            "docs/commands/zh-CN/code.md privacy boundary",
        );
    }

    assert_contains(
        &jsonl,
        "pub sidecar_digest: String",
        "digest-only IntentRevisionRecovery schema",
    );
    assert_contains(
        &jsonl,
        "intent_revision_consumption: Option<IntentRevisionConsumption>",
        "additive consumption receipt schema",
    );
    for needle in [
        "file.try_lock()",
        "options.custom_flags(libc::O_NOFOLLOW)",
        "code_workflow_append_lock_path_matches_file",
        "latest_recoverable_intent_revision_attempt_before_claim",
        "validated_intent_revision_consumption_receipts(replay)",
        "struct ValidatedIntentRevisionReceiptIndex",
        "fn source_terminals(",
        "fn exact_source_terminal(",
        "fn receipts(",
        "fn exact_receipt_for_source(",
        "fn committed_consumer_status(",
        "fn claimed_intent_revision_consumer_status(",
    ] {
        assert_contains(&jsonl, needle, "W2-03 crash-recovery authority");
    }
    for needle in [
        "struct ClaimingIntentRevision",
        "prepare_claiming_intent_revision",
        "promote_claiming_intent_revision_after_admission",
        "rearm_cancelled_intent_revision_consumer",
        "LoadedIntentRevisionSidecar::Claiming",
    ] {
        assert_contains(&headless, needle, "private Claiming sidecar protocol");
    }
    for needle in [
        "Process-lifetime exclusive writer lease for a durable headless session",
        "fn claim_for_persistence(",
        "claimed: AtomicBool",
        "compare_exchange(false, true",
        "libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC",
        "FILE_FLAG_OPEN_REPARSE_POINT",
        "windows_file_identity(file)? == windows_file_identity(&current)?",
    ] {
        assert_contains(&headless, needle, "process-lifetime session writer lease");
    }
    let session_lease_position = code_command
        .find("let session_lease = HeadlessSessionPersistence::acquire_session_lease(")
        .expect("session writer lease acquisition must remain present");
    let session_reload_position = code_command
        .find("let session_state = match session_store.load(&requested_session_id)")
        .expect("post-lease session reload must remain present");
    let projection_fold_position = code_command
        .find(
            "fold_code_ui_resume_from_session(&session_store, &session_state, bootstrap_snapshot)",
        )
        .expect("post-lease projection fold must remain present");
    assert!(
        session_lease_position < session_reload_position
            && session_reload_position < projection_fold_position,
        "the session writer lease must be acquired before authoritative reload and projection fold"
    );
    let command_retry_position = web_admission
        .find("if browser_command_id_supplied")
        .expect("command-id preflight must remain present");
    let direct_gate_position = web_admission
        .find("WebTurnMode::ExplicitDirect\n                | WebTurnMode::IntentRevisionModify")
        .expect("ExplicitDirect revision gate must remain present");
    assert!(
        command_retry_position < direct_gate_position,
        "exact durable command retries must be acknowledged before blocking fresh ExplicitDirect execution"
    );
    let claim_position = web_admission
        .find("prepare_claiming_intent_revision(persistence, pending, consumer)")
        .expect("Claiming prewrite must remain present");
    let runtime_submit_position = web_admission
        .find("let submission = runtime")
        .expect("Runtime submission must remain present");
    assert!(
        claim_position < runtime_submit_position,
        "Claiming must be durable before Runtime can fsync its command intent"
    );
    let note_limit_position = web_admission
        .find("changes.len() > crate::internal::ai::session::MAX_INTENT_REVISION_NOTE_BYTES")
        .expect("IntentSpec Modify size validation must remain present");
    assert!(
        note_limit_position < claim_position,
        "the 16 KiB Modify limit must be enforced before Claiming"
    );
    assert_contains(
        &sse_wire,
        "\"intent_revision_consumed\"",
        "dedicated SSE receipt kind",
    );
    assert_contains(
        &web_admission,
        "HeadlessPhase1Command::StartPlanExecution",
        "Network Allow admits confirmed plan execution",
    );
    assert_contains(
        &headless,
        "submit_confirmed_plan_execution",
        "Web confirmed-plan handoff uses the runtime queue",
    );
    assert_contains(
        &phase1,
        "require_complete_phase1_recovery_replay(&replay)?",
        "strict replay before Phase 1 recovery mutation",
    );
    assert_contains(
        &code_ui,
        "(\"PLAN_EXECUTION_NOT_AVAILABLE\", 409)",
        "Code UI error catalogue",
    );
    for (needle, description) in [
        (
            "(\"PHASE1_WORKSPACE_CHANGED\", 409)",
            "workspace drift conflict catalogue",
        ),
        (
            "(\"PLAN_REVISION_NOTE_REQUIRED\", 400)",
            "empty revision note catalogue",
        ),
    ] {
        assert_contains(&code_ui, needle, description);
    }
    assert_contains(
        &code_doc,
        "| `PLAN_EXECUTION_NOT_AVAILABLE` | 409 |",
        "docs/commands/code.md public 409 row",
    );
    assert_contains(
        &zh_code_doc,
        "| `PLAN_EXECUTION_NOT_AVAILABLE` | 409 |",
        "docs/commands/zh-CN/code.md public 409 row",
    );
    assert_contains(
        &error_codes,
        "| `PLAN_EXECUTION_NOT_AVAILABLE` | `409` |",
        "docs/error-codes.md public 409 row",
    );
    for (needle, description) in [
        (
            "| `PHASE1_WORKSPACE_CHANGED` | `409` |",
            "docs/error-codes.md drift row",
        ),
        (
            "| `PLAN_REVISION_NOTE_REQUIRED` | `400` |",
            "docs/error-codes.md revision-note row",
        ),
        (
            "| `SESSION_BUSY` | `409` |",
            "docs/error-codes.md revision admission row",
        ),
        (
            "| `INVALID_QUERY_PARAM` | `400` |",
            "docs/error-codes.md revision size row",
        ),
    ] {
        assert_contains(&error_codes, needle, description);
    }
    for needle in [
        "A metadata-only `workspaceWarning` may pass that exact recheck",
        "No mutation starts; stale Execute preserves the pending gate",
        "different repository requires Cancel and a new request/IntentSpec review",
        "Failed recapture leaves the note unconsumed",
        "Revision authority remains pending and unconsumed",
        "Exact terminal retries and matching live `commandId` retries are the only revision-gate exceptions",
        "A padded or otherwise non-exact `/intent cancel`",
        "rejected before Claiming, Runtime admission, or any workflow append",
    ] {
        assert_contains(&error_codes, needle, "docs/error-codes.md W2-03 semantics");
    }
    for (body, context) in [
        (&code_doc, "docs/commands/code.md public error table"),
        (
            &zh_code_doc,
            "docs/commands/zh-CN/code.md public error table",
        ),
    ] {
        for needle in [
            "| `PHASE1_WORKSPACE_CHANGED` | 409 |",
            "| `PLAN_REVISION_NOTE_REQUIRED` | 400 |",
            "| `SESSION_BUSY` | 409 |",
            "| `INVALID_QUERY_PARAM` | 400 |",
        ] {
            assert_contains(body, needle, context);
        }
    }

    for needle in [
        "workspace_snapshot_fingerprint",
        "workspace_snapshot_metadata_fingerprint",
        "workspace_change_matches",
        "same_intent_repository_as",
        "validate_same_intent_repository",
        "Fast drift hint used for projections and pre-write retries",
        "authorization always compares the content fingerprint above",
        "#[serde(default)]\n    pub workspace_change_token: String",
        "return self.workspace_matches(working_dir).await",
        "a matching metadata hint must never authorize stale content",
        "workspace_snapshot_stable_fingerprints_with_post_content_hook",
        "phase1_capture_rejects_change_between_content_and_metadata_scans",
    ] {
        assert_contains(&phase1, needle, "Phase 1 dual-token authority");
    }
    for needle in [
        "FINGERPRINT_MAX_ENTRIES: usize = 1_000_000",
        "FINGERPRINT_MAX_PATH_BYTES: usize = 128 * 1024 * 1024",
        "FINGERPRINT_MAX_DURATION: Duration = Duration::from_secs(30)",
        "collect_fingerprint_paths",
        "let next = walker.next();",
        "paths.sort();",
        "workspace_snapshot_stable_fingerprints_with_post_content_hook",
        "content_fingerprint_detects_same_length_change_with_restored_mtime",
        "metadata_fingerprint_changes_without_reading_file_bodies",
        "fingerprint_budget_counts_directories_and_propagates_entry_limit",
        "fingerprint_budget_propagates_path_name_limit",
        "fingerprint_budget_timeout_is_typed_and_actionable",
        "fingerprint_budget_streams_wide_directory_before_rejecting",
        "bounded_manifest_order_matches_legacy_sorted_walk",
        "stable_fingerprint_pair_rejects_change_after_content_scan",
        "cooperative work budget",
        "FingerprintCheckpoint::AfterWalkStep",
        "FingerprintCheckpoint::AfterFileOpen",
        "FingerprintCheckpoint::BeforeExactReturn",
        "libc::openat(",
        "libc::fstatat(",
        "libc::readlinkat(",
        "FILE_FLAG_OPEN_REPARSE_POINT",
        "FSCTL_GET_REPARSE_POINT",
        "windows_final_path",
        "secure workspace fingerprint reads are unsupported on this platform",
        "content_fingerprint_rejects_file_swapped_to_external_symlink_before_read",
        "content_fingerprint_rejects_file_swapped_to_fifo_before_read",
        "metadata_fingerprint_reuses_pinned_root_after_workspace_path_replacement",
        "windows_reparse_parser_rejects_truncated_odd_and_out_of_range_records",
    ] {
        assert_contains(
            &workspace_snapshot,
            needle,
            "bounded exact workspace fingerprint",
        );
    }
    let bounded_walk_start = workspace_snapshot
        .find("fn fingerprint_walk_builder")
        .expect("fingerprint walk builder must remain present");
    let bounded_walk_end = workspace_snapshot
        .find("fn enforce_fingerprint_budget")
        .expect("fingerprint budget helper must remain present");
    let bounded_walk = &workspace_snapshot[bounded_walk_start..bounded_walk_end];
    assert!(
        !bounded_walk.contains(".sort_by_file_path("),
        "bounded production fingerprint walk must not sort before enforcing entry/path budgets"
    );
    let pull_position = bounded_walk
        .find("let next = walker.next();")
        .expect("bounded walk must pull one entry at a time");
    let sort_position = bounded_walk
        .find("paths.sort();")
        .expect("bounded manifest must sort only after collection");
    assert!(
        pull_position < sort_position,
        "fingerprint entries must be streamed and bounded before deterministic sorting"
    );
    for needle in [
        "validate_same_intent_repository(",
        "The Intent repository changed since this plan was generated",
        ".validate_exact(\n                self.turn_executor.registry.working_dir(),",
        "Execute will perform an exact identity and content recheck",
    ] {
        assert_contains(&headless, needle, "Web Execute consumes exact authority");
    }

    for needle in [
        "Phase 1 Plan revision and Plan/network-policy gates are durable in the Web workflow",
        "IntentSpec revision uses Prepared/Active/Claiming/Consuming authority",
        "exactly one successful draft submission and a durable replacement-review marker",
        "retain an exact content fingerprint for Execute and add a `metadata-v1:<sha256>` resume/pre-write signal token",
        "A metadata-only warning may pass Execute's exact identity/content recheck; stale Execute returns `PHASE1_WORKSPACE_CHANGED`, preserves the gate, and performs no mutation",
        "new bindings capture metadata-before/exact-before/exact-after/metadata-after and reject any mismatch",
        "older contexts without the token remain readable and fall back to exact content comparison",
        "30-second / 1,000,000-traversed-entry / 128-MiB-encoded-path-name cooperative work budgets",
        "directories count before a bounded manifest is sorted",
        "blocking filesystem calls plus final EOF fail closed after control returns",
        "one shared linear `ValidatedIntentRevisionReceiptIndex`",
        "5,000-event/700-receipt regression permits at most four indexed relationship visits per event",
        "A valid receipt permanently closes its exact retry lineage",
        "restart heals stale cancel/replacement browser projection without rerunning the provider",
        "Fresh explicit-direct input, including padded `/intent cancel`, receives `SESSION_BUSY`",
        "Modify input above 16 KiB is rejected with `INVALID_QUERY_PARAM` before Claiming",
        "Path-name enumeration remains an ignore-aware walk and is not read authority",
        "Unix authoritative reads use pinned root/parent descriptors with `openat`/`fstatat`/`readlinkat`",
        "Windows uses pinned handles, `FILE_FLAG_OPEN_REPARSE_POINT`, final-path/file-identity checks, and `FSCTL_GET_REPARSE_POINT`",
        "unsupported platforms fail closed",
        "Empty revision notes return `PLAN_REVISION_NOTE_REQUIRED` and remain unconsumed",
        "Network Allow admits confirmed plan execution onto the serialized AgentRuntime queue; mutating tools still require approval/sandbox/ACL, and classified failures enter the W2-11 repair loop. The catalogued `PLAN_EXECUTION_NOT_AVAILABLE` 409 is retained for older clients and is no longer produced on Allow",
        "persistent regular-file OS append lock is never age-reclaimed or unlinked",
        "distinct process-lifetime Phase 1 writer lease is acquired before reload/fold",
        "It is not the append lock or browser controller lease",
    ] {
        assert_contains(&compatibility, needle, "COMPATIBILITY.md code row");
    }
    for needle in [
        "W2-04 confirmed-plan execution in the default Web runtime",
        "IntentSpec revision uses Prepared/Active/Claiming/Consuming authority",
        "exactly one successful draft submission and a durable replacement-review marker",
        "exact Execute content fingerprint plus an additive `metadata-v1` resume/pre-write signal token",
        "metadata-only warnings may pass Execute's exact recheck",
        "new bindings use a metadata-before/exact-before/exact-after/metadata-after stable interval",
        "contexts without that token remain readable and fall back to exact content comparison",
        "30-second / 1,000,000-traversed-entry / 128-MiB-encoded-path-name cooperative work budgets",
        "directory entries are capped before deterministic sorting",
        "blocking filesystem calls plus final EOF fail closed after control returns",
        "One linear `ValidatedIntentRevisionReceiptIndex` supplies source/receipt/consumer/marker/status authority",
        "5,000-event/700-receipt regression caps relationship visits at four per event",
        "A committed receipt permanently closes exact retry lineage",
        "restart heals stale cancel/replacement projection without rerunning the provider",
        "Fresh explicit-direct input (including padded cancel) is `SESSION_BUSY`",
        "16 KiB Modify limit is checked before Claiming",
        "Path enumeration remains ignore-aware and path-based but is not read authority",
        "Unix authoritative reads use pinned root/parent fds with `openat`/`fstatat`/`readlinkat`",
        "Windows uses pinned handles, `FILE_FLAG_OPEN_REPARSE_POINT`, final path/file identity, and `FSCTL_GET_REPARSE_POINT`",
        "unsupported platforms fail closed",
        "distinct process-lifetime Phase 1 writer lease is acquired before reload/fold",
        "separate from append/controller locks",
        "do not treat a `PHASE1_WORKSPACE_CHANGED` drift 409 as execution parity",
        "W2-04 confirmed-plan execution is implemented and tested",
        "Startup rejects sequence gaps/window cuts before gate authority or context GC",
    ] {
        assert_contains(
            &development_compatibility,
            needle,
            "docs/development/commands/_compatibility.md Code row",
        );
    }
    for needle in [
        "resume 投影与可判定 pre-write 校验/重试信号，绝不授权 Execute",
        "metadata-before/exact-before/exact-after/metadata-after 稳定区间捕获并拒绝任一不匹配",
        "旧 content-only context 继续可读并回退精确 content 比较",
        "30 秒 / 1,000,000 遍历 entry / 累计 128 MiB 编码路径名的 cooperative work budget",
        "目录在有界 manifest 排序前流式计数",
        "最终 EOF 返回后均复查预算并 fail-closed",
        "ignore-aware path walk，只提供候选列表而不充当 read authority",
        "pinned root/parent fd 与 `openat`/`fstatat`/`readlinkat`",
        "pinned handle、`FILE_FLAG_OPEN_REPARSE_POINT`、final-path/file-identity 与 `FSCTL_GET_REPARSE_POINT`",
        "其他平台 fail closed",
        "线性的 `ValidatedIntentRevisionReceiptIndex`",
        "process-lifetime Phase 1 writer lease 必须在 reload/fold 前取得",
        "不同于 append/controller lock",
        "content scan 后再次校验 checkout identity",
        "metadata-only 信号可通过 Execute 的精确复核",
        "精确 identity/content 漂移则以 `PHASE1_WORKSPACE_CHANGED` 保留 gate",
    ] {
        assert_contains(&tracing_doc, needle, "Phase 1 tracing contract");
    }

    assert_contains(
        &plan,
        "- [x] Plan revision 保留当前",
        "W2-03 acceptance evidence",
    );
    assert_contains(
        &plan,
        "- [x] Confirmed plan execution 进入 runtime serialized queue",
        "W2-04 confirmed-plan execution is closed on the default Web path",
    );
    for needle in [
        "2026-08-21 Web repair acceptance（候选；在完整验证与独立审查逐字 `PASS` 前全部保持 pending）",
        "- [x] 每份 committed replay 只构造一个线性的 `ValidatedIntentRevisionReceiptIndex`",
        "5,000 events / 2,000 retries 的 startup/Claiming/Consuming lookup",
        "receipt 后出现的 invalid non-mutating Web intent 均 fail closed",
        "pending cancel receipt 若先于 later Web command",
        "- [x] fresh explicit-direct（包括 padded `/intent cancel`）",
        "- [x] scanner 保留 ignore-aware、path-based name enumeration，但它不是 read authority",
        "- [x] 独立 process-lifetime Phase 1 writer lease 在 reload/fold 前取得",
        "完整 Windows repo check",
        "不得把 isolated scanner harness 的通过写成完整 Windows repo check 通过",
        "33 passed / 0 failed / 0 ignored / 4405 filtered",
        "不证明完整 Windows repository build",
    ] {
        assert_contains(&plan, needle, "W2-03 repair acceptance and evidence");
    }
    assert_contains(
        &changelog,
        "### Fixed (plan-20260715 W2-03 repair, 2026-08-21, v0.20.4)",
        "W2-03 patch release note",
    );
    for needle in [
        "The additive public `intent_revision` binding contains only",
        "never copies the raw note into a workflow terminal",
        "Ordinary transcript and session-snapshot retention remain unchanged",
        "Phase 1 workspace drift is recoverable without weakening Execute",
        "determinate pre-write validation/retry signals",
        "metadata-before/exact-before/exact-after/metadata-after scans",
        "1,000,000-traversed-entry",
        "128-MiB encoded-path-name budgets",
        "final EOF",
        "Legacy contexts without the additive token remain readable and fall back to exact content comparison",
        "exactly one successful `submit_intent_draft` call",
        "padded spellings receive no privileged handling",
        "one shared linear `ValidatedIntentRevisionReceiptIndex`",
        "5,000-event regression with 700 receipts",
        "Path-name enumeration remains an ignore-aware walk and is not trusted as read authority",
        "`openat`/`fstatat`/`readlinkat`",
        "`FILE_FLAG_OPEN_REPARSE_POINT`",
        "`FSCTL_GET_REPARSE_POINT`",
        "Other platforms fail closed",
        "metadata-only warning may pass Execute's exact recheck",
        "one process-lifetime session writer",
        "acquired before session reload or projection fold",
        "distinct from the short-lived workflow append lock and from browser controller leases",
        "Windows rejects reparse points and verifies volume/file identity",
        "persistent regular file protected by an OS advisory lock",
        "live owner is never reclaimed by age",
        "Unix opens use `O_NOFOLLOW`",
        "sequence gaps or a cut replay window fail closed",
        "`409 PHASE1_WORKSPACE_CHANGED`",
        "`400 PLAN_REVISION_NOTE_REQUIRED`",
    ] {
        assert_contains(&changelog_words, needle, "W2-03 privacy release note");
    }
    for (source, test_name) in [
        (
            &wire_tests,
            "intent_revision_recovery_is_additive_and_pins_sse_snake_case",
        ),
        (
            &wire_tests,
            "sse_wire_v2_intent_revision_consumed_uses_dedicated_payload",
        ),
        (
            &jsonl_tests,
            "intent_revision_receipt_orders_source_consumer_and_resyncs_exact_postwrite_retry",
        ),
        (
            &scenario_tests,
            "plan_review_modify_next_plain_text_opens_replacement_plan_gate",
        ),
        (
            &scenario_tests,
            "plan_review_empty_revision_note_is_typed_and_preserves_authority",
        ),
        (
            &scenario_tests,
            "plan_review_repository_replacement_after_modify_keeps_revision_note_retryable",
        ),
        (
            &scenario_tests,
            "plan_review_workspace_drift_survives_resume_and_modify_rearms_current_checkout",
        ),
        (
            &scenario_tests,
            "plan_review_metadata_only_drift_warns_but_exact_execute_recheck_succeeds",
        ),
        (
            &scenario_tests,
            "plan_review_head_drift_survives_resume_and_requires_explicit_modify",
        ),
        (
            &scenario_tests,
            "plan_review_repository_replacement_blocks_modify_and_preserves_gate",
        ),
        (
            &scenario_tests,
            "plan_review_network_allow_returns_conflict_and_preserves_pending_gate",
        ),
        (
            &scenario_tests,
            "plan_review_network_allow_enters_runtime_queue",
        ),
        (
            &wire_tests,
            "plan_execution_not_available_is_a_catalogued_conflict",
        ),
        (
            &wire_tests,
            "phase1_workspace_and_revision_errors_are_catalogued",
        ),
        (
            &phase1,
            "phase1_workspace_binding_uses_content_authority_and_legacy_fallback",
        ),
        (
            &phase1,
            "phase1_capture_rejects_change_between_content_and_metadata_scans",
        ),
        (
            &workspace_snapshot,
            "metadata_fingerprint_changes_without_reading_file_bodies",
        ),
        (
            &workspace_snapshot,
            "content_fingerprint_detects_same_length_change_with_restored_mtime",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_counts_directories_and_propagates_entry_limit",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_propagates_path_name_limit",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_timeout_is_typed_and_actionable",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_checks_manifest_post_blocking_and_return_boundaries",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_checks_exact_blocking_operations_after_return",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_checks_metadata_and_symlink_operations_after_return",
        ),
        (
            &workspace_snapshot,
            "fingerprint_budget_streams_wide_directory_before_rejecting",
        ),
        (
            &workspace_snapshot,
            "fingerprint_entry_cap_stops_before_poisoned_iterator_tail",
        ),
        (
            &workspace_snapshot,
            "bounded_manifest_order_matches_legacy_sorted_walk",
        ),
        (
            &workspace_snapshot,
            "stable_fingerprint_pair_rejects_change_after_content_scan",
        ),
        (
            &compat_tests,
            "w203_revision_receipt_and_network_boundary_stay_aligned",
        ),
        (
            &jsonl,
            "code_workflow_append_lock_uses_os_liveness_without_aba",
        ),
        (
            &jsonl,
            "code_workflow_append_lock_rejects_symlink_without_touching_event_log",
        ),
        (
            &phase1,
            "phase1_recovery_rejects_sequence_gap_without_revision_sidecar_before_gc",
        ),
        (
            &headless,
            "claiming_without_command_rearms_active_before_mutation_recovery",
        ),
        (
            &headless,
            "claiming_pending_command_promotes_before_generic_recovery",
        ),
        (
            &headless,
            "claiming_canonical_cancel_and_double_attempt_remain_retryable",
        ),
        (
            &headless_tests,
            "active_revision_blocks_new_direct_but_preserves_exact_terminal_retry",
        ),
        (
            &jsonl,
            "committed_revision_receipt_batch_index_visits_five_thousand_events_linearly",
        ),
        (
            &jsonl,
            "claiming_and_consuming_revision_retry_index_visits_five_thousand_events_linearly",
        ),
        (
            &jsonl,
            "invalid_nonmutating_web_intent_after_receipt_fails_closed",
        ),
        (
            &jsonl,
            "pending_revision_receipt_followed_by_web_intent_pins_display",
        ),
        (
            &jsonl,
            "resolved_replacement_receipt_permanently_closes_its_retry_lineage",
        ),
        (
            &jsonl,
            "intent_revision_terminal_retry_rejects_marker_appended_after_terminal",
        ),
        (
            &jsonl,
            "replacement_marker_after_consumer_terminal_never_authorizes_a_receipt",
        ),
        (
            &jsonl,
            "effectless_intent_revision_receipt_fails_closed_in_batch_validation",
        ),
        (
            &jsonl,
            "duplicate_source_intent_after_its_terminal_fails_closed_in_batch_validation",
        ),
        (
            &jsonl,
            "conflicting_or_duplicate_intent_review_marker_ownership_fails_closed",
        ),
        (
            &phase1,
            "phase1_exact_validation_rejects_identity_change_after_content_scan",
        ),
        (
            &workspace_snapshot,
            "content_fingerprint_rejects_file_swapped_to_external_symlink_before_read",
        ),
        (
            &workspace_snapshot,
            "content_fingerprint_rejects_file_swapped_to_fifo_before_read",
        ),
        (
            &workspace_snapshot,
            "metadata_fingerprint_reuses_pinned_root_after_workspace_path_replacement",
        ),
        (
            &workspace_snapshot,
            "windows_reparse_parser_rejects_truncated_odd_and_out_of_range_records",
        ),
        (
            &headless_tests,
            "slash_intent_cancel_durably_exits_revision_and_unblocks_direct_turns",
        ),
        (
            &headless_tests,
            "oversized_intent_modify_note_is_typed_400_without_consuming_gate",
        ),
        (
            &headless_tests,
            "slash_intent_modify_provider_prompt_uses_only_the_change_suffix",
        ),
        (
            &headless_tests,
            "slash_intent_modify_multiple_successful_drafts_preserve_the_revision",
        ),
        (
            &headless_tests,
            "slash_intent_cancel_succeeded_with_stale_non_streaming_ack_recovers",
        ),
        (
            &headless_tests,
            "pending_cancel_receipt_before_later_web_never_rewrites_later_projection",
        ),
        (
            &headless_tests,
            "restart_reconciles_stale_intent_projection_after_atomic_control_cancel",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_rejects_second_attach_and_reacquires_after_drop",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_cannot_be_rebound_to_another_session",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_clone_cannot_attach_a_second_persistence",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_rejects_symlink_without_touching_target",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_rejects_replaced_lock_inode_before_attach",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_rejects_fifo_without_blocking",
        ),
        (
            &headless_tests,
            "headless_session_writer_lease_is_released_immediately_after_sigkill",
        ),
    ] {
        assert_contains(source, &format!("fn {test_name}"), "W2-03 test source");
        assert_contains(&test_index, test_name, "tests/INDEX.md W2-03 evidence");
        assert_contains(&plan, test_name, "plan-20260715.md W2-03 evidence");
    }
}

#[test]
fn web_build_job_enforces_ignored_static_export_lifecycle() {
    let workflow = read_repo_file(".github/workflows/base.yml");
    assert_contains(
        &workflow,
        "test -f web/out/index.html",
        ".github/workflows/base.yml",
    );
    assert_contains(
        &workflow,
        "git ls-files --error-unmatch -- web/out",
        ".github/workflows/base.yml",
    );
    assert_contains(
        &workflow,
        "git check-ignore -q web/out/index.html",
        ".github/workflows/base.yml",
    );
    assert!(
        !repo_root().join("scripts").exists(),
        "scripts directory should be removed"
    );
}

#[test]
fn lfs_compatibility_docs_use_current_attributes_filename() {
    for path in [
        "COMPATIBILITY.md",
        "docs/development/commands/_compatibility.md",
        "docs/development/commands/_compatibility.md",
    ] {
        let body = read_repo_file(path);
        assert!(
            body.contains(".libra_attributes"),
            "{path} must mention the current Libra attributes filename"
        );
        assert!(
            !body.contains(".libraattributes"),
            "{path} must not mention the retired .libraattributes spelling"
        );
    }
}

#[test]
fn compatibility_governance_roadmap_marks_current_surfaces_without_batch_status() {
    let governance = read_repo_file("docs/development/commands/_compatibility.md");

    for row in [
        "| merge | partial | partial | fast-forward, single-head three-way, `-s ours`, `-X ours/theirs`, unrelated-history opt-in, and CLI/config merge shortlogs supported; octopus and other strategies/options deferred |",
        "| pull | partial | partial | fetch + fast-forward/three-way merge supported; `pull.rebase`/`branch.<name>.rebase`/`pull.ff` defaults are config-aware with local/global decryption, system-scope skip, and explicit unsupported diagnostics for interactive/rebase-merges modes; advanced strategy flags still partial |",
        "| push | partial | partial | branch/tag update, multi-refspec, delete, `--tags`, and `--mirror` supported; local file remote rejected intentionally |",
        "| checkout | partial | partial | visible branch compatibility surface including worktree-scoped `checkout -` previous-target toggling shared with `switch -`, `-b`/`-B <branch> [<start-point>]` symbolic-HEAD branch creation, `--orphan <branch>` unborn root branch creation (start-point currently rejected), plus explicit `checkout -- <path>` restoration alias; prefer `switch` / `restore` |",
    ] {
        assert!(
            governance.contains(row),
            "compatibility governance roadmap must retain completed row: {row}"
        );
    }

    for removed in ["批次状态", "C7", "C8", "C9", "C7-C9 后续补录"] {
        assert!(
            !governance.contains(removed),
            "governance roadmap must not retain batch status marker: {removed}"
        );
    }
}
