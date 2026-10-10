//! OpenCode plugin file management for installing and removing the Libra hook
//! forwarder (AG-19).
//!
//! Unlike Claude/Gemini (which edit a shared `settings.json`), OpenCode loads
//! JS plugin modules from `<project>/.opencode/plugin/*.js`, so Libra owns a
//! whole file: `.opencode/plugin/libra-hooks.js`. The file starts with a
//! Libra-managed marker comment; install refuses to overwrite a file without
//! the marker, and uninstall only removes files carrying it — a user-owned
//! plugin file is never touched.
//!
//! OpenCode (verified on 1.17.13) also loads plugins from the plural
//! `.opencode/plugins/` directory. Libra never writes there, but install,
//! uninstall, and status all detect a stray Libra-managed copy at that
//! location (a leftover from an older layout or a manual copy) because a
//! duplicate would double-forward every event: install and uninstall remove
//! it, status warns on stderr.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use super::super::super::{
    provider::ProviderInstallOptions,
    setup::{resolve_hook_binary_path, resolve_project_root},
};

const OPENCODE_DIR: &str = ".opencode";
/// Canonical plugin directory (singular) — the only location Libra writes.
const OPENCODE_PLUGIN_DIR: &str = "plugin";
/// Legacy/alternate plugin directory (plural) — also loaded by opencode;
/// only ever scanned for stray Libra-managed duplicates, never written.
const OPENCODE_LEGACY_PLUGIN_DIR: &str = "plugins";
const OPENCODE_PLUGIN_FILE: &str = "libra-hooks.js";

/// First line of every Libra-managed OpenCode plugin file. Uninstall and
/// stray-duplicate cleanup only touch files whose content starts with this
/// exact marker.
pub(super) const LIBRA_MANAGED_MARKER: &str =
    "// libra-managed: do not edit — installed by libra agent enable (AG-19)";

/// Placeholder replaced with the JSON-encoded (i.e. valid JS string literal)
/// hook command for the pinned Libra binary.
const LIBRA_COMMAND_PLACEHOLDER: &str = "__LIBRA_COMMAND_JSON__";

/// Plugin JS template. Contract verified against opencode 1.17.13 (probed
/// live 2026-07-05); see the module docs in `mod.rs` for the full upstream
/// facts. Every handler body is wrapped in try/catch, never throws, and drops
/// the event when spawning Libra fails — a Libra outage must never break the
/// OpenCode session.
const OPENCODE_PLUGIN_TEMPLATE: &str = r#"// libra-managed: do not edit — installed by libra agent enable (AG-19)
//
// Forwards OpenCode lifecycle events to Libra via
// `<libra> agent hooks opencode <verb>` with a JSON envelope on stdin.
// Verified against opencode 1.17.13 (probed 2026-07-05). Loaded from
// `.opencode/plugin/libra-hooks.js`; `opencode --pure` / OPENCODE_PURE=1
// disables all external plugins including this one. Plugin load errors are
// per-plugin and non-fatal (visible with `opencode --print-logs`).

const LIBRA_COMMAND = __LIBRA_COMMAND_JSON__;

