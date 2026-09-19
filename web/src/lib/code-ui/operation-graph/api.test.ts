import { describe, expect, it } from "vitest";

import { createOperationGraphApi, type OperationGraphTransport } from "./api";

describe("createOperationGraphApi", () => {
  it("requests the bounded operation graph endpoint with camel-case query keys", async () => {
    const requests: Array<{ path: string; init?: RequestInit }> = [];
    const transport: OperationGraphTransport = {
      request: async (path, init) => {
        requests.push({ path, init });
        return { operations: [], headOpIds: [] };
      },
    };

    await createOperationGraphApi(transport).fetchReadModel({
      limit: 25,
      depth: 8,
      pageToken: "25",
    });

    expect(requests).toEqual([
      {
        path: "/api/code/operation-graph?limit=25&depth=8&pageToken=25",
        init: undefined,
      },
    ]);
  });

  it("preserves structured backend errors for the host", async () => {
    const transport: OperationGraphTransport = {
      request: async () => {
        throw { status: 503, code: "OPERATION_GRAPH_UNAVAILABLE", message: "retry" };
      },
    };

    await expect(createOperationGraphApi(transport).fetchReadModel()).rejects.toEqual({
      status: 503,
      code: "OPERATION_GRAPH_UNAVAILABLE",
      message: "retry",
    });
  });
});
