# `libra var`

Print Libra's identity variables (`GIT_AUTHOR_IDENT`, `GIT_COMMITTER_IDENT`).

## Synopsis

```bash
libra var <NAME>
libra var -l
libra var --json <NAME>
```

## Description

`libra var` is a small Git-compatible plumbing command that prints the identity
Libra would put in a commit signature, in Git's ident form:

```text
Name <email> <unix-timestamp> <±hhmm>
```

The identity comes from `user.name` and `user.email` (local config first, then
global), and the timestamp/timezone come from the current local time, so the
`TZ` environment variable determines the printed offset.

`GIT_AUTHOR_IDENT` resolves exactly what `libra commit` would write as the
author: the name follows `GIT_AUTHOR_NAME` > `GIT_COMMITTER_NAME` >
`LIBRA_COMMITTER_NAME` > `user.name`, the email follows `GIT_AUTHOR_EMAIL` >
`GIT_COMMITTER_EMAIL` > `EMAIL` > `LIBRA_COMMITTER_EMAIL` > `user.email`, and
`GIT_AUTHOR_DATE` is honored (an invalid value is `LBR-CLI-002`). With
`user.useConfigOnly` set, the identity environment variables are ignored while
the date still applies. Only the author-side date is validated, so a broken
`GIT_COMMITTER_DATE` does not affect this variable.

`GIT_COMMITTER_IDENT` mirrors the committer side: name `GIT_COMMITTER_NAME` >
`GIT_AUTHOR_NAME` > `LIBRA_COMMITTER_NAME` > `user.name`, email
`GIT_COMMITTER_EMAIL` > `GIT_AUTHOR_EMAIL` > `EMAIL` > `LIBRA_COMMITTER_EMAIL` >
`user.email`, with `GIT_COMMITTER_DATE` honored and validated. Only the
committer-side date is checked, so a broken `GIT_AUTHOR_DATE` does not affect it.

Both variables therefore report exactly what `libra commit` would write; the
cross-role fallbacks and the "environment before config" order are Libra's
documented, intentional differences from Git.

`-l` lists the supported variables as `NAME=value` lines in the order
`GIT_COMMITTER_IDENT`, `GIT_AUTHOR_IDENT`, skipping variables whose identity
cannot be resolved (Git's behaviour for `var -l`). Unknown names, a missing name
and extra arguments are `LBR-CLI-002`; a missing identity is `LBR-AUTH-001`; the
command needs a repository (`LBR-REPO-001` outside one, an intentional
difference from Git).

## Options

| Option | Description |
|--------|-------------|
| `-l`, `--list` | List every supported variable as `NAME=value` |
| `--json` | Global flag: emit a `{"command":"var","data":…,"ok":true}` envelope |
| `<NAME>` | `GIT_COMMITTER_IDENT` or `GIT_AUTHOR_IDENT` |

## Examples

```bash
libra var GIT_COMMITTER_IDENT
# C O Mitter <c@d> 1795494061 +0800

libra var -l
# GIT_COMMITTER_IDENT=C O Mitter <c@d> 1795494061 +0800
# GIT_AUTHOR_IDENT=C O Mitter <c@d> 1795494061 +0800

libra --json var GIT_AUTHOR_IDENT
# {"command":"var","data":{"name":"GIT_AUTHOR_IDENT","value":"C O Mitter <c@d> 1795494061 +0800"},"ok":true}
```
