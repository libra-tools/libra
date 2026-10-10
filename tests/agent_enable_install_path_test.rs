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
    // The actual invocation uses the pinned filename without a shell.
    assert!(
        content.contains("spawnSync(LIBRA_COMMAND, args") && content.contains("shell: false"),
        "the forward invocation must use the pinned LIBRA_COMMAND filename:\n{content}"
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

// OG-02 executes the installed module under a real interpreter. Only the
// pinned executable JSON literal is replaced with a controlled fake exporter;
// every event handler, registration and timeout is the installed production JS.
fn og02_installed_module(repo: &HookRepo, executable: &std::path::Path) -> String {
    let out = repo.run(&["agent", "enable", "--agent", "opencode"], None, &[]);
    assert!(out.status.success(), "install: {}", describe(&out));
    let source =
        std::fs::read_to_string(repo.repo.join(".opencode/plugin/libra-hooks.js")).unwrap();
    let old = format!(
        "const LIBRA_COMMAND = {};",
        serde_json::to_string(&canonical_binary_path()).unwrap()
    );
    assert_eq!(source.matches(&old).count(), 1);
    source.replacen(
        &old,
        &format!(
            "const LIBRA_COMMAND = {};",
            serde_json::to_string(executable).unwrap()
        ),
        1,
    )
}

fn og02_runtime_path(runtime: &str) -> Option<PathBuf> {
    let candidate = if runtime == "bun" {
        std::env::var_os("LIBRA_TEST_BUN_BINARY").unwrap_or_else(|| runtime.into())
    } else {
        runtime.into()
    };
    let out = match Command::new(&candidate).arg("--version").output() {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("[skip] OG-02 interpreter unavailable: {runtime}; runtime gate unproved");
            return None;
        }
        Err(err) => panic!("probe {runtime}: {err}"),
    };
    assert!(
        out.status.success(),
        "interpreter probe failed: {}",
        describe(&out)
    );
    let resolved = Command::new(&candidate)
        .args(["-e", "process.stdout.write(process.execPath)"])
        .output()
        .unwrap();
    assert!(
        resolved.status.success(),
        "resolve interpreter: {}",
        describe(&resolved)
    );
    Some(PathBuf::from(String::from_utf8(resolved.stdout).unwrap()))
}

fn og02_native(name: &str, data: Value) -> Value {
    json!({"type":name,"data":data})
}

