# Libra CLI Error Codes

Libra now exposes failures through a stable three-layer contract:

1. `exit code`
   Fast shell/CI branching. `0` means success. Any non-zero value is a failure.
2. `stable error code`
   A machine-stable identifier for agents, wrappers, and higher-level UX.
3. `structured JSON report`
   When structured mode is enabled, the last stderr line is JSON and carries category, message, hints, and details.

This contract is implemented in [`src/utils/error.rs`](../src/utils/error.rs).

## Output Contract

On failure, Libra always writes a human-readable error block to `stderr`.

When `stderr` is **not** a TTY, Libra additionally writes:

1. An `Error-Code: ...` line
2. A final JSON line with the structured report

This keeps interactive terminal output readable while preserving structured data
for shell pipelines, CI, and wrappers that capture `stderr`.

To force structured output even in an interactive terminal, set:

```bash
LIBRA_ERROR_JSON=1
```

Example:

```text
fatal: not a libra repository (or any of the parent directories): .libra
Error-Code: LBR-REPO-001
Hint: run 'libra init' to create a repository in the current directory.
{"ok":false,"error_code":"LBR-REPO-001","category":"repo","exit_code":128,"severity":"fatal","message":"not a libra repository (or any of the parent directories): .libra","hints":["run 'libra init' to create a repository in the current directory."]}
```

Warnings and progress messages remain plain text. Only failures participate in this contract.

Status-only probes are an explicit exception. `libra cat-file -e` preserves Git-compatible
silent `0`/`1` behavior and does not emit the human-readable block or trailing JSON report
when the object is missing.

## Exit Codes

| Exit | Meaning | Primary automation use |
| --- | --- | --- |
| `0` | Success | Continue |
| `9` | Warnings emitted (`--exit-code-on-warning`) | Review warnings |
| `128` | Fatal runtime error | Check `error_code` for category |
| `129` | Usage / invalid target | Fix CLI invocation |

Set `LIBRA_FINE_EXIT_CODES=1` to re-enable the legacy fine-grained exit codes (2-8) described in the migration section below. When this variable is unset or `0`, Libra uses the Git-standard codes shown above.

## Migration From Fine-Grained Exit Codes

Libra previously used fine-grained exit codes (2-8) to distinguish failure categories.
The current default aligns with Git-standard exit codes: `128` for fatal errors,
`129` for usage errors, and `9` for warnings. The stable symbolic `error_code` field
in the JSON report continues to provide fine-grained classification.

This is an intentional migration from fine-grained exit codes (2-8) to Git-standard
exit codes (128/129), improving compatibility with Git-aware tooling and CI systems.

| Fine-grained behavior | Fine-grained exit | Git-standard contract |
| --- | --- | --- |
| Usage / invalid target | `2` | `129` + same `LBR-CLI-*` code |
| Fatal runtime errors (repo, config, conflict, network, auth, I/O, internal) | `3`-`8` | `128` + same `LBR-*` code |
| Warnings emitted | `9` | Unchanged `9` |
| `cat-file -e` missing object probe | `1` | Still `1` with no stderr output |

If you have existing scripts that branch on fine-grained exit codes (2-8), you can
set `LIBRA_FINE_EXIT_CODES=1` to preserve the old behavior. Otherwise, migrate your
scripts to branch on `128`/`129` and use the JSON `error_code` field for fine-grained
classification. If your automation allocates a TTY, set `LIBRA_ERROR_JSON=1` so the
structured report is always present.

## Complete Stable Code Table

