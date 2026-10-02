# `libra mega2 browser`

Browse **one directory level** of a remote Mega2 repository over HTTP — either
interactively in the terminal, or as a single machine-readable listing for
scripts and agents.

`mega2` is a Libra-only extension. It has **no Git-equivalent contract**: it
never clones, fetches or pushes Git objects, and it lists remote metadata rather
than local tree objects. For local object inspection use
[`libra ls-tree`](ls-tree.md).

## Synopsis

```
libra mega2 browser --server <BASE-URL> [PATH] [--ref <COMMIT-OR-TAG>] [--json|--machine]
```

## Description

The command validates every input **before** touching the terminal or the
network:

- `--server` must be `https://…`, or `http://…` only when the host is loopback
  (`127.0.0.1`, `::1`, `localhost`). Credentials (userinfo), query strings,
  fragments and a base path are rejected; the client always appends
  `/api/v1/tree`.
- `PATH` must be rooted (`/` by default) and may not contain `.`/`..`
  components, NUL, control characters or platform separators.
- `--ref` selects a commit or tag to list; when omitted the server's default
  revision is used.

Browsing sends exactly one anonymous `GET /api/v1/tree` per navigation or
reload. No `Authorization` header is attached, no repository is opened (the
command works outside a repository), and no local state — index, object store,
database, configuration — is read or written.

The client is bounded: it disables redirects and proxies, applies a 10-second
timeout, caps the response body at 1 MiB and the listing at 2000 entries, and
accepts only entries whose `content_type` is `directory` or `file`. Names with
`..`, path separators or terminal control characters are refused
fail-closed, and the server-provided item `path` is never used as navigation
authority.

### Interactive mode (default)

Interactive mode requires **stdin and stdout to be terminals**. If either is
not a TTY the command refuses immediately with a stable error and a hint to use
`--json`; it never alters the terminal.

While browsing, the terminal is switched to raw mode and driven by a bounded
state machine with no recursion and no background prefetch:

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | Move the selection |
| `Enter` | Open the selected directory (one fetch of the child path) |
| `Backspace` or `h` | Go to the parent directory (never above `/`) |
| `+` | Create a directory here (see below) |
| `d` | Delete the selected directory (extra confirmation line; files are inert) |
| `m` | Move the selected directory under an edited destination parent |
| `R` | Rename the selected directory (same parent; `r` still reloads) |
| `t` | Toggle the tag panel (see below) |
| `r` | Reload the current listing |
| `q` (or `Ctrl-C`, `Esc`) | Quit |

Terminal state (raw mode and the alternate screen) is restored on every exit
path, including errors and signals handled by the process.

### Machine mode (`--json` / `--machine`)

With `--json` (or `--machine`, which implies `--json=ndjson --no-pager
--color=never --quiet`) the command performs exactly **one** fetch and prints
the standard Libra JSON envelope:

```json
{
  "ok": true,
  "command": "mega2 browser",
  "data": {
    "server": "https://mega2.example.com",
    "ref": "v1.2",
    "path": "/src",
    "items": [
      { "name": "pkg", "content_type": "directory" },
      { "name": "main.rs", "content_type": "file" }
    ]
  }
}
```

`items` is deterministic: directories first, then names in ascending order.
`server` is the canonical scheme/host/port origin of the validated URL.

`--quiet` without a machine output mode is rejected: suppressing stdout would
break both interactive rendering and machine consumers.

### Creating a directory (`+`, interactive only)

