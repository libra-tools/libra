//! Shared installer helpers for provider-specific hook setup.
//!
//! Each provider (Claude, Gemini, …) has its own settings file format, but they
//! all share three concerns: locating the project root, deciding which Libra
//! binary path to embed in hook commands, and atomically updating a JSON
//! settings file. This module centralises those primitives so the per-provider
//! installers can stay short and audit-friendly.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Serialize, de::DeserializeOwned};

use crate::utils::util;

/// Locate the working directory of the active Libra repository.
///
/// Boundary conditions: returns an error message that asks the user to run the
/// command from inside a repository. Callers must not silently fall back to the
/// current directory.
pub(super) fn resolve_project_root() -> Result<PathBuf> {
    util::try_working_dir()
        .context("hook installation commands must be run inside a Libra repository")
}

/// Pick the path to the Libra binary to embed in the provider's hook command and
/// return it as a shell-quoted string ready for inclusion in a config file.
///
/// Functional scope:
/// - When the user passes `--binary-path`, that path is honoured (relative paths
///   are resolved against the current directory).
/// - Otherwise the path of the currently running binary is used.
/// - In both cases the path is canonicalised so the resulting hook command does
///   not depend on the user's CWD when later executed by the provider.
///
/// Boundary conditions:
/// - Empty `--binary-path` is rejected with an actionable message.
/// - A canonicalisation failure (e.g. the binary was deleted between resolution
///   and `fs::canonicalize`) bubbles up with the path in the error message.
pub(super) fn resolve_hook_binary_path(input: Option<&str>) -> Result<String> {
    let path = match input {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("invalid --binary-path: value cannot be empty");
            }
            let path = PathBuf::from(trimmed);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .context("failed to read current directory while resolving --binary-path")?
                    .join(path)
            }
        }
        None => std::env::current_exe()
            .context("failed to resolve current Libra binary path for hook installation")?,
    };

    let canonical = fs::canonicalize(&path)
        .with_context(|| format!("failed to resolve Libra binary path '{}'", path.display()))?;

    Ok(quote_command_path(&canonical))
}

/// Quote a path for safe inclusion in a shell command stored on disk.
///
/// Functional scope: on every platform, an unquoted path uses only the
/// conservative alphabet `[A-Za-z0-9/\\._\-:]`. Other Windows paths are
/// double-quoted and other Unix paths are single-quoted with embedded
/// apostrophes escaped using the standard `'\''` form. Keeping the emitted
/// grammar small lets provider cleanup recognize only direct command paths,
/// never shell expressions.
fn quote_command_path(path: &Path) -> String {
    let rendered = path.to_string_lossy();

    #[cfg(windows)]
    {
        if !rendered.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '/' | '\\' | '.' | '_' | '-' | ':')
        }) {
            return format!("\"{}\"", rendered.replace('"', "\\\""));
        }
        rendered.into_owned()
    }

    #[cfg(not(windows))]
    {
        if rendered
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
        {
            return rendered.into_owned();
        }
        format!("'{}'", rendered.replace('\'', r#"'\''"#))
    }
}

/// Read a provider's settings JSON file, returning `T::default()` when it is
/// missing or empty.
///
/// Functional scope: callers can roundtrip-update settings without having to
/// branch on whether the file already exists. Whitespace-only files are also
/// treated as empty so an editor that saved an unfinished file does not derail
/// the installer.
///
/// Boundary conditions: a syntactically broken JSON file is reported as an error
/// referring to the file path, never as a silent default.
pub(super) fn load_json_settings<T>(path: &Path, provider_name: &str) -> Result<T>
where
    T: Default + DeserializeOwned,
{
    if !path.exists() {
        return Ok(T::default());
    }

    let content = fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read {provider_name} settings file '{}'",
            path.display()
        )
    })?;
    if content.trim().is_empty() {
        return Ok(T::default());
    }

    serde_json::from_str(&content).map_err(|err| {
        anyhow!(
            "invalid {provider_name} settings JSON at '{}': {err}",
            path.display()
        )
    })
}

