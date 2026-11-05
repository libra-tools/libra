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

use super::{init_repo_via_cli, run_libra_command, run_libra_command_with_env};

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
    assert!(add_err.contains("claude-code, codex, opencode, grok, hermes, pi"));

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
/// roster: exactly the 3 supported agents (the first-batch trio), and the
/// codex row exposing its landed hook-installable capability.
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
    assert_eq!(
        agents.len(),
        6,
        "listing surfaces the supported roster, including grok, hermes and pi"
    );

    let supported: Vec<&str> = agents
        .iter()
        .filter(|row| row["supported"] == true)
        .map(|row| row["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        supported,
        ["claude-code", "codex", "opencode", "grok", "hermes", "pi"]
    );

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
        if row["slug"] == "grok" {
            assert_eq!(row["support_wave"], "grok");
            assert_eq!(row["supported"], true);
            assert_eq!(row["hook_installable"], true);
            assert_eq!(row["launchable_review"], false);
            assert_eq!(row["launchable_investigate"], false);
        } else if row["slug"] == "hermes" {
            assert_eq!(row["support_wave"], "hermes");
            assert_eq!(row["supported"], true);
            assert_eq!(row["hook_installable"], true);
            assert_eq!(row["transcript_readable"], true);
            assert_eq!(row["launchable_review"], false);
            assert_eq!(row["launchable_investigate"], false);
        } else if row["slug"] == "pi" {
            assert_eq!(row["support_wave"], "pi");
            assert_eq!(row["supported"], true);
            assert_eq!(row["hook_installable"], true);
            assert_eq!(row["transcript_readable"], false);
            assert_eq!(row["launchable_review"], false);
            assert_eq!(row["launchable_investigate"], false);
        } else if row["supported"] == true {
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

    // The unsupported agents are omitted from the listing entirely — only the
    // supported roster is shown. Their unsupported registry classification is
    // pinned separately against the static registry by
    // tests/compat/agent_capability_matrix_pin.rs.
    for slug in ["gemini", "cursor", "copilot", "factory-ai"] {
        assert!(
            !agents.iter().any(|row| row["slug"] == slug),
            "{slug} must not appear in the supported-only listing"
        );
    }
}

/// The `list --json` roster surface pins the A0-11 deferred-parity decision:
/// the listing surfaces exactly the first-batch trio (all supported), and
/// every non-first-batch agent (`gemini`/`cursor`/`copilot`/`factory-ai`) is
/// omitted from the listing entirely. Their fully-closed unsupported
/// classification stays pinned against the static registry by
/// `tests/compat/agent_capability_matrix_pin.rs`.
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

    // The listing surfaces exactly the first-batch trio, in registration
    // order — nothing more, nothing less.
    let slugs: Vec<&str> = agents
        .iter()
        .map(|row| row["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        slugs,
        ["claude-code", "codex", "opencode", "grok", "hermes", "pi"],
        "listing must surface the first-batch trio plus grok, hermes and pi"
    );

    // Every listed row is supported (the unsupported agents are omitted).
    for row in &agents {
        assert_eq!(
            row["supported"], true,
            "{} listed but not supported",
            row["slug"]
        );
    }

    // Every deferred-parity agent is omitted from the listing entirely. Their
    // fully-closed unsupported classification (supported/hook_installable/
    // launchable_*/capabilities.hooks all false) stays pinned against the
    // static registry by tests/compat/agent_capability_matrix_pin.rs.
    for slug in ["gemini", "cursor", "copilot", "factory-ai"] {
        assert!(
            !agents.iter().any(|row| row["slug"] == slug),
            "{slug} must not appear in the supported-only listing"
        );
    }
    let grok = agents
        .iter()
        .find(|row| row["slug"] == "grok")
        .expect("grok row");
    assert_eq!(grok["support_wave"], "grok");
    assert_eq!(grok["launchable_review"], false);
    assert_eq!(grok["launchable_investigate"], false);
    let pi = agents
        .iter()
        .find(|row| row["slug"] == "pi")
        .expect("pi row");
    assert_eq!(pi["support_wave"], "pi");
    assert_eq!(pi["supported"], true);
    assert_eq!(pi["hook_installable"], true);
    assert_eq!(pi["launchable_review"], false);
    assert_eq!(pi["launchable_investigate"], false);
    assert_eq!(pi["capabilities"]["hooks"], true);
}
/// PI-06: the project-local Pi extension installs and removes through the
/// CLI, bare enable skips a missing Pi binary, and doctor reports the
/// PI-17 coverage findings.
#[test]
fn pi_supported_enable_disable_and_bare_skip() {
    use libra::internal::ai::hooks::providers::pi::settings::{
        PI_ASSET_FILE, PiAssetOwnership, pi_asset_ownership, pi_asset_path,
    };

    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    init_repo_via_cli(&repo);

    let bare = run_libra_command(&["agent", "enable"], &repo);
    assert!(
        bare.status.success(),
        "{}",
        String::from_utf8_lossy(&bare.stderr)
    );
    assert!(
        String::from_utf8_lossy(&bare.stderr).contains("skipping 'pi'"),
        "{}",
        String::from_utf8_lossy(&bare.stderr)
    );
    assert!(
        !pi_asset_path(&repo).exists(),
        "bare enable must not install a Pi asset without a binary"
    );

    let explicit = run_libra_command(&["agent", "enable", "--agent", "pi"], &repo);
    assert!(!explicit.status.success());
    assert!(
        String::from_utf8_lossy(&explicit.stderr).contains("needs a `pi` executable on PATH"),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(!pi_asset_path(&repo).exists());

    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).expect("fake bin");
    let fake = bin.join("pi");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").expect("fake pi");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
    let enabled = run_libra_command_with_env(
        &["agent", "enable", "--agent", "pi"],
        &repo,
        &[("PATH", path.as_str())],
    );
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(
        pi_asset_ownership(&repo).expect("ownership"),
        PiAssetOwnership::Owned
    );
    assert!(pi_asset_path(&repo).ends_with(PI_ASSET_FILE));

    let doctor = run_libra_command(&["agent", "doctor", "--json"], &repo);
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).expect("doctor json");
    let pi = report["data"]["provider_hooks"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "pi"))
        .expect("pi provider row");
    assert_eq!(pi["installed"], true);
    let events = pi["events"].as_array().expect("pi events");
    assert!(
        events.iter().any(|event| event["event"] == "trust"),
        "doctor must report the unconfirmable Pi trust: {events:?}"
    );
    assert!(
        events.iter().any(|event| event["event"] == "launch"),
        "doctor must report the subdirectory launch gap: {events:?}"
    );

    let disabled = run_libra_command(&["agent", "disable", "--agent", "pi"], &repo);
    assert!(
        disabled.status.success(),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert_eq!(
        pi_asset_ownership(&repo).expect("ownership"),
        PiAssetOwnership::Absent
    );
    let again = run_libra_command(&["agent", "disable", "--agent", "pi"], &repo);
    assert!(again.status.success(), "repeated disable is idempotent");
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
    assert!(cursor_err.contains("claude-code, codex, opencode, grok"));
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
#[serial(cwd)]
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
#[serial(cwd)]
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

/// HM-06: install and update rewrite only a marker-and-hash owned asset.
/// An unmanaged file and a drifted file stay byte-for-byte.
#[test]
fn hermes_owned_asset_is_not_overwritten_when_unmanaged_or_drifted() {
    use libra::internal::ai::hooks::providers::hermes::settings::{
        hermes_capture_is_installed, install_owned_hermes_capture, remove_owned_hermes_capture,
    };

    let temp = tempdir().expect("tempdir");
    let home = temp.path();
    install_owned_hermes_capture(home, "/usr/bin/libra-a").expect("install");
    assert!(hermes_capture_is_installed(home).expect("installed"));
    let plugin = home.join("plugins/libra-hermes-capture");
    let init = plugin.join("__init__.py");
    let yaml = plugin.join("plugin.yaml");
    let config = home.join("config.yaml");
    assert!(
        std::fs::read_to_string(&init)
            .unwrap()
            .contains("libra capture")
    );
    assert!(
        std::fs::read_to_string(&init)
            .unwrap()
            .contains("/usr/bin/libra-a")
    );
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .contains("libra-hermes-capture")
    );

    install_owned_hermes_capture(home, "/usr/bin/libra-b").expect("update owned");
    let updated = std::fs::read_to_string(&init).unwrap();
    assert!(updated.contains("/usr/bin/libra-b"));
    assert!(!updated.contains("/usr/bin/libra-a"));

    let unmanaged = b"name: someone-else\n";
    std::fs::write(&yaml, unmanaged).unwrap();
    let err = install_owned_hermes_capture(home, "/usr/bin/libra-c").unwrap_err();
    assert!(err.to_string().contains("left unchanged"), "{err}");
    assert_eq!(std::fs::read(&yaml).unwrap(), unmanaged);

    remove_owned_hermes_capture(home).expect("remove leaves unmanaged");
    assert_eq!(std::fs::read(&yaml).unwrap(), unmanaged);
    assert!(init.exists(), "drifted sibling stays");

    let owned = tempdir().expect("owned home");
    install_owned_hermes_capture(owned.path(), "/usr/bin/libra-a").expect("reinstall");
    let owned_init = owned
        .path()
        .join("plugins/libra-hermes-capture/__init__.py");
    let mut drifted = std::fs::read(&owned_init).unwrap();
    drifted.extend_from_slice(b"\n# libra capture edited\n");
    std::fs::write(&owned_init, &drifted).unwrap();
    let err = install_owned_hermes_capture(owned.path(), "/usr/bin/libra-c").unwrap_err();
    assert!(err.to_string().contains("drifted"), "{err}");
    assert_eq!(std::fs::read(&owned_init).unwrap(), drifted);
    remove_owned_hermes_capture(owned.path()).expect("remove drifted is a no-op");
    assert_eq!(std::fs::read(&owned_init).unwrap(), drifted);

    let clean = tempdir().expect("clean home");
    install_owned_hermes_capture(clean.path(), "/usr/bin/libra-a").expect("clean install");
    remove_owned_hermes_capture(clean.path()).expect("remove owned");
    assert!(!hermes_capture_is_installed(clean.path()).expect("gone"));
    assert!(!clean.path().join("config.yaml").exists());
}

/// HM-06: a bare enable skips a missing Hermes runtime, and an explicit
/// enable refuses it before any file is written.
#[test]
fn hermes_missing_runtime_skips_bare_enable_and_refuses_explicit() {
    use libra::internal::ai::hooks::providers::hermes::settings::{
        HermesEnableGate, hermes_enable_gate, hermes_version_text_is_supported,
    };

    assert_eq!(hermes_enable_gate(true, false), HermesEnableGate::SkipBare);
    assert_eq!(
        hermes_enable_gate(false, false),
        HermesEnableGate::RefuseExplicit
    );
    assert_eq!(hermes_enable_gate(false, true), HermesEnableGate::Install);
    assert!(hermes_version_text_is_supported(
        "Hermes Agent v0.22.5 (2026.9.24) stable local bf0203a794a0"
    ));
    assert!(!hermes_version_text_is_supported(
        "Hermes Agent v0.22.6 bf0203a"
    ));
}
