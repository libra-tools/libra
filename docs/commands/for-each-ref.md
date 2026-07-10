# `libra for-each-ref`

List local refs with filtering and custom formatting.

> Status: public CLI with partial Git compatibility. The command enumerates references stored in Libra's SQLite-backed ref model. It covers local branches, remote-tracking branches, tags, and `--points-at` filtering. It does not read `.git/refs` or `packed-refs`.

## Synopsis

```sh
libra for-each-ref [--heads] [--tags] [--remotes] [--all] [--format=<format>] [--sort=<key>] [--count=<n>] [--points-at=<object>] [--shell | --perl | --python | --tcl] [<pattern>...]
```

## Description

`libra for-each-ref` enumerates refs stored in the repository (branches, tags, and remote-tracking refs) and prints each ref's object hash and name. Use `--heads`, `--tags`, or `--remotes` to restrict output to one namespace; the default is `--all`.

Positional `<pattern>` arguments act as substring filters on the fully-qualified ref name (e.g., `refs/heads/main`). Only refs whose name matches, contains, or ends with at least one pattern are included.

Use `--points-at <object>` to keep refs that point at the resolved object. Annotated tags match both their tag object and their peeled target commit, matching Git's common `for-each-ref --points-at HEAD` behavior.

When stdout is piped and the downstream command exits early, `libra for-each-ref` exits quietly
without printing panic/backtrace or `Broken pipe` diagnostics.

The `--format` option accepts Git's `%(atom)` language for refs. This is
separate from `log`/`show`/`shortlog`'s `%` pretty-format placeholders.
Supported atoms:

