# `libra describe`

Find the nearest reachable tag for a commit and format it as a human-readable
version description.

**Alias:** `desc`

## Synopsis

```
libra describe [OPTIONS] [COMMIT]
```

## Description

`libra describe` walks the commit ancestry graph (BFS) from the given commit
(default `HEAD`) to find the closest tag. In a shallow clone, commits listed in
`.libra/shallow` are treated as roots (`--contains` also stops at those
boundaries). A corrupt `.libra/shallow` file fails closed (`LBR-REPO-002`).
The output follows Git's describe format:

- Exact match: `v1.2.3`
- Reachable tag with distance: `v1.2.3-4-gabc1234`
- Fallback (`--always`): `abc1234`

By default only annotated tags are considered. Pass `--tags` to also match
lightweight tags. When multiple tags are reachable at the same distance,
annotated tags are preferred; ties are broken lexicographically.

When no tag can be found and `--always` is absent, the command fails with an
actionable hint suggesting `--tags` or `--always`.

`--exact-match` restricts the command to tags that point directly at the target
commit. If no exact tag exists, it fails even when `--always` is also present.

`--long` forces Git's long format when a tag describes the target. Exact matches
therefore print `v1.2.3-0-gabc1234` instead of `v1.2.3`. Like Git,
`--long --abbrev=0` is rejected because the long form requires a hash suffix.

`--dirty[=<mark>]` appends a suffix when tracked content differs from `HEAD`.
The default suffix is `-dirty`; custom marks are accepted with
`--dirty=<mark>`. Untracked files are ignored, matching Git's dirty check for
this command.

## Options

| Flag | Description | Default |
|------|-------------|---------|
| `<COMMIT>` | The commit-ish to describe. Accepts `HEAD`, branch names, tag names, raw SHA-1, `HEAD~N`. | `HEAD` |
| `--tags` | Include lightweight tags in the search (not just annotated tags). | Off |
| `--all` | Consider any ref (local branches, remote-tracking branches, and tags, including lightweight ones), not just tags. Names are shown with their `heads/`, `remotes/`, or `tags/` prefix; at a shared commit tags win, then heads, then remotes. | Off |
| `--abbrev <N>` | Number of hex digits for the abbreviated commit hash in the output. | `7` |
| `--always` | When no tag can describe the target, fall back to the abbreviated commit hash instead of failing. | Off |
| `--exact-match` | Only succeed when the target commit exactly matches a tag. | Off |
| `--candidates <N>` | `N=0` requires an exact tag match (equivalent to `--exact-match`); `N>=1` keeps Libra's deterministic nearest-tag search (the positive bound is not enforced). | — |
| `--long` | Force `tag-N-gHASH` output when a tag describes the target, including `tag-0-gHASH` for exact matches. | Off |
| `--dirty[=<mark>]` | Append a dirty mark when tracked content differs from `HEAD`. | Off, default mark `-dirty` when enabled |
| `--first-parent` | Follow only the first parent of merge commits when walking history. | Off |
| `--match <pattern>` | Only consider tags whose name matches the glob (repeatable; OR semantics, wax globs ≤256 chars). | None |
| `--exclude <pattern>` | Exclude tags whose name matches the glob (repeatable; takes precedence over `--match`). | None |
| `--contains` | Inverse containment query (git name-rev): name the target by its nearest **descendant** tag, printing `<tag>`, `<tag>~<n>`, or `<tag>~<n>^<m>~<k>`. Implies lightweight tags; equal-weight ties resolve by tag name; fails when no tag descends from the target. | Off |

### Examples

```bash
# Describe HEAD using annotated tags only
libra describe

# Include lightweight tags
libra describe --tags

# Always produce output, even without tags
libra describe --always

# Only succeed on an exact tag match
libra describe --exact-match

# Force tag-0-gHASH output on an exact tag match
libra describe --long

# Describe a specific commit
libra describe HEAD~5

# Use longer abbreviated hashes
libra describe --abbrev 12

# Append -dirty when tracked content differs from HEAD
libra describe --dirty

# Use a custom dirty mark
libra describe --dirty=-worktree

# Follow only the first parent of merge commits
libra describe --first-parent

# Only consider tags matching a glob
libra describe --match 'v1.*'

# Skip release-candidate tags
libra describe --exclude '*rc*'

# Name a commit by its nearest descendant tag (e.g. v1.0~2)
libra describe --contains HEAD~2

# JSON output for automation
libra describe --json
```

## Common Commands

```bash
libra describe
libra describe --tags
libra describe --always
libra describe --exact-match
libra describe --long
libra describe --dirty
libra describe HEAD~1
libra describe --contains HEAD~2
libra describe --json
libra describe --tags --abbrev 10
```

## Human Output

- Exact tag match: `v1.2.3`
- Exact tag match with `--long`: `v1.2.3-0-gabc1234`
- Reachable tag: `v1.2.3-4-gabc1234`
- `--always` fallback: `abc1234`
- `--dirty` on tracked changes: `v1.2.3-dirty`
- `--dirty=-worktree` on tracked changes: `v1.2.3-worktree`

`--quiet` suppresses `stdout`.

## Structured Output (JSON examples)

`--json` / `--machine` returns:

