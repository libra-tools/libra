import type {
  OperationGraphReadModel,
  OperationNode,
} from "./types";

export function operationNodeFixture(
  overrides: Partial<OperationNode> & Pick<OperationNode, "opId">,
): OperationNode {
  return {
    kind: "command",
    status: "success",
    commandName: "libra commit",
    description: "recorded command operation",
    actor: "libra-user",
    recordedAt: 1_700_000_000_000,
    parentOpIds: [],
    ...overrides,
  };
}

/** A repository with no recorded operations. */
export function emptyOperationGraphFixture(): OperationGraphReadModel {
  return { operations: [], headOpIds: [] };
}

/** A linear history head <- child <- grandchild. */
export function singleChainOperationGraphFixture(): OperationGraphReadModel {
  return {
    operations: [
      operationNodeFixture({
        opId: "op-root",
        parentOpIds: [],
        commandName: "libra init",
      }),
      operationNodeFixture({
        opId: "op-mid",
        parentOpIds: ["op-root"],
        commandName: "libra commit",
        recordedAt: 1_700_000_001_000,
      }),
      operationNodeFixture({
        opId: "op-head",
        parentOpIds: ["op-mid"],
        kind: "external_snapshot",
        commandName: "external snapshot",
        recordedAt: 1_700_000_002_000,
      }),
    ],
    headOpIds: ["op-head"],
  };
}

/** Two concurrent publications: the head set is not converged yet. */
export function multiHeadOperationGraphFixture(): OperationGraphReadModel {
  return {
    operations: [
      operationNodeFixture({ opId: "op-base", parentOpIds: [] }),
      operationNodeFixture({
        opId: "op-a",
        parentOpIds: ["op-base"],
        actor: "worktree-a",
      }),
      operationNodeFixture({
        opId: "op-b",
        parentOpIds: ["op-base"],
        actor: "worktree-b",
      }),
    ],
    headOpIds: ["op-a", "op-b"],
  };
}

/** Change genealogy projection alongside the operation DAG. */
export function changeGenealogyFixture(): OperationGraphReadModel["changes"] {
  return {
    revisions: [
      {
        changeId: "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        commitOid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        visibility: "visible",
        revisionOrdinal: 0,
      },
      {
        changeId: "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        commitOid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        visibility: "hidden",
        revisionOrdinal: 1,
      },
    ],
    predecessorEdges: [
      {
        successorOid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        predecessorOid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        relationKind: "amend",
        opId: "op-amend",
      },
    ],
  };
}
