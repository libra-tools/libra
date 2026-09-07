# `libra code-control` (removed)

> **Breaking change (W5-01):** the deprecated `libra code-control` forwarding
> shim was **physically removed** in the W5 breaking release. The binary now
> rejects `code-control` as an unknown command (`libra: 'code-control' is not a
> libra command.`, exit 129). This page is kept as a migration note only; it no
> longer describes an available command.

## Migration

Use the canonical stdio automation client. It speaks the same newline-delimited
JSON-RPC 2.0 protocol as the removed shim and additionally discovers the
endpoint from `.libra/code/control.json` by default:

| Removed invocation | Replacement |
|---|---|
| `libra code-control --stdio --url <baseUrl> --token-file <path>` (the shim required both flags) | `libra code --control stdio --control-url <baseUrl> --control-token-file <path>` |
| `libra code-control --stdio --url $(jq -r .baseUrl .libra/code/control.json) --token-file .libra/code/control-token` | `libra code --control stdio` (discovers `.libra/code/control.json` by default; override with `--control-url` / `--control-token-file` / `--control-info-file`) |

The JSON-RPC methods (`controller.attach`, `message.submit`,
`events.subscribe`, `diagnostics.get`, …) and the JSON-RPC error mapping are
unchanged; they are documented in [`code.md`](code.md) under "Local Automation
Control". `events.subscribe` explicitly requests SSE wire v2 and accepts an
optional last-acknowledged cursor (`?wire=2&cursor=<last>`); omitted params are
the cursor-0 bootstrap. Cursors are session-scoped. A v2 resync fetches one
session snapshot and reconnects at the server-provided durable tail; this marks
a workflow-event gap, so consumers must reconcile snapshot state and deduplicate
side effects by event ID. An ahead cursor is dropped after the same snapshot
recovery and v2 restarts from zero. If the server has no durable session store and returns
`WIRE_V2_REQUIRES_DURABLE_SESSION`, that error is terminal: the legacy v1
fallback was removed in 0.22.0 together with the server-side v1 wire (the
server's omitted-wire default has been v2 since DF-06). Do not confuse
`--control stdio` with the deprecated MCP-only
`libra code --stdio` transport (tools/resources; a dedicated
`libra mcp --stdio` is planned after W5, DEFER-02).

## Examples

```bash
# Discover endpoint/token from .libra/code/control.json (preferred)
libra code --control stdio

# Explicit endpoint/token (replaces the removed --url/--token-file spelling)
libra code --control stdio \
  --control-url http://127.0.0.1:3000 \
  --control-token-file .libra/code/control-token
```
