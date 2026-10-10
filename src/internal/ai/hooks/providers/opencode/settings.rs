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
//! OpenCode 2.0.26 uses a default plugin with `setup(context)` and a returned
//! cleanup function. Node and Bun share the same child_process forwarding.
//! The alternate plural directory is inspected for old managed duplicates.
//! Both layouts are preflighted before any reads or writes; Unix mutations
//! use pinned directory descriptors and never follow configuration symlinks.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

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
const PLUGIN_REVISION: &str = "// libra-plugin-template: opencode-2.0.26-v1";
const EVENT_COMMANDS_PLACEHOLDER: &str = "__EVENT_COMMANDS_JSON__";
const EXPORT_DEADLINE_PLACEHOLDER: &str = "__EXPORT_DEADLINE_MS__";

/// Pinned observe-only plugin; subprocess forwarding works in both Node and Bun.
const OPENCODE_PLUGIN_TEMPLATE: &str = r#"// libra-managed: do not edit — installed by libra agent enable (AG-19)
// libra-plugin-template: opencode-2.0.26-v1
// OpenCode 2.0.26. Observe-only; runs on Node and Bun using agent hooks opencode.
// OpenCode --pure / OPENCODE_PURE=1 disables external plugins.
import { spawn, spawnSync } from "node:child_process";
import { statSync } from "node:fs";

const LIBRA_COMMAND = __LIBRA_COMMAND_JSON__;
const EVENT_COMMANDS = __EVENT_COMMANDS_JSON__;
const EXPORT_DEADLINE_MS = __EXPORT_DEADLINE_MS__;
const FORWARD_TIMEOUT_MS = EXPORT_DEADLINE_MS + 15000;
const MAX_SESSIONS = 64;
const MAX_PENDING = 128;
const MAX_PROMPT = 16384;
const REGISTRY_KEY = Symbol.for("libra.opencode.exit.v2");

function exitRegistry() {
  if (!globalThis[REGISTRY_KEY]) {
    const registry = { instances: new Set(), owners: new Map() };
    process.on("exit", () => {
      const deadline = Date.now() + FORWARD_TIMEOUT_MS;
      for (const flush of registry.instances) {
        try { flush(deadline); } catch (_error) { /* Fail open during host shutdown. */ }
      }
    });
    globalThis[REGISTRY_KEY] = registry;
  }
  return globalThis[REGISTRY_KEY];
}

