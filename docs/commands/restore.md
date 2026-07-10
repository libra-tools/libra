# `libra restore`

Restore working tree files or index entries from a source.

**Alias:** `unstage`

## Synopsis

```
libra restore [--source <tree-ish>] [--staged] [--worktree] <pathspec>...
libra restore (--ours | --theirs | --merge | --conflict <style>) <pathspec>...
libra restore --ignore-unmerged [--source <tree-ish>] <pathspec>...
```

## Description

`libra restore` restores files in the working tree or index from a given source. By default (when neither `--staged` nor `--worktree` is specified), it restores files in the working tree from the index -- effectively discarding unstaged changes. With `--staged`, it restores the index from HEAD (or the specified `--source`), which unstages files. With both `-S` and `-W`, it restores both the index and working tree simultaneously.

For new workflows, use `libra restore` directly. `libra checkout -- <path>` and `libra checkout <tree-ish> -- <path>` are accepted only as Git-compatible aliases for this path-restore behavior.

The `<pathspec>` argument is required and accepts one or more file paths or directory paths. The special path `.` restores all files. Pathspecs use Libra's shared Git-style matcher: plain pathspecs match a file or directory prefix, wildcard pathspecs are supported, and the high-value magic forms `:(top)`, `:/`, `:(glob)`, `:(literal)`, `:(icase)`, `:(exclude)`, `:!`, and `:^` are honored. Exclude pathspecs subtract from the positive selection, pathspec matching follows `core.ignorecase` when enabled, and wildcard-looking pathspecs also match an exact path or directory prefix with the same literal text.

When a source commit contains files that do not exist in the current worktree, those files are created. In the default (`--no-overlay`) mode, when the current worktree contains tracked files that do not exist in the source, those files are deleted so the target matches the source exactly; with `--overlay` those source-absent tracked paths are left in place instead. The output reports both `restored_files` and `deleted_files` separately.

LFS-managed files are automatically downloaded from the LFS server when restoring from a commit that references LFS pointers.

Tracked symlinks are restored as symlinks on Unix for source tree, index, and
conflict-stage restores. The stored blob bytes are used directly as the link
target and are not interpreted as a path to open during restore, which avoids
writing through a link that points outside the worktree. `--merge` conflict
marker rebuilds also replace an existing worktree symlink before writing the
regular marker file. Platforms that cannot create symlinks return an explicit
unsupported diagnostic instead of materializing a regular file containing the
target text.

## Options

| Option | Short | Long | Description |
|--------|-------|------|-------------|
| Pathspec | | positional (required) | One or more files or directories to restore. Supports shared pathspec magic. Use `.` for all files. |
| Source | `-s` | `--source <tree-ish>` | Restore from the specified commit or tree-ish instead of the default source. When omitted, the default source depends on the mode: index for worktree restore, HEAD for staged restore. |
| Staged | `-S` | `--staged` | Restore the index (unstage files). Defaults the source to HEAD if `--source` is not given. |
| Worktree | `-W` | `--worktree` | Restore the working tree. This is the default when `--staged` is not given. |
| Ours | `-2` | `--ours` | For an unmerged path, write conflict stage 2 (our side) to the working tree. Mutually exclusive with `--theirs`, `--source`, `--staged`, and `--ignore-unmerged`. |
| Theirs | `-3` | `--theirs` | For an unmerged path, write conflict stage 3 (their side) to the working tree. Same exclusions as `--ours`. |
| Merge | | `--merge` | For an unmerged path, rewrite the working tree with the conflict markers rebuilt from the index stages (`ours` from stage 2, `theirs` from stage 3), leaving the index unmerged. Libra writes whole-file `ours`/`theirs` markers (with generic `ours`/`theirs` labels) — not Git's line-level 3-way. (Note: `libra merge`/`cherry-pick` now write line-level markers via the three-way merge engine; restore's index-stage rebuild remains whole-file.) Same exclusions as `--ours`. |
| Conflict style | | `--conflict <style>` | Implies `--merge`. `merge` (default) writes `ours`/`theirs` blocks; `diff3` also includes the `base` block (stage 1). `zdiff3` is not supported. |
| Ignore unmerged | | `--ignore-unmerged` | Skip unmerged paths instead of erroring; the remaining paths still restore. |
| Pathspec from file | | `--pathspec-from-file <FILE>` | Read shared-matcher pathspecs from `<FILE>` (one per line; `-` reads stdin). When given, the file contents replace any positional pathspecs (which then need not be supplied). |
| Pathspec file NUL | | `--pathspec-file-nul` | Pathspecs read via `--pathspec-from-file` are separated by NUL, not newlines (requires `--pathspec-from-file`). |
| No progress | | `--no-progress` | Do not show a progress meter. Accepted no-op for Git parity: Libra's restore never renders a progress meter. |
| Overlay | | `--overlay` | Restore in overlay mode: only create/update paths present in the source; tracked paths absent from the source are left alone instead of removed. Toggle pair with `--no-overlay` (last one wins). |
| No overlay | | `--no-overlay` | Do not restore in overlay mode (the default): paths absent from the source are removed so the target matches it exactly. Toggle pair with `--overlay` (last one wins). |
| JSON | | `--json` | Emit structured JSON output. |
| Quiet | | `--quiet` | Suppress human-readable output. |

