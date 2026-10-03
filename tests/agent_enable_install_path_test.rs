//! §765 install-path assertion for `libra agent enable` (AG-19).
//!
//! Drives the built binary end-to-end (harness mirrors
//! `tests/agent_lifecycle_event_test.rs`) and pins the installer contract:
//! every hook command the enable path writes embeds the **canonical
//! absolute path** of the running Libra binary — never a bare `libra`
//! PATH lookup — for both the OpenCode plugin file
//! (`src/internal/ai/hooks/providers/opencode/settings.rs`) and the Codex
//! `$CODEX_HOME/hooks.json` + `config.toml` trust entries
//! (`src/internal/ai/hooks/providers/codex/settings.rs`), plus the Codex
//! trust-gap stderr banner and marker-respecting disable semantics.

#![cfg(unix)]

use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

/// The eleven Codex events the installer forwards, with their CLI verbs
/// (pins `CODEX_HOOK_FORWARD_MAP` in codex/settings.rs).
const CODEX_FORWARDED_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "tool-use"),
    ("PostToolUse", "tool-use"),
    ("PermissionRequest", "permission-request"),
    ("PreCompact", "compaction"),
    ("PostCompact", "compaction"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-end"),
];

const CODEX_DEFAULT_TIMEOUT_SECS: u64 = 30;
const CODEX_DEFAULT_CAPTURE_BUDGET_MILLIS: u64 = 29_000;
const CODEX_SESSION_END_TIMEOUT_SECS: u64 = 3;
const CODEX_SESSION_END_CAPTURE_BUDGET_MILLIS: u64 = 2_000;

fn codex_installed_timing(event: &str) -> (u64, u64) {
    if event == "SessionEnd" {
        (
            CODEX_SESSION_END_TIMEOUT_SECS,
            CODEX_SESSION_END_CAPTURE_BUDGET_MILLIS,
        )
    } else {
        (
            CODEX_DEFAULT_TIMEOUT_SECS,
            CODEX_DEFAULT_CAPTURE_BUDGET_MILLIS,
        )
    }
}

