# `libra logfile`

Inspect Libra's tracing log-file configuration. Mirrors Lore's `logfile`
command. This is a diagnostic helper for the file-logging sink controlled by the
`LIBRA_LOG_FILE`, `LIBRA_LOG_ROTATION`, and `LIBRA_LOG` / `RUST_LOG` environment
variables.

## Synopsis

```
libra logfile info
```

## Description

`logfile info` reports how the current process would configure file logging,
resolved from the environment:

- **enabled** — whether tracing is on (a filter directive was resolved).
- **file** — the `LIBRA_LOG_FILE` path, or stderr when unset.
- **rotation** — the rolling strategy (`never` / `minutely` / `hourly` / `daily`).
- **filter** — the resolved `LIBRA_LOG` / `RUST_LOG` directive (or the
  `libra=debug` fallback used when only `LIBRA_LOG_FILE` is set).
- **size** — the total size and count of the log file(s) on disk (the single
  file under `never`, or every rolled file otherwise).

It needs no repository.

In human output mode, logging enabled without `LIBRA_LOG_FILE` writes tracing
diagnostics to stderr. Under `--json` / `--machine`, console tracing is disabled:
stdout carries command results and stderr carries structured errors. Set
`LIBRA_LOG_FILE` to retain tracing diagnostics in these modes; filtering and
rotation still apply to that file. The `enabled` field reports the environment's
filter configuration, independently of this console restriction.

### Log-file environment variables

| Variable | Meaning |
|----------|---------|
| `LIBRA_LOG_FILE` | Path of the append/rolled tracing sink. When unset, logs go to stderr in human output mode (only if a filter is set); JSON/machine mode disables console tracing. |
| `LIBRA_LOG_ROTATION` | `never` (default), `minutely`, `hourly`, or `daily`. When rolling, each active file is written as `<file>.<date-suffix>` so no single file grows without limit. Rotation only *splits* logs by time — it does **not** delete old files, so total disk usage still needs external retention (e.g. `logrotate`, or point `LIBRA_LOG_FILE` at a dedicated directory). |
| `LIBRA_LOG` / `RUST_LOG` | `tracing-subscriber` env filter. Falls back to `libra=debug` when only `LIBRA_LOG_FILE` is set. |

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `info` | Show the resolved log configuration. | `libra logfile info` |
| `--json` / `--machine` | Structured `{ enabled, file, rotation, filter, size_bytes, file_count }`. | `libra --json logfile info` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Configuration was reported. |

## Examples

```bash
# Show where logs would go with the current environment.
libra logfile info

# Roll the log daily and inspect the resolved config.
LIBRA_LOG_FILE=/var/log/libra/libra.log LIBRA_LOG_ROTATION=daily libra logfile info

# Structured output for tooling.
libra --json logfile info
```

## Comparison with Git

Git has no equivalent; this is a Libra diagnostic extension (like Lore's
`logfile`), classified `intentionally-different` in
[`COMPATIBILITY.md`](../../COMPATIBILITY.md).
