# `libra credential`

A vault-backed Git credential helper. Speaks the Git credential key/value
protocol on stdin/stdout and stores secrets AES-256-GCM-encrypted via the
repository vault — so credentials are never written to disk in clear text.

## Synopsis

```
libra credential get
libra credential store
libra credential erase
```

`get` is the Git helper protocol operation for a fill; `fill` is accepted as a
legacy alias. Each subcommand reads Git credential attributes (`key=value`
lines, terminated by a blank line) from stdin. An unknown operation is silently
ignored (Git helper convention).

## Description

- **`get`** (alias **`fill`**) — print the stored `username`/`password` for the
  requested `protocol`/`host`/`path`, or nothing. A miss (no entry, expired
  entry, wrong username, or no vault) and a hit both exit 0 and look identical
  apart from the output, so the exit code never reveals whether a credential
  exists.
- **`store`** — encrypt and persist the `username`/`password` from stdin. An
  optional `password_expiry_utc` is honoured; without one, the entry expires
  after 30 days. An already-expired `password_expiry_utc` is rejected.
- **`erase`** — remove the credential for the requested context.

Stored entries are keyed by a SHA-256 digest of `protocol/host/path`, so the
config never contains the host or username in clear text. There is one
credential per `protocol/host/path`; `fill` with a `username=` that does not
match the stored one is a miss.

**Security:** passwords and tokens are never logged, traced (even under
`RUST_LOG=debug`), or echoed in error messages — errors mention only the
non-secret routing context (`protocol://host`).

## Configuring as a Git helper

```
# in the repository config
[credential]
    helper = "!libra credential"
```

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success — `fill` printed a match or nothing; `store`/`erase` completed. |
| `128` | `store` with a missing username/password, an already-expired timestamp, or no initialized vault; or an unreadable request. |

## Examples

```bash
# Store a credential
printf 'protocol=https\nhost=example.com\nusername=alice\npassword=TOKEN\n' \
  | libra credential store

# Retrieve it
printf 'protocol=https\nhost=example.com\n' | libra credential fill

# Remove it
printf 'protocol=https\nhost=example.com\n' | libra credential erase
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Fill | `libra credential fill` | `git credential fill` |
| Store | `libra credential store` | `git credential-store store` |
| Erase | `libra credential erase` | `git credential-store erase` |

Differences: storage is vault-encrypted (not the plaintext `~/.git-credentials`)
and entries carry an expiry (default 30 days) and one credential per
`protocol/host/path`. Inside a repository the credential is repository-scoped;
outside a repository (or when the repo is absent) `store`/`get`/`erase` use a
user-level encrypted store in the global config (issues/480 HP-14), so the
helper is usable as a global `credential.helper`. Not exposed: `credential-cache`,
multiple usernames per host, and the consumer-side `credential.helper` chain
(Libra *is* a helper; it does not invoke external helpers).
