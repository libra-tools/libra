//! Immutable public identities for reviewed private producer artifacts.
//! A catalog match grants no issuer trust or native-field/archive authority.

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::codec::{self, Policy, Protocol};

/// Public reproducibility evidence. No installation paths or issuer keys belong here.
#[derive(Debug, Serialize)]
pub struct ArtifactIdentity {
    pub producer_version: &'static str,
    pub build_identity: &'static str,
    pub target: &'static str,
    pub binary_sha256: &'static str,
    pub binary_bytes: u64,
    pub official_base_commit: &'static str,
    pub source_archive_sha256: &'static str,
    pub source_manifest_sha256: &'static str,
    pub patch_sha256: &'static str,
    pub patch_manifest_sha256: &'static str,
    pub lock_sha256: &'static str,
    pub build_inputs_sha256: &'static str,
    pub bun_version: &'static str,
    pub bun_binary_sha256: &'static str,
    pub review_raw_sha256: &'static str,
}

/// A closed source-derived endpoint. A receipt cannot add or normalize endpoints.
#[derive(Debug, Serialize)]
pub struct ApprovedEndpoint {
    pub version: u32,
    pub protocol: Protocol,
    pub url: &'static str,
    pub sha256: &'static str,
}

/// Constructed only by this module's immutable catalog. Matching it is not registration.
#[derive(Debug, Serialize)]
pub struct ApprovedBuild {
    identity: ArtifactIdentity,
    protocols: &'static [Protocol],
    endpoints: &'static [ApprovedEndpoint],
    builtin_set_digest: &'static str,
    libra_template: &'static str,
    libra_template_digest: &'static str,
    event_commands_json: &'static str,
    event_map_digest: &'static str,
    export_deadline_millis: u32,
    transport_policy: &'static str,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApprovedBuildError {
    #[error(
        "OpenCode producer build is not reviewed; use the separately installed approved producer and keep this session partial"
    )]
    Unknown,
    #[error(
        "Libra observer command path must be an absolute canonical path without control characters or template slots; register the actual executable path"
    )]
    CommandPath,
    #[error(
        "Loaded OpenCode observer does not match the private producer's legacy recipe; use the reviewed matching historical observer only in an isolated single-repository setup; ordinary agent enable replaces it and cannot repair legacy matching"
    )]
    Observer,
    #[error(
        "Reviewed OpenCode build constants are inconsistent; keep this session partial and install a corrected reviewed release"
    )]
    CompiledPolicy,
    #[error("Cannot construct the native-origin installation policy: {0}")]
    Codec(#[from] codec::CodecError),
}

const SLOTS: [&str; 3] = [
    "__LIBRA_COMMAND_JSON__",
    "__EVENT_COMMANDS_JSON__",
    "__EXPORT_DEADLINE_MS__",
];
// Immutable recipe for the reviewed .1/.2 binaries; do not follow the ordinary default.
// Legacy v1 has no per-directory filtering: use only an isolated single-repository setup.
const LEGACY_V1_TEMPLATE: &str = r####"// libra-managed: do not edit — installed by libra agent enable (AG-19)
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
"####;
const EVENT_COMMANDS_JSON: &str = r####"{"message.updated":"prompt","server.instance.disposed":"session-end","session.compacted":"compaction","session.compaction.ended":"compaction","session.created":"session-start","session.deleted":"session-end","session.execution.failed":"stop","session.execution.interrupted":"stop","session.execution.succeeded":"stop","session.inbox.delivered":"prompt","session.status":"stop","tool.execute.after":"tool-use"}"####;
static REVIEWED: ApprovedBuild = ApprovedBuild {
    identity: ArtifactIdentity {
        producer_version: "2.0.26-libra-native.1",
        build_identity: "bac21cc4027896b8d1836133c347d20f",
        target: "darwin-arm64",
        binary_sha256: "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196",
        binary_bytes: 187635570,
        official_base_commit: "9b4ec5714d481559990db0a816d5dec19541a814",
        source_archive_sha256: "3602d20e47d9a98c1cc8e140f686490e42717cb7f7f6ac4d9146572297125aeb",
        source_manifest_sha256: "bdf271ed3217732bf70f21d310513c75867b4c84f73532590de54bea11e90a0f",
        patch_sha256: "d18a523dbf0c121084ac4e791b3edda266319c5fa4645839433e831375670664",
        patch_manifest_sha256: "61c95d1901201bcad6f9454104c8cd1deacd801f90d566892ceea91606122bf5",
        lock_sha256: "5dbf5d6003729a1e5f04eaec5ac66ef25993a83191a9e0438b9ed0ecfcf909fc",
        build_inputs_sha256: "98adee66a5a3e1c4edbee76d28f97374caf54d3ee4ecf37730bdd4822452b3bb",
        bun_version: "1.4.2",
        bun_binary_sha256: "35d20dd0263e5c950194434b925454fdfa9ba6e4467da960410fa05b08a7a5b5",
        review_raw_sha256: "0d388a824379310ba037ace230e5e4095ef8ff868d7b56cfbf3c417cd6e8bfa3",
    },
    protocols: &[Protocol::OpenaiResponses],
    endpoints: &[
        ApprovedEndpoint {
            version: 1,
            protocol: Protocol::OpenaiResponses,
            url: "https://ark.cn-beijing.volces.com/api/plan/v3/responses",
            sha256: "061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531",
        },
        ApprovedEndpoint {
            version: 1,
            protocol: Protocol::OpenaiResponses,
            url: "https://opencode.ai/zen/v1/responses",
            sha256: "253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3",
        },
    ],
    builtin_set_digest: "137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82",
    libra_template: LEGACY_V1_TEMPLATE,
    libra_template_digest: "9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d",
    event_commands_json: EVENT_COMMANDS_JSON,
    event_map_digest: "3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe",
    export_deadline_millis: 30000,
    transport_policy: "compiled-native-http-only-v1",
};

