# `libra apply`

Check whether a patch applies cleanly — an MVP of
`git apply --check`. This version validates only: it parses the patch,
safety-checks every target path, and test-applies each file's hunks against the
current working tree **without writing anything**.

## Synopsis

```
libra apply --check [-p<n>] [<patch>...]
```

## Description

`apply --check` reads one or more patches (from the named files, or
from stdin when none are given), splits them into per-file sections, and for
each file:

1. parses the hunks (a malformed patch is a fatal error);
2. resolves the target path, stripping `<n>` leading components (`-p<n>`,
   default 1) and rejecting any path that is absolute, contains an empty, `.`,
   or `..` component or NUL, points inside `.libra/`, or traverses an existing
   symlink component;
3. test-applies the hunks to the current file content (an empty base for a
   new-file patch whose source is `/dev/null`).

If every file applies, the exit code is 0; if any file does not apply, it is 1.
The working tree is never modified. Actually applying a patch (with an atomic
temp-file + rename) is a planned future extension; `--check` is required today.

Patches larger than 64 MiB are rejected.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--check` | Validate without writing (required in this version). | `libra apply --check fix.patch` |
| `-p<n>` | Strip `<n>` leading path components from each path (default 1). | `libra apply --check -p0 fix.patch` |
| `--json` / `--machine` | Structured output: `{ applies, files }`. | `libra --json apply --check fix.patch` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The patch applies cleanly. |
| `1` | The patch does not apply (conflicting context or a missing target). |
| `128` | Not inside a repository, `--check` was omitted, the patch is malformed/oversized/non-UTF-8, or a target path is unsafe. |

## Examples

```bash
# Will this patch apply to the current tree?
libra apply --check fix.patch && echo "clean"

# Patch made without a/ b/ prefixes
libra apply --check -p0 fix.patch

# From a pipeline
git format-patch -1 --stdout | libra apply --check
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Check a patch | `libra apply --check p` | `git apply --check p` |
| Path strip | `libra apply --check -p0 p` | `git apply --check -p0 p` |

Differences and deferred features: actually applying the patch (without
`--check`), `--index` / `--cached`, `--3way`, `--reverse`, `--unidiff-zero`,
binary patches, and rename/mode hunks are not yet supported. Conflict markers
are never written — `--check` only reports.
