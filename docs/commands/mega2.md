# `libra mega2 traces`

`libra mega2 traces` reads scoped agent history and can fetch one verified stream.

```
libra mega2 traces capabilities --server <BASE-URL> [--token-file <PATH> | --token <TOKEN>] [--require-search]
libra mega2 traces targets --server <BASE-URL> --path </project/foo> [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces streams --server <BASE-URL> --target <UUID> [--source <ID>] [--owner <UUID>] [--epoch <UUID>] [--cursor <CURSOR>] [--limit <N>] [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces sessions --server <BASE-URL> --stream <UUID> [--since <RFC3339>] [--until <RFC3339>] [--agent <KIND>] [--code-commit <OID>] [--session <ID>] [--cursor <CURSOR>] [--limit <N>] [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces show --server <BASE-URL> --stream <UUID> (--session <ID> | --checkpoint <ID>) [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces checkpoints --server <BASE-URL> --stream <UUID> --session <ID> [--cursor <CURSOR>] [--limit <N>] [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces search --server <BASE-URL> --target </project/foo> --query <TEXT> [--source <ID>] [--stream <UUID>] [--session <ID>] [--agent <KIND>] [--since <RFC3339>] [--until <RFC3339>] [--code-commit <OID>] [--cursor <CURSOR>] [--limit <N>] [--max-bytes <BYTES>] [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces transcript --server <BASE-URL> --stream <UUID> --checkpoint <ID> [--max-bytes <BYTES>] [--cursor <CURSOR>] [--token-file <PATH> | --token <TOKEN>]
libra mega2 traces fetch --remote <NAME> --stream <UUID> [--token-file <PATH> | --token <TOKEN>]
```

HTTP read commands accept an optional bearer token. HTTP(S) `traces fetch`
requires a token and authenticates with HTTP Basic using username `traces` and
the token as its password. Token sources are checked in this order:
`--token-file <PATH>`, `LIBRA_MEGA2_TOKEN`, then `--token <TOKEN>`. An inline
token can appear in shell history and process listings; prefer
`--token-file`. SSH `traces fetch` authenticates with the configured
`traces@` key and does not send a token.

Search text must be 1 to 256 UTF-8 bytes or the command does not open a socket.
The client sends the query unchanged; ASCII case-fold is the server's match.
List `--limit` defaults to 50 and accepts at most 200; search `--limit`
defaults to 20 hits and accepts at most 100. Search and transcript
`--max-bytes` defaults to 16384 and accepts at most 131072 bytes. Values above
the relevant maximum are rejected before a socket opens. `--owner`, `--epoch`,
`--agent`, `--session`, and `--cursor` are passed through as written.
`--code-commit` must be 40 or 64 hexadecimal characters. `--since` and
`--until` are RFC3339 values sent as UTC on the half-open interval
`[since, until)`; `since` later than `until` is rejected locally, and equal
bounds are an empty interval. `--session` and `--checkpoint` cannot be combined.
`--require-search` succeeds only when `search_ready` is boolean `true`; a
missing or non-boolean field is rejected, while a plain `capabilities` call
still prints the other fields. A redirect to another origin does not forward
`Authorization`. Traces JSON API responses over 1 MiB are rejected. The Git
pack fetched by `traces fetch` has a separate 512 MiB limit. Fetch reads one
stream with Git upload-pack from `/.traces/by-scope/<stream>.git` on the origin
of `remote.<name>.url`. It takes `server_id` from the authenticated
advertisement, verifies and indexes the received pack, and writes
`refs/remotes/<remote>/traces/<stream>/<sha1|sha256>` only when the advertised
tip is a commit in that pack and the hash kind matches the repository object
format. It records the server fingerprint and refuses a silent remote retarget.
It does not update `refs/heads/traces`, `refs/libra/traces`, `HEAD`, the index,
or the worktree. An unknown or unauthorized stream returns `unknown_scope` and
publishes nothing.

```
# For an HTTP(S) remote; SSH fetch uses the configured traces@ key instead.
libra mega2 traces fetch --remote origin --stream 3f1d2c4b-5a6e-4f70-8a9b-0c1d2e3f4a5b --token-file ~/.mega2-token
```

# `libra mega2 browser`

Browse a remote Mega2 repository over HTTP with a **directory tree on the left
and file content on the right**. Scripts and agents can still request a single
directory-level listing with `--list`, `--json` or `--machine`.

`mega2` is a Libra-only extension. It has **no Git-equivalent contract**:
`mega2 browser` never clones, fetches, or pushes Git objects. It lists remote
metadata and file previews rather than local tree objects. For local object inspection use
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
  the fixed `/api/v1/tree` and `/api/v1/blob` read routes.
- `PATH` must be rooted (`/` by default) and may not contain `.`/`..`
  components, NUL, control characters or platform separators.
- `--ref` selects a commit or tag to list; when omitted the server's default
  revision is used.

