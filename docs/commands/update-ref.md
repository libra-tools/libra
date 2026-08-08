# `libra update-ref`

Safely update, create, or delete a branch ref with an optional
compare-and-swap — a focused subset of `git update-ref`. The ref read, the ref
write/delete, and the reflog entry all happen inside a single SQLite
transaction, so a failed compare-and-swap rolls everything back atomically.

## Synopsis

```
libra update-ref [-m <reason>] refs/heads/<branch> <newvalue> [<oldvalue>]
libra update-ref -d [-m <reason>] refs/heads/<branch> [<oldvalue>]
```

## Description

`update-ref` points `refs/heads/<branch>` at `<newvalue>` (creating the ref if
it does not exist), or deletes it with `-d`. The optional `<oldvalue>` is a
**compare-and-swap** guard:

- a full object id — the ref must currently point there, or the command fails;
- `0000…0000` (the all-zero id) — the ref must **not** already exist
  (create-only).

When `<oldvalue>` is omitted, the ref is created or overwritten unconditionally.

**Scope (v1):** only `refs/heads/<branch>` is supported — the branch-tip case
Libra's SQLite `reference` table models directly. `HEAD`, `refs/tags/*`,
`refs/remotes/*`, and arbitrary ref namespaces are rejected; use
[`symbolic-ref`](symbolic-ref.md) / [`switch`](switch.md) for `HEAD` and
[`tag`](tag.md) for tags.

Every successful update writes an `update-ref` reflog entry for the ref. The
`<oldvalue>` you pass for the compare-and-swap is **never** recorded in the
reflog message; only the actual before/after object ids are.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `-d`, `--delete` | Delete the ref instead of updating it. | `libra update-ref -d refs/heads/old` |
| `-m <reason>` | Reflog reason recorded with the update. | `libra update-ref -m "reset tip" refs/heads/main <oid>` |
| `<newvalue>` | The new commit, as an object id or any revision expression (omit with `-d`). | `libra update-ref refs/heads/main HEAD~1` |
| `<oldvalue>` | Expected current value for a compare-and-swap, as an id or revision (`0{40}` = must not exist). | `libra update-ref -d refs/heads/topic HEAD` |
| `--json` / `--machine` | Structured output: `{ ref, old, new, deleted }`. | `libra --json update-ref refs/heads/main <oid>` |

A symbolic value (`ref:refs/heads/…`) and the null object id as `<newvalue>` are
rejected — use `symbolic-ref`, or `-d` to delete.

### Revision expressions in `<newvalue>`

`<newvalue>` goes through the same revision resolver as the rest of Libra, so
branch names, tags, `HEAD`, parent/ancestor navigation (`HEAD^`, `HEAD~2`) and
abbreviated object ids all work.

There is **no implicit peel**: whatever the expression names must itself be a
commit. A lightweight tag is the commit id, so it is accepted; a bare annotated
tag names a tag object and is refused with `LBR-CLI-003`, naming the type that
was resolved. Peel it explicitly to use it:

```bash
libra update-ref refs/heads/release v1.0^{commit}
```

This matches Git, which likewise refuses to write an annotated tag object into
`refs/heads/*`. A revision that does not resolve is `LBR-CLI-003`; the two
syntax-layer refusals above stay `LBR-CLI-002`. Both exit `128`. A failure
inside the object store (an unreadable or corrupt object) keeps its own
repository/IO code and is never reported as bad input.

`<oldvalue>` accepts the same expressions, with one deliberate difference: it
is **not** type-checked. It states what the ref points at *right now*, so the
resolved id is compared verbatim — naming an annotated tag there is an ordinary
compare-and-swap mismatch, not a "not a commit" refusal, which is also how Git
reports it. The all-zero id keeps its "must not exist" meaning.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The ref was updated, created, or deleted. |
| `128` | Not inside a repository, an unsupported/invalid ref, an unresolvable `<newvalue>` revision or one that does not name a commit, an invalid object id, a compare-and-swap mismatch, or deleting a ref that does not exist. |

## Examples

```bash
# Point a branch at a specific commit
libra update-ref refs/heads/main <oid>

# Compare-and-swap: only move main if it is still at <oldoid>
libra update-ref refs/heads/main <newoid> <oldoid>

# Create a branch only if it does not already exist
libra update-ref refs/heads/topic <oid> 0000000000000000000000000000000000000000

# Delete a branch ref, optionally guarded by its current value
libra update-ref -d refs/heads/old
libra update-ref -d refs/heads/old <oldoid>
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Update a branch ref | `libra update-ref refs/heads/b <oid>` | `git update-ref refs/heads/b <oid>` |
| Compare-and-swap | `libra update-ref refs/heads/b <new> <old>` | `git update-ref refs/heads/b <new> <old>` |
| Delete a ref | `libra update-ref -d refs/heads/b` | `git update-ref -d refs/heads/b` |

Deferred (not exposed): non-`refs/heads/*` namespaces, `HEAD`, `--stdin` batch
updates, `--create-reflog`, and `--no-deref`.
