//! Claude settings manipulation for installing and removing Libra hook entries.
//!
//! Boundary: only Libra-owned hook matchers are inserted or removed; unrelated Claude
//! user configuration must be preserved. Tests cover idempotent upsert, partial config,
//! and cleanup of obsolete hook commands.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::super::{
    provider::{
        ProviderInstallOptions, capture_budget_millis_for_host_timeout,
        is_managed_capture_budget_millis,
    },
    setup::{
        load_json_settings, resolve_hook_binary_path, resolve_project_root, write_json_settings,
    },
};

/// Host handler timeout written for every Libra-owned Claude hook except
/// `SessionEnd` when `libra agent enable` gets no `--hook-timeout`. Thirty
/// seconds (capture budget 29000ms) matches the Codex default and leaves a
/// `Stop` checkpoint room to finish on a heavily loaded host; an unloaded
/// callback still returns in a few hundred milliseconds.
const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;
/// Basis for the `SessionEnd` capture budget, which carries no host timeout
/// (`ClaudeInstallTarget::set_host_timeout`). It stays at the historical
/// ten-second basis (9000ms) regardless of [`DEFAULT_HOOK_TIMEOUT_SECS`] or
/// `--hook-timeout`, so the shared SessionEnd window does not grow and an
/// existing timeout-less `SessionEnd` entry keeps its ownership proof.
const SESSION_END_BUDGET_BASIS_SECS: u64 = 10;
const CLAUDE_SETTINGS_DIR: &str = ".claude";
const CLAUDE_SETTINGS_FILE: &str = "settings.json";
/// Read and write budget for one Claude settings file. A larger file is refused
/// before any rewrite so a failure cannot truncate it.
const CLAUDE_SETTINGS_MAX_BYTES: u64 = 1_048_576;
/// Durable marker attached to current Libra-owned command handlers.
///
/// Claude supports `statusMessage` on command hooks.  Unlike an arbitrary
/// flattened field, this is part of the provider schema and lets cleanup
/// distinguish a renamed canonical Libra executable from a user command that
/// merely happens to use the public hook grammar.
const LIBRA_CLAUDE_STATUS_MESSAGE: &str = "libra capture";
// The original six events keep their own verbs; every other safely installable
// registry event (`events::safely_installable_names`) forwards to `observe`.
// `PreToolUse` and `PostToolUse` both forward to `tool-use` (the parser maps both
// to `LifecycleEventKind::ToolUse`), giving an earlier liveness signal at the
// start of a tool call; a `ToolUse` event refreshes `agent_session` liveness on
// the AgentTraces path and does not write a checkpoint. `SubagentStart` /
// `SubagentStop` are installed through `observe` and reach the capture runtime as
// subagent boundaries. Keep in sync with `docs/commands/hooks.md`.
/// Historical verbs for the original six events. Later safe events use `observe`.
const CLAUDE_LEGACY_FORWARD_MAP: &[(&str, &str)] = &[
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "tool-use"),
    ("PostToolUse", "tool-use"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

struct ClaudeInstallTarget {
    event: &'static str,
    subcommand: &'static str,
    /// SessionEnd omits the host timeout so it cannot raise the shared
    /// SessionEnd budget. Every other safe event keeps the normal timeout.
    set_host_timeout: bool,
}

fn claude_install_targets() -> Vec<ClaudeInstallTarget> {
    let mut targets = Vec::new();
    for (event, subcommand) in CLAUDE_LEGACY_FORWARD_MAP {
        targets.push(ClaudeInstallTarget {
            event,
            subcommand,
            set_host_timeout: *event != "SessionEnd",
        });
    }
    for event in super::events::safely_installable_names() {
        if targets.iter().any(|target| target.event == event) {
            continue;
        }
        targets.push(ClaudeInstallTarget {
            event,
            subcommand: "observe",
            set_host_timeout: true,
        });
    }
    targets
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ClaudeSettings {
    #[serde(default)]
    hooks: BTreeMap<String, Vec<ClaudeHookMatcher>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct ClaudeHookMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matcher: Option<String>,
    hooks: Vec<ClaudeHookEntry>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct ClaudeHookEntry {
    #[serde(rename = "type")]
    entry_type: String,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
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

pub(super) fn install_claude_hooks(options: &ProviderInstallOptions) -> Result<()> {
    let binary_path = resolve_hook_binary_path(options.binary_path.as_deref())?;
    let timeout = options.timeout_secs.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS);
    let settings_path = claude_settings_path()?;
    install_claude_hooks_at(&settings_path, &binary_path, timeout)
}

/// Install/refresh the Libra-owned entries at an explicit settings path.
///
/// Keeping the mutation behind this path-oriented boundary lets tests exercise
/// the same installer flow with precise provider timeout values without
/// changing the process working directory.
fn install_claude_hooks_at(settings_path: &Path, binary_path: &str, timeout: u64) -> Result<()> {
    reject_unsafe_claude_settings(settings_path)?;
    let capture_budget_millis = capture_budget_millis_for_host_timeout(timeout)?;
    let mut settings = load_claude_settings(settings_path)?;
    let changed = upsert_claude_hooks(&mut settings, binary_path, timeout, capture_budget_millis);

    if changed {
        write_json_settings(settings_path, &settings, "Claude")?;
        println!(
            "Installed Claude hook forwarding at {}",
            settings_path.display()
        );
    } else {
        println!(
            "Claude hook forwarding is already up to date at {}",
            settings_path.display()
        );
    }
    Ok(())
}

pub(super) fn uninstall_claude_hooks() -> Result<()> {
    let settings_path = claude_settings_path()?;
    match std::fs::symlink_metadata(&settings_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "Claude hook settings not found at {}",
                settings_path.display()
            );
            return Ok(());
        }
        _ => {}
    }
    reject_unsafe_claude_settings(&settings_path)?;

    let mut settings = load_claude_settings(&settings_path)?;
    let changed = remove_libra_claude_hooks(&mut settings);
    if changed {
        write_json_settings(&settings_path, &settings, "Claude")?;
        println!(
            "Removed Claude hook forwarding at {}",
            settings_path.display()
        );
    } else {
        println!(
            "No Libra-managed Claude hooks found at {}",
            settings_path.display()
        );
    }
    Ok(())
}

pub(super) fn claude_hooks_are_installed() -> Result<bool> {
    let settings_path = claude_settings_path()?;
    match std::fs::symlink_metadata(&settings_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        _ => {}
    }
    reject_unsafe_claude_settings(&settings_path)?;
    let settings = load_claude_settings(&settings_path)?;
    let binary_path = resolve_hook_binary_path(None)?;
    Ok(all_claude_specs_installed(&settings, &binary_path))
}

/// One registry row for `libra agent doctor`.
///
/// `class` is `installable` or `excluded`. `reason` is a static sentence and
/// does not include a hook command or payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeHookDoctorEvent {
    pub event: &'static str,
    pub class: &'static str,
    pub configured: bool,
    pub reason: &'static str,
}