/// The six Claude events the installer forwards. Kept local to the
/// end-to-end uninstall regression so a legacy command is seeded for every
/// provider-owned handler, not just one representative event.
const CLAUDE_FORWARDED_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "tool-use"),
    ("PostToolUse", "tool-use"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

/// First line of every Libra-managed OpenCode plugin file (pins
/// `LIBRA_MANAGED_MARKER` in opencode/settings.rs).
const OPENCODE_MANAGED_MARKER: &str =
    "// libra-managed: do not edit — installed by libra agent enable (AG-19)";

/// One isolated libra repository plus a fake `$HOME`.
struct HookRepo {
    _tempdir: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl HookRepo {
    fn init() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let home = tempdir.path().join("home");
        let repo = tempdir.path().join("repo");
        std::fs::create_dir_all(&home).expect("create fake home");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        let this = Self {
            _tempdir: tempdir,
            repo,
            home,
        };
        let out = this.run(&["init"], None, &[]);
        assert!(
            out.status.success(),
            "libra init failed: {}",
            describe(&out)
        );
        this
    }

    fn run(&self, args: &[&str], stdin: Option<&str>, envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("LIBRA_TEST_HOME", &self.home)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().expect("spawn libra binary");
        if let Some(payload) = stdin {
            child
                .stdin
                .take()
                .expect("stdin piped")
                .write_all(payload.as_bytes())
                .expect("write hook envelope to stdin");
        }
        child.wait_with_output().expect("wait for libra binary")
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

/// Canonical absolute path of the binary under test — the exact string the
/// installers must embed (`resolve_hook_binary_path` canonicalizes
/// `current_exe`, and the CLI passes `current_exe` through).
fn canonical_binary_path() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_libra"))
        .expect("canonicalize the built libra binary")
        .display()
        .to_string()
}

/// `agent enable --agent opencode` writes the Libra-managed plugin at
/// `<repo>/.opencode/plugin/libra-hooks.js` with the marker first line and
/// the canonical absolute binary path baked into `LIBRA_COMMAND`; the hook
/// invocation interpolates that constant (never a bare `libra`). Disable
/// removes the managed file, and a user-owned file at the same path
/// survives a second disable.
#[test]
fn opencode_enable_writes_canonical_binary_path() {
    let repo = HookRepo::init();
    let canonical = canonical_binary_path();
    let plugin_path = repo
        .repo
        .join(".opencode")
        .join("plugin")
        .join("libra-hooks.js");

    let out = repo.run(
        &["agent", "enable", "--agent", "opencode", "--json"],
        None,
        &[],
    );
    assert!(out.status.success(), "enable opencode: {}", describe(&out));

    let content = std::fs::read_to_string(&plugin_path)
        .unwrap_or_else(|err| panic!("plugin missing at {}: {err}", plugin_path.display()));
    assert!(
        content.starts_with(OPENCODE_MANAGED_MARKER),
        "plugin must start with the Libra-managed marker line:\n{content}"
    );

    // The command constant must carry the canonical absolute binary path…
    let command_line = content
        .lines()
        .find(|line| line.starts_with("const LIBRA_COMMAND = "))
        .unwrap_or_else(|| panic!("no LIBRA_COMMAND line in plugin:\n{content}"));
    assert!(
        command_line.contains(&canonical),
        "LIBRA_COMMAND must embed the canonical binary path '{canonical}': {command_line}"
    );
    // …and never a bare PATH-dependent `libra` token.
    for bare in [
        r#"const LIBRA_COMMAND = "libra";"#,
        r#"const LIBRA_COMMAND = "'libra'";"#,
    ] {
        assert_ne!(
            command_line, bare,
            "hook command must not be a bare PATH lookup"
        );
    }
    // The actual invocation goes through the pinned constant (raw shell
    // interpolation), so no handler ever spells out a bare `libra ` call.
    assert!(
        content.contains("${{ raw: LIBRA_COMMAND }} agent hooks opencode ${verb}"),
        "the forward invocation must interpolate LIBRA_COMMAND:\n{content}"
    );

    // Disable removes the managed file.
    let out = repo.run(&["agent", "disable", "--agent", "opencode"], None, &[]);
    assert!(out.status.success(), "disable opencode: {}", describe(&out));
    assert!(
        !plugin_path.exists(),
        "disable must remove the Libra-managed plugin file"
    );

    // A user-owned file (no marker) at the same path survives another
    // disable byte-for-byte.
    let user_content = "export const MyPlugin = async () => ({});\n";
    std::fs::write(&plugin_path, user_content).expect("seed user plugin file");
    let out = repo.run(&["agent", "disable", "--agent", "opencode"], None, &[]);
    assert!(
        out.status.success(),
        "second disable must stay a safe no-op: {}",
        describe(&out)
    );
    assert_eq!(
        std::fs::read_to_string(&plugin_path).expect("read user plugin back"),
        user_content,
        "a user file without the marker must survive disable"
    );
}

/// `agent enable --agent codex` writes `$CODEX_HOME/hooks.json` handlers
/// that all start with the canonical binary path + ` hooks codex ` (the
/// stable installed surface is the top-level `libra hooks codex <verb>`
/// entry, which routes to AgentTraces; `agent hooks codex` is the legacy
/// hidden spelling), plus one `[hooks.state."…"]` trust section per
/// forwarded event (11) with a `sha256:` trusted_hash. A trusted install
/// ingests session-start with no trust-gap banner; tampering one hash
/// makes the banner name exactly one gap; disable removes the Libra
/// hooks.json entries and config.toml state sections.
#[test]
fn codex_enable_writes_canonical_binary_path_and_trust_entries() {
    let repo = HookRepo::init();
    let canonical = canonical_binary_path();

    let codex_home = repo.home.join(".codex");
    let codex_home_str = codex_home.display().to_string();
    let codex_env: &[(&str, &str)] = &[("CODEX_HOME", codex_home_str.as_str())];
    let hooks_path = codex_home.join("hooks.json");
    let config_path = codex_home.join("config.toml");

    let out = repo.run(&["agent", "enable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "enable codex: {}", describe(&out));

    // hooks.json: every forwarded event carries exactly the canonical
    // command, and every Libra-managed handler starts with the canonical
    // absolute path (no bare `libra`).
    let hooks_json: Value = serde_json::from_str(
        &std::fs::read_to_string(&hooks_path)
            .unwrap_or_else(|err| panic!("hooks.json missing at {}: {err}", hooks_path.display())),
    )
    .expect("hooks.json parses as JSON");
    let events = hooks_json["hooks"]
        .as_object()
        .unwrap_or_else(|| panic!("hooks.json has no hooks object: {hooks_json}"));
    for (event, verb) in CODEX_FORWARDED_EVENTS {
        let (expected_timeout, expected_capture_budget_millis) = codex_installed_timing(event);
        let expected = format!(
            "{canonical} hooks codex {verb} --capture-budget-ms {expected_capture_budget_millis}"
        );
        let found = events
            .get(*event)
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|group| {
                    group["hooks"].as_array().is_some_and(|handlers| {
                        handlers.iter().any(|handler| {
                            handler["type"] == json!("command")
                                && handler["command"] == json!(expected.clone())
                                && handler["timeout"] == json!(expected_timeout)
                        })
                    })
                })
            });
        assert!(
            found,
            "event '{event}' must carry the canonical command '{expected}': {hooks_json}"
        );
    }
    let managed_prefix = format!("{canonical} hooks codex ");
    for (event, groups) in events {
        for group in groups.as_array().into_iter().flatten() {
            for handler in group["hooks"].as_array().into_iter().flatten() {
                let command = handler["command"].as_str().unwrap_or_default();
                if command.contains(" hooks codex ") {
                    assert!(
                        command.starts_with(&managed_prefix),
                        "Libra handler under '{event}' must start with the canonical \
                         binary path: {command}"
                    );
                }
            }
        }
    }

    // config.toml: one `[hooks.state."…"]` section per forwarded event,
    // each with a sha256 trusted_hash.
    let config = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|err| panic!("config.toml missing at {}: {err}", config_path.display()));
    assert_eq!(
        config.matches("[hooks.state.\"").count(),
        CODEX_FORWARDED_EVENTS.len(),
        "one trust section per forwarded event expected:\n{config}"
    );
    assert_eq!(
        config.matches("trusted_hash = \"sha256:").count(),
        CODEX_FORWARDED_EVENTS.len(),
        "every trust section must carry a sha256 trusted_hash:\n{config}"
    );

    // A trusted install ingests session-start silently (gaps == 0 → no
    // banner on stderr). Use the exact surface the installed commands
    // invoke: the top-level `libra hooks codex <verb>` entry.
    let envelope = json!({
        "hook_event_name": "SessionStart",
        "session_id": "sess-codex-trust",
        "cwd": repo.repo.to_string_lossy(),
    })
    .to_string();
    let out = repo.run(
        &[
            "hooks",
            "codex",
            "session-start",
            "--capture-budget-ms",
            "29000",
        ],
        Some(&envelope),
        codex_env,
    );
    assert!(
        out.status.success(),
        "trusted session-start ingest: {}",
        describe(&out)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("not locally approved"),
        "zero trust gaps must render no banner: {}",
        describe(&out)
    );

    // Tamper exactly one trusted_hash → the next session-start still exits
    // 0 (banner only, never blocking) and names exactly one gap.
    let needle = "trusted_hash = \"";
    let start = config.find(needle).expect("a trusted_hash line to tamper") + needle.len();
    let end = start + config[start..].find('"').expect("closing quote");
    let tampered = format!("{}sha256:0000{}", &config[..start], &config[end..]);
    std::fs::write(&config_path, &tampered).expect("write tampered config.toml");

    let out = repo.run(
        &[
            "hooks",
            "codex",
            "session-start",
            "--capture-budget-ms",
            "29000",
        ],
        Some(&envelope),
        codex_env,
    );
    assert!(
        out.status.success(),
        "the trust-gap banner must never block the hook: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("1 Libra-managed Codex hook(s) are not locally approved"),
        "banner must name exactly one gap: {}",
        describe(&out)
    );

    // The tampered trust state makes strict status fail closed. Keep a user
    // handler alongside the still-canonical Libra entries as the preservation
    // control: disable must remove only handlers with durable ownership proof
    // and their matching trust sections without first repairing trust.
    let mut hooks_json: Value = serde_json::from_str(
        &std::fs::read_to_string(&hooks_path)
            .unwrap_or_else(|err| panic!("read hooks.json to seed user handler: {err}")),
    )
    .expect("installed hooks.json still parses");
    let events = hooks_json["hooks"]
        .as_object_mut()
        .expect("hooks.json has mutable hooks object");
    events
        .get_mut("SessionStart")
        .and_then(Value::as_array_mut)
        .expect("SessionStart groups")
        .push(json!({
            "matcher": "user-owned",
            "hooks": [{"type": "command", "command": "echo keep-codex-user"}],
        }));
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&hooks_json).expect("render hooks.json with user handler"),
    )
    .expect("seed canonical Codex commands and user hook");

    // Disable removes stale, untrusted but durably-owned Libra entries and
    // every matching trust section without touching the user-owned handler.
    let out = repo.run(&["agent", "disable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "disable codex: {}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hooks not installed or stale"),
        "the tampered config must fail strict status before cleanup: {}",
        describe(&out)
    );
    let hooks_after = std::fs::read_to_string(&hooks_path).expect("hooks.json is never deleted");
    assert!(
        !hooks_after.contains(" hooks codex "),
        "disable must remove every durably-owned Libra handler:\n{hooks_after}"
    );
    assert!(
        hooks_after.contains("echo keep-codex-user"),
        "disable must preserve the user-owned handler:\n{hooks_after}"
    );
    let config_after = std::fs::read_to_string(&config_path).expect("config.toml still readable");
    assert!(
        !config_after.contains("[hooks.state.\""),
        "disable must remove every Libra trust section:\n{config_after}"
    );
    assert!(
        !config_after.contains("libra-managed codex hook trust entry"),
        "disable must remove the Libra marker comments:\n{config_after}"
    );
}

