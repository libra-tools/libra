# `libra notes`

Add, append, copy, edit, show, list, or remove notes attached to commits
without modifying the commits themselves.

> Status: `partial`. `libra notes` is now registered in the public CLI. The core
> operations (`add`, `append`, `copy`, `edit`, `list`, `show`, `remove`) and
> `merge` (a 2-way merge of the flat note rows, with `--strategy`) are supported.
> `prune` and `get-ref` are also supported. The interactive editor fallback
> for `add`/`edit`/`append` (when no `-m`/`-F` is given) is supported.

## Synopsis

```
libra notes add [-m <message> | -F <file>] [-f] [<object>]
libra notes append [-m <message> | -F <file>] [<object>]
libra notes edit [-m <message> | -F <file>] [<object>]
libra notes copy [-f] <from-object> <to-object>
libra notes list [<object>]
libra notes show [<object>]
libra notes remove [<object>...]
libra notes merge [-s|--strategy <manual|ours|theirs|union|cat_sort_uniq>] <other-ref>
libra notes merge --commit
libra notes merge --abort | --quit
libra notes prune [-n|--dry-run] [-v]
libra notes get-ref
```

## Description

`libra notes` manages annotations attached to commit objects. Unlike commit
messages, notes can be added or removed after the commit is created — the
original commit hash stays unchanged. This makes them useful for post-hoc
metadata such as code-review results, CI status, or deploy tracking.

Notes are stored as blob objects under a notes ref (default
`refs/notes/commits`, or `core.notesRef` when set). Use `--ref <ref>` to operate on a different namespace
(e.g., `refs/notes/review`).

Omitting a subcommand defaults to `list`.

Bare `libra notes`, `list`, `show`, `get-ref`, and `prune --dry-run` do not record an Operation v2 entry. `add`, `append`, `edit`, `copy`, `remove`, `merge`, and `prune` without `--dry-run` still record one (`libra op log --command notes`).

### Reusing note content

`notes add`/`append` accept `-C <object>` (`--reuse-message`) to reuse the
object's blob bytes as the note, and `-c <object>` (`--reedit-message`) to reuse
them as an editor prefill. `-C`/`-c` compose with `-m`/`-F` in command-line
order. `notes list` prints full 40/64-character object ids.

## Options

| Flag | Long | Value | Description |
|------|------|-------|-------------|
| | `<object>` | positional (optional) | Commit to annotate, show, or remove notes from. Defaults to HEAD. |
| `-m` | `--message` | `<msg>` | Note message text. Repeatable; blank lines separate messages. |
| `-F` | `--file` | `<file>` | Read note message from file (`-` for stdin). |
| `-f` | `--force` | | Overwrite an existing note (for `add` and `copy`). |
| | `--ref` | `<ref>` | Operate on a specific notes ref (default: `core.notesRef` when configured, else `refs/notes/commits`). An explicit `--ref` wins over the configuration. A configured value is used verbatim and must start with `refs/notes/` for every subcommand except `get-ref`, which prints it as-is (`LBR-CLI-002` otherwise). |

### Subcommands

| Subcommand | Description |
|------------|-------------|
| `add` | Add a note to an object. Fails if a note already exists; use `-f` to overwrite. With no `-m`/`-F`, opens an editor — empty for a new note, or pre-filled with the existing note when `-f` is given (without `-f` on an existing note it aborts before opening the editor). |
| `append` | Append a message to an object's note (separated by a blank line), creating the note if absent. With no `-m`/`-F`, opens an editor (empty buffer). |
| `edit` | Set (replace) an object's note, creating it if absent — overwrites unconditionally, unlike `add`. With no `-m`/`-F`, opens an editor pre-filled with the existing note. |
| `copy` | Copy the note from `<from-object>` to `<to-object>`. Fails if the source has no note, or if the target already has one (use `-f` to overwrite). |
| `list` | List note objects and the commits they annotate (default subcommand). |
| `show` | Show the note text for an object. |
| `remove` | Remove notes for one or more objects. |
| `merge` | Merge another notes ref (`<other-ref>`) into the current `--ref`. A 2-way merge of the flat note rows: copies notes new to the current ref, skips identical ones, and resolves a differing note per `-s`/`--strategy` (`manual` default, `ours`, `theirs`, `union`, `cat_sort_uniq`). |
| `prune` | Remove notes whose annotated object no longer exists in the object store. Silent by default; `-n`/`--dry-run` reports what would be pruned, `-v` prints each pruned object id. |
| `get-ref` | Print the notes ref operations act on (honors `--ref`, then `core.notesRef`; default `refs/notes/commits`). It echoes an out-of-namespace configured value verbatim. |