export default {
  id: "libra-hooks",
  async setup(context) {
    const cwd = context?.location?.directory;
    const active = new Map();
    const pending = new Map();
    const seen = new Map();
    const earlyText = new Map();
    const warnings = new Set();
    const children = new Set();
    const controller = new AbortController();
    let disposed = false;
    let toolRegistration;
    const warn = (reason) => {
      if (warnings.has(reason)) return;
      warnings.add(reason);
      // Only fixed reasons: never include event bodies, paths or exception text.
      try { process.stderr.write("libra opencode: " + reason + "\n"); } catch (_error) {}
    };
    const validId = (id) => typeof id === "string" && id.length > 0 && id.length <= 512;
    const binaryAvailable = () => {
      try { return statSync(LIBRA_COMMAND).isFile(); } catch (_error) { return false; }
    };
    const frame = (name, sessionID, extra = {}) => ({
      hook_event_name: name, session_id: sessionID, cwd, ...extra,
    });
    const forward = (envelope, sync = true, deadline = undefined) => {
      try {
        const verb = EVENT_COMMANDS[envelope.hook_event_name];
        if (!verb || !validId(envelope.session_id) || !binaryAvailable()) return;
        const input = JSON.stringify(envelope);
        const args = ["agent", "hooks", "opencode", verb];
        const timeout = deadline === undefined ? FORWARD_TIMEOUT_MS : Math.min(FORWARD_TIMEOUT_MS, deadline - Date.now());
        if (timeout <= 0) { warn("shutdown_capture_budget_exhausted"); return; }
        const options = { cwd, timeout, killSignal: "SIGKILL",
          stdio: ["pipe", "ignore", "ignore"], windowsHide: true, shell: false };
        if (sync) {
          // SIGKILL is essential: spawnSync waits indefinitely after a caught SIGTERM.
          const result = spawnSync(LIBRA_COMMAND, args, { ...options, input });
          if (result.error || result.status !== 0) warn(result.error?.code === "ETIMEDOUT" ? "forward_timeout" : "forward_failed");
          return;
        }
        if (children.size >= 16) { warn("forward_capacity_exceeded"); return; }
        const child = spawn(LIBRA_COMMAND, args, options);
        children.add(child);
        const finish = () => { children.delete(child); };
        child.on("error", () => { finish(); warn("forward_failed"); });
        child.on("close", (code, signal) => {
          finish();
          if (!child.libraCancelled && (code !== 0 || signal)) warn(signal === "SIGKILL" ? "forward_timeout" : "forward_failed");
        });
        child.stdin?.on("error", () => {});
        child.stdin?.end(input);
      } catch (_error) { warn("forward_failed"); }
    };
    const start = (id) => {
      if (!validId(id)) return false;
      const key = JSON.stringify([cwd, id]);
      if (active.has(id)) {
        // Map insertion order is the process-wide least-recently-observed order.
        const close = registry.owners.get(key);
        registry.owners.delete(key);
        registry.owners.set(key, close);
        return true;
      }
      if (registry.owners.has(key)) return false;
      if (registry.owners.size >= MAX_SESSIONS) {
        warn("session_capacity_exceeded");
        const oldest = registry.owners.values().next().value;
        oldest?.();
        if (registry.owners.size >= MAX_SESSIONS) return false;
      }
      active.set(id, {});
      registry.owners.set(key, () => end(id, "server.instance.disposed"));
      forward(frame("session.created", id));
      return true;
    };
    const remember = (key) => {
      if (seen.has(key)) return false;
      if (seen.size >= 256) seen.delete(seen.keys().next().value);
      seen.set(key, JSON.parse(key)[0]);
      return true;
    };
    const boundedPrompt = (text) => {
      if (typeof text !== "string") { warn("prompt_unavailable"); text = ""; }
      const truncated = text.length > MAX_PROMPT;
      if (truncated) {
        warn("prompt_truncated");
        text = text.slice(0, MAX_PROMPT);
        // Do not emit a lone UTF-16 high surrogate: Rust JSON rejects it.
        const last = text.charCodeAt(text.length - 1);
        if (last >= 0xD800 && last <= 0xDBFF) text = text.slice(0, -1);
      }
      return { text, truncated };
    };
    const cachePrompt = (id, message, text, legacy = false) => {
      const key = JSON.stringify([id, message]);
      if (!validId(id) || !validId(message) || seen.has(key) || pending.has(key)) return;
      if (pending.size >= MAX_PENDING) { warn("prompt_capacity_exceeded"); return; }
      pending.set(key, { id, message, legacy, ...boundedPrompt(text) });
    };
    const deliver = (key, name, deadline = undefined) => {
      const item = pending.get(key);
      if (!item) { if (!seen.has(key)) warn("prompt_delivery_unclassified"); return; }
      pending.delete(key);
      earlyText.delete(key);
      if (!remember(key) || !start(item.id)) return;
      forward(frame(name, item.id, { role: "user", message_id: item.message, prompt: item.text, ...(item.truncated ? { prompt_truncated: true } : {}) }), true, deadline);
    };
    const end = (id, name, deadline = undefined) => {
      if (!active.has(id)) return;
      // Remove before spawning so disposal and exit cannot forward twice.
      active.delete(id);
      registry.owners.delete(JSON.stringify([cwd, id]));
      for (const [key, item] of pending) if (item.id === id) pending.delete(key);
      for (const [key, item] of earlyText) if (item.id === id) earlyText.delete(key);
      for (const [key, session] of seen) if (session === id) seen.delete(key);
      forward(frame(name, id), true, deadline);
    };
    const flush = (deadline = Date.now() + FORWARD_TIMEOUT_MS) => {
      // Node's timeout timers do not survive process.exit(). Kill pending
      // observations before leaving, including the explicit-exit fallback.
      for (const child of children) { try { child.libraCancelled = true; child.kill("SIGKILL"); } catch (_error) {} }
      children.clear();
      for (const [key, item] of pending) if (item.legacy) deliver(key, "message.updated", deadline);
      for (const id of active.keys()) end(id, "server.instance.disposed", deadline);
    };
    const handle = async (event) => {
      try {
        if (disposed || typeof event?.type !== "string") return;
        const name = event.type;
        const data = event.data ?? event.properties ?? {};
        const info = data.info;
        const id = data.sessionID ?? info?.sessionID ?? info?.id;
        // Old hosts used separate user-message and text-part events. Flush an
        // empty prompt only when the next event cannot supply that message's text.
        for (const [key, item] of pending) {
          if (item.legacy && !(name === "message.part.updated" && data.part?.sessionID === item.id && data.part?.messageID === item.message) && !(name === "message.updated" && info?.sessionID === item.id && info?.id === item.message)) {
            deliver(key, "message.updated");
          }
        }
        if (name === "server.instance.disposed") {
          if (active.size === 0) warn("disposed_without_session");
          flush();
          return;
        }
        if (!validId(id) && name !== "message.part.updated") return;
        switch (name) {
          case "session.created":
            start(id);
            break;
          case "session.inbox.enqueued":
            if (data.item?.type === "user") cachePrompt(id, data.inboxID, data.item.payload?.text);
            else if (["synthetic", "compaction", "move"].includes(data.item?.type)) remember(JSON.stringify([id, data.inboxID]));
            break;
          case "session.inbox.cancelled":
            pending.delete(JSON.stringify([id, data.inboxID]));
            remember(JSON.stringify([id, data.inboxID]));
            break;
          case "session.inbox.delivered":
            deliver(JSON.stringify([id, data.inboxID]), name);
            break;
          case "session.step.started":
            if (start(id) && typeof data.model?.id === "string") active.get(id).model = data.model.id.slice(0, 512);
            break;
          case "message.updated":
            if (info?.role === "user") {
              if (start(id)) {
                const key = JSON.stringify([id, info.id]);
                cachePrompt(id, info.id, "", true);
                const text = earlyText.get(key);
                if (text && pending.has(key)) {
                  Object.assign(pending.get(key), text.value);
                  deliver(key, name);
                }
              }
            } else if (info?.role === "assistant") {
              earlyText.delete(JSON.stringify([id, info.id]));
              if (start(id) && typeof info.modelID === "string") active.get(id).model = info.modelID.slice(0, 512);
            }
            break;
          case "message.part.updated": {
            const part = data.part;
            if (part?.type !== "text" || typeof part.text !== "string" || !validId(part.sessionID) || !validId(part.messageID)) break;
            const key = JSON.stringify([part.sessionID, part.messageID]);
            const item = pending.get(key);
            if (item?.legacy) {
              Object.assign(item, boundedPrompt(part.text));
              deliver(key, "message.updated");
            } else if (!seen.has(key) && !earlyText.has(key)) {
              if (earlyText.size >= MAX_PENDING) {
                warn("prompt_capacity_exceeded");
                earlyText.delete(earlyText.keys().next().value);
              }
              earlyText.set(key, { id: part.sessionID, value: boundedPrompt(part.text) });
            }
            break;
          }
          case "session.execution.succeeded":
          case "session.execution.failed":
          case "session.execution.interrupted": {
            if (name === "session.execution.interrupted" && !["user", "superseded", "inactivity"].includes(data.reason)) return;
            if (!start(id)) return;
            const extra = { ...active.get(id) };
            if (name === "session.execution.interrupted") extra.reason = data.reason;
            forward(frame(name, id, extra));
            break;
          }
          case "session.status":
            if (data.status?.type !== "idle" || !start(id)) return;
            forward(frame(name, id, { status: { type: "idle" }, ...active.get(id) }));
            break;
          case "session.deleted":
            end(id, name);
            break;
          case "session.compaction.ended":
          case "session.compacted":
            if (start(id)) forward(frame(name, id), false);
            break;
        }
      } catch (_error) { warn("event_skipped"); }
    };
    if (typeof cwd !== "string" || !cwd || !binaryAvailable()) return;
    const registry = exitRegistry();
    registry.instances.add(flush);
    try {
      toolRegistration = await context.tool.hook("execute.after", (input) => {
        try {
          if (disposed || !validId(input?.sessionID) || !start(input.sessionID)) return;
          const extra = {};
          if (typeof input.tool === "string") extra.tool_name = input.tool.slice(0, 512);
          if (typeof input.id === "string") extra.tool_use_id = input.id.slice(0, 512);
          // Do not forward input/result/provider state and never mutate the hook argument.
          forward(frame("tool.execute.after", input.sessionID, extra), false);
        } catch (_error) { warn("tool_event_skipped"); }
      });
    } catch (_error) { warn("tool_registration_unavailable"); }
    try {
      const stream = context.event.subscribe({ signal: controller.signal });
      void (async () => {
        try { for await (const event of stream) await handle(event); }
        catch (_error) { if (!disposed) warn("event_stream_unavailable"); }
      })();
    } catch (_error) { warn("plugin_setup_incomplete"); }
    return async () => {
      if (disposed) return;
      disposed = true;
      controller.abort();
      // Outstanding asynchronous observations are bounded and cannot outlive cleanup.
      for (const child of children) { try { child.libraCancelled = true; child.kill("SIGKILL"); } catch (_error) {} }
      children.clear();
      flush();
      pending.clear();
      seen.clear();
      earlyText.clear();
      registry.instances.delete(flush);
      try { await toolRegistration?.dispose(); } catch (_error) { warn("tool_cleanup_failed"); }
    };
  },
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
    let legacy_path = opencode_legacy_plugin_path()?;
    let content = render_opencode_plugin(&binary_path)?;
    validate_plugin_paths()?;
    // Complete both reads before creating anything: a corrupt/unreadable legacy
    // copy must not turn a failed install into two simultaneously loaded plugins.
    let previous = read_plugin_snapshot(&plugin_path)?;
    let legacy = read_plugin_snapshot(&legacy_path)?;
    if previous
        .as_ref()
        .is_some_and(|file| !is_libra_managed(&file.content))
    {
        bail!(
            "refusing to overwrite unmanaged OpenCode plugin file; move or rename .opencode/plugin/libra-hooks.js and re-run the install"
        );
    }
    let changed = previous.as_ref().is_none_or(|file| file.content != content);
    let written = if changed {
        Some(write_plugin_file_atomic(
            &plugin_path,
            &content,
            previous.as_ref(),
        )?)
    } else {
        None
    };
    if let Some(legacy) = legacy.filter(|file| is_libra_managed(&file.content)) {
        if let Err(error) = remove_plugin_file(&legacy_path, &legacy) {
            if let Some(written) = written.as_ref() {
                let rollback = match previous.as_ref() {
                    Some(previous) => {
                        write_plugin_file_atomic(&plugin_path, &previous.content, Some(written))
                            .map(|_| ())
                    }
                    None => remove_plugin_file(&plugin_path, written),
                };
                rollback.context("OpenCode duplicate cleanup and rollback failed; preserve both plugin files and retry after resolving concurrent edits or permissions")?;
            }
            return Err(error).context(if written.is_some() {
                "OpenCode duplicate cleanup failed; canonical plugin changes were rolled back"
            } else {
                "OpenCode duplicate cleanup failed; canonical plugin was not changed"
            });
        }
        println!("Removed stray Libra-managed OpenCode plugin duplicate");
    }
    println!(
        "OpenCode hook plugin {} at {}",
        if changed {
            "installed/updated"
        } else {
            "is already up to date"
        },
        plugin_path.display()
    );
    Ok(())
}

