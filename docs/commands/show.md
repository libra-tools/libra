# `libra show`

Show a commit, tag, tree, blob, or the blob referenced by `REV:path`.

## Synopsis

```
libra show [OPTIONS] [OBJECT] [-- <PATHS>...]
```

## Description

`libra show` resolves a single object reference and renders its contents. The
default target is `HEAD`. It understands commit-ish references (`HEAD~2`,
branch names, tag names), raw SHA-1 hashes, and the `REV:path` syntax for
extracting a specific blob from a tree at a given revision.

For commits the output includes the header (author, committer, date, message)
followed by a unified diff (the "patch"). Flags such as `--no-patch`, `--stat`,
and `--name-only` control how much diff context is shown. For annotated tags the
tagger metadata and message are printed, followed by the target object. Trees
list their entries and blobs print their text content (or a binary summary).

When stdout is piped and the downstream command exits early, `libra show` exits quietly without
printing panic/backtrace or `Broken pipe` diagnostics.

## Options

| Flag | Short | Description |
|------|-------|-------------|
| `<OBJECT>` | | Object name (commit, tag, tree, blob) or `<object>:<path>`. Defaults to `HEAD`. |
| `--no-patch` | `-s` | Skip patch output and only show object metadata. |
| `--oneline` | | Shorthand for `--pretty=oneline` -- prints hash and subject on one line. |
| `--pretty <FORMAT>` | | Format the commit header with a preset (`oneline`, `short`, `full`, `fuller`, `reference`, `raw`) or a `%`-placeholder template (`format:`/`tformat:`/bare). Uses the same custom placeholders as `libra log --format`, including `%b`, `%B`, `%n`, ASCII/control `%xNN`, `%%`, `%aI`, `%cI`, `%at`, `%ct`, `%D`, `%m`, and color placeholders. |
| `--format <FORMAT>` | | Alias for `--pretty=<FORMAT>` (Git's `--format`). Mutually exclusive with `--pretty`. |
| `--abbrev-commit` | | Abbreviate the commit object name in the default header to a 7-character prefix. |
| `--no-abbrev-commit` | | Show the full (unabbreviated) commit object name, countermanding an earlier `--abbrev-commit` (last one wins). The full hash is the default, so on its own this is a no-op. |
| `--name-only` | | Show only changed file names (no diff hunks). |
| `--name-status` | | Show changed file names prefixed by a status letter (`A`/`M`/`D`), tab-separated. |
| `--raw` | | Show the raw diff format `:<old-mode> <new-mode> <old-sha> <new-sha> <status>\t<path>` (object ids abbreviated to 7) instead of a patch, like `git show --raw`. |
| `--stat` | | Show diff statistics (insertions / deletions per file). |
| `--patch-with-stat` | | Show the diffstat block followed by the full patch (Git's legacy synonym for `-p --stat`). |
| `--summary` | | Show a condensed summary of created and deleted files (their mode and path), like `git show --summary`. Created/deleted files only — no rename/copy detection. |
| `--no-expand-tabs` | | Do not expand tabs in the commit message. Accepted no-op: Libra's show prints tabs verbatim. |
| `--no-notes` | | Do not show commit notes. Accepted no-op: Libra's show never displays notes inline. |
| `--no-mailmap` | | Do not apply a `.mailmap`. Accepted no-op: Libra's show shows the raw recorded identities. |
| `--no-show-signature` | | Do not display the GPG signature of signed commits. Accepted no-op: Libra's show never displays commit signatures inline. (Git's `--show-signature` is not implemented.) |
| `<PATHS>...` | | Limit output to matching paths (pathspec filter for commit diffs). |

### Examples

```bash
# Show the latest commit with full patch
libra show HEAD

# Show only metadata (no diff) for a tag
libra show --no-patch v1.0.0

# Show a specific file from a revision
libra show HEAD:src/main.rs

# One-line summary of a commit
libra show --oneline abc1234

# Diff statistics only
libra show --stat HEAD~1

# Limit diff to a subdirectory
libra show HEAD -- src/command/
```

## Common Commands

```bash
libra show                          # show HEAD commit and patch
libra show HEAD~3                   # show an ancestor commit
libra show -s v2.0.0                # metadata only for a tag
libra show HEAD:Cargo.toml          # print a file at HEAD
libra show --name-only HEAD         # list changed files
libra show --name-status HEAD       # list changed files with A/M/D status
libra show --stat HEAD              # diff statistics
libra show --patch-with-stat HEAD   # diffstat followed by the full patch
libra show --summary HEAD           # created/deleted file mode summary
libra --json show HEAD              # structured JSON output
```

## Human Output

Human mode preserves the existing presentation:

- Commit: header plus optional patch / stat / name-only output
- Annotated tag: tag metadata followed by the target object
- Tree: list of tree entries
- Blob: text content or a binary summary
- `--quiet`: validates the object reference but suppresses human output
- Human output uses the shared pager policy; pass global `--no-pager` to force direct stdout

## Structured Output (JSON examples)

`data.type` determines the schema. Possible values: `commit`, `tag`, `tree`,
`blob`.

### Commit

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "commit",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "short_hash": "abc1234",
    "author_name": "Alice",
    "author_email": "alice@example.com",
    "author_date": "2026-04-01T10:00:00+00:00",
    "committer_name": "Alice",
    "committer_email": "alice@example.com",
    "committer_date": "2026-04-01T10:00:00+00:00",
    "subject": "feat: add new feature",
    "body": "",
    "parents": ["def456..."],
    "refs": ["HEAD -> main"],
    "files": [
      { "path": "tracked.txt", "status": "added" }
    ]
  }
}
```

### Tag

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "tag",
    "tag_name": "v1.0.0",
    "tagger_name": "Alice",
    "tagger_email": "alice@example.com",
    "tagger_date": "2026-04-01T10:00:00+00:00",
    "message": "Release v1.0.0",
    "target_hash": "abc1234def5678901234567890abcdef12345678",
    "target_type": "commit"
  }
}
```

