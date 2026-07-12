//! Codex `$CODEX_HOME/hooks.json` + `config.toml` trust-state management for
//! installing and removing Libra hook entries (AG-19).
//!
//! Contract verified against codex-cli 0.142.4 (probed live 2026-07-05,
//! cross-checked byte-exact against source `rust-v0.142.4` @57d253ad); see
//! the module docs in `mod.rs` for the full upstream facts. Key points that
//! shape this installer:
//!
//! - **User level only.** A project-level `<repo>/.codex/hooks.json` is only
//!   loaded when the *user* config marks the project trusted
//!   (`[projects."<abs path>"] trust_level = "trusted"`), which Libra cannot
//!   arrange non-interactively for arbitrary repos. Libra therefore writes
//!   its entries into `$CODEX_HOME/hooks.json` and the matching
//!   `[hooks.state]` trust entries into `$CODEX_HOME/config.toml` — the
//!   proven fully non-interactive path.
//! - **Trust double gate.** A hook only runs when its `[hooks.state."<abs
//!   hooks.json path>:<event_snake>:<matcher_group_index>:<handler_index>"]`
//!   entry has `enabled != false` **and** a `trusted_hash` equal to
//!   `"sha256:" + sha256hex(<canonical identity JSON>)`. Untrusted hooks are
//!   skipped **silently** by `codex exec`, so install always (re)writes the
//!   trust entries and `codex_hook_trust_gaps` lets the dispatcher surface a
//!   SessionStart banner when captures would be dropped.
//! - **Positional-key hazard.** The state keys embed matcher-group/handler
//!   indices (upstream TODO: durable ids). Indices are recomputed from the
//!   final `hooks.json` on every (re)install, Libra's own group is updated
//!   in place whenever possible so its index never drifts, and stale
//!   Libra-managed state keys pointing at the wrong index are removed.
//!   Removing a Libra group that precedes user groups still shifts the
//!   *user's* positional keys — uninstall warns when that happens, but the
//!   user must re-approve those hooks in Codex.
//! - **No marker fields in JSON.** Codex deserializes `hooks.json` with
//!   `deny_unknown_fields` (group fields: `matcher`/`hooks`; handler fields:
//!   `type`/`command`/`commandWindows`/`timeout`/`async`/`statusMessage`),
//!   so Libra cannot tag its entries with an extra key. A handler is
//!   Libra-managed iff its command contains the substring
//!   `" hooks codex "` — robust across renamed/stale binary paths and
//!   across the legacy `agent hooks codex` spelling.
//! - **`config.toml` is edited surgically.** No format-preserving TOML
//!   editor exists in the dependency tree (`toml` 0.8 does not round-trip
//!   comments), so Libra uses a minimal line-based section editor: it only
//!   removes/appends its own `[hooks.state."…"]` sections, each tagged with
//!   a `# libra-managed …` comment line, leaving every other byte untouched
//!   (pinned by tests). Reads (trust-gap checks) parse the whole file with
//!   the `toml` crate, so they tolerate any layout.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::super::super::{
    lifecycle::normalize_json_value,
    provider::ProviderInstallOptions,
    setup::{load_json_settings, resolve_hook_binary_path, write_json_settings},
};

/// Default `timeout` (seconds) Libra writes on its own handlers.
const DEFAULT_CODEX_HOOK_TIMEOUT_SECS: u64 = 30;
/// Codex's own handler-timeout default (seconds), used as the *effective*
/// timeout in the canonical trust-hash identity when a handler omits it.
const CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS: u64 = 600;
/// `statusMessage` shown by Codex while a Libra capture hook runs.
const LIBRA_CODEX_STATUS_MESSAGE: &str = "libra capture";
/// A handler is Libra-managed iff its command contains this substring. Codex
/// rejects unknown JSON fields, so command-shape is the only durable marker;
/// the substring rule also matches stale absolute paths from older installs.
/// Present in both the stable installed form (`<binary> hooks codex
/// <verb>`) and the legacy hidden form (`<binary> agent hooks codex
/// <verb>`), so uninstall/status recognise entries written by either.
const CODEX_MANAGED_COMMAND_MARKER: &str = " hooks codex ";
/// Comment line written immediately above every Libra-managed
/// `[hooks.state."…"]` section in `$CODEX_HOME/config.toml`. The line-based
/// editor identifies Libra's sections by this marker (plus the hooks.json
/// path prefix inside the key) and never touches unmarked sections.
const CODEX_STATE_MARKER: &str = "# libra-managed codex hook trust entry (AG-19); do not edit";

const CODEX_HOOKS_FILE: &str = "hooks.json";
const CODEX_CONFIG_FILE: &str = "config.toml";

/// Codex events Libra forwards, with the `libra hooks codex <verb>`
/// CLI verb embedded in each installed command. `PreToolUse` is deliberately
/// not installed (PostToolUse carries the response as well); the compaction
/// pair is parseable but not captured by default.
const CODEX_HOOK_FORWARD_MAP: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PostToolUse", "tool-use"),
    ("Stop", "stop"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-end"),
];