### Option Details

**`--source` / `-s`**

Specify a commit, tag, or any tree-ish as the restore source:

```bash
# Restore from the previous commit
libra restore --source HEAD~1 src/main.rs

# Restore from a specific commit hash
libra restore -s abc1234 lib/
```

**`--staged` / `-S`**

Restores the index from HEAD (or `--source`), effectively unstaging files:

```bash
# Unstage a file
libra restore --staged file.txt

# Unstage all files
libra restore --staged .
```

**`--worktree` / `-W`**

Explicitly targets the working tree. This is the default when `--staged` is not specified, so it is only needed when combining with `--staged`:

```bash
# Restore both index and working tree
libra restore -S -W file.txt
```

**Conflict-stage restore: `--ours` / `-2`, `--theirs` / `-3`, `--ignore-unmerged`**

When a merge leaves a path unmerged, the index holds up to three conflict stages: stage 1 (the merge base), stage 2 ("ours" — the current branch), and stage 3 ("theirs" — the branch being merged). After editing a conflict-marked file you can take one whole side back:

```bash
# Take our side of the conflict
libra restore --ours file.txt

# Take their side of the conflict
libra restore --theirs file.txt
```

These flags read the conflict stages and rewrite **only the working tree** — the index is intentionally left unmerged, so `libra status` still reports the conflict until you stage a resolution with `libra add`. They are worktree-only by design and therefore reject `--source` and `--staged` (and each other) at the CLI layer (`LBR-CLI-002`, exit code 129).

**Modify/delete conflicts.** When the requested side deleted the file (a modify/delete conflict — the requested stage is absent), restoring that side means *removing* the file: in the default (`--no-overlay`) mode `libra restore --theirs <path>` deletes the working-tree file and exits 0 (matching `git restore`). Under `--overlay`, which never removes paths, the same case is instead an error — `path '<file>' does not have their version` (`LBR-CONFLICT-001`, exit 128).

**During a rebase, `--ours`/`--theirs` are swapped** (as in Git): the stages are read verbatim, and a rebase records stage 2 = the branch you are rebasing *onto* (the new base) and stage 3 = the commit being replayed. So mid-rebase `--ours` gives the onto side and `--theirs` the replayed commit — the reverse of a normal merge or cherry-pick (where `--ours` = your `HEAD` and `--theirs` = the incoming side).

`--ours`/`--theirs` act only on unmerged paths. A non-conflicted pathspec is skipped; if *every* pathspec is non-conflicted the command reports no match (`LBR-CONFLICT-001`, exit 128). Libra intentionally does **not** fall through to Git's plain index (stage-0) restore for a clean tracked path here, so a locally modified file is never silently reverted — use a plain `libra restore <path>` for that.

A plain `libra restore` over an unmerged path refuses to act and reports `path '<file>' is unmerged` (`LBR-CONFLICT-001`, exit 128) so a conflict is never silently overwritten or skipped. Pass `--ignore-unmerged` to skip the unmerged paths and restore the rest:

```bash
# Restore everything from HEAD, skipping still-conflicted paths
libra restore --ignore-unmerged --source HEAD .
```

> **Not yet supported:** Git's line-level 3-way conflict markers and the `zdiff3` style (Libra rebuilds whole-file `ours`/`theirs` markers from the index stages — unlike `libra merge`/`cherry-pick`, which now write line-level markers), and `-p` / `--patch`, are deferred. See [COMPATIBILITY.md](../../COMPATIBILITY.md).

## Common Commands

```bash
# Discard unstaged changes to a file (restore from index)
libra restore file.txt

# Unstage a file (restore index from HEAD)
libra restore --staged file.txt

# Restore from a specific commit
libra restore --source HEAD~1 src/main.rs

# Restore Rust files except generated output
libra restore ':(glob)src/*.rs' ':(exclude)src/generated.rs'

# Restore both working tree and index
libra restore -S -W file.txt

# Restore everything from HEAD
libra restore --source HEAD .

# Restore a tracked symlink as a symlink
libra restore --source HEAD link-to-target

# Take our / their side of a merge conflict
libra restore --ours file.txt
libra restore --theirs file.txt

# Restore from HEAD, skipping still-conflicted paths
libra restore --ignore-unmerged --source HEAD .

# Overlay restore: update files from an older commit without deleting newer ones
libra restore --overlay --source HEAD~3 .

# JSON output for scripting
libra restore --json --source HEAD .
```

## Human Output

```text
Updated 3 path(s) from HEAD
```