| Exit | Stable code | Category | Meaning | Typical examples |
| --- | --- | --- | --- | --- |
| `129` | `LBR-CLI-001` | `cli` | Unknown command | `libra wat` |
| `129` | `LBR-CLI-002` | `cli` | Invalid or missing CLI arguments | missing required flag, conflicting flags |
| `129` | `LBR-CLI-003` | `cli` | Invalid object, revision, pathspec, or move target | bad ref, invalid pathspec, outside-repo move target |
| `128` | `LBR-REPO-001` | `repo` | Not inside a Libra repository | running repo commands outside `.libra` |
| `128` | `LBR-REPO-002` | `repo` | Repository metadata is corrupt or incompatible | missing DB, corrupted metadata |
| `128` | `LBR-REPO-003` | `repo` | Repository state blocks the operation | no commits yet, detached state mismatch, missing configured remote |
| `128` | `LBR-CONFIG-001` | `config` | Global config DB schema is newer than this Libra binary supports | `pull`, `push`, `fetch`, `clone`, or `cloud` would otherwise silently ignore global storage config |
| `128` | `LBR-CONFLICT-001` | `conflict` | Unresolved conflict is present | merge/rebase conflict still unresolved |
| `128` | `LBR-CONFLICT-002` | `conflict` | Operation blocked to avoid overwriting state | non-fast-forward, destination exists, dirty worktree |
| `128` | `LBR-POLICY-001` | `conflict` | Branch policy (protect/archive metadata) blocked the ref update | `branch reset` / `update-ref` on a protected or archived branch |
| `128` | `LBR-CASE-001` | `conflict` | Paths that differ only by case collide on a case-insensitive filesystem | `add`/`checkout`/`switch`/`mv` under `core.casehandling=error` |
| `128` | `LBR-LAYER-001` | `conflict` | A layer overlay path collided with tracked content, or a layer path was staged | `layer apply` collision / `add` of a layer overlay path (lore.md 2.4) |
| `128` | `LBR-OBLITERATE-001` | `repo` | No payload found for the object to obliterate | `file obliterate` on an absent/unknown OID (lore.md 2.5) |
| `128` | `LBR-OBLITERATE-002` | `repo` | Object exists only inside a packfile; v1 cannot rewrite packs | `file obliterate` on a packed-only object (lore.md 2.5) |
| `128` | `LBR-OBLITERATE-003` | `conflict` | Obliteration not confirmed; it is irreversible and requires --yes | `file obliterate` without `--yes` (lore.md 2.5) |
| `128` | `LBR-NET-001` | `network` | Remote unreachable or transport unavailable | DNS, timeout, TLS, connection refused |
| `128` | `LBR-NET-002` | `network` | Protocol, negotiation, or pack failure | packet-line, sideband, unpack/ref update protocol errors |
| `128` | `LBR-AUTH-001` | `auth` | Missing identity, token, or credentials | missing commit identity, missing API key, missing SSH material |
| `128` | `LBR-AUTH-002` | `auth` | Credential present but permission denied | forbidden push, insufficient scope |
| `128` | `LBR-IO-001` | `io` | Read/open/load failure | failed to open pack, failed to read index |
| `128` | `LBR-IO-002` | `io` | Write/save/update/remove failure | failed to write index, failed to remove file |
| `128` | `LBR-INTERNAL-001` | `internal` | Unexpected internal invariant failure | invariant break, unclassified internal failure |
| `128` | `LBR-BISECT-001` | `repo` | `bisect view` / `bisect run` invoked outside an active bisect session | running `bisect view` before `bisect start` |
| `128` | `LBR-BISECT-002` | `internal` | `bisect run` command exited with code ≥ 128 or was killed by a signal | run script aborted via SIGINT, exit 130 |
| `128` | `LBR-BISECT-003` | `repo` | `bisect run` cannot advance because no candidate commits remain | bisect already converged when `run` is invoked |
| `129` | `LBR-ADD-001` | `cli` | `libra add` invoked with no matched paths and nothing already staged | `libra add nonexistent.txt` on an empty index |
| `128` | `LBR-UNSUPPORTED-001` | `repo` | Operation declined because the requested mode is intentionally unsupported in this batch | requesting a Git feature explicitly declined in `docs/development/commands/_compatibility.md` |
| `128` | `LBR-AGENT-001` | `internal` | AI agent run exceeded a configured budget dimension (tokens, tool calls, wall-clock, source calls, or cost) | a sub-agent ran 500 tool calls when `max_tool_calls = 200` |
| `128` | `LBR-AGENT-002` | `internal` | External `libra-agent-*` agents are disabled (`agent.external_agents.enabled` defaults to `false`) | `libra agent rpc invoke` before opting in to external discovery |
| `128` | `LBR-AGENT-003` | `internal` | External agent binary negotiated an incompatible protocol version | a `libra-agent-*` binary answers `info` with an unsupported `protocol_version` |
| `128` | `LBR-AGENT-004` | `internal` | Method not declared in the external agent's negotiated capabilities | invoking `read_transcript` when the binary's `capabilities` omit it |
| `128` | `LBR-AGENT-005` | `internal` | External agent binary failed provenance revalidation (untrusted, or sha256/device/inode/mtime changed since trust) | the trusted binary at `~/.libra/agents/libra-agent-foo` was replaced after `libra agent rpc trust` |
| `128` | `LBR-AGENT-006` | `internal` | External `libra-agent-*` binary tried to impersonate a built-in agent slug | a PATH binary named `libra-agent-claude-code` |
| `128` | `LBR-AGENT-007` | `internal` | External agent IO exceeded hard caps or failed redaction; output withheld fail-closed | a binary floods stderr past the 64 KiB cap |
| `128` | `LBR-AGENT-008` | `internal` | Hook envelope failed validation before checkpoint persistence | a provider hook posts malformed JSON to `libra agent hooks` |
| `128` | `LBR-AGENT-009` | `internal` | Agent checkpoint store inconsistent across ref/DB/object-index | `agent_checkpoint` row points at a missing traces object; run `libra agent doctor` |
| `128` | `LBR-AGENT-010` | `internal` | `review --fix` / `investigate fix` requires the internal AgentRuntime fix bridge, which is not available | `libra review --fix` before the fix bridge lands |
| `128` | `LBR-AGENT-011` | `internal` | Untrusted seed content cannot enter a mutating workflow without explicit approval | an issue-link seed attempting to drive a mutating fix |
| `128` | `LBR-AGENT-012` | `internal` | External agent RPC transport failed (invoke timeout, broken pipe/unexpected exit, or malformed JSON-RPC frame); invocation withheld fail-closed | a trusted `libra-agent-*` binary exits before answering the invoked method |
| `128` | `LBR-AGENT-013` | `internal` | Raw (un-redacted) checkpoint access/export denied without `--allow-raw`; redacted `--detail`/`--transcript` output stays available; the refusal is audited in `agent_audit_log` | `libra agent checkpoint export --raw` (or equivalent) without `--allow-raw` |
| `128` | `LBR-AGENT-014` | `internal` | A `review`/`investigate` run was refused because the shared run queue is full — more than `agent.max_concurrent_runs` runs are active and the wait queue is at its cap (10) | starting an 11th queued `libra review`/`libra investigate` run while the concurrency budget is saturated |
| `9` | `LBR-WARN-001` | `warning` | Command completed with warnings | `--exit-code-on-warning` |

