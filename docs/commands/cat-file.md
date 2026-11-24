# `libra cat-file`

Inspect Git objects and Libra AI history objects stored in the repository.

## Synopsis

```
libra cat-file [OPTIONS] [OBJECT]
libra cat-file <type> <object>
```

## Description

`libra cat-file` is a low-level debugging tool analogous to `git cat-file`. It
can print the type, size, or pretty-printed content of any Git object (commit,
tree, blob, tag), and can also check for object existence.

Libra extends the classic command with `--ai*` flags that inspect AI workflow
objects (Intent, Task, Run, Plan, PatchSet, Evidence, Session, etc.) stored on
the `libra/intent` orphan branch. This gives developers and agents a single
entry point for introspecting both version-control objects and AI process
artifacts.

Exactly one mode flag must be specified. Git modes (`-t`, `-s`, `-p`, `-e`)
require a positional `OBJECT` argument.

Without any mode flag, the two-positional `<type> <object>` form prints the raw
bytes of an object peeled to `<type>` (`blob`, `tree`, `commit`, or `tag`):
annotated tags peel through their targets, and a commit peels to its tree when
`tree` is requested (tree output is the binary tree object, not the `-p`
listing). An impossible peel is `LBR-CLI-003`; an unknown type is
`LBR-CLI-002`. Mode flags (`-t`/`-s`/`-p`/`-e`/`--batch*`/`--ai*`) cannot be
combined with the two-positional form (`LBR-CLI-002`, checked before any mode
dispatch). AI modes (`--ai`, `--ai-type`,
`--ai-list`, `--ai-list-types`) ignore `OBJECT` and operate on the AI history
branch.

Git object modes and batch inputs use the same strict resolver as `rev-parse`:
refs and unique OID prefixes, `@`, parent navigation, typed/recursive peel,
numeric reflog selectors, and `REV:path` are accepted. The resolver consumes
the whole input, so a malformed suffix such as `HEAD^{tree}junk` is rejected
instead of being partially interpreted.

In batch modes, only an unresolvable or nonexistent name is rendered as
`<input> missing`. A ref-database read failure or corrupt/unreadable object
fails the command with a structured error so storage damage is not hidden.

Batch modes read stdin one line at a time and answer each line before
reading the next. Without `--buffer`, every response is flushed immediately,
so a caller can keep one `cat-file --batch` process open and query it
interactively through a pipe. Input is treated as raw bytes: a line that is
not valid UTF-8 cannot name an object (refs, object ids, and tree entry names
are UTF-8) and is answered `<input> missing` with the original bytes echoed,
instead of aborting the batch.