### Flag examples

```bash
# Annotate HEAD with a review result
libra notes add -m "Reviewed-by: Alice <alice@example.com>"

# Add from a file
libra notes add -F review-summary.txt abc1234

# Force-overwrite an existing note
libra notes add -m "Updated review" -f HEAD

# Append another line to HEAD's note (blank-line separated)
libra notes append -m "Deployed-by: CI"

# Copy a note from one commit to another
libra notes copy abc1234 def5678

# Set (replace) HEAD's note unconditionally
libra notes edit -m "Replaces any existing note"

# List all notes
libra notes list

# Show the note on HEAD
libra notes show

# Show the note on a specific commit
libra notes show abc1234

# Remove the note on HEAD
libra notes remove

# Remove notes from multiple commits
libra notes remove abc1234 def5678

# Use a custom namespace
libra notes --ref refs/notes/ci add -m "Passed all tests" HEAD
libra notes --ref refs/notes/ci show HEAD

# Merge another notes ref into refs/notes/commits (take theirs on conflict)
libra notes merge --strategy=theirs refs/notes/ci

# JSON output for agents
libra notes show --json
libra notes list --json
```

## Common Commands

```bash
libra notes add -m "Reviewed-by: Alice"       # Add a note to HEAD
libra notes show                                # Show the note on HEAD
libra notes list                                # List all notes
libra notes remove abc1234                      # Remove a note
libra notes add -f -m "Updated" HEAD            # Force-overwrite a note
libra notes --json show                         # Structured JSON output
```

## Human Output

- `libra notes add -m "msg"`: `Added note to abc1234 in refs/notes/commits`
- `libra notes show`: prints the note text as-is
- `libra notes list`: `<note-hash> <annotated-object-hash>`, one per line
- `libra notes remove abc1234`: `Removed note from abc1234 in refs/notes/commits`
- `libra notes` (no args): same as `list`

## Structured Output (JSON examples)

With `--json` / `--machine`, the envelope's `action` field distinguishes operations:

### `add`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "add",
    "ref": "refs/notes/commits",
    "object": "abc1234...",
    "note_hash": "def5678..."
  }
}
```

### `show`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "show",
    "ref": "refs/notes/commits",
    "object": "abc1234...",
    "note_hash": "def5678...",
    "text": "Reviewed-by: Alice <alice@example.com>"
  }
}
```

### `list`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "list",
    "ref": "refs/notes/commits",
    "notes": [
      { "note_hash": "def5678...", "annotated_object": "abc1234..." },
      { "note_hash": "1111222...", "annotated_object": "def5678..." }
    ]
  }
}
```

When `<object>` is given and no note exists, `note_hash` is `null`.

### `remove`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "remove",
    "ref": "refs/notes/commits",
    "removed": [
      { "object": "abc1234...", "note_hash": "def5678..." }
    ]
  }
}
```

## Design Rationale

### Editor fallback

Like Git, `add`/`edit`/`append` open an editor when neither `-m` nor `-F` is
given. The editor is resolved from `GIT_EDITOR` → `core.editor` → `VISUAL` →
`EDITOR` (falling back to `vi` only on a terminal); in a headless/non-terminal
environment with no editor configured, the command fails with a clear "no editor
configured" error. `edit` pre-fills the buffer with the existing note, as does
`add -f` when a note already exists (plain `add` aborts before the editor opens
if a note exists); `add` (new note) and `append` start empty. The saved buffer is cleaned with `git stripspace`
whitespace rules but — unlike commit/tag messages — `#` lines are **preserved**,
since a note may legitimately contain them. An empty result aborts.

