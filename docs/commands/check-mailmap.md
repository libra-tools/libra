# `libra check-mailmap`

Resolve `Name <email>` contacts through the repository `.mailmap` — a focused
subset of `git check-mailmap`.

## Synopsis

```
libra check-mailmap <contact>...
libra check-mailmap --stdin
```

## Description

For each `Name <email>` contact (given as arguments or, with `--stdin`, one per
line on stdin), `check-mailmap` looks it up in the worktree's `.mailmap` and
prints the canonical `Name <email>`. A contact with no mailmap match is
printed unchanged. Names and emails compare ASCII case-insensitively, and the
same resolver is used by `libra shortlog`.

`.mailmap` lines take the usual Git forms:

```
Proper Name <proper@example.com>
<proper@example.com> <commit@example.com>
Proper Name <proper@example.com> <commit@example.com>
Proper Name <proper@example.com> Commit Name <commit@example.com>
```

A `(name, email)` rule (with a commit name) takes precedence over an
email-only rule for the same email, matching Git. Comments (`#`) and blank lines
are ignored.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<contact>...` | Contacts to resolve, each `Name <email>`. | `libra check-mailmap 'Bob <bob@old>'` |
| `--stdin` | Read contacts (one per line) from stdin. | `… \| libra check-mailmap --stdin` |
| `--json` / `--machine` | Structured output: `{ contacts: [...] }`. | `libra --json check-mailmap 'B <b@x>'` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Resolved contacts were printed. |
| `128` | Not inside a repository, no contacts given, or a contact missing `<email>`. |

## Examples

```bash
echo 'Old Name <old@example.com>' | libra check-mailmap --stdin
libra check-mailmap 'Old Name <old@example.com>' 'Other <other@example.com>'
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Resolve a contact | `libra check-mailmap '<c>'` | `git check-mailmap '<c>'` |
| From stdin | `libra check-mailmap --stdin` | `git check-mailmap --stdin` |

Mailmap sources: the worktree `.mailmap` is read first, then the file named by
`mailmap.file` (a path relative to the worktree root; entries from it override
the worktree file). `mailmap.blob` is not yet honoured, and the resolver is not
yet wired into `log` / `blame` author display — that integration is a documented
follow-up.
