/**
 * Read-only Settings tab for the workflow pane.
 *
 * Shows the active provider/model/context, capability flags, controller
 * ownership, and the loopback-only safety badge. All values are derived from
 * the live `CodeUiSessionSnapshot`; nothing is mutable in v1 because the
 * backend has no settings-mutation endpoints yet (per the docs/development/commands/_general.md
 * Phase 4 brief).
 */
"use client";

import { useEffect, useState, type ReactNode } from "react";

import { getDiagnostics } from "@/lib/code-ui/client";
import { useCodeUiStore } from "@/lib/code-ui/store";
import type { CodeUiCapabilities, CodeUiDiagnostics } from "@/lib/code-ui/types";
import { cn } from "@/lib/utils";

const CAPABILITY_LABELS: { key: keyof CodeUiCapabilities; label: string }[] = [
  { key: "messageInput", label: "Message input" },
  { key: "streamingText", label: "Streaming responses" },
  { key: "planUpdates", label: "Plan updates" },
  { key: "toolCalls", label: "Tool calls" },
  { key: "patchsets", label: "PatchSets" },
  { key: "interactiveApprovals", label: "Interactive approvals" },
  { key: "structuredQuestions", label: "Structured questions" },
  { key: "providerSessionResume", label: "Provider session resume" },
  { key: "commandIdempotency", label: "Command idempotency" },
];

export function SettingsView() {
  const { snapshot } = useCodeUiStore();
  const [diagnostics, setDiagnostics] = useState<CodeUiDiagnostics | null>(null);

  useEffect(() => {
    let cancelled = false;
    getDiagnostics()
      .then((value) => {
        if (!cancelled) setDiagnostics(value);
      })
      .catch(() => {
        // diagnostics is a best-effort surface — the rest of the tab still
        // renders without it.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (!snapshot) {
    return (
      <div className="px-[18px] pb-6 pt-4 text-[12.5px] italic text-ink-3">
        No active libra code session.
      </div>
    );
  }

  return (
    <div className="px-[18px] pb-6 pt-4">
      <Block label="Session">
        <Row label="Working dir">
          <span className="mono">{snapshot.workingDir || "—"}</span>
        </Row>
        {snapshot.threadId && (
          <Row label="Thread id">
            <span className="mono">{snapshot.threadId}</span>
          </Row>
        )}
        {diagnostics?.ports?.web !== undefined && (
          <Row label="Web port">
            <span className="mono">{diagnostics.ports.web}</span>
          </Row>
        )}
        {diagnostics?.ports?.mcp !== undefined && (
          <Row label="MCP port">
            <span className="mono">{diagnostics.ports.mcp}</span>
          </Row>
        )}
        {diagnostics?.logFile && (
          <Row label="Log file">
            <span className="mono">{diagnostics.logFile}</span>
          </Row>
        )}
        {diagnostics?.pid !== undefined && (
          <Row label="PID">
            <span className="mono">{diagnostics.pid}</span>
          </Row>
        )}
      </Block>

      <Block label="Provider">
        <Row label="Provider">
          <span className="mono">{snapshot.provider.provider}</span>
        </Row>
        {snapshot.provider.model && (
          <Row label="Model">
            <span className="mono">{snapshot.provider.model}</span>
          </Row>
        )}
        {snapshot.provider.mode && (
          <Row label="Mode">
            <span className="mono">{snapshot.provider.mode}</span>
          </Row>
        )}
        <Row label="Managed">
          <span className="mono">{snapshot.provider.managed ? "yes" : "no"}</span>
        </Row>
      </Block>

      <Block label="Controller">
        <Row label="Owner">
          <span className="mono">
            {snapshot.controller.kind}
            {snapshot.controller.ownerLabel
              ? ` · ${snapshot.controller.ownerLabel}`
              : ""}
          </span>
        </Row>
        <Row label="Can write">
          <span
            className={cn(
              "mono",
              snapshot.controller.canWrite ? "text-good" : "text-ink-3",
            )}
          >
            {snapshot.controller.canWrite ? "yes" : "no"}
          </span>
        </Row>
        <Row label="Loopback only">
          <span className="mono">
            {snapshot.controller.loopbackOnly ? "yes" : "no"}
          </span>
        </Row>
        {snapshot.controller.reason && (
          <Row label="Reason">
            <span className="text-ink-2">{snapshot.controller.reason}</span>
          </Row>
        )}
      </Block>

      <Block label="Capabilities">
        <ul className="m-0 list-none p-0">
          {CAPABILITY_LABELS.map(({ key, label }) => (
            <li
              key={key}
              className="flex items-center justify-between border-b border-rule py-[5px] text-[12px] text-ink-2"
            >
              <span>{label}</span>
              <span
                className={cn(
                  "mono text-[10.5px]",
                  snapshot.capabilities[key] ? "text-good" : "text-ink-3",
                )}
              >
                {snapshot.capabilities[key] ? "enabled" : "disabled"}
              </span>
            </li>
          ))}
        </ul>
      </Block>

      <div className="text-[11px] italic text-ink-3">
        Settings are read-only in v1. Restart `libra code` with different flags
        to change the provider, network access, approval policy, or browser
        control posture.
      </div>
    </div>
  );
}

function Block({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="mb-5">
      <div className="mb-2 text-[10px] font-medium uppercase tracking-[0.08em] text-ink-3">
        {label}
      </div>
      {children}
    </div>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex justify-between border-b border-rule py-[5px] text-[12px] text-ink-2">
      <span>{label}</span>
      {children}
    </div>
  );
}
