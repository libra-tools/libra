//! Integration coverage for the AG-17 CLI roster surface:
//! `libra agent list` (capability matrix), the `add`/`remove` aliases of
//! `enable`/`disable`, actionable-unsupported roster errors, and the gemini
//! uninstall-only channel.

use libra::{
    internal::ai::hooks::{provider::ProviderInstallOptions, providers::gemini_provider},
    utils::test::ChangeDirGuard,
};
use serial_test::serial;
use tempfile::tempdir;

use super::{init_repo_via_cli, run_libra_command};

/// `list` parses and succeeds; `add`/`remove` are strict aliases of
/// `enable --agent` / `disable --agent`: same exit code and same
/// user-visible diagnostics for the same inputs.
#[test]
fn agent_list_add_remove_aliases_parse() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    let list = run_libra_command(&["agent", "list"], &repo);
    assert!(list.status.success(), "agent list must succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("claude-code"), "matrix lists claude-code");

    // Alias parity on the error path: identical exit codes and identical
    // actionable message for an unknown slug.
    let add = run_libra_command(&["agent", "add", "bogus"], &repo);
    let enable = run_libra_command(&["agent", "enable", "--agent", "bogus"], &repo);
    assert_eq!(add.status.code(), enable.status.code());
    assert!(!add.status.success());
    let add_err = String::from_utf8_lossy(&add.stderr);
    let enable_err = String::from_utf8_lossy(&enable.stderr);
    assert_eq!(add_err, enable_err, "add/enable diagnostics must match");
    assert!(add_err.contains("unknown agent 'bogus'"));
    assert!(add_err.contains("claude-code, codex, opencode"));

    let remove = run_libra_command(&["agent", "remove", "bogus"], &repo);
    let disable = run_libra_command(&["agent", "disable", "--agent", "bogus"], &repo);
    assert_eq!(remove.status.code(), disable.status.code());
    assert!(!remove.status.success());
    assert_eq!(
        String::from_utf8_lossy(&remove.stderr),
        String::from_utf8_lossy(&disable.stderr),
        "remove/disable diagnostics must match"
    );

    // Alias parity on the success path: removing a supported agent whose
    // hooks were never installed is an idempotent no-op for both spellings.
    let remove_ok = run_libra_command(&["agent", "remove", "claude-code"], &repo);
    let disable_ok = run_libra_command(&["agent", "disable", "--agent", "claude-code"], &repo);
    assert!(remove_ok.status.success());
    assert_eq!(remove_ok.status.code(), disable_ok.status.code());
    assert_eq!(
        String::from_utf8_lossy(&remove_ok.stdout),
        String::from_utf8_lossy(&disable_ok.stdout),
        "remove/disable stdout must match"
    );
}

/// The `list --json` payload carries the frozen AG-17 row shape and the E9
/// roster: exactly 7 registered agents, supported = first-batch trio, and
/// the codex row pinned to transcript-readable-only.
#[test]
fn agent_list_json_contains_capability_fields() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    let output = run_libra_command(&["agent", "list", "--json"], &repo);
    assert!(output.status.success(), "agent list --json must succeed");
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list --json emits valid JSON");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "agent_list");
    let data = &envelope["data"];
    assert_eq!(data["schema_version"], 1);

    let agents = data["agents"].as_array().expect("agents array");
    assert_eq!(agents.len(), 7, "one row per AgentKind");

    let supported: Vec<&str> = agents
        .iter()
        .filter(|row| row["supported"] == true)
        .map(|row| row["slug"].as_str().unwrap())
        .collect();
    assert_eq!(supported, ["claude-code", "codex", "opencode"]);

    for row in agents {
        // Frozen wire keys (AG-17): missing keys are a schema break.
        for key in [
            "slug",
            "agent_kind",
            "stability",
            "supported",
            "registered",
            "transcript_readable",
            "hook_installable",
            "installed",
            "launchable_review",
            "launchable_investigate",
            "external_binary",
            "config_paths",
            "capabilities",
        ] {
            assert!(
                row.get(key).is_some(),
                "row {} missing frozen key {key}",
                row["slug"]
            );
        }
        // `support_wave` is part of the frozen shape for every row —
        // `first_batch` when supported, JSON null otherwise.
        assert!(
            row.as_object().unwrap().contains_key("support_wave"),
            "row {} missing frozen key support_wave",
            row["slug"]
        );
        if row["supported"] == true {
            assert_eq!(row["support_wave"], "first_batch", "{}", row["slug"]);
        } else {
            assert!(row["support_wave"].is_null(), "{}", row["slug"]);
        }
    }

    let codex = agents
        .iter()
        .find(|row| row["slug"] == "codex")
        .expect("codex row");
    assert_eq!(codex["agent_kind"], "codex");
    assert_eq!(codex["registered"], true);
    assert_eq!(codex["stability"], "stable");
    assert_eq!(codex["transcript_readable"], true);
    // AG-19: the Codex HookProvider landed — the row advertises hook
    // installation (config target: user-level $CODEX_HOME/hooks.json).
    assert_eq!(codex["hook_installable"], true);
    assert_eq!(codex["installed"], false);
    assert_eq!(codex["capabilities"]["hooks"], true);

    let gemini = agents
        .iter()
        .find(|row| row["slug"] == "gemini")
        .expect("gemini row");
    assert_eq!(gemini["supported"], false);
    assert_eq!(gemini["hook_installable"], false);
    assert_eq!(gemini["installed"], false);
}

