# `libra service`

Headless local service: notification bus + dirty-mark ingestion (a Libra
extension, lore.md §1.11). Git has no equivalent.

## Synopsis

```
libra service run [--host <LOOPBACK-IP>] [--port <PORT>]
libra service status
libra service events
```

## Description

`libra service run` starts a foreground, **local-only** HTTP service:

- `--host` must be a literal loopback IP (`127.0.0.0/8` or `::1`) — hostnames
  and non-loopback IPs are refused (exit 129). The service never opens an
  outward TCP port; every endpoint additionally rejects non-loopback peers.
- `--port` defaults to `0` (OS-assigned); the real address is published in
  `.libra/service/service.json`. One instance per repository
  (`service.lock`); stale locks from dead processes are reclaimed.
- Stop with Ctrl-C (or SIGTERM on Unix) — shutdown removes the discovery
  file and releases the lock.

**Endpoints** (all loopback-checked; data-carrying ones require the
`X-Libra-Service-Token` header matching the 0600 file
`.libra/service/service-token` — other local users are not trusted):

| Endpoint | Auth | Purpose |
|---|---|---|
| `GET /api/health` | loopback | Liveness probe. |
| `GET /api/service/events` | token | SSE notification stream. |
| `POST /api/service/dirty/mark` | token | `{"paths":[...]}` — advisory dirty marks through the validated owner API (whole batch refused if any path escapes the repo; over-report-only). In a multi-worktree repository (or when the worktree registry is corrupt/unreadable — including parseable states without exactly one main entry) scope-less requests are rejected with **409** — the dirty cache is per-worktree and the caller's scope is unknown; run `libra dirty <paths>` in the target worktree instead. |
| `POST /api/service/notify` | token | `{"type":"...","data":{...}}` — publish a custom notification (automation triggers). |

**Notification v1 semantics**: events are `{seq,type,at,data}` with `seq`
monotonic per service run. Delivery is **at-most-once** — a lagging consumer
receives a `resync` event and should re-read authoritative state
(`libra dirty --list`, `libra status`); `seq` restarts when the service
restarts. The durable facts (the marks) live in SQLite and survive `kill -9`;
everything on the bus is derivable. Request bodies are capped at 256 KiB.

`libra service status` reports the running instance (pid, URL, health; exits
1 when none). `libra service events` tails the stream (human lines; NDJSON
under `--json`; exits cleanly when the server goes away).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | `status` with no running instance. |
| `128` | Not a repository. |
| `129` | Usage errors (non-loopback `--host`). |

## Examples

```bash
libra service run                      # loopback, OS-assigned port
libra service status                   # pid, URL, health
libra service events                   # tail notifications
TOKEN=$(cat .libra/service/service-token)
URL=$(libra --json service status | jq -r .data.base_url)
curl -H "X-Libra-Service-Token: $TOKEN" -X POST "$URL/api/service/dirty/mark" \
     -H 'content-type: application/json' -d '{"paths":["src/main.rs"]}'
```

## Comparison with Git

Git has no local service surface (`git daemon` serves the wire protocol to
the network — the opposite of this design). Classified
`intentionally-different` in [`COMPATIBILITY.md`](../../COMPATIBILITY.md).