The confirmation reports a count over the union of files restored *and*
deleted (i.e. when a tracked file is removed in the source it gets
deleted from the worktree/index). When `--source` is omitted, the
source label is `HEAD` for `--staged` restores and `the index` for
worktree-only restores:

```text
Updated 1 path(s) from the index
```

`--quiet` suppresses all output. If neither a restored nor a deleted
path matched, no confirmation is emitted (so a no-op restore is
silent).

## Structured Output (JSON)

```json
{
  "command": "restore",
  "data": {
    "source": "HEAD",
    "worktree": true,
    "staged": false,
    "restored_files": ["src/main.rs"],
    "deleted_files": []
  }
}
```

When restoring from the index (no `--source` specified for worktree restore), the `source` field is `null`.

## Design Rationale

### Why separate from checkout?

Git's `checkout` command serves two very different purposes: switching branches and restoring files. This overloading is widely recognized as one of Git's worst UX decisions. Git itself addressed this by introducing `git restore` (for files) and `git switch` (for branches) in Git 2.23. Libra follows this split from the start, making `restore` the preferred command for file content and never for branch operations. `checkout -- <path>` remains available only as a compatibility alias for users bringing Git muscle memory.

### Why explicit `--worktree` / `--staged` flags?

Git's `restore` defaults to worktree-only restoration and requires `--staged` to target the index. Libra follows the same convention but makes the flags orthogonal and composable:

- No flag: worktree only (from index).
- `--staged`: index only (from HEAD).
- `--staged --worktree`: both targets.

This explicit model eliminates the confusion in Git's `checkout` where `git checkout -- file` restores the worktree and `git checkout HEAD -- file` restores both worktree and index, a distinction that many users never internalize.

### Why is `--source` auto-set to HEAD for `--staged`?

When unstaging files, the natural source is HEAD (the last commit). Requiring `--source HEAD` every time would be tedious and error-prone. Libra auto-defaults to HEAD when `--staged` is used without `--source`, matching Git's behavior and user expectations.

### Why require pathspec?

Unlike `git restore` which can operate on the entire worktree with `--worktree`, Libra requires at least one pathspec argument. This prevents accidental restoration of the entire working tree. Use `.` as a pathspec when you intentionally want to restore everything.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Pathspec | `<pathspec>...` (required) | `<pathspec>...` (optional) | `jj restore <paths>...` |
| Source commit | `-s` / `--source <tree-ish>` | `-s` / `--source <tree>` | `--from <revision>` |
| Target worktree | `-W` / `--worktree` | `-W` / `--worktree` (default) | Default behavior |
| Target index/staging | `-S` / `--staged` | `-S` / `--staged` | N/A (no staging area) |
| Both targets | `-S -W` | `-S -W` | N/A |
| Pathspec from file | `--pathspec-from-file <FILE>` / `--pathspec-file-nul` | `--pathspec-from-file` / `--pathspec-file-nul` | N/A |
| Overlay mode | `--overlay` / `--no-overlay` (last wins; default is no-overlay = remove absent paths) | `--overlay` / `--no-overlay` | N/A |
| Conflict resolution | `--ours` / `-2`, `--theirs` / `-3`, `--merge`, `--conflict=merge\|diff3` (worktree-only; whole-file markers) | `--ours` / `--theirs` / `--merge` / `--conflict` | `--restore-descendants` |
| Skip unmerged | `--ignore-unmerged` | `--ignore-unmerged` | N/A |
| Patch mode | Not supported | `-p` / `--patch` | N/A |
| No progress meter | `--no-progress` (no-op; never renders one) | `--no-progress` | N/A |
| Progress meter | Not supported | `--progress` | N/A |
| Target revision | Not supported | N/A | `--to <revision>` |
| Restore changes into | Not supported | N/A | `--changes-in <revision>` |
| JSON output | `--json` | Not supported | N/A |
| Quiet mode | `--quiet` | Not supported | N/A |

Note: jj's `restore` operates on revisions rather than a staging area, restoring the content of one revision into another. It does not distinguish between staged and unstaged changes.

## Error Handling

| Code | Condition |
|------|-----------|
| `LBR-REPO-001` | Not a libra repository |
| `LBR-CLI-003` | Failed to resolve source reference |
| `LBR-CLI-002` | Invalid path encoding |
| `LBR-IO-001` | Failed to read index or object |
| `LBR-IO-002` | Failed to write worktree file |
| `LBR-NET-001` | LFS download failed |
| `LBR-CONFLICT-001` | Path is unmerged and no conflict-resolution flag was given, or `--ours`/`--theirs` requested a missing conflict stage (exit 128) |

> `--ours` and `--theirs` are mutually exclusive with each other and with `--source`, `--staged`, and `--ignore-unmerged`; any such combination is rejected as `LBR-CLI-002` with exit code 129. (`--source`, `--staged`, and `--ignore-unmerged` may otherwise be combined — e.g. `--ignore-unmerged --source HEAD`.)