/// The `list --json` roster surface pins the A0-11 deferred-parity decision:
/// exactly the first-batch trio is supported/launchable, and every
/// non-first-batch agent (`gemini`/`cursor`/`copilot`/`factory-ai`) stays a
/// fully-closed row — `supported`/`hook_installable`/`launchable_review`/
/// `launchable_investigate`/`capabilities.hooks` all `false`. Keeps the docs'
/// "deferred parity" roster claim honest against the registry.
#[test]
fn agent_roster_surface() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    let output = run_libra_command(&["agent", "list", "--json"], &repo);
    assert!(output.status.success(), "agent list --json must succeed");
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list --json emits valid JSON");
    let agents = envelope["data"]["agents"]
        .as_array()
        .expect("agents array")
        .clone();

    // Supported roster is exactly the first batch — nothing more, nothing less.
    let supported: Vec<&str> = agents
        .iter()
        .filter(|row| row["supported"] == true)
        .map(|row| row["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        supported,
        ["claude-code", "codex", "opencode"],
        "supported roster must be the first-batch trio"
    );

    // Every deferred-parity agent is a fully-closed row.
    for slug in ["gemini", "cursor", "copilot", "factory-ai"] {
        let row = agents
            .iter()
            .find(|row| row["slug"] == slug)
            .unwrap_or_else(|| panic!("registry must list {slug}"));
        assert_eq!(row["supported"], false, "{slug} must stay unsupported");
        assert_eq!(
            row["hook_installable"], false,
            "{slug} must not be hook-installable"
        );
        assert_eq!(
            row["launchable_review"], false,
            "{slug} must not be launchable for review"
        );
        assert_eq!(
            row["launchable_investigate"], false,
            "{slug} must not be launchable for investigate"
        );
        assert_eq!(
            row["capabilities"]["hooks"], false,
            "{slug} must not advertise the hooks capability"
        );
        assert!(
            row["support_wave"].is_null(),
            "{slug} carries no support wave"
        );
    }
}

/// Enabling anything outside the supported roster is an actionable error
/// (nothing installed, non-zero exit); a supported agent without a landed
/// HookProvider is an informational skip, not an error.
#[test]
fn agent_add_non_hook_installable_returns_actionable_unsupported() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    // Never-supported agent: actionable unsupported, non-zero exit.
    let cursor = run_libra_command(&["agent", "add", "cursor"], &repo);
    assert!(!cursor.status.success());
    let cursor_err = String::from_utf8_lossy(&cursor.stderr);
    assert!(cursor_err.contains("not in the supported roster"));
    assert!(cursor_err.contains("claude-code, codex, opencode"));
    assert!(
        !repo.join(".cursor").exists(),
        "unsupported add must not write hooks"
    );

    // Demoted agent: unsupported for install, pointed at the uninstall channel.
    let gemini = run_libra_command(&["agent", "add", "gemini"], &repo);
    assert!(!gemini.status.success());
    let gemini_err = String::from_utf8_lossy(&gemini.stderr);
    assert!(gemini_err.contains("libra agent remove gemini"));
    assert!(
        !repo.join(".gemini").exists(),
        "gemini add must not write hooks"
    );

    // AG-19: codex/opencode are installable now; their install/uninstall
    // behavior (canonical binary path, trust entries, marker-gated
    // removal) is pinned end-to-end in tests/agent_enable_install_path_test.rs.
    // This test keeps only the never-supported / demoted refusals.

    // A batch containing an unsupported slug fails closed before any install.
    let batch = run_libra_command(&["agent", "add", "claude-code", "cursor"], &repo);
    assert!(!batch.status.success());
    assert!(
        !repo.join(".claude").exists(),
        "batch with unsupported member must not half-install"
    );

    // The uninstall-only channel is gemini-specific: removing any other
    // non-roster agent is an actionable unsupported error, not a silent
    // success.
    let cursor_remove = run_libra_command(&["agent", "remove", "cursor"], &repo);
    assert!(!cursor_remove.status.success());
    let cursor_remove_err = String::from_utf8_lossy(&cursor_remove.stderr);
    assert!(cursor_remove_err.contains("uninstall-only channel"));
}