fn og02_runtime_conformance(runtime: &str) {
    use std::os::unix::fs::PermissionsExt;

    use libra::internal::ai::hooks::providers::opencode::events::{
        OPENCODE_HOOK_EVENT_SPECS, OPENCODE_LEGACY_EVENT_SPECS, OpenCodeForwarding,
    };
    let Some(interpreter) = og02_runtime_path(runtime) else {
        return;
    };
    let repo = HookRepo::init();
    let fake = repo.repo.join("exporter with ' quote and $literal.mjs");
    let log = repo.repo.join("frames.jsonl");
    std::fs::write(&fake, format!(r#"#!{}
import {{ readFileSync, appendFileSync }} from 'node:fs';
const frame = JSON.parse(readFileSync(0, 'utf8'));
appendFileSync(process.env.LIBRA_TEST_FORWARD_LOG, JSON.stringify({{ frame, args:process.argv.slice(2), cwd:process.cwd() }})+'\n');
process.stdout.write('{{"decision":"deny","continue":false,"inject_context":"PRIVATE-CONTROL"}}');
"#, interpreter.display())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let plugin = og02_installed_module(&repo, &fake);
    std::fs::write(repo.repo.join("plugin.mjs"), &plugin).unwrap();
    let id = "synthetic-a";
    let events = json!([
        og02_native("session.created", json!({"sessionID":id})),
        og02_native(
            "session.inbox.enqueued",
            json!({"sessionID":id,"inboxID":"prompt-1","item":{"type":"user","payload":{"text":"Synthetic prompt"}}})
        ),
        og02_native(
            "session.inbox.delivered",
            json!({"sessionID":id,"inboxID":"prompt-1"})
        ),
        og02_native(
            "session.inbox.delivered",
            json!({"sessionID":id,"inboxID":"prompt-1"})
        ),
        og02_native(
            "session.step.started",
            json!({"sessionID":id,"model":{"id":"synthetic-model"}})
        ),
        og02_native("session.execution.succeeded", json!({"sessionID":id})),
        og02_native(
            "session.execution.failed",
            json!({"sessionID":id,"error":{"private":"PRIVATE-ERROR"}})
        ),
        og02_native(
            "session.execution.interrupted",
            json!({"sessionID":id,"reason":"user"})
        ),
        og02_native(
            "session.execution.interrupted",
            json!({"sessionID":id,"reason":"superseded"})
        ),
        og02_native(
            "session.execution.interrupted",
            json!({"sessionID":id,"reason":"inactivity"})
        ),
        og02_native(
            "session.execution.interrupted",
            json!({"sessionID":id,"reason":"shutdown"})
        ),
        og02_native(
            "session.execution.interrupted",
            json!({"sessionID":id,"reason":"unknown"})
        ),
        og02_native(
            "session.compaction.ended",
            json!({"sessionID":id,"text":"PRIVATE-COMPACTION"})
        ),
        og02_native("session.compacted", json!({"sessionID":id})),
        og02_native(
            "session.status",
            json!({"sessionID":id,"status":{"type":"busy"}})
        ),
        og02_native(
            "session.status",
            json!({"sessionID":id,"status":{"type":"retry"}})
        ),
        og02_native(
            "session.status",
            json!({"sessionID":id,"status":{"type":"idle"}})
        ),
        og02_native(
            "message.updated",
            json!({"info":{"sessionID":id,"id":"legacy-1","role":"user"}})
        ),
        og02_native(
            "message.part.updated",
            json!({"part":{"sessionID":id,"messageID":"legacy-1","type":"text","text":"Synthetic legacy prompt"}})
        ),
        og02_native(
            "session.inbox.enqueued",
            json!({"sessionID":id,"inboxID":"non-user","item":{"type":"synthetic","payload":{"text":"PRIVATE-SYNTHETIC"}}})
        ),
        og02_native(
            "session.inbox.delivered",
            json!({"sessionID":id,"inboxID":"non-user"})
        ),
        og02_native(
            "session.error",
            json!({"sessionID":id,"private":"PRIVATE-ERROR"})
        ),
        og02_native("session.deleted", json!({"sessionID":id})),
        og02_native("session.created", json!({"sessionID":"synthetic-b"})),
        og02_native("server.instance.disposed", json!({})),
    ]);
    let runner = format!(
        r#"
import plugin from './plugin.mjs';
import {{ readFileSync }} from 'node:fs';
const events={events};
let tool;
let finish; const finished=new Promise(resolve=>{{finish=resolve;}});
const context={{
 location:{{directory:{cwd}}},
 tool:{{hook:async(name,callback)=>{{ if(name!=='execute.after') throw new Error('wrong tool hook'); tool=callback; return {{dispose:async()=>{{}}}}; }}}},
 event:{{subscribe:({{signal}})=> (async function*(){{
   try {{ for(const event of events) {{
     const before=JSON.stringify(event);
     yield event;
     if(JSON.stringify(event)!==before) throw new Error('event mutated');
     // A sync hook must have completed its child before advancing the stream.
     if(['session.created','session.inbox.delivered','session.execution.succeeded','session.deleted'].includes(event.type) && event.data.inboxID!=='non-user') {{
       const frames=readFileSync(process.env.LIBRA_TEST_FORWARD_LOG,'utf8');
       if(!frames.includes(event.type)) throw new Error('sync hook not complete');
     }}
     if(event.type==='session.created' && event.data.sessionID==='synthetic-a') {{
       const input={{tool:'synthetic-tool',sessionID:'synthetic-a',id:'call-1',input:{{private:'PRIVATE-TOOL'}},result:{{private:'PRIVATE-RESULT'}},status:'completed'}};
       const beforeTool=JSON.stringify(input); const begin=performance.now(); tool(input);
       if(performance.now()-begin>25) throw new Error("async observation handler exceeded 25 ms");
       if(JSON.stringify(input)!==beforeTool) throw new Error('tool mutated');
     }}
     await new Promise(resolve=>setTimeout(resolve,50));
   }} }} finally {{finish();}}
 }})()}}
}};
const cleanup=await plugin.setup(context); await finished;
await new Promise(resolve=>setTimeout(resolve,100));
await cleanup(); await cleanup();
// A new instance in the same process exercises the singleton exit path on
// both interpreters, rather than disposing away its active registry first.
let exitDone; const exitFinished=new Promise(resolve=>{{exitDone=resolve;}});
context.event.subscribe=()=> (async function*(){{try{{
 yield {{type:'session.created',data:{{sessionID:'synthetic-exit-a'}}}};
 yield {{type:'session.created',data:{{sessionID:'synthetic-exit-b'}}}};
}}finally{{exitDone();}} }})();
await plugin.setup(context);await exitFinished;process.exit(0);
"#,
        cwd = serde_json::to_string(&repo.repo).unwrap()
    );
    std::fs::write(repo.repo.join("runner.mjs"), runner).unwrap();
    let out = Command::new(&interpreter)
        .arg("--no-warnings")
        .arg(repo.repo.join("runner.mjs"))
        .current_dir(&repo.repo)
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .env("LIBRA_TEST_FORWARD_LOG", &log)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "runtime {runtime}: {}",
        describe(&out)
    );
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "observation output: {}",
        describe(&out)
    );
    let raw = std::fs::read_to_string(log).unwrap();
    assert!(
        !raw.contains("PRIVATE-"),
        "private unobserved payload forwarded"
    );
    let frames: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let actual: std::collections::BTreeSet<_> = frames
        .iter()
        .map(|r| r["frame"]["hook_event_name"].as_str().unwrap())
        .collect();
    let expected: std::collections::BTreeSet<_> = OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .chain(OPENCODE_LEGACY_EVENT_SPECS)
        .filter(|s| {
            matches!(
                s.forwarding,
                OpenCodeForwarding::Standalone
                    | OpenCodeForwarding::PluginHook
                    | OpenCodeForwarding::LegacyCompatibility
            )
        })
        .map(|s| s.name)
        .collect();
    assert_eq!(
        actual, expected,
        "actual producer set == registry current + compat set"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|r| r["frame"]["hook_event_name"] == "session.inbox.delivered")
            .count(),
        1
    );
    assert_eq!(
        frames
            .iter()
            .filter(|r| r["frame"]["hook_event_name"] == "message.updated")
            .count(),
        1
    );
    for r in &frames {
        let f = &r["frame"];
        let verb = libra::internal::ai::hooks::providers::opencode::events::event_spec(
            f["hook_event_name"].as_str().unwrap(),
        )
        .unwrap()
        .command
        .unwrap()
        .to_string();
        assert_eq!(r["args"], json!(["agent", "hooks", "opencode", verb]));
        assert_eq!(r["cwd"], json!(std::fs::canonicalize(&repo.repo).unwrap()));
        if f["hook_event_name"]
            .as_str()
            .unwrap()
            .starts_with("session.execution.")
        {
            assert_eq!(f["model"], "synthetic-model");
        }
    }
    assert_eq!(
        frames
            .iter()
            .find(|r| r["frame"]["hook_event_name"] == "session.inbox.delivered")
            .unwrap()["frame"]["prompt"],
        "Synthetic prompt"
    );
    assert_eq!(
        frames
            .iter()
            .find(|r| r["frame"]["hook_event_name"] == "message.updated")
            .unwrap()["frame"]["prompt"],
        "Synthetic legacy prompt"
    );
    for id in ["synthetic-exit-a", "synthetic-exit-b"] {
        let endings: Vec<_> = frames
            .iter()
            .filter(|r| r["frame"]["session_id"] == id)
            .map(|r| r["frame"]["hook_event_name"].as_str().unwrap())
            .collect();
        assert_eq!(
            endings,
            ["session.created", "server.instance.disposed"],
            "{runtime} exit must flush each tracked session exactly once"
        );
    }
    println!("[executed] OG-02 installed module {runtime} runtime and registry equality");
}