/// Remove the Libra-managed OpenCode plugin file.
///
/// Checks both the canonical `.opencode/plugin/` and the legacy/alternate
/// `.opencode/plugins/` locations; only files starting with the Libra marker
/// are removed, so user-owned plugin files are never touched. Idempotent —
/// running it with nothing installed succeeds with a notice.
pub(super) fn uninstall_opencode_hooks() -> Result<()> {
    validate_plugin_paths()?;
    let paths = [opencode_plugin_path()?, opencode_legacy_plugin_path()?];
    let snapshots = [
        read_plugin_snapshot(&paths[0])?,
        read_plugin_snapshot(&paths[1])?,
    ];
    let mut removed_any = false;
    for (path, snapshot) in paths.into_iter().zip(snapshots) {
        let Some(existing) = snapshot else {
            continue;
        };
        if !is_libra_managed(&existing.content) {
            println!(
                "Skipping unmanaged OpenCode plugin file at {} (missing the Libra marker)",
                path.display()
            );
            continue;
        }
        remove_plugin_file(&path, &existing)?;
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
    validate_plugin_paths()?;
    let canonical = read_plugin_snapshot(&opencode_plugin_path()?)?;
    let legacy = read_plugin_snapshot(&opencode_legacy_plugin_path()?)?;
    if legacy
        .as_ref()
        .is_some_and(|file| is_libra_managed(&file.content))
    {
        return Ok(false);
    }
    let Some(file) = canonical else {
        return Ok(false);
    };
    if !is_libra_managed(&file.content) || file.content.lines().nth(1) != Some(PLUGIN_REVISION) {
        return Ok(false);
    }
    let Some(encoded) = file.content.lines().find_map(|line| {
        line.strip_prefix("const LIBRA_COMMAND = ")?
            .strip_suffix(';')
    }) else {
        return Ok(false);
    };
    let Ok(binary) = serde_json::from_str::<String>(encoded) else {
        return Ok(false);
    };
    if !Path::new(&binary).is_absolute() {
        return Ok(false);
    }
    Ok(file.content == render_opencode_plugin_for_binary(&binary)?)
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

#[derive(PartialEq, Eq)]
struct PluginSnapshot {
    content: String,
    #[cfg(unix)]
    identity: (u64, u64, u64, i64, i64, i64, i64),
    #[cfg(windows)]
    modified: std::time::SystemTime,
}

fn snapshot_file(mut file: fs::File) -> Result<PluginSnapshot> {
    let before = file.metadata()?;
    if !before.is_file() {
        bail!(
            "unsafe OpenCode plugin leaf: expected a regular file; remove the special file before retrying"
        );
    }
    let mut content = String::new();
    (&mut file)
        .take(1024 * 1024 + 1)
        .read_to_string(&mut content)
        .context("failed to read OpenCode plugin; restore a valid UTF-8 plugin")?;
    if content.len() > 1024 * 1024 {
        bail!(
            "OpenCode plugin exceeds the 1 MiB inspection limit; move it aside before reinstalling"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let identity = |m: &fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        if identity(&before) != identity(&file.metadata()?) {
            bail!("OpenCode plugin changed during inspection; retry after concurrent edits finish");
        }
        Ok(PluginSnapshot {
            content,
            identity: identity(&before),
        })
    }
    #[cfg(windows)]
    {
        let modified = before.modified()?;
        if file.metadata()?.modified()? != modified {
            bail!("OpenCode plugin changed during inspection; retry after concurrent edits finish");
        }
        Ok(PluginSnapshot { content, modified })
    }
}

#[cfg(unix)]
fn snapshot_in_directory(directory: &fs::File) -> Result<Option<PluginSnapshot>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: the descriptor is owned and the static leaf is NUL-terminated.
    // O_NONBLOCK closes the preflight-to-open FIFO substitution window.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"libra-hooks.js".as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error)
            .context("cannot safely open OpenCode plugin; check its type and permissions");
    }
    // SAFETY: openat returned a new descriptor which is transferred once.
    snapshot_file(unsafe { fs::File::from_raw_fd(fd) }).map(Some)
}

fn read_plugin_snapshot(path: &Path) -> Result<Option<PluginSnapshot>> {
    #[cfg(unix)]
    {
        match plugin_directory(path, false) {
            Ok(directory) => snapshot_in_directory(&directory),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(windows)]
    {
        let root = resolve_project_root()?;
        let directory = crate::utils::beneath::open_root(&root)?;
        match crate::utils::beneath::open_file_beneath(&directory, path.strip_prefix(root)?) {
            Ok(file) => snapshot_file(file).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("cannot safely open OpenCode plugin"),
        }
    }
}

fn verify_snapshot(
    current: Option<&PluginSnapshot>,
    expected: Option<&PluginSnapshot>,
) -> Result<()> {
    if current != expected {
        bail!(
            "OpenCode plugin changed since inspection; refusing to overwrite or remove concurrent user edits, retry after edits finish"
        );
    }
    if current.is_some_and(|file| !is_libra_managed(&file.content)) {
        bail!("refusing to change an unmanaged OpenCode plugin; move or rename it before retrying");
    }
    Ok(())
}

/// Render the plugin with the resolved hook command baked in.
///
/// Decode the shared resolver's shell quoting once, before embedding a plain
/// filename for `spawn(..., { shell: false })`. No shell interprets the result.
fn render_opencode_plugin(binary_command: &str) -> Result<String> {
    #[cfg(unix)]
    let binary = {
        let words = shlex::split(binary_command)
            .context("invalid quoted Libra binary path for OpenCode; reinstall the plugin")?;
        if words.len() != 1 {
            bail!("OpenCode requires one absolute Libra executable path");
        }
        words
            .into_iter()
            .next()
            .context("missing Libra executable path")?
    };
    #[cfg(windows)]
    let binary = binary_command.trim_matches('"').replace("\\\"", "\"");
    if !Path::new(&binary).is_absolute() {
        bail!("OpenCode requires an absolute Libra executable path");
    }
    render_opencode_plugin_for_binary(&binary)
}

fn render_opencode_plugin_for_binary(binary: &str) -> Result<String> {
    let literal = serde_json::to_string(binary)
        .context("failed to encode the Libra binary path for the OpenCode plugin")?;
    let commands: std::collections::BTreeMap<_, _> = super::events::OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .chain(super::events::OPENCODE_LEGACY_EVENT_SPECS)
        .filter_map(|spec| spec.command.map(|command| (spec.name, command.to_string())))
        .collect();
    Ok(OPENCODE_PLUGIN_TEMPLATE
        .replace(LIBRA_COMMAND_PLACEHOLDER, &literal)
        .replace(
            EVENT_COMMANDS_PLACEHOLDER,
            &serde_json::to_string(&commands)?,
        )
        .replace(
            EXPORT_DEADLINE_PLACEHOLDER,
            &crate::internal::ai::observed_agents::opencode_export::EXPORT_DEADLINE
                .as_millis()
                .to_string(),
        ))
}

/// Preflight both layouts before any read, mkdir, replacement or unlink. Errors
/// contain only the known configuration slot and its type, never file contents.
fn validate_plugin_paths() -> Result<()> {
    let root = resolve_project_root()?;
    for (relative, directory) in [
        (".opencode", true),
        (".opencode/plugin", true),
        (".opencode/plugins", true),
        (".opencode/plugin/libra-hooks.js", false),
        (".opencode/plugins/libra-hooks.js", false),
    ] {
        match fs::symlink_metadata(root.join(relative)) {
            Ok(metadata) => {
                validate_config_slot(relative, directory, metadata.file_type())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "cannot inspect OpenCode config path '{relative}'; check its permissions"
                    )
                });
            }
        }
    }
    Ok(())
}

