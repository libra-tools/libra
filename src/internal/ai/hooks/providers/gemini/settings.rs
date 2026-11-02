//! Gemini CLI settings manipulation for installing and removing Libra hook entries.
//!
//! Boundary: operations preserve unrelated user settings and only touch the hook specs
//! owned by Libra. Provider hook tests cover idempotent install, partial existing
//! config, and cleanup behavior.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::super::{
    provider::ProviderInstallOptions,
    setup::{
        load_json_settings, resolve_hook_binary_path, resolve_project_root, write_json_settings,
    },
};

const GEMINI_SETTINGS_DIR: &str = ".gemini";
const GEMINI_SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, Copy)]
struct GeminiHookSpec {
    event_name: &'static str,
    matcher: Option<&'static str>,
    hook_name: &'static str,
    subcommand: &'static str,
}

const GEMINI_HOOK_SPECS: &[GeminiHookSpec] = &[
    GeminiHookSpec {
        event_name: "SessionStart",
        matcher: None,
        hook_name: "libra-session-start",
        subcommand: "session-start",
    },
    GeminiHookSpec {
        event_name: "BeforeAgent",
        matcher: None,
        hook_name: "libra-before-agent",
        subcommand: "prompt",
    },
    GeminiHookSpec {
        event_name: "AfterTool",
        matcher: Some("*"),
        hook_name: "libra-after-tool",
        subcommand: "tool-use",
    },
    GeminiHookSpec {
        event_name: "AfterAgent",
        matcher: None,
        hook_name: "libra-after-agent",
        subcommand: "stop",
    },
    GeminiHookSpec {
        event_name: "SessionEnd",
        matcher: None,
        hook_name: "libra-session-end",
        subcommand: "session-end",
    },
    GeminiHookSpec {
        event_name: "BeforeModel",
        matcher: None,
        hook_name: "libra-before-model",
        subcommand: "model-update",
    },
    GeminiHookSpec {
        event_name: "PreCompress",
        matcher: None,
        hook_name: "libra-pre-compress",
        subcommand: "compaction",
    },
];

