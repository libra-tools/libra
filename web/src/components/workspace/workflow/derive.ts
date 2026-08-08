/**
 * Derive workflow-pane view-models from the live `CodeUiSessionSnapshot`.
 *
 * Phase 1 ships a low-fidelity mapping: enough so the Workflow / Summary /
 * Diff tabs render real data without leaking the mock module into the
 * production path. Phase 4 expands the mapping with full plan-step status
 * lattice, capability gating, and per-tool detail rendering.
 */

import type {
  CodeUiPlanSnapshot,
  CodeUiPlanStep,
  CodeUiSessionSnapshot,
  CodeUiTaskSnapshot,
  CodeUiToolCallSnapshot,
} from "@/lib/code-ui/types";

import type {
  ExecutionRun,
  IntentDoc,
  Plan,
  PlanStep,
  StepStatus,
  WorkflowState,
  WorkflowTask,
} from "./types";
import { EMPTY_WORKFLOW } from "./types";

export function deriveWorkflow(
  snapshot: CodeUiSessionSnapshot | null,
): WorkflowState {
  if (!snapshot) return EMPTY_WORKFLOW;
  const intent = deriveIntent(snapshot);
  const [executionPlan, testPlan] = deriveCanonicalPlans(snapshot.plans);
  const tasks = deriveTasks(snapshot.tasks);
  const runs = deriveRuns(snapshot.toolCalls);
  return {
    currentPhase: deriveCurrentPhase(snapshot),
    intent,
    plans: { execution: executionPlan, test: testPlan },
    tasks,
    runs,
    evidence: [],
  };
}

function deriveIntent(snapshot: CodeUiSessionSnapshot): IntentDoc {
  // The current snapshot doesn't carry a structured IntentSpec; fall back to
  // the most recent user message as a proxy for the active intent.
  const latestUser = [...snapshot.transcript]
    .reverse()
    .find((entry) => entry.kind === "user_message");
  if (!latestUser) {
    return EMPTY_WORKFLOW.intent;
  }
  return {
    title: latestUser.title ?? "Active intent",
    revision: snapshot.threadId ? `thread ${snapshot.threadId.slice(0, 8)}` : "r1",
    summary: latestUser.content ?? "",
    constraints: [],
    confirmed: snapshot.status !== "idle",
  };
}

function deriveCanonicalPlans(plans: CodeUiPlanSnapshot[]): [Plan, Plan] {
  const empty: Plan = { id: "—", steps: [] };
  if (plans.length === 0) return [empty, empty];
  const execution = planFromSnapshot(plans[0]);
  const test = plans.length > 1 ? planFromSnapshot(plans[1]) : empty;
  return [execution, test];
}

function planFromSnapshot(snapshot: CodeUiPlanSnapshot): Plan {
  return {
    id: snapshot.id,
    title: snapshot.title,
    summary: snapshot.summary,
    steps: snapshot.steps.map(stepFromSnapshot),
  };
}

function stepFromSnapshot(step: CodeUiPlanStep, index: number): PlanStep {
  return {
    id: `${step.step}-${index}`,
    label: step.step,
    status: normalizeStepStatus(step.status),
  };
}

function normalizeStepStatus(status: string): StepStatus {
  const lower = status.toLowerCase();
  if (lower === "running" || lower === "in_progress") return "running";
  if (lower === "done" || lower === "completed" || lower === "succeeded") return "done";
  if (lower === "failed" || lower === "error") return "failed";
  return "queued";
}

function deriveTasks(tasks: CodeUiTaskSnapshot[]): WorkflowTask[] {
  return tasks.map((task) => ({
    id: task.id,
    title: task.title ?? task.id,
    status: normalizeStepStatus(task.status),
    details: task.details,
    ago: relativeAgo(task.updatedAt),
  }));
}

function deriveRuns(toolCalls: CodeUiToolCallSnapshot[]): ExecutionRun[] {
  return toolCalls.map((tool) => {
    return {
      id: tool.id,
      step: tool.toolName,
      result: normalizeRunResult(tool.status),
      ago: relativeAgo(tool.updatedAt),
      label: tool.summary ?? tool.toolName,
      details: tool.details,
    };
  });
}

function normalizeRunResult(status: string): ExecutionRun["result"] {
  const lower = status.toLowerCase();
  if (lower === "succeeded" || lower === "completed") return "pass";
  if (lower === "failed" || lower === "error" || lower === "cancelled") return "fail";
  return "running";
}

function relativeAgo(updatedAt: string | undefined): string {
  if (!updatedAt) return "";
  const updated = new Date(updatedAt).getTime();
  if (Number.isNaN(updated)) return "";
  const seconds = Math.max(0, Math.floor((Date.now() - updated) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

function deriveCurrentPhase(snapshot: CodeUiSessionSnapshot): number {
  // Live status takes precedence — the agent's own state machine is the
  // strongest signal for which phase the workflow strip should highlight.
  switch (snapshot.status) {
    case "thinking":
      return 0;
    case "awaiting_interaction":
      return 1;
    case "executing_tool":
      return 2;
    case "completed":
      return 4;
    case "error":
      return 3;
    case "indeterminate_side_effect":
      return 3;
    default:
      break;
  }
  // Idle / unknown — fall back to the highest phase backed by snapshot
  // evidence (patchsets imply post-execution, tool calls imply execution,
  // plans imply phase 1). Empty snapshots default to phase 0 (intent).
  if (snapshot.patchsets.some((p) => isTerminalPatchsetStatus(p.status))) {
    return 4;
  }
  if (snapshot.toolCalls.length > 0) {
    return 2;
  }
  if (snapshot.tasks.length > 0) {
    return 2;
  }
  if (snapshot.plans.length > 0) {
    return 1;
  }
  return 0;
}

function isTerminalPatchsetStatus(status: string): boolean {
  const lower = status.toLowerCase();
  return lower === "applied" || lower === "released" || lower === "completed";
}
