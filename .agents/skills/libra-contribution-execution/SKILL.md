---
name: libra-contribution-execution
description: Implement and deliver a Libra task card selected by the user with implementation authorization. Use for code or documentation changes, focused verification, plan status updates, and PR preparation. Do not use for issue triage or deciding what work should be planned.
---

# Libra Contribution Execution

Implement the selected, authorized task card and prepare a reviewable contribution. Follow current repository policy and report actual results. If scope expands, dependencies change, or write sets overlap, stop the affected work and request a maintainer decision before proceeding.

## Confirm the authorized task

Before edits, inspect the current checkout and read:

- `AGENTS.md` for repository rules, validation, and version-control workflow.
- The selected task card and its plan/status entries.
- Relevant source, tests, docs, and compatibility behavior.
- Any linked GitHub Issue and the Issues for its declared prerequisites.

Refresh anchors against the current checkout. Confirm the user selected the card and authorized implementation. Preserve existing work and identify unrelated changes before editing. Follow the card's scope and write set. If the task is not clearly selected or authorized, return to plan/task authoring rather than inferring permission from a draft.

## Preflight the Issue, card, and prerequisites

The user may identify the work by a task card or a GitHub Issue. Resolve either reference to the matching card and Issue before editing: when given an Issue, find its planned task card; when given a card, find its linked Issue. Confirm that the Issue exists and that its title, scope, and acceptance criteria agree with the card. If there is no linked Issue, say so; if the user wants an Issue drafted, recommend `libra-issue-triage`. If a supplied Issue has no matching card, report that and guide the user to `libra-work-discovery` to check for related or already planned work, then `libra-plan-task-authoring` to select an existing card or prepare a proposal for a new one. Do not start implementation until a suitable card is selected and authorized; an Issue alone does not replace those requirements.

Check each declared prerequisite against the plan/status entries and its corresponding Issue, including whether the work and acceptance criteria are complete. Do not treat a closed Issue alone as proof that a prerequisite is complete when the plan or its acceptance criteria still show pending work. When the Issue and card/plan statuses disagree, name the conflicting statuses and give a concrete reconciliation recommendation: if an Issue is closed while its card is active, verify acceptance and either mark the card/plan complete or correct/reopen the Issue and keep the work active or blocked; if a card/plan is complete while its Issue is open, verify acceptance and recommend updating/closing the Issue when authorized, or correcting the card/plan status if work remains. For scope or acceptance mismatches, recommend aligning the Issue and card before implementation. Do not silently change card or Issue state. If a prerequisite is unresolved, or a mismatch affects scope or readiness, stop dependent implementation and ask for the plan or issue state to be reconciled. Otherwise, briefly record what was checked and proceed.

If the mismatch comes from an unclear problem statement or expected behavior, recommend `libra-issue-triage` to clarify the Issue brief. If the Issue/card mapping, lifecycle, acceptance, dependency, or plan state needs assessment or repair, recommend `libra-plan-task-authoring`; use `libra-work-discovery` when the user needs a read-only search or progress report. Resume implementation only after the discrepancy and readiness gates are resolved and the user has authorized the selected card.

## Branch and worktree context

This skill does not require a particular branch, branch name, or base, and does not require switching before implementation. Respect the branch the contributor is already using unless the contributor, task card, or current repository policy specifies a different branch operation.

Inspect `libra status --short --branch` before edits. If the current branch is `main`, warn that implementation would be on the main branch and recommend creating a task branch with `libra switch -c codex/<task-id>` (replace `<task-id>` with the card ID or a short task name). If the worktree is dirty, list the changed paths, identify unrelated changes, warn that they remain in the worktree, and give the same branch-creation recommendation. Creating a branch does not separate or discard existing working-tree changes; preserve them and do not mix unrelated edits into the task. Do not create or switch branches automatically. If the branch choice remains material and unspecified, ask before proceeding with edits. When a branch operation is explicitly required, use Libra commands and verify the result; do not use raw Git for repository state in this Libra checkout. Read-only investigation does not require a branch change.

## Implement and verify

1. When beginning implementation of the authorized card, first update the task card's `Lifecycle` to `in-progress` (e.g., `Lifecycle / Acceptance: in-progress / 空` or matching current acceptance) and record in `plan-status.md` that the task is in-progress on its task branch with a UTC timestamp. This claims and locks the card's declared `Implementation write set` under repository governance (ER-03 / G-10), ensuring other agents preflighting their tasks are blocked from concurrent edits to the same write set.
2. Follow the card's scope and write set. If scope expands, dependencies change, or write sets overlap, update the plan or resolve sequencing with the maintainer first.
3. Add or update focused tests and user documentation as required by the card and repository rules.
4. Run the exact verification required by the card and current repository guidance. Follow current ER-13/ER-14 rules; do not copy obsolete commands from old plans.
5. Report actual results. Do not mark a card complete while acceptance, release, or remote gates remain pending.
6. Update the card and `plan-status.md` in the same change when lifecycle, acceptance, dependency, or deferred status changes, marking `Lifecycle / Acceptance: done / complete` only when all required verification, acceptance criteria, and closure gates are met.

Use the project's Libra command workflow for repository operations. Follow current DCO/signing and commit/push instructions in `AGENTS.md`. Do not commit, push, merge, tag, or publish unless the user explicitly authorized that operation.

## PR and GitHub interactions

Prepare a concise PR description with intent, related Issue(s), changes, tests actually run, and reproduction or sample output for user-visible behavior. Use read-only `gh issue view` queries when needed to verify the selected card's linked Issue and prerequisite status; check `gh` availability and authentication first, and report when remote state cannot be verified. Use `gh` for other live GitHub actions only when the user explicitly authorizes that action. Do not post Issue comments outside authorized Q&A, assign or label Issues, close Issues, merge, push, tag, or publish a release without explicit authorization for that operation.

After review feedback, map requested changes to the relevant card and acceptance criteria. Keep local plan/card status separate from GitHub Issue/PR status and report unsynchronized state. Explain blockers and the decision needed; do not silently broaden the task.

When the implementation is locally complete but related Issue or plan progress remains unclear, recommend `libra-work-discovery` for a read-only status pass. Do not close or otherwise update a GitHub Issue without explicit authorization.

## Response

Summarize the selected card, files changed, verification actually run and results, remaining acceptance or release gates, and any local versus external status. Do not claim unrun checks passed or mark incomplete work complete.