#[derive(Debug, Serialize, Deserialize, Default)]
struct GeminiSettings {
    #[serde(rename = "hooksConfig", default)]
    hooks_config: GeminiHooksConfig,
    #[serde(default)]
    hooks: BTreeMap<String, Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct GeminiHooksConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct GeminiHookMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matcher: Option<String>,
    hooks: Vec<GeminiHookEntry>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
struct GeminiHookEntry {
    name: String,
    #[serde(rename = "type")]
    entry_type: String,
    command: String,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

pub(super) fn install_gemini_hooks(options: &ProviderInstallOptions) -> Result<()> {
    let binary_path = resolve_hook_binary_path(options.binary_path.as_deref())?;
    if options.timeout_secs.is_some() {
        bail!("Gemini hooks do not support --hook-timeout");
    }

    let settings_path = gemini_settings_path()?;
    let mut settings = load_gemini_settings(&settings_path)?;
    let changed = upsert_gemini_hooks(&mut settings, &binary_path)?;

    if changed {
        write_json_settings(&settings_path, &settings, "Gemini")?;
        println!(
            "Installed Gemini hook forwarding at {}",
            settings_path.display()
        );
    } else {
        println!(
            "Gemini hook forwarding is already up to date at {}",
            settings_path.display()
        );
    }

    Ok(())
}

pub(super) fn uninstall_gemini_hooks() -> Result<()> {
    let settings_path = gemini_settings_path()?;
    if !settings_path.exists() {
        println!(
            "Gemini hook settings not found at {}",
            settings_path.display()
        );
        return Ok(());
    }

    let mut settings = load_gemini_settings(&settings_path)?;
    let changed = remove_libra_gemini_hooks(&mut settings)?;
    if changed {
        write_json_settings(&settings_path, &settings, "Gemini")?;
        println!(
            "Removed Gemini hook forwarding at {}",
            settings_path.display()
        );
    } else {
        println!(
            "No Libra-managed Gemini hooks found at {}",
            settings_path.display()
        );
    }
    Ok(())
}

pub(super) fn gemini_hooks_are_installed() -> Result<bool> {
    let settings_path = gemini_settings_path()?;
    if !settings_path.exists() {
        return Ok(false);
    }
    let settings = load_gemini_settings(&settings_path)?;
    let binary_path = resolve_hook_binary_path(None)?;
    all_gemini_specs_installed(&settings, &binary_path)
}

fn gemini_settings_path() -> Result<PathBuf> {
    Ok(resolve_project_root()?
        .join(GEMINI_SETTINGS_DIR)
        .join(GEMINI_SETTINGS_FILE))
}

fn load_gemini_settings(path: &Path) -> Result<GeminiSettings> {
    load_json_settings(path, "Gemini")
}

fn upsert_gemini_hooks(settings: &mut GeminiSettings, binary_path: &str) -> Result<bool> {
    let mut changed = false;
    if !settings.hooks_config.enabled {
        settings.hooks_config.enabled = true;
        changed = true;
    }

    for spec in GEMINI_HOOK_SPECS {
        let value = settings
            .hooks
            .remove(spec.event_name)
            .unwrap_or(Value::Array(Vec::new()));
        let original_matchers = parse_gemini_hook_matchers(&value, spec.event_name)?;
        let expected_command = format!("{binary_path} hooks gemini {}", spec.subcommand);
        let desired_matcher = GeminiHookMatcher {
            matcher: spec.matcher.map(ToString::to_string),
            hooks: vec![GeminiHookEntry {
                name: spec.hook_name.to_string(),
                entry_type: "command".to_string(),
                command: expected_command.clone(),
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        let mut matchers = Vec::with_capacity(original_matchers.len() + 1);
        let mut has_desired_entry = false;

        for mut matcher in original_matchers {
            if matcher == desired_matcher {
                has_desired_entry = true;
                matchers.push(matcher);
                continue;
            }

            let original_hook_count = matcher.hooks.len();
            let matcher_name = matcher.matcher.as_deref();
            matcher.hooks.retain(|hook| {
                !is_replaced_managed_gemini_hook(matcher_name, hook, spec, &expected_command)
            });
            if matcher.hooks.len() != original_hook_count {
                changed = true;
            }
            if matcher.hooks.is_empty() {
                continue;
            }
            matchers.push(matcher);
        }

        if !has_desired_entry {
            matchers.push(desired_matcher);
            changed = true;
        }

        settings.hooks.insert(
            spec.event_name.to_string(),
            serde_json::to_value(matchers).context("failed to serialize Gemini hook matchers")?,
        );
    }

    Ok(changed)
}

fn remove_libra_gemini_hooks(settings: &mut GeminiSettings) -> Result<bool> {
    let keys: Vec<String> = settings.hooks.keys().cloned().collect();
    let mut changed = false;

    for key in keys {
        let Some(value) = settings.hooks.get(&key).cloned() else {
            continue;
        };
        let mut matchers = parse_gemini_hook_matchers(&value, &key)?;
        let original = matchers.clone();

        for matcher in &mut matchers {
            matcher
                .hooks
                .retain(|hook| !hook.name.starts_with("libra-"));
        }
        matchers.retain(|matcher| !matcher.hooks.is_empty());

        if matchers != original {
            changed = true;
            if matchers.is_empty() {
                settings.hooks.remove(&key);
            } else {
                settings.hooks.insert(
                    key.clone(),
                    serde_json::to_value(matchers)
                        .context("failed to serialize Gemini hook matchers")?,
                );
            }
        }
    }

    Ok(changed)
}

fn all_gemini_specs_installed(settings: &GeminiSettings, binary_path: &str) -> Result<bool> {
    if !settings.hooks_config.enabled {
        return Ok(false);
    }

    for spec in GEMINI_HOOK_SPECS {
        let value = settings
            .hooks
            .get(spec.event_name)
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let matchers = parse_gemini_hook_matchers(&value, spec.event_name)?;
        let expected_command = format!("{binary_path} hooks gemini {}", spec.subcommand);
        if !contains_gemini_command(&matchers, spec.matcher, spec.hook_name, &expected_command) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parse_gemini_hook_matchers(value: &Value, event_name: &str) -> Result<Vec<GeminiHookMatcher>> {
    match value {
        Value::Array(_) => serde_json::from_value(value.clone())
            .with_context(|| format!("invalid Gemini hooks format under event '{event_name}'")),
        Value::Null => Ok(Vec::new()),
        _ => bail!(
            "invalid Gemini hooks format under event '{}': expected array",
            event_name
        ),
    }
}

fn contains_gemini_command(
    matchers: &[GeminiHookMatcher],
    expected_matcher: Option<&str>,
    expected_name: &str,
    expected_command: &str,
) -> bool {
    matchers.iter().any(|matcher| {
        matcher.matcher.as_deref() == expected_matcher
            && matcher.hooks.iter().any(|hook| {
                hook.name == expected_name
                    && hook.entry_type == "command"
                    && hook.command == expected_command
            })
    })
}

fn is_replaced_managed_gemini_hook(
    matcher: Option<&str>,
    hook: &GeminiHookEntry,
    spec: &GeminiHookSpec,
    desired_command: &str,
) -> bool {
    if matcher == spec.matcher
        && hook.name == spec.hook_name
        && hook.entry_type == "command"
        && hook.command == desired_command
    {
        return false;
    }

    hook.name == spec.hook_name
        || hook
            .command
            .ends_with(&format!(" hooks gemini {}", spec.subcommand))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn upsert_gemini_hooks_is_idempotent() {
        let mut settings = GeminiSettings::default();
        let changed_first = upsert_gemini_hooks(&mut settings, "/tmp/libra").expect("upsert");
        let changed_second = upsert_gemini_hooks(&mut settings, "/tmp/libra").expect("upsert");

        assert!(changed_first);
        assert!(!changed_second);
        assert!(all_gemini_specs_installed(&settings, "/tmp/libra").expect("installed"));
    }

    #[test]
    fn remove_gemini_hooks_preserves_user_hooks() {
        let mut settings = GeminiSettings::default();
        settings.hooks.insert(
            "SessionStart".to_string(),
            json!([
                {
                    "matcher": "startup",
                    "hooks": [
                        {"name": "user-hook", "type": "command", "command": "echo keep"}
                    ]
                },
                {
                    "hooks": [
                        {"name": "libra-session-start", "type": "command", "command": "libra hooks gemini session-start"}
                    ]
                }
            ]),
        );

        let changed = remove_libra_gemini_hooks(&mut settings).expect("remove");
        assert!(changed);

        let value = settings.hooks.get("SessionStart").cloned().expect("remain");
        let matchers = parse_gemini_hook_matchers(&value, "SessionStart").expect("parse");
        assert_eq!(matchers.len(), 1);
        assert_eq!(matchers[0].hooks[0].name, "user-hook");
    }
}
