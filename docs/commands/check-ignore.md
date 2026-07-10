# `libra check-ignore`

Report which pathnames are excluded by Git/Libra ignore rules — the equivalent
of `git check-ignore`, with Libra extension files preserved.

Libra reads standard Git ignore sources (`.gitignore`, `.git/info/exclude`, and
`core.excludesFile`) as well as Libra extension files (`.libraignore`). Within
the same directory, `.libraignore` has higher precedence than `.gitignore`; a
nearer directory source overrides an ancestor; `.git/info/exclude` and
`core.excludesFile` are lower-precedence fallbacks. Pattern syntax is Git
ignore syntax.

## Synopsis

```
libra check-ignore [-v] [-n] [-z] [--no-index] <pathname>...
libra check-ignore [-v] [-n] [-z] [--no-index] --stdin
```

## Description

For each `<pathname>` (given on the command line, or read from `--stdin`),
`check-ignore` consults the active ignore sources and prints the paths that are
**ignored** (excluded). It is a read-only query: it never changes the index or
the working tree.

By default a path that is already tracked in the index is reported as *not*
ignored (an explicit `add` overrides the rules). Pass `--no-index` to report a
raw pattern match even for a tracked path — useful for debugging why a path was
not ignored as expected.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<pathname>...` | One or more paths to test. Mutually exclusive with `--stdin`. | `libra check-ignore build/ a.log` |
| `--stdin` | Read pathnames from standard input instead of the command line (newline-separated, or NUL-separated with `-z`). | `libra check-ignore --stdin < paths.txt` |
| `-z` | Use NUL (`\0`) as the delimiter for `--stdin` input and for output. Safe for pathnames containing whitespace or newlines. | `libra check-ignore -z --stdin` |
| `-v`, `--verbose` | For every matched path, also print the deciding rule: `<source>:<line>:<pattern>\t<path>`. The line number is recovered by scanning the source file. | `libra check-ignore -v target/` |
| `-n`, `--non-matching` | Also output pathnames that match no pattern (with empty source/line/pattern fields). Requires `-v`. | `libra check-ignore -v -n a.txt b.log` |
| `--no-index` | Do not consult the index; report a pattern match even for a tracked path. | `libra check-ignore --no-index tracked.log` |
| `--json` / `--machine` | Structured output: `{ results: [{ path, ignored, source?, line?, pattern? }] }`. | `libra check-ignore --json target/` |

## Exit codes

Aligned with Git:

| Code | Meaning |
|------|---------|
| `0` | At least one of the given paths is ignored. |
| `1` | None of the given paths are ignored (a clean signal, not an error; no output on stderr). |
| `128` | Usage error (e.g. `-n` without `-v`, paths combined with `--stdin`) or not inside a repository. |

## Output

- Default: one ignored pathname per line (NUL-terminated with `-z`).
- `-v`: `<source>:<line>:<pattern>\t<path>` per line. With `-z`, the four fields
  are NUL-separated and the record is NUL-terminated.
- `-n` (with `-v`): non-matching paths appear with empty source/line/pattern.

## Examples

```bash
# Is target/ ignored?
libra check-ignore target/

# Show the rule that ignores each path
libra check-ignore -v build/ debug.log

# Stream pathnames from another command, NUL-framed
libra ls-files --others -z | libra check-ignore -z --stdin

# Debug: would this path match a rule even though it is tracked?
libra check-ignore --no-index src/generated.rs

# Structured output for agents
libra check-ignore --json target/ node_modules/
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Check a path | `libra check-ignore target/` | `git check-ignore target/` |
| Show the matching rule | `libra check-ignore -v target/` | `git check-ignore -v target/` |
| Read from stdin | `libra check-ignore --stdin` | `git check-ignore --stdin` |
| Ignore the index | `libra check-ignore --no-index p` | `git check-ignore --no-index p` |

Not exposed (deferred): Git's `--exclude`, `--exclude-from`,
`--exclude-per-directory`, and full pathspec magic.
