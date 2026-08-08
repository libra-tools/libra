"use client";

import { useEffect, useMemo, useRef } from "react";

import { IconBranch, IconCopy, IconMore, IconTerm, IconThread } from "@/components/icons";
import { Splitter } from "@/components/workspace/splitter";
import { Terminal } from "@/components/workspace/terminal/terminal";
import { useBrowserController } from "@/lib/code-ui/controller";
import { useCodeUiStore } from "@/lib/code-ui/store";
import { deriveChatMessages } from "@/lib/code-ui/view-model";
import { useStoredBoolean, useStoredNumber } from "@/lib/persisted-state";
import { clamp } from "@/lib/storage";

import { Composer } from "./composer";
import { InteractionPanel } from "./interaction-panel";
import { Message } from "./message";

export function Chat() {
  const { snapshot, repo, status, connection } = useCodeUiStore();
  const controller = useBrowserController();
  const messages = useMemo(() => deriveChatMessages(snapshot), [snapshot]);

  const [termOpen, setTermOpen] = useStoredBoolean("libra.termOpen", true);
  const [termH, setTermH] = useStoredNumber("libra.termH", 240);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const chatBodyRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [messages]);

  const composerDisabledReason = composerDisabledHint(snapshot, connection, controller.status);
  const canSubmit = composerDisabledReason === null;

  async function submit(draft: string) {
    if (!canSubmit) return;
    try {
      await controller.submit(draft);
    } catch {
      /* errors surface via controller.status */
    }
  }

  async function cancelTurn() {
    try {
      await controller.cancel();
    } catch {
      /* errors surface via controller.status */
    }
  }

  const cancelEnabled =
    snapshot?.controller.canWrite !== false &&
    snapshot?.status !== undefined &&
    ["thinking", "executing_tool", "awaiting_interaction"].includes(snapshot.status);

  function onTerminalDrag(dy: number, startH: number) {
    const parentH = chatBodyRef.current?.parentElement?.clientHeight ?? 800;
    const max = parentH - 260;
    setTermH(clamp(startH - dy, 120, max));
  }

  const branchLabel = branchLabelFromStatus(status);
  const headerTitle = headerTitleFromSnapshotAndRepo(snapshot, repo);
  const phaseLabel = derivePhaseLabel(snapshot);

  return (
    <section className="flex min-w-0 flex-1 flex-col bg-paper">
      <header className="flex h-12 shrink-0 items-center justify-between border-b border-rule px-5">
        <div className="flex min-w-0 items-center gap-2.5">
          <IconThread size={15} />
          <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[13.5px] font-semibold">
            {headerTitle}
          </div>
          {branchLabel && (
            <span className="mono inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-sm border border-rule-2 bg-paper-2 px-1.5 py-0.5 text-[10.5px] text-ink-2">
              <IconBranch size={11} /> {branchLabel}
            </span>
          )}
          {phaseLabel && (
            <span className="mono inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-sm border border-accent-line bg-accent-soft px-1.5 py-0.5 text-[10.5px] text-accent">
              {phaseLabel}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1 text-ink-3">
          {cancelEnabled && (
            <button
              type="button"
              onClick={cancelTurn}
              className="mr-1 inline-flex items-center gap-1.5 rounded-sm border border-bad/40 bg-paper-2 px-2 py-1 text-[11px] text-bad"
              title="Cancel current turn (Esc-equivalent)"
            >
              Cancel turn
            </button>
          )}
          {!termOpen && (
            <button
              type="button"
              onClick={() => setTermOpen(true)}
              className="mr-1 inline-flex items-center gap-1.5 rounded-sm border border-rule-2 bg-paper-2 px-2 py-1 text-[11px] text-ink-2"
              title="Show terminal"
            >
              <IconTerm size={13} /> Terminal
            </button>
          )}
          <button
            type="button"
            className="grid h-7 w-7 place-items-center rounded-md text-ink-3"
            title="Share"
          >
            <IconCopy size={14} />
          </button>
          <button
            type="button"
            className="grid h-7 w-7 place-items-center rounded-md text-ink-3"
            title="More"
          >
            <IconMore size={14} />
          </button>
        </div>
      </header>

      <div ref={chatBodyRef} className="flex min-h-0 flex-1 flex-col">
        <div ref={scrollRef} className="flex-1 overflow-y-auto px-8 pb-5 pt-6">
          <ChatBanner connection={connection} hasMessages={messages.length > 0} />
          {messages.map((m) => (
            <Message
              key={m.id}
              message={{
                id: m.id,
                role: m.role === "user" ? "user" : "assistant",
                time: m.time,
                body: m.body,
                fullBody: m.fullBody,
                hiddenChars: m.hiddenChars,
                streaming: m.streaming,
                kind: m.kind,
                title: m.title,
              }}
            />
          ))}
          <InteractionPanel />
        </div>
        <Composer
          onSubmit={submit}
          disabled={!canSubmit}
          disabledReason={composerDisabledReason ?? undefined}
        />
      </div>

      {termOpen && (
        <>
          <Splitter
            orientation="horizontal"
            value={termH}
            onDrag={onTerminalDrag}
          />
          <Terminal height={termH} onClose={() => setTermOpen(false)} />
        </>
      )}
    </section>
  );
}

function ChatBanner({
  connection,
  hasMessages,
}: {
  connection: ReturnType<typeof useCodeUiStore>["connection"];
  hasMessages: boolean;
}) {
  if (connection.kind === "loading") {
    return <DividerLine label="connecting to libra code session…" />;
  }
  if (connection.kind === "reconnecting") {
    return (
      <DividerLine
        label={`reconnecting (attempt ${connection.attempt})`}
      />
    );
  }
  if (connection.kind === "unavailable") {
    return (
      <DividerLine label="no active libra code session — start `libra code` to begin" />
    );
  }
  if (!hasMessages) {
    return <DividerLine label="session ready · waiting for first message" />;
  }
  return null;
}

function DividerLine({ label }: { label: string }) {
  return (
    <div className="mb-[22px] flex items-center gap-2.5">
      <div className="h-px flex-1 bg-rule" />
      <div className="mono text-[11px] text-ink-3">{label}</div>
      <div className="h-px flex-1 bg-rule" />
    </div>
  );
}

function branchLabelFromStatus(
  status: ReturnType<typeof useCodeUiStore>["status"],
): string | null {
  if (!status) return null;
  if (status.head.type === "detached") {
    return `detached @ ${status.head.oid.slice(0, 7)}`;
  }
  return status.head.name;
}

function headerTitleFromSnapshotAndRepo(
  snapshot: ReturnType<typeof useCodeUiStore>["snapshot"],
  repo: ReturnType<typeof useCodeUiStore>["repo"],
): string {
  if (snapshot?.threadId) return snapshot.threadId;
  if (repo?.name) return repo.name;
  return "libra code";
}

function composerDisabledHint(
  snapshot: ReturnType<typeof useCodeUiStore>["snapshot"],
  connection: ReturnType<typeof useCodeUiStore>["connection"],
  controllerStatus: ReturnType<typeof useBrowserController>["status"],
): string | null {
  if (connection.kind !== "ready") {
    return "Reconnecting to libra code session…";
  }
  if (!snapshot) return "No active session";
  if (!snapshot.capabilities.messageInput) {
    return "This provider does not support browser-driven messages";
  }
  if (
    snapshot.controller.loopbackOnly === false &&
    !snapshot.controller.canWrite
  ) {
    return "Browser writes are disabled outside loopback";
  }
  if (snapshot.controller.kind !== "none" && !snapshot.controller.canWrite) {
    return `Read-only — controlled by ${snapshot.controller.kind}${
      snapshot.controller.ownerLabel ? ` (${snapshot.controller.ownerLabel})` : ""
    }`;
  }
  if (controllerStatus.kind === "error") {
    if (controllerStatus.code === "BROWSER_CONTROL_DISABLED") {
      return "Browser control disabled — restart `libra code` with `--browser-control loopback`";
    }
    return `${controllerStatus.code}: ${controllerStatus.message}`;
  }
  if (snapshot.status === "thinking" || snapshot.status === "executing_tool") {
    return "Agent is busy — wait or use Cancel turn";
  }
  return null;
}

function derivePhaseLabel(
  snapshot: ReturnType<typeof useCodeUiStore>["snapshot"],
): string | null {
  if (!snapshot) return null;
  switch (snapshot.status) {
    case "thinking":
      return "Thinking";
    case "executing_tool":
      return "Executing";
    case "awaiting_interaction":
      return "Awaiting input";
    case "completed":
      return "Completed";
    case "error":
      return "Error";
    case "indeterminate_side_effect":
      return "Reconciliation required";
    case "idle":
    default:
      return null;
  }
}