fn validate_config_slot(relative: &str, directory: bool, kind: fs::FileType) -> Result<()> {
    if kind.is_symlink()
        || if directory {
            !kind.is_dir()
        } else {
            !kind.is_file()
        }
    {
        let reason = if kind.is_symlink() {
            "symlink"
        } else if kind.is_dir() {
            "directory"
        } else if kind.is_file() {
            "regular file"
        } else {
            "special file (FIFO/socket/device)"
        };
        bail!(
            "unsafe OpenCode config path '{relative}': {reason}; replace it with a {} before retrying",
            if directory {
                "directory"
            } else {
                "regular file"
            }
        );
    }
    Ok(())
}

/// Pin directories component by component so a concurrent symlink substitution
/// cannot redirect installation or removal outside the discovered repository.
#[cfg(unix)]
fn plugin_directory(path: &Path, create: bool) -> Result<fs::File> {
    use std::{
        ffi::CString,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
    };
    let root = resolve_project_root()?;
    let parent = path
        .parent()
        .context("OpenCode plugin path has no parent")?;
    let relative = parent.strip_prefix(&root)?;
    let mut directory = crate::utils::beneath::open_root(&root)?;
    for component in relative.components() {
        let name = component.as_os_str();
        if create {
            let c_name =
                CString::new(name.as_bytes()).context("invalid OpenCode directory name")?;
            // SAFETY: directory owns the fd; c_name is a live NUL-terminated component.
            let result = unsafe { libc::mkdirat(directory.as_raw_fd(), c_name.as_ptr(), 0o755) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error).context(
                        "cannot create OpenCode plugin directory; check repository permissions",
                    );
                }
            }
        }
        directory = crate::utils::beneath::open_beneath(&directory, Path::new(name)).context(
            "cannot safely open OpenCode plugin directory; remove symlinks or special files",
        )?;
    }
    Ok(directory)
}

#[cfg(unix)]
fn remove_plugin_file(path: &Path, expected: &PluginSnapshot) -> Result<()> {
    use std::os::fd::AsRawFd;
    let directory = plugin_directory(path, false)?;
    verify_snapshot(snapshot_in_directory(&directory)?.as_ref(), Some(expected))?;
    // SAFETY: directory owns its descriptor; the constant is NUL-terminated.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), c"libra-hooks.js".as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to remove OpenCode plugin; check directory permissions");
    }
    directory
        .sync_all()
        .context("failed to persist OpenCode plugin removal")
}

#[cfg(windows)]
fn remove_plugin_file(path: &Path, expected: &PluginSnapshot) -> Result<()> {
    validate_plugin_paths()?;
    verify_snapshot(read_plugin_snapshot(path)?.as_ref(), Some(expected))?;
    fs::remove_file(path).context("failed to remove OpenCode plugin")
}