Expanding or reloading a directory sends one anonymous `GET /api/v1/tree`.
Selecting a file sends one anonymous `GET /api/v1/blob?path=…&refs=…` (the `refs`
parameter is omitted without `--ref`). Moving among directories, collapsing
branches, and scrolling already loaded content are local operations.
No `Authorization` header is attached to either read route, no repository is opened (the
command works outside a repository), and no local state — index, object store,
database, configuration — is read or written.

The client is bounded: it disables redirects and proxies, applies a 10-second
timeout, caps the response body at 1 MiB and the listing at 2000 entries, and
accepts only entries whose `content_type` is `directory` or `file`. Names with
`..`, path separators or terminal control characters are refused
fail-closed, and the server-provided item `path` is never used as navigation
authority.

> **Revision warning:** `--ref` selects the revision for browsing and reads only.
> Current interactive create/delete/move/rename actions still target the server default
> revision, not the selected commit/tag. Treat `--ref` browsing as read-only and do not
> confirm interactive writes there; the behavior fix is deferred. Non-interactive write
> flags combined with `--ref` remain rejected before any request.

### Interactive mode (default)

Interactive mode requires **stdin and stdout to be terminals**. If either is
not a TTY the command refuses immediately with a stable error and a hint to use
`--json`; it never alters the terminal.

While browsing, the terminal is switched to raw mode and driven by a bounded
state machine with lazy expansion and no recursive/background prefetch. The
left pane retains expanded branches and their siblings. Selecting a file loads
its text in the right pane, with line numbers and independent scrolling.
Loading, empty, missing, binary/unsupported, and oversized files have explicit
messages; a failed preview does not exit the browser or display the previous
file. The server must support UTF-8 text via `/api/v1/blob`. Preview text is
limited to 256 KiB, with a 1 MiB wire-response cap and a 10-second deadline;
no preview is saved locally. Tabs display as four spaces, terminal control and
bidirectional-control characters are neutralized. The expanded tree is capped
at 8192 rows, 4 MiB of stored path/name bytes and 128 child levels; collapse a
branch to free space. Unix terminals redraw on resize; below 49 columns by 10
rows the UI shows a resize hint.

