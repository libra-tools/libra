import { expect, test, type Browser, type Page } from "@playwright/test";

import {
  e2eOrigin,
  e2eStartPath,
  submitMessage,
  waitForSessionReady,
} from "./helpers/session";

/**
 * W3-15 main chain: submit → approval/user-input → goal/task/skill → usage →
 * plan execution/repair → resume/cancel → SSE reconnect.
 *
 * Specs assert only through user-visible DOM (and the live HTTP surface the
 * page itself uses). They must not import frontend stores.
 *
 * One shared browser context keeps the controller lease across steps — a fresh
 * context per test hits CONTROLLER_CONFLICT while the prior lease is alive.
 */
test.describe("Code UI main chain (real browser)", () => {
  test.describe.configure({ mode: "serial" });

  let browser: Browser;
  let page: Page;

  test.beforeAll(async ({ browser: workerBrowser }) => {
    browser = workerBrowser;
    const origin = e2eOrigin();
    const context = await browser.newContext({
      baseURL: origin,
      extraHTTPHeaders: {
        Origin: origin,
      },
    });
    page = await context.newPage();
    await page.goto(e2eStartPath());
    await waitForSessionReady(page);
  });

  test.afterAll(async () => {
    // Best-effort lease release so a follow-up local run is not blocked.
    try {
      await page.evaluate(async () => {
        const raw = sessionStorage.getItem("libra.code.browserClientId");
        // Token is memory-only; navigate away so beforeunload keepalive may fire.
        void raw;
      });
      await page.close();
      await page.context().close();
    } catch {
      // Runtime may already be gone during teardown.
    }
  });

  test("loads shell and submits a chat turn", async () => {
    // `/run` selects the tool-capable / explicit-direct intent so the fake
    // provider can answer with the e2e_main_chain text match. A bare message
    // on the Web-default path enters Phase 0 IntentSpec review instead.
    await submitMessage(page, "/run e2e-hello from playwright");
    await expect(page.getByLabel("Transcript")).toContainText("e2e-hello", {
      timeout: 60_000,
    });
    await expect(page.getByLabel("Transcript")).toContainText("e2e hello ack", {
      timeout: 60_000,
    });
  });

  test("approval path: shell tool → Approve", async () => {
    // `/run` selects the tool-capable intent so `shell` is in allowed_tools
    // (same trigger as code_ui_remote_approval_matrix).
    await submitMessage(
      page,
      "/run approval-shell-test: trigger the shell tool to exercise the approval gate",
    );
    const approval = page.getByLabel("Approval request");
    await expect(approval).toBeVisible({ timeout: 60_000 });
    await expect(approval).toContainText(/shell|Command|Approve command/i);
    await approval.getByRole("button", { name: "Approve" }).click();
    await expect(approval).toHaveCount(0, { timeout: 60_000 });
    await expect(page.getByLabel("Transcript")).toContainText("e2e turn complete", {
      timeout: 60_000,
    });
    await expect(approval).toHaveCount(0);
  });

  test("user-input + workflow panels surface", async () => {
    // Direct fake-provider match (avoids depending on Phase-0 /plan routing on
    // the web-only path). Workflow host is always mounted once the session loads.
    await submitMessage(page, "/run e2e-user-input: ask the risk profile question");
    const userInput = page.getByLabel("User input request");
    await expect(userInput).toBeVisible({ timeout: 60_000 });
    await expect(userInput).toContainText("risk level");

    const riskSelect = userInput.getByLabel("What risk level should be used?");
    await riskSelect.selectOption("Low");
    await userInput.getByRole("button", { name: "Submit answers" }).click();
    await expect(userInput).toHaveCount(0, { timeout: 60_000 });
    await expect(page.getByLabel("Transcript")).toContainText("e2e turn complete", {
      timeout: 60_000,
    });
    await expect(userInput).toHaveCount(0);

    // WorkflowHost mounts only while an intent/plan/network review is pending;
    // the always-on execution/repair host covers the plan-projection half of AC2.
    await expect(page.getByLabel("Execution repair workspace")).toBeVisible();
  });

  test("goal / task / skill panels are interactive", async () => {
    const goal = page.getByLabel("Goal panel");
    await expect(goal).toBeVisible();
    await goal.getByLabel("Goal objective").fill("e2e goal: document playwright chain");
    await goal.getByRole("button", { name: "Start goal" }).click();
    await expect(goal).toContainText(/Goal |Objective:|accepted|running|active/i, {
      timeout: 60_000,
    });
    await expect(goal.getByRole("alert")).toHaveCount(0);

    const tasks = page.getByLabel("Task panel");
    await expect(tasks).toBeVisible();
    await tasks.getByLabel("Task agent").fill("explore");
    await tasks.getByLabel("Task prompt").fill("e2e task prompt");
    await tasks.getByRole("button", { name: "Dispatch task" }).click();
    // Explicit negative: default worktree has multi_agent disabled in agents.toml.
    await expect(tasks.getByRole("alert")).toContainText(/multi_agent\.enabled|agents\.toml/i, {
      timeout: 60_000,
    });

    const skills = page.getByLabel("Skill search panel");
    await expect(skills).toBeVisible();
    await skills.getByRole("button", { name: /Validate \/review \(claude-code\)/ }).click();
    await expect(skills.getByRole("status")).toContainText(/review|claude-code|validated/i, {
      timeout: 60_000,
    });
  });

  test("usage panel refresh stays user-visible", async () => {
    const usage = page.getByLabel("Usage workspace");
    await expect(usage).toBeVisible();
    const refresh = usage.getByRole("button", { name: "Refresh usage" });
    await expect(refresh).toBeEnabled();
    await refresh.click();
    // Wait for the busy cycle to finish so a late /api/code/usage failure
    // cannot land an alert after this test has already passed.
    await expect(refresh).toBeEnabled({ timeout: 30_000 });
    await expect(usage.getByLabel("Cumulative")).toContainText(/\d/);
    await expect(usage.getByRole("alert")).toHaveCount(0);
  });

  test("plan execution / repair projection host is mounted", async () => {
    await submitMessage(page, "/run e2e-plan-draft: project a draft execution plan");
    const progress = page.getByLabel("Execution progress panel");
    await expect(progress).toContainText("Draft execution plan", { timeout: 60_000 });
    await expect(progress).toContainText("Inspect the current Code UI planning contract");
    await expect(progress).toContainText("Expose planning draft projection in the browser");
    await expect(progress).toContainText(/Status: (running|completed|pending)/);

    await submitMessage(page, "/run e2e-update-plan: advance plan step status");
    await expect(progress).toContainText("Current plan", { timeout: 60_000 });
    await expect(progress).toContainText("completed");
    await expect(progress).toContainText("in_progress");
    await expect(progress).toContainText("Verify execution progress in the browser");

    const repair = page.getByLabel("Repair panel");
    await expect(repair).toBeVisible();
    await expect(repair).toContainText("Repair");
    // Live web-only repair gates remain GATE-WEB-PLAN; the projected plan
    // steps above are the execution half. If a repair interaction is
    // projected, Continue/Cancel must be the live controls.
    const continueRepair = repair.getByRole("button", { name: "Continue repair" });
    if (await continueRepair.count()) {
      await expect(continueRepair).toBeVisible();
      await expect(repair.getByRole("button", { name: "Cancel repair" })).toBeVisible();
    }
  });

  test("resume / cancel controls are available", async () => {
    await expect(page.getByLabel("Thread list panel")).toBeVisible();

    await submitMessage(page, "/run slow-shell-tool: hold the turn so cancel can fire");
    // Shell still needs a human approval even after an earlier Approve-once
    // (the first approval test leaves "Apply to future commands" at No).
    const executing = page.getByText(/fake-local: (executing|running|tool)/i);
    const approval = page.getByLabel("Approval request");
    await expect(approval.or(executing)).toBeVisible({ timeout: 30_000 });
    if (await approval.isVisible()) {
      await approval.getByRole("button", { name: "Approve" }).click();
    }
    await expect(executing).toBeVisible({ timeout: 30_000 });
    const cancel = page.getByRole("button", { name: "Cancel turn" }).first();
    await expect(cancel).toBeEnabled();
    await cancel.click();
    await expect(page.getByText(/fake-local: (ready|idle|cancelled)/i)).toBeVisible({
      timeout: 60_000,
    });

    const thread = page.getByLabel("Thread list").locator("button, li").first();
    if (await thread.count()) {
      await thread.click();
    }
    const resume = page.getByRole("button", { name: "Prepare resume" });
    await expect(resume).toBeVisible();
    if (await resume.isEnabled()) {
      await resume.click();
      await expect(page.getByLabel("Session resume cancel panel")).toContainText(
        /resume|thread|W3-01|working directory/i,
      );
    }
  });

  test("SSE reconnect / resync path after events stream abort", async () => {
    const sse = page.getByLabel("SSE resilience panel");
    await expect(sse).toBeVisible();
    await expect(sse).toContainText(/SSE connected/);
    await expect(sse).toContainText(/Last cursor seq|No cursor/);

    // Fail only the SSE endpoint so snapshot HTTP still bootstraps the shell,
    // then reload so a fresh EventSource hits the abort (existing streams may
    // ignore setOffline / mid-flight route aborts).
    await page.route("**/api/code/events**", (route) => route.abort("connectionreset"));
    await page.reload({ waitUntil: "domcontentloaded" });
    await waitForSessionReady(page);

    await expect(page.getByLabel("SSE resilience panel")).toContainText(
      /SSE reconnecting|SSE resync required|Resync snapshot/,
      { timeout: 60_000 },
    );

    const resync = page.getByRole("button", { name: "Resync snapshot" });
    if (await resync.count()) {
      await resync.click();
      await expect(page.getByLabel("SSE resilience panel").getByRole("alert")).toHaveCount(0);
    }

    await page.unroute("**/api/code/events**");
    // Force a new EventSource against the live stream (mid-flight abort of an
    // existing EventSource is a no-op; reconnect must observe a healthy open).
    await page.reload({ waitUntil: "domcontentloaded" });
    await waitForSessionReady(page);
    const recovered = page.getByLabel("SSE resilience panel");
    await expect(recovered).toContainText(/SSE connected|SSE resynced/, { timeout: 60_000 });
    await expect(recovered).not.toContainText("SSE reconnecting");
    await expect(recovered).toContainText(/Last cursor seq: \d+/, { timeout: 60_000 });
  });
});