### Tree

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "tree",
    "entries": [
      { "mode": "100644", "object_type": "blob", "hash": "abc123...", "name": "README.md" },
      { "mode": "040000", "object_type": "tree", "hash": "def456...", "name": "src" }
    ]
  }
}
```

### Blob

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "blob",
    "hash": "abc123...",
    "size": 1024,
    "is_binary": false,
    "content": "fn main() { ... }"
  }
}
```

Notes:

- Commit JSON `refs` are best-effort decoration metadata; unrelated branch/tag rows no longer block `show`
- Human `--quiet` still validates the target object but suppresses stdout and does not initialize the pager
- Commit patch / stat paths stay strict: corrupt historical blobs fail with `LBR-REPO-002` instead of falling back to working tree contents

## Design Rationale

### Why support `REV:path` syntax?

The `REV:path` notation (e.g., `HEAD:src/main.rs`) is one of the most useful
idioms in Git because it lets users and tools retrieve any file at any point in
history without checking out an entire commit. For AI agents this is especially
valuable: an agent can read specific files at specific revisions to compare
implementations across branches or time, without mutating the working tree.
Libra preserves this syntax for full Git compatibility and because it maps
naturally to the internal tree-walk operation that Libra already performs.

### `--pretty` / `--format` and structured JSON

`--pretty=<fmt>` and its alias `--format=<fmt>` render the commit header with the
`oneline` preset or a `%`-placeholder template (`format:`/`tformat:`/bare), sharing
`libra log`'s formatter. The named presets `short`, `full`, `fuller`, `reference`,
and `raw` are rendered distinctly (matching Git's preset structure); `medium`
maps to the default format. (This is separate from the `--raw` diff format —
see the `--raw` option — which selects the raw `:<old-mode> <new-mode> …` diff
format instead of a preset.) For programmatic consumers, `--json` remains the
recommended interface: it gives every field in a well-typed, type-discriminated
schema (typed fields vs. string parsing), avoiding format-string fragility.

### Why type-aware JSON schema?

The `data.type` discriminator (`commit`, `tag`, `tree`, `blob`) means that
JSON consumers can switch on the type and access only the fields that exist for
that object kind. This is more ergonomic than a flat schema with many nullable
fields, and it mirrors the object model of Git itself. Each variant carries
exactly the fields that make sense (e.g., `tagger_name` appears only in tags,
`parents` only in commits), which eliminates an entire class of "field is null
but I expected it" bugs in agent tooling.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Libra | Git | jj |
|---------|-------|-----|----|
| Default target | `HEAD` | `HEAD` | N/A (`jj show` removed; use `jj log -r @`) |
| `REV:path` syntax | Yes | Yes | No (use `jj file show -r REV path`) |
| `--no-patch` / `-s` | Yes | Yes | N/A |
| `--oneline` | Yes | Yes | N/A (use `jj log --template`) |
| `--name-only` | Yes | Yes | N/A |
| `--stat` | Yes | Yes | N/A (`jj diff --stat -r REV`) |
| `--patch-with-stat` | Yes | Yes | N/A |
| `--pretty` / `--format` | Yes (`oneline` + `%`-templates; presets pending) | Yes | No (use templates) |
| `--abbrev-commit` | Yes | Yes | N/A |
| `--quiet` | Yes (validates only) | No | N/A |
| JSON output | `--json` with typed schema | No | No |
| Pathspec filter | Yes (trailing `<PATHS>...`) | Yes | No (use `jj diff --from/--to`) |
| Tag-aware display | Auto-detects annotated tags | Auto-detects annotated tags | No tag objects |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Outside a repository | `LBR-REPO-001` | 128 |
| Invalid revision or missing path | `LBR-CLI-003` | 129 |
| Failed to read the object | `LBR-REPO-002` | 128 |