## Stable Codes By Category

### CLI

| Stable code | Meaning |
| --- | --- |
| `LBR-CLI-001` | Unknown command |
| `LBR-CLI-002` | Invalid or missing CLI arguments |
| `LBR-CLI-003` | Invalid object, revision, pathspec, or move target |
| `LBR-ADD-001` | `libra add` matched no paths and nothing already staged |

### Repository

| Stable code | Meaning |
| --- | --- |
| `LBR-REPO-001` | Not inside a Libra repository |
| `LBR-REPO-002` | Repository metadata is corrupt or incompatible |
| `LBR-REPO-003` | Repository state blocks the operation |

### Config

| Stable code | Meaning |
| --- | --- |
| `LBR-CONFIG-001` | Global config database schema is newer than this Libra binary supports; update Libra or explicitly use `--offline` / `LIBRA_READ_POLICY=offline|local` when local-only object access is intended. |

### Conflict

| Stable code | Meaning |
| --- | --- |
| `LBR-CONFLICT-001` | Unresolved conflict is present |
| `LBR-CONFLICT-002` | Operation blocked to avoid overwriting state |
| `LBR-POLICY-001` | Branch policy (protect/archive metadata) blocked the ref update. |
| `LBR-CASE-001` | Paths that differ only by case collide on a case-insensitive filesystem. |
| `LBR-LAYER-001` | A layer overlay path collided with tracked content; a layer may only add untracked paths. |
| `LBR-OBLITERATE-001` | No payload was found for the object to obliterate. |
| `LBR-OBLITERATE-002` | The object exists only inside a packfile; v1 obliteration cannot rewrite packs. |
| `LBR-OBLITERATE-003` | Obliteration was not confirmed; it is irreversible and requires --yes. |

### Network

