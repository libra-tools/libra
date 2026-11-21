# `libra gc`

Clean up repository objects and refs, like `git gc`.

## Synopsis

```bash
libra gc [--prune=<date>] [--no-prune] [--aggressive] [--auto]
```

## Description

`libra gc` runs the same maintenance steps Git's `gc` performs, in order, by
reusing the existing maintenance primitives rather than re-implementing a
reachability traversal:

1. **`pack-refs`** — collapse loose refs into `packed-refs`;
2. **reflog expiry** — expire reflog entries past the default
   `gc.reflogExpire` / `gc.reflogExpireUnreachable` policy (90/30-day defaults);
3. **`repack -a -d`** — pack all reachable objects into a fresh pack and drop
   the now-packed loose copies;
4. **unreachable loose-object pruning** — delete unreachable loose objects
   past the prune expiry, using the shared maintenance reachability inventory
   (refs, reflogs, every index stage, stash/autostash sidecars, sequencer
   state) and the alternates borrow protection;
5. **`worktree prune`** — retire worktree registry entries whose path
   vanished.

Reachable objects are never deleted. The deletion steps refuse while another
repository borrows this object store through alternates (a borrowed object is
reachable to the borrower, not to this repository's walk), and re-verify
reachability under an exclusive maintenance hold immediately before unlinking.

The human summary prints one `gc <step>:` line per executed step;
`--json`/`--machine` emit a `gc` envelope whose `data.steps[]` summarizes each
step (task, success, and per-step counters such as `objects_removed`,
`refs_packed`, and `reflog_entries_pruned`).

## Options

`--auto` runs only when maintenance is worthwhile: `gc.auto <= 0` never triggers;
otherwise a run happens when the number of packs without a `.keep` file exceeds
`gc.autoPackLimit` (default 50; a non-positive value disables that trigger) or the
loose object count reaches `gc.auto` (default 6700). A triggered run prints
`Auto packing the repository for optimum performance.` on stderr. The decision is
shared with the automatic packing that `fetch` performs.

### `--prune <DATE>`

Prune unreachable loose objects older than `<DATE>`. Defaults to the
`gc.pruneExpire` config value, falling back to `2.weeks.ago`. Accepts
`now` (delete immediately), `never` (disable pruning), bare integers (days),
Git's dotted approxidate forms (`N.seconds.ago`, `N.minutes.ago`,
`N.hours.ago`, `N.days.ago`, `N.weeks.ago`, `N.months.ago`, `N.years.ago`),
and the shared date parser's absolute forms (Unix timestamps, RFC3339,
`YYYY-MM-DD`). An invalid value fails with `LBR-CLI-002` / exit 129 before
any step writes.

```bash
libra gc --prune=now
libra gc --prune=3.days.ago
```

### `--no-prune`

Do not prune any unreachable loose objects; the other steps still run.

```bash
libra gc --no-prune
```

### `--aggressive`

Spend more time optimizing the pack (a larger delta-compression window). The
enlarged window (`gc.aggressiveWindow`/`gc.aggressiveDepth`, Git's 250/50
defaults) lands with `repack --window`/`--depth` (MX-09); until then the flag
is accepted and reported in the `--json` envelope (`data.aggressive`) without
changing pack parameters.

```bash
libra gc --aggressive
```

### `--auto`

Only run when the loose-object count is at least `gc.auto` (default 6700).
Below the threshold the command exits 0 with **zero writes** and reports
`Auto gc skipped`. A non-positive `gc.auto` disables auto-gc entirely,
matching Git's `too_many_loose_objects`.

```bash
libra gc --auto
libra config set gc.auto 2000 && libra gc --auto
```

## Examples

```bash
libra gc                              # default policy: prune past 2.weeks.ago
libra gc --prune=now                  # delete unreachable loose objects immediately
libra gc --no-prune                   # clean up without deleting any object
libra gc --auto                       # no-op below gc.auto (6700)
libra gc --aggressive                 # optimize the pack (reported; window lands with MX-09)
libra gc --quiet                      # suppress the step summary
libra --json gc                       # structured per-step summary
```

## Human Output

```text
gc pack-refs: packed 2 refs into packed-refs
gc reflog-expire: expired 0 reflog entries across 4 ref(s)
gc repack: repacked all reachable objects and removed the now-packed loose copies
gc prune: removed 1 unreachable loose objects
gc worktree-prune: pruned worktree registry entries whose paths vanished
```

`--quiet` suppresses the summary while keeping warnings and errors on stderr.

## Structured Output

```json
{
  "ok": true,
  "command": "gc",
  "data": {
    "auto": false,
    "auto_skipped": false,
    "aggressive": false,
    "prune": "2.weeks.ago",
    "steps": [
      { "task": "pack-refs", "success": true, "refs_packed": 2, "message": "..." },
      { "task": "reflog-expire", "success": true, "reflog_entries_pruned": 0, "message": "..." },
      { "task": "repack", "success": true, "message": "..." },
      { "task": "prune", "success": true, "objects_removed": 1, "message": "..." },
      { "task": "worktree-prune", "success": true, "message": "..." }
    ]
  }
}
```

A skipped `--auto` run reports `auto_skipped: true` with an empty `steps[]`.

## Compatibility

| Feature | Libra | Git |
|---------|-------|-----|
| Keep reachable objects | Supported (shared maintenance reachability inventory) | Supported |
| pack-refs + reflog expire + worktree prune | Supported (same order as `git gc`) | Supported |
| Repack reachable loose objects (`repack -a -d`) | Supported (single undeltified pack; redundant-*pack* removal lands with MX-08) | Supported |
| Prune old unreachable loose objects | `--prune=<date>` / `--no-prune` (default `gc.pruneExpire` = `2.weeks.ago`) | `--prune=<date>` / `--no-prune` |
| Auto mode | `--auto` (zero writes below `gc.auto` = 6700; `gc.auto <= 0` disables) | `--auto` |
| Aggressive repack | Accepted and reported; enlarged window lands with MX-09 | Larger delta window |
| Alternates borrow protection | Deletion refuses while a live borrower exists (`LBR-CONFLICT-002`) | N/A (Libra extension) |
| Cruft packs / commit-graph / bitmaps | Unsupported | Supported |
| JSON output | `--json` / `--machine` step summary | N/A |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Not inside a Libra repository | `LBR-REPO-001` | 128 |
| Invalid prune date | `LBR-CLI-002` | 129 |
| `--prune` combined with `--no-prune` | `LBR-CLI-002` | 129 |
| A live repository borrows this object store (alternates) | `LBR-CONFLICT-002` | 128 |
| Object storage cannot be read | `LBR-IO-001` | 128 |
| Object deletion fails | `LBR-IO-002` (message-inferred) | 128 |
