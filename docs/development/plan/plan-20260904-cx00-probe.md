# CX-00 probe (codex-cli 0.162.1)

Recorded 2026-10-10 09:02:07 UTC. Binary: `codex-cli 0.162.1` on PATH. Isolated `CODEX_HOME` was a fresh directory outside the user home. `auth.json` was copied at mode 0600 and was not written back. This file stores field names, counts, and hashes only.

## Version census

User-home rollout files named `rollout-*.jsonl`: 260. `cli_version` counts:

| cli_version | rollouts |
|---|---|
| 0.158.0-alpha.2.1 | 217 |
| 0.160.0 | 30 |
| 0.160.1 | 9 |
| 0.159.0-alpha.12.1 | 3 |
| 0.159.2 | 1 |
| 0.162.1 | 0 |

The pinned 0.162.1 census below uses only sessions this probe created. That set has 5 rollouts. It does not meet the ≥150 source requirement.

## Top-level `type` counts (5 rollouts, cli_version 0.162.1)

75 lines. Every line had `ordinal`.

| type | count |
|---|---|
| session_meta | 5 |
| event_msg | 25 |
| response_item | 30 |
| world_state | 5 |
| turn_context | 5 |
| token_usage_record | 5 |
| compacted | 0 |

`world_state` records: 5. `compacted` records: 0.

## `response_item.payload.type`

| payload.type | count |
|---|---|
| message | 30 |

No `reasoning` item appeared, so `summary` was neither empty nor non-empty: the array was absent. This does not prove 0.162.1 never emits `reasoning.summary[]`.

## `event_msg.payload.type`

| payload.type | count |
|---|---|
| task_started | 5 |
| item_completed | 10 |
| token_count | 5 |
| task_complete | 5 |

## `ordinal`

Present on 75 of 75 lines in the 5-rollout sample. Absent on 0 lines.

## `turn_id`

In rollout `event_msg` payloads, `turn_id` occurred on `task_started` (5), `item_completed` (10), and `task_complete` (5). It was absent on `token_count`. On hook stdin it occurred for `UserPromptSubmit` and `Stop`, and was absent for `SessionStart` and `SessionEnd`.

## Hook stdin field names

Eleven handlers were installed. A no-tool exec does not emit compaction, tool, permission, or subagent events. With `--dangerously-bypass-hook-trust` and `-s read-only`, the fired events and their stdin key paths were:

| event | stdin keys |
|---|---|
| SessionStart | cwd, hook_event_name, model, permission_mode, session_id, source, transcript_path |
| UserPromptSubmit | cwd, hook_event_name, model, permission_mode, prompt, session_id, transcript_path, turn_id |
| PreToolUse | not emitted |
| PostToolUse | not emitted |
| PermissionRequest | not emitted |
| PreCompact | not emitted |
| PostCompact | not emitted |
| Stop | cwd, hook_event_name, last_assistant_message, model, permission_mode, session_id, stop_hook_active, transcript_path, turn_id |
| SessionEnd | cwd, hook_event_name, reason, session_id, transcript_path |
| SubagentStart | not emitted |
| SubagentStop | not emitted |

## SessionEnd payload subset

Observed SessionEnd stdin keys: `cwd`, `hook_event_name`, `reason`, `session_id`, `transcript_path`. No other key was present on that event.

## `SubagentStop.agent_transcript_path`

`SubagentStop` did not fire. `agent_transcript_path` was not observed.

## Trust vectors

Identity JSON is compact, with sorted keys, `async` false, and no `statusMessage`. The command was `python3` plus the isolated hook script. Codex 0.162.1 did not run the hook when these hashes were installed.

| event | configured timeout | identity timeout | trusted_hash |
|---|---|---|---|
| session_end | 3 | 3 | sha256:9d05051ee4391776b399e6f06ec2b45e9a7c83c9a433f5f3be20d6caab9a4465 |
| stop | 30 | 30 | sha256:313468d1b7b2eaf080da52bddde53af84757654f52837a19aa3bc60fe74d76b9 |

Codex printed `clamping SessionEnd hook timeout to 3s` while the handler file still stored timeout 30, and again after that handler was rewritten to timeout 3. With the hashes above installed, neither `-s read-only` nor `--dangerously-bypass-approvals-and-sandbox` ran the hook command. The same read-only exec with `--dangerously-bypass-hook-trust` did run SessionStart, UserPromptSubmit, Stop, and SessionEnd.

## Hook trigger

| invocation | hook command ran |
|---|---|
| `-s read-only`, installed trust hash | no |
| `--dangerously-bypass-approvals-and-sandbox`, installed trust hash | no |
| `-s read-only` plus `--dangerously-bypass-hook-trust` | yes, four events listed above |

## Reasoning summary configuration

`codex doctor --strict-config` accepts `model_reasoning_summary` and warns that `reasoning_summary` and `model_reasoning_summary_format` are unrecognized. The bundled model catalog string `default_reasoning_summary` is `none`. The user config does not set `model_reasoning_summary`. The 5 new rollouts contained no reasoning item, so `reasoning.summary[]` non-emptiness is unproven.

## `encrypted_content`

In the 5 rollouts, top-level `encrypted_content` count is 0 and `compacted.replacement_history` `encrypted_content` count is 0. Byte-count table:

| site | count | byte lengths |
|---|---|---|
| top-level | 0 | none |
| compacted.replacement_history | 0 | none |

Locator SHA-256 uniqueness inside each rollout: no locators, so no collision and no uniqueness proof.

## CX-22 count snapshot

2026-10-10 09:02:07 UTC. Read-only query of the user-home `state_*.sqlite` `threads` and `thread_spawn_edges` tables. No path, title, or preview was read.

| query | count |
|---|---|
| threads | 258 |
| thread_spawn_edges | 224 |
| threads.cli_version = 0.162.1 | 0 |
| threads.cli_version = 0.158.0-alpha.2.1 | 216 |
| threads.cli_version = 0.160.0 | 30 |
| threads.cli_version = 0.160.1 | 8 |
| threads.cli_version = 0.159.0-alpha.12.1 | 3 |
| threads.cli_version = 0.159.2 | 1 |

The rollout-file census and this index differ by two rows and are not the same population. Neither contains 150 sessions of 0.162.1.
