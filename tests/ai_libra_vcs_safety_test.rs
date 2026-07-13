//! CEX-02 run_libra_vcs parameter-level safety tests.
//!
//! These tests keep the MCP bridge and direct MCP implementation aligned with
//! the provider-neutral safety contract introduced by CEX-01.

use std::{path::PathBuf, sync::Arc};

use libra::internal::ai::{
    libra_vcs::classify_run_libra_vcs_safety,
    mcp::{resource::RunLibraVcsParams, server::LibraMcpServer},
    runtime::hardening::{
        BlastRadius, PrincipalContext, PrincipalRole, SafetyDisposition, ToolBoundaryPolicy,
        ToolOperation,
    },
    tools::{
        context::{ToolInvocation, ToolPayload},
        handlers::McpBridgeHandler,
    },
};
use serde_json::Value;

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn run_libra_vcs_invocation(arguments: &str) -> ToolInvocation {
    ToolInvocation::new(
        "call-1",
        "run_libra_vcs",
        ToolPayload::Function {
            arguments: arguments.to_string(),
        },
        PathBuf::from("/tmp"),
    )
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ai_libra_vcs_safety_allows_read_only_parameter_combinations() {
    for (command, args) in [
        ("status", vec!["--json"]),
        ("status", vec!["--porcelain", "v2", "--untracked-files=all"]),
        // A diff is read-only only with BOTH `--no-textconv` and `--no-ext-diff`
        // (textconv and the external diff driver both run configured shell
        // commands by default).
        (
            "diff",
            vec![
                "--no-textconv",
                "--no-ext-diff",
                "--stat",
                "--",
                "src/main.rs",
            ],
        ),
        (
            "diff",
            vec![
                "--no-textconv",
                "--no-ext-diff",
                "-S",
                "old_api",
                "--name-only",
            ],
        ),
        (
            "diff",
            vec![
                "--no-textconv",
                "--no-ext-diff",
                "-Ghandler_v[0-9]",
                "--name-only",
            ],
        ),
        ("log", vec!["--oneline", "--max-count=5"]),
        ("log", vec!["--patch-with-stat", "--max-count=1"]),
        ("show", vec!["HEAD", "--stat"]),
        ("show", vec!["HEAD", "--patch-with-stat"]),
        ("show-ref", vec!["--heads"]),
        ("branch", vec!["--show-current"]),
        ("ls-files", vec!["--others", "--exclude-standard"]),
        ("ls-files", vec!["--error-unmatch", "src"]),
        // Git's read-only mode short aliases mirror their long forms, including
        // clap's grouped form (`-dm` == `-d -m`).
        ("ls-files", vec!["-c", "-d", "-m", "-o"]),
        ("ls-files", vec!["-dm"]),
    ] {
        let decision = classify_run_libra_vcs_safety(command, &strings(&args));

        assert_eq!(
            decision.disposition,
            SafetyDisposition::Allow,
            "{command} {args:?} should be read-only"
        );
        assert_eq!(decision.rule_name, "libra_vcs.read_only_allowlist");
        assert_eq!(decision.blast_radius, BlastRadius::Repository);
    }
}

#[test]
fn ai_libra_vcs_safety_requires_human_for_recoverable_or_unknown_combinations() {
    for (command, args, rule_name) in [
        ("add", vec!["src/main.rs"], "libra_vcs.recoverable_mutation"),
        (
            "commit",
            vec!["-m", "checkpoint"],
            "libra_vcs.recoverable_mutation",
        ),
        ("switch", vec!["feature"], "libra_vcs.recoverable_mutation"),
        (
            "branch",
            vec!["feat/local-spike"],
            "libra_vcs.recoverable_mutation",
        ),
        // A diff WITHOUT both filter-disabling flags may run a configured textconv
        // or external-diff shell command.
        ("diff", vec!["--stat"], "libra_vcs.diff_default_filters"),
        // `--no-textconv` alone is not enough — `diff.external` still runs.
        (
            "diff",
            vec!["--no-textconv", "--stat"],
            "libra_vcs.diff_default_filters",
        ),
        // Pickaxe itself is read-only, but it observes textconv output and must
        // retain the same default-filter approval boundary as ordinary diff.
        (
            "diff",
            vec!["-Sneedle", "--stat"],
            "libra_vcs.diff_default_filters",
        ),
        // Filter-disabling flags AFTER `--` are pathspecs, not the flags.
        (
            "diff",
            vec!["--stat", "--", "--no-textconv", "--no-ext-diff"],
            "libra_vcs.diff_default_filters",
        ),
        ("ls-files", vec!["-z"], "libra_vcs.unknown_args"),
        // A grouped short containing a non-allowlisted letter (`z`) stays unknown.
        ("ls-files", vec!["-dz"], "libra_vcs.unknown_args"),
        ("status", vec!["--unknown"], "libra_vcs.unknown_args"),
        ("stash", vec!["pop"], "libra_vcs.unknown_command"),
    ] {
        let decision = classify_run_libra_vcs_safety(command, &strings(&args));

        assert_eq!(
            decision.disposition,
            SafetyDisposition::NeedsHuman,
            "{command} {args:?} should require approval"
        );
        assert_eq!(decision.rule_name, rule_name);
        assert_eq!(decision.blast_radius, BlastRadius::Repository);
    }
}

#[test]
fn ai_libra_vcs_safety_denies_irreversible_or_high_blast_radius_combinations() {
    for (command, args, blast_radius) in [
        ("branch", vec!["-D", "main"], BlastRadius::Repository),
        // Inline value forms of the delete flag must reach the same Deny
        // decision — `--delete=main` previously slipped through to NeedsHuman
        // because the protected-branch scanner skipped any `-`-prefixed arg.
        ("branch", vec!["--delete=main"], BlastRadius::Repository),
        (
            "branch",
            vec!["--delete=release/1.2"],
            BlastRadius::Repository,
        ),
        ("branch", vec!["-D=master"], BlastRadius::Repository),
        (
            "branch",
            vec!["--delete-force=main"],
            BlastRadius::Repository,
        ),
        (
            "branch",
            vec!["--delete-force", "main"],
            BlastRadius::Repository,
        ),
        ("reset", vec!["--hard", "HEAD"], BlastRadius::Repository),
        ("clean", vec!["-fdx"], BlastRadius::Repository),
        (
            "push",
            vec!["--force", "origin", "main"],
            BlastRadius::Network,
        ),
        ("diff", vec!["--output=patch.diff"], BlastRadius::Repository),
    ] {
        let decision = classify_run_libra_vcs_safety(command, &strings(&args));

        assert_eq!(
            decision.disposition,
            SafetyDisposition::Deny,
            "{command} {args:?} should be denied"
        );
        assert_eq!(decision.rule_name, "libra_vcs.irreversible_mutation");
        assert_eq!(decision.blast_radius, blast_radius);
    }
}

#[tokio::test]
async fn ai_libra_vcs_direct_mcp_returns_approval_required_without_spawning() {
    let server = LibraMcpServer::new(None, None);

    let result = server
        .run_libra_vcs_impl(RunLibraVcsParams {
            command: "add".to_string(),
            args: Some(strings(&["src/main.rs"])),
        })
        .await
        .expect("safety preflight should return a tool result");

    assert_eq!(result.is_error, Some(true));
    let body: Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(body["status"], "approval_required");
    assert_eq!(body["rule_name"], "libra_vcs.recoverable_mutation");
    assert_eq!(body["approval_required"], true);
}

#[tokio::test]
async fn ai_libra_vcs_direct_mcp_denies_destructive_combinations_without_spawning() {
    let server = LibraMcpServer::new(None, None);

    let result = server
        .run_libra_vcs_impl(RunLibraVcsParams {
            command: "reset".to_string(),
            args: Some(strings(&["--hard", "HEAD"])),
        })
        .await
        .expect("safety preflight should return a tool result");

    assert_eq!(result.is_error, Some(true));
    let body: Value = serde_json::from_str(&result_text(&result)).unwrap();
    assert_eq!(body["status"], "denied");
    assert_eq!(body["rule_name"], "libra_vcs.irreversible_mutation");
    assert_eq!(body["approval_required"], false);
}

#[tokio::test]
async fn ai_libra_vcs_bridge_reports_read_only_calls_as_non_mutating() {
    let server = Arc::new(LibraMcpServer::new(None, None));
    let handler = McpBridgeHandler::all_handlers(server)
        .into_iter()
        .find_map(|(name, handler)| (name == "run_libra_vcs").then_some(handler))
        .expect("run_libra_vcs handler should be registered");

    let invocation = run_libra_vcs_invocation(r#"{"command":"status","args":["--json"]}"#);

    assert!(!handler.is_mutating(&invocation).await);
}

#[tokio::test]
async fn ai_libra_vcs_bridge_reports_uncertain_calls_as_mutating() {
    let server = Arc::new(LibraMcpServer::new(None, None));
    let handler = McpBridgeHandler::all_handlers(server)
        .into_iter()
        .find_map(|(name, handler)| (name == "run_libra_vcs").then_some(handler))
        .expect("run_libra_vcs handler should be registered");

    let invocation = run_libra_vcs_invocation(r#"{"command":"status","args":["--unknown"]}"#);

    assert!(handler.is_mutating(&invocation).await);
}

#[test]
fn ai_libra_vcs_tool_boundary_allows_read_only_run_libra_vcs() {
    let policy = ToolBoundaryPolicy::default_runtime();
    let decision = policy.decide(
        &PrincipalContext {
            principal_id: "reviewer".to_string(),
            role: PrincipalRole::Observer,
        },
        &ToolOperation::tool("run_libra_vcs", false, false),
    );

    assert!(decision.allowed, "{}", decision.reason);
    assert!(!decision.approval_required);
}
