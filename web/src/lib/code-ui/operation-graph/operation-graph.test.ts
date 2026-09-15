import { describe, expect, it } from "vitest";

import {
  changeGenealogyFixture,
  emptyOperationGraphFixture,
  multiHeadOperationGraphFixture,
  operationNodeFixture,
  singleChainOperationGraphFixture,
} from "./fixtures";
import { toOperationGraphView } from "./view-model";

describe("toOperationGraphView", () => {
  it("renders an empty repository as an empty, converged graph", () => {
    const view = toOperationGraphView(emptyOperationGraphFixture());
    expect(view.nodes).toEqual([]);
    expect(view.edges).toEqual([]);
    expect(view.unreconciledHeadIds).toEqual([]);
    expect(view.truncated).toBe(false);
  });

  it("orders a single chain parents-first and derives its edges", () => {
    const view = toOperationGraphView(singleChainOperationGraphFixture());
    expect(view.nodes.map((node) => node.opId)).toEqual([
      "op-root",
      "op-mid",
      "op-head",
    ]);
    expect(view.edges).toEqual([
      { from: "op-root", to: "op-mid" },
      { from: "op-mid", to: "op-head" },
    ]);
    expect(view.unreconciledHeadIds).toEqual([]);
    expect(view.truncated).toBe(false);
  });

  it("surfaces unreconciled heads for a multi-head repository", () => {
    const view = toOperationGraphView(multiHeadOperationGraphFixture());
    expect(view.unreconciledHeadIds).toEqual(["op-a", "op-b"]);
    expect(view.nodes.map((node) => node.opId)).toContain("op-base");
  });

  it("drops unknown payload fields so secrets never reach the render surface", () => {
    const model = {
      ...singleChainOperationGraphFixture(),
      operations: singleChainOperationGraphFixture().operations.map((node) => ({
        ...node,
        // Simulate an over-sharing backend: unknown keys must be dropped by
        // the view-model pick, not forwarded to the renderer.
        prompt: "do not render",
        leaseToken: "do-not-render",
        transcriptUrl: "do-not-render",
      })),
      leaseToken: "do-not-render",
    };
    const view = toOperationGraphView(model);
    const serialized = JSON.stringify(view);
    expect(serialized).not.toContain("do not render");
    expect(serialized).not.toContain("do-not-render");
    expect(view.nodes).toHaveLength(3);
  });

  it("bounds the rendered node count and reports truncation", () => {
    const operations = Array.from({ length: 12 }, (_, index) =>
      operationNodeFixture({
        opId: `op-${index}`,
        parentOpIds: index === 0 ? [] : [`op-${index - 1}`],
      }),
    );
    const view = toOperationGraphView(
      { operations, headOpIds: ["op-11"] },
      { maxNodes: 5 },
    );
    expect(view.nodes).toHaveLength(5);
    expect(view.truncated).toBe(true);
  });

  it("bounds traversal depth and keeps pagination truncation visible", () => {
    const view = toOperationGraphView(
      {
        ...singleChainOperationGraphFixture(),
        nextPageToken: "page-2",
      },
      { maxDepth: 1 },
    );
    expect(view.nodes.every((node) => node.depth <= 1)).toBe(true);
    expect(view.truncated).toBe(true);
    expect(view.nextPageToken).toBe("page-2");
  });

  it("carries the redacted change genealogy through unchanged", () => {
    const view = toOperationGraphView({
      ...singleChainOperationGraphFixture(),
      changes: changeGenealogyFixture(),
    });
    expect(view.changes?.revisions).toHaveLength(2);
    expect(view.changes?.predecessorEdges[0].relationKind).toBe("amend");
    expect(
      view.changes?.predecessorEdges[0].opId,
    ).toBe("op-amend");
  });

  it("does not render hidden revisions as a different identity", () => {
    const view = toOperationGraphView({
      ...singleChainOperationGraphFixture(),
      changes: changeGenealogyFixture(),
    });
    const hidden = view.changes?.revisions.filter(
      (revision) => revision.visibility === "hidden",
    );
    expect(hidden).toHaveLength(1);
    // Hidden revisions keep the same logical change id as their visible
    // successor: the genealogy, not the visibility flag, carries identity.
    expect(hidden?.[0].changeId).toBe(
      view.changes?.revisions[0].changeId,
    );
  });
});