During a non-squash merge paused on conflicts, `AUTO_MERGE` is also accepted
as an object spec and names the automatic result tree with its conflict
markers. It exists only for that merge state's lifetime; clean and squash
merges do not create it. The historical pseudo-refs (`ORIG_HEAD`,
`MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `REBASE_HEAD`, and
`FETCH_HEAD`) remain unavailable to object resolution.

## Options

| Flag | Short | Description |
|------|-------|-------------|
| `-t` | | Print the object type (`commit`, `tree`, `blob`, `tag`). |
| `-s` | | Print the object size in bytes. |
| `-p` | | Pretty-print the object content. |
| `-e` | | Check if the object exists. Without `--json`, exit status only (0 = exists, 1 = absent), no stdout. With `--json`/`--machine`, emits `{ "exists": bool }` while keeping the same exit codes. |
| `--batch-check[=<fmt>]` | | Read object names from stdin (one per line); print `<sha> <type> <size>` (or `<input> missing`). Optional format atoms `%(objectname)`/`%(objecttype)`/`%(objectsize)`. |
| `--batch[=<fmt>]` | | Like `--batch-check` plus the raw object contents and a trailing newline. |
| `--batch-command[=<fmt>]` | | Read commands from stdin: `info <object>` (header only) or `contents <object>` (header + contents). The `flush` command is accepted only under `--buffer`. |
| `--buffer` | | Block-buffer batch output instead of flushing it after every object. With `--batch-command`, commands are queued and run only on an explicit `flush` (or at end of input); this is what makes `flush` valid. Requires a batch mode. |
| `--batch-all-objects` | | With `--batch`/`--batch-check`, operate on every object in the store (loose + packed) in id order instead of reading stdin. |
| `--ai <ID>` | | Pretty-print an AI object by ID. Accepts `TYPE:ID` to disambiguate. |
| `--ai-type <ID>` | | Print the AI object type for the given ID. |
| `--ai-list <TYPE>` | | List all AI objects of the given type (e.g., `intent`, `patchset`, `event`). |
| `--ai-list-types` | | List all AI object types present in the history branch. |
| `<OBJECT>` | | Git object spec: hash/ref, typed peel, numeric reflog selector, or `REV:path`. Required for `-t`/`-s`/`-p`/`-e`; ignored for `--ai*` modes; batch modes read object specs from stdin instead. In the two-positional form the first value is the type and the second the object. |

### Examples

```bash
# Print the type of HEAD
libra cat-file -t HEAD

# Print the size of a specific object
libra cat-file -s 40d352ee7190f92dcf7883b8a81f2c730fd8a860

# Pretty-print HEAD commit
libra cat-file -p HEAD

# Inspect a peeled tree or a blob selected by path
libra cat-file -t 'HEAD^{tree}'
libra cat-file -p 'HEAD:src/main.rs'

# Check existence (exit code 0 = exists)
libra cat-file -e abc1234

# Check existence as JSON for agents ({ "exists": bool }; exit code preserved)
libra cat-file -e abc1234 --json

# Structured JSON type query
libra cat-file -t HEAD --json

# List all AI intent objects
libra cat-file --ai-list intent

# Pretty-print an AI object (disambiguate with TYPE:ID)
libra cat-file --ai patchset:call_KjR3NB4cQaT5Rm1c7zXjsskQ

# Print the type of an AI object
libra cat-file --ai-type debug-local-1772707227

# List all AI object types in the repository
libra cat-file --ai-list-types --json
```

## Common Commands

```bash
libra cat-file -t HEAD
libra cat-file -s HEAD
libra cat-file -p HEAD
libra cat-file -t 'HEAD^{tree}'
libra cat-file -p 'HEAD:src/main.rs'
libra cat-file -t HEAD --json
libra cat-file --ai-list-types --json
libra cat-file --ai-list intent
libra cat-file --ai <session-id>
```

## Human Output

- `-t`: prints the object type on a single line (e.g., `commit`)
- `-s`: prints the size in bytes on a single line (e.g., `342`)
- `-p`: pretty-prints content depending on type (matching `git cat-file -p`):
  - Commit: the raw object exactly as stored — every header in order
    (`tree`, `parent`, `author`, `committer`, plus `gpgsig`, `encoding`,
    `mergetag`, … when present), the blank line, and the message byte-for-byte
    (no newline is appended)
  - Tree: `<mode> <type> <hash>\t<name>` per entry, with Git's zero-padded
    octal mode (`100644`, `100755`, `120000`, `040000`, `160000`) and the
    pointed-to type (`blob`, `tree`, or `commit` for a gitlink/submodule)
  - Blob: raw content
  - Tag: tag header and message
- `-e`: no output; exit code 0 if the object exists, non-zero otherwise (with `--json`/`--machine`, also writes `{ "exists": bool }` to stdout)
- `--ai <ID>`: prints a formatted summary (session summary for `ai_session` objects, full JSON for others)
- `--ai-list <TYPE>`: one object ID per line
- `--ai-list-types`: one type name per line

## Structured Output (JSON examples)

### Type mode (`-t`)

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "type",
    "object": "HEAD",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "object_type": "commit"
  }
}
```

### Size mode (`-s`)

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "size",
    "object": "HEAD",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "size": 342
  }
}
```

### Pretty-print mode (`-p`) -- commit

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "pretty",
    "object": "HEAD",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "object_type": "commit",
    "tree": "def456...",
    "parents": ["abc123..."],
    "author": {
      "name": "Alice",
      "email": "alice@example.com",
      "timestamp": 1711929600,
      "timezone": "+0000"
    },
    "committer": {
      "name": "Alice",
      "email": "alice@example.com",
      "timestamp": 1711929600,
      "timezone": "+0000"
    },
    "message": "feat: add new feature"
  }
}
```

`message` is the commit message only (trimmed); extra headers such as
`gpgsig` or `encoding` never appear in it.

### Pretty-print mode (`-p`) -- tree

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "pretty",
    "object": "HEAD^{tree}",
    "hash": "def456...",
    "object_type": "tree",
    "entries": [
      { "mode": "100644", "object_type": "blob", "hash": "ce0136...", "name": "a.txt" },
      { "mode": "040000", "object_type": "tree", "hash": "add479...", "name": "src" },
      { "mode": "160000", "object_type": "commit", "hash": "5474b9...", "name": "vendor" }
    ]
  }
}
```

### AI list types

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "ai-list-types",
    "types": ["intent", "patchset", "plan", "run", "task"]
  }
}
```

Notes:

