# Libra Code Web UI

This directory holds the Next.js source for the embedded `libra code` browser UI. The build is consumed two ways:

1. **`pnpm dev`** during local development serves the UI on the Next.js dev server's default `http://localhost:3000`. All API calls use relative `/api/...` paths with `same-origin` credentials (see `src/lib/code-ui/client.ts`), so the dev server must share its origin with a running `libra code` process. The typical workflow is to launch the backend on a non-default port — `libra code --port 4400` — and then run `pnpm dev -- --port 4400` so both share the loopback origin. There is **no** `LIBRA_DEV_API_BASE`-style env-var-based proxy: the client speaks to `/api/*` directly and the Rust side's `ensure_loopback_api_request` guard refuses remote callers regardless.
2. **`pnpm build`** emits a static export to the ignored `web/out/` directory. The Rust binary embeds that directory at compile time via `WebAssets` (`src/command/web_assets.rs`) and serves it from `axum::Router::fallback`. `build.rs` generates it when Cargo needs to rebuild the frontend; CI verifies a fresh export can be embedded. Do not commit `web/out/`.

## Scripts

```bash
pnpm install        # install deps (uses pnpm-lock.yaml)
pnpm dev            # local dev server with HMR
pnpm lint           # eslint, no warnings allowed
pnpm test           # vitest unit tests for the browser foundation
pnpm test:e2e       # Playwright real-browser suite (W3-15; needs live runtime)
pnpm build          # static export → web/out/
```

### Playwright e2e (W3-15)

Specs live under `web/e2e/` and assert only through user-visible DOM against an
**already-started** deterministic `libra code` (default Web) process (they do not
spawn the runtime and must not import frontend stores).

```bash
# Terminal A — deterministic fake-provider Web runtime
./web/e2e/scripts/start-deterministic-runtime.sh
# prints LIBRA_E2E_BASE_URL=http://127.0.0.1:4410

# Terminal B — browsers + suite (fail-closed for completion evidence)
pnpm --dir web exec playwright install chromium
export LIBRA_E2E_BASE_URL=http://127.0.0.1:4410
export LIBRA_E2E_REQUIRE=1
pnpm --dir web test:e2e
```

Without `LIBRA_E2E_REQUIRE=1`, a missing Chromium or unreachable `/api/health`
prints `skip: …` and exits 0 for local diagnosis only — that soft-skip is **not**
Checkpoint C / W3-15 completion evidence. Artifacts on failure:
`web/test-results/` (screenshots/traces/videos) and `web/playwright-report/`
(HTML). Both are gitignored; delete them after triage.

## Current UI status (W2-07 … W2-16 / W3-07)

The shipped page mounts the shared session/SSE store and browser-controller
lease provider, domain panels through W2-15, workflow review
(`SessionWorkflow`: IntentSpec confirm/modify/cancel, Plan execute/modify/cancel,
and network-policy allow/deny/back as an explicit human gate), and a capability-gated
message composer (`SessionComposer`) with optional `commandId` retention when
`commandIdempotency` is advertised. It does not yet ship a three-pane workspace
or terminal.

W2-16 owns workflow under `web/src/lib/code-ui/workflow/` and
`web/src/components/workspace/workflow/` without modifying W2-07 root adapter
files. Phase 2 frontend domain cards are complete; real browser e2e is W3-15.

## Live API contract

The browser only talks to its same-origin server. The Rust side enforces loopback at every `/api/*` route, so this client does not host-check. Non-loopback HTML navigation receives the embedded `remote-notice/` static page instead of the SPA, and non-loopback asset/API fallbacks return 404. Source of truth: `src/internal/ai/web/mod.rs`.

| Endpoint | Verb | Purpose |
|----------|------|---------|
| `/api/health` | GET | Liveness probe — returns plain `"ok"`. Cheapest sanity check that the embedded server is bound. |
| `/api/repo` | GET | Repository identity (`id`, `name`, `description`). |
| `/api/repo/status` | GET | Working-tree status — same JSON envelope as `libra status --json` (`{ ok, command: "status", data }`). |
| `/api/code/session` | GET | Initial `CodeUiSessionSnapshot`. |
| `/api/code/thread-graph?threadId=` | GET | Indexed Intent/Plan/Task/Run/PatchSet graph for one thread (`threadId` must be a UUID). Loopback observe; 404 `THREAD_GRAPH_NOT_FOUND` when the thread is missing, 500 `THREAD_GRAPH_UNAVAILABLE` / `THREAD_GRAPH_STORAGE_UNAVAILABLE` / `REDACTION_FAILED` on loader or redaction failure. |
| `/api/code/events` | GET (SSE) | `session_updated` / `status_changed` / `controller_changed` frames; server lag emits a full `session_updated` snapshot, and clients fall back to `GET /api/code/session` on disconnect. |
| `/api/code/threads?limit&offset` | GET | Active thread projections for the sidebar (`{ items, nextOffset }`). |
| `/api/code/diagnostics` | GET | Redacted runtime info (PID, ports, log file, controller). |
| `/api/code/controller/attach` | POST | Issue a lease (`{ clientId, kind: "browser" }`). Returns `controllerToken`. |
| `/api/code/controller/detach` | POST | Release the lease (header `X-Code-Controller-Token`). |
| `/api/code/messages` | POST | Submit a user message (header `X-Code-Controller-Token`, body ≤256 KiB). |
| `/api/code/interactions/{id}` | POST | Resolve a pending `CodeUiInteractionRequest`. |
| `/api/code/control/cancel` | POST | Cancel the active turn. Browser leases need only the controller token; automation leases additionally require `X-Libra-Control-Token`. |
| `/api/code/goal/start` | POST | Start a Goal (`{ objective }`, header `X-Code-Controller-Token`) → `{ accepted, status }`. |
| `/api/code/goal/status` | GET | Observe the active Goal status text (`{ status }`). No controller token; empty sessions return a no-active-Goal error treated as empty UI state. |
| `/api/code/goal/cancel` | POST | Cancel the active Goal (`{ reason }`, header `X-Code-Controller-Token`) → `{ accepted, status }`. |
| `/api/code/task/dispatch` | POST | Dispatch a user-initiated sub-agent task (`{ agent, prompt }`, header `X-Code-Controller-Token`) → `{ accepted, result }`. |

