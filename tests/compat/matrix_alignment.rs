mod matrix_alignment_support;

use std::fs;

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
    let source_and_docs = [
        web_mod.as_str(),
        read_repo_file("src/internal/ai/web/code_ui.rs").as_str(),
        read_repo_file("src/internal/tui/control.rs").as_str(),
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

    for header in ["X-Libra-Control-Token", "X-Code-Controller-Token"] {
        assert_contains(&code_doc, header, "docs/commands/code.md");
        assert_contains(&source_and_docs, header, "source/docs control contract");
    }

    for code in [
        "CONTROL_DISABLED",
        "LOOPBACK_REQUIRED",
        "MISSING_CONTROL_TOKEN",
        "INVALID_CONTROL_TOKEN",
        "MISSING_CONTROLLER_TOKEN",
        "INVALID_CONTROLLER_TOKEN",
        "CONTROLLER_CONFLICT",
        "SESSION_BUSY",
        "INTERACTION_NOT_ACTIVE",
    ] {
        assert_contains(&source_and_docs, code, "source/docs control error contract");
    }

    for flag in ["--control", "--control-token-file", "--control-info-file"] {
        assert_contains(&code_doc, flag, "docs/commands/code.md");
    }

    for (body, needle, context) in [
        (
            code_control_doc.as_str(),
            "code-control --stdio",
            "docs/commands/code-control.md",
        ),
        (
            code_control_doc.as_str(),
            "diagnostics.get",
            "docs/commands/code-control.md",
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
        "Run TUI automation scenarios",
        ".github/workflows/base.yml",
    );
    assert!(
        !workflow.contains("RUST_LOG:"),
        "Run TUI automation scenarios must not set global RUST_LOG in CI"
    );

    for path in [
        "tests/harness/scenario.rs",
        "tests/diagnostics_redaction_test.rs",
        "tests/code_codex_default_tui_test.rs",
    ] {
        assert!(
            repo_root().join(path).exists(),
            "required path is missing: {path}"
        );
    }
}

#[test]
fn web_build_job_checks_static_export_drift_inline() {
    let workflow = read_repo_file(".github/workflows/base.yml");
    assert_contains(
        &workflow,
        "git status --porcelain -- web/out",
        ".github/workflows/base.yml",
    );
    assert_contains(
        &workflow,
        "web/out has untracked, staged, or unstaged files after the static export build.",
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
        "| merge | partial | partial | fast-forward and single-head three-way merge supported; `-m`/`--ff-only`/`--no-ff`/`--squash`/`--no-commit`/`--no-edit`/`--verify-signatures` (vault-key PGP only) supported; octopus/custom strategies deferred |",
        "| pull | partial | partial | fetch + fast-forward/three-way merge supported; advanced strategy flags still partial |",
        "| push | partial | partial | branch/tag update, multi-refspec, delete, `--tags`, and `--mirror` supported; local file remote rejected intentionally |",
        "| checkout | partial | partial | visible branch compatibility surface including `-b`/`-B <branch> [<start-point>]` symbolic-HEAD branch creation, `--orphan <branch>` unborn root branch creation (start-point currently rejected), plus explicit `checkout -- <path>` restoration alias; prefer `switch` / `restore` |",
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