- `cat-file -e --json` / `--machine` emits `{ "exists": bool }` to stdout while preserving the exit-code contract (present → 0, well-formed-but-absent → 1, malformed name → 129)
- Blob/tag pretty-print JSON requires UTF-8 content; non-text payloads fail explicitly instead of returning lossy data

## Design Rationale

### Why add `--ai*` flags?

Libra's AI agent infrastructure stores process artifacts (intents, plans, tasks,
runs, patch sets, evidence, sessions) as Git objects on an orphan branch. Rather
than requiring a separate inspection tool, `cat-file` is the natural home
because it already handles "show me the raw content of an object by ID." The
`--ai*` flags extend this to the AI object namespace while keeping the familiar
interface. This means a single command can answer both "what type is this
commit?" and "what does this AI plan contain?" -- which is especially useful
during debugging of agent workflows.

### Batch modes and structured output

Git's batch modes read object IDs (or commands) from stdin for bulk inspection.
Libra exposes `--batch-check`, `--batch`, and `--batch-command` (the latter
dispatching `info`/`contents` per line), all sharing the same per-object
formatter with optional `=<format>` atom expansion. `--batch-all-objects` (with
`--batch`/`--batch-check`) enumerates every object in the store — loose plus
packed — in id order, instead of reading stdin. For agents, `--json` remains
the recommended interface — it returns typed fields in one call. Batch input is
streamed: each line is answered (and, without `--buffer`, flushed) before the
next is read, and non-UTF-8 lines are reported as `missing` rather than
failing the batch. `--buffer` is supported (block-buffer output instead of
flushing per object; with `--batch-command`, queue commands until an explicit
`flush` command or end of input — it is what makes `flush` valid; without
`--buffer` the `flush` command is rejected exactly as Git does, and `--buffer` requires
a batch mode). `--follow-symlinks` is not exposed.

### How does `-e` behave with `--json`?

By default the `-e` (existence check) mode is a silent probe that communicates
its result via exit code only: 0 means the object exists, non-zero means it does
not. This is the Unix convention for boolean predicates, so scripts can write
`if libra cat-file -e $hash; then ...`.

For agents that prefer structured output, `-e --json` (or `--machine`) emits a
`{ "exists": bool }` envelope on stdout **without** changing the exit-code
contract: a present object still exits 0, a well-formed but absent object still
exits 1 (with the JSON written first), and a malformed object name is still a
hard error (`LBR-CLI-003`, exit 129) that emits no envelope.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Libra | Git | jj |
|---------|-------|-----|----|
| Print object type | `-t` | `-t` | N/A (no direct equivalent) |
| Print object size | `-s` | `-s` | N/A |
| Pretty-print content | `-p` | `-p` | N/A (`jj file show` for blobs) |
| Check existence | `-e` | `-e` | N/A |
| Batch mode | `--batch[=<format>]`, `--batch-check[=<format>]`, `--batch-command[=<format>]` (info/contents), `--batch-all-objects` (`%(objectname)`/`%(objecttype)`/`%(objectsize)` atoms), line-by-line streaming with byte-safe input, `--buffer` (block buffering; enables `--batch-command`'s `flush`) | `--batch`, `--batch-check`, `--batch-command`, `--batch-all-objects`, `--buffer` | N/A |
| AI object inspection | `--ai`, `--ai-type` | N/A | N/A |
| AI object listing | `--ai-list`, `--ai-list-types` | N/A | N/A |
| JSON output | `--json` | No | No |
| Object resolution | SHA-1/SHA-256, refs, `@`, `^N`/`~N`, typed/recursive peel, numeric reflog selectors, `REV:path` | full rev-parse syntax | Change IDs, revsets |
| `--filters` | No | `--filters` (convert to/from external) | N/A |
| `--textconv` | No | `--textconv` | N/A |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Invalid object / revision | `LBR-CLI-003` | 129 |
| Unsupported argument combination | `LBR-CLI-002` | 129 |
| Failed to read object data | `LBR-IO-001` / `LBR-REPO-002` | 128 |

> Exit-code note: unresolvable object names and invalid argument combinations
> exit `129` (`LBR-CLI-003` / `LBR-CLI-002`), intentionally different from
> Git's `128` (ADR-PL-02). The `-e` existence probe keeps Git's exact contract:
> present `0`, well-formed-but-absent `1` (silent), malformed name `129`.


> `--separate-git-dir` pointer layouts are supported: when `<worktree>/.libra` is a
> `gitdir: <dir>` pointer file (created by `libra clone --separate-git-dir`), this
> command resolves the real metadata directory and reads `<dir>/libra.db` and
> `<dir>/objects` instead of `<worktree>/.libra`.
