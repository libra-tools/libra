//! Libra-owned Hermes capture plugin.
//!
//! Install, update, and remove rewrite a file only when it contains the
//! `libra capture` marker and its bytes match the recorded sha256. An
//! unmanaged file and a drifted file stay as they are.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use semver::Version;
use sha2::{Digest, Sha256};

use super::super::super::provider::ProviderInstallOptions;

pub const LIBRA_CAPTURE_MARKER: &str = "libra capture";
pub const MIN_HERMES_VERSION: Version = Version::new(0, 22, 5);
const PLUGIN_ID: &str = "libra-hermes-capture";
const HASH_NAME: &str = "libra-capture.sha256";
const CONFIG_HASH_NAME: &str = "libra-capture-config.sha256";
const PLUGIN_FILES: [&str; 2] = ["plugin.yaml", "__init__.py"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HermesEnableGate {
    Install,
    SkipBare,
    RefuseExplicit,
}

/// `bare` is `libra agent enable` with no agent name. A missing or
/// unmatched Hermes binary skips that roster entry. An explicit request
/// refuses before any file is written.
pub fn hermes_enable_gate(bare: bool, runtime_ready: bool) -> HermesEnableGate {
    if runtime_ready {
        HermesEnableGate::Install
    } else if bare {
        HermesEnableGate::SkipBare
    } else {
        HermesEnableGate::RefuseExplicit
    }
}

pub fn hermes_version_text_is_supported(text: &str) -> bool {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Hermes Agent v"))
        .and_then(|banner| banner.split_whitespace().next())
        .and_then(|token| Version::parse(token).ok())
        .is_some_and(|version| version.cmp_precedence(&MIN_HERMES_VERSION).is_ge())
}

/// Check the minimum Hermes version on `PATH`. `--version` is answered before
/// Hermes reads config, so this does not open a profile database. The
/// probe clears `HERMES_HOME` because a new profile has no dependency
/// environment and would make the version command fail closed.
pub fn hermes_runtime_is_supported() -> bool {
    let output = Command::new("hermes")
        .arg("--version")
        .env_remove("HERMES_HOME")
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    hermes_version_text_is_supported(&format!("{text}\n{err}"))
}

pub fn install_hermes_hooks(options: &ProviderInstallOptions) -> Result<()> {
    let home = hermes_home_dir()
        .context("Hermes home is not an absolute directory. Set HERMES_HOME to an absolute path")?;
    let binary = options
        .binary_path
        .clone()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| "libra".to_string());
    install_owned_hermes_capture(&home, &binary)
}

pub fn uninstall_hermes_hooks() -> Result<()> {
    let Some(home) = hermes_home_dir() else {
        return Ok(());
    };
    remove_owned_hermes_capture(&home)
}

pub fn hermes_hooks_are_installed() -> Result<bool> {
    let Some(home) = hermes_home_dir() else {
        return Ok(false);
    };
    hermes_capture_is_installed(&home)
}

pub fn install_owned_hermes_capture(home: &Path, binary: &str) -> Result<()> {
    if binary.trim().is_empty() {
        bail!("hermes capture needs the path of the libra binary");
    }
    let plugin_dir = home.join("plugins").join(PLUGIN_ID);
    let files = canonical_plugin_files(binary)?;
    ensure_plugin_slot(&plugin_dir, &files)?;
    fs::create_dir_all(&plugin_dir).with_context(|| {
        format!(
            "failed to create the Hermes capture plugin directory {}",
            plugin_dir.display()
        )
    })?;
    for (name, bytes) in &files {
        atomic_write(&plugin_dir.join(name), bytes)?;
    }
    atomic_write(&plugin_dir.join(HASH_NAME), &hash_record(&files))?;
    install_owned_config(home)?;
    Ok(())
}

pub fn remove_owned_hermes_capture(home: &Path) -> Result<()> {
    let plugin_dir = home.join("plugins").join(PLUGIN_ID);
    if plugin_dir.is_dir() {
        let files = read_plugin_files(&plugin_dir)?;
        if files_are_owned(&plugin_dir, &files)? {
            for name in PLUGIN_FILES {
                let path = plugin_dir.join(name);
                if path.is_file() {
                    fs::remove_file(&path).with_context(|| {
                        format!(
                            "failed to remove the owned Hermes capture file {}",
                            path.display()
                        )
                    })?;
                }
            }
            let hash_path = plugin_dir.join(HASH_NAME);
            if hash_path.is_file() {
                fs::remove_file(&hash_path)
                    .with_context(|| format!("failed to remove {}", hash_path.display()))?;
            }
            let _ = fs::remove_dir(&plugin_dir);
        }
    }
    remove_owned_config(home)?;
    Ok(())
}