| Stable code | Meaning |
| --- | --- |
| `LBR-NET-001` | Remote unreachable / transport unavailable |
| `LBR-NET-002` | Protocol, negotiation, or pack failure |

### Auth

| Stable code | Meaning |
| --- | --- |
| `LBR-AUTH-001` | Missing identity, token, or credential material |
| `LBR-AUTH-002` | Credential present but permission denied |

### I/O

| Stable code | Meaning |
| --- | --- |
| `LBR-IO-001` | Read/open/load failure |
| `LBR-IO-002` | Write/save/update/remove failure |

### Internal

| Stable code | Meaning |
| --- | --- |
| `LBR-INTERNAL-001` | Unexpected internal invariant failure |
| `LBR-AGENT-001` | AI agent run exceeded a configured budget dimension (tokens, tool calls, wall-clock, source calls, or cost) |
| `LBR-AGENT-002` | External `libra-agent-*` agents are disabled (`agent.external_agents.enabled` defaults to `false`) |
| `LBR-AGENT-003` | External agent binary negotiated an incompatible protocol version |
| `LBR-AGENT-004` | Method not declared in the external agent's negotiated capabilities |
| `LBR-AGENT-005` | External agent binary failed provenance revalidation (untrusted, or sha256/device/inode/mtime changed since trust) |
| `LBR-AGENT-006` | External `libra-agent-*` binary tried to impersonate a built-in agent slug |
| `LBR-AGENT-007` | External agent IO exceeded hard caps or failed redaction; output withheld fail-closed |
| `LBR-AGENT-008` | Hook envelope failed validation before checkpoint persistence |
| `LBR-AGENT-009` | Agent checkpoint store inconsistent across ref/DB/object-index |
| `LBR-AGENT-010` | `review --fix` / `investigate fix` requires the internal AgentRuntime fix bridge, which is not available |
| `LBR-AGENT-011` | Untrusted seed content cannot enter a mutating workflow without explicit approval |
| `LBR-AGENT-012` | External agent RPC transport failed (invoke timeout, broken pipe/unexpected exit, or malformed JSON-RPC frame); invocation withheld fail-closed |
| `LBR-AGENT-013` | Raw (un-redacted) checkpoint access/export denied without `--allow-raw`; redacted `--detail`/`--transcript` output stays available; the refusal is audited in `agent_audit_log` |
| `LBR-AGENT-014` | A `review`/`investigate` run was refused because the shared run queue is full (over `agent.max_concurrent_runs` active and the wait queue at its cap) |

### Unsupported

| Stable code | Meaning |
| --- | --- |
| `LBR-UNSUPPORTED-001` | Operation declined because the requested mode is intentionally unsupported in this batch (see `docs/development/commands/_compatibility.md`) |

Reportable internal failures (`CliError::internal`, explicit
`InternalInvariant` mappings, and legacy `internal error` / `panic` /
`invariant` / `unexpected` messages) include the Libra GitHub Issues URL in
human and JSON output so users and automation have a stable report destination.

### Bisect

The `LBR-BISECT-*` codes are emitted exclusively by `libra bisect` and its
subcommands. They use existing categories (`repo` for state issues, `internal`
for run-script failures) so generic `LBR-REPO-*` / `LBR-INTERNAL-*` shell
patterns continue to match them.

| Stable code | Category | Meaning |
| --- | --- | --- |
| `LBR-BISECT-001` | `repo` | `bisect view` or `bisect run` invoked outside an active bisect session |
| `LBR-BISECT-002` | `internal` | `bisect run` command exited with code ≥ 128 or was killed by a signal |
| `LBR-BISECT-003` | `repo` | `bisect run` cannot advance because no candidate commits remain |

### Warning

| Stable code | Meaning |
| --- | --- |
| `LBR-WARN-001` | Command completed with warnings (`--exit-code-on-warning`) |

## How To Use Codes

### Shell And CI

Use `exit code` for coarse branching:

```bash
if libra push; then
  echo "ok"
else
  case "$?" in
    128) echo "fatal error (check error_code for details)" ;;
    129) echo "fix CLI invocation" ;;
    9)   echo "warnings emitted" ;;
  esac
fi
```

For fine-grained classification, parse the JSON report from stderr:

