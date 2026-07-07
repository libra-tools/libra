# tests/ INDEX

> One-line index of every integration test target in `tests/`.
> Format: `target | wave | one-line purpose | relevant src paths`
>
> - `target` is the cargo `--test` name (matches `tests/<target>.rs`).
> - `wave` references `docs/development/integration/integration-test-plan.md §4`.
> - Use the three-part form `<target>::<test_fn>` whenever you reference a
>   specific test in PRs, reviews, or issue trackers (see §9.1 of the plan).
>
> Rows marked `TODO` need an owner pass; do not delete them — the file is the
> contract that AI reviewers reason against.

---

## Wave 1 — command layer & compat

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `command_test` | 1 | Top-level dispatcher covering most `libra <subcmd>` integration paths | `src/command/`, `src/cli.rs` |
| `compat_stash_subcommand_surface` | 1 | Guards `libra stash` subcommand surface vs. git CLI | `src/command/stash.rs` |
| `compat_bisect_subcommand_surface` | 1 | Guards `libra bisect` subcommand surface | `src/command/bisect.rs` |
| `compat_worktree_delete_dir` | 1 | Guards worktree delete semantics on dir removal | `src/command/worktree.rs` |
| `compat_checkout_alias_help` | 1 | Guards `--help` text for checkout aliases | `src/command/checkout.rs` |
| `compat_matrix_alignment` | 1 | Guards public docs/release matrices vs. real CLI/API surfaces | `COMPATIBILITY.md`, `docs/commands/code.md`, `.github/workflows/base.yml`, `src/cli.rs`, `src/internal/ai/web/mod.rs` |
| `compat_live_compat_workflow` | 1 | Guards optional live AI/cloud workflow remains manual/scheduled and secret-gated | `.github/workflows/live-compat.yml` |
| `compat_branch_lossy_wrapper_guard` | 1 | Guards branch-name lossy conversion wrapper | `src/internal/branch.rs` |
| `compat_lfs_client_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `internal/protocol/lfs_client.rs` | `src/internal/protocol/lfs_client.rs` |
| `compat_config_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `internal/config.rs` | `src/internal/config.rs` |
| `compat_head_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `internal/head.rs` | `src/internal/head.rs` |
| `compat_util_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `common_utils.rs` / `utils/` | `src/common_utils.rs`, `src/utils/` |
| `compat_client_storage_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `utils/client_storage.rs` | `src/utils/client_storage.rs` |
| `compat_extra_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in miscellaneous modules | `src/**` |
| `compat_all_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in general production codebase | `src/**` |
| `compat_agent_run_non_exhaustive_guard` | 1 | Enforces `#[non_exhaustive]` on every `pub enum` under `agent_run/` for additive evolution | `src/internal/ai/agent_run/` |
| `compat_agent_docs_contract` | 1 | Guards active Agent plan claims against stale removed-provider status, public schema/retention/raw-export wording, and stale internal-plan links | `docs/development/tracing/agent.md`, `src/command/code.rs` |
| `compat_agent_capability_matrix_pin` | 1 | Pins the E1 8-bool `DeclaredAgentCaps` wire keys and the first-batch supported roster (`claude-code`/`codex`/`opencode`) against drift (AG-16) | `src/internal/ai/observed_agents/{capability,registry}.rs`, `docs/development/tracing/agent.md` |
| `compat_agent_architecture_guard` | 1 | Observed-agents capture layer stays decoupled from AgentRuntime/checkpoint layers; every `AgentKind` resolves an adapter; external agents need the AG-18 info/trust flow; SQL CHECK / doc roster / enum stay in sync (AG-16) | `src/internal/ai/observed_agents/`, `sql/migrations/2026050303_agent_capture.sql`, `docs/development/tracing/agent.md` |
| `agent_rpc_external_test` | 1 | AG-18 external `libra-agent-*` protocol v2 + security: info/v1 negotiation, protocol-version fail-closed, capability gate, timeout/oversize caps, stderr capture/cap/redaction, env_clear allowlist, built-in slug impersonation skip | `src/internal/ai/observed_agents/rpc.rs` |
| `agent_rpc_span_test` | 1 | `agent.rpc.invoke` span fake-sink assertion (required fields present, raw response absent) — own binary to avoid tracing callsite-cache races | `src/internal/ai/observed_agents/rpc.rs` |
| `agent_transcript_intelligence_test` | 1 | AG-21 transcript intelligence: first-batch adapters extract prompts/model/tokens/modified-files/subagent totals/skill events from fixtures (provenance manifest in tests/fixtures/agent_transcripts/MANIFEST.md); E6 wire-key mapping pinned; fail-open partial semantics | `src/internal/ai/observed_agents/{extract.rs,builtin/}` |
| `agent_audit_log_test` | 3 | AG-24a compliance: append-only `agent_audit_log` enforcement (UPDATE/DELETE rejected by triggers, INSERT/SELECT allowed, denials recorded); retention-default constants pinned | `sql/migrations/2026070803_agent_audit_log.sql`, `src/internal/ai/observed_agents/compliance.rs` |
| `agent_lifecycle_event_test` | 1 | AG-19 central hook dispatcher contract (plan.md Task A4): invalid-envelope rejection without stdin echo, first-writer-wins owner filtering (SessionStart exempt), unknown-event skip-and-log, verb/kind mismatch fail-closed — via `libra agent hooks <agent> <verb>` E2E | `src/internal/ai/hooks/runtime.rs` |
| `agent_checkpoint_redaction_test` | 1 | AG-19 redaction-before-persist (plan.md Task A4): prompt and tool_response secrets scrubbed before the `agent_session` row lands, `redaction_report` records the rule hits, token absent from all `agent session` CLI JSON | `src/internal/ai/hooks/runtime.rs` |
| `agent_hook_span_test` | 1 | AG-19 `agent.hook.ingest` / `agent.redaction.apply` span fake-sink assertion (plan.md Task A4): required fields present (provider/verb/event_kind/frame_bytes/validated/partial), `rules_hit>=1` on a secret-bearing prompt, unknown-event `partial=true` + `unknown_event_type` warn, `validated=false` on a bad envelope, raw prompt/secret absent — own binary to avoid tracing callsite-cache races | `src/internal/ai/hooks/runtime.rs` |
| `agent_hook_crash_test` | 1 | AG-19 强制补强项 #10 crash regression (plan.md Task A4): SIGKILL before/mid stdin read, injected panic after read+validate (`LIBRA_TEST_HOOK_PANIC_AFTER_READ`), and SIGKILL racing a `stop` checkpoint write all leave no partial `agent_session`/`agent_checkpoint` state visible and never echo raw stdin | `src/internal/ai/hooks/runtime.rs` |
| `agent_enable_install_path_test` | 1 | AG-19 §765 install-path assertion (plan.md Task A4): `agent enable` embeds the canonical absolute binary path (OpenCode plugin `LIBRA_COMMAND`, Codex `<binary> hooks codex <verb>` handlers + 6 `[hooks.state]` trust entries), Codex trust-gap banner names one gap after hash tamper, disable removes only Libra-managed state | `src/internal/ai/hooks/providers/codex/settings.rs`, `src/internal/ai/hooks/providers/opencode/settings.rs` |
| `agent_checkpoint_export_test` | 1 | AG-20 E4-libra writer (plan.md Task A5): six-entry checkpoint tree with exact names (`transcript/<agent_kind>.jsonl` rename), manifest role/OID/byte-length agreement, `content_hash.txt` `sha256:<64hex>` format + recompute (reader tolerates bare hex), E5 line-safe chunking (single-file small / ordered `.jsonl.%03d` parts / oversize-line hard error, via `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD`), stage-(d) probe-first idempotent catalog insert, window A/B in-flight marker lifecycle + TTL expiry | `src/internal/ai/history.rs`, `src/internal/ai/hooks/runtime.rs` |
| `agent_checkpoint_span_test` | 1 | AG-20 `agent.checkpoint.write` span fake-sink assertion (plan.md Task A5): required fields (checkpoint_id/session_id/stage→done/cas_retries/object_count) present, transcript body + raw secret absent — own binary to avoid tracing callsite-cache races | `src/internal/ai/hooks/runtime.rs` |
| `agent_checkpoint_reader_test` | 1 | AG-20 reader slice (plan.md Task A5): keyset pagination for `session list`/`checkpoint list` (default 50, cap-500 clamp with stderr note, `--limit 0`→1, opaque `v1:<ts>:<id>` cursor, 120-row no-overlap/no-gap walk, malformed-cursor fail-closed), `checkpoint show` layout classification (E4-libra manifest roles + `content_hash` format check, legacy-v1 fixture fallback pinned to README OIDs, chunked parts in manifest order) and metadata-first discipline (deleted transcript blob → availability `missing`, never an error), plus EXPLAIN QUERY PLAN index-hit on the 2026070802 pagination indexes against a real `libra init` repo DB | `src/command/agent/checkpoint.rs`, `src/command/agent/session.rs`, `sql/migrations/2026070802_agent_checkpoint_paging.sql` |
| `agent_clean_span_test` | 1 | AG-20 `agent.clean.prune` span fake-sink assertion (plan.md Task A5): required fields (deleted_objects/deleted_sessions/window_guard/duration_ms) present with guards verified, raw repository path absent — own binary to avoid tracing callsite-cache races | `src/internal/ai/history.rs`, `src/command/agent/clean.rs` |
| `agent_doctor_repair_test` | 1 | AG-20 `agent doctor [--repair]` three-class detection/repair (plan.md Task A5): window-B row re-INSERT with key-field equality, stale row rebuilt from `refs/libra/traces`, genuinely missing objects manual-only (no destructive action) — incl. single missing E4 sidecar blob and missing `manifest.json` (never misread as legacy-v1, other sidecars still checked, healthy only after the object returns), `object_index` idempotent re-insert with writer row semantics for row-column OIDs AND sidecar blobs (`agent_transcript` tag preserved; o_size from manifest `byte_len` so transcript payloads are never read), stale row + missing index rows on one checkpoint fixed by a single `--repair` (class-3 targets ref-side OIDs), legacy-v1 fixture classified (never repaired, byte-identical after `--repair`), session-without-checkpoint never flagged, gemini uninstall-channel hint, `agent.doctor.repair` span fields via `LIBRA_LOG_FILE` (no transcript leak), all repairs no-op on second run | `src/command/agent/doctor.rs` |
| `agent_review_workflow_test` | 1 | AG-22 review workflow (plan.md Task A7): pinned scenarios — fake `/bin/sh` reviewers (fixtures + provenance README in `tests/fixtures/agent_workflows/`) cover success/error/cancel/slow-output, flooding reviewer never blocks the sink (64 KiB cap + truncation marker, quiet sibling output intact), E8 `manifest.json` exactly the 12 keys with empty `manual_attach` placeholder + spotlighting-delimited redacted `findings.md` + `reviewers/<slug>.{stdout,stderr}.redacted.log`, `review --fix` fails closed with `LBR-AGENT-010` (exit 128, JSON error surface), cancel marker kills the reviewer PID (kill -0 fails) and releases the workspace with idempotent second cancel — plus the plan.md:961 cancel-during-pending-output stress bound and the 强制补强项 #5 `review list --json --limit --cursor` keyset envelope (exact `{schema_version, items, next_cursor, has_more}`, no-dup/no-loss walk, `run_id DESC` tiebreak, malformed cursor fails closed at exit 129) through the real CLI | `src/internal/ai/review/`, `src/command/agent/review.rs` |
| `agent_review_span_test` | 1 | AG-22 `agent.review.run` span fake-sink assertion (plan.md Task A7 / agent.md §6 :1334): required fields (run_id/agent_count/terminal_state/duration_ms) present on close, reviewer stdout text absent from the sink while provably present in `findings.md` — own binary to avoid tracing callsite-cache races | `src/internal/ai/review/runner.rs` |
| `agent_investigate_workflow_test` | 1 | AG-23 investigate workflow (plan.md Task A8): pinned scenarios — fake `/bin/sh` investigators (fixtures + provenance README in `tests/fixtures/agent_workflows/`) drive STRICT round-robin to terminal `quorum` (agent order preserved, per-stance sequence) and `max_turns` (round-robin wraps a,b,a), stall→paused `stalled` + `pending_turn` (non-terminal) then `continue` resumes to terminal, non-zero investigator→paused `agent_failure`, cancel→terminal `cancelled` (workspace released, no worktree mutation), E8 `manifest.json` exactly the 12 keys with `kind="investigate"` + empty `manual_attach` + spotlighting-delimited redacted `findings.md` (seed topic persisted, fake `sk-` stance secret + ANSI scrubbed from findings/`*.redacted.log`), run-id flock makes a concurrent `continue` fail closed `RunLocked` (released→succeeds), `investigate fix` fails closed with `LBR-AGENT-010` (exit 128, JSON error surface, names read-only alternative), and the 强制补强项 #5 `investigate list --json --limit --cursor` keyset envelope (exact `{schema_version, items, next_cursor, has_more}`, no-dup/no-loss walk, `run_id DESC` tiebreak, malformed cursor fails closed at exit 129) through the real CLI | `src/internal/ai/investigate/`, `src/command/agent/investigate.rs` |
| `agent_investigate_span_test` | 1 | AG-23 `agent.investigate.run` span fake-sink assertion (plan.md Task A8 / agent.md §6 :1335): required fields (run_id/turn/next_agent_idx/terminal_state) present on close, the untrusted seed topic and investigator stdout text absent from the sink while provably present in `findings.md` — own binary to avoid tracing callsite-cache races | `src/internal/ai/investigate/runner.rs` |
| `compat_help_examples_banner` | 1 | Every visible command in `src/cli.rs::Commands` renders an `EXAMPLES:` / `Examples:` section in `<cmd> --help` (cross-cutting item B) | `src/cli.rs`, `src/command/**` |
| `compat_error_codes_doc_sync` | 1 | Every `LBR-*-NNN` literal in `src/utils/error.rs` is documented in `docs/error-codes.md` | `src/utils/error.rs`, `docs/error-codes.md` |
| `compat_command_docs_examples_section` | 1 | Every `docs/commands/<name>.md` page carries an `## Examples` / `## Common Commands` heading | `docs/commands/**` |
| `compat_help_flag_descriptions` | 1 | Every visible flag and positional under `Options:` / `Arguments:` carries a non-empty description; covers 42 root commands + 53 sub/sub-sub-commands (110 surfaces) | `src/cli.rs`, `src/command/**` |
| `compat_help_no_impl_meta_leak` | 1 | No `libra <cmd> --help` body leaks contributor-facing rustdoc into clap's long_about; forbids 6 phrase classes (e.g. `Codex pass-`, raw markdown headings, code fences) | `src/cli.rs`, `src/command/**` |
| `verify_pack_multi_test` | 1 | Guards `verify-pack <idx>...` multi-index verification, JSON wrapping, and `--pack` argument rejection | `src/command/verify_pack*.rs` |
| `db_migration_test` | 1 | SQLite schema bootstrap + migration round-trip | `src/internal/db.rs`, `sql/` |

## Wave 2 — Code UI & local automation

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `harness_self_test` | 2 | Smoke-checks the PTY harness itself | `tests/harness/` |
| `code_ui_scenarios` | 2 | End-to-end scenarios on the Code UI through the harness | `src/command/code.rs`, `src/internal/tui/` |
| `code_ui_remote_lease_matrix` | 2 | Browser/automation lease lifecycle matrix | `src/command/code.rs` controller, `src/command/code_control.rs` |
| `code_ui_remote_sse_matrix` | 2 | SSE event stream matrix from web view | `src/internal/tui/`, `src/command/code.rs` (axum) |
| `code_ui_remote_state_matrix` | 2 | Cross-surface state replication matrix, including mid-turn detach/cancel settling | `src/internal/tui/`, `src/internal/ai/web/code_ui.rs`, `src/command/code_control.rs` |
| `code_ui_remote_security_matrix` | 2 | Auth/token/origin enforcement matrix | `src/command/code_control*.rs` |
| `code_ui_remote_generation_matrix` | 2 | Generation control across surfaces (no live LLM) | `src/internal/tui/app.rs` |
| `code_ui_remote_approval_matrix` | 2 | Approval flow across TUI/Web/automation | `src/internal/ai/agent/` approvals |
| `code_cli_dispatch_test` | 2 | `libra code …` argv parsing & dispatch | `src/command/code.rs` |
| `code_provider_boot_test` | 2 | Provider/agent bootstrap inside `libra code` | `src/internal/ai/providers/`, `src/internal/ai/agent/` |
| `code_tool_acl_test` | 2 | Tool registry ACL & safety classification | `src/internal/ai/tools/` |
| `code_mcp_dual_entry_test` | 2 | MCP stdio + http dual entry parity | `src/internal/ai/mcp/`, `src/command/code.rs` |
| `code_resume_test` | 2 | Session resume across restarts | `src/internal/ai/session/`, `src/command/code.rs` |
| `code_codex_default_tui_test` | 2 | `--provider codex` routes through the default managed-runtime TUI (legacy stdin loop unreachable) | `src/command/code.rs`, `src/internal/ai/codex/`, `src/internal/tui/` |
| `code_codex_runtime_test` | 2 | `--provider codex` WS runtime boot: `--codex-port` validation, managed app-server initialize/thread-start, approval-interaction regression | `src/command/code.rs`, `src/internal/ai/codex/` |
| `ai_code_ui_headless_test` | 2 | Headless Code UI runtime and projection coverage | `src/internal/ai/web/headless.rs` |
| `ai_code_ui_projection_test` | 2 | Projection snapshot replication | `src/internal/ai/history.rs`, `src/internal/tui/` |
| `ai_code_ui_wire_test` | 2 | Wire-format contract for UI events | `src/internal/tui/`, `src/internal/ai/agent/` |
| `intent_flow_test` | 2 | IntentSpec → Plan → Run pipeline (no live LLM) | `src/internal/ai/intentspec/`, `src/internal/ai/orchestrator/` |
| `e2e_mcp_flow` | 2 | End-to-end MCP server flow | `src/internal/ai/mcp/` |
| `mcp_integration_test` | 2 | MCP integration tests | `src/internal/ai/mcp/` |
| `ai_automation_test` | 2 | `.libra/automations.toml` rule execution | `src/internal/ai/automation/`, `src/command/automation.rs` |
| `ai_dag_tool_loop_test` | 2 | DAG-based tool loop regression | `src/internal/ai/agent/` |
| `ai_mock_provider_test` | 2 | Mock provider used by `test-provider` feature | `src/internal/ai/providers/` (test-only) |
| `agent_capture_migration_test` | 2 | Capture/replay store migration | `src/internal/ai/history.rs` |
| `ai_agent_baseline_test` | 2 | Step 1.0 / CEX-00 single-agent baseline tests | `src/command/code.rs`, `src/internal/ai/agent/` |
| `ai_approval_ttl_test` | 2 | CEX-11 approval TTL and canonical key contract tests | `src/internal/ai/agent/` |
| `ai_classifier_test` | 2 | CEX-08 TaskIntent classifier contract tests | `src/internal/ai/completion/` |
| `ai_command_safety_test` | 2 | CEX-01 command safety contract tests | `src/internal/ai/commands/` |
| `ai_compaction_filter_test` | 2 | Integration tests for filter_compacted projection | `src/internal/ai/context_budget/` |
| `ai_compaction_handoff_e2e_test` | 2 | S5 compaction handoff end-to-end scenario | `src/internal/ai/context_budget/` |
| `ai_concurrency_lock_test` | 2 | Session-level advisory lock and CAS conflict tests | `src/command/code.rs`, `src/internal/ai/session/` |
| `ai_context_budget_test` | 2 | CEX-13a context budget core contract tests | `src/internal/ai/context_budget/` |
| `ai_context_compaction_prune_test` | 2 | S5 prune phase + budget-driven sequence tests | `src/internal/ai/context_budget/` |
| `ai_context_frame_test` | 2 | Context frame serialization and lifecycle | `src/internal/ai/context_budget/` |
| `ai_context_handoff_test` | 2 | S5 compaction handoff template parser tests | `src/internal/ai/context_budget/` |
| `ai_dagrs_081_spike_test` | 2 | Phase 0 spike for dagrs 0.8.1 API assumptions | `src/internal/ai/orchestrator/` |
| `ai_dynamic_prompt_test` | 2 | CEX-09 dynamic prompt and intent tool-policy tests | `src/internal/ai/prompt/` |
| `ai_file_undo_test` | 2 | CEX-10 file-level undo contract tests | `src/internal/ai/tools/` |
| `ai_goal_completion_gate_test` | 2 | OC-Phase 6 P6.7 completion gate scenarios | `src/internal/ai/goal/` |
| `ai_goal_flag_off_regression_test` | 2 | OC-Phase 6 Goal mode opt-in flag-off regression tests | `src/internal/ai/goal/` |
| `ai_goal_resume_test` | 2 | OC-Phase 6 Goal mode supervisor resume replay tests | `src/internal/ai/goal/` |
| `ai_goal_state_test` | 2 | OC-Phase 6 Goal mode schema integration tests | `src/internal/ai/goal/` |
| `ai_goal_supervisor_test` | 2 | OC-Phase 6 S6 supervisor non-completion E2E | `src/internal/ai/goal/` |
| `ai_goal_verifier_test` | 2 | OC-Phase 6 P6.2 deterministic GoalVerifier integration tests | `src/internal/ai/goal/` |
| `ai_hardening_contract_test` | 2 | Phase E hardening contract tests | `src/internal/ai/sandbox/` |
| `ai_json_repair_test` | 2 | JSON repair and correction parser tests | `src/internal/ai/completion/` |
| `ai_libra_vcs_safety_test` | 2 | CEX-02 run_libra_vcs parameter-level safety tests | `src/internal/ai/tools/` |
| `ai_memory_anchor_test` | 2 | Short-term/long-term memory anchor contract tests | `src/internal/ai/agent/` |
| `ai_multi_agent_e2e_test` | 2 | S7 multi-agent declarative config E2E | `src/internal/ai/agent/` |
| `ai_projection_resolver_test` | 2 | Phase B projection resolver and scheduler repository tests | `src/internal/ai/orchestrator/` |
| `ai_provider_context_overflow_compact_loop_test` | 2 | OC-Phase 4 context-overflow compaction loop integration tests | `src/internal/ai/providers/` |
| `ai_provider_error_taxonomy_test` | 2 | Integration fixtures for OC-Phase 4 provider error taxonomy | `src/internal/ai/providers/` |
| `ai_provider_retry_policy_test` | 2 | OC-Phase 4 retry-policy integration test | `src/internal/ai/providers/` |
| `ai_provider_transform_test` | 2 | Integration tests for OC-Phase 4 P4.1 provider transform pipeline | `src/internal/ai/providers/` |
| `ai_runtime_contract_test` | 2 | Wave 1A runtime contract tests pinning TaskExecutor | `src/internal/ai/runtime/` |
| `ai_scheduler_plan_set_test` | 2 | Phase 0 selected plan set and task dependency tests | `src/internal/ai/orchestrator/` |
| `ai_schema_migration_test` | 2 | Phase 0 schema migration tests for AI runtime contract tables | `src/internal/db.rs`, `sql/` |
| `ai_security_runtime_test` | 2 | Phase 5 security runtime (authz, redaction, shell, audit) | `src/internal/ai/sandbox/` |
| `ai_semantic_rust_test` | 2 | Semantic Rust code indexing and structure extraction | `src/internal/ai/skills/` |
| `ai_semantic_tools_test` | 2 | Semantic tools registration and classification | `src/internal/ai/tools/` |
| `ai_session_jsonl_test` | 2 | Session JSONL persistence format and event streaming | `src/internal/ai/session/` |
| `ai_skill_test` | 2 | System skills load, parse, and execution validation | `src/internal/ai/skills/` |
| `ai_source_pool_test` | 2 | CEX-14 source-pool isolation and MCP integration tests | `src/internal/ai/session/` |
| `ai_storage_flow_test` | 2 | Integration tests for AI object storage on local and R2 backends | `src/utils/storage/` |
| `ai_subagent_contract_test` | 2 | CEX-S2-10 schema contract tests | `src/internal/ai/agent_run/` |
| `ai_subagent_evidence_query_test` | 2 | CEX-S2-18 Step 2.8 read-only evidence query API: `evidence_query_by_scope` / `evidence_stream` (AND filter) / `merge_decision_distillable_evidence` over the frozen `AgentEvidence` / `MergeDecision` schema; empty-input → empty (flag-off analogue) | `src/internal/ai/agent_run/evidence_query.rs` |
| `ai_subagent_llm_initiated_test` | 3 | OC-Phase 3 LlmInitiated E2E: fake provider → dispatcher → `DefaultSubAgentChildRunner` → tool loop → parent JSONL `Spawned + Completed` | `src/internal/ai/agent/runtime/`, `src/internal/ai/providers/fake/`. Gated `--features test-provider`. |
| `ai_subagent_runtime_context_inheritance_test` | 3 | CEX-S2-12 / S2-INV-06 E2E: child tool invocation inherits the parent's `DispatchContext::runtime_context` (sandbox + approval + file-history authority + output budget) verbatim; a recording tool captures the invocation context, reverting the forward makes it observe `None` | `src/internal/ai/agent/runtime/sub_agent.rs`, `src/internal/ai/providers/fake/`. Gated `--features test-provider`. |
| `ai_subagent_user_initiated_test` | 3 | OC-Phase 3 UserInitiated{bypass_permission_ask:true} E2E: rejecting asker proves bypass really skips step 8; rest of the chain matches the LlmInitiated sibling | `src/internal/ai/agent/runtime/`, `src/internal/ai/providers/fake/`. Gated `--features test-provider`. |
| `ai_subagent_user_initiated_cancel_test` | 3 | OC-Phase 3 UserInitiated cancel E2E: pre-flight cancel short-circuits before JSONL writes; mid-flight parent abort returns `Cancelled { ParentAbort }`, parent JSONL writes `Spawned + Cancelled { UserRequested }`, and child JSONL replays to a cancelled snapshot | `src/internal/ai/agent/runtime/`. Gated `--features test-provider`. |
| `ai_subagent_worktree_readonly_test` | 3 | Sub-agent worktree isolation guard: pins historical edit-tool pre-filter and `libra code` workspace-isolation bootstrap wiring | `src/internal/ai/tools/registry.rs`, `src/internal/ai/permission/`, `src/command/code.rs` |
| `ai_usage_stats_test` | 2 | CEX-16 usage stats persistence and aggregation tests | `src/internal/ai/usage/` |
| `ai_usage_tui_test` | 2 | CEX-16 usage display formatting tests | `src/internal/ai/usage/` |
| `ai_validation_decision_flow_test` | 2 | Phase D validation and decision derived-record tests | `src/internal/ai/orchestrator/` |
| `diagnostics_redaction_test` | 2 | Diagnostics logs redaction and sanitization | `src/internal/ai/usage/` |
| `local_client_test` | 2 | Local Git protocol client working directory restoration on error | `src/internal/protocol/` |
| `publish_ai_export_test` | 2 | Publish pipeline export representation for AI tasks | `src/internal/publish/` |
| `publish_ai_object_model_contract_test` | 2 | Publish pipeline AI object model contract | `src/internal/publish/` |
| `publish_incremental_test` | 2 | Publish pipeline incremental sync and state tracking | `src/internal/publish/` |
| `publish_preflight_test` | 2 | Publish pipeline validation and preflight checks | `src/internal/publish/` |
| `publish_redaction_contract_test` | 2 | Publish pipeline redaction rules and scanning | `src/internal/publish/` |
| `publish_refs_test` | 2 | Publish pipeline references and branch tracking | `src/internal/publish/` |
| `publish_snapshot_test` | 2 | Publish pipeline snapshot generation and verification | `src/internal/publish/` |
| `publish_upload_test` | 2 | Publish pipeline bundle upload to cloud storage | `src/internal/publish/` |
| `publish_worker_template_embed_test` | 2 | Verification of embedded Worker template exclusion list | `src/internal/publish/` |
| `redaction_contract_test` | 2 | Pin the RedactedBytes contract for transcript output | `src/internal/ai/session/` |

## Wave 3 — network (test-network)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `network_remotes_test` | 3 | Real-network smoke tests against GitHub | `src/internal/protocol/`, `src/git_protocol.rs` |
| `protocol_timeout_recovery` | 3 | git:// connect/idle timeout recovery via a local hung/refused listener (self-contained) | `src/internal/protocol/git_client.rs` |
| `protocol_capability_negotiation` | 3 | Fetch want-line advertises only decoder-supported capabilities (ofs-delta yes; thin-pack/report-status no) | `src/internal/protocol/mod.rs` |

## Wave 4 — Live AI (test-live-ai / DEEPSEEK_API_KEY)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `ai_agent_test` | 4 | Live LLM agent loop smoke | `src/internal/ai/agent/`, `src/internal/ai/providers/` |
| `ai_chat_agent_test` | 4 | Live LLM chat-mode agent | `src/internal/ai/agent/` |
| `code_ui_remote_model_generation_matrix` | 4 | Live model generation matrix (ignored by default) | `src/internal/ai/providers/`, `src/internal/tui/` |
| `ai_ollama_live_gate_test` | 4 | Ollama live-gate smoke | `src/internal/ai/providers/ollama/` |

## Wave 5 — Live Cloud (test-live-cloud / D1+R2)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `cloud_storage_backup_test` | 5 | D1/R2 backup + restore round-trip | `src/command/cloud.rs`, `src/utils/d1_client.rs`, `src/utils/client_storage.rs` |
| `publish_live_test` | 5 | Publish pipeline against live R2 | `src/publish/`, `src/command/publish.rs` |
| `storage_r2_test` | 5 | Object store R2 path | `src/utils/client_storage.rs` |

## Wave 6 — Performance smoke (LIBRA_RUN_PERF=1)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `code_ui_perf_smoke_test` | 6 | Code UI perf / SSE soak smoke | `src/command/code.rs`, `src/internal/tui/`, `src/internal/ai/web/` |

---

## Wave 7 — Local agent capture smoke (LIBRA_RUN_LOCAL_AGENTS=1)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `agent_local_capture_smoke_test` | 7 | A6.5 first-batch hard gate: drives the real local `codex`/`claude`/`opencode` CLIs (one paid session each; `#[ignore]` + env-gate, serial) through hook install → capture → session/checkpoint/traces/doctor assertions → uninstall smoke; driver in `tests/harness/agent_local_capture.rs` | `src/command/agent/`, `src/command/hooks.rs`, `src/internal/ai/hooks/` |

---

## TODO — uncategorised (one-liner pass needed)

None. All currently known integration targets have a wave, purpose, and
relevant source entry above.

---

## Maintenance

- Every new `tests/<name>.rs` must add a row here in the same PR (enforced by
  §10 of `docs/development/integration/integration-test-plan.md`).
- Renames must update both this index and the plan; `compat_matrix_alignment`
  will fail CI on dangling references.
- TODO rows are tracked as `BASELINE_GAP-INTEG-007` — the index pass.
