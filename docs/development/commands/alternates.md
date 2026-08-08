# libra alternates

`libra alternates` manages **object alternates** (lore.md 2.3): borrow objects
from a shared/parent object store instead of copying them. A Libra extension
(git has no `alternates` command — you edit `objects/info/alternates` by hand).

## Compatibility

- Level: `intentionally-different`.

## Design

The single-owner `internal::alternates` module reads/writes two git-standard
files under `objects/info/`:

- `alternates` — object dirs this store borrows FROM. The read-resolver
  consults the transitive chain (cycle-safe, depth-capped) on a LOCAL miss;
  every borrowed hit is full-byte OID-verified before it is returned.
- `borrowers` — object dirs that borrow FROM this store (a Libra extension).

`exist` consults alternates, so a borrowed-but-present object is never treated
as missing.

### Deletion safety (airtight)

Registering a base ALSO records this repo as a borrower of it. While any live
borrower exists, the base's `gc` and `cache evict` **refuse to prune loose
objects** — a shared base can never delete an object a borrower still needs.
`file obliterate` refuses a borrow-only object (it never reaches into a
parent's store); `fsck` reports a dangling alternate as an actionable error.

### Guards

`add` refuses a self-reference, a base with a different `core.objectformat`
(never borrow across hash kinds), and a TIERED (s3/r2) base (a local alternate
cannot reach the base's remote tier).

## Examples

```bash
libra alternates add /path/to/base/.libra/objects   # borrow from a shared store
libra alternates list
libra alternates remove /path/to/base/.libra/objects # stop borrowing
libra alternates prune --dry-run                     # list borrowers whose repo is gone
libra alternates prune                               # retire them (unblocks this store's gc)
libra alternates prune /gone/.libra/objects          # retire one that cannot be checked at all
```

`prune` runs in the BASE. It exists because absence is not proof: an
automounted or temporarily unavailable borrower answers ENOENT for a path that
returns the moment it is mounted, so nothing retires a registration on that
evidence alone. The only automatic cleanup is a registration whose path exists
and is not a directory (an object directory never is).

## Deferred (not v1)

`git clone --reference`/`--shared` copy-avoidance (needs fetch have-negotiation
against the alternate — the flags stay accepted no-ops for now); `--dissociate`
(copy borrowed objects in + break the link); the 2.11 default shared-store.