// Candidate `.2` identity is pinned to the independently measured private
// binary and the reviewed R4 source closure. Catalog membership alone never
// registers an issuer, enables capture, or grants artifact authority.
static REVIEWED_ANTHROPIC: ApprovedBuild = ApprovedBuild {
    identity: ArtifactIdentity {
        producer_version: codec::ANTHROPIC_PRODUCER_VERSION,
        build_identity: "78c1f19e9249da8d0143a079f90aec9a",
        target: "darwin-arm64",
        binary_sha256: "28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c",
        binary_bytes: 187635570,
        official_base_commit: "9b4ec5714d481559990db0a816d5dec19541a814",
        source_archive_sha256: "70d550c2371c40da2003de98dc12ea88e7d3de064d618c75a8aff72d22ae2ded",
        source_manifest_sha256: "549ab9e8159c18ea25f5c85348b3df458e464527aec83cdcd86a7ea97638506c",
        patch_sha256: "d14ae6dbda0c6c9859acbf290fdcb5842bf23fb1270befe07c4ee22240658ec5",
        patch_manifest_sha256: "1ac2a777337c76609ffa737d4ee8add3fd67f6bfc3ff6a1c08053be5cf038556",
        lock_sha256: "5dbf5d6003729a1e5f04eaec5ac66ef25993a83191a9e0438b9ed0ecfcf909fc",
        build_inputs_sha256: "b20fdb22e57e60b7d40a5eb0881d937a24ca9fa9dceb164e3014f51345366426",
        bun_version: "1.4.2",
        bun_binary_sha256: "35d20dd0263e5c950194434b925454fdfa9ba6e4467da960410fa05b08a7a5b5",
        review_raw_sha256: "9ce650ffbd6e4ba147434d4dbff1083829ef8f4d32f3eeffcb91720197373ed7",
    },
    protocols: &[Protocol::AnthropicMessages, Protocol::OpenaiResponses],
    endpoints: &[
        ApprovedEndpoint {
            version: 1,
            protocol: Protocol::OpenaiResponses,
            url: "https://ark.cn-beijing.volces.com/api/plan/v3/responses",
            sha256: "061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531",
        },
        ApprovedEndpoint {
            version: 1,
            protocol: Protocol::OpenaiResponses,
            url: "https://opencode.ai/zen/v1/responses",
            sha256: "253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3",
        },
        ApprovedEndpoint {
            version: 2,
            protocol: Protocol::AnthropicMessages,
            url: codec::ANTHROPIC_ENDPOINT,
            sha256: "4d87d389c2853ef2ee92e297146b3576d59c343900f70d099f6ed977573bcae0",
        },
    ],
    builtin_set_digest: "137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82",
    libra_template: LEGACY_V1_TEMPLATE,
    libra_template_digest: "9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d",
    event_commands_json: EVENT_COMMANDS_JSON,
    event_map_digest: "3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe",
    export_deadline_millis: 30000,
    transport_policy: "compiled-native-http-only-v1",
};