#[cfg(unix)]
fn write_plugin_file_atomic(
    path: &Path,
    content: &str,
    expected: Option<&PluginSnapshot>,
) -> Result<PluginSnapshot> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd},
    };
    let directory = plugin_directory(path, true)?;
    let name = CString::new(format!(".libra-hooks-{}.tmp", uuid::Uuid::new_v4()))?;
    // SAFETY: the pinned directory and NUL-terminated random basename are live.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("cannot create temporary OpenCode plugin; check directory permissions");
    }
    // SAFETY: successful openat transferred a new owned descriptor.
    let mut temporary = unsafe { fs::File::from_raw_fd(fd) };
    let result = (|| -> Result<PluginSnapshot> {
        temporary
            .write_all(content.as_bytes())
            .context("failed to write OpenCode plugin")?;
        temporary
            .sync_all()
            .context("failed to persist OpenCode plugin")?;
        verify_snapshot(snapshot_in_directory(&directory)?.as_ref(), expected)?;
        if expected.is_none() {
            // SAFETY: hard-link publication is atomic and never overwrites a
            // concurrently created destination; both names are in the held dir.
            if unsafe {
                libc::linkat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    directory.as_raw_fd(),
                    c"libra-hooks.js".as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error()).context(
                    "OpenCode plugin appeared during installation; refusing to overwrite it",
                );
            }
            // SAFETY: remove only our temporary link after successful publication.
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
            }
        } else {
            // SAFETY: both names are relative to the same live, pinned directory.
            if unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    directory.as_raw_fd(),
                    c"libra-hooks.js".as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("failed to replace OpenCode plugin atomically");
            }
        }
        directory
            .sync_all()
            .context("failed to persist OpenCode plugin directory")?;
        snapshot_in_directory(&directory)?
            .context("OpenCode plugin disappeared immediately after installation")
    })();
    if result.is_err() {
        // SAFETY: unlink only our random temporary basename in the pinned directory.
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
        }
    }
    result
}

