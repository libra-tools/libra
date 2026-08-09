/**
 * Wire types for the Libra `libra code` Web UI v1 contract.
 *
 * Field names use camelCase to match Rust `#[serde(rename_all = "camelCase")]`;
 * tagged enum values use snake_case to match Rust `#[serde(rename_all = "snake_case")]`.
 * Source of truth: src/internal/ai/web/code_ui.rs.
 *
 * Pure type module — no runtime values — safe to import anywhere.
 */

/** Session status. Maps to Rust `CodeUiSessionStatus`. */
export type CodeUiSessionStatus =
  | "idle"
  | "thinking"
  | "executing_tool"
  | "awaiting_interaction"
  | "completed"
  | "error"
  /** A mutating command may have taken effect; reconcile before retrying. */
  | "indeterminate_side_effect";

/** Capability flags advertised by the runtime. All eight default to `false`. */
export type CodeUiCapabilities = {
  messageInput: boolean;
  streamingText: boolean;
  planUpdates: boolean;
  toolCalls: boolean;
  patchsets: boolean;
  interactiveApprovals: boolean;
  structuredQuestions: boolean;
  providerSessionResume: boolean;
  /** When true, browser submits include a stable `commandId` for retry dedup. */
  commandIdempotency?: boolean;
};

export type CodeUiProviderInfo = {
  provider: string;
  model?: string;
  mode?: string;
  managed?: boolean;
};

export type CodeUiControllerKind =
  | "none"
  | "browser"
  | "automation"
  | "tui"
  | "cli";

/** Active controller state derived by the runtime. */
export type CodeUiControllerState = {
  kind: CodeUiControllerKind;
  ownerLabel?: string;
  canWrite: boolean;
  /** ISO 8601 timestamp; absent when no lease is active. */
  leaseExpiresAt?: string;
  reason?: string;
  loopbackOnly: boolean;
};

export type CodeUiTranscriptEntryKind =
  | "user_message"
  | "assistant_message"
  | "tool_call"
  | "plan_summary"
  | "diff"
  | "info_note";

export type CodeUiTranscriptEntry = {
  id: string;
  kind: CodeUiTranscriptEntryKind;
  title?: string;
  content?: string;
  status?: string;
  streaming: boolean;
  metadata: unknown;
  createdAt: string;
  updatedAt: string;
};

export type CodeUiInteractionKind =
  | "approval"
  | "sandbox_approval"
  | "request_user_input"
  | "intent_review_choice"
  | "post_plan_choice";

export type CodeUiInteractionStatus = "pending" | "resolved" | "cancelled";

export type CodeUiInteractionOption = {
  id: string;
  label: string;
  description?: string;
};

export type CodeUiInteractionRequest = {
  id: string;
  kind: CodeUiInteractionKind;
  title?: string;
  description?: string;
  prompt?: string;
  options: CodeUiInteractionOption[];
  status: CodeUiInteractionStatus;
  metadata: unknown;
  requestedAt: string;
  resolvedAt?: string;
};

export type CodeUiApplyToFuture = "no" | "accept_all" | "decline_all";

export type CodeUiInteractionResponse = {
  approved?: boolean;
  applyToFuture?: CodeUiApplyToFuture;
  selectedOption?: string;
  note?: string;
  /** Map question id → list of answer values (used by `request_user_input`). */
  answers?: Record<string, string[]>;
};

export type CodeUiPlanStep = {
  step: string;
  status: string;
};

export type CodeUiPlanSnapshot = {
  id: string;
  title?: string;
  summary?: string;
  status: string;
  steps: CodeUiPlanStep[];
  updatedAt: string;
};

export type CodeUiTaskSnapshot = {
  id: string;
  title?: string;
  status: string;
  details?: string;
  updatedAt: string;
};

export type CodeUiToolCallSnapshot = {
  id: string;
  toolName: string;
  status: string;
  summary?: string;
  details?: string;
  updatedAt: string;
};

export type CodeUiPatchChange = {
  path: string;
  changeType: string;
  diff?: string;
};

export type CodeUiPatchsetSnapshot = {
  id: string;
  status: string;
  changes: CodeUiPatchChange[];
  updatedAt: string;
};

export type CodeUiSessionSnapshot = {
  sessionId: string;
  threadId?: string;
  workingDir: string;
  provider: CodeUiProviderInfo;
  capabilities: CodeUiCapabilities;
  controller: CodeUiControllerState;
  status: CodeUiSessionStatus;
  transcript: CodeUiTranscriptEntry[];
  plans: CodeUiPlanSnapshot[];
  tasks: CodeUiTaskSnapshot[];
  toolCalls: CodeUiToolCallSnapshot[];
  patchsets: CodeUiPatchsetSnapshot[];
  interactions: CodeUiInteractionRequest[];
  updatedAt: string;
};

/** Allowed values for `CodeUiEventEnvelope.type`. */
export const CODE_UI_EVENT_TYPES = [
  "session_updated",
  "status_changed",
  "controller_changed",
] as const;
export type CodeUiEventType = (typeof CODE_UI_EVENT_TYPES)[number];

export type CodeUiEventEnvelope = {
  seq: number;
  type: CodeUiEventType;
  at: string;
  /** Every Code UI SSE event carries the full typed snapshot for gap recovery. */
  data: CodeUiSessionSnapshot;
};

export type CodeUiControllerAttachRequest = {
  clientId: string;
  /** Defaults to `"browser"` server-side when omitted. */
  kind?: CodeUiControllerKind;
};

export type CodeUiControllerAttachResponse = {
  controllerToken: string;
  leaseExpiresAt: string;
  controller: CodeUiControllerState;
};

export type CodeUiControllerDetachRequest = {
  clientId: string;
};

export type CodeUiMessageRequest = {
  text: string;
  /** Caller-stable runtime command identity for retry de-duplication. */
  commandId?: string;
};

export type CodeUiAckResponse = {
  accepted: boolean;
};

export type CodeUiDiagnosticsPorts = {
  web?: number;
  mcp?: number;
};

export type CodeUiDiagnostics = {
  pid: number;
  provider: string;
  model?: string;
  threadId?: string;
  status: CodeUiSessionStatus;
  controller: CodeUiControllerState;
  ports?: CodeUiDiagnosticsPorts;
  logFile?: string;
  activeInteractionId?: string;
  lastError?: string;
};

/** Server-side error envelope returned by `/api/code/*` routes on non-2xx responses. */
export type CodeUiErrorEnvelope = {
  error: {
    code: string;
    message: string;
  };
};

/** Known error codes surfaced by the server. Extend as new codes appear. */
export const CODE_UI_ERROR_CODES = [
  "CODE_UI_UNAVAILABLE",
  "LOOPBACK_REQUIRED",
  "CONTROLLER_CONFLICT",
  "MISSING_CONTROLLER_TOKEN",
  "INVALID_CONTROLLER_TOKEN",
  "INVALID_CONTROLLER_KIND",
  "BROWSER_CONTROL_DISABLED",
  "CONTROL_DISABLED",
  "MISSING_CONTROL_TOKEN",
  "INVALID_CONTROL_TOKEN",
  "AUTOMATION_CONTROLLER_REQUIRED",
  "PAYLOAD_TOO_LARGE",
  "UNSUPPORTED_OPERATION",
  "INTERNAL_ERROR",
] as const;
export type CodeUiErrorCode = (typeof CODE_UI_ERROR_CODES)[number];
