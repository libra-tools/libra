# `libra rev-list`

List commit objects reachable from a revision.

## Synopsis

```bash
libra rev-list [OPTIONS] [SPEC]... [-- <PATH>...]
```

## Description

`libra rev-list` resolves one or more revision inputs to commits, walks the reachable history, applies optional exclusion/range, symmetric-difference side, cherry-equivalence, first-parent, author, committer, message grep, path, time-window, parent-count, and count/limit filters, and prints commit IDs newest first. When `<SPEC>` is omitted, the command defaults to `HEAD`. Output formatting can include parent commit IDs (`--parents`), child commit IDs (`--children`), committer timestamps (`--timestamp`), side markers (`--left-right`), and cherry-equivalence markers (`--cherry-mark` / `--cherry`). `--reverse` flips the output to oldest-first (applied after commit limiting).

In a shallow clone, commits listed in `.libra/shallow` are treated as roots, so
`--count HEAD` matches the fetched history. A corrupt `.libra/shallow` file
fails closed (`LBR-REPO-002`). `--parents` still prints the ids recorded on
the commit object.

## Options

| Flag | Description |
|------|-------------|
| `-n <N>`, `--max-count <N>` | Limit output to at most `N` commits after sorting. |
| `--skip <N>` | Skip the first `N` commits before output or counting. |
| `--reverse` | Output the selected commits in reverse order. Commit limiting (`--max-count`/`--skip`) is applied first, then the result is reversed. |
| `--all` | Seed the walk with every ref (branches, remote-tracking branches, and tags) and the current HEAD, in addition to any explicit `<SPEC>`. |
| `--date-order` | Show commits in committer-date order (newest first). Accepted as a no-op for Libra's existing default ordering. Unlike Git, Libra does not add the topo "no parent before its children" constraint (only observable under committer-date skew). |
| `--count` | Print only the number of commits after filters. |
| `--since <DATE>`, `--after <DATE>` | Print commits whose committer timestamp is at or after `DATE`. |
| `--until <DATE>`, `--before <DATE>` | Print commits whose committer timestamp is at or before `DATE`. |
| `--merges` | Print only commits with at least two parents. |
| `--no-merges` | Omit commits with at least two parents. |
| `--min-parents <N>` | Print only commits with at least `N` parents. |
| `--max-parents <N>` | Print only commits with at most `N` parents. |
| `--no-min-parents` | Clear the lower parent-count bound. |
| `--no-max-parents` | Clear the upper parent-count bound. |
| `--first-parent` | Follow only the first parent when walking through merge commits. |
| `--author <PATTERN>` | Print only commits whose author name or email contains `PATTERN` case-insensitively. |
| `--committer <PATTERN>` | Print only commits whose committer name or email contains `PATTERN` case-insensitively. |
| `--grep <PATTERN>` | Print only commits whose message matches `PATTERN` as a case-sensitive regular expression. May be repeated; any matching pattern includes the commit. |
| `--left-right` | Prefix commits from `A...B` symmetric-difference input with `<` for the left side and `>` for the right side. |
| `--left-only` | With symmetric-difference input, keep only commits unique to the left side. Conflicts with `--right-only`. |
| `--right-only` | With symmetric-difference input, keep only commits unique to the right side. Conflicts with `--left-only`. |
| `--cherry-pick` | Omit patch-equivalent commits that appear on both sides of a symmetric difference. Conflicts with `--cherry-mark`. |
| `--cherry-mark` | Mark patch-equivalent commits with `=` and non-equivalent commits with `+`. Conflicts with `--cherry-pick`. |
| `--cherry` | Shorthand for right-side, cherry-marked, no-merge symmetric-difference output. Equivalent commits are prefixed with `=`, unique right-side commits with `+`, or `>` when combined with `--left-right`. |
| `--parents` | Print parent commit IDs after each listed commit. |
| `--children` | Print child commit IDs after each listed commit. Conflicts with `--parents`. Child relationships are built from the traversal before output filters such as `--skip`, `--max-count`, `--grep`, and parent-count filters are applied. |
| `--timestamp` | Prefix each listed commit with its committer timestamp, matching Git's `timestamp commit [parents...]` field order. |
| `--boundary` | Also print the boundary commits at the frontier — the parents of a listed commit that are not themselves listed (excluded by a `^spec`/range start, or beyond a `--max-count`/`--skip` cut) — each prefixed with `-`. They normally follow the listed commits; under `--reverse` the whole stream is reversed, so they lead. Boundary commits are formatted through the same path, so `--parents`/`--children`/`--timestamp` metadata is preserved (with two merge nuances matching Git: under `--first-parent --parents` an un-walked second-parent boundary prints bare, and `--children` lists a boundary's children from the output set). `--count` includes the boundary commits in the total. |
| `--objects` | After the commit lines, also list the deduplicated tree and blob objects reachable from the printed commits, each as `<oid> <path>`. The root tree of each commit is printed as `<oid> ` (a trailing space, empty path); subtrees and blobs carry their worktree-relative path. Objects are walked in pre-order (the tree itself, then each entry in tree order, recursing into a subtree immediately after emitting it), matching `git rev-list --objects`. Object lines always follow the commit/boundary stream and are not reordered by `--reverse`. Objects reachable from **excluded** commits (e.g. the `A` side of `A..B`, or a `^rev`) are treated as uninteresting and omitted, so a range lists only the objects new to the included side. A `-- <pathspec>` limit prunes the walk to the trees on the path to a pathspec plus everything under it (the root tree is always kept). Gitlink/submodule (`160000`) entries are skipped. A missing/corrupt tree on the included side is a hard error (`LBR-REPO-002`) rather than a silently truncated listing. `--count --objects` adds the objects to the total but is rejected together with `--left-right`/`--cherry-mark`/`--cherry` (objects carry no side). |
| `--objects-edge` | Imply `--objects` and additionally print the excluded boundary commits (the frontier) prefixed with `-`, so a pack builder can treat them as edges. |
| `--objects-edge-aggressive` | Accepted as an alias of `--objects-edge`. Git's aggressive variant marks more edge commits to build thinner packs; Libra emits the same boundary frontier (a documented narrowing). |
| `<SPEC>...` | Revisions to enumerate from. Defaults to `HEAD`; accepts multiple positive revisions, `^<rev>` exclusions, `A..B`, and `A...B`. |
| `-- <PATH>...` | Limit commits to changes that touched one of the listed paths. |

## Common Commands

```bash
libra rev-list
libra rev-list HEAD
libra rev-list --count HEAD
libra rev-list -n 5 HEAD
libra rev-list --reverse HEAD
libra rev-list --all
libra rev-list --date-order HEAD
libra rev-list --skip 5 --max-count 10 HEAD
libra rev-list --since 2026-01-01 HEAD
libra rev-list --after "2 weeks ago" --before 2026-06-01 HEAD
libra rev-list main feature
libra rev-list ^main feature
libra rev-list main..feature
libra rev-list main...feature
libra rev-list --boundary main..feature
libra rev-list --merges HEAD
libra rev-list --no-merges HEAD
libra rev-list --min-parents 1 --max-parents 1 HEAD
libra rev-list --min-parents 1 --no-min-parents HEAD
libra rev-list --max-parents 0 HEAD
libra rev-list --max-parents 0 --no-max-parents HEAD
libra rev-list --first-parent HEAD
libra rev-list --author alice HEAD
libra rev-list --committer alice HEAD
libra rev-list --grep 'fix|feat' HEAD
libra rev-list HEAD -- src/
libra rev-list --left-right main...feature
libra rev-list --right-only main...feature
libra rev-list --cherry-pick main...feature
libra rev-list --cherry-mark main...feature
libra rev-list --cherry main...feature
libra rev-list --left-right --cherry main...feature
libra rev-list --count --left-right --cherry-mark main...feature
libra rev-list --count --cherry main...feature
libra rev-list --parents HEAD
libra rev-list --children HEAD
libra rev-list --timestamp --parents HEAD
libra rev-list HEAD~1
libra rev-list refs/remotes/origin/main
libra rev-list --objects HEAD
libra rev-list --objects-edge main..feature
libra --json rev-list HEAD
```

## Human Output

Output is one commit ID per line by default. Multiple positive revisions are unioned and de-duplicated. `^<rev>` excludes commits reachable from that revision. `A..B` is equivalent to `^A B`; `A...B` prints the symmetric difference between both sides. `--left-right` prefixes symmetric-difference commits with `<` or `>`, `--left-only` and `--right-only` keep one side, `--cherry-pick` removes patch-equivalent pairs across sides, `--cherry-mark` prefixes equivalent commits with `=` and unique commits with `+`, and `--cherry` keeps the right side, marks equivalent commits with `=`, marks unique right-side commits with `+`, and implies no-merge output. With `--left-right --cherry`, unique right-side commits use `>` while equivalent commits keep `=`. `--first-parent` limits traversal through merge commits to the first parent chain. Author, committer, message grep, path, time-window, side/cherry, and parent-count filters are applied before `--skip`, `--max-count`, and `--count`. `--children` child relationships are built from the traversal before those output filters, so a printed commit can list a child that was skipped or filtered out. `--author` and `--committer` match the respective `name <email>` string case-insensitively. `--grep` matches the full commit message with a case-sensitive regular expression; repeated `--grep` patterns use OR semantics. Path filters must follow an explicit `--` separator and match files or directories relative to the worktree root. `--since`/`--after` and `--until`/`--before` accept `YYYY-MM-DD`, RFC3339/full timestamps with timezone, Unix timestamps, and relative forms such as `2 weeks ago`. With `--parents`, each line becomes `commit parent...`. With `--children`, each line becomes `commit child...`. With `--timestamp`, each line becomes `timestamp commit`; combining `--timestamp --children` produces `timestamp commit child...`. `--parents` and `--children` are mutually exclusive. With `--count`, default output is a single decimal count; `--left-right` produces `<left>\t<right>`, `--cherry-mark` or `--cherry` produces `<unique>\t<equivalent>`, and combining `--left-right --cherry-mark` or `--left-right --cherry` produces `<left-unique>\t<right-unique>\t<equivalent>`.

```text
abc1234def5678901234567890abcdef12345678
def5678901234567890abcdef12345678abc1234
```

```text
1715788800 abc1234def5678901234567890abcdef12345678 def5678901234567890abcdef12345678abc1234
1715702400 def5678901234567890abcdef12345678abc1234
```

```text
<abc1234def5678901234567890abcdef12345678
>def5678901234567890abcdef12345678abc1234
```

```text
=abc1234def5678901234567890abcdef12345678
+def5678901234567890abcdef12345678abc1234
```

## Structured Output

```json
{
  "ok": true,
  "command": "rev-list",
  "data": {
    "input": "HEAD",
    "inputs": ["HEAD"],
    "commits": [
      "abc1234def5678901234567890abcdef12345678",
      "def5678901234567890abcdef12345678abc1234"
    ],
    "total": 2,
    "count_only": false,
    "parents": false,
    "children": false,
    "timestamp": false,
    "reverse": false,
    "first_parent": false,
    "author": null,
    "committer": null,
    "grep": [],
    "pathspecs": [],
    "left_right": false,
    "left_only": false,
    "right_only": false,
    "cherry_pick": false,
    "cherry_mark": false,
    "cherry": false,
    "since": null,
    "until": null,
    "merges": false,
    "no_merges": false,
    "min_parents": null,
    "max_parents": null,
    "no_min_parents": false,
    "no_max_parents": false,
    "max_count": null,
    "skip": 0
  }
}
```

When `--parents`, `--children`, `--timestamp`, `--left-right`, `--cherry-mark`, or `--cherry` is present, `commits[]` remains the plain commit-ID list for compatibility and `entries[]` carries the optional metadata used for human output.

With `--boundary`, the frontier commits appear in a separate `boundary[]` array (omitted when empty) using the same entry shape as `entries[]`, each carrying `"boundary": true` plus any `--parents`/`--children`/`--timestamp` metadata; they are NOT included in `commits[]` or `total`, but `--count` adds them to the count fields. A `"reverse"` boolean reflects `--reverse`: the JSON `commits[]`/`entries[]` arrays follow `--reverse` (already reversed), while `boundary[]` is a separate array that keeps its natural committer-date-descending order. In human output, `--reverse` reverses the complete stream so boundary rows lead, but each boundary entry's own child list is unaffected.

With `--objects` (or `--objects-edge[-aggressive]`), the reachable tree/blob objects appear in a separate `objects[]` array of `{ "oid", "path" }` (omitted when empty), deduplicated and in the same pre-order as the human output. A root tree carries an empty `"path"`. `--objects-edge[-aggressive]` additionally populates `boundary[]` with the `-`-prefixed edge commits.

```json
{
  "ok": true,
  "command": "rev-list",
  "data": {
    "input": "HEAD",
    "inputs": ["HEAD"],
    "commits": [
      "abc1234def5678901234567890abcdef12345678"
    ],
    "entries": [
      {
        "commit": "abc1234def5678901234567890abcdef12345678",
        "side": "left",
        "cherry_equivalent": false,
        "parents": [
          "def5678901234567890abcdef12345678abc1234"
        ],
        "timestamp": 1715788800
      }
    ],
    "total": 1,
    "count_only": false,
    "parents": true,
    "children": false,
    "timestamp": true,
    "reverse": false,
    "first_parent": false,
    "author": null,
    "committer": null,
    "grep": [],
    "pathspecs": [],
    "left_right": true,
    "left_only": false,
    "right_only": false,
    "cherry_pick": false,
    "cherry_mark": true,
    "cherry": false,
    "since": null,
    "until": null,
    "merges": false,
    "no_merges": false,
    "min_parents": null,
    "max_parents": null,
    "no_min_parents": false,
    "no_max_parents": false,
    "max_count": 1,
    "skip": 0
  }
}
```

With `--children`, `entries[]` includes child commit IDs while `commits[]` remains plain:

```json
{
  "children": true,
  "commits": ["def5678901234567890abcdef12345678abc1234"],
  "entries": [
    {
      "commit": "def5678901234567890abcdef12345678abc1234",
      "children": ["abc1234def5678901234567890abcdef12345678"]
    }
  ]
}
```

## Parameter Comparison: Libra vs Git vs jj

| Feature | Libra | Git | jj |
|---------|-------|-----|----|
| Default target | `HEAD` | `HEAD` | current revision |
| Revision navigation | `HEAD~1`, tags, remote refs | Same | revsets |
| Multiple revisions | Supported, de-duplicated | Same | revsets |
| Exclusion/range syntax | `^A`, `A..B`, `A...B` | Same | revsets |
| Count and limit | `--count`, `-n` / `--max-count`, `--skip` | Same | revset functions |
| Time filters | `--since` / `--after`, `--until` / `--before` | Same | revset predicates |
| Parent-count filters | `--merges`, `--no-merges`, `--min-parents`, `--max-parents`, `--no-min-parents`, `--no-max-parents` | Same | revset predicates |
| First-parent traversal | `--first-parent` | Same | revset/graph predicates |
| Author filter | `--author <PATTERN>` | Same | revset predicates |
| Committer filter | `--committer <PATTERN>` | Same | revset predicates |
| Message grep | `--grep <PATTERN>` | Same | revset predicates |
| Path limitation | `-- <PATH>...` | Same | revset/file predicates |
| Symmetric side output/filtering | `--left-right`, `--left-only`, `--right-only` | Same | revset predicates/templates |
| Cherry-equivalence filtering | `--cherry`, `--cherry-pick`, `--cherry-mark` | Same | revset predicates/templates |
| Parent output | `--parents` | Same | revset/template output |
| Child output | `--children` | Same | revset/template output |
| Timestamp output | `--timestamp` | Same | template output |
| JSON output | `--json` | No | No |
| Ordering | Newest first | Reachability order | Revset-dependent |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Invalid target ref | `LBR-CLI-003` | 129 |
| Invalid date filter | `LBR-CLI-002` | 129 |
| Invalid grep regex | `LBR-CLI-002` | 129 |
| Conflicting side/cherry flags | `LBR-CLI-002` | 129 |
| Conflicting parent/child output flags | `LBR-CLI-002` | 129 |
| Failed to read repository metadata | `LBR-IO-001` | 128 |
| Corrupt stored refs/objects | `LBR-REPO-002` | 128 |