/// `$CODEX_HOME/hooks.json` — top-level key must be `"hooks"`
/// (`deny_unknown_fields` upstream); unknown keys found on disk are still
/// round-tripped rather than dropped.
#[derive(Debug, Serialize, Deserialize, Default)]
struct CodexHooksFile {
    #[serde(default)]
    hooks: BTreeMap<String, Vec<CodexHookMatcherGroup>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// One matcher group: `{"matcher": "<optional regex>", "hooks": [ … ]}`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct CodexHookMatcherGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matcher: Option<String>,
    hooks: Vec<CodexHookHandler>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// One handler. Codex accepts `type`/`command`/`commandWindows`/`timeout`/
/// `async`/`statusMessage`; the fields Libra does not write (`commandWindows`,
/// `async`) are preserved via `extra` when present on user handlers.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct CodexHookHandler {
    #[serde(rename = "type")]
    handler_type: String,
    command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
    #[serde(
        rename = "statusMessage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    status_message: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// One desired/current `[hooks.state]` entry: the positional key and the
/// canonical trust hash of the handler it gates.
#[derive(Debug, Clone, PartialEq)]
struct CodexStateEntry {
    key: String,
    trusted_hash: String,
}

pub(super) fn install_codex_hooks(options: &ProviderInstallOptions) -> Result<()> {
    let binary_path = resolve_hook_binary_path(options.binary_path.as_deref())?;
    let timeout = options
        .timeout_secs
        .unwrap_or(DEFAULT_CODEX_HOOK_TIMEOUT_SECS);
    if timeout == 0 {
        bail!("invalid --timeout: value must be greater than 0");
    }
    let codex_home = resolve_codex_home()?;
    install_codex_hooks_at(&codex_home, &binary_path, timeout)
}

pub(super) fn uninstall_codex_hooks() -> Result<()> {
    let codex_home = resolve_codex_home()?;
    uninstall_codex_hooks_at(&codex_home)
}

pub(super) fn codex_hooks_are_installed() -> Result<bool> {
    let codex_home = resolve_codex_home()?;
    let binary_path = resolve_hook_binary_path(None)?;
    codex_hooks_are_installed_at(&codex_home, &binary_path)
}

/// Count Libra-managed handlers in `$CODEX_HOME/hooks.json` lacking a
/// matching + current `trusted_hash` state entry in `$CODEX_HOME/config.toml`
/// (AG-19 trust-gap banner support: Codex skips untrusted hooks *silently*,
/// so the dispatcher surfaces a SessionStart banner when this is non-zero).
///
/// An entry with a matching hash but an explicit `enabled = false` is a
/// deliberate user disable, not a trust gap, and is not counted.
pub(super) fn codex_hook_trust_gaps() -> Result<usize> {
    let codex_home = resolve_codex_home()?;
    codex_hook_trust_gaps_at(&codex_home)
}

/// Resolve `$CODEX_HOME`: the `CODEX_HOME` env var when set (must be
/// absolute — Codex trust-state keys embed the absolute hooks.json path),
/// else `<home>/.codex`, where `<home>` honours the crate's test override
/// (`LIBRA_TEST_HOME`, mirroring the vault and hook-runtime modules) before
/// falling back to [`dirs::home_dir`].
fn resolve_codex_home() -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os("CODEX_HOME")
        && !raw.is_empty()
    {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            bail!(
                "invalid CODEX_HOME '{}': must be an absolute path (Codex trust-state keys \
                 embed the absolute hooks.json path)",
                path.display()
            );
        }
        return Ok(path);
    }
    let home = std::env::var_os("LIBRA_TEST_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .context(
            "failed to resolve the home directory for Codex hook installation \
             (set CODEX_HOME to override)",
        )?;
    Ok(home.join(".codex"))
}

/// Install/refresh the Libra-managed entries under `codex_home`:
/// read-modify-write `hooks.json` (preserving every user entry), then
/// recompute the positional `[hooks.state]` trust entries from the *final*
/// file and sync them into `config.toml`.
fn install_codex_hooks_at(codex_home: &Path, binary_path: &str, timeout: u64) -> Result<()> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    let config_path = codex_home.join(CODEX_CONFIG_FILE);

    let mut file: CodexHooksFile = load_json_settings(&hooks_path, "Codex")?;
    let changed = upsert_codex_hooks(&mut file, binary_path, timeout);
    if changed {
        write_json_settings(&hooks_path, &file, "Codex")?;
        println!(
            "Installed Codex hook forwarding at {}",
            hooks_path.display()
        );
    } else {
        println!(
            "Codex hook forwarding is already up to date at {}",
            hooks_path.display()
        );
    }

    // Positional-key hazard: always recompute the state keys from the final
    // file and rewrite Libra's trust entries, dropping stale Libra keys that
    // point at outdated indices.
    let entries = libra_state_entries(&hooks_path, &file);
    let state_changed = sync_codex_trust_state(&config_path, &hooks_path, &entries)?;
    if state_changed {
        println!(
            "Updated Codex hook trust state at {}",
            config_path.display()
        );
    } else {
        println!(
            "Codex hook trust state is already up to date at {}",
            config_path.display()
        );
    }
    Ok(())
}

/// Remove Libra-managed handlers from `hooks.json` (never deleting the file)
/// and Libra's `[hooks.state]` entries from `config.toml`. Idempotent.
fn uninstall_codex_hooks_at(codex_home: &Path) -> Result<()> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    let config_path = codex_home.join(CODEX_CONFIG_FILE);

    // Capture the pre-removal positions so exact stale keys can be cleaned
    // from config.toml even when they were written without a marker (e.g. by
    // hand or by an older install).
    let mut stale_keys = BTreeSet::new();
    if hooks_path.exists() {
        let mut file: CodexHooksFile = load_json_settings(&hooks_path, "Codex")?;
        for entry in libra_state_entries(&hooks_path, &file) {
            stale_keys.insert(entry.key);
        }
        let changed = remove_libra_codex_hooks(&mut file);
        if changed {
            write_json_settings(&hooks_path, &file, "Codex")?;
            println!("Removed Codex hook forwarding at {}", hooks_path.display());
        } else {
            println!(
                "No Libra-managed Codex hooks found at {}",
                hooks_path.display()
            );
        }
    } else {
        println!("Codex hook settings not found at {}", hooks_path.display());
    }

    let state_changed = rewrite_config_and_write(&config_path, &hooks_path, &stale_keys, &[])?;
    if state_changed {
        println!(
            "Removed Codex hook trust state at {}",
            config_path.display()
        );
    }
    Ok(())
}

