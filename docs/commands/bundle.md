# `libra bundle`

Create, verify, inspect, and unpack Git v2 bundle files. A bundle is a text
header followed by a pack and can be consumed by system Git or Libra.

## Synopsis

```text
libra bundle create <file> [--all] [--branches] [--tags] [<rev>...]
libra bundle verify <file>
libra bundle list-heads <file>
libra bundle unbundle <file>
```

## Description

- `create` writes a full, non-thin bundle. Explicit revisions may be combined
  with `--all`, `--branches`, or `--tags`; at least one selector is required.
  `--all` and an explicit `HEAD` revision advertise a `HEAD` line (detached
  `HEAD` points at the detached commit; attached `HEAD` stays `HEAD`, not
  `refs/heads/<branch>`). Annotated tag heads retain the tag-object OID and the
  pack includes tag target closure. Output uses a private temporary file, syncs
  it, then renames it into place.
- `verify` validates the v2 header, local prerequisites, pack version, and the
  complete pack checksum.
- `list-heads` prints the advertised `<oid> <ref>` lines without importing.
- `unbundle` validates prerequisites and checksum, builds the correct SHA-1 or
  SHA-256 pack index, and installs the pack/index pair in the object store. It
  prints the advertised heads but deliberately does **not** update refs, matching
  `git bundle unbundle`. Repeated imports verify the installed pair before
  reporting success.

Bundle input, collected raw object data, and final output are each capped at
1 GiB. This also bounds memory before pack compression, so a highly compressible
object graph whose raw data exceeds the cap is rejected. Bundle creation is
full-history only; prerequisite/thin/incremental range creation remains deferred.

## Options

| Option | Description |
|---|---|
| `<rev>...` | Include explicit revisions as advertised heads. `HEAD` is advertised as `HEAD`. |
| `--all` | Include all local branches and tags, plus a `HEAD` line. |
| `--branches` | Include all local branches. |
| `--tags` | Include all local tags, preserving annotated objects. |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | `verify`/`list-heads` found an unreadable or invalid bundle or missing prerequisite. |
| `128` | Repository/usage/write failure, or `unbundle` validation/index/install failure. |

## Examples

```bash
libra bundle create repository.bundle --all
libra bundle create releases.bundle --tags main
libra bundle verify repository.bundle
libra bundle list-heads repository.bundle

# Import objects, inspect printed heads, then update only the refs you want
libra bundle unbundle repository.bundle
libra update-ref refs/heads/restored <printed-commit-oid>

# System Git can consume Libra's bundle directly
git clone repository.bundle restored
```

## Comparison with Git

| Task | Libra | Git |
|---|---|---|
| Create | `libra bundle create <f> --all` | `git bundle create <f> --all` |
| Verify | `libra bundle verify <f>` | `git bundle verify <f>` |
| List heads | `libra bundle list-heads <f>` | `git bundle list-heads <f>` |
| Import objects | `libra bundle unbundle <f>` | `git bundle unbundle <f>` |

Deferred surfaces are prerequisite/thin/incremental bundle creation.
`libra clone <bundle>` reads a Git v2 bundle (directory first, then
`<path>.bundle`, then `<path>`). `verify` checks checksum integrity but does
not build a temporary index to exhaustively decode every pack entry.