pub fn hermes_capture_is_installed(home: &Path) -> Result<bool> {
    let plugin_dir = home.join("plugins").join(PLUGIN_ID);
    if !plugin_dir.is_dir() {
        return Ok(false);
    }
    let files = read_plugin_files(&plugin_dir)?;
    files_are_owned(&plugin_dir, &files)
}

fn hermes_home_dir() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("HERMES_HOME") {
        let path = PathBuf::from(value);
        return path.is_absolute().then_some(path);
    }
    dirs::home_dir().map(|home| home.join(".hermes"))
}

fn canonical_plugin_files(binary: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let binary_literal =
        serde_json::to_string(binary).context("the libra binary path is not valid Unicode")?;
    let init = PLUGIN_INIT.replace("__LIBRA_BINARY__", &binary_literal);
    Ok(vec![
        ("plugin.yaml".to_string(), PLUGIN_YAML.as_bytes().to_vec()),
        ("__init__.py".to_string(), init.into_bytes()),
    ])
}

fn ensure_plugin_slot(plugin_dir: &Path, next: &[(String, Vec<u8>)]) -> Result<()> {
    if plugin_dir.symlink_metadata().is_ok() && !plugin_dir.is_dir() {
        bail!(
            "hermes capture path {} is not a directory and was left unchanged",
            plugin_dir.display()
        );
    }
    if !plugin_dir.is_dir() {
        return Ok(());
    }
    for (name, _) in next {
        let path = plugin_dir.join(name);
        if !path.exists() {
            continue;
        }
        if path
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
            || !path.is_file()
        {
            bail!(
                "hermes capture file {} is not a regular file and was left unchanged",
                path.display()
            );
        }
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        if !bytes_have_marker(&bytes) {
            bail!(
                "hermes capture file {} is not a libra-owned asset and was left unchanged",
                path.display()
            );
        }
    }
    let current = read_plugin_files(plugin_dir)?;
    if !current.is_empty() && !files_are_owned(plugin_dir, &current)? {
        bail!(
            "hermes capture files in {} have drifted and were left unchanged",
            plugin_dir.display()
        );
    }
    Ok(())
}

fn read_plugin_files(plugin_dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for name in PLUGIN_FILES {
        let path = plugin_dir.join(name);
        if path.is_file() {
            let bytes =
                fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
            files.push((name.to_string(), bytes));
        }
    }
    Ok(files)
}

fn files_are_owned(plugin_dir: &Path, files: &[(String, Vec<u8>)]) -> Result<bool> {
    if files.len() != PLUGIN_FILES.len() {
        return Ok(false);
    }
    if files.iter().any(|(_, bytes)| !bytes_have_marker(bytes)) {
        return Ok(false);
    }
    let recorded = fs::read_to_string(plugin_dir.join(HASH_NAME)).unwrap_or_default();
    Ok(recorded == String::from_utf8_lossy(&hash_record(files)))
}

fn install_owned_config(home: &Path) -> Result<()> {
    let path = home.join("config.yaml");
    let next = owned_config_bytes();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        atomic_write(&path, &next)?;
        atomic_write(&home.join(CONFIG_HASH_NAME), &hash_line(&next))?;
        return Ok(());
    }
    if path
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
        || !path.is_file()
    {
        bail!(
            "hermes config {} is not a regular file and was left unchanged",
            path.display()
        );
    }
    let current = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if !bytes_have_marker(&current) {
        return Ok(());
    }
    let recorded = fs::read(home.join(CONFIG_HASH_NAME)).unwrap_or_default();
    if recorded != hash_line(&current) {
        // The profile config grew past the owned bytes. Leave it in place.
        return Ok(());
    }
    atomic_write(&path, &next)?;
    atomic_write(&home.join(CONFIG_HASH_NAME), &hash_line(&next))?;
    Ok(())
}

fn remove_owned_config(home: &Path) -> Result<()> {
    let path = home.join("config.yaml");
    if !path.is_file() {
        return Ok(());
    }
    let current = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if !bytes_have_marker(&current) {
        return Ok(());
    }
    let recorded = fs::read(home.join(CONFIG_HASH_NAME)).unwrap_or_default();
    if recorded != hash_line(&current) {
        return Ok(());
    }
    fs::remove_file(&path).with_context(|| {
        format!(
            "failed to remove the owned Hermes config {}",
            path.display()
        )
    })?;
    let hash_path = home.join(CONFIG_HASH_NAME);
    if hash_path.is_file() {
        fs::remove_file(&hash_path)
            .with_context(|| format!("failed to remove {}", hash_path.display()))?;
    }
    Ok(())
}