| Atom | Value |
|---|---|
| `%(refname)` | Full ref name, e.g. `refs/heads/main` |
| `%(refname:short)` | Short ref name (namespace prefix stripped), e.g. `main` |
| `%(refname:lstrip=N)` | Ref name with `N` leading path components removed (`N<0` keeps the last `|N|`) |
| `%(refname:rstrip=N)` | Ref name with `N` trailing path components removed (`N<0` keeps the first `|N|`) |
| `%(objectname)` | Object hash the ref points to |
| `%(objectname:short)` | Abbreviated object hash (7 characters) |
| `%(objectname:short=N)` | Abbreviated object hash to `N` characters (capped at the full length) |
| `%(objecttype)` | Object type: `commit`, `tag`, `tree`, or `blob` |
| `%(*objectname)` | The object an annotated tag dereferences to (its peeled target); empty for non-tag refs |
| `%(*objectname:short)` | Abbreviated dereferenced object hash (7 characters); empty for non-tag refs |
| `%(*objecttype)` | Type of the dereferenced object (e.g. `commit`); empty for non-tag refs |
| `%(*objectsize)` | Byte size of the dereferenced object; empty for non-tag refs |
| `%(objectsize)` | Byte size of the object the ref points at directly (the tag object for an annotated tag, not the peeled commit) |
| `%(HEAD)` | `*` if the ref is the currently checked-out branch, otherwise a space |
| `%(upstream)` | The branch's upstream tracking ref (e.g. `refs/remotes/origin/main`); empty when none |
| `%(upstream:short)` | The upstream ref with the `refs/remotes/` prefix stripped (e.g. `origin/main`) |
| `%(push)` | The branch's push tracking ref. The push remote follows `branch.<name>.pushRemote`, then `remote.pushDefault`, then `branch.<name>.remote`; empty when none |
| `%(push:short)` | The push ref with the `refs/remotes/` prefix stripped |
| `%(symref)` | For a symbolic ref (e.g. `refs/remotes/<remote>/HEAD`), the full ref name it points to; empty for ordinary refs |
| `%(symref:short)` | The symbolic-ref target with its namespace prefix stripped (e.g. `origin/main`); empty for ordinary refs |
| `%(symref:lstrip=N)` / `%(symref:rstrip=N)` | The symbolic-ref target with `N` leading / trailing path components removed (`N<0` keeps the last / first `\|N\|`); empty for ordinary refs |
| `%(worktreepath)` | Absolute path of the worktree that has this ref checked out; empty otherwise. Libra worktrees share one HEAD, so the checked-out branch is the current HEAD branch and the path is the current worktree (the one the command runs in) — git-compatible for a single-worktree repo. |
| `%(subject)` | First line of the ref object's message (commit or annotated-tag message); empty for trees/blobs |
| `%(contents)` | Full message of the commit/annotated-tag object |
| `%(contents:subject)` | Same as `%(subject)` |
| `%(body)` / `%(contents:body)` | Message body — everything after the first blank line |
| `%(authorname)` | Commit author name (empty for non-commit refs such as annotated tags) |
| `%(authoremail)` | Commit author email, angle-bracketed (e.g. `<a@example.com>`); empty for non-commit refs |
| `%(committername)` | Commit committer name; empty for non-commit refs |
| `%(committeremail)` | Commit committer email, angle-bracketed; empty for non-commit refs |
| `%(taggername)` | Annotated-tag tagger name; empty for non-tag refs (lightweight tags and commits) |
| `%(taggeremail)` | Annotated-tag tagger email, angle-bracketed; empty for non-tag refs |
| `%(authordate)` | Commit author date in Git's default format; empty for non-commit refs |
| `%(committerdate)` | Commit committer date in Git's default format; empty for non-commit refs |
| `%(taggerdate)` | Annotated-tag tagger date in Git's default format; empty for non-tag refs |
| `%(creatordate)` | Ref creation date — the committer date for commits/lightweight tags, the tagger date for annotated tags |
| `%(authordate:<fmt>)` / `%(committerdate:<fmt>)` / `%(taggerdate:<fmt>)` / `%(creatordate:<fmt>)` | The same dates in a chosen format (see the date-format note below) |
| `%(tree)` / `%(tree:short)` | The commit's tree id (full / 7-char); empty for non-commit refs |
| `%(parent)` / `%(parent:short)` | The commit's parent ids, space-separated (full / 7-char); empty for a root commit or non-commit ref |
| `%(numparent)` | The commit's parent count; empty for non-commit refs |
| `%(color:<spec>)` | An ANSI color/attribute escape (e.g. `%(color:red)`, `%(color:bold green)`, `%(color:reset)`), emitted only when color is enabled (`--color=always`, or `auto` to a terminal); empty under `--color=never`/`NO_COLOR` (under `--shell`/etc. the value is still quoted like any atom). The spec is a space-separated list: up to two colors (foreground then background) plus attributes. Supports the 8 basic names + `bright<name>`, `default`, 256-color indices, `#rrggbb`, and the `bold`/`dim`/`italic`/`ul`/`blink`/`reverse`/`strike` attributes (with both compact `nobold` and hyphenated `no-bold` negation). A third color or an unrecognized word is a format error. When a row leaves color active, a trailing reset (`\x1b[m`, Git's `GIT_COLOR_RESET`) is appended so color does not bleed into the next row (under `--shell`/etc. it is a separate quoted field); an explicit trailing `%(color:reset)` is not doubled. |

Date atoms use Git's default format (`Day Mon DD HH:MM:SS YYYY +ZZZZ`) and, like
`libra log`, render in UTC (`+0000`) rather than the commit's original timezone.
Date atoms accept a `:<format>` modifier — `%(committerdate:iso)`, `%(authordate:short)`, `%(taggerdate:unix)`, `%(creatordate:relative)`, etc. Supported formats: `default`, `short`, `iso`/`iso8601`, `iso-strict`/`iso8601-strict`, `rfc`/`rfc2822`, `unix`, `raw`, and `relative` (git-style "… ago"). `%(creatordate)` resolves to the committer date for commits/lightweight tags and the tagger date for annotated tags. The `local`/`human`/`format:<strftime>` modifiers are not yet supported (they fall back to the default format).

The `%(align:<width>[,<position>])` … `%(end)` block pads everything it encloses
to `<width>` display columns. `<position>` is `left` (the default), `right`, or
`middle`; width and position may be given positionally in either order
(`%(align:10,right)` or `%(align:right,10)`) or as `width=`/`position=`
key/value pairs. Content already at or beyond the width is left unchanged (no
truncation), and align blocks may nest. Under `--shell`/`--perl`/`--python`/`--tcl`,
the block's contents render unquoted and the whole padded block is quoted once
as a single string literal (matching Git: only the topmost align block is
quoted; nested blocks and block literals do not quote separately).

The `%(if[:equals=<v>|:notequals=<v>])` … `%(then)` … [`%(else)` …] `%(end)` conditional emits the then-branch when the condition between `%(if…)` and `%(then)` holds, else the else-branch (which may be omitted). A plain `%(if)` is true when its rendered condition is non-empty after trimming whitespace; `equals`/`notequals` compare the raw rendered value. Conditionals nest, and nest inside `%(align)` blocks (sharing the `%(end)` terminator). `%(raw)` / `%(raw:size)` emit the raw decompressed object content and its byte size (the size equals `%(objectsize)`; `%(raw)` is rejected with `--shell`/`--python`/`--tcl`, matching Git). `%(raw)` supports text objects (commits, annotated tags); a non-UTF-8 (binary) object is rejected rather than lossily transcoded. `%(describe[:<opts>])` runs `git describe` on each ref's commit, with the `tags`, `abbrev=<n>`, `match=<glob>`, and `exclude=<glob>` options (comma-separated; an unrecognized option is a usage error, validated even when no ref matches); a commit with no reachable tag renders as an empty string. `%(symref)` / `%(symref:short)` / `%(symref:lstrip=N)` / `%(symref:rstrip=N)` give the target a symbolic ref points to (e.g. `refs/remotes/<remote>/HEAD`); they are empty for ordinary refs. `%(worktreepath)` gives the absolute path of the worktree that has the ref checked out (empty otherwise); Libra worktrees share one HEAD, so the checked-out branch is the current HEAD branch and the path is the current worktree (the one the command runs in) — matching git for a single-worktree repo. The supported atom set is comprehensive; the remaining niche atom `%(deltabase)` is not implemented.

## Options

| Option | Description |
|---|---|
| `--heads` | List local branch refs under `refs/heads/`. |
| `--tags` | List tag refs under `refs/tags/`. |
| `--remotes` | List remote-tracking refs under `refs/remotes/`. |
| `--all` | List all supported ref namespaces. This is the default when no namespace flag is given. |
| `--format=<format>` | Render simple atoms. Supported atoms: `%(refname)`, `%(refname:short)`, `%(refname:lstrip=N)`, `%(refname:rstrip=N)`, `%(objectname)`, `%(objectname:short)` (7-char), `%(objectname:short=N)`, `%(objecttype)`, `%(objectsize)`, `%(*objectname)`, `%(*objectname:short)`, `%(*objecttype)`, `%(*objectsize)`, `%(HEAD)`, `%(upstream)`, `%(upstream:short)`, `%(push)`, `%(push:short)`, `%(subject)`, `%(contents)`, `%(contents:subject)`, `%(body)`, `%(contents:body)`, `%(authorname)`, `%(authoremail)`, `%(committername)`, `%(committeremail)`, `%(taggername)`, `%(taggeremail)`, `%(authordate)`, `%(committerdate)`, `%(taggerdate)`, `%(creatordate)` (all four also accept a `:<format>` modifier, e.g. `%(committerdate:iso)`), `%(tree)`, `%(tree:short)`, `%(parent)`, `%(parent:short)`, `%(numparent)`, `%(color:<spec>)`, `%(raw)`, `%(raw:size)`, `%(describe[:<opts>])` (runs `git describe` per ref; options `tags`/`abbrev=<n>`/`match=<glob>`/`exclude=<glob>`), `%(symref)`, `%(symref:short)`, `%(symref:lstrip=N)`, `%(symref:rstrip=N)` (the target a symbolic ref such as `refs/remotes/<remote>/HEAD` points to; empty for ordinary refs). |
| `--sort=<key>` | Sort by `refname`, `objectname`, `version:refname` (alias `v:refname`; orders embedded numbers numerically, so `v1.9` precedes `v1.10`), a date key — `committerdate`, `authordate`, or `creatordate` — `objectsize` (the ref object's byte size), or the dereference keys `*objectname` / `*objecttype` / `*objectsize` (an annotated tag's target object id / type / byte size — empty for non-tag refs, so they sort first). Date keys peel annotated tags to the commit; `creatordate` uses an annotated tag's own tagger date. Prefix any key with `-` to reverse. |
| `--count=<n>` | Limit output to at most `n` refs after filtering and sorting. |
| `--points-at=<object>` | Keep refs that point at the object. Annotated tags also match their peeled target. |
| `--contains=<commit>` / `--no-contains=<commit>` | Keep (or exclude) refs whose tip has `<commit>` as an ancestor. |
| `--merged=<commit>` / `--no-merged=<commit>` | Keep (or exclude) refs whose tip is reachable from `<commit>` (already merged into it). |
| `--shell` / `--perl` / `--python` / `--tcl` | Quote each interpolated field as a string literal of the named language so the output can be `eval`-ed/sourced. Mutually exclusive. |
| `--exclude=<pattern>` | Do not list refs matching `<pattern>` (repeatable; applied after the positional include patterns). |
| `<pattern>...` | Keep refs whose full name matches, contains, or ends with the pattern. |

## Examples

```sh
libra for-each-ref
libra for-each-ref --heads
libra for-each-ref --tags --format='%(refname) %(objectname)'
libra for-each-ref --points-at HEAD --format='%(refname) %(objecttype)'
libra for-each-ref --sort=-refname --count=5
libra for-each-ref --format='%(refname:short) %(committerdate:relative)' --sort=-committerdate
libra --json for-each-ref --remotes
```

## Compatibility

Compatibility tier is `partial`. `--contains` / `--no-contains` are supported (filter refs whose tip has, or does not have, the given commit as an ancestor), as are `--merged` / `--no-merged` (filter refs whose tip is, or is not, reachable from the given commit) and `--exclude` (drop refs matching the given pattern, applied after the positional include patterns). Supported sort keys are `refname`, `objectname`, `version:refname`, the date keys `committerdate` / `authordate` / `creatordate`, `objectsize` (the ref object's byte size — also available as the `%(objectsize)` atom), and the dereference keys `*objectname` / `*objecttype` / `*objectsize` (an annotated tag's dereferenced object id / type / byte size — also available as the `%(*objectname)` / `%(*objectname:short)` / `%(*objecttype)` / `%(*objectsize)` atoms; empty for non-tag refs, which sort first), each reversible with a `-` prefix. The output quoting modes `--shell`, `--perl`, `--python`, and `--tcl` (mutually exclusive) are supported: each interpolated field is wrapped as a string literal of that language (the literal text between atoms, and the default `<oid> <refname>` separators, are left unquoted). The `%(align:<width>[,<position>])` … `%(end)` block pads its rendered contents to a column width (`position` is `left` (default), `right`, or `middle`; content wider than the width is not truncated; blocks may nest), and the `%(if[:equals|:notequals])` … `%(then)` … [`%(else)` …] `%(end)` conditional block selects a branch by testing its condition (plain `%(if)` trims whitespace; `equals`/`notequals` compare the raw value; blocks nest, including inside align). The `%(tree)`/`%(tree:short)`/`%(parent)`/`%(parent:short)`/`%(numparent)` commit-graph atoms are also supported. `%(raw)` / `%(raw:size)` emit the raw decompressed object content and its byte size (the size equals `%(objectsize)`; `%(raw)` is rejected with `--shell`/`--python`/`--tcl`, matching Git). `%(raw)` supports text objects (commits, annotated tags); a non-UTF-8 (binary) object is rejected rather than lossily transcoded. `%(describe[:<opts>])` runs `git describe` on each ref's commit, with the `tags`, `abbrev=<n>`, `match=<glob>`, and `exclude=<glob>` options (comma-separated; an unrecognized option is a usage error, validated even when no ref matches); a commit with no reachable tag renders as an empty string. `%(symref)` / `%(symref:short)` / `%(symref:lstrip=N)` / `%(symref:rstrip=N)` give the target a symbolic ref points to (e.g. `refs/remotes/<remote>/HEAD`); they are empty for ordinary refs. `%(worktreepath)` gives the absolute path of the worktree that has the ref checked out (empty otherwise); Libra worktrees share one HEAD, so the checked-out branch is the current HEAD branch and the path is the current worktree (the one the command runs in) — matching git for a single-worktree repo. The supported atom set is comprehensive; the remaining niche atom `%(deltabase)` is not implemented. Git flat-file ref storage parity is intentionally not applicable to Libra.

## Structured Output

`--json` and `--machine` return the standard Libra envelope. `data` is an array of entries with `refname`, `objectname`, and `objecttype` fields, plus an optional `symref` field (the target ref name) present only for symbolic refs such as `refs/remotes/<remote>/HEAD`.
