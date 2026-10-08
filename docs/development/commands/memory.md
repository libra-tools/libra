# `libra memory`

Libra-only deterministic Agent development-history projection (plan-20260926
DM-01..DM-13). No Git analogue. Surface registered in `src/cli.rs` (DM-03); the
behavior, stable error codes (`LBR-MEMORY-*`) and full documentation are
delivered by DM-11.

## Usage

```text
libra memory <subcommand> [options]
```

| Subcommand | Description |
|---|---|
| `status` | Report projection freshness / horizon state (`memory_projection_state`). |
| `list` | Enumerate derived memory episodes. |
| `show <episode_id>` | Render a single derived memory episode. |
| `rebuild` | Re-derive the zero-authority projection (GC-DM-01). |

Read-only on the repository DB surface; no public CLI behavior change until the
subcommand behavior lands (DM-11).
