# `agent_workflows` fixtures — provenance

Deterministic fake `/bin/sh` investigator/reviewer scripts for the AG-23
`agent investigate` and AG-7 `agent review` workflow tests
(`tests/agent_investigate_workflow_test.rs`, `tests/agent_review_workflow_test.rs`).
They are driven through the `InvestigatorSource::Custom` / `ReviewerCommand`
test seam — **no network, no credentials, no real agent CLI**.

Each script is launched as `<script> [args...] <prompt>` (the review launcher
appends the prompt as the final positional argument) with stdin closed to EOF.
Stance disposition is classified from redacted stdout by
`classify_stance_disposition`, which treats the case-insensitive token
`conclud` as a concluding stance and everything else as continuing.

| fixture | role |
|---|---|
| `investigator-conclude.sh` | concluding stance (counts toward quorum); finding pins `cache.rs:42` |
| `investigator-continue.sh` | continuing stance (avoids the token `conclud`); exhausts `max_turns` / seeds prior context |
| `investigator-silent.sh` | silent successful turn → `stalled` pause (empty ≠ stance) |
| `investigator-secret.sh` | concluding stance that emits a fake `sk-` credential **assembled at runtime** (never a literal) to prove redaction |
| `reviewer-error.sh` | non-zero exit → `agent_failure` pause with a retry detail |
| `reviewer-slow.sh` | `sleep "${1:-30}"` so cancel / timeout can preempt it |

The full `sk-…` credential is intentionally never written as a literal in
`investigator-secret.sh`; it is concatenated from two harmless halves at run
time so the fixture source stays credential-free while still exercising the
redaction path end to end.
