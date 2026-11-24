# `libra replace`

Substitute one object for another whenever an object is read — a focused subset
of `git replace`.

## Synopsis

```
libra replace [-f] <object> <replacement>
libra replace -d <object>...
libra replace [-l] [<pattern>]
```

## Description

A replacement records that, whenever `<object>` would be read, `<replacement>`
should be returned instead. The substitution is applied in the object-loading
layer (`load_object`), so it is honoured transparently by every reader that goes
through it — `log`, `show`, `rev-parse` peeling, and so on — not just one command.

- **create** (`libra replace <object> <replacement>`) — record the replacement.
  Both objects must exist; their types must match unless `-f` is given. An
  existing replacement is only overwritten with `-f`. An object cannot replace
  itself.
- **delete** (`-d <object>...`) — remove the replacement(s) one item at a time.
  Each `<object>` is resolved without peeling: a short id of ANY object type
  works, and a full-length 40/64-hex id is accepted even when the object is
  absent, so a replacement ref can still be removed. Every success prints
  `Deleted replace ref '<oid>'`; an unresolvable item or one without a
  replacement is reported and the remaining items still run, after which the
  command fails with `LBR-CLI-003` and exit code 128 (Git exits 1 here — an
  intentional difference).
- **list** (`-l [<pattern>]`) — print the ids of replaced objects, one per line
  (Git's default `short` format), optionally filtered by a `wildmatch` pattern
  matched against the WHOLE object id (`*`, `?` and `[...]` are wildcards; the
  default pattern is `*`). `--format=medium/long` also shows the replacement oid
  with its type. `-l` accepts at most one pattern.

Replacements are stored as loose refs under `.libra/refs/replace/<oid>` (Git's
`refs/replace/` namespace).

## Options

| Option | Description |
|--------|-------------|
| `-f`, `--force` | Overwrite an existing replacement and allow a type mismatch. |
| `-d`, `--delete` | Delete the replacement(s) for the given object(s). |
| `--format=<short\|medium\|long>` | `-l` output format: `short` (replaced oid), `medium` (`<replaced> -> <replacement>`), `long` (both with object types) | `libra replace -l --format=medium` |
| `--graft` | Create a replacement commit with the given parents: `replace --graft <commit> [<parent>...]` (none = parentless) | `libra replace --graft HEAD` |
| `--edit` | Accepted for Git parity: behaves like `--graft` when parents are given, otherwise a no-op (Libra never opens an editor) | `libra replace --edit HEAD` |
| `--convert-graft-file` | Convert `.libra/info/grafts` into replace refs and remove the file | `libra replace --convert-graft-file` |
| `-l`, `--list` | List replaced object ids, optionally filtered by a wildmatch `<pattern>` matched against the whole object id. |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `128` | Not inside a repository, an invalid object, a missing replacement to delete, a type mismatch without `-f`, an existing replacement without `-f`, or an IO error. |

## Examples

```bash
# Make history read an amended commit in place of the original
libra replace <old-commit> <new-commit>
libra log         # now shows the replacement

libra replace -l                 # list replaced objects
libra replace -d <old-commit>    # stop replacing it
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Create | `libra replace <o> <r>` | `git replace <o> <r>` |
| Delete | `libra replace -d <o>` | `git replace -d <o>` |
| List | `libra replace -l` | `git replace -l` |

Differences and deferred features: replacements are stored as loose refs under
`.libra/refs/replace/` rather than in the SQLite reference table, so `show-ref` /
`for-each-ref` do not list them yet; `-l` prints object ids only (Git's default
short format) and filters by whole-name `wildmatch`; `--format`, `--edit`,
`--graft`, and `--convert-graft-file` are not implemented. Mode validation
matches Git: `-d` with `-l`, `--format` without `-l`, `-f` outside a write
mode, more than one `-l` pattern, and a create with anything other than exactly
two positional arguments are all usage errors (`LBR-CLI-002`) rejected before
any ref is read or written.