/// Exact three-part lookup. Versions, nonces and hashes copied from an export do
/// not establish binary provenance; the registration caller must measure the
/// currently trusted producer and independently validate the owned descriptor.
pub fn lookup(
    version: &str,
    build_identity: &str,
    measured_binary_sha256: &str,
) -> Result<&'static ApprovedBuild, ApprovedBuildError> {
    let build = select_measured(build_identity, measured_binary_sha256)?;
    if version != build.identity.producer_version {
        return Err(ApprovedBuildError::Unknown);
    }
    Ok(build)
}

/// Choose only from controlled measured bytes and the installation's build
/// identity. Neither the challenge's claimed version nor export data selects
/// which producer receives the private key-directory path.
pub fn select_measured(
    build_identity: &str,
    measured_binary_sha256: &str,
) -> Result<&'static ApprovedBuild, ApprovedBuildError> {
    [&REVIEWED, &REVIEWED_ANTHROPIC]
        .into_iter()
        .find(|build| {
            build.identity.build_identity == build_identity
                && build.identity.binary_sha256 == measured_binary_sha256
        })
        .ok_or(ApprovedBuildError::Unknown)
}

impl ApprovedBuild {
    pub fn identity(&self) -> &ArtifactIdentity {
        &self.identity
    }

    pub fn endpoint(&self, protocol: Protocol, prepared_url: &str) -> Option<&ApprovedEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == protocol && endpoint.url == prepared_url)
    }

    pub fn matches_endpoint_pair(&self, protocol: Protocol, digest: &str) -> bool {
        self.endpoints
            .iter()
            .any(|endpoint| endpoint.protocol == protocol && endpoint.sha256 == digest)
    }

    /// Pure rendering of the frozen public template. No filesystem, environment
    /// or receipt is consulted; the caller must supply its controlled canonical
    /// executable path. This does not approve a self-reported path.
    pub fn render_observer(
        &self,
        controlled_command_path: &str,
    ) -> Result<String, ApprovedBuildError> {
        if !controlled_command_path.starts_with('/')
            || controlled_command_path.len() > 4096
            || controlled_command_path.chars().any(char::is_control)
            || controlled_command_path
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == "..")
            || SLOTS
                .iter()
                .any(|slot| controlled_command_path.contains(slot))
        {
            return Err(ApprovedBuildError::CommandPath);
        }
        if SLOTS
            .iter()
            .any(|slot| self.libra_template.matches(slot).count() != 1)
            || hex::encode(Sha256::digest(self.libra_template.as_bytes()))
                != self.libra_template_digest
        {
            return Err(ApprovedBuildError::CompiledPolicy);
        }
        let literal = serde_json::to_string(controlled_command_path)
            .map_err(|_| ApprovedBuildError::CommandPath)?;
        let rendered = self
            .libra_template
            .replace(SLOTS[0], &literal)
            .replace(SLOTS[1], self.event_commands_json)
            .replace(SLOTS[2], &self.export_deadline_millis.to_string());
        if rendered.len() > codec::DESCRIPTOR_BYTE_CAP
            || SLOTS.iter().any(|slot| rendered.contains(slot))
        {
            return Err(ApprovedBuildError::CompiledPolicy);
        }
        Ok(rendered)
    }

    /// Build an untrusted wire candidate for controlled registration. The caller
    /// owns actual path and loaded-byte provenance; this helper performs no I/O,
    /// registers no issuer and creates no NativeProven or reasoning artifact.
    pub fn installation_policy(
        &self,
        controlled_command_path: &str,
        measured_observer_bytes: &[u8],
    ) -> Result<Policy, ApprovedBuildError> {
        if measured_observer_bytes.len() > codec::DESCRIPTOR_BYTE_CAP {
            return Err(ApprovedBuildError::Observer);
        }
        let rendered = self.render_observer(controlled_command_path)?;
        if measured_observer_bytes != rendered.as_bytes() {
            return Err(ApprovedBuildError::Observer);
        }
        for endpoint in self.endpoints {
            let descriptor = codec::Endpoint {
                version: endpoint.version,
                protocol: endpoint.protocol,
                url: endpoint.url.to_owned(),
            };
            let bytes = codec::endpoint_bytes(&descriptor)
                .map_err(|_| ApprovedBuildError::CompiledPolicy)?;
            if hex::encode(Sha256::digest(bytes)) != endpoint.sha256 {
                return Err(ApprovedBuildError::CompiledPolicy);
            }
        }
        let mut endpoint_protocols = self
            .endpoints
            .iter()
            .map(|endpoint| endpoint.protocol)
            .collect::<Vec<_>>();
        endpoint_protocols.sort_unstable();
        endpoint_protocols.dedup();
        if endpoint_protocols.as_slice() != self.protocols {
            return Err(ApprovedBuildError::CompiledPolicy);
        }
        let policy = Policy {
            version: 1,
            build_identity: self.identity.build_identity.to_owned(),
            protocols: self.protocols.to_vec(),
            endpoint_digests: self
                .endpoints
                .iter()
                .map(|endpoint| endpoint.sha256.to_owned())
                .collect(),
            builtin_set_digest: self.builtin_set_digest.to_owned(),
            libra_template_digest: self.libra_template_digest.to_owned(),
            libra_rendered_digest: hex::encode(Sha256::digest(rendered.as_bytes())),
            libra_command_path: controlled_command_path.to_owned(),
            event_map_digest: self.event_map_digest.to_owned(),
            export_deadline_millis: self.export_deadline_millis,
            transport_policy: self.transport_policy.to_owned(),
        };
        codec::policy_bytes(&policy)?;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::{super::codec::Endpoint, *};

    const ACCEPTED_ARTIFACT: &str = r####"{
  "accepted_utc": "2026-10-11T08:18:00.318020+00:00",
  "card": "NPR-08",
  "producer_version": "2.0.26-libra-native.1",
  "build_identity": "bac21cc4027896b8d1836133c347d20f",
  "target": "darwin-arm64",
  "official_base_commit": "9b4ec5714d481559990db0a816d5dec19541a814",
  "binary_sha256": "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196",
  "binary_bytes": 187635570,
  "source_archive_sha256": "3602d20e47d9a98c1cc8e140f686490e42717cb7f7f6ac4d9146572297125aeb",
  "source_manifest_sha256": "bdf271ed3217732bf70f21d310513c75867b4c84f73532590de54bea11e90a0f",
  "patch_sha256": "d18a523dbf0c121084ac4e791b3edda266319c5fa4645839433e831375670664",
  "patch_manifest_sha256": "61c95d1901201bcad6f9454104c8cd1deacd801f90d566892ceea91606122bf5",
  "lock_sha256": "5dbf5d6003729a1e5f04eaec5ac66ef25993a83191a9e0438b9ed0ecfcf909fc",
  "build_inputs_sha256": "98adee66a5a3e1c4edbee76d28f97374caf54d3ee4ecf37730bdd4822452b3bb",
  "bun_version": "1.4.2",
  "bun_binary_sha256": "35d20dd0263e5c950194434b925454fdfa9ba6e4467da960410fa05b08a7a5b5",
  "review": "actual Claude Code single PASS",
  "review_raw_sha256": "0d388a824379310ba037ace230e5e4095ef8ff868d7b56cfbf3c417cd6e8bfa3",
  "review_turns": 98,
  "private_acceptance": "PASS",
  "full_T2_pass": 9019,
  "full_T2_fail": 0,
  "full_T2_skip": 86,
  "model_session_attempts": 3,
  "successful_model_sessions": 0,
  "native_field_coverage": "PARTIAL",
  "SourceA": "UNPROVEN",
  "native_GO": false,
  "public_family_CD": "PENDING NPR-20",
  "private_install_paths_rendered_policy_issuer_or_keys": "not included"
}
"####;
    const INDEPENDENT_VECTORS: &str = r####"{
  "prepared_utc": "2026-10-11T08:09:17.512486+00:00",
  "status": "independent Python fixture preparation only; Rust NPR15 implementation/tests UNRUN",
  "SourceA": "UNPROVEN",
  "rows": [
    {
      "policy": {
        "version": 1,
        "buildIdentity": "bac21cc4027896b8d1836133c347d20f",
        "protocols": [
          "openai-responses"
        ],
        "endpointDigests": [
          "061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531",
          "253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3"
        ],
        "builtinSetDigest": "137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82",
        "libraTemplateDigest": "9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d",
        "libraRenderedDigest": "e116b0f36e73bc9b0201e3ed0e65743205193233d46f4077a5ffa8a008d6ec6b",
        "libraCommandPath": "/Applications/Libra.app/Contents/MacOS/libra",
        "eventMapDigest": "3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe",
        "exportDeadlineMillis": 30000,
        "transportPolicy": "compiled-native-http-only-v1"
      },
      "policy_bytes_sha256": "92ca0b7127e92865cfee1ad34b82c4deff63c18cfb8e80514311820ccd067fa5",
      "policy_preimage_hex": "6c696272612d6f70656e636f64652d6e61746976652d706f6c6963792d76310000000001bac21cc4027896b8d1836133c347d20f00000001000000106f70656e61692d726573706f6e73657300000002061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e829c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27de116b0f36e73bc9b0201e3ed0e65743205193233d46f4077a5ffa8a008d6ec6b0000002c2f4170706c69636174696f6e732f4c696272612e6170702f436f6e74656e74732f4d61634f532f6c696272613fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe000075300000001c636f6d70696c65642d6e61746976652d687474702d6f6e6c792d7631",
      "rendered_utf8_bytes": 13993
    },
    {
      "policy": {
        "version": 1,
        "buildIdentity": "bac21cc4027896b8d1836133c347d20f",
        "protocols": [
          "openai-responses"
        ],
        "endpointDigests": [
          "061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531",
          "253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3"
        ],
        "builtinSetDigest": "137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82",
        "libraTemplateDigest": "9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d",
        "libraRenderedDigest": "be742723a06b5b26357d5c4a935eca2ee9caf0e377596afcac7605b9cd9938ac",
        "libraCommandPath": "/opt/homebrew/bin/libra",
        "eventMapDigest": "3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe",
        "exportDeadlineMillis": 30000,
        "transportPolicy": "compiled-native-http-only-v1"
      },
      "policy_bytes_sha256": "368d6c66e9c605a3989ba815aeb638e78d33834f73dd7192d62337274b1d5753",
      "policy_preimage_hex": "6c696272612d6f70656e636f64652d6e61746976652d706f6c6963792d76310000000001bac21cc4027896b8d1836133c347d20f00000001000000106f70656e61692d726573706f6e73657300000002061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e829c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27dbe742723a06b5b26357d5c4a935eca2ee9caf0e377596afcac7605b9cd9938ac000000172f6f70742f686f6d65627265772f62696e2f6c696272613fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe000075300000001c636f6d70696c65642d6e61746976652d687474702d6f6e6c792d7631",
      "rendered_utf8_bytes": 13972
    }
  ]
}
"####;

    fn reviewed() -> &'static ApprovedBuild {
        lookup(
            "2.0.26-libra-native.1",
            "bac21cc4027896b8d1836133c347d20f",
            "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196",
        )
        .unwrap()
    }

    #[test]
    fn anthropic_build_requires_measured_identity_and_exact_endpoint_pairs() {
        let build = select_measured(
            "78c1f19e9249da8d0143a079f90aec9a",
            "28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c",
        )
        .unwrap();
        assert_eq!(
            build.identity().producer_version,
            codec::ANTHROPIC_PRODUCER_VERSION
        );
        assert_eq!(build.identity().binary_bytes, 187635570);
        let candidate: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/agent_transcripts/opencode/native-origin-build-anthropic.json"
        ))
        .unwrap();
        let pinned = serde_json::to_value(build.identity()).unwrap();
        for (name, value) in pinned.as_object().unwrap() {
            assert_eq!(value, &candidate[name], "candidate identity member {name}");
        }
        assert_eq!(candidate["native_source_a_go"], false);
        assert!(
            lookup(
                codec::PRODUCER_VERSION,
                build.identity().build_identity,
                build.identity().binary_sha256,
            )
            .is_err()
        );
        assert!(
            select_measured(
                reviewed().identity().build_identity,
                build.identity().binary_sha256,
            )
            .is_err()
        );
        assert!(
            select_measured(
                build.identity().build_identity,
                reviewed().identity().binary_sha256,
            )
            .is_err()
        );

        let path = "/Applications/Libra.app/Contents/MacOS/libra";
        let rendered = build.render_observer(path).unwrap();
        let policy = build
            .installation_policy(path, rendered.as_bytes())
            .unwrap();
        assert_eq!(
            policy.protocols,
            vec![Protocol::AnthropicMessages, Protocol::OpenaiResponses]
        );
        assert_eq!(
            policy.endpoint_digests,
            vec![
                "061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531",
                "253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3",
                "4d87d389c2853ef2ee92e297146b3576d59c343900f70d099f6ed977573bcae0",
            ]
        );
        for endpoint in build.endpoints {
            let descriptor = Endpoint {
                version: endpoint.version,
                protocol: endpoint.protocol,
                url: endpoint.url.into(),
            };
            let bytes = codec::endpoint_bytes(&descriptor).unwrap();
            assert_eq!(hex::encode(Sha256::digest(bytes)), endpoint.sha256);
            assert!(build.matches_endpoint_pair(endpoint.protocol, endpoint.sha256));
        }
        assert!(
            !build.matches_endpoint_pair(Protocol::AnthropicMessages, build.endpoints[0].sha256,)
        );
        assert!(
            !build.matches_endpoint_pair(Protocol::OpenaiResponses, build.endpoints[2].sha256,)
        );
        assert!(
            !reviewed()
                .matches_endpoint_pair(Protocol::AnthropicMessages, build.endpoints[2].sha256,)
        );
    }

    #[test]
    fn npr_15_approved_identity() {
        let accepted: serde_json::Value = serde_json::from_str(ACCEPTED_ARTIFACT).unwrap();
        let actual = serde_json::to_value(reviewed().identity()).unwrap();
        for name in [
            "producer_version",
            "build_identity",
            "target",
            "binary_bytes",
            "official_base_commit",
            "review_raw_sha256",
        ] {
            assert_eq!(
                actual[name], accepted[name],
                "frozen actual artifact member {name}"
            );
        }
        assert_eq!(
            reviewed().identity().producer_version,
            codec::PRODUCER_VERSION
        );
        assert_eq!(reviewed().identity().target, "darwin-arm64");
    }

    #[test]
    fn npr_15_binary_sha() {
        let accepted: serde_json::Value = serde_json::from_str(ACCEPTED_ARTIFACT).unwrap();
        assert_eq!(
            reviewed().identity().binary_sha256,
            accepted["binary_sha256"]
        );
        assert!(
            lookup(
                reviewed().identity().producer_version,
                reviewed().identity().build_identity,
                &"0".repeat(64)
            )
            .is_err()
        );
        assert!(
            lookup(
                reviewed().identity().producer_version,
                reviewed().identity().build_identity,
                &reviewed().identity().binary_sha256.to_uppercase()
            )
            .is_err()
        );
    }

    #[test]
    fn npr_15_source_sha() {
        let accepted: serde_json::Value = serde_json::from_str(ACCEPTED_ARTIFACT).unwrap();
        let actual = serde_json::to_value(reviewed().identity()).unwrap();
        for name in [
            "source_archive_sha256",
            "source_manifest_sha256",
            "patch_sha256",
            "patch_manifest_sha256",
            "lock_sha256",
            "build_inputs_sha256",
            "bun_version",
            "bun_binary_sha256",
        ] {
            assert_eq!(
                actual[name], accepted[name],
                "frozen source/runtime member {name}"
            );
        }
    }

    #[test]
    fn npr_15_unknown() {
        let identity = reviewed().identity();
        for version in [
            "2.0.26",
            "2.0.26-libra-native.2",
            "",
            "FORGED_SECRET_ARGUMENT",
        ] {
            assert_eq!(
                lookup(version, identity.build_identity, identity.binary_sha256).unwrap_err(),
                ApprovedBuildError::Unknown
            );
        }
        for nonce in [
            "",
            "00000000000000000000000000000000",
            "BAC21CC4027896B8D1836133C347D20F",
        ] {
            assert!(lookup(identity.producer_version, nonce, identity.binary_sha256).is_err());
        }
        assert_eq!(
            ApprovedBuildError::Unknown.to_string(),
            "OpenCode producer build is not reviewed; use the separately installed approved producer and keep this session partial"
        );
        assert_eq!(
            ApprovedBuildError::CommandPath.to_string(),
            "Libra observer command path must be an absolute canonical path without control characters or template slots; register the actual executable path"
        );
        assert_eq!(
            ApprovedBuildError::Observer.to_string(),
            "Loaded OpenCode observer does not match the private producer's legacy recipe; use the reviewed matching historical observer only in an isolated single-repository setup; ordinary agent enable replaces it and cannot repair legacy matching"
        );
        assert_eq!(
            ApprovedBuildError::CompiledPolicy.to_string(),
            "Reviewed OpenCode build constants are inconsistent; keep this session partial and install a corrected reviewed release"
        );
        let codec_error = codec::decode_policy(b"{}").unwrap_err();
        assert_eq!(
            ApprovedBuildError::Codec(codec_error).to_string(),
            "Cannot construct the native-origin installation policy: Invalid native-origin wire: closed grammar; discard the proof and keep the content partial"
        );
    }

    #[test]
    fn npr_15_no_key() {
        let compiled = serde_json::to_value(reviewed()).unwrap();
        let mut keys: Vec<_> = compiled
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "builtin_set_digest",
                "endpoints",
                "event_commands_json",
                "event_map_digest",
                "export_deadline_millis",
                "identity",
                "libra_template",
                "libra_template_digest",
                "protocols",
                "transport_policy"
            ]
        );
        let encoded = serde_json::to_string(reviewed()).unwrap();
        for forbidden in [
            "issuerPublicKey",
            "privateKey",
            "BEGIN PRIVATE KEY",
            "libraCommandPath",
            "libraRenderedDigest",
            "/Applications/Libra.app/Contents/MacOS/libra",
            "/opt/homebrew/bin/libra",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "public catalog must exclude {forbidden}"
            );
        }
        for slot in SLOTS {
            assert_eq!(reviewed().libra_template.matches(slot).count(), 1);
        }
    }

    #[test]
    fn npr_15_policy() {
        let build = reviewed();
        assert_eq!(build.libra_template.len(), 13600);
        assert_eq!(
            hex::encode(Sha256::digest(build.libra_template.as_bytes())),
            "9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d"
        );
        let vectors: serde_json::Value = serde_json::from_str(INDEPENDENT_VECTORS).unwrap();
        let mut policy_digests = Vec::new();
        for row in vectors["rows"].as_array().unwrap() {
            let path = row["policy"]["libraCommandPath"].as_str().unwrap();
            let rendered = build.render_observer(path).unwrap();
            assert_eq!(
                rendered.len() as u64,
                row["rendered_utf8_bytes"].as_u64().unwrap()
            );
            let policy = build
                .installation_policy(path, rendered.as_bytes())
                .unwrap();
            assert_eq!(serde_json::to_value(&policy).unwrap(), row["policy"]);
            let preimage = codec::policy_bytes(&policy).unwrap();
            assert_eq!(hex::encode(&preimage), row["policy_preimage_hex"]);
            let digest = hex::encode(Sha256::digest(&preimage));
            assert_eq!(digest, row["policy_bytes_sha256"]);
            policy_digests.push(digest);
            let mut changed = rendered.into_bytes();
            changed[0] ^= 1;
            assert_eq!(
                build.installation_policy(path, &changed).unwrap_err(),
                ApprovedBuildError::Observer
            );
        }
        assert_eq!(policy_digests.len(), 2);
        assert_ne!(policy_digests[0], policy_digests[1]);
        for path in [
            "libra",
            "/",
            "/usr//bin/libra",
            "/usr/../bin/libra",
            "/usr/./bin/libra",
            "/usr/bin/libra
",
            "/x/__LIBRA_COMMAND_JSON__",
            "/x/__EVENT_COMMANDS_JSON__",
            "/x/__EXPORT_DEADLINE_MS__",
        ] {
            assert_eq!(
                build.render_observer(path).unwrap_err(),
                ApprovedBuildError::CommandPath
            );
        }
        assert_eq!(
            build
                .render_observer(&format!("/{}", "x".repeat(4096)))
                .unwrap_err(),
            ApprovedBuildError::CommandPath
        );
        for endpoint in build.endpoints {
            assert_eq!(
                hex::encode(Sha256::digest(
                    codec::endpoint_bytes(&Endpoint {
                        version: 1,
                        protocol: endpoint.protocol,
                        url: endpoint.url.to_owned()
                    })
                    .unwrap()
                )),
                endpoint.sha256
            );
            assert!(build.endpoint(endpoint.protocol, endpoint.url).is_some());
            assert!(
                build
                    .endpoint(endpoint.protocol, &format!("{}?forged=1", endpoint.url))
                    .is_none()
            );
            assert!(
                build
                    .endpoint(Protocol::AnthropicMessages, endpoint.url)
                    .is_none()
            );
        }
        assert!(
            build
                .endpoint(Protocol::OpenaiResponses, "https://localhost/responses")
                .is_none()
        );
        let event_json = build.event_commands_json.as_bytes();
        let mut event_preimage = b"libra-opencode-native-event-map-v1\0".to_vec();
        event_preimage.extend_from_slice(&(event_json.len() as u32).to_be_bytes());
        event_preimage.extend_from_slice(event_json);
        assert_eq!(
            hex::encode(Sha256::digest(event_preimage)),
            build.event_map_digest
        );
        let current = include_str!("../../hooks/providers/opencode/settings.rs");
        let prefix = "const OPENCODE_PLUGIN_TEMPLATE: &str = r#\"";
        let actual = current
            .split_once(prefix)
            .unwrap()
            .1
            .split_once("\"#;")
            .unwrap()
            .0;
        assert_ne!(actual, build.libra_template);
        let current_commands: std::collections::BTreeMap<_, _> =
            crate::internal::ai::hooks::providers::opencode::events::OPENCODE_HOOK_EVENT_SPECS
                .iter()
                .chain(
                    crate::internal::ai::hooks::providers::opencode::events::OPENCODE_LEGACY_EVENT_SPECS,
                )
                .filter_map(|spec| spec.command.map(|command| (spec.name, command.to_string())))
                .collect();
        let current_events = serde_json::to_string(&current_commands).unwrap();
        let current_deadline =
            crate::internal::ai::observed_agents::opencode_export::EXPORT_DEADLINE
                .as_millis()
                .to_string();
        for legacy in [build, &REVIEWED_ANTHROPIC] {
            assert_eq!(legacy.libra_template.len(), 13600);
            assert_eq!(
                hex::encode(Sha256::digest(legacy.libra_template.as_bytes())),
                "9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d"
            );
            assert_eq!(
                hex::encode(Sha256::digest(legacy.event_commands_json.as_bytes())),
                "85977fd993350c87dde2cd8cafc4473d7f86309f362a397def7cd16efbcd5dc4"
            );
            for row in vectors["rows"].as_array().unwrap() {
                let path = row["policy"]["libraCommandPath"].as_str().unwrap();
                let current_rendered = actual
                    .replace(SLOTS[0], &serde_json::to_string(path).unwrap())
                    .replace(SLOTS[1], &current_events)
                    .replace(SLOTS[2], &current_deadline);
                assert_eq!(
                    legacy
                        .installation_policy(path, current_rendered.as_bytes())
                        .unwrap_err(),
                    ApprovedBuildError::Observer
                );
            }
        }
    }

    #[test]
    fn anthropic_legacy_policy_keeps_original_gold() {
        let build = &REVIEWED_ANTHROPIC;
        // Independent encoding of the signed6685 recipe, not the current renderer's output.
        for (path, bytes, rendered_sha256, policy_sha256) in [
            (
                "/Applications/Libra.app/Contents/MacOS/libra",
                13993,
                "e116b0f36e73bc9b0201e3ed0e65743205193233d46f4077a5ffa8a008d6ec6b",
                "794e9116fe4c244f6f553168f31a8cb54d9d004e66df274b15e4e7b49939f16a",
            ),
            (
                "/opt/homebrew/bin/libra",
                13972,
                "be742723a06b5b26357d5c4a935eca2ee9caf0e377596afcac7605b9cd9938ac",
                "3593923e7f7e5b86048bc192a66b3b75a7872fe97891846747b94f34dbd867d8",
            ),
        ] {
            let rendered = build.render_observer(path).unwrap();
            assert_eq!(rendered.len(), bytes);
            assert_eq!(
                hex::encode(Sha256::digest(rendered.as_bytes())),
                rendered_sha256
            );
            let policy = build
                .installation_policy(path, rendered.as_bytes())
                .unwrap();
            assert_eq!(
                hex::encode(Sha256::digest(codec::policy_bytes(&policy).unwrap())),
                policy_sha256
            );
            let other_path = format!("{path}-other");
            assert_eq!(
                build
                    .installation_policy(&other_path, rendered.as_bytes())
                    .unwrap_err(),
                ApprovedBuildError::Observer
            );
        }
    }
}
