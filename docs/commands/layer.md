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

**Per-worktree since W1** (plan-20260714 §C.4.1.1, migration `2026072303`):
the registry and path ownership are scoped by `worktree_id` — the same layer
name (and the same destination path) can exist independently in different
worktrees, every subcommand acts only on the current worktree's rows, and
removing a worktree purges its layer rows only when its directory is deleted
too (a retained directory keeps them, so its overlay files stay
un-stageable). Legacy repository-global rows are
adopted by the main worktree only when no linked worktree exists; otherwise
the schema migration fails closed and asks for an explicit
`layer unapply`/`layer remove` from the owning worktree first.

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