/// All six forwarded events carry the exact desired command **and** every
/// Libra-managed handler has a current trust entry (`codex exec` silently
/// skips untrusted hooks, so "installed but untrusted" must read as not
/// installed).
fn codex_hooks_are_installed_at(codex_home: &Path, binary_path: &str) -> Result<bool> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    if !hooks_path.exists() {
        return Ok(false);
    }
    let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex")?;
    let all_present = CODEX_HOOK_FORWARD_MAP.iter().all(|(event, verb)| {
        let expected = codex_hook_command(binary_path, verb);
        file.hooks.get(*event).is_some_and(|groups| {
            groups.iter().any(|group| {
                group.matcher.is_none()
                    && group
                        .hooks
                        .iter()
                        .any(|hook| hook.handler_type == "command" && hook.command == expected)
            })
        })
    });
    if !all_present {
        return Ok(false);
    }
    Ok(codex_hook_trust_gaps_at(codex_home)? == 0)
}

fn codex_hook_trust_gaps_at(codex_home: &Path) -> Result<usize> {
    let hooks_path = codex_home.join(CODEX_HOOKS_FILE);
    if !hooks_path.exists() {
        return Ok(0);
    }
    let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex")?;
    let entries = libra_state_entries(&hooks_path, &file);
    if entries.is_empty() {
        return Ok(0);
    }

    let config_path = codex_home.join(CODEX_CONFIG_FILE);
    let config = load_codex_config_value(&config_path)?;
    let gaps = entries
        .iter()
        .filter(|entry| {
            let current = config
                .as_ref()
                .and_then(|value| value.get("hooks"))
                .and_then(|hooks| hooks.get("state"))
                .and_then(|state| state.get(entry.key.as_str()));
            match current {
                Some(state) => {
                    state.get("trusted_hash").and_then(|hash| hash.as_str())
                        != Some(entry.trusted_hash.as_str())
                }
                None => true,
            }
        })
        .count();
    Ok(gaps)
}