pub(super) fn claude_hook_doctor_events() -> Result<Vec<ClaudeHookDoctorEvent>> {
    let settings_path = claude_settings_path()?;
    reject_unsafe_claude_settings(&settings_path)?;
    let settings = match std::fs::symlink_metadata(&settings_path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ClaudeSettings::default(),
        _ => load_claude_settings(&settings_path)?,
    };
    let binary_path = resolve_hook_binary_path(None)?;
    let targets = claude_install_targets();
    let mut rows = Vec::new();
    for event in super::events::known_event_names() {
        let Some(class) = super::events::event_class(event) else {
            continue;
        };
        let row = match class {
            super::events::ClaudeEventClass::SafelyInstallable => {
                let configured = targets.iter().any(|target| {
                    target.event == event && event_has_owned_hook(&settings, target, &binary_path)
                });
                ClaudeHookDoctorEvent {
                    event,
                    class: "installable",
                    configured,
                    reason: if configured {
                        "libra-owned observer"
                    } else {
                        "not configured"
                    },
                }
            }
            super::events::ClaudeEventClass::ControlSensitive => ClaudeHookDoctorEvent {
                event,
                class: "excluded",
                configured: false,
                reason: "control-sensitive; not installed",
            },
            super::events::ClaudeEventClass::DeprecatedAlias => ClaudeHookDoctorEvent {
                event,
                class: "excluded",
                configured: false,
                reason: "deprecated alias; not in the installable set",
            },
        };
        rows.push(row);
    }
    Ok(rows)
}

fn reject_unsafe_claude_settings(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                anyhow::bail!(
                    "Claude settings directory '{}' is not a regular directory; refusing to follow it",
                    parent.display()
                );
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "failed to inspect Claude settings directory '{}'",
                        parent.display()
                    )
                });
            }
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            anyhow::bail!(
                "Claude settings file '{}' is not a regular file; refusing to follow it",
                path.display()
            );
        }
        Ok(meta) if meta.len() > CLAUDE_SETTINGS_MAX_BYTES => {
            anyhow::bail!(
                "Claude settings file '{}' exceeds the 1 MiB read budget",
                path.display()
            );
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to inspect Claude settings file '{}'",
                path.display()
            )
        }),
    }
}

fn claude_settings_path() -> Result<PathBuf> {
    Ok(resolve_project_root()?
        .join(CLAUDE_SETTINGS_DIR)
        .join(CLAUDE_SETTINGS_FILE))
}

fn load_claude_settings(path: &Path) -> Result<ClaudeSettings> {
    load_json_settings(path, "Claude")
}

fn upsert_claude_hooks(
    settings: &mut ClaudeSettings,
    binary_path: &str,
    timeout: u64,
    capture_budget_millis: u64,
) -> bool {
    let mut changed = false;

    let session_end_budget = capture_budget_millis_for_host_timeout(SESSION_END_BUDGET_BASIS_SECS)
        .unwrap_or(capture_budget_millis);
    for target in claude_install_targets() {
        let event_name = target.event;
        let subcommand = target.subcommand;
        let budget = if target.set_host_timeout {
            capture_budget_millis
        } else {
            session_end_budget
        };
        let desired_entry = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: claude_hook_command(binary_path, subcommand, budget),
            timeout: target.set_host_timeout.then_some(timeout),
            status_message: Some(LIBRA_CLAUDE_STATUS_MESSAGE.to_string()),
            extra: BTreeMap::new(),
        };

        let original_matchers = settings.hooks.remove(event_name).unwrap_or_default();
        let mut rebuilt_matchers = Vec::with_capacity(original_matchers.len() + 1);
        let mut has_desired_entry = false;

        for mut matcher in original_matchers {
            if matcher.matcher.is_none() && matcher.hooks == vec![desired_entry.clone()] {
                has_desired_entry = true;
                rebuilt_matchers.push(matcher);
                continue;
            }

            let original_hook_count = matcher.hooks.len();
            // Libra only writes matcher-less groups. A user may deliberately
            // scope the same public command to a Claude matcher; that group
            // is not ours to rewrite or remove.
            if matcher.matcher.is_none() {
                matcher
                    .hooks
                    .retain(|hook| !is_replaced_managed_claude_hook(hook, subcommand, binary_path));
            }
            if matcher.hooks.len() != original_hook_count {
                changed = true;
            }
            if matcher.hooks.is_empty() {
                continue;
            }
            rebuilt_matchers.push(matcher);
        }

        if !has_desired_entry {
            rebuilt_matchers.push(ClaudeHookMatcher {
                matcher: None,
                hooks: vec![desired_entry],
                extra: BTreeMap::new(),
            });
            changed = true;
        }

        settings
            .hooks
            .insert(event_name.to_string(), rebuilt_matchers);
    }

    changed
}