fn owned_config_bytes() -> Vec<u8> {
    format!("# {LIBRA_CAPTURE_MARKER}\nplugins:\n  enabled:\n    - {PLUGIN_ID}\n").into_bytes()
}

fn bytes_have_marker(bytes: &[u8]) -> bool {
    let needle = LIBRA_CAPTURE_MARKER.as_bytes();
    bytes.windows(needle.len()).any(|window| window == needle)
}

fn hash_record(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, bytes) in files {
        out.extend(format!("{name} {}\n", hex_hash(bytes)).into_bytes());
    }
    out
}

fn hash_line(bytes: &[u8]) -> Vec<u8> {
    format!("{}\n", hex_hash(bytes)).into_bytes()
}

fn hex_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create a temporary file next to {}",
            path.display()
        )
    })?;
    tmp.write_all(bytes).with_context(|| {
        format!(
            "failed to write the Libra-owned Hermes capture file {}",
            path.display()
        )
    })?;
    tmp.as_file().sync_all().with_context(|| {
        format!(
            "failed to flush the Libra-owned Hermes capture file {}",
            path.display()
        )
    })?;
    tmp.persist(path).map_err(|error| {
        anyhow::anyhow!(
            "failed to replace {} with the Libra-owned Hermes capture file: {error}",
            path.display()
        )
    })?;
    Ok(())
}

const PLUGIN_YAML: &str = "\
name: libra-hermes-capture
version: \"1\"
description: \"libra capture observer for the proven Hermes events\"
provides_hooks:
  - post_llm_call
  - on_session_end
  - on_session_finalize
  - on_session_reset
  - agent_loop_stopped
";

const PLUGIN_INIT: &str = r#"# libra capture
"""Libra-owned Hermes observer. It does not open state.db."""

from __future__ import annotations

import json
import os
import subprocess
import threading
from pathlib import Path
from typing import Optional

LIBRA_BINARY = __LIBRA_BINARY__
_SLOTS = threading.BoundedSemaphore(8)
_OBSERVERS = (
    "post_llm_call",
    "on_session_end",
    "on_session_finalize",
    "on_session_reset",
    "agent_loop_stopped",
)
_PHASE = {
    "post_llm_call": "turn",
    "on_session_end": "turn",
    "on_session_finalize": "session",
    "on_session_reset": "reset",
    "agent_loop_stopped": "interrupt",
}


def register(ctx) -> None:
    for event in _OBSERVERS:
        ctx.register_hook(event, _callback(event))


def _callback(event: str):
    def run(**kwargs):
        if not _SLOTS.acquire(blocking=False):
            return None
        try:
            _forward(event, kwargs)
        finally:
            _SLOTS.release()
        return None

    return run


def _observer_env() -> dict:
    env = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", ""),
    }
    home = os.environ.get("HERMES_HOME", "")
    if (
        home
        and "\x00" not in home
        and "\n" not in home
        and "\r" not in home
        and (home.startswith("/") or (len(home) >= 3 and home[1] == ":" and home[2] in "\\/"))
    ):
        env["HERMES_HOME"] = home
    return env