/// A failed installation-state probe is not the same as a valid stale
/// configuration. In particular, Codex removal edits `hooks.json` before its
/// separate `config.toml` trust-state rewrite, so `agent disable` must reject
/// malformed trust state before either file is changed. The preceding Codex
/// regression keeps the complementary valid-but-stale cleanup case pinned.
#[test]
fn codex_disable_rejects_malformed_trust_config_without_partial_mutation() {
    let repo = HookRepo::init();
    let codex_home = repo.home.join(".codex");
    let codex_home_str = codex_home.display().to_string();
    let codex_env: &[(&str, &str)] = &[("CODEX_HOME", codex_home_str.as_str())];
    let hooks_path = codex_home.join("hooks.json");
    let config_path = codex_home.join("config.toml");

    let out = repo.run(&["agent", "enable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "enable Codex: {}", describe(&out));
    let hooks_before = std::fs::read_to_string(&hooks_path).expect("read installed hooks.json");
    let malformed_config = "[hooks.state\ntrusted_hash = \"unterminated table\"\n";
    std::fs::write(&config_path, malformed_config).expect("seed malformed Codex config.toml");

    let out = repo.run(&["agent", "disable", "--agent", "codex"], None, codex_env);
    assert!(
        !out.status.success(),
        "malformed Codex trust state must fail before cleanup: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to inspect 'codex' hook installation state before disabling")
            && stderr.contains("invalid Codex config TOML"),
        "the error must identify the failed preflight, not claim cleanup: {}",
        describe(&out)
    );
    assert_eq!(
        std::fs::read_to_string(&hooks_path).expect("read hooks after failed disable"),
        hooks_before,
        "hooks.json must remain byte-stable when config preflight fails"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read config after failed disable"),
        malformed_config,
        "config.toml must remain byte-stable when preflight fails"
    );
}

/// Enable is the same coupled two-file operation as disable. A malformed
/// pre-existing trust config must therefore reject the command before it can
/// add any managed handler to an otherwise valid user hooks.json.
#[test]
fn codex_enable_rejects_malformed_trust_config_without_partial_mutation() {
    let repo = HookRepo::init();
    let codex_home = repo.home.join(".codex");
    let codex_home_str = codex_home.display().to_string();
    let codex_env: &[(&str, &str)] = &[("CODEX_HOME", codex_home_str.as_str())];
    let hooks_path = codex_home.join("hooks.json");
    let config_path = codex_home.join("config.toml");
    std::fs::create_dir_all(&codex_home).expect("create Codex home");
    let user_hooks = b"{\n  \"hooks\": {\n    \"SessionStart\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo keep-user-hook\"}]}]\n  }\n}\n";
    let malformed_config = b"[hooks.state\ntrusted_hash = \"unterminated table\"\n";
    std::fs::write(&hooks_path, user_hooks).expect("seed user hooks.json");
    std::fs::write(&config_path, malformed_config).expect("seed malformed config.toml");

    let out = repo.run(&["agent", "enable", "--agent", "codex"], None, codex_env);
    assert!(
        !out.status.success(),
        "malformed Codex config must reject enable before publishing hooks: {}",
        describe(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid Codex config TOML"),
        "the error must identify the configuration preflight: {}",
        describe(&out)
    );
    assert_eq!(
        std::fs::read(&hooks_path).expect("read hooks after failed enable"),
        user_hooks,
        "hooks.json must remain byte-stable after failed enable",
    );
    assert_eq!(
        std::fs::read(&config_path).expect("read config after failed enable"),
        malformed_config,
        "config.toml must remain byte-stable after failed enable",
    );
}

/// A marker-owned command with an old executable is valid cleanup input but
/// intentionally reads as status-stale. That `Ok(false)` status must not
/// bypass Codex's own two-file preflight: malformed trust config still has to
/// stop removal before `hooks.json` is published.
#[test]
fn codex_disable_keeps_stale_managed_hooks_when_trust_config_is_malformed() {
    let repo = HookRepo::init();
    let codex_home = repo.home.join(".codex");
    let codex_home_str = codex_home.display().to_string();
    let codex_env: &[(&str, &str)] = &[("CODEX_HOME", codex_home_str.as_str())];
    let hooks_path = codex_home.join("hooks.json");
    let config_path = codex_home.join("config.toml");

    let out = repo.run(&["agent", "enable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "enable Codex: {}", describe(&out));
    let mut hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(&hooks_path).expect("read installed hooks.json"),
    )
    .expect("parse installed hooks.json");
    let stale_handler = &mut hooks["hooks"]["SessionStart"][0]["hooks"][0];
    assert_eq!(stale_handler["statusMessage"], json!("libra capture"));
    stale_handler["command"] =
        json!("/stale/libra hooks codex session-start --capture-budget-ms 29000");
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&hooks).expect("render stale managed hooks.json"),
    )
    .expect("seed stale managed handler");
    let hooks_before = std::fs::read_to_string(&hooks_path).expect("read stale hooks.json");
    let malformed_config = "[hooks.state\ntrusted_hash = \"unterminated table\"\n";
    std::fs::write(&config_path, malformed_config).expect("seed malformed Codex config.toml");

    let out = repo.run(&["agent", "disable", "--agent", "codex"], None, codex_env);
    assert!(
        !out.status.success(),
        "stale Codex cleanup with malformed config must fail before mutation: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to disable 'codex'")
            && stderr.contains("invalid Codex config TOML"),
        "provider-local config preflight must surface its error: {}",
        describe(&out)
    );
    assert_eq!(
        std::fs::read_to_string(&hooks_path).expect("read hooks after failed disable"),
        hooks_before,
        "the stale, marker-owned handler must remain when config preparation fails"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read config after failed disable"),
        malformed_config,
        "config.toml must remain byte-stable when preparation fails"
    );
}