File previews detect syntax from the filename/extension (or a script's shebang).
Rust, Python, JavaScript, C/C++, shell, JSON, YAML, Markdown, HTML/CSS and
TOML/INI configuration files are highlighted; `.buckconfig`, `.gitconfig`,
`.editorconfig` and `BUCK`/`BUILD` also have filename rules. Keywords, strings,
numbers, names and comments use the terminal's own color palette. The content
header identifies the detected syntax. Unknown types stay plain text.
`--no-color`, `--color=never` and automatic `NO_COLOR`/`TERM=dumb` handling disable
syntax colors; explicit `--color=always` overrides the automatic environment
policy. Only interactive previews are colored: machine/list output is unchanged.
Highlighting is cached per file and does not make extra requests. Text larger
than 64 KiB after display normalization, lines over 4096 bytes, or exhausted
highlighting work limits fall back to plain text with a header note; the 256 KiB
text-preview limit still applies. Multiline syntax remains correct while scrolling.

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | Move the tree selection; in the content pane, scroll lines |
| `Enter` | Expand/fold a directory; on a file, focus its content |
| `→` | Expand a directory or select its first child; in content, scroll right |
| `←`, `Backspace` or `h` | Fold a branch or select its parent; at a subtree root, load its parent (never above `/`); content `←` scrolls left |
| `Tab` | Switch focus between directory tree and file content |
| `PageUp` / `PageDown` | Scroll file content one screen |
| `a` | Create a child inside the selected directory (see below) |
| `d` | Delete the selected directory (confirmation dialog; files are inert) |
| `m` | Move the selected file or directory with a destination-folder picker |
| `e` | Rename the selected directory (same parent; `r` still reloads) |
| `t` | Toggle the tag panel (see below) |
| `r` | Reload the selected directory or file; from an empty tree, reload its root |
| `q` (or `Ctrl-C`, `Esc`) | Quit |

The primary shortcuts use single keys without Shift. `a` also creates a tag
inside the tag panel; legacy `+` (create) and `R` (rename) remain aliases.
While entering a name or tag message, printable shortcut characters are literal
text (including `q`); Enter submits, Backspace edits and Esc cancels. Directory
names and tag names/messages use centered popup dialogs with a labeled input
and visible cursor. Long input scrolls to its end; validation errors appear
inside the dialog. Delete confirmations also use a popup. Moving uses a separate
folder picker: `Right` enters a destination folder, `Enter` chooses it, and a
second `Enter` confirms the write. Dialogs fit terminals down to 49 × 10, work
without color, and preserve
the underlying tree/tag selection when cancelled.

Terminal state (raw mode and the alternate screen) is restored on every exit
path, including errors and signals handled by the process.
Directory mutation shortcuts apply only while the tree has focus. `a` creates
inside the selected directory, whether expanded or folded, and refreshes that
directory's children. Delete and rename operate on the selected directory;
move operates on the selected file or directory, using its actual parent even
for nested rows, and refreshes that parent branch. A cross-parent move from a
nested branch also reloads the visible root and folds its branches, avoiding
stale destination entries. JSON/machine schemas and their one-request behavior
are unchanged.

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
| start, expand/reload a directory, load a parent root | `--list` (PATH is the directory; `--json` without a flag is the same) | `GET /api/v1/tree` |
| `a` | `--create-dir <NAME>` (PATH is the parent) | `POST /api/v1/create-entry` |
| `d` | `--delete-dir <NAME>` (PATH is the parent) | `POST /api/v1/delete-entry` |
| `m` | `--move-dir <NAME> <PARENT-PATH>` (directory only; PATH is NAME's current parent; the name is kept) | `POST /api/v1/move-entry` |
| `e` | `--rename-dir <NAME> <NEW-NAME>` (PATH is the parent, which is kept) | `POST /api/v1/move-entry` |
| `t`, then `n`/`p` | `--list-tags [--page <N>] [--per-page <N>]` (PATH must be `/`) | `GET /api/v1/tags/list` |
| `t`, then `a` | `--create-tag <NAME> [--message <TEXT>]` (PATH must be `/`) | `POST /api/v1/tags` |
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

### Creating a directory (`a`, interactive only)

Press `a` to open a centered name dialog for a child of the selected
directory (or of the visible root when the tree is empty). The dialog shows the
parent path: selecting `/artifact` and entering `rust` creates `/artifact/rust`,
not `/rust`. The selected directory need not be expanded. While a **file** is
selected the editor refuses to open — selection must be a directory or an
empty area. `Enter` validates the name with the same rules used for the wire
(`/`, `\`, `.`, `..`, NUL and control characters are refused) and only then
performs **one** `POST /api/v1/create-entry` with `is_directory=true`,
`skip_build=true` and no `content`; `Esc` cancels with no network at all.

A confirmed creation reloads the target directory exactly once and shows its children. Failures
(401/403, duplicate name, timeout, malformed response) keep the last safe
listing on screen and show a secret-free status line; the terminal is never
left in raw mode and the TUI never asks you to type a raw token on the
alternate screen.

HTTP 400 does not necessarily mean the name already exists: check the parent
path, the server's path policy and the name. Mega2 deployments restrict new
paths to configured monorepo roots; selecting `/artifact` creates beneath that
root without attempting a new top-level path. Raw server error bodies are not
displayed, to avoid leaking secrets.

Write tokens are read, in order of precedence, from `--token-file <path>`,
then the `LIBRA_MEGA2_TOKEN` environment variable, then `--token` (visible in
shell history — prefer the first two). The same rule applies to the
non-interactive write operations (see above). Read operations refuse token
flags: `--json`/`--machine` without a write flag and `--list` take no
credentials.

### Deleting, moving and renaming (`d`, `m`, `e`, interactive only)

`d` opens a confirmation dialog before deleting the selected directory; `m`
opens a destination-folder picker for the selected file or directory; `e`
renames a selected directory in place (the same-parent form of the move
operation). The picker fixes the source before browsing: `Right` enters a
folder, the first `Enter` chooses it, and the second `Enter` performs the
move. `Esc` returns from confirmation to the picker, then cancels without a
write request. Files stay inert for delete and rename only.

Mega2 trunk writes cannot land on `/`; therefore a move cannot cross top-level
monorepo roots. For example, `/artifact/rust` cannot move to `/data`. The
picker only shows destinations below the source root (for example,
`/artifact`), so that impossible request is never sent. It also refuses a
directory's own descendants before the POST.

Each confirmed mutation performs **one** `POST /api/v1/delete-entry` or
`POST /api/v1/move-entry` followed by **one** parent-branch listing reload.
A cross-parent move from a nested branch adds one root reload and folds the tree.
Success clears
the status; failures (401/403, missing source, duplicate destination,
timeout) keep the last safe listing and show a secret-free status line while
the terminal stays intact. There is no multi-select and no recursion: one
selected entry per operation (a directory for delete/rename, or a file or
directory for move).

### Tag panel (`t`, interactive only)

`t` opens the tag panel and performs exactly **one** anonymous
`GET /api/v1/tags/list` for page 1 (the three required query keys are
`page`, `per_page`, `path`; this MVP always uses the repository root
`path=/`). `n`/`p` request the explicit next/previous page — one request per
key, never prefetched. `a` collects a tag name and then an optional message
(an empty message creates a lightweight tag; a non-empty one creates an
annotated tag); `d` deletes the selected tag after a confirmation
dialog. `t`, `Esc` or `q` close the panel without leaving the browser, and the
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

- One request per directory expansion/reload or file selection; moving between
  directory rows and collapsing branches sends none. No recursive prefetch,
  background task, or cache that outlives the process.
- Browse is read-only and anonymous. A confirmed `a` adds at most one
  `create-entry` POST plus one reload GET; `d`/`m`/`e` likewise add one
  `delete-entry`/`move-entry` POST plus one parent reload GET (a cross-parent
  move from a nested branch adds one root reload). No recursive or
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

# Move /src/pkg to /src/archive/pkg without a terminal
libra --json mega2 browser --server https://mega2.example.com --move-dir pkg /src/archive /src --token-file ~/.mega2-token

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

# Create/delete/rename directories or move a file/directory interactively (press a, d, m or e; t for tags)
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