export const LibraHooks = async ({ project, client, directory, worktree, serverUrl, $ }) => {
  const forward = async (verb, envelope) => {
    // Never throw: a Libra forwarding failure must not break the session.
    try {
      const payload = JSON.stringify(envelope);
      // LIBRA_COMMAND is a pre-quoted shell fragment (canonical absolute
      // path), so it is interpolated raw; verb is a fixed kebab-case token.
      await $`${{ raw: LIBRA_COMMAND }} agent hooks opencode ${verb} < ${new Response(payload)}`
        .quiet()
        .nothrow();
    } catch (_error) {
      // Drop the event: Libra hook ingestion is best-effort by design.
    }
  };

  const sessionIdOf = (properties) => {
    if (!properties || typeof properties !== "object") return "";
    if (typeof properties.sessionID === "string") return properties.sessionID;
    const info = properties.info;
    if (info && typeof info === "object") {
      if (typeof info.sessionID === "string") return info.sessionID;
      if (typeof info.id === "string") return info.id;
    }
    return "";
  };

  return {
    event: async ({ event }) => {
      try {
        if (!event || typeof event.type !== "string") return;
        const properties = event.properties || {};
        const sessionID = sessionIdOf(properties);
        const cwd =
          directory ||
          (properties.info && typeof properties.info.directory === "string"
            ? properties.info.directory
            : "");
        if (!sessionID || !cwd) return;
        const envelope = {
          hook_event_name: event.type,
          session_id: sessionID,
          cwd,
        };
        switch (event.type) {
          case "session.created":
            await forward("session-start", envelope);
            break;
          case "message.updated": {
            const info = properties.info;
            // Only user messages open a turn; assistant updates are dropped
            // here; the parser also validates the explicit prompt/role fields.
            if (!info || info.role !== "user") return;
            envelope.role = "user";
            // Legacy text is unavailable here. Contract A requires an
            // explicit string; OG-02 owns text-part pairing and transport.
            envelope.prompt = "";
            await forward("prompt", envelope);
            break;
          }
          case "session.idle":
            // Libra-side inference: reliable end-of-turn marker in headless
            // runs; NOT an official OpenCode terminal event.
            await forward("stop", envelope);
            break;
          case "session.deleted":
            await forward("session-end", envelope);
            break;
          case "session.compacted":
            await forward("compaction", envelope);
            break;
          default:
            // Streaming / unmapped events (message.part.updated, delta,
            // session.status, session.updated, session.diff, ...) are
            // intentionally never forwarded.
            return;
        }
      } catch (_error) {
        // Never throw from a plugin handler.
      }
    },
    "tool.execute.after": async (input, output) => {
      try {
        const sessionID =
          input && typeof input.sessionID === "string" ? input.sessionID : "";
        if (!sessionID || !directory) return;
        const envelope = {
          hook_event_name: "tool.execute.after",
          session_id: sessionID,
          cwd: directory,
        };
        if (input && typeof input.tool === "string") envelope.tool_name = input.tool;
        if (input && typeof input.callID === "string") envelope.tool_use_id = input.callID;
        if (output && typeof output.title === "string") envelope.tool_response = output.title;
        await forward("tool-use", envelope);
      } catch (_error) {
        // Never throw from a plugin handler.
      }
    },
  };
};
"#;

/// Install the Libra-managed OpenCode plugin at
/// `<project>/.opencode/plugin/libra-hooks.js` (project-local, mirroring the
/// Claude installer's `resolve_project_root()` target).
///
/// Boundary conditions:
/// - The embedded hook command uses the canonical absolute Libra binary path
///   from [`resolve_hook_binary_path`] — never a bare `libra` PATH lookup.
/// - An existing plugin file without the Libra marker is treated as
///   user-owned: install fails with an actionable error and leaves it intact.
/// - A stray Libra-managed duplicate under `.opencode/plugins/` is removed so
///   events are not double-forwarded (opencode loads both directories).
pub(super) fn install_opencode_hooks(options: &ProviderInstallOptions) -> Result<()> {
    let binary_path = resolve_hook_binary_path(options.binary_path.as_deref())?;
    if options.timeout_secs.is_some() {
        bail!("OpenCode hooks do not support --timeout");
    }

    let plugin_path = opencode_plugin_path()?;
    let content = render_opencode_plugin(&binary_path)?;

    if plugin_path.exists() {
        let existing = read_plugin_file(&plugin_path)?;
        if !is_libra_managed(&existing) {
            bail!(
                "refusing to overwrite unmanaged OpenCode plugin file '{}': it does not start \
                 with the Libra marker '{}'; move or rename the file and re-run the install",
                plugin_path.display(),
                LIBRA_MANAGED_MARKER,
            );
        }
        if existing == content {
            println!(
                "OpenCode hook plugin is already up to date at {}",
                plugin_path.display()
            );
        } else {
            write_plugin_file_atomic(&plugin_path, &content)?;
            println!("Updated OpenCode hook plugin at {}", plugin_path.display());
        }
    } else {
        write_plugin_file_atomic(&plugin_path, &content)?;
        println!(
            "Installed OpenCode hook plugin at {}",
            plugin_path.display()
        );
    }

    remove_stray_managed_duplicate()?;
    Ok(())
}

