// @vitest-environment happy-dom

import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const clientMocks = vi.hoisted(() => ({
  attachController: vi.fn(),
  cancelTurn: vi.fn(),
  detachController: vi.fn(),
  respondInteraction: vi.fn(),
  submitMessage: vi.fn(),
}));

const storeState = vi.hoisted(() => ({
  snapshot: null as null | {
    capabilities: { commandIdempotency?: boolean };
  },
}));

vi.mock("./client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./client")>();
  return {
    ...actual,
    attachController: clientMocks.attachController,
    cancelTurn: clientMocks.cancelTurn,
    detachController: clientMocks.detachController,
    respondInteraction: clientMocks.respondInteraction,
    submitMessage: clientMocks.submitMessage,
  };
});

vi.mock("./store", () => ({
  useCodeUiStore: () => ({
    snapshot: storeState.snapshot,
  }),
}));

import { CodeUiClientError } from "./client";
import {
  BrowserControllerProvider,
  useBrowserController,
  type BrowserControllerHook,
} from "./controller";
import type { CodeUiControllerAttachResponse } from "./types";

type ActGlobal = typeof globalThis & {
  IS_REACT_ACT_ENVIRONMENT?: boolean;
};

function attachResponse(token: string): CodeUiControllerAttachResponse {
  return {
    controllerToken: token,
    leaseExpiresAt: new Date(Date.now() + 60_000).toISOString(),
    controller: {
      kind: "browser",
      canWrite: true,
      loopbackOnly: true,
      ownerLabel: "browser",
    },
  };
}

function renderController() {
  const container = document.createElement("div");
  document.body.appendChild(container);
  let root: Root | null = null;
  let controller: BrowserControllerHook | null = null;

  function Probe({ onController }: { onController: (value: BrowserControllerHook) => void }) {
    const value = useBrowserController();
    useEffect(() => {
      onController(value);
    }, [onController, value]);
    return null;
  }

  act(() => {
    root = createRoot(container);
    root.render(
      <BrowserControllerProvider>
        <Probe onController={(value) => {
          controller = value;
        }} />
      </BrowserControllerProvider>,
    );
  });

  return {
    get controller() {
      if (!controller) throw new Error("controller hook was not rendered");
      return controller;
    },
    unmount() {
      act(() => root?.unmount());
      container.remove();
    },
  };
}

async function captureControllerError(run: () => Promise<void>): Promise<unknown> {
  let caught: unknown;
  await act(async () => {
    try {
      await run();
    } catch (error) {
      caught = error;
    }
  });
  return caught;
}