/// A Claude settings file that resembles a pre-capture-budget installer does
/// not carry the timeout/budget ownership proof required by current cleanup.
/// Strict status reports it as stale, but `agent disable` must preserve those
/// unverified entries as well as a neighbouring user hook.
#[test]
fn claude_disable_preserves_unverified_legacy_commands_and_user_hook() {
    let repo = HookRepo::init();
    let settings_path = repo.repo.join(".claude/settings.json");

    let out = repo.run(&["agent", "enable", "--agent", "claude"], None, &[]);
    assert!(out.status.success(), "enable Claude: {}", describe(&out));

    let mut settings: Value = serde_json::from_str(
        &std::fs::read_to_string(&settings_path)
            .unwrap_or_else(|err| panic!("read installed Claude settings: {err}")),
    )
    .expect("installed Claude settings parse");
    {
        let hooks = settings["hooks"]
            .as_object_mut()
            .expect("Claude settings have mutable hooks object");
        let mut legacy_commands = 0;
        for matchers in hooks.values_mut() {
            for matcher in matchers.as_array_mut().into_iter().flatten() {
                for hook in matcher["hooks"].as_array_mut().into_iter().flatten() {
                    let Some(command) = hook["command"].as_str().map(str::to_owned) else {
                        continue;
                    };
                    if command.contains(" hooks claude ") {
                        let legacy = command
                            .strip_suffix(" --capture-budget-ms 9000")
                            .unwrap_or_else(|| {
                                panic!("expected current Claude capture budget in '{command}'")
                            });
                        hook["command"] = json!(legacy);
                        legacy_commands += 1;
                    }
                }
            }
        }
        assert_eq!(
            legacy_commands,
            CLAUDE_FORWARDED_EVENTS.len(),
            "every managed Claude command must exercise the no-budget legacy shape"
        );
        hooks
            .get_mut("SessionStart")
            .and_then(Value::as_array_mut)
            .expect("Claude SessionStart matchers")
            .push(json!({
                "matcher": "user-owned",
                "hooks": [{"type": "command", "command": "echo keep-claude-user", "timeout": 1}],
            }));
    }
    let seeded_settings =
        serde_json::to_string_pretty(&settings).expect("render legacy Claude settings");
    std::fs::write(&settings_path, &seeded_settings)
        .expect("seed legacy Claude commands and user hook");

    let out = repo.run(&["agent", "disable", "--agent", "claude"], None, &[]);
    assert!(out.status.success(), "disable Claude: {}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hooks not installed or stale"),
        "the no-budget Claude config must fail strict status before cleanup: {}",
        describe(&out)
    );
    let settings_after =
        std::fs::read_to_string(&settings_path).expect("Claude settings remain readable");
    assert!(
        settings_after.contains(" hooks claude "),
        "disable must not claim legacy Claude handlers without a budget proof:\n{settings_after}"
    );
    assert!(
        settings_after.contains("echo keep-claude-user"),
        "disable must preserve the user-owned Claude hook:\n{settings_after}"
    );
    assert_eq!(
        settings_after, seeded_settings,
        "disable must not rewrite settings containing only unverified legacy and user hooks"
    );
}

