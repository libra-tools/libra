/**
 * Read model types for the Operation / Change read-only web graph
 * (plan-20260822 OL-14).
 *
 * Redaction contract: these types intentionally model only opaque,
 * redacted identifiers. The backend never places prompt, transcript,
 * secret, or lease-token material in these documents (`ai_operation_link`
 * rows are already redacted server-side), and the view model picks only
 * the fields declared here so any unknown payload key is dropped
 * by construction.
 */

export type OperationStatus =
  | "running"
  | "success"
  | "failed"
  | "partial"
  | "aborted";

export type OperationKind =
  | "command"
  | "external_snapshot"
  | "restore"
  | "undo"
  | "redo"
  | "revert"
  | "reconcile"
  | "legacy";

/** One node of the append-only operation DAG. */
export interface OperationNode {
  opId: string;
  kind: OperationKind;
  status: OperationStatus;
  commandName?: string;
  description?: string;
  actor?: string;
  /** Unix millis when the operation was recorded, when known. */
  recordedAt?: number;
  parentOpIds: string[];
}

export type RevisionVisibility = "visible" | "hidden";

export type RelationKind =
  | "amend"
  | "rebase"
  | "cherry_pick"
  | "squash"
  | "split"
  | "duplicate"
  | "import"
  | "external_reconcile";

export interface ChangeRevisionView {
  /** Opaque, canonical 128-bit Change ID (hex). */
  changeId: string;
  /** Opaque commit object id. */
  commitOid: string;
  visibility: RevisionVisibility;
  revisionOrdinal: number;
}

export interface ChangeGenealogyEdge {
  successorOid: string;
  predecessorOid: string;
  relationKind: RelationKind;
  /** Redacted operation reference that produced this edge. */
  opId: string;
}

/**
 * The read-only projection served by the backend. The backend bounds node
 * count and traversal depth; `nextPageToken` continues the traversal.
 */
export interface OperationGraphReadModel {
  operations: OperationNode[];
  headOpIds: string[];
  changes?: {
    revisions: ChangeRevisionView[];
    predecessorEdges: ChangeGenealogyEdge[];
  };
  nextPageToken?: string;
}

export interface OperationGraphQuery {
  /** Bounded page size requested from the backend. */
  limit?: number;
  /** Traversal depth from the current heads. */
  depth?: number;
  /** Continuation token from a previous page. */
  pageToken?: string;
}

/** One rendered operation row plus its parent links. */
export interface RenderedOperation {
  opId: string;
  kind: OperationKind;
  status: OperationStatus;
  label: string;
  parentOpIds: string[];
  depth: number;
}

/** Bounded, redacted render input derived from a read model. */
export interface OperationGraphView {
  nodes: RenderedOperation[];
  /** parent -> child edges, bounded to the rendered nodes. */
  edges: Array<{ from: string; to: string }>;
  /** Head ids when more than one head exists (unresolved concurrency). */
  unreconciledHeadIds: string[];
  /** True when the backend reported more nodes than were rendered. */
  truncated: boolean;
  changes?: {
    revisions: ChangeRevisionView[];
    predecessorEdges: ChangeGenealogyEdge[];
  };
  nextPageToken?: string;
}
