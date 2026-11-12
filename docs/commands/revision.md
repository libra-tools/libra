# `libra revision`

Revision ordinal index (a Libra extension, lore.md §1.16, porting Lore's
`revision find number`). Git has no equivalent surface.

## Synopsis

```
libra revision find --number <N> [--ref <branch>]
libra revision number <commitish> [--ref <branch>]
libra revision index [--ref <branch>] [--rebuild]
libra revision import --manifest <file>
```

## Description

Each branch's **first-parent chain** gets a monotonic 1-based numbering
(1 = root, N = tip), stored in a rebuildable SQLite side table. The numbering
is a pure function of the tip (deterministic across rebuilds and machines).
Commits reachable only through merged-in side branches have **no** ordinal —
the reverse lookup says so explicitly rather than inventing a number.

Freshness is re-validated on **every** read, in the same transaction as the
lookup: fast-forwards append (existing ordinals never change); history
rewrites (rebase/amend/reset) and `refs/replace` changes trigger a full
deterministic rebuild. A stale index never answers. The first query on a
long branch walks its whole chain once (O(chain) object loads, possibly
remote under tiered storage); later queries are index hits.

| Subcommand | Purpose |
|---|---|
| `find --number <N>` | Print the OID of revision #N (Lore's `revision find number`). Out of range → exit 1 naming the chain length; `N < 1` → 129. |
| `number <commitish>` | Reverse lookup: the ordinal of a commit on the ref's chain. Not on the chain → exit 1 with the coverage note. |
| `index [--rebuild]` | Freshness report (tip, count, built-at); `--rebuild` forces a deterministic rebuild and prunes index rows for deleted branches. |

`--ref <branch>` targets any local branch; the default is the current branch
(detached HEAD → an error suggesting `--ref`). `--json` emits structured
output. `find --metadata` (search by 1.10 revision metadata) is a documented
follow-up — the ordinal index provides the deterministic iteration order such
a scan would walk.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `1` | Miss (ordinal out of range; commit not on the chain). |
| `128` | Fatal (not a repository; detached HEAD without `--ref`). |
| `129` | Usage (`--number` < 1). |

## Examples

```bash
libra revision find -n 1                 # the root revision
libra revision number HEAD               # how long is the mainline?
libra revision find -n 42 --ref main     # the 42nd revision of main
libra revision index --rebuild           # deterministic rebuild + prune
libra --json revision number HEAD        # structured output
```

## Comparison with Git

Nearest Git analogues: `git rev-list --first-parent --count <oid>` (reverse
direction) and `<tip>~<k>` suffix arithmetic (forward). Classified
`intentionally-different` in [`COMPATIBILITY.md`](../../COMPATIBILITY.md).

## Offline history import

`revision import --manifest <file>` records immutable Git commits as durable
change facts for the current repository and frozen HEAD. Ordinary clone and
`revision index` do not perform this import. `memory rebuild` can then derive
commit Episodes from the imported changes.

The version 1 JSON manifest contains `schema_version`, `repo_id`,
`object_format`, `base_commit`, `visibility_policy`, `cutoff_ms`, `commit_oids`,
`commit_inventory_sha256`, `objects`, `object_inventory_sha256`, `shallow_cuts`,
and `sources`. Unknown fields at every level are rejected. Each object contains
`oid`, `kind`, and `size`; each source contains `commit_oid` and
`available_at_ms`; each shallow cut contains `child` and `parent`. The commit
digest hashes sorted `OID\n` lines; the object digest hashes sorted
`OID TYPE SIZE\n` lines using SHA-256 and UTF-8. OIDs use full lowercase hex.

The importer checks repo identity, SHA-1/SHA-256 format, current HEAD, canonical
object hashes, complete finite ancestor membership and tree/blob closure.
Replacement refs are refused. Declared shallow parents must be absent locally;
unknown missing parents and unreachable/future objects in the inventory fail.
Reads use existing local objects only, without alternates, cloud fallback or
network fetch. Limits are 2 MiB per manifest, 5,000 commits, 20,000 objects,
64 MiB of object bodies, 16 MiB per object (1 MiB per commit), and 128 sources.

`strict_time` requires a cutoff and rejects a base or ancestor newer than it.
`base_snapshot` explicitly permits a retrospective base snapshot; it does not
certify historical task eligibility. A source with an availability timestamp
after a supplied cutoff is always rejected. Publication/merge availability
requires independently audited evidence; Git timestamps do not establish it.
Retain that evidence and the exact manifest with the import receipt.

An existing visible change mapping is reused; a legacy commit receives the
existing object-format-separated synthetic Change ID. No patch-similarity
identity or external sidecar identity is inferred. All new facts and projection
invalidation publish in one transaction through the change revision writer.
Failure preserves prior facts and projection rows; a failed operation audit
record may remain. Repeating the same manifest reuses identities/revisions and
does not invalidate an unchanged projection. Facts already outside the new
base view are refused: use a fresh base-specific repository. With a cutoff,
`sources` must list every already-stored revision admitted to that view; omitted
facts are refused so a narrower cutoff cannot inherit a previous loose view.
Use separate repositories for each base/cutoff/policy.

The current operation records the manifest SHA-256 and the import time. An
import operation succeeding does not establish a historical repair/test
outcome: imported Episodes have `outcome: unknown`. Import never manufactures
historical agent sessions or historical successful operations. A successful
new batch invalidates an existing Memory generation; run `memory rebuild`
before reading it. Current Memory path recall, large-window selection,
revocation counters and consumer limitations remain separate.

`--json` returns the manifest digest, base, imported/reused counts, the finite
history flag, and explicit commit-to-change mappings. Rejection or transaction
failure exits 128 with `LBR-IO-002`. Terminal control sequences in rejected
manifest field names are removed from the diagnostic.