/// The exact hook command is a public surface, so it alone cannot prove
/// ownership. Re-enable and disable must preserve a user-scoped Claude
/// handler and an unmarked *renamed* Codex handler lacking persistent
/// installer provenance.
#[test]
fn enable_and_disable_preserve_scoped_or_unmarked_renamed_user_hook_commands() {
    let repo = HookRepo::init();
    let canonical = canonical_binary_path();

    let claude_path = repo.repo.join(".claude/settings.json");
    let claude_command = format!("{canonical} hooks claude session-start --capture-budget-ms 9000");
    let out = repo.run(&["agent", "enable", "--agent", "claude"], None, &[]);
    assert!(out.status.success(), "enable Claude: {}", describe(&out));
    let mut claude_settings: Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_path)
            .unwrap_or_else(|err| panic!("read Claude settings: {err}")),
    )
    .expect("Claude settings parse");
    claude_settings["hooks"]["SessionStart"]
        .as_array_mut()
        .expect("Claude SessionStart matchers")
        .push(json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": claude_command.clone(), "timeout": 10}],
        }));
    std::fs::write(
        &claude_path,
        serde_json::to_string_pretty(&claude_settings).expect("render Claude settings"),
    )
    .expect("seed scoped Claude user hook");
    let out = repo.run(&["agent", "enable", "--agent", "claude"], None, &[]);
    assert!(out.status.success(), "re-enable Claude: {}", describe(&out));
    let out = repo.run(&["agent", "disable", "--agent", "claude"], None, &[]);
    assert!(out.status.success(), "disable Claude: {}", describe(&out));
    let claude_after = std::fs::read_to_string(&claude_path).expect("read Claude settings");
    assert!(
        claude_after.contains("\"matcher\": \"Bash\"") && claude_after.contains(&claude_command),
        "enable/disable must preserve the scoped Claude user hook:\n{claude_after}"
    );

    let codex_home = repo.home.join(".codex");
    let codex_home_str = codex_home.display().to_string();
    let codex_env: &[(&str, &str)] = &[("CODEX_HOME", codex_home_str.as_str())];
    let hooks_path = codex_home.join("hooks.json");
    let codex_command = format!("{canonical} hooks codex session-start --capture-budget-ms 29000");
    let unmarked_renamed_codex_command =
        "/opt/user-capture-hook hooks codex session-start --capture-budget-ms 29000";
    let out = repo.run(&["agent", "enable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "enable Codex: {}", describe(&out));
    let mut codex_hooks: Value = serde_json::from_str(
        &std::fs::read_to_string(&hooks_path)
            .unwrap_or_else(|err| panic!("read Codex hooks: {err}")),
    )
    .expect("Codex hooks parse");
    let session_start = codex_hooks["hooks"]["SessionStart"]
        .as_array_mut()
        .expect("Codex SessionStart groups");
    session_start.push(json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": codex_command.clone(),
            "timeout": 30,
            "statusMessage": "libra capture",
        }],
    }));
    session_start.push(json!({
        "hooks": [{
            "type": "command",
            "command": unmarked_renamed_codex_command,
            "timeout": 30,
        }],
    }));
    std::fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&codex_hooks).expect("render Codex hooks"),
    )
    .expect("seed Codex user hooks");
    let out = repo.run(&["agent", "enable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "re-enable Codex: {}", describe(&out));
    let out = repo.run(&["agent", "disable", "--agent", "codex"], None, codex_env);
    assert!(out.status.success(), "disable Codex: {}", describe(&out));
    let codex_after: Value = serde_json::from_str(
        &std::fs::read_to_string(&hooks_path)
            .unwrap_or_else(|err| panic!("read Codex hooks after disable: {err}")),
    )
    .expect("Codex hooks remain valid JSON");
    let surviving_groups = codex_after["hooks"]["SessionStart"]
        .as_array()
        .expect("Codex SessionStart groups remain");
    assert!(
        surviving_groups.iter().any(|group| {
            group["matcher"] == json!("Bash")
                && group["hooks"].as_array().is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook["command"] == json!(codex_command.clone())
                            && hook["statusMessage"] == json!("libra capture")
                    })
                })
        }),
        "enable/disable must preserve the scoped Codex user hook: {codex_after}"
    );
    assert!(
        surviving_groups.iter().any(|group| {
            group.get("matcher").is_none()
                && group["hooks"].as_array().is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook["command"] == json!(unmarked_renamed_codex_command)
                            && hook.get("statusMessage").is_none()
                    })
                })
        }),
        "enable/disable must preserve the unmarked Codex user hook: {codex_after}"
    );
}