/// A broken provider settings file is a surfaced error — `list` must not
/// silently report `installed=false` when the install state cannot be read.
#[test]
fn agent_list_surfaces_hook_state_inspection_errors() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    let claude_dir = repo.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("mkdir .claude");
    std::fs::write(claude_dir.join("settings.json"), b"{not json").expect("write junk");

    let output = run_libra_command(&["agent", "list"], &repo);
    assert!(
        !output.status.success(),
        "list must fail loudly on unreadable hook state"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("claude-code") && stderr.contains("hook installation state"),
        "error names the agent and the failing inspection: {stderr}"
    );
}

/// The gemini uninstall-only channel: legacy Libra-managed hooks (installed
/// by an older binary) can still be removed, the removal is idempotent, and
/// `add gemini` stays rejected afterwards.
#[test]
#[serial]
fn agent_remove_gemini_uninstalls_legacy_hooks_idempotent() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    // Simulate the legacy install with the real provider (in-process; the
    // provider resolves the repo root from the current directory). Install
    // with the same binary the CLI tests spawn so the spawned binary's
    // `hooks_are_installed` check recognises the entries as its own.
    let provider = gemini_provider();
    {
        let _guard = ChangeDirGuard::new(&repo);
        provider
            .install_hooks(&ProviderInstallOptions {
                binary_path: Some(env!("CARGO_BIN_EXE_libra").to_string()),
                timeout_secs: None,
            })
            .expect("legacy gemini install succeeds");
    }
    let settings_path = repo.join(".gemini/settings.json");
    let installed = std::fs::read_to_string(&settings_path).expect("settings written");
    assert!(
        installed.contains("libra-session-start"),
        "legacy install wrote Libra-managed entries: {installed}"
    );

    // First remove uninstalls the Libra-managed entries.
    let first = run_libra_command(&["agent", "remove", "gemini"], &repo);
    assert!(first.status.success(), "gemini remove must succeed");
    let after_remove = std::fs::read_to_string(&settings_path).expect("settings survive");
    assert!(
        !after_remove.contains("libra-session-start"),
        "Libra-managed gemini hooks must be gone after remove: {after_remove}"
    );

    // Second remove is idempotent: exit 0 with a clear notice, no error stack.
    let second = run_libra_command(&["agent", "remove", "gemini"], &repo);
    assert!(second.status.success(), "repeat remove must stay exit 0");
    let second_out = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_out.contains("not installed") || second_out.contains("nothing to do"),
        "repeat remove prints an explicit no-op notice: {second_out}"
    );

    // The demotion holds: gemini cannot be re-added through the CLI.
    let re_add = run_libra_command(&["agent", "add", "gemini"], &repo);
    assert!(!re_add.status.success());
}

/// Removing gemini strips only the Libra-managed entries — user-authored
/// hook entries and unrelated settings keys survive verbatim.
#[test]
#[serial]
fn agent_remove_preserves_user_hook_entries() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    // A user-authored settings file that predates the Libra install.
    let settings_dir = repo.join(".gemini");
    std::fs::create_dir_all(&settings_dir).expect("mkdir .gemini");
    let user_settings = serde_json::json!({
        "theme": "dark",
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "name": "my-custom-hook",
                    "type": "command",
                    "command": "echo hello"
                }]
            }]
        }
    });
    std::fs::write(
        settings_dir.join("settings.json"),
        serde_json::to_vec_pretty(&user_settings).unwrap(),
    )
    .expect("write user settings");

    let provider = gemini_provider();
    {
        let _guard = ChangeDirGuard::new(&repo);
        provider
            .install_hooks(&ProviderInstallOptions {
                binary_path: Some(env!("CARGO_BIN_EXE_libra").to_string()),
                timeout_secs: None,
            })
            .expect("legacy gemini install succeeds");
    }

    let removed = run_libra_command(&["agent", "remove", "gemini"], &repo);
    assert!(removed.status.success());

    let after: serde_json::Value = serde_json::from_slice(
        &std::fs::read(settings_dir.join("settings.json")).expect("settings survive"),
    )
    .expect("settings stay valid JSON");
    assert_eq!(after["theme"], "dark", "unrelated user keys survive");
    let rendered = after.to_string();
    assert!(
        rendered.contains("my-custom-hook"),
        "user hook entry survives: {rendered}"
    );
    assert!(
        !rendered.contains("libra-session-start"),
        "Libra-managed entries are removed: {rendered}"
    );
}