fn remove_libra_claude_hooks(settings: &mut ClaudeSettings) -> bool {
    let keys: Vec<String> = settings.hooks.keys().cloned().collect();
    let mut changed = false;

    for key in keys {
        let Some(target) = claude_install_targets()
            .into_iter()
            .find(|target| target.event == key)
        else {
            // Unknown event names were never installed by Libra. Preserve
            // their groups verbatim even if a user command resembles ours.
            continue;
        };
        let Some(mut matchers) = settings.hooks.remove(&key) else {
            continue;
        };
        let original = matchers.clone();

        for matcher in &mut matchers {
            if matcher.matcher.is_none() {
                matcher
                    .hooks
                    .retain(|hook| !is_managed_claude_hook_for_subcommand(hook, target.subcommand));
            }
        }
        matchers.retain(|matcher| !matcher.hooks.is_empty());

        if matchers != original {
            changed = true;
        }
        if !matchers.is_empty() {
            settings.hooks.insert(key, matchers);
        }
    }

    changed
}

fn all_claude_specs_installed(settings: &ClaudeSettings, binary_path: &str) -> bool {
    claude_install_targets()
        .iter()
        .all(|target| event_has_owned_hook(settings, target, binary_path))
}

fn event_has_owned_hook(
    settings: &ClaudeSettings,
    target: &ClaudeInstallTarget,
    binary_path: &str,
) -> bool {
    settings.hooks.get(target.event).is_some_and(|matchers| {
        matchers.iter().any(|matcher| {
            matcher.matcher.is_none()
                && matcher
                    .hooks
                    .iter()
                    .any(|hook| hook_matches_install_target(hook, target, binary_path))
        })
    })
}

fn hook_matches_install_target(
    hook: &ClaudeHookEntry,
    target: &ClaudeInstallTarget,
    binary_path: &str,
) -> bool {
    if hook.entry_type != "command"
        || hook.status_message.as_deref() != Some(LIBRA_CLAUDE_STATUS_MESSAGE)
    {
        return false;
    }
    let budget = if target.set_host_timeout {
        let Some(timeout) = hook.timeout else {
            return false;
        };
        match capture_budget_millis_for_host_timeout(timeout) {
            Ok(budget) => budget,
            Err(_) => return false,
        }
    } else if hook.timeout.is_some() {
        return false;
    } else {
        match capture_budget_millis_for_host_timeout(SESSION_END_BUDGET_BASIS_SECS) {
            Ok(budget) => budget,
            Err(_) => return false,
        }
    };
    hook.command == claude_hook_command(binary_path, target.subcommand, budget)
}

fn hook_matches_session_end_without_timeout(hook: &ClaudeHookEntry) -> bool {
    if hook.timeout.is_some() || hook.status_message.as_deref() != Some(LIBRA_CLAUDE_STATUS_MESSAGE)
    {
        return false;
    }
    let Ok(budget) = capture_budget_millis_for_host_timeout(SESSION_END_BUDGET_BASIS_SECS) else {
        return false;
    };
    matches!(
        parse_managed_claude_command_for_subcommand(&hook.command, "session-end"),
        Some(ManagedClaudeCommand::Canonical {
            capture_budget_millis,
            ..
        }) if capture_budget_millis == budget
    )
}

fn claude_hook_command(binary_path: &str, subcommand: &str, capture_budget_millis: u64) -> String {
    format!("{binary_path} hooks claude {subcommand} --capture-budget-ms {capture_budget_millis}")
}

/// The persisted command is not by itself enough to claim ownership: users
/// can deliberately invoke the same public command with their own timeout.
/// Durable ownership requires the provider marker plus the exact effective
/// timeout/budget pair. An unmarked form is eligible only for explicit enable
/// migration to the selected binary; disable never removes it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ManagedClaudeCommand {
    Legacy {
        executable: String,
    },
    Canonical {
        executable: String,
        capture_budget_millis: u64,
    },
}

/// A previous current-form install could predate `statusMessage`. During an
/// explicit install only, migrate an *exact* unmarked direct command for the
/// selected binary to the marked form. Never use this exception from
/// uninstall: without a persisted marker there is no safe ownership proof.
fn is_replaced_managed_claude_hook(
    hook: &ClaudeHookEntry,
    subcommand: &str,
    binary_path: &str,
) -> bool {
    is_managed_claude_hook_for_subcommand(hook, subcommand)
        || is_current_unmarked_claude_hook(hook, subcommand, binary_path)
}

fn is_managed_claude_hook_for_subcommand(hook: &ClaudeHookEntry, subcommand: &str) -> bool {
    if hook.entry_type != "command" {
        return false;
    }
    if subcommand == "session-end" && hook_matches_session_end_without_timeout(hook) {
        return true;
    }
    match parse_managed_claude_command_for_subcommand(&hook.command, subcommand) {
        // `statusMessage` was introduced with the budget-bearing canonical
        // shape. A marker-less legacy command has no durable ownership proof.
        Some(ManagedClaudeCommand::Legacy { .. }) => false,
        Some(ManagedClaudeCommand::Canonical {
            executable: _,
            capture_budget_millis,
        }) => {
            let Some(timeout) = hook.timeout else {
                return false;
            };
            hook.status_message.as_deref() == Some(LIBRA_CLAUDE_STATUS_MESSAGE)
                && capture_budget_millis_for_host_timeout(timeout)
                    .is_ok_and(|expected| expected == capture_budget_millis)
        }
        None => false,
    }
}

fn is_current_unmarked_claude_hook(
    hook: &ClaudeHookEntry,
    subcommand: &str,
    binary_path: &str,
) -> bool {
    if hook.entry_type != "command" || hook.status_message.is_some() {
        return false;
    }
    match parse_managed_claude_command_for_subcommand(&hook.command, subcommand) {
        Some(ManagedClaudeCommand::Legacy { executable }) => executable == binary_path,
        Some(ManagedClaudeCommand::Canonical {
            executable,
            capture_budget_millis,
        }) => {
            // A timeout-less unmarked entry is the historical `SessionEnd`
            // form, whose budget always used the ten-second basis.
            executable == binary_path
                && capture_budget_millis_for_host_timeout(
                    hook.timeout.unwrap_or(SESSION_END_BUDGET_BASIS_SECS),
                )
                .is_ok_and(|expected| expected == capture_budget_millis)
        }
        None => false,
    }
}