beforeEach(() => {
  (globalThis as ActGlobal).IS_REACT_ACT_ENVIRONMENT = true;
  let uuidCounter = 0;
  vi.stubGlobal("crypto", {
    randomUUID: () => {
      uuidCounter += 1;
      return `id-${uuidCounter}`;
    },
  });
  storeState.snapshot = null;
  vi.clearAllMocks();
});

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("BrowserControllerProvider", () => {
  it("reattaches once when a cached lease token is rejected", async () => {
    clientMocks.attachController
      .mockResolvedValueOnce(attachResponse("stale-token"))
      .mockResolvedValueOnce(attachResponse("fresh-token"));
    clientMocks.submitMessage
      .mockRejectedValueOnce(
        new CodeUiClientError("INVALID_CONTROLLER_TOKEN", "expired lease", 401),
      )
      .mockResolvedValueOnce({ accepted: true });

    const harness = renderController();
    await act(async () => {
      await harness.controller.submit("/chat hello");
    });

    expect(clientMocks.attachController).toHaveBeenCalledTimes(2);
    expect(clientMocks.attachController).toHaveBeenNthCalledWith(1, {
      clientId: expect.stringMatching(/^browser-id-\d+$/),
      kind: "browser",
    });
    expect(clientMocks.submitMessage).toHaveBeenNthCalledWith(
      1,
      { text: "/chat hello" },
      "stale-token",
    );
    expect(clientMocks.submitMessage).toHaveBeenNthCalledWith(
      2,
      { text: "/chat hello" },
      "fresh-token",
    );
    expect(harness.controller.status).toMatchObject({
      kind: "attached",
      lease: { controllerToken: "fresh-token" },
    });

    harness.unmount();
  });

  it("includes a stable commandId when the runtime advertises commandIdempotency", async () => {
    storeState.snapshot = { capabilities: { commandIdempotency: true } };
    clientMocks.attachController.mockResolvedValueOnce(attachResponse("lease-token"));
    clientMocks.submitMessage
      .mockRejectedValueOnce(
        new CodeUiClientError("INVALID_CONTROLLER_TOKEN", "expired lease", 401),
      )
      .mockResolvedValueOnce({ accepted: true });
    clientMocks.attachController.mockResolvedValueOnce(attachResponse("fresh-token"));

    const harness = renderController();
    await act(async () => {
      await harness.controller.submit("/chat hello");
    });

    expect(clientMocks.submitMessage).toHaveBeenNthCalledWith(
      1,
      { text: "/chat hello", commandId: expect.stringMatching(/^cmd-/) },
      "lease-token",
    );
    expect(clientMocks.submitMessage).toHaveBeenNthCalledWith(
      2,
      { text: "/chat hello", commandId: expect.stringMatching(/^cmd-/) },
      "fresh-token",
    );
    const firstCommandId = clientMocks.submitMessage.mock.calls[0]?.[0]?.commandId;
    const secondCommandId = clientMocks.submitMessage.mock.calls[1]?.[0]?.commandId;
    expect(firstCommandId).toBe(secondCommandId);

    harness.unmount();
  });

  it("reuses the same commandId when submit is retried after a transport failure", async () => {
    storeState.snapshot = { capabilities: { commandIdempotency: true } };
    clientMocks.attachController.mockResolvedValue(attachResponse("lease-token"));
    clientMocks.submitMessage
      .mockRejectedValueOnce(new CodeUiClientError("INTERNAL_ERROR", "network blip", 500))
      .mockResolvedValueOnce({ accepted: true });

    const harness = renderController();
    const firstError = await captureControllerError(() =>
      harness.controller.submit("/chat hello"),
    );
    expect(firstError).toBeInstanceOf(CodeUiClientError);

    await act(async () => {
      await harness.controller.submit("/chat hello");
    });

    const firstCommandId = clientMocks.submitMessage.mock.calls[0]?.[0]?.commandId;
    const secondCommandId = clientMocks.submitMessage.mock.calls[1]?.[0]?.commandId;
    expect(firstCommandId).toMatch(/^cmd-/);
    expect(secondCommandId).toBe(firstCommandId);

    harness.unmount();
  });

  it("keeps distinct pending commandIds when submits interleave before ack", async () => {
    storeState.snapshot = { capabilities: { commandIdempotency: true } };
    clientMocks.attachController.mockResolvedValue(attachResponse("lease-token"));
    clientMocks.submitMessage
      .mockRejectedValueOnce(new CodeUiClientError("INTERNAL_ERROR", "first failed", 500))
      .mockRejectedValueOnce(new CodeUiClientError("INTERNAL_ERROR", "second failed", 500))
      .mockResolvedValueOnce({ accepted: true })
      .mockResolvedValueOnce({ accepted: true });

    const harness = renderController();
    await captureControllerError(() => harness.controller.submit("/chat one"));
    await captureControllerError(() => harness.controller.submit("/chat two"));

    await act(async () => {
      await harness.controller.submit("/chat one");
      await harness.controller.submit("/chat two");
    });

    const ids = clientMocks.submitMessage.mock.calls.map((call) => call[0]?.commandId);
    expect(ids[0]).toMatch(/^cmd-/);
    expect(ids[1]).toMatch(/^cmd-/);
    expect(ids[0]).not.toBe(ids[1]);
    expect(ids[2]).toBe(ids[0]);
    expect(ids[3]).toBe(ids[1]);

    harness.unmount();
  });

  it("surfaces CONTROLLER_CONFLICT without retrying attach", async () => {
    clientMocks.attachController.mockResolvedValueOnce(attachResponse("lease-token"));
    clientMocks.submitMessage.mockRejectedValueOnce(
      new CodeUiClientError(
        "CONTROLLER_CONFLICT",
        "another browser already owns the lease",
        409,
      ),
    );

    const harness = renderController();
    const error = await captureControllerError(() =>
      harness.controller.submit("/chat hello"),
    );

    expect(error).toBeInstanceOf(CodeUiClientError);
    expect(error).toMatchObject({ code: "CONTROLLER_CONFLICT", status: 409 });
    expect(clientMocks.attachController).toHaveBeenCalledTimes(1);
    expect(clientMocks.submitMessage).toHaveBeenCalledTimes(1);
    expect(harness.controller.status).toMatchObject({
      kind: "error",
      code: "CONTROLLER_CONFLICT",
    });

    harness.unmount();
  });

  it("surfaces BROWSER_CONTROL_DISABLED from lazy attach", async () => {
    clientMocks.attachController.mockRejectedValueOnce(
      new CodeUiClientError(
        "BROWSER_CONTROL_DISABLED",
        "start libra code with --browser-control loopback",
        403,
      ),
    );

    const harness = renderController();
    const error = await captureControllerError(() =>
      harness.controller.submit("/chat hello"),
    );

    expect(error).toBeInstanceOf(CodeUiClientError);
    expect(error).toMatchObject({ code: "BROWSER_CONTROL_DISABLED", status: 403 });
    expect(clientMocks.attachController).toHaveBeenCalledTimes(1);
    expect(clientMocks.submitMessage).not.toHaveBeenCalled();
    expect(harness.controller.status).toMatchObject({
      kind: "error",
      code: "BROWSER_CONTROL_DISABLED",
    });

    harness.unmount();
  });
});
