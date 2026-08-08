# libra layer

`libra layer` implements Lore's **local-overlay primitive** (lore.md 2.4): a
named, purely-local overlay of files materialized onto the working tree on
explicit command that **never enters a commit**. It is the Phase-2 landable
half of the §3.5 composition pair (its versioned sibling `link` is deferred to
the §3.4 RFC); the §3.5 red line forbids a *default* auto-compose model, not
this opt-in, explicit-command overlay.

## Compatibility

- Level: `intentionally-different` — a Libra-only extension with no Git
  equivalent (Appendix A `无直接等价`).

## Design

A layer is `(name, source local dir, priority, enabled)`. State lives in two
SQLite side-tables (`layer`, `layer_path`) owned solely by
`internal::layer::LayerStore` — never serialized into any object. Two
invariants:

1. **Never-enters-commit** — enforced at two chokepoints: materialized paths
   are un-negatably excluded from the ignore engine (`status`/`add .` skip
   them), AND the `add` staging path hard-refuses any layer-owned path even
   under `--force` (which bypasses ignore). Staging one is `LBR-LAYER-001`.
2. **Never-clobbers** — a destination that collides with a tracked (index or
   HEAD) path is refused at `apply` time (`LBR-LAYER-001`, fail-closed);
   `unapply`/`remove` skip user-edited overlay files (content-hash mismatch).

Precedence on a same-destination collision between two enabled layers:
higher `priority` wins, ties broken by name (last-writer-wins in stack order).

**Worktree scoping (W1, plan-20260714 §C.4.1.1, migration `2026072303`)**:
`layer` is keyed by `(worktree_id, name)` and `layer_path` by
`(worktree_id, path)`. Every `LayerStore` method, `apply`/`unapply`, and the
`add` staging guard take the request's ONE resolved `WorktreeScope`
explicitly. Scope↔workdir binding: `add` derives its scope from the CAPTURED
working directory (`WorktreeScope::for_workdir`), so the hard staging guard
always checks the ownership rows of the very tree it stages into — immune to
a concurrent process-cwd switch; the mutation entry points `apply`/`unapply`
re-derive the scope from their resolved workdir and FAIL CLOSED on a
mismatch (`verify_scope_matches_workdir`). The sync ignore-exclusion
snapshot stores `(scope_key, set)` and stays advisory — a stale consult can
only misclassify `status`/`clean` cosmetically; the commit invariant is held
by the DB-scoped staging guard, and NO destructive gate may consult the
snapshot (the `worktree remove --delete-dir` dirty check reads the target
scope's overlay set straight from the DB, fail-closed, and subtracts it
explicitly from the unstaged side only). `worktree remove` purges the
removed scope's layer rows ONLY when the directory is actually gone
(`--delete-dir`, or already vanished), and `worktree prune` GCs each pruned
scope's rows the same way: a RETAINED directory still operates as a
repository, so its ownership rows survive to keep the still-materialized
overlay files un-stageable (it cannot be re-registered while non-empty —
`worktree add` refuses — so the rows guard it until the directory is
cleared; orphaned rows are reclaimed by the W3 worktree doctor). Legacy
migration is FAIL-CLOSED by a CHECK guard: rows adopt to main (`''`) only
when no linked HEAD row exists in `reference`; otherwise the migration
refuses and asks for explicit `layer unapply`/`layer remove` from the owning
worktree (layer ownership is not rebuildable — clearing it would make
overlay files committable). The down migration equally fails closed while
any linked-scope row exists.

## Examples

```bash
libra layer add scratch --source ./overlays/scratch   # register a local overlay
libra layer add ci --source ./ci --priority 10        # higher priority wins collisions
libra layer list                                      # show registered layers
libra layer apply                                     # materialize enabled overlays
libra layer status                                    # show materialized paths
libra layer unapply --layer scratch                   # remove one layer's files (keep edits)
libra layer remove scratch                            # unregister (unapplies first)
```

## Deferred (not in v1)

Auto-materialization on checkout/switch/merge/clone (the §4.1 bypass surface —
v1 is explicit-command only); versioned composition (`link`/subtree,
§3.4-RFC-gated); remote/object-DB sources; overriding a tracked path (refused,
never silently shadowed).