For headless or agent-driven workflows, prefer `-m <message>` or `-F <file>`
(`-` for stdin), which never invoke the editor.

### `merge`, `prune`, and `get-ref`

`notes merge <other-ref>` merges another notes ref into the current one
(`--ref`, default `refs/notes/commits`). Because Libra stores notes as flat
SQLite rows rather than Git's commit-backed notes trees, there is no common base
to do Git's true 3-way merge — this is a 2-way merge: objects annotated only in
the other ref are copied, identical notes are skipped, and an object with a
differing note on both sides is a conflict resolved by `--strategy`:

- `manual` (default): when any note conflicts, the `notes` table is left
  untouched and the conflict is materialized under the worktree gitdir:
  `NOTES_MERGE_WORKTREE/<oid>` holds Git-style conflict markers
  (`<<<<<<< <target-ref>` … `=======` … `>>>>>>> <source-ref>`, each side
  terminated with a newline) and `NOTES_MERGE_STATE` (version 1 JSON) records the
  target ref, source ref, the entries to copy and the per-conflict before/source
  blob ids. The command still fails with `LBR-CONFLICT-002`.
  `NOTES_MERGE_WORKTREE`/`NOTES_MERGE_STATE`/`NOTES_MERGE_COMMITTING` left over
  from an unfinished merge make a new merge fail with `LBR-REPO-003`. Every
  `notes merge` form takes an exclusive `NOTES_MERGE_LOCK` first and removes it
  when it finishes; an existing lock fails with `LBR-CONFLICT-002` before any
  state is read or written (a stale lock is removed by hand after confirming no


  After resolving the conflict files, `libra notes merge --commit` reads
  `NOTES_MERGE_STATE` (missing: `LBR-REPO-003`; unparseable: `LBR-REPO-002`) and
  takes each `NOTES_MERGE_WORKTREE/<oid>` file as the resolved note (a deleted
  file keeps the target-side note). It first creates the exclusive
  `NOTES_MERGE_COMMITTING` marker, then in ONE write transaction verifies every
  row's current value is either the recorded before-value or the target value
  before writing anything; a row holding anything else rolls the whole
  transaction back, removes the marker, keeps the state and fails with
  `LBR-CONFLICT-002`. Rows already at the target value (including copies) are
  skipped, not treated as conflicts. After the commit it removes


  If `NOTES_MERGE_COMMITTING` already exists (a previous `--commit` crashed),
  the retry only CONFIRMS — it never replays: when every conflict entry and
  copied entry already holds its target value it just removes the three state
  paths and succeeds (including the crash-before-transaction case where the
  target values equal the current ones); otherwise it fails with
  `LBR-CONFLICT-002` and keeps all state, so the only way forward is
  `--abort` and a fresh merge. A missing `NOTES_MERGE_STATE` is still


  `libra notes merge --abort` discards the merge without touching the `notes`
  table: it removes `NOTES_MERGE_STATE`, `NOTES_MERGE_WORKTREE/` and
  `NOTES_MERGE_COMMITTING` (a partial residue is cleaned the same way) and fails
  with `LBR-REPO-003` when no merge state exists. 

  The three control forms are mutually exclusive and none of them accepts a
  `-s/--strategy` or a positional notes ref: mixing them is a usage error
  (`LBR-CLI-002`) rejected before the lock is taken, while
  `-s/--strategy <ref>` remains the ordinary strategy merge. Both forms take the
  `NOTES_MERGE_LOCK` first, so an existing lock fails with
  `LBR-CONFLICT-002`.
- `ours` / `theirs`: keep the current note / take the other ref's note.
- `union`: concatenate both note contents.
- `cat_sort_uniq`: concatenate, then sort and de-duplicate the combined lines.

`notes prune` removes notes whose annotated object no longer exists in the
object store — Libra checks each flat note row's object against the object store
and deletes the orphaned rows (in a transaction, with a blob compare-and-swap so
a concurrently-rewritten note is left intact). It is silent by default; `-n` /
`--dry-run` reports what would be pruned without deleting, and `-v` prints each
pruned object id. `notes get-ref` prints the notes ref that operations act on
(honoring `--ref`; default `refs/notes/commits`).

### Why SQLite-backed notes refs?

Libra stores notes refs in SQLite rather than loose files under
`.git/refs/notes/`. This provides atomic transactions (add/remove in a single
operation), efficient queries (listing all notes is one query, not a directory
scan), and concurrency safety via SQLite WAL mode.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | Libra | jj |
|---------|-----|-------|----|
| Add note | `git notes add [-m <msg>] [<obj>]` | `libra notes add [-m <msg>] [<obj>]` (editor fallback when no `-m`/`-F`) | N/A |
| List notes | `git notes list [<obj>]` | `libra notes list [<obj>]` | N/A |
| Show note | `git notes show [<obj>]` | `libra notes show [<obj>]` | N/A |
| Remove note | `git notes remove [<obj>...]` | `libra notes remove [<obj>...]` | N/A |
| Append | `notes append` | Supported | N/A |
| Copy | `notes copy [-f] <from> <to>` | Supported | N/A |
| Edit | `notes edit` (`-m`/`-F` or editor) | Supported (editor pre-fills the existing note) | N/A |
| Merge | `notes merge [-s <strategy>] <ref>` | `libra notes merge [-s <strategy>] <ref>` (2-way flat-row merge) | N/A |
| Prune | `notes prune [-n] [-v]` | `libra notes prune [-n] [-v]` | N/A |
| Get ref | `notes get-ref` | `libra notes get-ref` | N/A |
| Custom ref | `--ref <ref>` | `--ref <ref>` | N/A |
| File input | `-F <file>` | `-F <file>` | N/A |
| Editor support | Interactive editor (default) | Editor fallback when no `-m`/`-F` (`edit` pre-fills; `#` lines kept) | N/A |
| Structured output | No | `--json` / `--machine` | N/A |
| Ref storage | Loose files + packed-refs | SQLite (libra.db) | N/A |

Note: jj does not have a notes feature.

## Error Handling

| Scenario | Error Code | Hint |
|----------|-----------|------|
| Object already has a note (add or copy target) | `LBR-CONFLICT-002` | "use '-f' to overwrite the existing note." |
| Object has no note (show/remove) | `LBR-CLI-003` | "use 'libra notes list' to see which objects have notes." |
| No editor configured and no `-m`/`-F` (non-terminal env) | `LBR-REPO-003` | "set GIT_EDITOR, core.editor, VISUAL, or EDITOR" / pass `-m`/`-F`. |
| Edited note buffer is empty (no `-m`/`-F`) | `LBR-CLI-002` | "write some text in the editor, or pass -m/--message." |
| Invalid object reference | `LBR-CLI-003` | "use 'libra log' to find valid commit references." |
| Invalid notes ref name | `LBR-CLI-002` | "notes refs must start with 'refs/notes/'; e.g. 'refs/notes/commits'." |
| `merge` conflict under the default `manual` strategy | `LBR-CONFLICT-002` | "re-run with --strategy=ours/theirs/union/cat_sort_uniq …" |
| `merge` with an unknown `--strategy` value | `LBR-CLI-002` | "valid strategies: manual, ours, theirs, union, cat_sort_uniq" |
| Not a libra repository | `LBR-REPO-001` | Initialize with `libra init` or navigate to a repo. |
| Failed to load/store blob object | `LBR-IO-002` | Check repository integrity. |
| Failed to read/write notes ref | `LBR-IO-002` | Check database permissions and writability. |