/// Remove the Libra-managed OpenCode plugin file.
///
/// Checks both the canonical `.opencode/plugin/` and the legacy/alternate
/// `.opencode/plugins/` locations; only files starting with the Libra marker
/// are removed, so user-owned plugin files are never touched. Idempotent —
/// running it with nothing installed succeeds with a notice.
pub(super) fn uninstall_opencode_hooks() -> Result<()> {
    let mut removed_any = false;

    for path in [opencode_plugin_path()?, opencode_legacy_plugin_path()?] {
        if !path.exists() {
            continue;
        }
        let existing = read_plugin_file(&path)?;
        if !is_libra_managed(&existing) {
            println!(
                "Skipping unmanaged OpenCode plugin file at {} (missing the Libra marker)",
                path.display()
            );
            continue;
        }
        fs::remove_file(&path).with_context(|| {
            format!(
                "failed to remove Libra-managed OpenCode plugin file '{}'",
                path.display()
            )
        })?;
        println!("Removed OpenCode hook plugin at {}", path.display());
        removed_any = true;
    }

    if !removed_any {
        println!("No Libra-managed OpenCode plugin found under .opencode/");
    }
    Ok(())
}

/// Whether the Libra-managed plugin file exists at the canonical location
/// (`.opencode/plugin/libra-hooks.js`) with its marker intact.
///
/// A status probe is deliberately silent even when a stray managed duplicate
/// exists under `.opencode/plugins/`: probes run inside structured commands
/// such as `agent doctor`, where an unsolicited path-bearing stderr warning
/// would break the JSON boundary. Install/uninstall continues to clean the
/// duplicate deterministically.
pub(super) fn opencode_hooks_are_installed() -> Result<bool> {
    let legacy_path = opencode_legacy_plugin_path()?;
    if legacy_path.exists() {
        // Preserve the read/error behavior (a corrupt managed duplicate is a
        // real status-probe failure), but retain no path in this read-only
        // predicate's output channel.
        let _ = is_libra_managed(&read_plugin_file(&legacy_path)?);
    }

    let plugin_path = opencode_plugin_path()?;
    if !plugin_path.exists() {
        return Ok(false);
    }
    Ok(is_libra_managed(&read_plugin_file(&plugin_path)?))
}

fn opencode_plugin_path() -> Result<PathBuf> {
    Ok(resolve_project_root()?
        .join(OPENCODE_DIR)
        .join(OPENCODE_PLUGIN_DIR)
        .join(OPENCODE_PLUGIN_FILE))
}

fn opencode_legacy_plugin_path() -> Result<PathBuf> {
    Ok(resolve_project_root()?
        .join(OPENCODE_DIR)
        .join(OPENCODE_LEGACY_PLUGIN_DIR)
        .join(OPENCODE_PLUGIN_FILE))
}

fn is_libra_managed(content: &str) -> bool {
    content.starts_with(LIBRA_MANAGED_MARKER)
}

fn read_plugin_file(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read OpenCode plugin file '{}'", path.display()))
}

/// Render the plugin with the resolved hook command baked in.
///
/// The command is a pre-quoted shell fragment (see
/// [`resolve_hook_binary_path`]), so it is embedded as a JSON-encoded JS
/// string literal and interpolated raw into the BunShell template inside the
/// plugin, preserving the shell quoting.
fn render_opencode_plugin(binary_command: &str) -> Result<String> {
    let literal = serde_json::to_string(binary_command)
        .context("failed to encode the Libra binary path for the OpenCode plugin")?;
    Ok(OPENCODE_PLUGIN_TEMPLATE.replace(LIBRA_COMMAND_PLACEHOLDER, &literal))
}