The wire types are pinned in two places — keep them in lock-step:

- TypeScript: `web/src/lib/code-ui/types.ts`.
- Rust: `src/internal/ai/web/code_ui.rs` (`#[serde(rename_all = "camelCase")]` on every struct, `#[serde(rename_all = "snake_case")]` on every enum). The serde golden tests in `tests/ai_code_ui_wire_test.rs` fail loudly when the JSON shape drifts.

## Module layout

```
web/src/
├── app/                       # Next.js app router entry (mounts domain panels)
├── components/workspace/
│   ├── interactions/          # Approval + request_user_input panels (W2-08)
│   ├── goal-task-skill/       # Goal / task / skill panels (W2-09)
│   ├── session-lifecycle/     # Thread list / resume / cancel (W2-10)
│   ├── usage/                 # Usage cumulative / turn / sub-agent (W2-13)
│   ├── execution-repair/      # Plan execution + repair projection (W2-14)
│   ├── sse-resilience/        # SSE reconnect / resync banners (W2-15)
│   ├── thread-graph/          # Indexed thread version graph (W4-04)
│   └── workflow/              # IntentSpec / Plan / network policy (W2-16)
└── lib/
    └── code-ui/               # Shared wire types, client, store, controller (W2-07)
        ├── interactions/      # Approval/user-input helpers + fixtures (W2-08)
        ├── goal-task-skill/   # Goal/task API + A0-07 skill helpers (W2-09)
        ├── session-lifecycle/ # Threads API + resume affordance (W2-10)
        ├── usage/             # W2-12 RuntimeUsageTotals mirror + fixtures (W2-13)
        ├── execution-repair/  # Repair view helpers + fixtures (W2-14)
        ├── sse-resilience/    # Fixture-driven SSE status reducer (W2-15)
        ├── thread-graph/      # Thread graph view helpers + fixtures (W4-04)
        └── workflow/          # Workflow review helpers + fixtures (W2-16)
```

`web/src/lib/code-ui/store.tsx` owns the `CodeUiSessionSnapshot` and the SSE reconnect loop. `web/src/lib/code-ui/controller.tsx` owns the browser controller lease. `app/page.tsx` mounts both providers once and renders the domain Session* panels through W2-16.

## Browser write surface

Composer and other domain writers will continue to land in later W2 cards.
Approval, `request_user_input`, Goal start/cancel, task dispatch, and turn
cancel already flow through `useBrowserController()` via `SessionInteractions` /
`SessionGoalTaskSkill` / `SessionLifecycle`: writes post with
`X-Code-Controller-Token` after `withLease` recovery. Skill buttons only
validate the A0-07 curated registry until W3-01 exposes skill HTTP. Thread
resume remains process-level (`libra code --resume`) until W3-01. On the first
write the hook calls `POST /api/code/controller/attach`, caches
`controllerToken` + `leaseExpiresAt` in memory, and replays the original
request. The browser `clientId` is persisted in `sessionStorage` so an
unexpected reload can renew the same lease; an explicit detach still clears
write access for a clean hand-off.

Recovery semantics in `controller.tsx`:

- `MISSING_CONTROLLER_TOKEN` / `INVALID_CONTROLLER_TOKEN` — clear cache, retry once.
- `CONTROLLER_CONFLICT` — surface the current owner; do not loop on retry.
- `BROWSER_CONTROL_DISABLED` — show a hint pointing to the `--browser-control loopback` CLI flag.
- `PAYLOAD_TOO_LARGE` — surfaced inline; the client also caps body at 256 KiB before posting.

`beforeunload` issues a best-effort `fetch("/api/code/controller/detach", { keepalive: true })` so the next browser session can attach without bumping into a stale lease. `navigator.sendBeacon` cannot set custom headers and is therefore not used for the detach call.

## Capability gating

Every writable control is gated on `snapshot.capabilities.*`. First write uses
`withLease` to attach (composer send and cancel do **not** require projected
`controller.canWrite` beforehand). Managed Codex ask-mode approvals are
browser-writable after W3-07 (`browserInteractionRespondSupported` always allows
respond; the Rust sidecar forwards `respond_interaction` to the app-server). The
current capability set is set by the Rust runtime: Web `--provider codex`
advertises the full set, `HeadlessCodeRuntime` advertises `messageInput` +
`streamingText` + `toolCalls`, and the read-only placeholder advertises none.

## Dev tips

- `pnpm dev` does not embed assets into the Rust binary; you'll see "Loading…" placeholders for any feature that depends on a live `libra code` API. Run a TUI session in another terminal so the SSE channel has data to stream.
- `cargo build` invokes `pnpm build` through `build.rs` unless `LIBRA_SKIP_WEB_BUILD=1` is set. Run `pnpm build` directly for fast frontend feedback, then run `cargo build` to embed the fresh export; `web/out/` remains generated and ignored.
- The static export needs `output: "export"`, `trailingSlash: true`, and `images.unoptimized` (configured in `next.config.ts`). Don't toggle these without updating `WebAssets` accordingly.
