"use client";

import { useEffect, useMemo, useState } from "react";

import {
  createOperationGraphApi,
  toOperationGraphView,
  type OperationGraphQuery,
  type OperationGraphView,
} from "@/lib/code-ui/operation-graph";

interface OperationGraphHostProps {
  query?: OperationGraphQuery;
}

/**
 * Read-only Operation / Change graph host (plan-20260822 OL-14). The
 * projection is bounded server-side; this component renders the redacted
 * view model and surfaces multi-head and truncation states explicitly.
 */
export function OperationGraphHost({ query }: OperationGraphHostProps) {
  const api = useMemo(() => createOperationGraphApi(), []);
  const [view, setView] = useState<OperationGraphView | null>(null);
  const [error, setError] = useState<string | null>(null);

  const queryKey = JSON.stringify(query ?? {});

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const model = await api.fetchReadModel(
          JSON.parse(queryKey) as OperationGraphQuery,
        );
        if (cancelled) return;
        setView(toOperationGraphView(model));
        setError(null);
      } catch (thrown) {
        if (cancelled) return;
        const status = (thrown as { status?: number }).status;
        if (status === 404) {
          // The backend read-model route is not registered yet: empty state.
          setView(null);
          return;
        }
        const message = (thrown as { message?: string }).message;
        setError(message ?? "operation graph unavailable");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, queryKey]);

  return (
    <section aria-label="Operation graph" data-testid="operation-graph">
      <h2>Operation Graph</h2>
      {error ? (
        <p role="alert">{error}</p>
      ) : view === null ? (
        <p>No operation graph available.</p>
      ) : (
        <>
          {view.unreconciledHeadIds.length > 0 ? (
            <p role="status" data-testid="operation-graph-unreconciled">
              Unreconciled operation heads: {view.unreconciledHeadIds.join(", ")} —
              run <code>libra op reconcile</code> to converge.
            </p>
          ) : null}
          {view.nodes.length === 0 ? (
            <p>No operations recorded.</p>
          ) : (
            <ol>
              {view.nodes.map((node) => (
                <li key={node.opId} data-status={node.status} data-kind={node.kind}>
                  <code>{node.opId}</code> — {node.label} [{node.status}]
                  {node.parentOpIds.length > 0 ? (
                    <small> ← {node.parentOpIds.join(", ")}</small>
                  ) : null}
                </li>
              ))}
            </ol>
          )}
          {view.changes ? (
            <div data-testid="operation-graph-changes">
              <h3>Change genealogy</h3>
              <ul>
                {view.changes.predecessorEdges.map((edge) => (
                  <li key={`${edge.successorOid}-${edge.predecessorOid}`}>
                    {edge.successorOid.slice(0, 8)} ← {edge.relationKind} ←{" "}
                    {edge.predecessorOid.slice(0, 8)} (op {edge.opId})
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {view.truncated ? (
            <p role="note" data-testid="operation-graph-truncated">
              Showing the most recent operations; older history is bounded.
            </p>
          ) : null}
        </>
      )}
    </section>
  );
}