fn parse_managed_claude_command_for_subcommand(
    command: &str,
    subcommand: &str,
) -> Option<ManagedClaudeCommand> {
    let suffix = format!(" hooks claude {subcommand}");
    let (executable, tail) = command.rsplit_once(&suffix)?;
    match parse_managed_capture_budget_tail(tail)? {
        ManagedCaptureBudgetTail::Legacy => is_direct_absolute_command_executable(executable)
            .map(|executable| ManagedClaudeCommand::Legacy { executable }),
        ManagedCaptureBudgetTail::Canonical {
            capture_budget_millis,
        } => is_direct_absolute_command_executable(executable).map(|executable| {
            ManagedClaudeCommand::Canonical {
                executable,
                capture_budget_millis,
            }
        }),
    }
}

/// Parse one direct shell command token without invoking a shell.  It accepts
/// only the quoting emitted by `setup::quote_command_path` (including the
/// Unix `'<apostrophe>'` escape), then requires an absolute, lexical-normal
/// path.  This prevents a marked handler such as `sh -c ...` from becoming
/// owned merely because its tail resembles Libra's CLI grammar.
fn is_direct_absolute_command_executable(executable: &str) -> Option<String> {
    let decoded = unquote_direct_command_path(executable)?;
    let path = Path::new(&decoded);
    (path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        }))
    .then_some(executable.to_string())
}

fn unquote_direct_command_path(executable: &str) -> Option<String> {
    if executable.is_empty() || executable.trim() != executable {
        return None;
    }
    match executable.chars().next()? {
        '\'' => unquote_single_quoted_command_path(executable),
        '"' => unquote_double_quoted_command_path(executable),
        _ if executable
            .chars()
            .all(is_conservative_command_path_character) =>
        {
            Some(executable.to_string())
        }
        _ => None,
    }
}

fn is_conservative_command_path_character(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '/' | '\\' | '.' | '_' | '-' | ':')
}

fn unquote_single_quoted_command_path(executable: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = executable;
        None
    }

    #[cfg(not(windows))]
    {
        let mut rest = executable.strip_prefix('\'')?;
        let mut decoded = String::new();
        loop {
            let (segment, after_quote) = rest.split_once('\'')?;
            decoded.push_str(segment);
            if after_quote.is_empty() {
                return Some(decoded);
            }
            // `quote_command_path` emits `\\''` between adjacent quoted
            // segments for a literal apostrophe.
            let after_escape = after_quote.strip_prefix(r#"\''"#)?;
            decoded.push('\'');
            rest = after_escape;
        }
    }
}

fn unquote_double_quoted_command_path(executable: &str) -> Option<String> {
    #[cfg(not(windows))]
    {
        let _ = executable;
        None
    }

    #[cfg(windows)]
    {
        let inner = executable.strip_prefix('"')?.strip_suffix('"')?;
        let mut decoded = String::new();
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '"' {
                return None;
            }
            if ch == '\\' && chars.clone().next() == Some('"') {
                chars.next();
                decoded.push('"');
            } else {
                decoded.push(ch);
            }
        }
        Some(decoded)
    }
}

enum ManagedCaptureBudgetTail {
    Legacy,
    Canonical { capture_budget_millis: u64 },
}