### Tag match (exact)

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "v1.2.3",
    "tag": "v1.2.3",
    "distance": 0,
    "abbreviated_commit": null,
    "exact_match": true,
    "used_always": false,
    "long_format": false,
    "dirty": false,
    "dirty_mark": null
  }
}
```

### Tag match (with distance)

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "v1.2.3-4-gabc1234",
    "tag": "v1.2.3",
    "distance": 4,
    "abbreviated_commit": "abc1234",
    "exact_match": false,
    "used_always": false,
    "long_format": false,
    "dirty": false,
    "dirty_mark": null
  }
}
```

### Fallback (`--always`, no tag found)

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "abc1234",
    "tag": null,
    "distance": null,
    "abbreviated_commit": "abc1234",
    "exact_match": false,
    "used_always": true,
    "long_format": false,
    "dirty": false,
    "dirty_mark": null
  }
}
```

When `--always` is used and no tag matches, `tag` and `distance` are `null` and
`abbreviated_commit` contains the emitted hash.

### Dirty suffix

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "v1.2.3-dirty",
    "tag": "v1.2.3",
    "distance": 0,
    "abbreviated_commit": null,
    "exact_match": true,
    "used_always": false,
    "long_format": false,
    "dirty": true,
    "dirty_mark": "-dirty"
  }
}
```

## Design Rationale

### `--match`, `--exclude`, `--first-parent`, and what is still missing

Libra exposes `--match` and `--exclude` (wax globs, capped at 256 chars, with
exclude taking precedence over match) and `--first-parent` (follow only the
first parent of merge commits during the BFS walk). `--candidates <N>` is
exposed for its well-defined boundary: `N=0` means "only exact matches"
(equivalent to `--exact-match`), while `N>=1` keeps Libra's predictable
nearest-tag BFS (the candidate bound only affects which of several equally
reachable tags Git would prefer, which Libra resolves deterministically).
`--all` is supported: branches (`heads/`), remote-tracking branches
(`remotes/`), and tags (`tags/`, including lightweight ones) are all folded
into the candidate set for the same BFS, with tags taking precedence at a
shared commit, then heads, then remotes. `--contains` is supported: it runs
Git's reverse-walk containment algorithm (name-rev) — a Dijkstra-style walk
backward from every tag commit where first-parent steps are cheap and
other-parent steps are expensive, so the closest descendant tag's straightest
path wins — and prints `<tag>`, `<tag>~<n>`, or `<tag>~<n>^<m>~<k>`. It
implies lightweight-tag inclusion (like `git name-rev --tags`) and fails when
no tag descends from the commit.

### Why include both string and structured fields?

Human output follows Git's string format, including `--long` for exact matches.
The JSON output also includes separate `tag`, `distance`, `abbreviated_commit`,
`exact_match`, and `long_format` fields, so automation can avoid parsing the
human string when it needs to distinguish exact matches from reachable tags.

### Why BFS instead of Git's candidate algorithm?

Git's `describe` uses a more complex algorithm that considers multiple tag
candidates and picks the one with the smallest distance, with heuristics to
avoid walking the entire graph. Libra uses a simpler BFS from the target
commit, which guarantees finding the closest tag (shortest path in the DAG).
For the repository sizes Libra targets (monorepos with structured tagging),
BFS is fast enough and its behavior is trivially predictable. The trade-off
is that very deep histories with many tags could be slower than Git's pruned
search, but this has not been a problem in practice.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Libra | Git | jj |
|---------|-------|-----|----|
| Default target | `HEAD` | `HEAD` | N/A (no built-in describe) |
| Annotated tags only | Default behavior | Default behavior | N/A |
| Include lightweight tags | `--tags` | `--tags` | N/A |
| Abbreviated hash length | `--abbrev <N>` (default 7) | `--abbrev=<N>` (default dynamically chosen) | N/A |
| Fallback to hash | `--always` | `--always` | N/A |
| Exact match only | `--exact-match` | `--exact-match` | N/A |
| Force long format | `--long` | `--long` | N/A |
| Match tag pattern | `--match <glob>` (wax, ≤256 chars, repeatable) | `--match <glob>` | N/A |
| Exclude tag pattern | `--exclude <glob>` (exclude wins over match) | `--exclude <glob>` | N/A |
| Candidate count | `--candidates <N>` (N=0 ⇒ exact-match; N≥1 ⇒ nearest-tag BFS) | `--candidates=<N>` (default 10) | N/A |
| First-parent only | `--first-parent` | `--first-parent` | N/A |
| Consider all refs | `--all` (heads/remotes/tags, prefixed) | `--all` | N/A |
| Find tags containing a commit | `--contains` (name-rev, prints `<tag>~<n>^<m>`) | `--contains` | N/A |
| Dirty suffix | `--dirty[=<mark>]` | `--dirty[=<mark>]` | N/A |
| JSON output | `--json` with typed fields | No | No |
| Algorithm | BFS (shortest path) | Heuristic multi-candidate | N/A |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Invalid revision | `LBR-CLI-003` | 129 |
| `HEAD` has no commit | `LBR-REPO-003` | 128 |
| No tags can describe the target and `--always` is absent | `LBR-REPO-003` | 128 |
| `--exact-match` target has no exact tag (incl. `--contains --exact-match` resolving only to a relative `~N` name) | `LBR-REPO-003` | 128 |
| `--contains` target has no descendant tag | `LBR-REPO-003` | 128 |
| `--long --abbrev=0` | `LBR-CLI-002` | 129 |
| Failed to read refs or objects | `LBR-IO-001` / `LBR-REPO-002` | 128 |
