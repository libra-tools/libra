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

**Deliberate limitation (this release):** the command ignores every identity and
date environment variable (`GIT_COMMITTER_NAME`, `GIT_AUTHOR_NAME`,
`GIT_COMMITTER_EMAIL`, `GIT_AUTHOR_EMAIL`, `EMAIL`, `LIBRA_COMMITTER_*`,
`GIT_COMMITTER_DATE`, `GIT_AUTHOR_DATE`). It always reports the configured
identity with "now". The author and committer sides are rewired to the commit
signature resolvers by the follow-up cards.

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