```bash
output="$(libra push 2>&1)" || {
  json_line="$(printf '%s\n' "$output" | tail -n 1)"
  code="$(printf '%s\n' "$json_line" | jq -r '.error_code')"
  case "$code" in
    LBR-REPO-*)     echo "repository problem" ;;
    LBR-NET-*)      echo "network problem" ;;
    LBR-AUTH-*)     echo "auth problem" ;;
    LBR-CONFLICT-*) echo "conflict" ;;
    LBR-IO-*)       echo "I/O problem" ;;
    LBR-CLI-*)      echo "usage problem" ;;
    *)              echo "other: $code" ;;
  esac
}
```

### Agents And Wrappers

Use the final stderr JSON line for precise handling. The recommended order is:

1. Check `exit_code` to decide coarse recovery.
2. Check `error_code` to classify the exact failure family.
3. Use `message`, `hints`, and `details` to build the next user-facing prompt.

Example extraction:

```bash
stderr="$(libra add missing.txt 2>&1 >/dev/null)" || true
json_line="$(printf '%s\n' "$stderr" | tail -n 1)"
printf '%s\n' "$json_line" | jq '.error_code, .message, .hints'
```

If the wrapper runs Libra under a pseudo-terminal, export `LIBRA_ERROR_JSON=1`
to force the structured report.

### Interactive Discovery

Libra exposes the table directly through help:

```bash
libra help error-codes
```

Alias:

```bash
libra help errors
```

## JSON Schema

Every structured failure report includes:

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | `bool` | Always `false` for error reports |
| `error_code` | `string` | Stable code such as `LBR-REPO-001` |
| `category` | `string` | `cli`, `repo`, `conflict`, `network`, `auth`, `io`, `internal`, `warning` |
| `exit_code` | `number` | Shell-facing exit code |
| `severity` | `string` | `error` or `fatal` |
| `message` | `string` | User-facing error summary without prefix |
| `usage` | `string?` | Optional usage text for CLI errors |
| `hints` | `string[]` | Optional actionable hints |
| `details` | `object` | Optional structured context |

## Architecture

The design has four layers:

1. `CliError`
   Owns stable code, exit code, hints, details, and rendering.
2. `execute_safe(...) -> CliResult<()>`
   CLI-facing command entrypoints return structured errors instead of printing ad hoc text.
3. `emit_legacy_stderr(...)`
   Compatibility bridge for legacy commands that still produce `fatal:` / `error:` strings.
4. `main`
   Exits with `err.exit_code()` and keeps success at `0`.

This lets Libra migrate incrementally without breaking the stable external contract.

## How To Change Codes

Stable codes are part of Libra's public CLI contract. Changing them requires compatibility discipline.

### Rules

1. Never reuse an existing stable code for a different failure meaning.
2. Do not change an existing code's `exit code` or `category` unless the old mapping is clearly wrong and the migration is intentional.
3. Prefer adding a new stable code over silently repurposing an existing one.
4. Keep the human-readable `message` flexible, but treat `error_code`, `category`, and `exit_code` as stable.
5. When heuristics classify legacy text, update the classifier so old code paths still map to the same stable contract.

### Required Change Steps

When adding or changing a code:

1. Update [`src/utils/error.rs`](../src/utils/error.rs):
   add the `StableErrorCode` variant, its string, category, exit-code mapping, and description.
2. Update classification:
   adjust the legacy inference helpers so old `fatal:` / `error:` messages still map correctly.
3. Update command mapping:
   when a command has a precise failure mode, set the stable code explicitly instead of relying only on heuristics.
4. Update documentation:
   keep this file and `libra help error-codes` output in sync.
5. Update tests:
   assert both human-readable stderr and parsed JSON fields.

### Compatibility Guidance

- Adding a new stable code is backward compatible if old codes keep their meaning.
- Reclassifying a failure from one existing stable code to another is externally visible and should be treated like a CLI contract change.
- If a change affects automation, wrappers, or agents, note it in release notes or migration notes.

## Testing

Integration tests run Libra with captured `stderr`, so structured mode is enabled by default.
They parse the final JSON stderr line and assert both:

- human-readable text still makes sense
- machine-readable fields are stable

Shared helpers live in [`tests/command/mod.rs`](../tests/command/mod.rs).
