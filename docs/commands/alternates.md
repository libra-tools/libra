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
libra alternates prune --dry-run                     # show borrowers whose repo is gone
libra alternates prune                               # retire them (unblocks this store's gc)
```

## Retiring a borrower whose repository is gone

Registering an alternate records this repository as a BORROWER in the base, and
while that registration exists the base's `gc`, `repack -d`, `cache evict`,
`agent clean` and `file obliterate` all refuse to delete objects.

The only registration retired automatically is one whose path exists and is
not a directory — an object directory never is. Absence is deliberately NOT
enough: an absent borrower path is indistinguishable from an unmounted one, and
guessing wrong deletes objects a borrower still needs the moment its mount
comes back.
Run `libra alternates prune` IN THE BASE to retire registrations whose path does
not exist — the flag `--dry-run` lists them first. When a registration cannot be
checked at all (an unreachable mount, a permission-denied parent), name it:
`libra alternates prune /path/to/gone/.libra/objects` retires that one exact
registration whatever the filesystem says, because there the user is the only
available evidence. The path is matched verbatim against the registration, so a
symlink pointing at a different borrower cannot retire it by accident. The
normal route is still `libra alternates remove` from the borrower, which
unregisters both sides.

## Deferred (not v1)

`git clone --reference`/`--shared` copy-avoidance (needs fetch have-negotiation
against the alternate — the flags stay accepted no-ops for now); `--dissociate`
(copy borrowed objects in + break the link); the 2.11 default shared-store.