/// Remove a Libra-managed duplicate under `.opencode/plugins/`, if present.
/// User files (no marker) are left alone.
fn remove_stray_managed_duplicate() -> Result<()> {
    let legacy_path = opencode_legacy_plugin_path()?;
    if !legacy_path.exists() {
        return Ok(());
    }
    let existing = read_plugin_file(&legacy_path)?;
    if !is_libra_managed(&existing) {
        return Ok(());
    }
    fs::remove_file(&legacy_path).with_context(|| {
        format!(
            "failed to remove stray Libra-managed OpenCode plugin duplicate '{}'",
            legacy_path.display()
        )
    })?;
    println!(
        "Removed stray Libra-managed OpenCode plugin duplicate at {} (opencode loads both \
         'plugin/' and 'plugins/'; keeping both would double-forward every event)",
        legacy_path.display()
    );
    Ok(())
}

/// Atomically write the plugin file using the same temp-file + rename dance
/// as `setup::write_json_settings` (which is JSON-specific and therefore not
/// reused directly here).
fn write_plugin_file_atomic(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow!(
            "invalid OpenCode plugin path without parent: '{}'",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create OpenCode plugin directory '{}'",
            parent.display()
        )
    })?;

    let tmp_path = path.with_extension("js.tmp");
    fs::write(&tmp_path, content).with_context(|| {
        format!(
            "failed to write temporary OpenCode plugin file '{}'",
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
                        "failed to replace existing OpenCode plugin file '{}': {err}",
                        path.display()
                    ));
                }
            }
        }
    }

    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to replace OpenCode plugin file '{}' with '{}'",
            path.display(),
            tmp_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;
    use crate::utils::test::ChangeDirGuard;

    /// Seed a minimal on-disk Libra repository marker so
    /// `resolve_project_root()` treats the tempdir as the project root.
    fn seed_libra_repo(root: &Path) {
        let libra_dir = root.join(".libra");
        fs::create_dir_all(&libra_dir).expect("create .libra");
        fs::write(libra_dir.join("libra.db"), b"").expect("seed libra.db");
    }

    /// Create a fake (existing, canonicalizable) binary and return its
    /// canonical path — `resolve_hook_binary_path` requires the file to exist.
    fn seed_fake_binary(root: &Path) -> PathBuf {
        let fake_binary = root.join("libra-fake-binary");
        fs::write(&fake_binary, "#!/bin/sh\n").expect("write fake binary");
        fs::canonicalize(&fake_binary).expect("canonicalize fake binary")
    }

    fn install_options(binary: &Path) -> ProviderInstallOptions {
        ProviderInstallOptions {
            binary_path: Some(binary.display().to_string()),
            timeout_secs: None,
        }
    }

    /// The template's first line is the marker constant — install/uninstall
    /// marker detection depends on the two never drifting apart.
    #[test]
    fn plugin_template_starts_with_managed_marker() {
        assert!(OPENCODE_PLUGIN_TEMPLATE.starts_with(LIBRA_MANAGED_MARKER));
        assert!(OPENCODE_PLUGIN_TEMPLATE.contains(LIBRA_COMMAND_PLACEHOLDER));
    }

    /// `--timeout` has no meaning for an OpenCode JS plugin — reject it like
    /// the Gemini installer does.
    #[test]
    fn install_rejects_timeout_option() {
        let options = ProviderInstallOptions {
            binary_path: None,
            timeout_secs: Some(5),
        };
        let err = install_opencode_hooks(&options).unwrap_err();
        assert!(
            format!("{err:#}").contains("do not support --timeout"),
            "got: {err:#}",
        );
    }

    /// Full round trip: install writes the marker file with the canonical
    /// binary path, a second install is a no-op, uninstall removes the file,
    /// and a second uninstall stays idempotent.
    #[test]
    #[serial(cwd)]
    fn install_round_trip_is_idempotent() {
        let tmp = TempDir::new().expect("tmp dir");
        seed_libra_repo(tmp.path());
        let canonical_binary = seed_fake_binary(tmp.path());
        let _guard = ChangeDirGuard::new(tmp.path());
        let root = fs::canonicalize(tmp.path()).expect("canonicalize root");
        let plugin_path = root.join(".opencode/plugin/libra-hooks.js");

        install_opencode_hooks(&install_options(&canonical_binary)).expect("install");
        let content = fs::read_to_string(&plugin_path).expect("plugin written");
        assert!(content.starts_with(LIBRA_MANAGED_MARKER));
        assert!(
            content.contains(&canonical_binary.display().to_string()),
            "plugin must embed the canonical binary path; got: {content}",
        );
        assert!(content.contains("agent hooks opencode"));
        assert!(!content.contains(LIBRA_COMMAND_PLACEHOLDER));
        assert!(opencode_hooks_are_installed().expect("status"));

        // Second install is a no-op and leaves identical content behind.
        install_opencode_hooks(&install_options(&canonical_binary)).expect("re-install");
        let content_after = fs::read_to_string(&plugin_path).expect("plugin still there");
        assert_eq!(content, content_after);

        uninstall_opencode_hooks().expect("uninstall");
        assert!(!plugin_path.exists());
        assert!(!opencode_hooks_are_installed().expect("status"));

        // Idempotent: uninstalling again succeeds with nothing to do.
        uninstall_opencode_hooks().expect("second uninstall");
    }

    /// A user-owned (unmarked) plugin file at the managed path is never
    /// overwritten by install and never removed by uninstall.
    #[test]
    #[serial(cwd)]
    fn user_owned_plugin_file_is_never_touched() {
        let tmp = TempDir::new().expect("tmp dir");
        seed_libra_repo(tmp.path());
        let canonical_binary = seed_fake_binary(tmp.path());
        let _guard = ChangeDirGuard::new(tmp.path());
        let root = fs::canonicalize(tmp.path()).expect("canonicalize root");
        let plugin_dir = root.join(".opencode/plugin");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        let plugin_path = plugin_dir.join("libra-hooks.js");
        let user_content = "export const MyPlugin = async () => ({});\n";
        fs::write(&plugin_path, user_content).expect("write user plugin");

        let err = install_opencode_hooks(&install_options(&canonical_binary)).unwrap_err();
        assert!(
            format!("{err:#}").contains("refusing to overwrite"),
            "got: {err:#}",
        );
        assert_eq!(
            fs::read_to_string(&plugin_path).expect("read back"),
            user_content,
            "failed install must leave the user's file byte-identical",
        );
        assert!(!opencode_hooks_are_installed().expect("status"));

        uninstall_opencode_hooks().expect("uninstall is a safe no-op");
        assert_eq!(
            fs::read_to_string(&plugin_path).expect("read back"),
            user_content,
            "uninstall must leave the user's file byte-identical",
        );
    }

    /// A stray Libra-managed duplicate under the plural `.opencode/plugins/`
    /// directory is cleaned by both uninstall and install, while a user file
    /// at that location survives.
    #[test]
    #[serial(cwd)]
    fn stray_managed_duplicate_in_plugins_dir_is_cleaned() {
        let tmp = TempDir::new().expect("tmp dir");
        seed_libra_repo(tmp.path());
        let canonical_binary = seed_fake_binary(tmp.path());
        let _guard = ChangeDirGuard::new(tmp.path());
        let root = fs::canonicalize(tmp.path()).expect("canonicalize root");
        let legacy_dir = root.join(".opencode/plugins");
        fs::create_dir_all(&legacy_dir).expect("create plugins dir");
        let legacy_path = legacy_dir.join("libra-hooks.js");
        let managed_content = format!("{LIBRA_MANAGED_MARKER}\n// stray copy\n");

        // Uninstall removes a managed stray even with nothing at the
        // canonical location.
        fs::write(&legacy_path, &managed_content).expect("seed stray");
        uninstall_opencode_hooks().expect("uninstall");
        assert!(!legacy_path.exists(), "uninstall must clean the stray");

        // Install cleans a managed stray alongside writing the canonical file.
        fs::write(&legacy_path, &managed_content).expect("re-seed stray");
        install_opencode_hooks(&install_options(&canonical_binary)).expect("install");
        assert!(!legacy_path.exists(), "install must clean the stray");
        assert!(root.join(".opencode/plugin/libra-hooks.js").exists());

        // A user-owned file in plugins/ is left alone by both paths.
        let user_content = "export const MyPlugin = async () => ({});\n";
        fs::write(&legacy_path, user_content).expect("seed user file");
        install_opencode_hooks(&install_options(&canonical_binary)).expect("re-install");
        uninstall_opencode_hooks().expect("uninstall");
        assert_eq!(
            fs::read_to_string(&legacy_path).expect("read back"),
            user_content,
            "user file under plugins/ must never be touched",
        );
    }

    /// The rendered plugin embeds the command as a valid JS string literal
    /// even when the shell-quoted path contains quotes.
    #[test]
    fn render_embeds_command_as_json_literal() {
        let rendered = render_opencode_plugin("'/tmp/dir with spaces/libra'").expect("render");
        assert!(rendered.contains(r#"const LIBRA_COMMAND = "'/tmp/dir with spaces/libra'";"#));
    }
    // Executes the actual old template handler with a controlled BunShell
    // transport. This proves its wire field, not OpenCode 2.0.26 integration.
    #[test]
    fn legacy_prompt_forwarding_supplies_explicit_string() {
        let dir = TempDir::new().expect("isolated Node harness");
        let source = OPENCODE_PLUGIN_TEMPLATE.replace(
            "__LIBRA_COMMAND_JSON__",
            &serde_json::to_string("/synthetic/libra").expect("synthetic command"),
        );
        let harness = r#"
const frames = [];
const shell = (_strings, raw, verb, body) => ({
  quiet() { return this; },
  async nothrow() { frames.push({verb, payload:JSON.parse(await body.text())}); }
});
const plugin = await LibraHooks({directory:"/synthetic-project", $:shell});
await plugin.event({event:{type:"message.updated", properties:{sessionID:"synthetic-user", info:{role:"user"}}}});
await plugin.event({event:{type:"message.updated", properties:{sessionID:"synthetic-assistant", info:{role:"assistant"}}}});
console.log(JSON.stringify(frames));
"#;
        let script = dir.path().join("legacy-prompt.mjs");
        fs::write(&script, format!("{source}\n{harness}")).expect("write actual-template harness");
        let output = match std::process::Command::new("node")
            .arg("--no-warnings")
            .arg(&script)
            .current_dir(dir.path())
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "[skip] OG-01 actual-template runtime: Node is not installed; no producer proof recorded"
                );
                return;
            }
            Err(error) => panic!("run Node template harness: {error}"),
        };
        assert!(
            output.status.success(),
            "actual template failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "unexpected template diagnostic");
        let actual: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("runtime frames JSON");
        assert_eq!(
            actual,
            serde_json::json!([{
                "verb":"prompt",
                "payload": {
                    "hook_event_name":"message.updated", "session_id":"synthetic-user",
                    "cwd":"/synthetic-project", "role":"user", "prompt":""
                }
            }]),
            "user forwards once with explicit empty-string prompt; assistant never forwards"
        );
        println!("[executed] OG-01 actual-template Node legacy prompt wire contract");
    }
}
