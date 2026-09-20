# `libra sandbox`

Inspect AI sandbox diagnostics for the current machine.

## Synopsis

```bash
libra sandbox status
libra --json sandbox status
```

## Description

`libra sandbox status` reports the sandbox backend Libra would use for AI shell
execution diagnostics. It does not require a repository, so it can be used while
debugging provider or CI hosts for external-agent capture and review.

The default runtime is best-effort: Linux uses the external helper configured by
`LIBRA_LINUX_SANDBOX_EXE`, and if that helper is unavailable `libra` will try
the built-in `bwrap` backend (optionally overridden by
`LIBRA_BWRAP_BINARY`). macOS uses Seatbelt when `/usr/bin/sandbox-exec` is
available, and unsupported or unconfigured hosts report warnings instead of
claiming isolation. Set `LIBRA_SANDBOX_ENFORCEMENT=required` to fail commands
that request Libra's internal sandbox when no supported backend can be applied.

On macOS, the default Seatbelt policy keeps project files readable but denies
common credential, token, and browser profile paths. The built-in deny list
includes `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.netrc`, `.azure`, `.docker`,
`.npmrc`, `.pypirc`, Cargo/Gem credentials, `~/.config/gcloud`, `~/.config/gh`,
`~/.config/hub`, `~/.kube`, `~/.config/libra/vault`, Firefox, Chrome,
Chromium, and Brave profile directories, macOS `Library/Cookies`, and
`/etc/shadow`. Repos can append project-specific paths with
`.libra/sandbox.toml` `deny_read = [...]`. Unreadable or malformed
`sandbox.toml` fails closed (the command does not fall back to an empty
policy). In a linked worktree the repository file is the baseline; a
per-worktree overlay may only tighten `deny_read` / `[sandbox.network]`
and cannot switch `denied` → `full`.

## Human Output

```text
Sandbox status
  platform: linux
  sandbox_type: none
  enforcement: best_effort
  effective_enforcement: best_effort
  network: denied
  proxy_backend: noop
  bwrap_available: false
  bwrap_requested: false
  seatbelt_available: false
  helper_path: (not configured)
  helper_path_exists: false
  writable_roots:
    - /path/to/workspace
  warnings:
    - linux sandbox helper is not configured; AI shell commands currently fall back to no OS sandbox
```

## JSON Output

```json
{
  "ok": true,
  "command": "sandbox.status",
  "data": {
    "platform": "linux",
    "sandbox_type": "none",
    "enforcement": "best_effort",
    "effective_enforcement": "best_effort",
    "writable_roots": ["/path/to/workspace"],
    "network": {
      "mode": "denied",
      "allowlist": []
    },
    "proxy_backend": "noop",
    "bwrap_available": false,
    "bwrap_requested": false,
    "seatbelt_available": false,
    "helper_path": {
      "path": null,
      "exists": false
    },
    "warnings": []
  }
}
```

## Fields

| Field | Description |
|-------|-------------|
| `platform` | Rust target OS for the running Libra binary |
| `sandbox_type` | Effective OS sandbox backend, or `none` when no backend is currently usable |
| `enforcement` | Current enforcement policy from `LIBRA_SANDBOX_ENFORCEMENT`; `required` rejects missing internal sandboxes, while `best_effort` reports downgrade risk without failing commands |
| `effective_enforcement` | Enforcement mode after environment parsing and fallback warnings |
| `writable_roots` | Default workspace-write roots after resolving the current directory and temporary directories |
| `network.mode` | Current network policy summary (`denied`, `allowlist`, `full`) |
| `network.allowlist` | Host/service allowlist when `network.mode` is `allowlist` |
| `proxy_backend` | Selected network proxy strategy: `noop` (deny-all, used by `denied` mode), `allowlist` (per-host allowlist proxy, used by `allowlist` mode), `loopback-only` (the `full`-mode diagnostic placeholder), or `none` (an `allowlist`-mode proxy was requested but could not be constructed — either the `required` enforcement tier rejected the run, or `prefer_strict` / `best_effort` degraded it to deny-all; see `warnings` for the reason) |
| `bwrap_available` | Whether `bwrap` is executable on `PATH` |
| `bwrap_requested` | Whether `LIBRA_USE_LINUX_SANDBOX_BWRAP` is enabled |
| `seatbelt_available` | Whether `/usr/bin/sandbox-exec` is executable |
| `helper_path` | `LIBRA_LINUX_SANDBOX_EXE` path and executable probe result |
| `warnings` | Downgrade or unsupported-platform diagnostics |

## Examples

```bash
# Show effective sandbox diagnostics for AI tool execution
libra sandbox status

# Structured JSON output for agents
libra sandbox --json status

# Machine-strict JSON (implies --json=ndjson --no-pager --color=never --quiet)
libra sandbox --machine status
```

The same banner is rendered by `libra sandbox --help` so the doc and the
CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout, see
`docs/development/commands/_general.md` item B).
