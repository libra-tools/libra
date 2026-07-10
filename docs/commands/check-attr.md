# `libra check-attr`

Report the Git/Libra attributes that apply to one or more pathnames — the Libra
analogue of `git check-attr`.

> Intentional difference (see
> [`docs/development/commands/_compatibility.md`](../development/commands/_compatibility.md)
> decision **D5**): Libra does **not** implement the Git `.gitattributes`
> smudge/clean filter bridge. `check-attr` is a read-only query over
> attributes, not a filter driver.

Attribute sources are applied from lower to higher precedence:
`core.attributesFile`, per-directory `.gitattributes` from root to child,
same-directory `.libra_attributes`, then `.git/info/attributes`.
Libra extension files override sibling `.gitattributes` rules, while
`.git/info/attributes` keeps Git's highest-precedence worktree-local tier.

## Synopsis

```
libra check-attr [-z] <attr>... [--] <pathname>...
libra check-attr [-z] -a | --all [--] <pathname>...
libra check-attr [-z] (<attr>... | --all) --stdin
```

## Description

For every `(pathname, attribute)` pair, `check-attr` prints the attribute's
value. The value is one of:

- `lfs` — for the `filter` attribute when the path is LFS-tracked
  (an attributes source has a `filter=lfs` pattern matching it).
- `set` / `unset` — for bare attributes or `-attr` rules.
- `unspecified` — the attribute is not set on the path.

`--all` reports only the attributes that are actually **set** on each path
(for example `filter: lfs` or `diff: <driver>`).

The command always exits `0` on success, even when every queried attribute is
`unspecified`; it exits `128` on a usage or repository error.

## Argument forms

Because attributes and pathnames are both positional, they are disambiguated as:

- `--all`: every positional argument is a pathname.
- An explicit `--`: attributes precede it, pathnames follow it.
- `--stdin`: positional arguments are attribute names; pathnames come from stdin.
- Otherwise: the **first** positional is the attribute and the rest are
  pathnames (use `--` for multiple attributes).

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<attr>...` | Attribute names to query. | `libra check-attr filter a.bin` |
| `<pathname>...` | Paths to test (after `--`, or following the attribute). | `libra check-attr filter -- a.bin b.c` |
| `-a`, `--all` | Report every attribute set on each path. | `libra check-attr --all data.bin` |
| `--stdin` | Read pathnames from standard input. | `libra check-attr filter --stdin` |
| `-z` | NUL-delimit `--stdin` input and output. | `libra check-attr -z filter --stdin` |
| `--json` / `--machine` | Structured output: `{ results: [{ path, attr, value }] }`. | `libra check-attr --json filter a.bin` |

## Output

- Default: `<path>: <attr>: <value>` per line.
- `-z`: the three fields NUL-separated, each record NUL-terminated.

## Examples

```bash
# Is a.bin run through the LFS filter?
libra check-attr filter a.bin
# -> a.bin: filter: lfs   (if an attributes source tracks *.bin)

# Query multiple attributes (use -- to separate)
libra check-attr filter text -- a.bin notes.txt

# All set attributes for a path
libra check-attr --all a.bin

# Stream pathnames from another command
libra ls-files -z | libra check-attr -z filter --stdin

# Structured output for agents
libra check-attr --json filter a.bin
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Query an attribute | `libra check-attr filter a.bin` | `git check-attr filter a.bin` |
| All attributes | `libra check-attr --all a.bin` | `git check-attr --all a.bin` |
| From stdin | `libra check-attr filter --stdin` | `git check-attr filter --stdin` |

Libra reads attributes such as `filter`, `diff`, and `export-ignore`, but it
does not run smudge/clean filters. Git's `--cached`, `--source`, and attributes
macro expansion are not exposed.
