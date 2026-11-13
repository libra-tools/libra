# `libra apply`

Apply a unified-diff patch to the working tree, or validate it — a
Git-compatible subset of `git apply`.

## Synopsis

```
libra apply [--check] [--stat | --numstat | --summary] [--apply] [-R]
            [--3way] [--index] [--include=<pat>...] [--exclude=<pat>...]
            [--directory=<dir>] [-p<n>] [<patch>...]
```

## Description

`apply` reads one or more patches (from the named files, or from stdin when none
are given), splits them into per-file sections, and for each file:

1. parses the hunks (a malformed patch is a fatal error);
2. resolves the target path, stripping `<n>` leading components (`-p<n>`,
   default 1) and rejecting any path that is absolute, contains NUL, or escapes
   the worktree;
3. prepares the patched bytes in memory (so a multi-file patch fails before the
   first write), then atomically replaces each target.

Without `--check` the working tree is written. `--check` validates only.
`--stat`, `--numstat`, and `--summary` print a report and do not apply unless
`--apply` is also given (in which case the report is printed first, then the
patch is applied). `-R`/`--reverse` applies the patch backwards.
A patch that does not apply reports the first failing target and exits 1 without
writing.

Extended git sections are supported: `GIT binary patch` (literal and delta),
`rename from`/`to`, `copy from`/`to`, mode-only changes, and
`new file mode`/`deleted file mode` annotations.

Patches larger than 64 MiB are rejected.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--check` | Validate without writing. | `libra apply --check fix.patch` |
| `--stat` | Print a Git-style diffstat and exit without applying. | `libra apply --stat fix.patch` |
| `--numstat` | Print `insertions\tdeletions\tpath` and exit without applying. | `libra apply --numstat fix.patch` |
| `--summary` | Print create/delete/mode-change lines and exit without applying. | `libra apply --summary fix.patch` |
| `--apply` | Apply even with a report flag: print the report first, then apply. | `libra apply --stat --apply fix.patch` |
| `-R`, `--reverse` | Apply the patch in reverse (swap old/new sides). | `libra apply -R fix.patch` |
| `--3way` | Three-way fallback using the patch's `index` blobs when a hunk does not apply; conflicting merges leave markers and warn. | `libra apply --3way fix.patch` |
| `--index` | Require the working tree to match the index for every touched path (`<path>: does not match index` otherwise, with no writes), then update both. | `libra apply --index fix.patch` |
| `--cached` | Update only the index; never touch the working tree. Incompatible with `--index`. | `libra apply --cached fix.patch` |
`--include`/`--exclude` are matched in command-line order and the first matching pattern decides (Git wildmatch).

| `--include=<pat>` | Only apply sections whose target matches the pattern. | `libra apply --include='src/*' fix.patch` |
| `--exclude=<pat>` | Skip sections whose target matches the pattern. | `libra apply --exclude='docs/*' fix.patch` |
| `--directory=<dir>` | Prepend a directory to every patched path (it must exist). | `libra apply --directory=build fix.patch` |
| `-p<n>` | Strip `<n>` leading path components (default 1). | `libra apply -p0 fix.patch` |
| `--json` / `--machine` | Structured output: `{ applies, files, applied }`. | `libra --json apply --check fix.patch` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The patch applied (or validated cleanly). |
| `1` | The patch does not apply (conflicting context or a missing target). |
| `128` | Not inside a repository, the patch is malformed/oversized/non-UTF-8, or a target path is unsafe. |

## Examples

```bash
# Apply a patch
libra apply fix.patch

# Validate first
libra apply --check fix.patch && echo "clean"

# Reverse an already-applied patch
libra apply -R fix.patch

# Three-way fallback, then stage the result
libra apply --3way --index fix.patch

# Patch made without a/ b/ prefixes
libra apply -p0 fix.patch

# From a pipeline
git format-patch -1 --stdout | libra apply --check
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Apply a patch | `libra apply p` | `git apply p` |
| Check a patch | `libra apply --check p` | `git apply --check p` |
| Reverse-apply | `libra apply -R p` | `git apply -R p` |
| Three-way fallback | `libra apply --3way p` | `git apply --3way p` |

Deferred: `--unidiff-zero`, `--recount`, and `--allow-empty`. Libra's `--3way`
does not pre-check that the index matches (Git refuses first), and conflict
markers are written with a warning + exit 1 like Git.
