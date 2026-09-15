import type {
  OperationGraphReadModel,
  OperationGraphView,
  OperationNode,
  RenderedOperation,
} from "./types";

/** Render bounds; the backend bounds its own traversal as well. */
export const OPERATION_GRAPH_MAX_NODES = 200;
export const OPERATION_GRAPH_MAX_DEPTH = 32;

function operationLabel(node: OperationNode): string {
  if (node.commandName && node.commandName.length > 0) {
    return node.commandName;
  }
  return node.kind;
}

/**
 * Derive the bounded, redacted render input from a backend read model.
 *
 * The traversal starts at the published heads and walks parent edges
 * breadth-first, bounded by `maxNodes` and `maxDepth`. Only the fields
 * declared on `RenderedOperation` are carried over — any additional key on
 * the payload is dropped here, which keeps secrets, prompts, transcripts,
 * and lease tokens out of the rendered surface by construction.
 */
export function toOperationGraphView(
  model: OperationGraphReadModel,
  options: { maxNodes?: number; maxDepth?: number } = {},
): OperationGraphView {
  const maxNodes = options.maxNodes ?? OPERATION_GRAPH_MAX_NODES;
  const maxDepth = options.maxDepth ?? OPERATION_GRAPH_MAX_DEPTH;

  const byId = new Map<string, OperationNode>();
  for (const node of model.operations) {
    byId.set(node.opId, node);
  }

  const rendered = new Map<string, RenderedOperation>();
  const seen = new Set<string>();
  const queue: Array<{ id: string; depth: number }> = [];

  // Breadth-first from the published heads along parent edges: children get
  // depth 0, their parents 1, and so on. Parents render first (larger depth).
  const enqueue = (id: string, depth: number) => {
    if (!seen.has(id)) {
      seen.add(id);
      queue.push({ id, depth });
    }
  };
  for (const headId of [...model.headOpIds].sort()) {
    enqueue(headId, 0);
  }
  for (let cursor = 0; cursor < queue.length; cursor += 1) {
    const { id, depth } = queue[cursor];
    const node = byId.get(id);
    if (!node) {
      continue;
    }
    for (const parentId of node.parentOpIds) {
      enqueue(parentId, depth + 1);
    }
  }
  // Orphan/disconnected nodes (not reachable from any head) still render,
  // anchored at depth 0, so the graph is never silently incomplete.
  for (const node of model.operations) {
    enqueue(node.opId, 0);
  }

  let truncated = false;
  for (let cursor = 0; cursor < queue.length; cursor += 1) {
    if (rendered.size >= maxNodes) {
      truncated = true;
      break;
    }
    const { id, depth } = queue[cursor];
    const node = byId.get(id);
    if (!node) {
      // Referenced by an edge but not present on this page.
      continue;
    }
    rendered.set(id, {
      opId: node.opId,
      kind: node.kind,
      status: node.status,
      label: operationLabel(node),
      parentOpIds: node.parentOpIds,
      depth: Math.min(depth, maxDepth),
    });
    if (depth >= maxDepth) {
      truncated = true;
    }
  }
  if (model.nextPageToken) {
    truncated = true;
  }

  // Parents-first render order: ancestors (farther from the heads) render
  // before their descendants, then stable id order.
  const nodes = [...rendered.values()].sort((left, right) => {
    if (left.depth !== right.depth) return right.depth - left.depth;
    return left.opId.localeCompare(right.opId);
  });
  const renderedIds = new Set(nodes.map((node) => node.opId));
  const edges: OperationGraphView["edges"] = [];
  for (const node of nodes) {
    for (const parentId of node.parentOpIds) {
      if (renderedIds.has(parentId)) {
        edges.push({ from: parentId, to: node.opId });
      }
    }
  }

  return {
    nodes,
    edges,
    unreconciledHeadIds:
      model.headOpIds.length > 1 ? [...model.headOpIds].sort() : [],
    truncated,
    changes: model.changes
      ? {
          revisions: model.changes.revisions,
          predecessorEdges: model.changes.predecessorEdges,
        }
      : undefined,
    nextPageToken: model.nextPageToken,
  };
}