/// Atomically write `settings` to `path` using a temp-file + rename dance.
///
/// Functional scope:
/// - Creates parent directories as needed.
/// - Serialises with `to_vec_pretty` and appends a trailing newline so the file
///   is friendly to text editors.
/// - Writes a sibling `<path>.tmp`, then renames it on top of the target so the
///   file is never observed in a partial state.
///
/// Boundary conditions:
/// - On Windows `fs::rename` cannot overwrite an existing file, so the target is
///   removed first; the temp file is cleaned up if removal fails.
/// - All error messages include both the target and temp paths so operators can
///   recover from a partial write manually.
pub(super) fn write_json_settings<T>(path: &Path, settings: &T, provider_name: &str) -> Result<()>
where
    T: Serialize,
{
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "invalid {provider_name} settings path without parent: '{}'",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create {provider_name} settings directory '{}'",
            parent.display()
        )
    })?;

    let mut data = serde_json::to_vec_pretty(settings)
        .with_context(|| format!("failed to serialize {provider_name} settings to JSON"))?;
    data.push(b'\n');

    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &data).with_context(|| {
        format!(
            "failed to write temporary {provider_name} settings file '{}'",
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
                        "failed to replace existing {provider_name} settings file '{}': {err}",
                        path.display()
                    ));
                }
            }
        }
    }

    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace {provider_name} settings file '{}' with '{}'",
            path.display(),
            tmp_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use tempfile::TempDir;

    use super::*;

    /// Empty `--binary-path` triggers an actionable error. Pin so a
    /// future "trim and accept" refactor doesn't silently accept
    /// `--binary-path=` and resolve to the current directory.
    #[test]
    fn resolve_hook_binary_path_rejects_empty_input() {
        for raw in ["", "   ", "\t\n"] {
            let err = resolve_hook_binary_path(Some(raw)).unwrap_err();
            let rendered = format!("{err:#}");
            assert!(
                rendered.contains("invalid --binary-path"),
                "raw {raw:?} produced: {rendered}",
            );
            assert!(
                rendered.contains("cannot be empty"),
                "must mention empty-value rule; got: {rendered}",
            );
        }
    }

    /// Non-existent absolute path surfaces a canonicalisation error
    /// referencing the offending path so operators can fix the
    /// config.
    #[test]
    fn resolve_hook_binary_path_reports_canonicalisation_failure() {
        // Pick a path that almost certainly doesn't exist.
        let bogus = "/definitely-not-a-real-binary-9f7a3c5e";
        let err = resolve_hook_binary_path(Some(bogus)).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains(bogus),
            "error must mention the offending path; got: {rendered}",
        );
        assert!(
            rendered.contains("failed to resolve"),
            "must mention resolution failure; got: {rendered}",
        );
    }

    /// `load_json_settings` returns `T::default()` for a non-existent
    /// path — callers can blindly load + mutate + write without
    /// branching on file existence.
    #[test]
    fn load_json_settings_returns_default_for_missing_path() {
        #[derive(Default, Debug, PartialEq, Deserialize)]
        struct Settings {
            #[serde(default)]
            field: String,
        }
        let tmp = TempDir::new().expect("tmp dir");
        let missing = tmp.path().join("never-created.json");
        let loaded: Settings = load_json_settings(&missing, "test").expect("load ok");
        assert_eq!(loaded, Settings::default());
    }

    /// `load_json_settings` returns `T::default()` for a
    /// whitespace-only file. Pin the documented "editor-saved
    /// unfinished file doesn't derail the installer" rule.
    #[test]
    fn load_json_settings_returns_default_for_whitespace_only_file() {
        #[derive(Default, Debug, PartialEq, Deserialize)]
        struct Settings {
            #[serde(default)]
            field: String,
        }
        let tmp = TempDir::new().expect("tmp dir");
        let path = tmp.path().join("empty.json");
        for content in ["", "   ", "\n\n\t"] {
            fs::write(&path, content).expect("write");
            let loaded: Settings = load_json_settings(&path, "test").expect("load ok");
            assert_eq!(loaded, Settings::default(), "content {content:?}");
        }
    }

    /// Malformed JSON surfaces an error referencing the file path
    /// and provider name — operators must see which file is broken.
    /// Pin so a future "silent default fallback" regression breaks.
    #[test]
    fn load_json_settings_reports_invalid_json_with_path_and_provider() {
        #[derive(Default, Debug, Deserialize)]
        struct Settings {
            #[serde(default)]
            _field: String,
        }
        let tmp = TempDir::new().expect("tmp dir");
        let path = tmp.path().join("broken.json");
        fs::write(&path, "{ this is not json }").expect("write");

        let err = load_json_settings::<Settings>(&path, "claude").unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("claude"),
            "error must include provider name; got: {rendered}",
        );
        assert!(
            rendered.contains(&path.display().to_string()),
            "error must include path; got: {rendered}",
        );
    }

    /// `load_json_settings` successfully parses a valid JSON file.
    #[test]
    fn load_json_settings_parses_valid_json() {
        #[derive(Default, Debug, PartialEq, Deserialize)]
        struct Settings {
            #[serde(default)]
            field: String,
        }
        let tmp = TempDir::new().expect("tmp dir");
        let path = tmp.path().join("ok.json");
        fs::write(&path, r#"{"field":"hello"}"#).expect("write");
        let loaded: Settings = load_json_settings(&path, "test").expect("load ok");
        assert_eq!(loaded.field, "hello");
    }

    /// `write_json_settings` creates parent directories, writes
    /// pretty-printed JSON with a trailing newline, and leaves the
    /// content readable by `load_json_settings`.
    #[test]
    fn write_json_settings_atomically_writes_with_trailing_newline() {
        #[derive(Serialize, Default, Debug, PartialEq, Deserialize)]
        struct Settings {
            field: String,
        }
        let tmp = TempDir::new().expect("tmp dir");
        let path = tmp.path().join("nested/dir/output.json");
        let settings = Settings {
            field: "value".to_string(),
        };
        write_json_settings(&path, &settings, "test").expect("write ok");

        // File exists, with a trailing newline.
        let content = fs::read_to_string(&path).expect("read");
        assert!(
            content.ends_with('\n'),
            "must end with newline; got: {content:?}",
        );
        // Round-trips through load_json_settings.
        let loaded: Settings = load_json_settings(&path, "test").expect("load ok");
        assert_eq!(loaded, settings);
        // Temp file was cleaned up.
        let tmp_path = path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "temp file must be renamed away; survived at {}",
            tmp_path.display(),
        );
    }

    /// `write_json_settings` overwrites an existing file atomically:
    /// reading the file after the write yields the new content
    /// (never a partial-write intermediate state).
    #[test]
    fn write_json_settings_overwrites_existing_file() {
        #[derive(Serialize, Default, Debug, PartialEq, Deserialize)]
        struct Settings {
            field: String,
        }
        let tmp = TempDir::new().expect("tmp dir");
        let path = tmp.path().join("existing.json");
        fs::write(&path, r#"{"field":"old"}"#).expect("seed");

        let new_settings = Settings {
            field: "new".to_string(),
        };
        write_json_settings(&path, &new_settings, "test").expect("write ok");

        let loaded: Settings = load_json_settings(&path, "test").expect("load ok");
        assert_eq!(loaded.field, "new");
    }
}
