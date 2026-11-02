// Observe-only Pi extension. Every handler returns undefined and never
// changes a Pi message, tool call, tool result or context. It forwards the
// ADR-PI-06 envelope only while the project is trusted, to a fixed absolute
// Libra executable with fixed argv and no shell. Prompts, message bodies,
// tool input/output and thinking are never forwarded, and this receiver
// stores lifecycle metadata only. A delivery error is ignored, and each
// delivery is bounded in time, attempts and concurrent children.
import { spawn } from "node:child_process";

const LIBRA_COMMAND = __LIBRA_COMMAND_JSON__;
// The child carries its own capture budget, shorter than the delivery
// window, so it ends its work even if Pi dies without running any hook.
const CHILD_CAPTURE_BUDGET_MS = 1500;
const LIBRA_ARGS = [
  "agent",
  "hooks",
  "--capture-budget-ms",
  String(CHILD_CAPTURE_BUDGET_MS),
  "pi",
  "observe",
];
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

// Resource contract (PI-13): one delivery gets at most MAX_ATTEMPTS child
// processes that share one DELIVERY_DEADLINE_MS window on the monotonic
// clock. A child still running at the deadline is killed with SIGKILL, and
// Node reaps it when its exit event arrives. At most MAX_IN_FLIGHT children
// run at once in this Pi process, across every loaded copy of this
// extension: a delivery or retry that would exceed it is dropped, never
// queued, so Pi's loop is never held and no frame is kept.
//
// Only a child that never started is retried: the receiver has no replay
// identity yet, so a child that ran, even one that exited non-zero or was
// killed, may already have recorded the event and is never run again. A
// spawn that throws synchronously fails the same way every time and is not
// retried either. A failed kill is retried exactly once. When Pi exits
// normally every running child is killed; if Pi dies without running any
// hook, each child still ends on its own: libra stops capture at its
// 1500 ms budget and ends its process a fixed grace after that, whatever
// its main thread is doing.
const DELIVERY_DEADLINE_MS = 2000;
const MAX_ATTEMPTS = 3;
const MAX_IN_FLIGHT = 8;
const KILL_RETRY_MS = 50;
const SHARED = Symbol.for("libra.pi-hooks.delivery");
const shared = (globalThis[SHARED] ??= { running: new Set(), exitHookInstalled: false });

// How children are started and how time is read. Pi never changes these;
// tests replace them to fake the child processes or the clock.
let spawnChild = spawn;
let clock = {
  now: () => performance.now(),
  setTimeout: (callback, ms) => setTimeout(callback, ms),
  clearTimeout: (handle) => clearTimeout(handle),
};

export function __setSpawnForTest(replacement) {
  spawnChild = replacement;
}

export function __setClockForTest(replacement) {
  clock = replacement;
}

function stillRunning(child) {
  return child.exitCode === null && child.signalCode === null;
}

// A failed kill is retried exactly once. The slot stays taken until the
// child's exit event arrives.
function retryKillOnce(child) {
  if (child.libraKillRetried) {
    return;
  }
  child.libraKillRetried = true;
  clock.setTimeout(() => {
    if (!stillRunning(child)) {
      return;
    }
    try {
      child.kill("SIGKILL");
    } catch {
      // Nothing more to do here; the exit event still releases the slot.
    }
  }, KILL_RETRY_MS);
}

function forceKill(child) {
  if (!stillRunning(child)) {
    return;
  }
  try {
    child.kill("SIGKILL");
  } catch {
    retryKillOnce(child);
  }
}

function killRunning() {
  for (const child of shared.running) {
    forceKill(child);
  }
}

function attempt(frame, deadline, attemptNumber) {
  if (attemptNumber > MAX_ATTEMPTS || shared.running.size >= MAX_IN_FLIGHT) {
    return;
  }
  if (deadline - clock.now() <= 0) {
    return;
  }
  let child;
  try {
    child = spawnChild(LIBRA_COMMAND, LIBRA_ARGS, {
      cwd: frame.cwd,
      shell: false,
      stdio: ["pipe", "ignore", "ignore"],
      windowsHide: true,
    });
  } catch {
    return;
  }
  shared.running.add(child);
  let settled = false;
  // Measure the time left after spawn returns, so spawn itself cannot
  // stretch the window.
  const timer = clock.setTimeout(() => forceKill(child), Math.max(0, deadline - clock.now()));
  const finish = (neverStarted) => {
    if (settled) {
      return;
    }
    settled = true;
    clock.clearTimeout(timer);
    shared.running.delete(child);
    if (neverStarted) {
      attempt(frame, deadline, attemptNumber + 1);
    }
  };
  child.on("error", () => {
    if (child.pid === undefined) {
      // Never started: nothing was recorded, and no exit event follows.
      finish(true);
    } else {
      // A kill that failed while the child still runs.
      retryKillOnce(child);
    }
  });
  // The child ran, so it may have recorded the event: never run it again.
  child.on("exit", () => finish(false));
  child.stdin.on("error", () => {});
  child.stdin.end(JSON.stringify(frame));
}

function deliver(frame) {
  if (!shared.exitHookInstalled) {
    shared.exitHookInstalled = true;
    process.once("exit", killRunning);
  }
  attempt(frame, clock.now() + DELIVERY_DEADLINE_MS, 1);
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
