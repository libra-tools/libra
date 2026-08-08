# libra sparse-view

`libra sparse-view` manages a **read-only sparse VIEW filter** (lore.md 2.2) —
the non-declined complement of git sparse-checkout. It is a Libra extension,
deliberately NOT named `sparse-checkout`: it NEVER touches the working tree.

## Compatibility

- Level: `intentionally-different`.
- The MATERIALIZING forms — the top-level `sparse-checkout` command and
  `clone --sparse` — remain declined (D10). `mv --sparse` / `rm --sparse` stay
  accepted no-ops (skip-worktree cone membership, still unimplemented).

## Design

An allowlist of gitignore-syntax include patterns scopes what the read/query
commands DISPLAY:

- `ls-files` — lists only in-view tracked/other entries (unmerged entries are
  always shown).
- `diff` — the WORKING-TREE diff (unstaged) is scoped to the view.

It is strictly read-only and commit-safe:

- The working tree is never modified; no skip-worktree bits are written.
- `status` content is NEVER filtered — it stays honest about what `commit`
  will record (only a one-line advisory notes the view is active).
- `diff --staged` (commit-authoritative) and `diff A..B` (rev-vs-rev) are
  NEVER filtered.

Pattern semantics are an ALLOWLIST: the last matching pattern wins, a `!pat`
carves a hole back out even under a broader include, and a path matched by no
pattern is out-of-view (default-exclude). There is no ancestor-dominance
short-circuit (which would defeat `!child` negations). A disabled or empty
view is a no-op (output is byte-identical to no view configured).

State: patterns in the `sparse_view` SQLite table (owner `internal::sparse`);
the toggle in the per-worktree `sparse_view_meta` projection (W1 §C.4.1.1,
migration `2026072304` — it retired the scope-less config_kv `sparse.enabled`
key).

**Per-worktree since W1**: patterns and the enabled toggle are per-worktree
facts — every subcommand and the `ls-files`/`diff`/`hydrate` gates act on the
current worktree's own view, and removing/pruning a worktree GCs its rows
once the directory is gone. Legacy repository-global state adopts to the
main worktree only when no linked worktree exists; otherwise the schema
migration fails closed and asks for an explicit `sparse-view clear` (or
re-creation in the intended worktree) first — patterns are never copied to
every worktree.

## Examples

```bash
libra sparse-view set 'src/**' 'docs/**'   # scope ls-files/diff to these paths
libra sparse-view add '!src/gen/**'        # carve a hole out of the view
libra sparse-view list
libra sparse-view status                   # enabled state + pattern count
libra sparse-view disable                  # off (patterns kept)
libra sparse-view clear                    # drop all patterns and disable
```

## Deferred (not v1)

Cone mode (auto-including parent dirs + full subtrees); any materialization
(that is the declined D10 sparse-checkout).
