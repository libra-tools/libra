# `libra upgrade`

Check the Ed25519-signed stable release channel for a newer version of Libra
and, after an interactive confirmation, replace the installed binary with it.
This is the explicit half of the auto-upgrade subsystem (see
[auto-upgrade](../auto-upgrade.md)) and the consumer of
`upgrade.mode=manual`: users who turned automatic upgrades off can still pull
new releases on demand, through exactly the same verified pipeline. This is a
Libra-only extension — Git has no equivalent.

## Synopsis

```
libra upgrade [--check | -y|--yes]
```

## Description

`libra upgrade` runs the fully verified upgrade pipeline on demand:

1. Fetches `stable/manifest-v1.json` from the pinned release origin
   (HTTPS-only, no redirects, ≤ 1 MiB).
2. Verifies the Ed25519 signature against the compiled production trust root,
   plus every anti-rollback floor (key generation, control revision, trusted
   time) persisted next to the installed binary.
3. Compares the signed latest version with the running version.
4. If a newer version exists, shows both versions and the download size, and
   asks for confirmation (`[y/N]`, default **No**).
5. On confirmation, downloads the artifact (sha256 and size enforced during
   streaming, ≤ 128 MiB), stages it next to the installed binary, runs a
   pre-install self-check, and commits an atomic install transaction with a
   post-install probe — a failing probe rolls back to the previous binary
   automatically.

The new binary takes effect on the next `libra` command.

The command manages the **installed binary next to the running executable**;
it never reads or writes repository state and works outside a repository.
Only official installs are upgrade-manageable: a dev build (`cargo run`), a
renamed copy, or an install without the official marker is refused with a
pointer to the install script. Declining an offered upgrade still persists the
verified manifest's monotone floors, so control decisions (key rotations,
revocations) are never forgotten.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--check` | Only report whether a newer signed version exists; never install. Exits 0 in every informational state. | `libra upgrade --check` |
| `-y`, `--yes` | Install a newer version without the confirmation prompt (for scripts and non-interactive shells). Conflicts with `--check`. | `libra upgrade --yes` |

With the global `--json` flag the command emits the standard machine
envelope; the payload lives under `data`:

```json
{
  "ok": true,
  "command": "upgrade",
  "data": {"status": "available", "installed": "0.22.9", "latest": "0.22.10"}
}
```

`data.status` is one of `up_to_date`, `available`, `installed`, `declined`,
`paused`, `latest_revoked`, `not_official_install`, `unsupported_platform`. Machine modes (`--json`/`--machine`) and `--quiet`
never prompt: an available upgrade without `--yes` or `--check` is refused
with the exact flags to use, so stdout always stays a clean machine
document.

## Behaviour in edge states

- **Re-verification after confirmation**: the prompt can stay open for any
  amount of time, so after you confirm, the manifest is fetched and verified
  AGAIN before anything is downloaded. A pause, a revocation, or a different
  version published while you were deciding wins — the command installs
  nothing and exits non-zero (`LBR-CONFLICT-002`) with what changed and a
  hint to re-run, so a scripted `--yes` can never sail on as if it
  upgraded.
- **Durable floors at check time**: the moment a manifest is accepted (offer
  or skip), its anti-rollback floors are persisted; a floor-persist failure
  is an error, never silently ignored. Declining an offer needs no extra
  bookkeeping — the floors are already on disk.
- **Bounded network**: each manifest fetch has a 30-second wall-clock
  budget (the local signature/policy decision after it is effectively
  instant) and the artifact download a 300-second one; a stalled or
  trickling server times out instead of hanging the terminal.

- **Publisher pause** (`paused=true` in the signed manifest): reported as an
  emergency stop; nothing is installed.
- **Revoked latest**: if the newest published version revokes itself, the
  command stays on the current version and says so.
- **Non-interactive stdin without `--yes`**: refused with a clear error and
  the exact flags to use — the command never installs on an unconfirmed
  default.
- **Concurrent upgrade**: if another Libra process holds the upgrade lock or
  made progress first, the command reports that nothing was applied and asks
  you to re-run.
- **Failed self-check**: the previous binary is restored automatically and
  the command exits non-zero.

## Examples

```sh
# Interactive: check, show versions, ask, install
libra upgrade

# Only report availability (never installs)
libra upgrade --check

# Non-interactive install (CI, scripts)
libra upgrade --yes

# Machine-readable status
libra --json upgrade --check
```

## Related

- [auto-upgrade](../auto-upgrade.md) — the automatic background check
  (`upgrade.mode=auto`), the trust model, and the signed-manifest contract.
- `libra config set --global upgrade.mode <auto|manual|off>` — pick between
  automatic upgrades, manual-only (`libra upgrade`), or fully off.