fn parse_managed_capture_budget_tail(tail: &str) -> Option<ManagedCaptureBudgetTail> {
    if tail.is_empty() {
        return Some(ManagedCaptureBudgetTail::Legacy);
    }
    let value = tail.strip_prefix(" --capture-budget-ms ")?;
    (!value.is_empty() && !value.contains(char::is_whitespace))
        .then(|| value.parse::<u64>().ok())
        .flatten()
        .filter(|budget| is_managed_capture_budget_millis(*budget))
        .map(
            |capture_budget_millis| ManagedCaptureBudgetTail::Canonical {
                capture_budget_millis,
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_install_targets_cover_safe_events_and_hold_control_sensitive() {
        let targets = claude_install_targets();
        assert_eq!(
            targets.len(),
            super::super::events::safely_installable_names().len()
        );
        for (event, subcommand) in CLAUDE_LEGACY_FORWARD_MAP {
            assert!(
                targets.iter().any(|target| {
                    target.event == *event
                        && target.subcommand == *subcommand
                        && target.set_host_timeout == (*event != "SessionEnd")
                }),
                "{event} must keep its historical verb"
            );
        }
        assert!(targets.iter().any(|target| {
            target.event == "FileChanged"
                && target.subcommand == "observe"
                && target.set_host_timeout
        }));
        for held in ["WorktreeCreate", "WorktreeRemove", "PreModelSwitch"] {
            assert!(
                targets.iter().all(|target| target.event != held),
                "{held} must stay out of the install set"
            );
        }
    }

    #[test]
    fn upsert_claude_hooks_is_idempotent() {
        let mut settings = ClaudeSettings::default();
        assert!(upsert_claude_hooks(&mut settings, "/tmp/libra", 10, 9_000));
        assert!(!upsert_claude_hooks(&mut settings, "/tmp/libra", 10, 9_000));
        assert!(all_claude_specs_installed(&settings, "/tmp/libra"));
    }

    #[test]
    fn renamed_binary_install_is_idempotent_and_uninstallable() {
        let binary = "'/opt/Libra Install/capture-hook-custom-name'";
        let mut settings = ClaudeSettings::default();

        assert!(upsert_claude_hooks(&mut settings, binary, 10, 9_000));
        assert!(!upsert_claude_hooks(&mut settings, binary, 10, 9_000));
        assert!(all_claude_specs_installed(&settings, binary));
        assert!(settings.hooks.values().all(|matchers| {
            matchers.iter().any(|matcher| {
                matcher.matcher.is_none()
                    && matcher.hooks.iter().any(|hook| {
                        hook.status_message.as_deref() == Some(LIBRA_CLAUDE_STATUS_MESSAGE)
                    })
            })
        }));

        assert!(remove_libra_claude_hooks(&mut settings));
        assert!(settings.hooks.is_empty());
    }

    #[test]
    fn custom_command_path_parser_accepts_only_installer_quoting() {
        let escaped_apostrophe = r#"'/opt/Libra'\''s/capture-hook'"#;
        assert_eq!(
            unquote_direct_command_path(escaped_apostrophe).as_deref(),
            Some("/opt/Libra's/capture-hook")
        );
        assert!(is_direct_absolute_command_executable(escaped_apostrophe).is_some());
        assert!(is_direct_absolute_command_executable("/opt/$(not-a-binary)").is_none());
        assert!(is_direct_absolute_command_executable("sh -c /opt/libra").is_none());

        #[cfg(not(windows))]
        assert!(is_direct_absolute_command_executable("\"/opt/$(not-a-binary)\"").is_none());
    }

    #[test]
    fn install_migrates_only_the_exact_unmarked_renamed_binary() {
        let binary = "/opt/capture-hook-custom-name";
        let old = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: claude_hook_command(binary, "session-start", 9_000),
            timeout: Some(10),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![old],
                extra: BTreeMap::new(),
            }],
        );

        assert!(upsert_claude_hooks(&mut settings, binary, 10, 9_000));
        assert!(!upsert_claude_hooks(&mut settings, binary, 10, 9_000));
        let session_start = settings.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(session_start.len(), 1);
        assert_eq!(session_start[0].hooks.len(), 1);
        assert_eq!(
            session_start[0].hooks[0].status_message.as_deref(),
            Some(LIBRA_CLAUDE_STATUS_MESSAGE)
        );
        assert!(remove_libra_claude_hooks(&mut settings));
        assert!(settings.hooks.is_empty());
    }

    #[test]
    fn unmarked_standard_canonical_hook_migrates_and_uninstalls() {
        let old = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: claude_hook_command("/opt/libra", "session-start", 9_000),
            timeout: Some(10),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![old],
                extra: BTreeMap::new(),
            }],
        );

        assert!(upsert_claude_hooks(&mut settings, "/opt/libra", 10, 9_000));
        assert!(!upsert_claude_hooks(&mut settings, "/opt/libra", 10, 9_000));
        assert_eq!(
            settings.hooks["SessionStart"][0].hooks[0]
                .status_message
                .as_deref(),
            Some(LIBRA_CLAUDE_STATUS_MESSAGE)
        );
        assert!(remove_libra_claude_hooks(&mut settings));
        assert!(settings.hooks.is_empty());
    }

    #[test]
    fn explicit_enable_adopts_only_exact_current_unmarked_claude_handlers() {
        let current_binary = "/current/libra";
        let hook = |command: &str, timeout: Option<u64>| ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: command.to_string(),
            timeout,
            status_message: None,
            extra: BTreeMap::new(),
        };
        let bare_legacy = hook("libra hooks claude session-start", Some(10));
        let bare_canonical = hook(
            "libra hooks claude session-start --capture-budget-ms 9000",
            Some(10),
        );
        let other_legacy = hook("/other/libra hooks claude session-start", Some(10));
        let other_canonical = hook(
            "/other/libra hooks claude session-start --capture-budget-ms 9000",
            Some(10),
        );
        let exact_legacy = hook("/current/libra hooks claude session-start", Some(10));
        let exact_canonical = hook(
            "/current/libra hooks claude stop --capture-budget-ms 9000",
            Some(10),
        );
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![
                    bare_legacy.clone(),
                    bare_canonical.clone(),
                    other_legacy.clone(),
                    other_canonical.clone(),
                    exact_legacy,
                ],
                extra: BTreeMap::new(),
            }],
        );
        settings.hooks.insert(
            "Stop".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![exact_canonical],
                extra: BTreeMap::new(),
            }],
        );

        assert!(
            !remove_libra_claude_hooks(&mut settings),
            "disable must not claim any unmarked handler, including exact-current history"
        );
        assert!(upsert_claude_hooks(
            &mut settings,
            current_binary,
            DEFAULT_HOOK_TIMEOUT_SECS,
            29_000
        ));
        assert!(all_claude_specs_installed(&settings, current_binary));

        let session_start = settings
            .hooks
            .get("SessionStart")
            .expect("SessionStart groups")
            .iter()
            .flat_map(|matcher| matcher.hooks.iter())
            .collect::<Vec<_>>();
        for user_hook in [
            &bare_legacy,
            &bare_canonical,
            &other_legacy,
            &other_canonical,
        ] {
            assert!(
                session_start.contains(&user_hook),
                "enable must preserve unmarked user hook: {user_hook:?}"
            );
        }
        assert!(session_start.iter().any(|entry| {
            entry.status_message.as_deref() == Some(LIBRA_CLAUDE_STATUS_MESSAGE)
                && entry.command == claude_hook_command(current_binary, "session-start", 29_000)
                && entry.timeout == Some(DEFAULT_HOOK_TIMEOUT_SECS)
        }));
        assert!(
            !session_start
                .iter()
                .any(|entry| entry.command == "/current/libra hooks claude session-start")
        );
        let stop = settings
            .hooks
            .get("Stop")
            .expect("Stop groups")
            .iter()
            .flat_map(|matcher| matcher.hooks.iter())
            .collect::<Vec<_>>();
        assert_eq!(stop.len(), 1);
        assert_eq!(
            stop[0].command,
            claude_hook_command(current_binary, "stop", 29_000)
        );
        assert_eq!(
            stop[0].status_message.as_deref(),
            Some(LIBRA_CLAUDE_STATUS_MESSAGE)
        );

        assert!(remove_libra_claude_hooks(&mut settings));
        let after_disable = settings
            .hooks
            .get("SessionStart")
            .expect("user SessionStart hooks remain")
            .iter()
            .flat_map(|matcher| matcher.hooks.iter())
            .collect::<Vec<_>>();
        assert_eq!(after_disable.len(), 4);
        for user_hook in [
            &bare_legacy,
            &bare_canonical,
            &other_legacy,
            &other_canonical,
        ] {
            assert!(
                after_disable.contains(&user_hook),
                "disable must preserve unmarked user hook: {user_hook:?}"
            );
        }
        assert!(!settings.hooks.contains_key("Stop"));
    }

    /// Provider timeouts are the outer contract: the installer must retain a
    /// usable capture slice for the historical one-second configuration and
    /// must not silently clamp a valid value above Codex's 600-second default.
    #[test]
    fn installer_emits_exact_short_and_long_capture_budgets() {
        for (timeout_secs, capture_budget_millis) in [(1, 500), (601, 600_000)] {
            let tmp = tempfile::tempdir().expect("create temporary Claude settings directory");
            let settings_path = tmp.path().join(".claude/settings.json");

            install_claude_hooks_at(&settings_path, "/opt/libra", timeout_secs)
                .expect("install Claude hooks");

            let settings = load_claude_settings(&settings_path).expect("read installed settings");
            for target in claude_install_targets() {
                let (expected_timeout, expected_budget) = if target.set_host_timeout {
                    (Some(timeout_secs), capture_budget_millis)
                } else {
                    (None, 9_000)
                };
                let expected =
                    claude_hook_command("/opt/libra", target.subcommand, expected_budget);
                let installed = settings.hooks.get(target.event).is_some_and(|matchers| {
                    matchers.iter().any(|matcher| {
                        matcher.matcher.is_none()
                            && matcher.hooks.iter().any(|hook| {
                                hook.entry_type == "command"
                                    && hook.command == expected
                                    && hook.timeout == expected_timeout
                                    && hook.status_message.as_deref()
                                        == Some(LIBRA_CLAUDE_STATUS_MESSAGE)
                            })
                    })
                });
                assert!(
                    installed,
                    "{} must retain timeout {expected_timeout:?} and emit '{expected}'",
                    target.event
                );
            }
        }
    }

    /// Libra-owned entries for one event, regardless of matcher group.
    fn owned_entries<'a>(settings: &'a ClaudeSettings, event: &str) -> Vec<&'a ClaudeHookEntry> {
        settings
            .hooks
            .get(event)
            .into_iter()
            .flatten()
            .flat_map(|matcher| matcher.hooks.iter())
            .filter(|hook| hook.status_message.as_deref() == Some(LIBRA_CLAUDE_STATUS_MESSAGE))
            .collect()
    }

    /// The default host timeout is 30s (29000ms budget) for every event that
    /// carries one, while `SessionEnd` stays timeout-less on the historical
    /// 9000ms budget.
    #[test]
    fn default_install_uses_thirty_second_timeout_and_keeps_session_end_budget() {
        assert_eq!(DEFAULT_HOOK_TIMEOUT_SECS, 30);
        let tmp = tempfile::tempdir().expect("create temporary Claude settings directory");
        let settings_path = tmp.path().join(".claude/settings.json");
        install_claude_hooks_at(&settings_path, "/opt/libra", DEFAULT_HOOK_TIMEOUT_SECS)
            .expect("install Claude hooks");

        let settings = load_claude_settings(&settings_path).expect("read installed settings");
        for target in claude_install_targets() {
            let owned = owned_entries(&settings, target.event);
            assert_eq!(owned.len(), 1, "{}: one Libra entry", target.event);
            let (timeout, budget) = if target.set_host_timeout {
                (Some(30), 29_000)
            } else {
                (None, 9_000)
            };
            assert_eq!(owned[0].timeout, timeout, "{} timeout", target.event);
            assert_eq!(
                owned[0].command,
                claude_hook_command("/opt/libra", target.subcommand, budget),
                "{} command",
                target.event
            );
        }
        assert_eq!(
            owned_entries(&settings, "Stop")[0].command,
            "/opt/libra hooks claude stop --capture-budget-ms 29000"
        );
        assert_eq!(
            owned_entries(&settings, "SessionEnd")[0].command,
            "/opt/libra hooks claude session-end --capture-budget-ms 9000"
        );
    }

    /// Hooks written by the former ten-second default (or any other valid
    /// timeout) stay Libra-owned: re-enabling replaces each one in place
    /// rather than appending a duplicate, and disable still removes them.
    #[test]
    fn reenable_replaces_previous_timeout_install_without_duplicates() {
        let tmp = tempfile::tempdir().expect("create temporary Claude settings directory");
        let settings_path = tmp.path().join(".claude/settings.json");
        install_claude_hooks_at(&settings_path, "/opt/libra", 10).expect("install 10s hooks");
        let previous = load_claude_settings(&settings_path).expect("read 10s settings");
        assert!(all_claude_specs_installed(&previous, "/opt/libra"));
        assert_eq!(
            owned_entries(&previous, "Stop")[0].command,
            "/opt/libra hooks claude stop --capture-budget-ms 9000"
        );

        for (timeout, budget) in [(DEFAULT_HOOK_TIMEOUT_SECS, 29_000), (45, 44_000)] {
            install_claude_hooks_at(&settings_path, "/opt/libra", timeout)
                .expect("re-install Claude hooks");
            let settings = load_claude_settings(&settings_path).expect("read settings");
            assert!(all_claude_specs_installed(&settings, "/opt/libra"));
            for target in claude_install_targets() {
                let owned = owned_entries(&settings, target.event);
                assert_eq!(
                    owned.len(),
                    1,
                    "{}: no duplicate after re-enable",
                    target.event
                );
                if target.set_host_timeout {
                    assert_eq!(owned[0].timeout, Some(timeout));
                    assert_eq!(
                        owned[0].command,
                        claude_hook_command("/opt/libra", target.subcommand, budget)
                    );
                } else {
                    assert_eq!(owned[0].timeout, None);
                    assert_eq!(
                        owned[0].command,
                        claude_hook_command("/opt/libra", target.subcommand, 9_000)
                    );
                }
            }
        }

        let mut settings = load_claude_settings(&settings_path).expect("read settings");
        assert!(remove_libra_claude_hooks(&mut settings));
        assert!(
            claude_install_targets()
                .iter()
                .all(|target| owned_entries(&settings, target.event).is_empty()),
            "disable must remove every custom-timeout entry"
        );
    }

    /// Builds one `matcher: null` Libra command matcher for `event → verb`.
    fn libra_matcher(verb: &str) -> ClaudeHookMatcher {
        ClaudeHookMatcher {
            matcher: None,
            hooks: vec![ClaudeHookEntry {
                entry_type: "command".to_string(),
                command: format!("/tmp/libra hooks claude {verb}"),
                timeout: Some(10),
                status_message: None,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        }
    }

    // DR-00 upgrade path: a config installed by a pre-DR-00 binary (the five
    // legacy events, no PreToolUse) must gain the Libra PreToolUse forward on
    // the next upsert, stay idempotent on a rerun, preserve a user-owned
    // PreToolUse hook, and drop only Libra-managed hooks on uninstall.
    #[test]
    fn upsert_upgrades_legacy_five_event_config_and_preserves_user_hooks() {
        let mut settings = ClaudeSettings::default();
        for (event, verb) in [
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "prompt"),
            ("PostToolUse", "tool-use"),
            ("Stop", "stop"),
            ("SessionEnd", "session-end"),
        ] {
            settings
                .hooks
                .insert(event.to_string(), vec![libra_matcher(verb)]);
        }
        // A user-owned PreToolUse hook that Libra must never clobber.
        settings.hooks.insert(
            "PreToolUse".to_string(),
            vec![ClaudeHookMatcher {
                matcher: Some("Bash".to_string()),
                hooks: vec![ClaudeHookEntry {
                    entry_type: "command".to_string(),
                    command: "echo user-pre".to_string(),
                    timeout: Some(5),
                    status_message: None,
                    extra: BTreeMap::new(),
                }],
                extra: BTreeMap::new(),
            }],
        );

        // Upgrade: the missing Libra PreToolUse forward is added (change == true).
        assert!(upsert_claude_hooks(&mut settings, "/tmp/libra", 10, 9_000));
        assert!(all_claude_specs_installed(&settings, "/tmp/libra"));
        let pre = settings.hooks.get("PreToolUse").expect("PreToolUse");
        assert!(
            pre.iter().any(|m| m.matcher.as_deref() == Some("Bash")
                && m.hooks.iter().any(|h| h.command == "echo user-pre")),
            "user PreToolUse hook must survive upgrade: {settings:?}"
        );
        assert!(
            pre.iter().any(|m| m.matcher.is_none()
                && m.hooks
                    .iter()
                    .any(|h| h.command
                        == "/tmp/libra hooks claude tool-use --capture-budget-ms 9000")),
            "Libra PreToolUse forward must be installed: {settings:?}"
        );

        // Idempotent: a second upsert reports no change.
        assert!(!upsert_claude_hooks(&mut settings, "/tmp/libra", 10, 9_000));

        // Uninstall drops only Libra-managed hooks; the user hook survives.
        assert!(remove_libra_claude_hooks(&mut settings));
        let pre = settings
            .hooks
            .get("PreToolUse")
            .expect("PreToolUse remains");
        assert!(
            pre.iter()
                .any(|m| m.hooks.iter().any(|h| h.command == "echo user-pre")),
            "user PreToolUse hook must survive uninstall: {settings:?}"
        );
        assert!(
            !pre.iter().any(|m| m
                .hooks
                .iter()
                .any(|h| h.command.contains("hooks claude tool-use"))),
            "Libra PreToolUse forward must be removed on uninstall: {settings:?}"
        );
    }

    #[test]
    fn remove_claude_hooks_preserves_non_libra_entries() {
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![
                ClaudeHookMatcher {
                    matcher: None,
                    hooks: vec![ClaudeHookEntry {
                        entry_type: "command".to_string(),
                        command: "libra hooks claude session-start".to_string(),
                        timeout: Some(10),
                        status_message: None,
                        extra: BTreeMap::new(),
                    }],
                    extra: BTreeMap::new(),
                },
                ClaudeHookMatcher {
                    matcher: Some("startup".to_string()),
                    hooks: vec![ClaudeHookEntry {
                        entry_type: "command".to_string(),
                        command: "echo keep".to_string(),
                        timeout: Some(3),
                        status_message: None,
                        extra: BTreeMap::new(),
                    }],
                    extra: BTreeMap::new(),
                },
            ],
        );

        assert!(!remove_libra_claude_hooks(&mut settings));
        let session_start = settings.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(session_start.len(), 2);
        assert!(session_start.iter().any(|matcher| {
            matcher
                .hooks
                .iter()
                .any(|hook| hook.command == "libra hooks claude session-start")
        }));
        assert!(
            session_start
                .iter()
                .any(|matcher| { matcher.hooks.iter().any(|hook| hook.command == "echo keep") })
        );
    }

    #[test]
    fn remove_claude_hooks_keeps_non_libra_wrapper_commands() {
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![ClaudeHookEntry {
                    entry_type: "command".to_string(),
                    command: "/tmp/custom-wrapper hooks claude session-start".to_string(),
                    timeout: Some(10),
                    status_message: None,
                    extra: BTreeMap::new(),
                }],
                extra: BTreeMap::new(),
            }],
        );

        assert!(!remove_libra_claude_hooks(&mut settings));
        let session_start = settings.hooks.get("SessionStart").expect("SessionStart");
        assert_eq!(
            session_start[0].hooks[0].command,
            "/tmp/custom-wrapper hooks claude session-start"
        );
    }

    #[test]
    fn claude_ownership_requires_a_command_handler_and_matching_budget() {
        let canonical = "/opt/libra hooks claude session-start --capture-budget-ms 9000";
        assert!(is_managed_claude_hook_for_subcommand(
            &ClaudeHookEntry {
                entry_type: "command".to_string(),
                command: canonical.to_string(),
                timeout: Some(10),
                status_message: Some(LIBRA_CLAUDE_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_claude_hook_for_subcommand(
            &ClaudeHookEntry {
                entry_type: "command".to_string(),
                command: canonical.to_string(),
                timeout: Some(30),
                status_message: Some(LIBRA_CLAUDE_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_claude_hook_for_subcommand(
            &ClaudeHookEntry {
                entry_type: "prompt".to_string(),
                command: canonical.to_string(),
                timeout: Some(10),
                status_message: Some(LIBRA_CLAUDE_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_claude_hook_for_subcommand(
            &ClaudeHookEntry {
                entry_type: "command".to_string(),
                command: "/old/libra hooks claude session-start".to_string(),
                timeout: None,
                status_message: None,
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(is_managed_claude_hook_for_subcommand(
            &ClaudeHookEntry {
                entry_type: "command".to_string(),
                command: "/opt/capture-hook-custom-name hooks claude session-start --capture-budget-ms 9000"
                    .to_string(),
                timeout: Some(10),
                status_message: Some(LIBRA_CLAUDE_STATUS_MESSAGE.to_string()),
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
        assert!(!is_managed_claude_hook_for_subcommand(
            &ClaudeHookEntry {
                entry_type: "command".to_string(),
                command: "/opt/capture-hook-custom-name hooks claude session-start --capture-budget-ms 9000"
                    .to_string(),
                timeout: Some(10),
                status_message: None,
                extra: BTreeMap::new(),
            },
            "session-start"
        ));
    }

    #[test]
    fn install_and_uninstall_preserve_mismatched_user_claude_hooks() {
        let user_mismatched_budget = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: "/opt/libra hooks claude session-start --capture-budget-ms 500".to_string(),
            timeout: Some(30),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let user_non_command_handler = ClaudeHookEntry {
            entry_type: "prompt".to_string(),
            command: "/opt/libra hooks claude session-start --capture-budget-ms 9000".to_string(),
            timeout: Some(10),
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![
                    user_mismatched_budget.clone(),
                    user_non_command_handler.clone(),
                ],
                extra: BTreeMap::new(),
            }],
        );

        assert!(upsert_claude_hooks(&mut settings, "/opt/libra", 10, 9_000));
        let commands_after_install = settings
            .hooks
            .get("SessionStart")
            .expect("SessionStart after install")
            .iter()
            .flat_map(|matcher| matcher.hooks.iter())
            .collect::<Vec<_>>();
        assert!(commands_after_install.contains(&&user_mismatched_budget));
        assert!(commands_after_install.contains(&&user_non_command_handler));
        assert!(commands_after_install.iter().any(|hook| {
            hook.entry_type == "command"
                && hook.command == "/opt/libra hooks claude session-start --capture-budget-ms 9000"
                && hook.timeout == Some(10)
        }));

        assert!(remove_libra_claude_hooks(&mut settings));
        let commands_after_uninstall = settings
            .hooks
            .get("SessionStart")
            .expect("SessionStart after uninstall")
            .iter()
            .flat_map(|matcher| matcher.hooks.iter())
            .collect::<Vec<_>>();
        assert!(commands_after_uninstall.contains(&&user_mismatched_budget));
        assert!(commands_after_uninstall.contains(&&user_non_command_handler));
        assert!(!commands_after_uninstall.iter().any(|hook| {
            hook.command == "/opt/libra hooks claude session-start --capture-budget-ms 9000"
                && hook.entry_type == "command"
                && hook.timeout == Some(10)
        }));
    }

    #[test]
    fn install_and_uninstall_preserve_scoped_wrong_and_unknown_event_commands() {
        let canonical = "/opt/libra hooks claude session-start --capture-budget-ms 9000";
        let scoped_user_hook = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: canonical.to_string(),
            timeout: Some(10),
            status_message: Some(LIBRA_CLAUDE_STATUS_MESSAGE.to_string()),
            extra: BTreeMap::new(),
        };
        let wrong_event_user_hook = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: "/old/libra hooks claude session-start".to_string(),
            timeout: None,
            status_message: None,
            extra: BTreeMap::new(),
        };
        let unknown_event_user_hook = wrong_event_user_hook.clone();
        let legacy_session_end = ClaudeHookEntry {
            entry_type: "command".to_string(),
            command: "/old/libra hooks claude session-end".to_string(),
            timeout: None,
            status_message: None,
            extra: BTreeMap::new(),
        };
        let mut settings = ClaudeSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            vec![ClaudeHookMatcher {
                matcher: Some("Bash".to_string()),
                hooks: vec![scoped_user_hook.clone()],
                extra: BTreeMap::new(),
            }],
        );
        settings.hooks.insert(
            "Stop".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![wrong_event_user_hook.clone()],
                extra: BTreeMap::new(),
            }],
        );
        settings.hooks.insert(
            "UserCustomEvent".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![unknown_event_user_hook.clone()],
                extra: BTreeMap::new(),
            }],
        );
        settings.hooks.insert(
            "SessionEnd".to_string(),
            vec![ClaudeHookMatcher {
                matcher: None,
                hooks: vec![legacy_session_end],
                extra: BTreeMap::new(),
            }],
        );

        assert!(upsert_claude_hooks(&mut settings, "/opt/libra", 10, 9_000));
        for (event, expected) in [
            ("SessionStart", &scoped_user_hook),
            ("Stop", &wrong_event_user_hook),
            ("UserCustomEvent", &unknown_event_user_hook),
        ] {
            assert!(
                settings
                    .hooks
                    .get(event)
                    .into_iter()
                    .flatten()
                    .flat_map(|matcher| &matcher.hooks)
                    .any(|hook| hook == expected),
                "install must preserve the user-owned {event} hook: {settings:?}"
            );
        }

        assert!(remove_libra_claude_hooks(&mut settings));
        for (event, expected) in [
            ("SessionStart", &scoped_user_hook),
            ("Stop", &wrong_event_user_hook),
            ("UserCustomEvent", &unknown_event_user_hook),
        ] {
            assert!(
                settings
                    .hooks
                    .get(event)
                    .into_iter()
                    .flatten()
                    .flat_map(|matcher| &matcher.hooks)
                    .any(|hook| hook == expected),
                "uninstall must preserve the user-owned {event} hook: {settings:?}"
            );
        }
        assert!(
            settings
                .hooks
                .get("SessionEnd")
                .into_iter()
                .flatten()
                .flat_map(|matcher| &matcher.hooks)
                .any(|hook| hook.command == "/old/libra hooks claude session-end"),
            "unmarked legacy command must remain user-owned after uninstall: {settings:?}"
        );
    }
}