/// Parse `config.toml` tolerantly for *reading* (any layout the `toml` crate
/// accepts); returns `None` when the file is missing or blank.
fn load_codex_config_value(config_path: &Path) -> Result<Option<toml::Value>> {
    if !config_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(config_path).with_context(|| {
        format!(
            "failed to read Codex config file '{}'",
            config_path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let value = toml::from_str(&content).map_err(|err| {
        anyhow!(
            "invalid Codex config TOML at '{}': {err}",
            config_path.display()
        )
    })?;
    Ok(Some(value))
}

fn codex_hook_command(binary_path: &str, verb: &str) -> String {
    // Stable installed surface per the Codex capture contract
    // (`docs/development/tracing/agent.md` item 4): the top-level
    // `libra hooks codex <verb>` entry, which routes to AgentTraces.
    format!("{binary_path} hooks codex {verb}")
}

/// The durable Libra-ownership rule (see module docs): Codex's
/// `deny_unknown_fields` schema leaves no room for a marker key, so any
/// handler whose command contains `" hooks codex "` — including stale
/// absolute paths from older installs — is treated as Libra-managed.
fn is_managed_codex_command(command: &str) -> bool {
    command.contains(CODEX_MANAGED_COMMAND_MARKER)
}

fn desired_codex_handler(binary_path: &str, verb: &str, timeout: u64) -> CodexHookHandler {
    CodexHookHandler {
        handler_type: "command".to_string(),
        command: codex_hook_command(binary_path, verb),
        timeout: Some(timeout),
        status_message: Some(LIBRA_CODEX_STATUS_MESSAGE.to_string()),
        extra: BTreeMap::new(),
    }
}

/// Upsert the Libra-managed matcher groups, preserving every user entry.
///
/// Index-stability strategy (positional trust keys): an existing
/// matcher-less, all-Libra group is updated **in place** so its group index
/// never changes; Libra handlers found anywhere else (stale events, mixed
/// groups, duplicates) are stripped; missing groups are appended at the end
/// so user group indices are unaffected. Only groups *emptied by this
/// cleanup* are dropped — a user's already-empty group keeps its position.
fn upsert_codex_hooks(file: &mut CodexHooksFile, binary_path: &str, timeout: u64) -> bool {
    let mut changed = false;

    let mut events: Vec<String> = file.hooks.keys().cloned().collect();
    for (event, _) in CODEX_HOOK_FORWARD_MAP {
        if !events.iter().any(|name| name == event) {
            events.push((*event).to_string());
        }
    }

    for event in events {
        let desired = CODEX_HOOK_FORWARD_MAP
            .iter()
            .find(|(name, _)| *name == event)
            .map(|(_, verb)| desired_codex_handler(binary_path, verb, timeout));

        let mut groups = file.hooks.remove(&event).unwrap_or_default();
        let mut emptied = vec![false; groups.len()];
        let mut satisfied = false;

        for (index, group) in groups.iter_mut().enumerate() {
            let libra_owned = group.matcher.is_none()
                && !group.hooks.is_empty()
                && group
                    .hooks
                    .iter()
                    .all(|hook| is_managed_codex_command(&hook.command));
            match desired.as_ref() {
                Some(want) if libra_owned && !satisfied => {
                    if group.hooks.len() != 1 || group.hooks[0] != *want || !group.extra.is_empty()
                    {
                        group.hooks = vec![want.clone()];
                        group.extra.clear();
                        changed = true;
                    }
                    satisfied = true;
                }
                _ => {
                    let before = group.hooks.len();
                    group
                        .hooks
                        .retain(|hook| !is_managed_codex_command(&hook.command));
                    if group.hooks.len() != before {
                        changed = true;
                        emptied[index] = group.hooks.is_empty();
                    }
                }
            }
        }

        let mut kept: Vec<CodexHookMatcherGroup> = Vec::with_capacity(groups.len());
        for (index, group) in groups.into_iter().enumerate() {
            if emptied[index] {
                continue;
            }
            kept.push(group);
        }

        if !satisfied && let Some(want) = desired {
            kept.push(CodexHookMatcherGroup {
                matcher: None,
                hooks: vec![want],
                extra: BTreeMap::new(),
            });
            changed = true;
        }

        if !kept.is_empty() {
            file.hooks.insert(event, kept);
        }
        // An event key emptied by the cleanup stays dropped; `changed` was
        // already set when its handlers were removed.
    }

    changed
}

/// Strip every Libra-managed handler; drop groups/events *emptied by that
/// removal* (user entries — even empty ones — are preserved). Warns when a
/// removed Libra group preceded surviving user groups, because Codex's
/// positional trust keys for those user hooks then point at the wrong index
/// (upstream TODO: durable hook ids).
fn remove_libra_codex_hooks(file: &mut CodexHooksFile) -> bool {
    let mut changed = false;
    let events: Vec<String> = file.hooks.keys().cloned().collect();

    for event in events {
        let Some(mut groups) = file.hooks.remove(&event) else {
            continue;
        };
        let mut emptied = vec![false; groups.len()];
        for (index, group) in groups.iter_mut().enumerate() {
            let before = group.hooks.len();
            group
                .hooks
                .retain(|hook| !is_managed_codex_command(&hook.command));
            if group.hooks.len() != before {
                changed = true;
                emptied[index] = group.hooks.is_empty();
            }
        }

        let mut kept: Vec<CodexHookMatcherGroup> = Vec::with_capacity(groups.len());
        let mut dropped_so_far = 0usize;
        let mut shifted_user_groups = 0usize;
        for (index, group) in groups.into_iter().enumerate() {
            if emptied[index] {
                dropped_so_far += 1;
                continue;
            }
            if dropped_so_far > 0 {
                shifted_user_groups += 1;
            }
            kept.push(group);
        }
        if shifted_user_groups > 0 {
            eprintln!(
                "warning: removing Libra-managed Codex hooks shifted {shifted_user_groups} user \
                 hook group(s) under '{event}'; Codex [hooks.state] trust keys are positional, \
                 so re-approve those hooks in Codex if they stop running"
            );
        }
        if !kept.is_empty() {
            file.hooks.insert(event, kept);
        }
    }

    changed
}

/// Convert a PascalCase Codex event name to the snake_case label used in
/// `[hooks.state]` keys and canonical identities (matches the ten upstream
/// labels: `session_start`, `user_prompt_submit`, `pre_tool_use`,
/// `post_tool_use`, `stop`, `subagent_start`, `subagent_stop`,
/// `pre_compact`, `post_compact`, `permission_request`).
fn event_snake_label(event_name: &str) -> String {
    let mut out = String::with_capacity(event_name.len() + 4);
    for (index, ch) in event_name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The exact canonical identity JSON Codex hashes for its trust gate:
/// compact JSON with recursively sorted keys of
/// `{"event_name": <snake>, "matcher": <if any>, "hooks": [{"type":
/// "command", "command": <cmd>, "timeout": <effective>, "async": false,
/// "statusMessage": <if present>}]}`. Libra handlers always run synchronous,
/// so `async` is fixed at `false`.
///
/// Verified against live-probe vectors (codex-cli 0.142.4); see
/// `tests::canonical_identity_hash_matches_live_probe_vectors`.
fn canonical_hook_identity_json(
    event_snake: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    status_message: Option<&str>,
) -> String {
    let mut handler = Map::new();
    handler.insert("type".to_string(), json!("command"));
    handler.insert("command".to_string(), json!(command));
    handler.insert("timeout".to_string(), json!(timeout));
    handler.insert("async".to_string(), json!(false));
    if let Some(message) = status_message {
        handler.insert("statusMessage".to_string(), json!(message));
    }

    let mut root = Map::new();
    root.insert("event_name".to_string(), json!(event_snake));
    if let Some(matcher) = matcher {
        root.insert("matcher".to_string(), json!(matcher));
    }
    root.insert(
        "hooks".to_string(),
        Value::Array(vec![Value::Object(handler)]),
    );

    normalize_json_value(Value::Object(root)).to_string()
}

fn codex_trusted_hash(
    event_snake: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
    status_message: Option<&str>,
) -> String {
    let canonical =
        canonical_hook_identity_json(event_snake, matcher, command, timeout, status_message);
    format!("sha256:{}", hex::encode(Sha256::digest(canonical)))
}

/// Enumerate the positional trust entries for every Libra-managed handler in
/// `file` — key `<hooks.json path>:<event_snake>:<group_index>:<handler_index>`
/// plus the canonical hash Codex must find in `[hooks.state]` for the hook
/// to run.
fn libra_state_entries(hooks_path: &Path, file: &CodexHooksFile) -> Vec<CodexStateEntry> {
    let mut entries = Vec::new();
    for (event, groups) in &file.hooks {
        let snake = event_snake_label(event);
        for (group_index, group) in groups.iter().enumerate() {
            for (handler_index, handler) in group.hooks.iter().enumerate() {
                if !is_managed_codex_command(&handler.command) {
                    continue;
                }
                entries.push(CodexStateEntry {
                    key: format!(
                        "{}:{snake}:{group_index}:{handler_index}",
                        hooks_path.display()
                    ),
                    trusted_hash: codex_trusted_hash(
                        &snake,
                        group.matcher.as_deref(),
                        &handler.command,
                        handler
                            .timeout
                            .unwrap_or(CODEX_UPSTREAM_DEFAULT_TIMEOUT_SECS),
                        handler.status_message.as_deref(),
                    ),
                });
            }
        }
    }
    entries
}

/// Sync Libra's `[hooks.state]` sections in `config.toml` to `desired`,
/// validating that both the original and rewritten files parse as TOML so a
/// conflicting layout (e.g. an inline `hooks.state` table holding one of our
/// keys) fails loudly instead of writing a broken config.
fn sync_codex_trust_state(
    config_path: &Path,
    hooks_path: &Path,
    desired: &[CodexStateEntry],
) -> Result<bool> {
    let remove_exact: BTreeSet<String> = desired.iter().map(|entry| entry.key.clone()).collect();
    rewrite_config_and_write(config_path, hooks_path, &remove_exact, desired)
}

/// Shared read-rewrite-validate-write path for `config.toml`; returns whether
/// the file changed.
fn rewrite_config_and_write(
    config_path: &Path,
    hooks_path: &Path,
    remove_exact: &BTreeSet<String>,
    append: &[CodexStateEntry],
) -> Result<bool> {
    let original = if config_path.exists() {
        fs::read_to_string(config_path).with_context(|| {
            format!(
                "failed to read Codex config file '{}'",
                config_path.display()
            )
        })?
    } else {
        String::new()
    };

    if !original.trim().is_empty() {
        toml::from_str::<toml::Value>(&original).map_err(|err| {
            anyhow!(
                "invalid Codex config TOML at '{}': {err}; fix or move the file, then re-run",
                config_path.display()
            )
        })?;
    }

    let rewritten = rewrite_codex_state_sections(
        &original,
        &hooks_path.display().to_string(),
        remove_exact,
        append,
    );
    if rewritten == original {
        return Ok(false);
    }

    toml::from_str::<toml::Value>(&rewritten).map_err(|err| {
        anyhow!(
            "refusing to update Codex config at '{}': the rewrite would produce invalid TOML \
             ({err}); an existing [hooks.state] entry for a Libra key likely uses a different \
             layout — remove it manually and re-run",
            config_path.display()
        )
    })?;

    write_text_settings_atomic(config_path, &rewritten, "Codex config")?;
    Ok(true)
}

/// The minimal line-based `[hooks.state]` section editor (see module docs).
///
/// Removes (a) sections whose exact key is in `remove_exact` and (b)
/// marker-tagged sections whose key starts with `<hooks.json path>:` (stale
/// Libra keys with outdated indices), then appends `append` as fresh
/// marker-tagged sections. Every other byte of the file is preserved; the
/// round trip install→reinstall and install→uninstall is byte-stable (pinned
/// by tests).
fn rewrite_codex_state_sections(
    content: &str,
    hooks_json_path: &str,
    remove_exact: &BTreeSet<String>,
    append: &[CodexStateEntry],
) -> String {
    let our_prefix = format!("{hooks_json_path}:");
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut removed_any = false;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        if let Some(key) = parse_state_section_key(line) {
            let marked = out
                .last()
                .is_some_and(|previous| previous.trim() == CODEX_STATE_MARKER);
            if remove_exact.contains(&key) || (marked && key.starts_with(&our_prefix)) {
                if marked {
                    out.pop();
                }
                removed_any = true;
                index += 1;
                // Consume the section body: everything up to the next
                // section header or the next Libra marker (which belongs to
                // the following section). The final empty element of the
                // `split('\n')` encodes the file's trailing newline, not a
                // body line — consuming it would eat a trailing blank line
                // the user's config carried before install and break the
                // byte-restore contract (uninstall must reproduce the
                // pre-install bytes exactly).
                while index < lines.len() {
                    let body = lines[index];
                    if body.trim_start().starts_with('[') || body.trim() == CODEX_STATE_MARKER {
                        break;
                    }
                    if body.is_empty() && index == lines.len() - 1 {
                        break;
                    }
                    index += 1;
                }
                continue;
            }
        }
        out.push(line);
        index += 1;
    }

    let mut result = out.join("\n");
    if (removed_any || !append.is_empty()) && !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    for entry in append {
        result.push_str(&format!(
            "{CODEX_STATE_MARKER}\n[hooks.state.\"{}\"]\nenabled = true\ntrusted_hash = \"{}\"\n",
            escape_toml_basic_string(&entry.key),
            entry.trusted_hash,
        ));
    }
    result
}

/// Parse a `[hooks.state."<key>"]` / `[hooks.state.'<key>']` header line into
/// its unescaped key. Returns `None` for anything else (including layouts we
/// do not own, which are then left untouched).
fn parse_state_section_key(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let body = trimmed
        .strip_prefix("[hooks.state.")?
        .strip_suffix(']')?
        .trim();
    if let Some(inner) = body
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        unescape_toml_basic_string(inner)
    } else {
        body.strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .map(str::to_string)
    }
}

/// Escape a raw string for use inside a TOML basic (double-quoted) string.
fn escape_toml_basic_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ch if (ch as u32) < 0x20 || ch == '\u{7F}' => {
                out.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out
}

/// Minimal inverse of [`escape_toml_basic_string`]; returns `None` on any
/// escape sequence it does not understand so the caller treats the section
/// as foreign and leaves it untouched.
fn unescape_toml_basic_string(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'b' => out.push('\u{0008}'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'f' => out.push('\u{000C}'),
            'r' => out.push('\r'),
            'u' => {
                let digits: String = chars.by_ref().take(4).collect();
                if digits.len() != 4 {
                    return None;
                }
                let code = u32::from_str_radix(&digits, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            'U' => {
                let digits: String = chars.by_ref().take(8).collect();
                if digits.len() != 8 {
                    return None;
                }
                let code = u32::from_str_radix(&digits, 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Atomically write a text settings file using the same temp-file + rename
/// dance as `setup::write_json_settings` (which is JSON-specific and
/// therefore not reused directly here).
fn write_text_settings_atomic(path: &Path, content: &str, label: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "invalid {label} settings path without parent: '{}'",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create {label} settings directory '{}'",
            parent.display()
        )
    })?;

    let tmp_path = path.with_extension("toml.tmp");
    fs::write(&tmp_path, content).with_context(|| {
        format!(
            "failed to write temporary {label} settings file '{}'",
            tmp_path.display()
        )
    })?;

    #[cfg(windows)]
    {
        if path.exists() {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(anyhow!(
                        "failed to replace existing {label} settings file '{}': {err}",
                        path.display()
                    ));
                }
            }
        }
    }

    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace {label} settings file '{}' with '{}'",
            path.display(),
            tmp_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const BINARY: &str = "/opt/libra";

    fn hooks_path_of(codex_home: &Path) -> PathBuf {
        codex_home.join(CODEX_HOOKS_FILE)
    }

    fn config_path_of(codex_home: &Path) -> PathBuf {
        codex_home.join(CODEX_CONFIG_FILE)
    }

    /// Live-probe ground truth (codex-cli 0.142.4, 2026-07-05): the
    /// canonical-identity hash must reproduce the byte-exact `trusted_hash`
    /// values Codex itself wrote for these two hooks.
    #[test]
    fn canonical_identity_hash_matches_live_probe_vectors() {
        assert_eq!(
            codex_trusted_hash(
                "session_start",
                None,
                "/tmp/claude-1000/codex-hooks-probe/hook.sh user-session-start",
                600,
                None,
            ),
            "sha256:11a16641ac6ee4381e5ae3674660428e467257b8cc7f18c68637c64725ef6195",
        );
        assert_eq!(
            codex_trusted_hash(
                "pre_tool_use",
                None,
                "/tmp/claude-1000/codex-hooks-probe/hook.sh user-pre-tool-use",
                600,
                None,
            ),
            "sha256:eddbdb39ec91f713977b8fef8f233e9e78aa8512013c4806a859862660217688",
        );
    }

    /// The canonical identity JSON is compact with recursively sorted keys;
    /// `matcher` and `statusMessage` slot into their sorted positions.
    #[test]
    fn canonical_identity_json_is_compact_and_sorted() {
        assert_eq!(
            canonical_hook_identity_json(
                "session_start",
                None,
                "/opt/libra hooks codex session-start",
                30,
                None,
            ),
            r#"{"event_name":"session_start","hooks":[{"async":false,"command":"/opt/libra hooks codex session-start","timeout":30,"type":"command"}]}"#,
        );
        assert_eq!(
            canonical_hook_identity_json(
                "pre_tool_use",
                Some("Bash.*"),
                "/opt/libra hooks codex tool-use",
                30,
                Some("libra capture"),
            ),
            r#"{"event_name":"pre_tool_use","hooks":[{"async":false,"command":"/opt/libra hooks codex tool-use","statusMessage":"libra capture","timeout":30,"type":"command"}],"matcher":"Bash.*"}"#,
        );
    }

    /// The generic PascalCase→snake conversion reproduces all ten upstream
    /// event labels used in `[hooks.state]` keys.
    #[test]
    fn event_snake_label_matches_upstream_labels() {
        let cases = [
            ("SessionStart", "session_start"),
            ("UserPromptSubmit", "user_prompt_submit"),
            ("PreToolUse", "pre_tool_use"),
            ("PostToolUse", "post_tool_use"),
            ("Stop", "stop"),
            ("SubagentStart", "subagent_start"),
            ("SubagentStop", "subagent_stop"),
            ("PreCompact", "pre_compact"),
            ("PostCompact", "post_compact"),
            ("PermissionRequest", "permission_request"),
        ];
        for (event, expected) in cases {
            assert_eq!(event_snake_label(event), expected, "event {event}");
        }
    }

    #[test]
    fn upsert_codex_hooks_is_idempotent() {
        let mut file = CodexHooksFile::default();
        assert!(upsert_codex_hooks(&mut file, BINARY, 30));
        assert!(!upsert_codex_hooks(&mut file, BINARY, 30));
        assert_eq!(file.hooks.len(), CODEX_HOOK_FORWARD_MAP.len());
        for (event, verb) in CODEX_HOOK_FORWARD_MAP {
            let groups = file.hooks.get(*event).expect("event installed");
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].hooks.len(), 1);
            assert_eq!(groups[0].hooks[0].command, codex_hook_command(BINARY, verb));
            assert_eq!(groups[0].hooks[0].timeout, Some(30));
            assert_eq!(
                groups[0].hooks[0].status_message.as_deref(),
                Some(LIBRA_CODEX_STATUS_MESSAGE)
            );
        }
    }

    /// A stale handler from an older install (different binary path) is
    /// replaced *in place*, keeping its group index — positional trust keys
    /// must not drift on binary upgrades.
    #[test]
    fn upsert_replaces_stale_binary_in_place() {
        let mut file = CodexHooksFile::default();
        file.hooks.insert(
            "SessionStart".to_string(),
            vec![
                CodexHookMatcherGroup {
                    matcher: None,
                    hooks: vec![CodexHookHandler {
                        handler_type: "command".to_string(),
                        command: "/old/path/libra agent hooks codex session-start".to_string(),
                        timeout: Some(10),
                        status_message: None,
                        extra: BTreeMap::new(),
                    }],
                    extra: BTreeMap::new(),
                },
                CodexHookMatcherGroup {
                    matcher: Some("user".to_string()),
                    hooks: vec![CodexHookHandler {
                        handler_type: "command".to_string(),
                        command: "echo user".to_string(),
                        timeout: None,
                        status_message: None,
                        extra: BTreeMap::new(),
                    }],
                    extra: BTreeMap::new(),
                },
            ],
        );

        assert!(upsert_codex_hooks(&mut file, BINARY, 30));
        let groups = file.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(groups.len(), 2, "no group added or removed");
        assert_eq!(
            groups[0].hooks[0].command,
            codex_hook_command(BINARY, "session-start"),
            "Libra group updated in place at index 0",
        );
        assert_eq!(groups[1].hooks[0].command, "echo user");
    }

    /// Full round trip against a tempdir CODEX_HOME: user entries in both
    /// files survive byte-for-byte (config.toml) / structurally (hooks.json),
    /// reinstall is byte-stable, and uninstall restores the user's
    /// config.toml exactly.
    #[test]
    fn install_round_trip_preserves_user_content() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);

        fs::create_dir_all(&codex_home).expect("create codex home");
        let user_hooks = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"matcher": "startup", "hooks": [
                        {"type": "command", "command": "echo keep", "timeout": 3}
                    ]}
                ],
                "PreToolUse": [
                    {"hooks": [{"type": "command", "command": "echo pre"}]}
                ]
            }
        });
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&user_hooks).expect("render"),
        )
        .expect("seed hooks.json");
        let user_config = "# user config\nmodel = \"gpt-5.4\"\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n\n[hooks.state.\"/elsewhere/hooks.json:stop:0:0\"]\nenabled = true\ntrusted_hash = \"sha256:userhash\"\n";
        fs::write(&config_path, user_config).expect("seed config.toml");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");

        // hooks.json: user groups intact, Libra appended after the user's
        // SessionStart group; PreToolUse (not forwarded) untouched.
        let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let session_start = file.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(session_start.len(), 2);
        assert_eq!(session_start[0].hooks[0].command, "echo keep");
        assert_eq!(
            session_start[1].hooks[0].command,
            codex_hook_command(BINARY, "session-start"),
        );
        assert_eq!(
            file.hooks.get("PreToolUse").expect("PreToolUse")[0].hooks[0].command,
            "echo pre",
        );

        // config.toml: user bytes are an exact prefix; our sections appended
        // with the user's SessionStart group shifting ours to index 1.
        let config_after = fs::read_to_string(&config_path).expect("read config");
        assert!(
            config_after.starts_with(user_config),
            "user config bytes must be preserved as an exact prefix; got:\n{config_after}",
        );
        let session_start_key = format!("{}:session_start:1:0", hooks_path.display());
        assert!(
            config_after.contains(&format!(
                "[hooks.state.\"{}\"]",
                escape_toml_basic_string(&session_start_key)
            )),
            "our SessionStart trust key must use group index 1; got:\n{config_after}",
        );
        let expected_hash = codex_trusted_hash(
            "session_start",
            None,
            &codex_hook_command(BINARY, "session-start"),
            30,
            Some(LIBRA_CODEX_STATUS_MESSAGE),
        );
        assert!(config_after.contains(&expected_hash));

        assert!(codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Reinstall: both files byte-stable.
        let hooks_before = fs::read_to_string(&hooks_path).expect("read hooks");
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("re-install");
        assert_eq!(
            fs::read_to_string(&hooks_path).expect("read hooks"),
            hooks_before
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            config_after,
        );

        // Uninstall: user config restored byte-for-byte; user hooks intact.
        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
            "uninstall must restore the user's config.toml byte-for-byte",
        );
        let file: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let session_start = file.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(session_start.len(), 1);
        assert_eq!(session_start[0].hooks[0].command, "echo keep");
        assert!(hooks_path.exists(), "hooks.json is never deleted");
        assert!(!codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Idempotent second uninstall.
        uninstall_codex_hooks_at(&codex_home).expect("second uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
        );
    }

    /// A user config.toml with CRLF line endings (including a user-owned
    /// `[hooks.state."…"]` section) survives install → uninstall
    /// byte-for-byte: `split('\n')` keeps each `\r` attached to its line
    /// and `join("\n")` reassembles them unchanged, while
    /// `parse_state_section_key` trims the `\r` before matching keys —
    /// refutation evidence for the review claim that the line-based
    /// editor corrupts CRLF files.
    #[test]
    fn crlf_user_config_survives_install_and_uninstall_byte_for_byte() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let config_path = config_path_of(&codex_home);
        let user_config = "# user config\r\nmodel = \"gpt-5.4\"\r\n\r\n[hooks.state.\"/elsewhere/hooks.json:stop:0:0\"]\r\nenabled = true\r\ntrusted_hash = \"sha256:userhash\"\r\n";
        fs::write(&config_path, user_config).expect("seed CRLF config.toml");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let after_install = fs::read_to_string(&config_path).expect("read config");
        assert!(
            after_install.starts_with(user_config),
            "user CRLF bytes must survive install as an exact prefix; got:\n{after_install:?}",
        );
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Reinstall is byte-stable on the CRLF file too.
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("re-install");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            after_install,
            "reinstall over a CRLF user config must be byte-stable",
        );

        // Uninstall restores the CRLF user bytes exactly — user section,
        // `\r` terminators and all.
        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
            "uninstall must restore the user's CRLF config byte-for-byte",
        );
    }

    /// A user config.toml ending with a trailing blank line (here the
    /// real-world shape that broke the A6.5 local smoke on 2026-07-12: a
    /// bare user-owned `[hooks.state]` header followed by a blank line at
    /// EOF) survives install → uninstall byte-for-byte. Regression: the
    /// section-body consumer used to swallow the final empty `split('\n')`
    /// element — the encoding of the file's trailing newline — so uninstall
    /// returned the file one byte short (`…[hooks.state]\n` instead of
    /// `…[hooks.state]\n\n`).
    #[test]
    fn trailing_blank_line_user_config_survives_install_and_uninstall() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let config_path = config_path_of(&codex_home);
        let user_config =
            "# user config\nmodel = \"gpt-5.4\"\n\n[hooks]\nenabled = false\n\n[hooks.state]\n\n";
        fs::write(&config_path, user_config).expect("seed config.toml");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let after_install = fs::read_to_string(&config_path).expect("read config");
        assert!(
            after_install.starts_with(user_config),
            "user bytes (incl. the trailing blank line) must survive install as an exact \
             prefix; got:\n{after_install:?}",
        );

        uninstall_codex_hooks_at(&codex_home).expect("uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
            "uninstall must restore the trailing blank line byte-for-byte",
        );

        // Idempotent second uninstall.
        uninstall_codex_hooks_at(&codex_home).expect("second uninstall");
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            user_config,
        );
    }

    /// Fresh install into an empty CODEX_HOME creates both files and reports
    /// installed with zero trust gaps.
    #[test]
    fn fresh_install_creates_files_and_trusts_all_entries() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        assert!(hooks_path_of(&codex_home).exists());
        assert!(config_path_of(&codex_home).exists());
        assert!(codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        let config = fs::read_to_string(config_path_of(&codex_home)).expect("read config");
        assert_eq!(
            config.matches(CODEX_STATE_MARKER).count(),
            CODEX_HOOK_FORWARD_MAP.len(),
            "one marked trust section per forwarded event",
        );
    }

    /// Positional-key recomputation: when the on-disk group order changes,
    /// reinstall rewrites our state key to the new index and drops the stale
    /// marked key pointing at the old index.
    #[test]
    fn reinstall_recomputes_positional_state_keys() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        let hooks_path = hooks_path_of(&codex_home);
        let config_path = config_path_of(&codex_home);

        fs::create_dir_all(&codex_home).expect("create codex home");
        let seeded = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"matcher": "user", "hooks": [{"type": "command", "command": "echo user"}]}
                ]
            }
        });
        fs::write(&hooks_path, serde_json::to_string(&seeded).expect("render"))
            .expect("seed hooks.json");

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let old_key = format!("{}:session_start:1:0", hooks_path.display());
        assert!(
            fs::read_to_string(&config_path)
                .expect("read")
                .contains(&old_key)
        );

        // The user deletes their group: our group is now index 0.
        let mut file: CodexHooksFile = load_json_settings(&hooks_path, "Codex").expect("load");
        let groups = file.hooks.get_mut("SessionStart").expect("SessionStart");
        groups.remove(0);
        write_json_settings(&hooks_path, &file, "Codex").expect("rewrite");

        assert_eq!(
            codex_hook_trust_gaps_at(&codex_home).expect("gaps"),
            1,
            "the shifted handler is untrusted until reinstall",
        );

        install_codex_hooks_at(&codex_home, BINARY, 30).expect("re-install");
        let config = fs::read_to_string(&config_path).expect("read");
        let new_key = format!("{}:session_start:0:0", hooks_path.display());
        assert!(
            config.contains(&new_key),
            "recomputed key missing:\n{config}"
        );
        assert!(
            !config.contains(&old_key),
            "stale positional key must be removed:\n{config}"
        );
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);
    }

    /// Trust-gap counting: missing config, tampered hash, and explicit
    /// disable are classified per the documented semantics.
    #[test]
    fn trust_gaps_count_missing_and_stale_entries() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        install_codex_hooks_at(&codex_home, BINARY, 30).expect("install");
        let config_path = config_path_of(&codex_home);

        // All trusted after install.
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Tamper one hash: exactly one gap.
        let config = fs::read_to_string(&config_path).expect("read");
        let stop_hash = codex_trusted_hash(
            "stop",
            None,
            &codex_hook_command(BINARY, "stop"),
            30,
            Some(LIBRA_CODEX_STATUS_MESSAGE),
        );
        let tampered = config.replace(&stop_hash, "sha256:0000");
        assert_ne!(tampered, config, "stop hash must be present to tamper");
        fs::write(&config_path, &tampered).expect("tamper");
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 1);

        // `enabled = false` with a matching hash is a deliberate disable,
        // not a trust gap.
        let disabled = config.replacen("enabled = true", "enabled = false", 1);
        fs::write(&config_path, &disabled).expect("disable");
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);

        // Missing config.toml: every managed handler is a gap.
        fs::remove_file(&config_path).expect("remove config");
        assert_eq!(
            codex_hook_trust_gaps_at(&codex_home).expect("gaps"),
            CODEX_HOOK_FORWARD_MAP.len(),
        );
        assert!(!codex_hooks_are_installed_at(&codex_home, BINARY).expect("status"));

        // No hooks.json at all: nothing to gap.
        fs::remove_file(hooks_path_of(&codex_home)).expect("remove hooks");
        assert_eq!(codex_hook_trust_gaps_at(&codex_home).expect("gaps"), 0);
    }

    /// An invalid config.toml is a hard, actionable error — never silently
    /// overwritten.
    #[test]
    fn install_rejects_invalid_config_toml() {
        let tmp = TempDir::new().expect("tmp dir");
        let codex_home = tmp.path().join(".codex");
        fs::create_dir_all(&codex_home).expect("create codex home");
        let config_path = config_path_of(&codex_home);
        fs::write(&config_path, "this is [not toml").expect("seed broken config");

        let err = install_codex_hooks_at(&codex_home, BINARY, 30).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("invalid Codex config TOML"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains(&config_path.display().to_string()),
            "error must name the file; got: {rendered}",
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read back"),
            "this is [not toml",
            "the broken file must be left untouched",
        );
    }

    /// The section editor only touches Libra's marked sections and exact
    /// keys; a user's own hooks.state section for the *same* hooks.json (from
    /// their own manual entry at a non-Libra position) survives.
    #[test]
    fn state_section_editor_leaves_foreign_sections_alone() {
        let hooks_json = "/home/u/.codex/hooks.json";
        let user_key = format!("{hooks_json}:stop:0:0");
        let content = format!(
            "[hooks.state.\"{user_key}\"]\nenabled = true\ntrusted_hash = \"sha256:user\"\n"
        );
        let desired = [CodexStateEntry {
            key: format!("{hooks_json}:stop:1:0"),
            trusted_hash: "sha256:ours".to_string(),
        }];
        let remove_exact: BTreeSet<String> =
            desired.iter().map(|entry| entry.key.clone()).collect();

        let rewritten = rewrite_codex_state_sections(&content, hooks_json, &remove_exact, &desired);
        assert!(
            rewritten.starts_with(&content),
            "unmarked user section (same hooks.json, different position) must survive:\n{rewritten}",
        );
        assert!(rewritten.contains("sha256:ours"));

        // Removing our marked section again restores the user bytes exactly.
        let cleaned = rewrite_codex_state_sections(&rewritten, hooks_json, &BTreeSet::new(), &[]);
        assert_eq!(cleaned, content);
    }

    /// Keys containing TOML-special characters (quotes, backslashes) escape
    /// and re-parse losslessly.
    #[test]
    fn state_key_escaping_round_trips() {
        let key = r#"C:\Users\dev\.codex\hooks.json:stop:0:0"#;
        let header = format!("[hooks.state.\"{}\"]", escape_toml_basic_string(key));
        assert_eq!(parse_state_section_key(&header).as_deref(), Some(key));

        let quoted = "with\"quote and\ttab";
        let header = format!("[hooks.state.\"{}\"]", escape_toml_basic_string(quoted));
        assert_eq!(parse_state_section_key(&header).as_deref(), Some(quoted));
    }

    /// Install rejects a zero timeout with an actionable message.
    #[test]
    fn install_rejects_zero_timeout() {
        let options = ProviderInstallOptions {
            binary_path: None,
            timeout_secs: Some(0),
        };
        let err = install_codex_hooks(&options).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid --timeout"),
            "got: {err:#}",
        );
    }
}