#[cfg(windows)]
fn write_plugin_file_atomic(
    path: &Path,
    content: &str,
    expected: Option<&PluginSnapshot>,
) -> Result<PluginSnapshot> {
    validate_plugin_paths()?;
    let parent = path
        .parent()
        .context("OpenCode plugin path has no parent")?;
    fs::create_dir_all(parent).context("failed to create OpenCode plugin directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(content.as_bytes())?;
    temporary.as_file().sync_all()?;
    validate_plugin_paths()?;
    verify_snapshot(read_plugin_snapshot(path)?.as_ref(), expected)?;
    if expected.is_none() {
        temporary
            .persist_noclobber(path)
            .context("OpenCode plugin appeared during installation; refusing to overwrite it")?;
    } else {
        temporary
            .persist(path)
            .context("failed to replace OpenCode plugin atomically")?;
    }
    read_plugin_snapshot(path)?.context("OpenCode plugin disappeared after installation")
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

    /// Current template follows the existing export deadline; OG-07 will
    /// subsequently tune the Rust value without a second JS constant.
    #[test]
    fn plugin_timeout_matches_deadline_constant() {
        let rendered = render_opencode_plugin("'/tmp/dir with spaces/libra'").expect("render");
        let expected_ms =
            crate::internal::ai::observed_agents::opencode_export::EXPORT_DEADLINE.as_millis();
        assert!(
            rendered.contains(&format!("const EXPORT_DEADLINE_MS = {expected_ms};")),
            "rendered plugin must pin EXPORT_DEADLINE_MS to {expected_ms} ms"
        );
        assert!(
            rendered.contains("const FORWARD_TIMEOUT_MS = EXPORT_DEADLINE_MS + 15000;"),
            "rendered plugin must derive the forward timeout from the deadline"
        );
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

        // Upgrading replaces the entire stale managed template, not a partial
        // edit that could retain obsolete event forwarding or shell calls.
        fs::write(
            &plugin_path,
            format!("{LIBRA_MANAGED_MARKER}\n// old template\n"),
        )
        .unwrap();
        assert!(!opencode_hooks_are_installed().expect("stale template status"));
        install_opencode_hooks(&install_options(&canonical_binary)).expect("upgrade");
        assert_eq!(fs::read_to_string(&plugin_path).unwrap(), content);

        fs::write(&plugin_path, format!("{content}// edited template\n")).unwrap();
        assert!(!opencode_hooks_are_installed().unwrap());
        install_opencode_hooks(&install_options(&canonical_binary)).unwrap();
        assert!(opencode_hooks_are_installed().unwrap());

        // A user replaces a previously inspected managed leaf before mutation.
        let inspected = read_plugin_snapshot(&plugin_path).unwrap().unwrap();
        fs::remove_file(&plugin_path).unwrap();
        fs::write(&plugin_path, "// concurrent user plugin\n").unwrap();
        assert!(write_plugin_file_atomic(&plugin_path, &content, Some(&inspected)).is_err());
        assert!(remove_plugin_file(&plugin_path, &inspected).is_err());
        assert!(write_plugin_file_atomic(&plugin_path, &content, None).is_err());
        assert_eq!(
            fs::read_to_string(&plugin_path).unwrap(),
            "// concurrent user plugin\n"
        );
        fs::write(&plugin_path, &content).unwrap();

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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Root bypasses mode-based refusal, so it cannot prove this subgate.
            if unsafe { libc::geteuid() } == 0 {
                eprintln!("[skip] OG-02 permission rollback subgate requires a non-root user");
                return;
            }
            let canonical_path = root.join(".opencode/plugin/libra-hooks.js");
            let stale = format!("{LIBRA_MANAGED_MARKER}\n// stale canonical rollback fixture\n");
            fs::write(&canonical_path, &stale).unwrap();
            fs::write(&legacy_path, &managed_content).unwrap();
            fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o500)).unwrap();
            let outcome = install_opencode_hooks(&install_options(&canonical_binary));
            fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o700)).unwrap();
            // This test requires ordinary-user permission enforcement; a root
            // run cannot prove the rollback gate and must not count as such.
            let error = outcome.expect_err("ordinary-user duplicate unlink must fail");
            assert!(format!("{error:#}").contains("rolled back"));
            assert_eq!(fs::read_to_string(&canonical_path).unwrap(), stale);
            assert_eq!(fs::read_to_string(&legacy_path).unwrap(), managed_content);
            install_opencode_hooks(&install_options(&canonical_binary)).unwrap();
            assert!(!legacy_path.exists());
            assert!(opencode_hooks_are_installed().unwrap());
        }

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
        assert!(rendered.contains(r#"const LIBRA_COMMAND = "/tmp/dir with spaces/libra";"#));
    }

    #[cfg(unix)]
    fn run_plugin(
        runtime: &str,
        events: serde_json::Value,
        tail: &str,
        missing_binary: bool,
    ) -> Option<(Vec<serde_json::Value>, std::process::Output)> {
        use std::{
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
        };
        match Command::new(runtime)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) => assert!(status.success(), "OpenCode runtime probe failed: {runtime}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "[skip] OG-02 interpreter unavailable: {runtime}; no runtime proof recorded"
                );
                return None;
            }
            Err(error) => panic!("OpenCode runtime probe failed: {runtime}: {error}"),
        }
        let temp = TempDir::new().unwrap();
        let binary = temp.path().join("libra with ' quotes");
        let log = temp.path().join("frames.jsonl");
        fs::write(&binary, "#!/bin/sh\ncat >> \"$LIBRA_TEST_FORWARD_LOG\"\nprintf '\\n' >> \"$LIBRA_TEST_FORWARD_LOG\"\nprintf '{\"decision\":\"deny\"}'\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let command = format!("'{}'", binary.display().to_string().replace('\'', "'\\''"));
        let plugin = temp.path().join("plugin.mjs");
        fs::write(&plugin, render_opencode_plugin(&command).unwrap()).unwrap();
        if missing_binary {
            fs::remove_file(&binary).unwrap();
        }
        let runner = format!(
            r#"
import plugin from "./plugin.mjs";
const events = {events};
let finish;
const finished = new Promise(resolve => {{ finish = resolve; }});
let tool;
const context = {{
  location: {{ directory: {directory} }},
  tool: {{ hook: async (name, callback) => {{
    if (name !== "execute.after") throw new Error("unexpected hook");
    tool = callback;
    return {{ dispose: async () => {{}} }};
  }} }},
  event: {{ subscribe: () => (async function* () {{
    try {{ for (const event of events) yield event; }} finally {{ finish(); }}
  }})() }},
}};
const cleanup = await plugin.setup(context);
if (cleanup) await finished;
{tail}
"#,
            directory = serde_json::to_string(&temp.path()).unwrap()
        );
        let script = temp.path().join("runner.mjs");
        fs::write(&script, runner).unwrap();
        let output = Command::new(runtime)
            .arg("--no-warnings")
            .arg(&script)
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .env("LIBRA_TEST_FORWARD_LOG", &log)
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "runtime failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let frames = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        println!("[executed] OG-02 actual-template runtime {runtime}");
        Some((frames, output))
    }

    #[cfg(unix)]
    fn event(name: &str, data: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"type":name,"data":data})
    }

    #[cfg(unix)]
    #[test]
    fn disposed_forward_carries_tracked_id() {
        let Some((frames, _)) = run_plugin(
            "node",
            serde_json::json!([
                event(
                    "session.created",
                    serde_json::json!({"sessionID":"synthetic-a"})
                ),
                event("server.instance.disposed", serde_json::json!({})),
            ]),
            "await cleanup?.();",
            false,
        ) else {
            return;
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1]["session_id"], "synthetic-a");
        assert_eq!(frames[1]["hook_event_name"], "server.instance.disposed");
    }

    #[cfg(unix)]
    #[test]
    fn disposed_missing_id_skips_with_warning() {
        let Some((frames, output)) = run_plugin(
            "node",
            serde_json::json!([event(
                "server.instance.disposed",
                serde_json::json!({"secret":"PRIVATE_CANARY"})
            ),]),
            "await cleanup?.();",
            false,
        ) else {
            return;
        };
        assert!(frames.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("disposed_without_session"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_CANARY"));
    }

    #[cfg(unix)]
    #[test]
    fn session_end_resets_tracked_id() {
        let Some((frames, _)) = run_plugin(
            "node",
            serde_json::json!([
                event(
                    "session.created",
                    serde_json::json!({"sessionID":"synthetic-a"})
                ),
                event(
                    "session.deleted",
                    serde_json::json!({"sessionID":"synthetic-a"})
                ),
            ]),
            "await cleanup?.();",
            false,
        ) else {
            return;
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1]["hook_event_name"], "session.deleted");
    }

    #[cfg(unix)]
    #[test]
    fn busy_retry_never_forwarded() {
        let Some((frames, _)) = run_plugin(
            "node",
            serde_json::json!([
                event(
                    "session.created",
                    serde_json::json!({"sessionID":"synthetic-a"})
                ),
                event(
                    "session.status",
                    serde_json::json!({"sessionID":"synthetic-a","status":{"type":"busy"}})
                ),
                event(
                    "session.status",
                    serde_json::json!({"sessionID":"synthetic-a","status":{"type":"retry"}})
                ),
                event(
                    "session.status",
                    serde_json::json!({"sessionID":"synthetic-a","status":{"type":"idle"}})
                ),
            ]),
            "await cleanup?.();",
            false,
        ) else {
            return;
        };
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1]["status"]["type"], "idle");
    }

    #[cfg(unix)]
    #[test]
    fn prompt_truncation_and_unknown_delivery_are_explicit() {
        use serde_json::json;
        for (prompt, expected) in [
            (
                format!("{}PRIVATE-TRUNCATED-SUFFIX", "a".repeat(16384)),
                "a".repeat(16384),
            ),
            (
                format!("{}😀PRIVATE-TRUNCATED-SUFFIX", "a".repeat(16383)),
                "a".repeat(16383),
            ),
        ] {
            let events = json!([
                event(
                    "session.inbox.enqueued",
                    json!({"sessionID":"synthetic-a","inboxID":"user-1","item":{"type":"user","payload":{"text":prompt}}})
                ),
                event(
                    "session.inbox.delivered",
                    json!({"sessionID":"synthetic-a","inboxID":"user-1"})
                ),
                event(
                    "session.inbox.delivered",
                    json!({"sessionID":"synthetic-a","inboxID":"user-1"})
                ),
                event(
                    "message.updated",
                    json!({"info":{"sessionID":"synthetic-a","id":"legacy-1","role":"user"}})
                ),
                event(
                    "message.part.updated",
                    json!({"part":{"sessionID":"synthetic-a","messageID":"legacy-1","type":"text","text":prompt}})
                ),
                event(
                    "session.inbox.enqueued",
                    json!({"sessionID":"synthetic-a","inboxID":"non-user","item":{"type":"synthetic","payload":{"text":"PRIVATE-SYNTHETIC"}}})
                ),
                event(
                    "session.inbox.delivered",
                    json!({"sessionID":"synthetic-a","inboxID":"non-user"})
                ),
                event(
                    "session.inbox.delivered",
                    json!({"sessionID":"unknown-session","inboxID":"PRIVATE-UNKNOWN-DELIVERY"})
                ),
            ]);
            let Some((frames, output)) = run_plugin("node", events, "await cleanup?.();", false)
            else {
                return;
            };
            assert_eq!(
                frames.len(),
                4,
                "only start, two classified prompts, and end"
            );
            for (frame, name) in frames[1..3]
                .iter()
                .zip(["session.inbox.delivered", "message.updated"])
            {
                assert_eq!(frame["hook_event_name"], name);
                assert_eq!(frame["prompt"], expected);
                assert_eq!(frame["prompt_truncated"], true);
            }
            assert!(
                frames
                    .iter()
                    .all(|frame| frame["session_id"] == "synthetic-a")
            );
            assert!(output.stdout.is_empty());
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                "libra opencode: prompt_truncated\nlibra opencode: prompt_delivery_unclassified\n"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn legacy_prompt_without_text_flushes_before_next_event() {
        use serde_json::json;

        let (frames, output) = run_plugin(
            "node",
            json!([
                event(
                    "message.updated",
                    json!({"info":{"sessionID":"synthetic-a","id":"user-1","role":"user"}})
                ),
                event(
                    "session.execution.succeeded",
                    json!({"sessionID":"synthetic-a"})
                ),
            ]),
            "await cleanup?.();",
            false,
        )
        .expect("Node is required for the legacy empty-prompt fallback regression");
        let names: Vec<_> = frames
            .iter()
            .map(|frame| frame["hook_event_name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "session.created",
                "message.updated",
                "session.execution.succeeded",
                "server.instance.disposed",
            ]
        );
        assert_eq!(frames[1]["prompt"], "");
        assert!(output.stdout.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn binary_probe_failure_skips_silently() {
        let Some((frames, output)) = run_plugin(
            "node",
            serde_json::json!([event(
                "session.created",
                serde_json::json!({"sessionID":"synthetic-a"})
            ),]),
            "await cleanup?.();",
            true,
        ) else {
            return;
        };
        assert!(frames.is_empty());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn plugin_stdout_has_no_control_output() {
        let Some((_, output)) = run_plugin(
            "node",
            serde_json::json!([event(
                "session.created",
                serde_json::json!({"sessionID":"synthetic-a"})
            ),]),
            "await cleanup?.();",
            false,
        ) else {
            return;
        };
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    #[cfg(unix)]
    fn assert_flush(tail: &str) {
        let Some((frames, _)) = run_plugin(
            "node",
            serde_json::json!([
                event(
                    "session.created",
                    serde_json::json!({"sessionID":"synthetic-a"})
                ),
                event(
                    "session.created",
                    serde_json::json!({"sessionID":"synthetic-b"})
                ),
            ]),
            tail,
            false,
        ) else {
            return;
        };
        assert_eq!(frames.len(), 4);
        for (index, id) in [(2, "synthetic-a"), (3, "synthetic-b")] {
            assert_eq!(frames[index]["session_id"], id);
            assert_eq!(frames[index]["hook_event_name"], "server.instance.disposed");
        }
    }

    #[cfg(unix)]
    #[test]
    fn exit_flush_forwards_all_tracked_sessions() {
        assert_flush("process.exit(0);");
    }

    #[cfg(unix)]
    #[test]
    fn dispose_hook_flushes_all_tracked_sessions() {
        assert_flush("await cleanup?.(); await cleanup?.();");
    }

    #[cfg(unix)]
    #[test]
    fn exit_flush_without_tracked_sessions_noop() {
        let Some((frames, _)) =
            run_plugin("node", serde_json::json!([]), "process.exit(0);", false)
        else {
            return;
        };
        assert!(frames.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn reload_closes_then_reopens_same_session_without_fabricating_prompt() {
        use serde_json::json;
        let Some((frames, output)) = run_plugin(
            "node",
            json!([
                event("session.created", json!({"sessionID":"synthetic-reload"})),
                event(
                    "session.inbox.enqueued",
                    json!({"sessionID":"synthetic-reload","inboxID":"user-1","item":{"type":"user","payload":{"text":"Synthetic prompt"}}})
                ),
                event(
                    "session.inbox.delivered",
                    json!({"sessionID":"synthetic-reload","inboxID":"user-1"})
                ),
                event(
                    "session.execution.interrupted",
                    json!({"sessionID":"synthetic-reload","reason":"shutdown"})
                ),
            ]),
            r#"
await cleanup();
context.event.subscribe = () => (async function* () {
  yield { type: "session.step.started", data: { sessionID: "synthetic-reload", model: { id: "synthetic-model" } } };
  yield { type: "session.execution.succeeded", data: { sessionID: "synthetic-reload" } };
})();
const reloaded = await plugin.setup(context);
await new Promise(resolve => setImmediate(resolve));
await reloaded();
"#,
            false,
        ) else {
            return;
        };
        let names: Vec<_> = frames
            .iter()
            .map(|frame| frame["hook_event_name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "session.created",
                "session.inbox.delivered",
                "server.instance.disposed",
                "session.created",
                "session.execution.succeeded",
                "server.instance.disposed"
            ]
        );
        assert!(
            frames
                .iter()
                .all(|frame| frame["session_id"] == "synthetic-reload")
        );
        assert_eq!(frames[4]["model"], "synthetic-model");
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn exit_handler_singleton_no_duplicate_flush() {
        assert_flush(
            r#"
const before = process.listenerCount("exit");
const second = await plugin.setup(context);
await new Promise(resolve => setImmediate(resolve));
if (before !== process.listenerCount("exit")) throw new Error("exit listener leaked");
await second();
await cleanup();
"#,
        );
    }

    #[test]
    fn pure_blindspot_disclosed() {
        assert!(OPENCODE_PLUGIN_TEMPLATE.contains("--pure / OPENCODE_PURE=1"));
    }

    #[cfg(unix)]
    #[test]
    fn plugin_leaf_fifo_replacement_is_nonblocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};
        let temp = TempDir::new().unwrap();
        let leaf = temp.path().join(OPENCODE_PLUGIN_FILE);
        fs::write(&leaf, LIBRA_MANAGED_MARKER).unwrap();
        let directory = fs::File::open(temp.path()).unwrap();
        assert!(snapshot_in_directory(&directory).unwrap().is_some());
        fs::remove_file(&leaf).unwrap();
        let name = CString::new(leaf.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            send.send(snapshot_in_directory(&directory).is_err())
                .unwrap();
        });
        assert!(
            receive
                .recv_timeout(Duration::from_secs(1))
                .expect("FIFO replacement must not block open")
        );
        reader.join().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn legacy_prompt_forwarding_supplies_explicit_string() {
        let (frames, output) = run_plugin("node", serde_json::json!([
            event("message.updated", serde_json::json!({"info":{"sessionID":"synthetic-user","id":"message-1","role":"user"}})),
            event("session.execution.succeeded", serde_json::json!({"sessionID":"synthetic-user"})),
        ]), "await cleanup?.();", false).expect("Node required for the local producer wire gate");
        assert_eq!(frames[1]["hook_event_name"], "message.updated");
        assert_eq!(frames[1]["role"], "user");
        assert_eq!(frames[1]["prompt"], "");
        assert_eq!(frames.len(), 4);
        assert!(output.stdout.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn legacy_text_before_message_is_paired_once() {
        use serde_json::json;
        let (frames, output) = run_plugin("node", json!([
            event("message.part.updated",json!({"part":{"sessionID":"synthetic-first","messageID":"u1","type":"text","text":"first text"}})),
            event("message.updated",json!({"info":{"sessionID":"synthetic-first","id":"u1","role":"user"}})),
            event("message.updated",json!({"info":{"sessionID":"synthetic-first","id":"u1","role":"user"}})),
            event("message.part.updated",json!({"part":{"sessionID":"synthetic-first","messageID":"u1","type":"text","text":"duplicate"}})),
        ]), "await cleanup?.();", false).expect("Node required for local pairing gate");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1]["prompt"], "first text");
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_flushes_last_legacy_empty_prompt_before_end() {
        let (frames, output) = run_plugin(
            "node",
            serde_json::json!([event(
                "message.updated",
                serde_json::json!({"info":{"sessionID":"synthetic-last","id":"u1","role":"user"}})
            ),]),
            "await cleanup?.();",
            false,
        )
        .expect("Node required for cleanup ordering gate");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1]["hook_event_name"], "message.updated");
        assert_eq!(frames[1]["prompt"], "");
        assert_eq!(frames[2]["hook_event_name"], "server.instance.disposed");
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn config_device_type_is_rejected_without_opening_it() {
        let kind = fs::symlink_metadata("/dev/null")
            .expect("real device metadata")
            .file_type();
        assert!(!kind.is_file() && !kind.is_dir());
        for directory in [true, false] {
            let error = validate_config_slot(".opencode/plugin/libra-hooks.js", directory, kind)
                .unwrap_err();
            assert!(format!("{error:#}").contains("FIFO/socket/device"));
        }
    }
    #[cfg(unix)]
    #[test]
    fn history_rollover_keeps_new_prompts_and_retires_ended_sessions() {
        use serde_json::json;
        let mut events = vec![event(
            "session.created",
            json!({"sessionID":"synthetic-long"}),
        )];
        for n in 0..260 {
            let message = format!("u{n}");
            events.push(event("session.inbox.enqueued",json!({"sessionID":"synthetic-long","inboxID":message,"item":{"type":"user","payload":{"text":"Synthetic"}}})));
            events.push(event(
                "session.inbox.delivered",
                json!({"sessionID":"synthetic-long","inboxID":message}),
            ));
        }
        events.push(event(
            "session.inbox.delivered",
            json!({"sessionID":"synthetic-long","inboxID":"u259"}),
        ));
        events.push(event(
            "session.deleted",
            json!({"sessionID":"synthetic-long"}),
        ));
        events.push(event(
            "session.created",
            json!({"sessionID":"synthetic-long"}),
        ));
        events.push(event("session.inbox.enqueued", json!({"sessionID":"synthetic-long","inboxID":"u259","item":{"type":"user","payload":{"text":"New session history"}}})));
        events.push(event(
            "session.inbox.delivered",
            json!({"sessionID":"synthetic-long","inboxID":"u259"}),
        ));
        let (frames, output) = run_plugin("node", json!(events), "await cleanup?.();", false)
            .expect("Node required for live history regression");
        assert_eq!(frames.len(), 265);
        assert_eq!(frames[260]["message_id"], "u259");
        assert_eq!(frames[261]["hook_event_name"], "session.deleted");
        assert_eq!(frames[263]["prompt"], "New session history");
        assert_eq!(frames[264]["hook_event_name"], "server.instance.disposed");
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn lifecycle_subscription_survives_tool_registration_failure() {
        let (frames, output)=run_plugin("node",serde_json::json!([]),r#"
await cleanup();
context.tool.hook=async()=>{throw new Error("PRIVATE-REGISTRATION-ERROR");};
context.event.subscribe=()=> (async function*(){yield {type:"session.created",data:{sessionID:"synthetic-no-tool"}};})();
const next=await plugin.setup(context);
await new Promise(resolve=>setImmediate(resolve));
await next();
"#,false).expect("Node required for tool-registration fallback gate");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["hook_event_name"], "session.created");
        assert_eq!(frames[1]["hook_event_name"], "server.instance.disposed");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "libra opencode: tool_registration_unavailable\n"
        );
        assert!(output.stdout.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn session_capacity_rolls_across_instances_and_preserves_recent_sessions() {
        use serde_json::json;
        let mut events = Vec::new();
        for n in 0..64 {
            events.push(event(
                "session.created",
                json!({"sessionID":format!("old-{n}")}),
            ));
        }
        events.push(event(
            "session.step.started",
            json!({"sessionID":"old-0","model":{"id":"synthetic-model"}}),
        ));
        let (frames, output) = run_plugin("node", json!(events), r#"
const extra = {...context, event: {subscribe: () => (async function*(){
  for (let n=0;n<2;n++) yield {type:"session.created",data:{sessionID:"new-"+n}};
  yield {type:"session.inbox.enqueued",data:{sessionID:"new-1",inboxID:"new-prompt",item:{type:"user",payload:{text:"New prompt after capacity"}}}};
  yield {type:"session.inbox.delivered",data:{sessionID:"new-1",inboxID:"new-prompt"}};
})()}};
const extraCleanup = await plugin.setup(extra);
await new Promise(resolve => setImmediate(resolve));
await extraCleanup();
await cleanup();
"#, false).expect("Node required for process-wide session-capacity regression");
        let starts: Vec<_> = frames
            .iter()
            .filter(|f| f["hook_event_name"] == "session.created")
            .collect();
        let ends: Vec<_> = frames
            .iter()
            .filter(|f| f["hook_event_name"] == "server.instance.disposed")
            .collect();
        assert_eq!(starts.len(), 66);
        assert_eq!(ends.len(), 66);
        assert_eq!(ends[0]["session_id"], "old-1");
        assert_eq!(ends[1]["session_id"], "old-2");
        assert_eq!(
            frames
                .iter()
                .filter(|f| f["prompt"] == "New prompt after capacity")
                .count(),
            1
        );
        for start in starts {
            assert_eq!(
                ends.iter()
                    .filter(|f| f["session_id"] == start["session_id"])
                    .count(),
                1
            );
        }
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "libra opencode: session_capacity_exceeded\n"
        );
    }
    #[cfg(unix)]
    #[test]
    fn early_text_capacity_rolls_without_disabling_later_user_prompts() {
        use serde_json::json;
        let mut events = Vec::new();
        for n in 0..130 {
            events.push(event("message.part.updated", json!({"part":{"type":"text","sessionID":"legacy-session","messageID":format!("unclassified-{n}"),"text":"Synthetic early text"}})));
        }
        events.push(event(
            "message.updated",
            json!({"info":{"id":"unclassified-129","sessionID":"legacy-session","role":"user"}}),
        ));
        let (frames, output) = run_plugin("node", json!(events), "await cleanup();", false)
            .expect("Node required for early-text rollover regression");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1]["message_id"], "unclassified-129");
        assert_eq!(frames[1]["prompt"], "Synthetic early text");
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "libra opencode: prompt_capacity_exceeded\n"
        );
    }
}