#[test]
fn opencode_plugin_runs_under_node_fake_exporter() {
    og02_runtime_conformance("node");
}
#[test]
fn opencode_plugin_runs_under_bun_fake_exporter() {
    og02_runtime_conformance("bun");
}

#[test]
fn opencode_plugin_template_is_node_child_process() {
    let repo = HookRepo::init();
    let out = repo.run(&["agent", "enable", "--agent", "opencode"], None, &[]);
    assert!(out.status.success());
    let source =
        std::fs::read_to_string(repo.repo.join(".opencode/plugin/libra-hooks.js")).unwrap();
    for absent in ["BunShell", ".quiet()", ".nothrow()", "session.idle", "${"] {
        assert!(!source.contains(absent), "obsolete producer token {absent}");
    }
    for present in [
        "node:child_process",
        "shell: false",
        "spawnSync",
        "killSignal: \"SIGKILL\"",
        "export default",
        "setup(context)",
        "context.event.subscribe",
        "context.tool.hook(\"execute.after\"",
        "// libra-plugin-template: opencode-2.0.26-v1",
    ] {
        assert!(
            source.contains(present),
            "missing production token {present}"
        );
    }
}

#[test]
fn opencode_plugin_skip_visible_when_interpreter_missing() {
    assert!(og02_runtime_path("libra-absent-interpreter-og02-6b67e91").is_none());
}

