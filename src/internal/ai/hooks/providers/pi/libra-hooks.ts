// Observe-only Pi extension. Every handler returns undefined and never
// changes a Pi message, tool call, tool result or context. It forwards the
// ADR-PI-06 envelope only while the project is trusted, to a fixed absolute
// Libra executable with fixed argv and no shell. Prompts, message bodies,
// tool input/output and thinking are never forwarded, and this receiver
// stores lifecycle metadata only. A delivery error is ignored.
import { spawn } from "node:child_process";

const LIBRA_COMMAND = __LIBRA_COMMAND_JSON__;
const LIBRA_ARGS = ["agent", "hooks", "pi", "observe"];
const ENVELOPE_SCHEMA = 1;
const OBSERVED_EVENTS = [
  "session_start",
  "session_shutdown",
  "before_agent_start",
  "turn_end",
  "tool_call",
  "tool_result",
  "agent_settled",
];

function nullableText(value) {
  return typeof value === "string" && value !== "" ? value : null;
}

function envelope(event, payload, ctx) {
  const manager = ctx.sessionManager;
  const sessionId = manager.getSessionId();
  if (typeof sessionId !== "string" || sessionId === "") {
    return undefined;
  }
  const frame = {
    schema: ENVELOPE_SCHEMA,
    event,
    sessionId,
    sessionFile: nullableText(manager.getSessionFile()),
    cwd: ctx.cwd,
    emittedAt: new Date().toISOString(),
  };
  switch (event) {
    case "session_start":
      frame.reason = payload.reason;
      frame.previousSessionFile = nullableText(payload.previousSessionFile);
      break;
    case "session_shutdown":
      frame.reason = payload.reason;
      frame.targetSessionFile = nullableText(payload.targetSessionFile);
      break;
    case "before_agent_start":
      frame.messageType = payload.messageType;
      break;
    case "turn_end":
      frame.turnIndex = payload.turnIndex;
      frame.messageEntryId = payload.messageEntryId;
      frame.toolResultEntryIds = Array.isArray(payload.toolResultEntryIds)
        ? payload.toolResultEntryIds.filter((id) => typeof id === "string" && id !== "")
        : [];
      frame.outcome = payload.outcome;
      break;
    case "tool_call":
    case "tool_result":
      frame.toolCallId = payload.toolCallId;
      frame.parentToolCallId = nullableText(payload.parentToolCallId);
      frame.toolName = payload.toolName;
      break;
    case "agent_settled":
      frame.aborted = payload.aborted === true;
      break;
    default:
      return undefined;
  }
  return frame;
}

function deliver(frame) {
  const child = spawn(LIBRA_COMMAND, LIBRA_ARGS, {
    cwd: frame.cwd,
    shell: false,
    stdio: ["pipe", "ignore", "ignore"],
    windowsHide: true,
  });
  child.on("error", () => {});
  child.stdin.on("error", () => {});
  child.stdin.end(JSON.stringify(frame));
}

function observe(event, payload, ctx) {
  try {
    if (typeof ctx?.isProjectTrusted !== "function" || ctx.isProjectTrusted() !== true) {
      return;
    }
    const frame = envelope(event, payload, ctx);
    if (frame !== undefined) {
      deliver(frame);
    }
  } catch {
    // Observation must not change what Pi does.
  }
}

export default function libraHooks(pi) {
  for (const event of OBSERVED_EVENTS) {
    pi.on(event, (payload, ctx) => {
      observe(event, payload, ctx);
      return undefined;
    });
  }
}
