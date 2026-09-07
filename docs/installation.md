# Installing Libra

## Script installer

The recommended macOS/Linux installation is:

```bash
curl -fsSL https://download.libra.tools/install.sh | sh
```

### Trust model: the signed stable channel

By default both installers resolve the version through the **Ed25519-signed
stable manifest** (`https://download.libra.tools/libra/releases/stable/manifest-v1.json`):
the manifest signature is verified against a public key embedded in the
installer, and the binary download is then checked byte-for-byte against the
signed `sha256` and `size`. Any verification failure aborts the install with
nothing written — a compromised mirror cannot substitute a binary.

Three paths deliberately do **not** verify against the signed manifest, and
each is explicit:

- **`-v <version>` / `-Version`** — pinning a historic version bypasses the
  stable channel; the installer prints a warning.
- **A custom mirror** (`LIBRA_BASE_URL` / `-DownloadBaseUrl`) — same warning.
- **Transition states** — when the signed manifest does not exist yet (the
  signature chain is not enabled) or the host cannot verify signatures
  (`install.sh` needs OpenSSL ≥ 1.1.1 with Ed25519 support plus a sha256
  tool; `install.ps1` ships its own verifier), the installer stops and asks
  for explicit confirmation, or `LIBRA_ALLOW_FALLBACK=1`, before falling back
  to the unverified legacy path. It never falls back silently.

The embedded public key has **no runtime override**: no flag or environment
variable can change the trust root of a released installer. Because the
installer script itself is fetched from the same CDN, its whole-file
substitution remains out of scope of this check — the out-of-band trust
anchor (published script hashes) is tracked separately in the UP-01 design
document.

The installer places `libra` in `$LIBRA_HOME/bin` (`~/.libra/bin` by default),
writes shell environment files, and creates the optional relative symlink:

```text
~/.libra/bin/lba -> libra
```

Both names execute the same binary. The relative target remains valid when the
whole Libra home directory is moved.

## Windows

```powershell
irm https://download.libra.tools/install.ps1 | iex
```

`install.ps1` installs per-user (no administrator rights), places `libra.exe`
under `%LOCALAPPDATA%\libra\bin`, adds that directory to the user `PATH`, and
writes `libra.cmd` plus the optional `lba.cmd` shorthand into the first
writable shim directory (`%LOCALAPPDATA%\Microsoft\WindowsApps`, else
`%USERPROFILE%\.local\bin`).

Windows has no symlink equivalent available to an unprivileged install, so the
alias is a `.cmd` shim rather than a symlink. The safety contract is otherwise
the same as the POSIX one:

- Every shim the installer writes carries a `@rem libra-managed-shim` marker.
  That marker is how a reinstall recognises its own shim (rewriting is
  idempotent) and how it recognises one it did not write.
- An existing `lba.cmd` WITHOUT the marker, or any existing `lba.exe`,
  `lba.bat`, or `lba.ps1`, is left untouched — `lba` is short enough to belong
  to another tool, and Windows would resolve those extensions ahead of ours.
- `-NoAlias` (or `LIBRA_NO_ALIAS=1`) skips the shorthand. To pass the
  switch, save and invoke the installer rather than piping it to `iex`:

  ```powershell
  irm https://download.libra.tools/install.ps1 -OutFile install.ps1
  .\install.ps1 -NoAlias
  ```

- Failing to write the shim warns; it does not fail the installation.

## Alias safety and idempotency

- A fresh install creates `lba` by default.
- Re-running the installer for the already-installed version repairs a missing
  alias without downloading or replacing `libra`.
- A valid `lba -> libra` or `lba -> $LIBRA_INSTALL_DIR/libra` symlink is
  accepted and refreshed to the relative form.
- A regular file, directory, or symlink to another target named `lba` is
  user-owned and is never overwritten. The installer prints a warning and
  continues.
- If the platform or filesystem cannot create symlinks, installing `libra`
  still succeeds. Use the full `libra` command after the warning.

The installer does not create a copy, hard link, shell function, or alias in a
profile. `lba` is only the optional filesystem symlink beside `libra`.

## Opting out

Use the flag for one invocation:

```bash
curl -fsSL https://download.libra.tools/install.sh | sh -s -- --no-alias
```

Or set the environment variable for automated installations:

```bash
curl -fsSL https://download.libra.tools/install.sh | LIBRA_NO_ALIAS=1 sh
```

The opt-out does not remove an existing alias; it only prevents the current
installer run from creating or refreshing one. Remove a known Libra-owned
symlink explicitly if desired:

```bash
test "$(readlink "$HOME/.libra/bin/lba" 2>/dev/null)" = libra &&
    rm "$HOME/.libra/bin/lba"
```

## Related options

| Option / variable | Effect |
|-------------------|--------|
| `-v`, `--version <VERSION>` | Install a specific release |
| `-d`, `--dir <PATH>` | Override the binary directory |
| `LIBRA_INSTALL_DIR` | Environment equivalent for the binary directory |
| `--no-modify-path` | Write env files but do not edit shell rc files |
| `--no-alias` | Do not create or refresh `lba` for this run |
| `LIBRA_NO_ALIAS=1` | Environment opt-out for `lba` |

Run `sh install.sh --help` from a checkout for the complete option and
environment-variable list.
