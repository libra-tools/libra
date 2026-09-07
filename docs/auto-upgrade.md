# Auto-Upgrade

Libra can keep an official script install up to date automatically. This is
**opt-in and off by default**, and it is designed to be safe: every upgrade is
cryptographically verified, anti-rollback protected, crash-safe, and isolated
so it can never break or change the outcome of your normal commands.

> **Status.** Releases since `v0.22.1` carry the production signing trust
> root (releases up to `v0.22.0` remain inert without it), but auto-upgrade
> still installs nothing until a valid, signed stable manifest is published;
> a missing or invalid manifest fails closed. The persisted monotonic
> key-generation floor described below ships with the A1-01 release
> (`0.22.6` on the merged baseline) — clients released before it enforce
> only the manifest and compiled floors.
> The install scripts (`install.sh` / `install.ps1`) verify the SAME signed
> stable manifest on their default path before downloading anything; see
> `docs/installation.md` for the bootstrap trust model.

## Enabling it

```bash
# Only for official script installs (curl … | sh). One of: auto | manual | off.
libra config set --global upgrade.mode auto
libra config get --global upgrade.mode        # -> auto
libra config unset --global upgrade.mode      # resets to off, keeps the file
```

`upgrade.mode` is a **reserved config namespace**: it is stored in
`{LIBRA_HOME}/upgrade/settings.json` (default `~/.libra/upgrade/settings.json`),
never in the SQLite config databases. Only single-value `set`/`get`/`unset`
with `--global` are accepted; every other spelling (local/system scope,
multivalue, type conversion, sections) fails closed. See
[`docs/commands/config.md`](commands/config.md#reserved-upgrade-namespace).

## What `auto` does

When `upgrade.mode=auto` on an official install, each normal command also:

1. **Throttles.** A successful online check sets a ~15-minute cross-process
   cooldown (plus small jitter), so at most one network check happens per
   cooldown window; failures back off up to one hour.
2. **Fetches and verifies** a signed release manifest from
   `https://download.libra.tools`. The manifest is Ed25519-signed against a
   trust table compiled into your binary; the HTTPS transport pins the host,
   refuses redirects and plain HTTP, and bounds the response size.
3. **Decides.** It installs only a strictly newer, non-revoked, non-paused
   release for your exact platform, and only if anti-rollback state allows it
   (a lower version or replayed control revision is refused).
4. **Downloads and self-checks** the candidate binary (size- and
   sha256-verified) and runs it through a side-effect-free self-check before
   trusting it.
5. **Installs atomically** under an advisory lock through a crash-safe
   transaction: the previous binary is backed up, the new one is put in place,
   a post-install self-check runs, and only then is the install committed. If
   the self-check fails, the previous version is restored automatically.

The check never returns an error and never trips `--exit-code-on-warning`; in
JSON/machine output modes it is silent. In human mode you may see a one-line
advisory when an upgrade was installed or rolled back.

## Platform support (first phase)

| Platform | Auto-upgrade |
| --- | --- |
| Linux x86_64 | Supported |
| Linux aarch64 | Supported |
| macOS aarch64 | Supported |
| Windows x86_64 | Published, but returns `UnsupportedPlatform` (binary untouched) |
| macOS x86_64 | Not in the release matrix; no auto-upgrade |

## What is *not* auto-upgraded

Only installs performed by the official signed script installer are eligible.
Homebrew, from-source builds, manual copies, and third-party package managers
are never marked official and never auto-upgrade — an official-install marker
is written only after a verified signed-manifest install, and it must match the
actual binary's platform, size, and sha256 (its version field is
informational; version and digest come out of the same verified signed
manifest row, which is what binds them). A
binary hashing itself, or a marker copied next to a different binary, never
qualifies.

## Recovery and safety

- A crashed upgrade is detected and resolved before your next command runs:
  it is either completed, or rolled back to the previous working version.
- Anti-rollback state (`{INSTALL_DIR}/.libra-upgrade-state.json`) prevents
  downgrade and replay. It also records the highest accepted manifest
  `min_key_generation`: later manifests may not lower it, and a signature is
  accepted only when its key generation meets the maximum of the manifest,
  compiled, and persisted floors. If the state is ever corrupt, Libra refuses
  to upgrade rather than silently discarding that protection.
- If an installed version is later revoked, Libra keeps running it (it does not
  auto-downgrade) but surfaces a high-priority warning pointing at the fixed
  release.

## Upgrading on demand: `libra upgrade`

The explicit counterpart to the background check — and the consumer of
`upgrade.mode=manual` — is the [`libra upgrade`](commands/upgrade.md)
command:

```bash
libra upgrade           # check, show current vs latest, ask before installing
libra upgrade --check   # only report whether a newer signed version exists
libra upgrade --yes     # install without the prompt (scripts / CI)
```

It runs exactly the same verified pipeline as `auto` (signed manifest,
anti-rollback floors, sha256/size-enforced download, locked install
transaction with self-check and rollback), but on your explicit request and
regardless of the auto check's cooldown. `upgrade.mode` does not gate it:
even with `off`, an explicit `libra upgrade` works — the mode only controls
the background behaviour.

Since v0.22.10 the install script writes the official-install marker itself
on every verified install, so a fresh `curl … | sh` install is immediately
upgrade-manageable. Installs made with older script versions carry no
marker yet: run the install script once more and `libra upgrade` works from
then on.

## Turning it off

```bash
libra config set --global upgrade.mode off
```