def _forward(event: str, kwargs: dict) -> None:
    if event not in _OBSERVERS or event == "on_session_reset":
        return
    frame = _frame(event, kwargs)
    if frame is None:
        return
    try:
        subprocess.run(
            [LIBRA_BINARY, "agent", "hooks", "hermes", "observe"],
            input=json.dumps(frame).encode("utf-8"),
            cwd=frame["cwd"],
            env=_observer_env(),
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return


def _frame(event: str, kwargs: dict) -> Optional[dict]:
    if "platform" not in kwargs:
        return None
    source = _text(kwargs, "platform")
    if source.lower() not in {"cli", "local"}:
        return None
    created = kwargs.get("created_source")
    user = kwargs.get("user_id")
    if source.lower() == "local":
        if not isinstance(created, str) or created.lower() != "cli":
            return None
        if not isinstance(user, str) or user == "":
            return None
    root = _launch_worktree()
    if root is None:
        return None
    turn_id = _text(kwargs, "turn_id")
    message_id = _text(kwargs, "message_id") or turn_id
    phase = _PHASE[event]
    if turn_id == "" or message_id == "" or phase == "":
        return None
    session_id = _text(kwargs, "session_id")
    if session_id == "":
        return None
    frame = {
        "hook_event_name": event,
        "session_id": session_id,
        "cwd": str(root),
        "source": source,
        "profile_name": "default",
        "turn_id": turn_id,
        "message_id": message_id,
        "phase": phase,
        "outcome": _outcome(event, kwargs),
        "event_id": "|".join((session_id, turn_id, message_id, phase, event)),
    }
    if isinstance(created, str) and created != "":
        frame["created_source"] = created
    if isinstance(user, str) and user != "":
        frame["user_id"] = user
    return frame


def _outcome(event: str, kwargs: dict) -> str:
    if event == "post_llm_call":
        return "completed"
    if kwargs.get("interrupted") is True:
        return "interrupted"
    if kwargs.get("failed") is True:
        return "failed"
    if kwargs.get("completed") is False:
        return "failed"
    return "completed"


def _text(kwargs: dict, key: str) -> str:
    value = kwargs.get(key)
    if isinstance(value, str):
        return value
    if isinstance(value, int) and not isinstance(value, bool):
        return str(value)
    return ""


def _launch_worktree() -> Optional[Path]:
    # The gateway process cwd is the Hermes install. The launching shell's
    # PWD is still the worktree. Only an exact root match is admitted.
    candidates = []
    for key in ("PWD",):
        value = os.environ.get(key)
        if isinstance(value, str) and value:
            candidates.append(value)
    candidates.append(os.getcwd())
    for start in candidates:
        root = _worktree_root(start)
        try:
            if root is not None and Path(start).resolve() == root:
                return root
        except OSError:
            continue
    return None


def _worktree_root(start: str) -> Optional[Path]:
    path = Path(start).resolve()
    for candidate in [path, *path.parents]:
        if (candidate / ".libra").is_dir():
            return candidate
    return None
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_enable_gate_skips_bare_and_refuses_explicit() {
        assert_eq!(hermes_enable_gate(true, true), HermesEnableGate::Install);
        assert_eq!(hermes_enable_gate(true, false), HermesEnableGate::SkipBare);
        assert_eq!(
            hermes_enable_gate(false, false),
            HermesEnableGate::RefuseExplicit
        );
    }

    #[test]
    fn hermes_version_text_checks_the_minimum_release() {
        for banner in [
            "Hermes Agent v0.22.5",
            "Hermes Agent v0.22.5+local.1",
            "Hermes Agent v0.22.50",
            "Hermes Agent v0.23.2 (2026.9.24) · stable · local ae2a81ccc00e",
            "Hermes Agent v1.0.0-rc.1",
        ] {
            assert!(hermes_version_text_is_supported(banner), "{banner}");
        }
        for banner in [
            "Hermes Agent v0.22.4 (2026.9.24) bf0203a",
            "Hermes Agent v0.22.5-rc.1",
            "Hermes Agent v0.22",
            "Hermes Agent v0.22.5.invalid",
            "Hermes Agent unknown\nPython: v3.14.9",
            "Other Agent v1.0.0",
        ] {
            assert!(!hermes_version_text_is_supported(banner), "{banner}");
        }
    }

    #[test]
    fn hermes_plugin_forwards_real_fields_and_does_not_forge_cli() {
        let home = tempfile::tempdir().expect("home");
        fs::create_dir(home.path().join(".libra")).unwrap();
        let script = home.path().join("plugin.py");
        let source = PLUGIN_INIT.replace("__LIBRA_BINARY__", "\"/usr/bin/libra\"");
        fs::write(&script, source).unwrap();
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import importlib.util, json, os, sys\n\
                 os.environ['PWD']=sys.argv[1]\n\
                 os.chdir(sys.argv[1])\n\
                 spec=importlib.util.spec_from_file_location('plugin', sys.argv[2])\n\
                 mod=importlib.util.module_from_spec(spec)\n\
                 spec.loader.exec_module(mod)\n\
                 print(json.dumps({\n\
                   'missing': mod._frame('post_llm_call', {'session_id':'s','turn_id':'t'}),\n\
                   'local': mod._frame('post_llm_call', {'platform':'local','session_id':'s','turn_id':'t'}),\n\
                   'cli': mod._frame('post_llm_call', {'platform':'cli','session_id':'s','turn_id':'t'}),\n\
                   'full': mod._frame('post_llm_call', {'platform':'local','created_source':'cli','user_id':'uid:1','session_id':'s','turn_id':'t'}),\n\
                 }))\n",
            )
            .arg(home.path())
            .arg(&script)
            .output()
            .expect("python");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let frames: serde_json::Value = serde_json::from_slice(&output.stdout).expect("frames");
        assert!(frames["missing"].is_null());
        assert!(frames["local"].is_null());
        assert_eq!(frames["cli"]["source"], "cli");
        assert!(frames["cli"].get("created_source").is_none());
        assert!(frames["cli"].get("user_id").is_none());
        assert_eq!(frames["full"]["source"], "local");
        assert_eq!(frames["full"]["created_source"], "cli");
        assert_eq!(frames["full"]["user_id"], "uid:1");
    }
}
