# `libra whoami`

Report the identity associated with a stored Libra host session token.

`libra whoami` is a Libra-only extension (no Git equivalent). It queries the
host's `/api/cli/whoami` endpoint to report the current identity. If no
session is stored it errors with `LBR-AUTH` missing-credentials; if the
host is unreachable or rejects the token the command fails (network /
permission error). It is independent of the credentials used by `cloud` /
`publish`.

## Options

| Flag | Description |
|------|-------------|
| `--host <url>` | Target host (defaults to the built-in host) |
| `--refresh` | Accepted for forward compatibility; currently a no-op (identity is always validated against the host and the stored token is not rewritten) |

Human-readable output displays the host returned by the server after URL credential redaction. JSON output returns the server's host value unchanged.

## Examples

```bash
libra whoami
libra whoami --host https://libra.tools
```
