import type { CodeUiApiError } from "../types";

import type { OperationGraphQuery, OperationGraphReadModel } from "./types";

export interface OperationGraphTransport {
  request<T>(path: string, init?: RequestInit): Promise<T>;
}

export class FetchOperationGraphTransport implements OperationGraphTransport {
  constructor(private readonly baseUrl = "") {}

  async request<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      credentials: "same-origin",
      ...init,
    });
    if (!response.ok) {
      let message = response.statusText;
      let code: string | undefined;
      try {
        const body = (await response.json()) as { error?: Partial<CodeUiApiError> };
        if (typeof body.error?.message === "string") message = body.error.message;
        if (typeof body.error?.code === "string") code = body.error.code;
      } catch {
        // Non-JSON failures still surface statusText.
      }
      throw { status: response.status, message, code } satisfies CodeUiApiError;
    }
    return (await response.json()) as T;
  }
}

/** Operation/Change read-only graph HTTP surface (OL-14). */
export function createOperationGraphApi(
  transport: OperationGraphTransport = new FetchOperationGraphTransport(),
) {
  return {
    fetchReadModel(
      query: OperationGraphQuery = {},
    ): Promise<OperationGraphReadModel> {
      const params = new URLSearchParams();
      if (query.limit !== undefined) params.set("limit", String(query.limit));
      if (query.depth !== undefined) params.set("depth", String(query.depth));
      if (query.pageToken) params.set("pageToken", query.pageToken);
      const queryString = params.toString();
      return transport.request(
        `/api/code/operation-graph${queryString ? `?${queryString}` : ""}`,
      );
    },
  };
}
