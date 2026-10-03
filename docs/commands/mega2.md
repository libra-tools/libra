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
libra mega2 browser --server <BASE-URL> [PATH] [--ref <COMMIT-OR-TAG>] --list [--json|--machine]
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
--color=never --quiet`) and no operation flag, the command lists PATH exactly
like `--list` (see [Non-interactive operations](#non-interactive-operations)):
**one** fetch, printed as the standard Libra JSON envelope:

```json
{
  "ok": true,
  "command": "mega2 browser",
  "data": {
    "operation": "list",
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

`--quiet` without a machine output mode or an operation flag is rejected:
suppressing stdout would break interactive rendering. With an operation flag,
`--quiet` suppresses the plain-text summary.

### Non-interactive operations

Browser functions also have non-interactive forms for scripts, CI and
black-box tests: each is one operation flag on the same `mega2 browser`
command, and at most one operation flag is accepted per call. The table lists
the forms available in this release.

| Interactive key | Flag | Request |
|-----------------|------|---------|
| start, `Enter`, `Backspace`/`h`, `r` | `--list` (PATH is the directory; `--json` without a flag is the same) | `GET /api/v1/tree` |
| `+` | `--create-dir <NAME>` (PATH is the parent) | `POST /api/v1/create-entry` |
| `d` | `--delete-dir <NAME>` (PATH is the parent) | `POST /api/v1/delete-entry` |
| `m` | `--move-dir <NAME> <PARENT-PATH>` (PATH is NAME's current parent; the name is kept) | `POST /api/v1/move-entry` |
| `R` | `--rename-dir <NAME> <NEW-NAME>` (PATH is the parent, which is kept) | `POST /api/v1/move-entry` |
| `t`, then `n`/`p` | `--list-tags [--page <N>] [--per-page <N>]` (PATH must be `/`) | `GET /api/v1/tags/list` |
| `t`, then `+` | `--create-tag <NAME> [--message <TEXT>]` (PATH must be `/`) | `POST /api/v1/tags` |
| `t`, then `d` | `--delete-tag <NAME>` (PATH must be `/`) | `DELETE /api/v1/tags/{name}` |

Every non-interactive call follows the same rules:

- **No terminal, no input.** It runs with stdin closed or redirected and
  stdout/stderr piped; it never reads stdin, never prompts and never changes
  terminal state.
- **One request.** Input is validated locally first; a call that passes sends
  exactly one HTTP request and a call that fails validation sends none. There
  is no reload, preflight or retry.
- **Output.** On success stdout holds only the JSON envelope (`--json`,
  `--machine`) or a plain-text summary with control characters replaced
  (nothing with `--quiet`). On failure stdout is empty and the error goes to
  stderr: the JSON error envelope with `--json`/`--machine` (see
  [Machine error details](#machine-error-details)), the human error otherwise.
- **Credentials.** Read operations refuse token flags: they are anonymous,
  reject `--token` and `--token-file` (`LBR-CLI-002`, no request) and ignore
  `LIBRA_MEGA2_TOKEN`. Write operations, in human and machine mode alike, take
  at most one token — `--token-file`, then `LIBRA_MEGA2_TOKEN`, then `--token`
  — and send it as one `Authorization: Bearer` header; with no source they
  write anonymously (for `push_auth=none` servers). The token is never
  persisted or printed, not even when the server rejects it.
- **One operation, no `--ref` on writes.** Combining two operation flags is a
  usage error (`LBR-CLI-002`), and so is `--ref` with a write operation:
  writes always target the server's default revision. Neither sends a request.
- **Tags: root only, no revision.** Tag operations act on root tags: PATH
  must be `/` (the default), and `--ref` is refused. Both fail with
  `LBR-CLI-002` before any request.
- **Writes are not retried.** A write whose outcome is unknown is never
  repeated by Libra. That happens when it times out or fails before any
  response (`LBR-NET-001` with `details.transport` `timeout` or `request`),
  when the connection drops while the receipt is read (`LBR-NET-001` with
  `details.http_status`), or when a `2xx` carries an invalid receipt
  (`LBR-NET-002` with a `2xx` `details.http_status`). Check the result before
  repeating the call: with `--list` after a directory write; after a tag
  write, with `--list-tags`, paging until `has_next` is `false` — an annotated
  tag appears on one of the pages, but a lightweight tag can be missing from
  all of them (see the paging note under `--list-tags`). `details.transport`
  `connect` means the request never reached the server.
- **Exit codes.** `0` on success; on failure the stable code's exit code
  (`129` for usage errors such as `LBR-CLI-002` and `LBR-CLI-003`, `128`
  otherwise), or the category code (`2`–`9`) when `LIBRA_FINE_EXIT_CODES=1`.

`--list` prints one line per entry, directories first, as `dir  <name>` or
`file  <name>`; with `--json`/`--machine` it prints the payload shown above.

`--create-dir <NAME>` creates directory NAME under PATH with one
`POST /api/v1/create-entry` and does not reload. The plain-text summary is
`created directory <path> (commit <commit_id>)`; with `--json`/`--machine` the
payload separates what was asked from what the server answered:

```json
{
  "operation": "create-dir",
  "server": "https://mega2.example.com",
  "target": { "parent": "/src", "name": "pkg", "path": "/src/pkg" },
  "receipt": { "commit_id": "…", "new_oid": "…", "path": "/src/pkg", "cl_link": null }
}
```

`target` is built from your validated input; `receipt` is the server's
receipt verbatim (`path` and `cl_link` may be `null`). Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| NAME is `.`/`..`, empty, too long, or has a separator or control character | `LBR-CLI-002` | none (no request) |
| Combined with another operation flag or with `--ref` | `LBR-CLI-002` | none (no request) |
| The directory already exists (mega2 answers HTTP 500 today) | `LBR-NET-002` | `http_status: 500` |
| HTTP 400 (rejected name or parent) | `LBR-CLI-003` | `http_status: 400` |
| HTTP 401 (server needs a write token) / 403 (token not allowed here) | `LBR-AUTH-001` / `LBR-AUTH-002` | `http_status` |
| HTTP 409 | `LBR-CONFLICT-002` | `http_status: 409` |
| Other non-2xx, or a 2xx whose receipt is invalid | `LBR-NET-002` | `http_status` |
| Timeout or connection failure before any response | `LBR-NET-001` | `transport` |
| Connection drops while the receipt is read (outcome unknown) | `LBR-NET-001` | `http_status` |

`--delete-dir <NAME>` deletes directory NAME under PATH with one
`POST /api/v1/delete-entry` and does not reload. There is no confirmation
line: the flag names the exact target. The plain-text summary is
`deleted directory <path> (commit <commit_id>)`; with `--json`/`--machine` the
payload is:

```json
{
  "operation": "delete-dir",
  "server": "https://mega2.example.com",
  "target": { "parent": "/src", "name": "pkg", "path": "/src/pkg" },
  "receipt": { "commit_id": "…", "path": "/src/pkg", "cl_link": null }
}
```

Libra does not check beforehand whether NAME exists or is a directory: the
server answers both. A file target is `LBR-CLI-003`; a missing target is
`LBR-NET-002` today, told apart from other failures by `details.http_status`.
Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| NAME is `.`/`..`, empty, too long, or has a separator or control character | `LBR-CLI-002` | none (no request) |
| Combined with another operation flag or with `--ref` | `LBR-CLI-002` | none (no request) |
| HTTP 400: NAME is a file, PATH runs through a file, or the server refuses the path (for example a top-level directory on a trunk server) | `LBR-CLI-003` | `http_status: 400` |
| NAME, or PATH itself, does not exist (HTTP 404) | `LBR-NET-002` | `http_status: 404` |
| HTTP 401 / 403 / 409, other failures, unknown outcomes | as for `--create-dir` | as for `--create-dir` |

`--move-dir <NAME> <PARENT-PATH>` moves directory NAME from PATH into the
existing directory PARENT-PATH, keeping its name, with one
`POST /api/v1/move-entry`, and does not reload. PARENT-PATH is validated like
PATH before the request. The plain-text summary is
`moved directory <from> -> <to> (commit <commit_id>)`; with `--json`/`--machine`
the payload is:

```json
{
  "operation": "move-dir",
  "server": "https://mega2.example.com",
  "target": {
    "from": { "parent": "/src", "name": "pkg", "path": "/src/pkg" },
    "to": { "parent": "/lib", "name": "pkg", "path": "/lib/pkg" }
  },
  "receipt": { "commit_id": "…", "from_path": "/src/pkg", "to_path": "/lib/pkg", "cl_link": null }
}
```

As with `--delete-dir`, the server answers whether the move is possible.
Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| NAME is `.`/`..`, empty, too long, or has a separator or control character | `LBR-CLI-002` | none (no request) |
| PARENT-PATH is not rooted, or has `.`/`..` components, a `\` or a control character | `LBR-CLI-003` | none (no request) |
| Combined with another operation flag or with `--ref` | `LBR-CLI-002` | none (no request) |
| HTTP 400: PARENT-PATH already has an entry named NAME, PARENT-PATH is PATH itself or lies inside NAME, NAME is a file, PATH or PARENT-PATH runs through a file, or the server refuses the path (for example, on a trunk server, a move between two top-level directories) | `LBR-CLI-003` | `http_status: 400` |
| HTTP 404: NAME, PATH or PARENT-PATH does not exist | `LBR-NET-002` | `http_status: 404` |
| HTTP 401 / 403 / 409, other failures, unknown outcomes | as for `--create-dir` | as for `--create-dir` |

`--rename-dir <NAME> <NEW-NAME>` renames directory NAME under PATH to NEW-NAME
with one same-parent `POST /api/v1/move-entry`, and does not reload. The
plain-text summary is `renamed directory <from> -> <to> (commit <commit_id>)`;
with `--json`/`--machine` the payload has the `--move-dir` shape, with
`target.to.parent` equal to `target.from.parent`:

```json
{
  "operation": "rename-dir",
  "server": "https://mega2.example.com",
  "target": {
    "from": { "parent": "/src", "name": "old", "path": "/src/old" },
    "to": { "parent": "/src", "name": "new", "path": "/src/new" }
  },
  "receipt": { "commit_id": "…", "from_path": "/src/old", "to_path": "/src/new", "cl_link": null }
}
```

Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| NAME or NEW-NAME is `.`/`..`, empty, too long, or has a separator or control character | `LBR-CLI-002` | none (no request) |
| Combined with another operation flag or with `--ref` | `LBR-CLI-002` | none (no request) |
| HTTP 400: NEW-NAME equals NAME, PATH already has an entry named NEW-NAME, NAME is a file, PATH runs through a file, or the server refuses the path (for example a top-level directory on a trunk server) | `LBR-CLI-003` | `http_status: 400` |
| HTTP 404: NAME or PATH does not exist | `LBR-NET-002` | `http_status: 404` |
| HTTP 401 / 403 / 409, other failures, unknown outcomes | as for `--create-dir` | as for `--create-dir` |

`--list-tags` lists one page of root tags with one anonymous
`GET /api/v1/tags/list` (query `page`, `per_page` and `path=/`). `--page`
(1–1000, default 1) and `--per-page` (1–100, default 20, the interactive
panel's page size) choose the page; both are refused without `--list-tags`.
Each tag prints as `<name>  <object_type>  <tagger>`, followed by its message
on a line indented by four spaces when it has one, then a
`page <page> · per_page <per_page> · total <total>` line; control characters
in server strings print as `?`. With `--json`/`--machine` the payload is:

```json
{
  "operation": "list-tags",
  "server": "https://mega2.example.com",
  "path": "/",
  "page": 1,
  "per_page": 20,
  "total": 42,
  "has_next": true,
  "items": [
    { "name": "v1.0", "tag_id": "…", "object_id": "…", "object_type": "commit",
      "tagger": "…", "message": "…", "created_at": "…" }
  ]
}
```

`has_next` is `page * per_page < total`; `items` keep every server field
verbatim. mega2 today pages annotated tags in its database, then fills the
rest of the page from its `refs/tags/*` refs, always starting again from the
first ref and skipping only the annotated tags already on that page. Annotated
tags have refs too, so a page can repeat tags from other pages; such entries,
like lightweight tags, carry ref-only fields (`object_type` `commit`, empty
`tagger` and `message`, `tag_id` equal to `object_id`). `total` adds every
such ref to the annotated count, so it can exceed the number of distinct tags
and differ from page to page, and `has_next` inherits that. Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| `--page` outside 1–1000 or `--per-page` outside 1–100 | `LBR-CLI-002` | none (no request) |
| `--page` or `--per-page` without `--list-tags` | `LBR-CLI-002` | none (no request) |
| A malformed PATH (not rooted, or with `.`/`..` components) | `LBR-CLI-003` | none (no request) |
| A rooted PATH other than `/`, `--ref`, a token flag, or another operation flag | `LBR-CLI-002` | none (no request) |
| HTTP, transport or response failures | the failure's stable code (see Machine error details) | `http_status` or `transport` |

`--create-tag <NAME>` creates root tag NAME with one `POST /api/v1/tags`
(`path_context` `/`). Without `--message` the tag is lightweight;
`--message <TEXT>` makes it annotated. Unlike the interactive panel, where an
empty message means lightweight, a non-interactive `--message` must be
non-empty, at most 1024 bytes and free of control characters, newlines
included: it is refused, never rewritten. A root tag needs a write token whose
paths cover `/`, or a `push_auth=none` server; a token limited to other paths
gets HTTP 403 (`LBR-AUTH-002`). The plain-text summary is
`created <kind> tag <name> -> <object_id>`; with `--json`/`--machine` the
payload is:

```json
{
  "operation": "create-tag",
  "server": "https://mega2.example.com",
  "target": { "name": "v1.0", "kind": "annotated", "path": "/" },
  "receipt": { "name": "v1.0", "tag_id": "…", "object_id": "…", "object_type": "commit",
               "tagger": "…", "message": "…", "created_at": "…" }
}
```

`target.kind` is `lightweight` or `annotated`; `receipt` is the server's tag
verbatim. Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| NAME breaks the tag-name rules (see the tag panel) | `LBR-CLI-002` | none (no request) |
| `--message` empty, over 1024 bytes or with a control character, or given without `--create-tag` | `LBR-CLI-002` | none (no request) |
| A malformed PATH (not rooted, or with `.`/`..` components) | `LBR-CLI-003` | none (no request) |
| A rooted PATH other than `/`, `--ref`, or another operation flag | `LBR-CLI-002` | none (no request) |
| HTTP 400 (for example, the tag already exists), 405 or 422 | `LBR-CLI-002` | `http_status` |
| HTTP 401 (server needs a write token) / 403 (the token does not cover `/`) | `LBR-AUTH-001` / `LBR-AUTH-002` | `http_status` |
| HTTP 404 / 409 | `LBR-CLI-003` / `LBR-CONFLICT-002` | `http_status` |
| Any other non-2xx or an invalid receipt / a timeout or dropped connection | `LBR-NET-002` / `LBR-NET-001`, as for `--create-dir` | `http_status` or `transport` |

`--delete-tag <NAME>` deletes root tag NAME with one
`DELETE /api/v1/tags/{name}?path=/` (NAME percent-encoded), with no
confirmation line and no reload. Like creating one, deleting a root tag needs
a write token whose paths cover `/`, or a `push_auth=none` server. The
plain-text summary is `deleted tag <name>`; with `--json`/`--machine` the
payload is:

```json
{
  "operation": "delete-tag",
  "server": "https://mega2.example.com",
  "target": { "name": "v1.0", "path": "/" },
  "receipt": { "deleted_tag": "v1.0", "message": "…" }
}
```

Errors:

| Situation | Stable code | `details` |
|-----------|-------------|-----------|
| NAME breaks the tag-name rules (see the tag panel) | `LBR-CLI-002` | none (no request) |
| A malformed PATH (not rooted, or with `.`/`..` components) | `LBR-CLI-003` | none (no request) |
| A rooted PATH other than `/`, `--ref`, or another operation flag | `LBR-CLI-002` | none (no request) |
| The tag does not exist (HTTP 404) | `LBR-CLI-003` | `http_status: 404` |
| HTTP 401 (server needs a write token) / 403 (the token does not cover `/`) | `LBR-AUTH-001` / `LBR-AUTH-002` | `http_status` |
| HTTP 400, 405 or 422 / 409 | `LBR-CLI-002` / `LBR-CONFLICT-002` | `http_status` |
| Any other non-2xx or an invalid receipt / a timeout or dropped connection | `LBR-NET-002` / `LBR-NET-001`, as for `--create-dir` | `http_status` or `transport` |

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
shell history — prefer the first two). The same rule applies to the
non-interactive write operations (see above). Read operations refuse token
flags: `--json`/`--machine` without a write flag and `--list` take no
credentials.

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
| `[PATH]` | Rooted directory to list, or the parent for a directory write; defaults to `/`. Tag operations accept only `/`. |
| `--ref <COMMIT-OR-TAG>` | Optional commit or tag to list. |
| `--list` | List PATH once without a terminal: plain-text lines, or the JSON payload with `--json`/`--machine`. |
| `--create-dir <NAME>` | Create directory NAME under PATH without a terminal (one POST, no reload). |
| `--delete-dir <NAME>` | Delete directory NAME under PATH without a terminal (one POST, no confirmation, no reload). |
| `--move-dir <NAME> <PARENT-PATH>` | Move directory NAME from PATH into PARENT-PATH, keeping its name, without a terminal (one POST, no reload). |
| `--rename-dir <NAME> <NEW-NAME>` | Rename directory NAME under PATH to NEW-NAME, keeping its parent, without a terminal (one POST, no reload). |
| `--list-tags` | List one page of root tags without a terminal (one GET). |
| `--page <N>` | With `--list-tags`: the page to list (1–1000; default 1). |
| `--per-page <N>` | With `--list-tags`: tags per page (1–100; default 20). |
| `--create-tag <NAME>` | Create root tag NAME without a terminal (one POST): lightweight, or annotated with `--message`. |
| `--message <TEXT>` | With `--create-tag`: the annotated tag's message (non-empty, at most 1024 bytes, no control characters). |
| `--delete-tag <NAME>` | Delete root tag NAME without a terminal (one DELETE, no confirmation). |
| `--token-file <PATH>` | Write operations (interactive or non-interactive): read the write token from a file (highest precedence). Refused by read operations. |
| `--token <TOKEN>` | Write operations: inline write token (lowest precedence; visible in shell history — prefer `--token-file`). Refused by read operations. |
| `--json[=<FORMAT>]` | Global flag: one request, JSON envelope (`pretty`/`compact`/`ndjson`). |
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
libra mega2 browser --server https://mega2.example.com /src/pkg

# Plain-text listing without a terminal (scripts, CI)
libra mega2 browser --server https://mega2.example.com --list /src/pkg

# Create /src/pkg without a terminal (anonymous write, push_auth=none servers)
libra --json mega2 browser --server https://mega2.example.com --create-dir pkg /src

# The same write with a token from a file (token-protected servers)
libra --json mega2 browser --server https://mega2.example.com --create-dir pkg /src --token-file ~/.mega2-token

# Delete /src/pkg without a terminal (no confirmation: the flag names the target)
libra --json mega2 browser --server https://mega2.example.com --delete-dir pkg /src --token-file ~/.mega2-token

# Move /src/pkg to /lib/pkg without a terminal
libra --json mega2 browser --server https://mega2.example.com --move-dir pkg /lib /src --token-file ~/.mega2-token

# Rename /src/old to /src/new without a terminal
libra --json mega2 browser --server https://mega2.example.com --rename-dir old new /src --token-file ~/.mega2-token

# Second page of root tags, 50 per page, as JSON
libra --json mega2 browser --server https://mega2.example.com --list-tags --page 2 --per-page 50

# Create annotated root tag v1.0 without a terminal
libra --json mega2 browser --server https://mega2.example.com --create-tag v1.0 --message "release 1.0" --token-file ~/.mega2-token

# Delete root tag v1.0 without a terminal (no confirmation)
libra --json mega2 browser --server https://mega2.example.com --delete-tag v1.0 --token-file ~/.mega2-token

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
