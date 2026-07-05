# `libra auth`

Host-scoped HTTP token auth (a Libra extension, lore.md §1.6). Token-only
v1: the full lifecycle — write, read, expiry detection, revoke — in one
surface.

## Synopsis

```
libra auth login --host <host[:port]> [--username <u>] [--with-token] [--expires-at <RFC3339> | --expires-in <N>d|h|m|s]
libra auth status [--host <host>]
libra auth logout [--host <host> | --all]
libra auth clear
```

## Description

`auth login` stores a token for a host. **There is deliberately no
`--token <value>` flag** — argv lands in shell history and `/proc`; the
token arrives via a hidden prompt (TTY) or `--with-token` stdin:

```bash
printf '%s' "$TOKEN" | libra auth login --host git.example.com --with-token
```

### GitHub HTTPS PATs

GitHub HTTPS Git accepts a personal access token (PAT) as the HTTP
password. `libra auth` stores that token for the `github.com` host and sends
it as Basic auth only when a later Libra HTTPS request matches the same
normalized host:port scope.

For an interactive terminal, prefer the hidden prompt so the token is never
written into argv or shell history:

```bash
libra auth login --host github.com --username x-access-token --expires-in 90d
```

Paste the PAT at the hidden token prompt. For scripts, feed the token on
stdin from an environment secret, password manager, or CI secret store:

```bash
printf '%s\n' "$GITHUB_PAT" \
  | libra auth login \
      --host github.com \
      --username x-access-token \
      --with-token \
      --expires-in 90d
```

Then verify without printing the secret:

```bash
libra auth status --host github.com
```

Use an HTTPS remote such as `https://github.com/OWNER/REPO.git`; do **not**
embed the PAT in the remote URL (`https://x-access-token:PAT@github.com/...`)
because URLs can leak through shell history, config, process lists, logs, and
error output. If a Git-compatible consumer calls `libra credential fill` with
an explicit `username=<your-login>`, store the token with the same username
instead, because username pinning is honored.

Choose the least-privileged GitHub token that can access the repositories you
need. For private repositories, organization resources, or SAML-enforced
organizations, GitHub policy may require additional repository permissions or
SSO authorization. See GitHub's PAT documentation:
<https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens>.

### Secret storage and key handling

Tokens are AES-256-GCM-encrypted with the global vault key
(`~/.libra/vault-unseal-key`, 0600) and stored as ciphertext in the global
config DB — `libra config get/list/unset` can neither dump nor forge nor
delete `auth.token.*` entries (the auth surface is the only door). One token
per host:port scope; re-login overwrites. The **OS-keyring backend** (lore.md
2.7) is selected with `libra config --global auth.backend keyring` (release
binaries ship it; Linux uses a statically-vendored libdbus): the secret then
lives in the platform keychain and only a non-secret marker stays in the
config store. `libra auth migrate --to keyring|file` moves stored tokens
(probed, verified, idempotent); flipping `auth.backend` alone is
non-destructive (lookups consult both). Revocation always reaches both
backends. `status` reports each token's `backend` and an `unreadable` state
when a keyring entry is missing or the service is unavailable.

Use the default file backend when the global Libra vault key is the desired
local secret root. Use the OS keyring backend when you want GitHub PATs and
other host tokens to live in the platform keychain instead:

```bash
libra config --global auth.backend keyring
libra auth migrate --to keyring
libra auth status --host github.com
```

`auth status` reports the active backend and never reveals token material.
Switching back to the encrypted file backend is explicit:

```bash
libra auth migrate --to file
```

**Attach rules (the trust boundary, stored tokens)**: a stored token is sent
only on requests whose normalized host:port matches, over **https** (http
only for loopback hosts — note a token stored without an explicit port
normalizes to 443, so log in with the explicit port for non-443 loopback
remotes). Cross-host requests never see it. Redirects that would downgrade
https→http are refused outright (reqwest only strips credentials on
host/port changes, not scheme changes). The interactive 401 prompt remains
the process-wide fallback and takes precedence. The `credential fill` helper
also consults the store (https only, silent misses, username pinning
honored); `credential store/erase` never manage auth tokens.

`auth status` never prints the token: per host it reports the username,
expiry, and `valid` / `expired` / `undecryptable` (key changed — log in
again). With `--host` it is scriptable: exit 0 iff a valid token exists.
Expired tokens are warned about at use time with an `auth login` hint.

**Interactive flows**: a 401 on a non-TTY run fails fast with an
`auth login` hint (piped protocol data is never consumed by a prompt); on a
TTY the prompt shows the hint once, and after a prompted attempt genuinely
succeeds you are offered — once per host, default No — to store the
credential (`auth.saveOnPrompt` = `ask`/`always`/`never`).

`auth logout --host <h>` revokes one host; `--all` / `auth clear` (Lore's
verb) revoke everything — revocation works even after key rotation (no
decryption needed) and is idempotent.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (`status --host`: a valid token exists). |
| `1` | `status --host` with no valid token. |
| `128` | Storage failures. |
| `129` | Usage (bad host/expiry/username; non-TTY without `--with-token`; empty token). |

## Examples

```bash
libra auth login --host git.example.com              # hidden prompt
printf '%s' "$TOKEN" | libra auth login --host git.example.com --with-token
libra auth login --host github.com --username x-access-token --expires-in 90d
printf '%s\n' "$GITHUB_PAT" | libra auth login --host github.com --username x-access-token --with-token
libra config --global auth.backend keyring && libra auth migrate --to keyring
libra auth login --host git.example.com:8443 --expires-in 30d
libra auth status                                    # all hosts, no secrets
libra auth status --host git.example.com && echo ok  # scriptable
libra auth logout --host git.example.com
libra auth clear
```

## Comparison with Git

Git delegates this to credential helpers (`git credential-store` writes
PLAINTEXT to `~/.git-credentials`; managers are external programs). Libra
ships encrypted-at-rest host tokens natively; the repo-scoped
`libra credential` helper protocol remains for Git-compatible flows.
Classified `intentionally-different` in
[`COMPATIBILITY.md`](../../COMPATIBILITY.md).