Press `+` to open a single-line name editor for a new subdirectory of the
current path (at the root the parent sent is `/`). While a **file** is
selected the editor refuses to open — selection must be a directory or an
empty area. `Enter` validates the name with the same rules used for the wire
(`/`, `\`, `.`, `..`, NUL and control characters are refused) and only then
performs **one** `POST /api/v1/create-entry` with `is_directory=true`,
`skip_build=true` and no `content`; `Esc` cancels with no network at all.

A confirmed creation reloads the current listing exactly once. Failures
(401/403, duplicate name, timeout, malformed response) keep the last safe
listing on screen and show a secret-free status line; the terminal is never
left in raw mode and the TUI never asks you to type a raw token on the
alternate screen.

Write tokens are read, in order of precedence, from `--token-file <path>`,
then the `LIBRA_MEGA2_TOKEN` environment variable, then `--token` (visible in
shell history — prefer the first two). Token flags are TUI-only: combining
them with `--json`/`--machine` is refused, and machine mode never POSTs.

### Deleting, moving and renaming (`d`, `m`, `R`, interactive only)

`d` asks for an extra confirmation line before deleting the selected
directory; `m` moves it under an edited destination parent path; `R` renames
it in place (the same-parent form of the move operation). Files are inert for
all three keys, `Esc` cancels every editor without any request, and hostile
destinations (`..`, separators, unrooted paths) are refused before the POST.

Each confirmed mutation performs **one** `POST /api/v1/delete-entry` or
`POST /api/v1/move-entry` followed by **one** listing reload. Success clears
the status; failures (401/403, missing source, duplicate destination,
timeout) keep the last safe listing and show a secret-free status line while
the terminal stays intact. There is no multi-select and no recursion: one
selected directory per operation.

### Tag panel (`t`, interactive only)

`t` opens the tag panel and performs exactly **one** anonymous
`GET /api/v1/tags/list` for page 1 (the three required query keys are
`page`, `per_page`, `path`; this MVP always uses the repository root
`path=/`). `n`/`p` request the explicit next/previous page — one request per
key, never prefetched. `+` collects a tag name and then an optional message
(an empty message creates a lightweight tag; a non-empty one creates an
annotated tag); `d` deletes the selected tag after an extra confirmation
line. `t`, `Esc` or `q` close the panel without leaving the browser, and the
directory listing reappears unchanged.

Tag names are validated with the server's rules before any request (non-empty,
at most 255 bytes, no `..`, `@{`, `//`, no `.lock` suffix and no
whitespace/control/forbidden characters). Listing is anonymous; create and
delete reuse the session write token from ADR-MB-03. Rendered tag fields
(tagger, message) are sanitized so hostile server strings cannot control the
terminal. The panel operates root tags only.

## Options

| Option | Description |
|--------|-------------|
| `--server <BASE-URL>` | Mega2 server base URL (required). HTTPS, or loopback HTTP. |
| `[PATH]` | Rooted directory path to list; defaults to `/`. |
| `--ref <COMMIT-OR-TAG>` | Optional commit or tag to list. |
| `--token-file <PATH>` | Interactive only: read the write token from a file (highest precedence). |
| `--token <TOKEN>` | Interactive only: inline write token (lowest precedence; visible in shell history). |
| `--json[=<FORMAT>]` | Global flag: one fetch, JSON envelope (`pretty`/`compact`/`ndjson`). |
| `--machine` | Global flag: strict NDJSON machine mode for automation. |

## Errors

| Situation | Stable behavior |
|-----------|-----------------|
| Invalid invocation (bad URL, unrooted path, missing `--server`) | Usage error, no request |
| Interactive mode without a TTY | Refused before any terminal change or request; hint to use `--json` |
| Network unavailable / timeout | Stable network error, no response body echoed |
| HTTP 4xx/5xx or a redirect | Stable error naming the status; the body is never printed |
| Malformed or hostile server response | Stable protocol error; nothing is rendered or cached |

Errors never echo server response bodies, credentials, tokens or unvalidated
paths.

### Machine error details

Every failed Mega2 request adds `details` to the JSON error envelope (on stderr
with `--json`/`--machine`, and on the trailing JSON line when stderr is not a
terminal). The stable code and message stay as listed above; `details` lets
automation tell HTTP statuses apart without parsing the message:

| Key | Meaning |
|-----|---------|
| `method` | `GET`, `POST` or `DELETE` |
| `route` | Route template, for example `/api/v1/tree` or `/api/v1/tags/{name}` |
| `http_status` | Status received — also for a `2xx` whose body is invalid, or a connection that drops while the body is read |
| `transport` | `timeout`, `connect` or `request` when no status was received |

```json
{"ok":false,"error_code":"LBR-NET-002","category":"network","exit_code":128,"severity":"fatal","message":"mega2 server returned HTTP 500 Internal Server Error","details":{"http_status":500,"method":"GET","route":"/api/v1/tree"}}
```

Invalid input rejected before any request carries no `details`. The values
never include the response body, the server URL or a token; see
[error codes](../error-codes.md#command-specific-details).

## Limits and boundaries

- One request per navigation/reload; no recursion, no prefetch, no background
  task, no cache that outlives the process.
- Browse is read-only and anonymous. A confirmed `+` adds at most one
  `create-entry` POST plus one reload GET; `d`/`m`/`R` likewise add one
  `delete-entry`/`move-entry` POST plus one reload GET. No recursive or
  multi-selection operation exists in this command.
- No configuration or credential persistence: nothing written to disk.

## Examples

```bash
# Browse the remote root interactively
libra mega2 browser --server https://mega2.example.com

# Open a rooted path directly
libra mega2 browser --server https://mega2.example.com src/pkg

# List a specific commit or tag
libra mega2 browser --server https://mega2.example.com --ref v1.2

# Create, delete, move or rename directories interactively (press +, d, m or R; t for tags)
libra mega2 browser --server https://mega2.example.com --token-file ~/.mega2-token

# Exactly one fetch, JSON envelope (works without a TTY, outside any repository)
libra --json mega2 browser --server https://mega2.example.com

# NDJSON for automation
libra --machine mega2 browser --server http://127.0.0.1:8080
```

## Comparison with `libra ls-tree`

| Aspect | `libra mega2 browser` | `libra ls-tree` |
|--------|----------------------|-----------------|
| Data source | Remote Mega2 HTTP API (`/api/v1/tree`) | Local object database |
| Requires a repository | No | Yes |
| Depth | Exactly one directory level per fetch | Arbitrary tree paths (`-r` for recursion) |
| Auth | Anonymous (no token) | Local repository access |
| Git compatibility | None (Libra-only extension) | Git-compatible plumbing |
