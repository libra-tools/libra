import { expect, type Page } from "@playwright/test";

/** Loopback origin for the deterministic runtime (no bootstrap query). */
export function e2eOrigin(): string {
  return (process.env.LIBRA_E2E_BASE_URL ?? "http://127.0.0.1:4410").replace(/\/$/, "");
}

/**
 * Browser write attach requires the one-time `?bt=` bootstrap token minted
 * by `libra code --browser-control loopback`. Without it, composer submit
 * fails closed with `X-Libra-Browser-Bootstrap is required`.
 */
export function e2eStartPath(): string {
  const token = process.env.LIBRA_E2E_BOOTSTRAP_TOKEN?.trim();
  return token ? `/?bt=${encodeURIComponent(token)}` : "/";
}

/** Wait until the embedded shell has a live session snapshot (not Loading…). */
export async function waitForSessionReady(page: Page): Promise<void> {
  await expect(page.getByRole("heading", { name: "Libra — Agent Workspace" })).toBeVisible();
  await expect(page.getByText("Loading session…")).toHaveCount(0, { timeout: 60_000 });
  await expect(page.getByLabel("Message composer")).toBeVisible({ timeout: 60_000 });
  await expect(page.getByLabel("Session message")).toBeEnabled({ timeout: 60_000 });
}

/** Wait until the projected phase is idle/ready so the next write is accepted. */
export async function waitForTurnSettled(page: Page): Promise<void> {
  await expect(page.getByText(/fake-local: (ready|idle)/i)).toBeVisible({ timeout: 60_000 });
  await expect(page.getByLabel("Approval request")).toHaveCount(0);
  await expect(page.getByLabel("User input request")).toHaveCount(0);
}

export async function submitMessage(page: Page, text: string): Promise<void> {
  await waitForTurnSettled(page);
  const box = page.getByLabel("Session message");
  await expect(box).toBeEnabled();
  await box.fill(text);
  await expect(box).toHaveValue(text);
  const send = page.getByRole("button", { name: "Send" });
  await expect(send).toBeEnabled();
  await send.click();
  await expect(page.getByLabel("Transcript")).toContainText(text, { timeout: 60_000 });
}