#[test]
fn opencode_installer_rejects_non_regular_config_paths_without_blocking() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::symlink},
        time::Instant,
    };
    for slot in [
        ".opencode",
        ".opencode/plugin",
        ".opencode/plugins",
        ".opencode/plugin/libra-hooks.js",
        ".opencode/plugins/libra-hooks.js",
    ] {
        for kind in ["symlink", "wrong-kind", "fifo", "socket"] {
            let repo = HookRepo::init();
            let path = repo.repo.join(slot);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let outside = repo.home.join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("sentinel"), "PRIVATE-SENTINEL").unwrap();
            let _socket = match kind {
                "symlink" => {
                    symlink(&outside, &path).unwrap();
                    None
                }
                "wrong-kind" => {
                    if slot.ends_with(".js") {
                        std::fs::create_dir(&path).unwrap();
                    } else {
                        std::fs::write(&path, "PRIVATE-SENTINEL").unwrap();
                    }
                    None
                }
                "fifo" => {
                    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                    None
                }
                "socket" => Some(std::os::unix::net::UnixListener::bind(&path).unwrap()),
                _ => unreachable!(),
            };
            let config_snapshot = og02_config_snapshot(&repo.repo.join(".opencode"));
            let before = Instant::now();
            for args in [
                &["agent", "enable", "--agent", "opencode"][..],
                &["agent", "disable", "--agent", "opencode"][..],
            ] {
                let out = repo.run(args, None, &[]);
                assert!(!out.status.success(), "unsafe {slot} {kind} accepted");
                assert!(!String::from_utf8_lossy(&out.stderr).contains("PRIVATE-SENTINEL"));
            }
            // List's structured boundary may report installed=false or a sanitized
            // inspection error, but must not block or echo configuration contents.
            let out = repo.run(&["agent", "list", "--json"], None, &[]);
            assert!(!String::from_utf8_lossy(&out.stdout).contains("PRIVATE-SENTINEL"));
            assert!(!String::from_utf8_lossy(&out.stderr).contains("PRIVATE-SENTINEL"));
            assert!(
                before.elapsed() < std::time::Duration::from_secs(5),
                "special path caused blocking"
            );
            assert_eq!(
                og02_config_snapshot(&repo.repo.join(".opencode")),
                config_snapshot,
                "unsafe installer changed its config tree"
            );
            assert_eq!(
                std::fs::read_to_string(outside.join("sentinel")).unwrap(),
                "PRIVATE-SENTINEL"
            );
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
            if kind == "wrong-kind" && !slot.ends_with(".js") {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "PRIVATE-SENTINEL");
            }
            if slot == ".opencode/plugins/libra-hooks.js" {
                assert!(
                    !repo.repo.join(".opencode/plugin").exists(),
                    "legacy preflight must precede canonical mkdir"
                );
            }
        }
    }
}

fn og02_config_snapshot(root: &std::path::Path) -> Vec<(PathBuf, u64, Vec<u8>)> {
    use std::os::unix::fs::MetadataExt;
    fn visit(path: &std::path::Path, rows: &mut Vec<(PathBuf, u64, Vec<u8>)>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let value = if metadata.is_file() {
            std::fs::read(path).unwrap()
        } else if metadata.file_type().is_symlink() {
            std::fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        rows.push((path.to_path_buf(), metadata.ino(), value));
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                visit(&entry.unwrap().path(), rows);
            }
        }
    }
    let mut rows = Vec::new();
    visit(root, &mut rows);
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}
