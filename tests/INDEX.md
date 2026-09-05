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
| `operation_schema_v2` | 1 | OL-02 fresh/legacy schema convergence, idempotence, and rollback guards | `src/internal/db/migration.rs`, `sql/migrations/2026090101_operation_v2.sql` |
| `operation_dag` | 1 | OL-04 v2 operation, journal, and op-head CAS persistence | `src/internal/operation/store.rs` |
| `commit_change_id_header_spike` | 1 | OL-00 real-Git Change ID header vs sidecar-only compatibility spike | `docs/development/internal/operation-log-working-copy-change-id.md` |
| `command_test` | 1 | Top-level dispatcher covering most `libra <subcmd>` integration paths, including W4 `worktree doctor` read-only/schema, confirmed legacy-capture adoption, W4-08 linked-worktree `libra code`/`automation` enablement, and the W5-08 `graph_machine_survives_tui_removal` breaking guard (interactive graph entry refused with a migration hint; `--json`/`--machine` wire intact) | `src/command/`, `src/cli.rs`, `tests/command/worktree_doctor_test.rs`, `tests/command/code_agent_linked_guard_test.rs` |
| `compat_stash_subcommand_surface` | 1 | Guards `libra stash` subcommand surface vs. git CLI | `src/command/stash.rs` |
| `compat_bisect_subcommand_surface` | 1 | Guards `libra bisect` subcommand surface | `src/command/bisect.rs` |
| `compat_worktree_delete_dir` | 1 | Guards worktree delete semantics on dir removal | `src/command/worktree.rs` |
| `compat_checkout_alias_help` | 1 | Guards `--help` text for checkout aliases | `src/command/checkout.rs` |
| `compat_matrix_alignment` | 1 | Guards public docs/release matrices vs. real CLI/API surfaces, including `w203_revision_receipt_and_network_boundary_stay_aligned` for W2-03 digest-only receipts, the shared linear receipt index, permanent retry closure, typed revision errors, Phase 1 scanner/session-writer leases, and the Network-Allow 409 boundary | `COMPATIBILITY.md`, `CHANGELOG.md`, `docs/commands/code.md`, `docs/commands/zh-CN/code.md`, `docs/error-codes.md`, `docs/development/{commands/_compatibility,tracing/code,plan/plan-20260715}.md`, `src/internal/ai/{session/jsonl,workspace_snapshot}.rs`, `src/internal/ai/web/{headless,web_admission}.rs` |
| `compat_install_alias` | 1 | Guards IX-01 full-installer `lba -> libra` creation, same-version repair/idempotency, CLI/env opt-outs, foreign-path preservation, and symlink-unavailable fallback with an isolated fake downloader | `install.sh`, `tests/compat/install_alias_smoke.sh`, `README.md`, `README.zh-CN.md` |
| `compat_live_compat_workflow` | 1 | Guards optional live AI/cloud workflow remains manual/scheduled and secret-gated | `.github/workflows/live-compat.yml` |
| `compat_release_rclone_env_guard` | 1 | Guards release workflow never exports bare `RCLONE_*` option variables in YAML, POSIX shell, or PowerShell assignments, while permitting only the configured `RCLONE_CONFIG_R2_*` remote namespace | `.github/workflows/release.yml` |
| `compat_branch_lossy_wrapper_guard` | 1 | Guards branch-name lossy conversion wrapper | `src/internal/branch.rs` |
| `compat_lfs_client_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `internal/protocol/lfs_client.rs` | `src/internal/protocol/lfs_client.rs` |
| `media_fastcdc_test` | 1 / manual interop | Feature-gated chunk/cache/verify/probe tests; ignored `mega_fastcdc_http_interop` exercises real Mega HTTP upload, dedup, resume and download (requires `MEGA_FASTCDC_READY_FILE`) | `src/utils/media/`, `src/internal/protocol/lfs_client.rs` |
| `compat_config_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `internal/config.rs` | `src/internal/config.rs` |
| `compat_head_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `internal/head.rs` | `src/internal/head.rs` |
| `compat_util_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `common_utils.rs` / `utils/` | `src/common_utils.rs`, `src/utils/` |
| `compat_client_storage_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in `utils/client_storage.rs` | `src/utils/client_storage.rs` |
| `compat_extra_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in miscellaneous modules | `src/**` |
| `compat_all_production_unwrap_guard` | 1 | Bans `unwrap()/expect()` in general production codebase | `src/**` |
| `compat_agent_run_non_exhaustive_guard` | 1 | Enforces `#[non_exhaustive]` on every `pub enum` under `agent_run/` for additive evolution | `src/internal/ai/agent_run/` |
| `compat_agent_docs_contract` | 1 | Guards active Agent plan claims against stale removed-provider status, public schema/retention/raw-export wording, and stale internal-plan links | `docs/development/tracing/agent.md`, `src/command/code.rs` |
| `compat_agent_capability_matrix_pin` | 1 | Pins the E1 8-bool `DeclaredAgentCaps` wire keys and the first-batch supported roster (`claude-code`/`codex`/`opencode`) against drift (AG-16) | `src/internal/ai/observed_agents/{capability,registry}.rs`, `docs/development/tracing/agent.md` |
| `compat_agent_architecture_guard` | 1 | Observed-agents capture layer stays decoupled from AgentRuntime/checkpoint layers; every `AgentKind` resolves an adapter; external agents need the AG-18 info/trust flow; SQL CHECK / doc roster / enum stay in sync (AG-16); W0-01’s C1–C10/A0 runtime-anchor audit and W0-03 Web-only completion gate prevent a premature TUI removal; W5-10 forbids restoring terminal-UI dependencies or production symbols | `src/internal/ai/observed_agents/`, `src/internal/ai/runtime/`, `src/{command,internal}/`, `Cargo.toml`, `sql/migrations/2026050303_agent_capture.sql`, `docs/development/tracing/{agent,code}.md` |
| `compat_subface_labels` | 1 | Guards the CG-01 sub-face grading matrix: fixed five-label enumeration, graded command set pinned to `src/cli.rs::Commands`, no dual-tier sub-face, and unsupported sub-faces carrying governance IDs synced bidirectionally with `_compatibility.md` | `COMPATIBILITY.md`, `docs/development/commands/_compatibility.md`, `src/cli.rs` |
| `compat_conflict_status_diff` | 1 | Guards unmerged conflict reporting across merge/rebase/cherry-pick: status v1/v2, ls-files stages/tags, and conflict-aware diff headers | `src/command/status.rs`, `src/command/diff.rs`, `src/command/ls_files.rs`, `src/command/merge.rs` |
| `compat_diff_check_safety` | 1 | Guards `diff --check` safety classes: trailing whitespace, space-before-tab in indent, leftover conflict markers, and new blank lines at EOF with exit code 2 | `src/command/diff.rs` |
| `compat_diff_review_options` | 1 | Guards P1-08a/b/c script/review metadata, pickaxe, and algorithm selection: raw A/D/M/R/T records and NUL fields, worktree zero IDs, mode-only summary/external-driver metadata, executable unified/full-index mode headers, mode retention through textconv/whitespace suppression, same-file-type rename pairing, compact stat labels, diff-filter include/exclude/all-or-none plus sparse-view projection, full-index IDs, CLI src/dst prefixes, per-file `-S` count and `-G` hunk-line matching, textconv reuse, external-driver prefilter, invalid-regex diagnostics, truthful Myers default, MyersMinimal/Patience/Histogram named/shorthand parity, anchored Patience uniqueness/prefix/selector-retention semantics, and propagation through whitespace/blank, textconv, and rename bodies | `src/command/diff.rs`, `src/command/diff/options.rs` |
| `compat_rev_parse_peel_selectors` | 1 | Guards strict typed/recursive object peel, tree paths, `@`, numeric reflog selectors, full tag refs, annotated-tag branch filtering/show-ref dereference, and SHA-256 hash-kind neutrality | `src/utils/util.rs`, `src/command/rev_parse.rs`, `src/command/cat_file.rs`, `src/command/read_tree.rs`, `src/internal/reflog.rs` |
| `compat_libra_hooks_lifecycle` | 1 | Guards sandboxed `.libra/hooks` lifecycle ordering/argv/stdin, commit and merge message mutation, advisory order/warning exit, no-op suppression, escape valves, deterministic resolution, caller-env secret stripping, fail-closed file validation, protected-mount cleanup, metadata protection, worktree-bound writes, and the PD-08 bridge-inert pin (`.git/hooks`/`core.hooksPath`/`hooks.gitCompatibility` never execute) | `src/internal/repo_hooks.rs`, `src/internal/ai/sandbox/`, `src/command/commit.rs`, `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/merge.rs`, `src/command/rebase.rs`, `src/command/pull.rs` |
| `compat_import_export_roundtrip` | 1 | Guards P1-11 fast-export/import fidelity (ranges, multiple refs, annotated tags, notes/tree translation, quoted paths, inline/C/R/N, reset deletion, type/config/transaction failure), bundle selectors/checksum/idempotent unbundle, SHA-256, and bidirectional system-Git interoperability when available | `src/command/fast_export.rs`, `src/command/fast_import.rs`, `src/command/bundle.rs` |
| `compat_status_args_parser_compile` | 1 | plan-20260714 R0-4: public `StatusArgs` external source compatibility — `try_parse_from` + struct literal compile, `-z` and `--null` both accepted | `src/command/status.rs` |
| `compat_legacy_short_api_pre_r0_equivalence` | 1 | plan-20260714 R0-6: the legacy `generate_short_format_status` tuple API keeps its pre-R0 rename decomposition (D/A endpoints, merged chain, `??` destinations) while `generate_short_status_entries` carries renames first-class | `src/command/status.rs` |
| `compat_status_wave0_register` | 1 | plan-20260714 B.9 registration gate: `STATUS_WAVE0_TESTS` manifest ↔ the module's `#[test]`/`#[tokio::test]` declarations (syn-parsed source, comment-proof, fails closed on `cfg_attr`/unknown attributes/item macros/nested modules) in bidirectional set equality + strict ordering, so the wave-0 status module cannot be silently dropped from CI; `#[cfg(unix)]` cases are inventoried by the `STATUS_WAVE0_TESTS_UNIX_ONLY` subset (source parsing is cfg-independent, so no platform filtering) | `tests/command/status_wave0_test.rs`, `tests/compat/status_wave0_manifest.rs` |
| `compat_clone_shallow_integrity` | 1 | Guards local Libra `clone/fetch --depth` fail-closed behavior and `rev-parse --is-shallow-repository` shallow metadata reporting | `src/command/clone.rs`, `src/command/fetch.rs`, `src/command/rev_parse.rs` |
| `compat_checkout_branch_startpoint` | 1 | Guards `checkout -b/-B <branch> <start-point>` and `switch -C <branch> <start-point>` keep HEAD on the symbolic branch and preserve HEAD on invalid start-points | `src/command/checkout.rs`, `src/command/switch.rs` |
| `compat_previous_branch_shortcut` | 1 | Guards worktree-scoped `checkout -` / `switch -` branch and detached-target toggling, cross-command reflog history, and fail-closed missing/deleted targets | `src/command/checkout.rs`, `src/command/switch.rs`, `src/internal/reflog.rs` |
| `compat_mail_am_basic` | 1 | Guards P2-01 plain-mail replay, metadata preservation, add/delete, SHA-256, continue/skip/abort, pristine-window resume, write interruption rollback, tip/path/ignored-target safety, JSON/help, and executable preservation | `src/command/am.rs`, `src/command/apply.rs`, `src/internal/sequencer/mod.rs` |
| `compat_mailinfo_basic` | 1 | Guards P2-02 bounded stdin mail parsing, Git-shaped metadata, shared subject/author/transfer cleanup, body/patch extraction, JSON/quiet output, output-alias and fail-closed overwrite safety, and no-repository use | `src/command/mailinfo.rs`, `src/command/am.rs` |
| `compat_format_patch_mail_roundtrip` | 1 | Guards P2-03 `-1`/`--root`, strict `format.*` defaults and CLI precedence, cover threading/in-reply-to, MIME boundary consumption, full-index/algorithm/prefix controls, upstream patch-id suppression, and Git↔Libra am round trips | `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/am.rs` |
| `compat_switch_orphan_root` | 1 | Guards `switch --orphan` / `checkout --orphan` leave HEAD on an unborn branch, preserve the index/worktree, report JSON `unborn`, and make the first user commit a root commit | `src/command/switch.rs`, `src/command/checkout.rs`, `src/command/commit.rs` |
| `compat_broken_pipe_output` | 1 | Guards high-output stdout commands, including `format-patch --stdout`, treat downstream BrokenPipe as quiet normal termination with no panic/backtrace noise | `src/main.rs`, `src/utils/output.rs`, `src/command/ls_files.rs`, `src/command/format_patch.rs` |
| `compat_commit_amend_no_edit` | 1 | Guards clean `commit --amend --no-edit` rewrites HEAD, preserves tree/parents/message, and refreshes committer date instead of reporting success for an unchanged ref | `src/command/commit.rs` |
| `compat_commit_identity_date` | 1 | Guards `commit` honors Git identity/date env overrides, `--date`, `--reset-author`, and `-C/-c` author metadata reuse | `src/command/commit.rs` |
| `compat_sequencer_message_author` | 1 | Guards `cherry-pick` preserves original author metadata, `revert` uses current identity, and signed commit subjects are de-signed before generated messages | `src/command/cherry_pick.rs`, `src/command/revert.rs`, `src/command/commit.rs` |
| `compat_write_tree_missing_object` | 1 | Guards `write-tree` and `commit` reject missing or mistyped index objects with `LBR-REPO-002` before writing tree/commit objects | `src/internal/tree_plumbing.rs`, `src/command/write_tree.rs`, `src/command/commit.rs` |
| `compat_init_shared_mode` | 1 | Guards `init --shared=<numeric>` prevalidates traversable directory permissions without leaving partial repos, and persists `core.sharedRepository` for shared modes | `src/command/init.rs` |
| `compat_symlink_basic` | 1 | Guards symlink mode `120000` staging, reset pathspec mode preservation, checkout/restore/reset materialization as real symlinks, and status/diff/ls-files detection of symlink target changes | `src/command/add.rs`, `src/command/restore.rs`, `src/command/reset.rs`, `src/command/status.rs`, `src/command/diff.rs`, `src/command/ls_files.rs` |
| `compat_global_config_schema_future` | 1 | Guards too-new global config DB schema fail-closed behavior for remote/cloud commands, explicit offline/local downgrade warnings, env/local-satisfied storage config, JSON `LBR-CONFIG-001`, and secret-free diagnostics | `src/cli.rs`, `src/utils/client_storage.rs`, `src/utils/error.rs` |
| `compat_pathspec_magic` | 1 | Guards shared pathspec magic parsing for `top`/`exclude`/`icase`/`literal`/`glob`, subdirectory-relative semantics, and read-only consumers `ls-files`/`grep`/`diff`/`status` | `src/utils/pathspec/`, `src/command/ls_files.rs`, `src/command/grep.rs`, `src/command/diff.rs`, `src/command/status.rs` |
| `compat_ignore_attributes_sources` | 1 | Guards Git standard ignore/attributes sources plus Libra extension precedence across status/add/clean/check-ignore/check-attr/LFS/diff/archive | `src/utils/util.rs`, `src/utils/attributes.rs`, `src/command/check_ignore.rs`, `src/command/check_attr.rs`, `src/command/diff.rs`, `src/command/archive.rs` |
| `compat_machine_porcelain_contract` | 1 | Guards machine-readable porcelain contracts for `status -z`, default `diff` excluding untracked files, `ls-files --error-unmatch` exit 1, and `grep` 0/1/2 exit codes | `src/command/status.rs`, `src/command/diff.rs`, `src/command/ls_files.rs`, `src/command/grep.rs` |
| `compat_pretty_format_placeholders` | 1 | Guards Git-like pretty-format placeholders across `log`, `show`, and `shortlog` (including ASCII/control `%xNN`, `%%`, and forced color), plus `log -z` name-only/name-status separators | `src/internal/log/formatter.rs`, `src/command/log.rs`, `src/command/show.rs`, `src/command/shortlog.rs` |
| `compat_config_defaults_semantics` | 1 | Guards high-impact Git config defaults across local/global/system scopes, case-insensitive variables, empty/invalid fail-closed values, real `init.defaultBranch`/pull rebase behavior, CLI overrides, `pull.ff=true|false|only`, `fetch.prune`/`remote.<name>.prune` (remote-key-first precedence across scopes, Git numeric booleans, `--all` pre-network validation), the `status.*` display defaults (untracked modes, short/branch shaping the human short format only, showStash, relativePaths; porcelain config-immune; validated before any output), and `branch.sort`/`tag.sort` (flag wins; branch config neither implies --list nor hides the unborn-HEAD line — both tested; tag config never flips creation into listing; unset tags list refname-ascending; repeated values collapse to the last of the winning scope; unreadable config store fails LBR-IO-001 before listing), plus `diff.context`/`diff.renames` (Git `int` range and suffixes, default-true rename detection, strict cascade, flag wins, real `copies`/`copy` degradation, stable errors before progress/output) and `diff.noPrefix`/`diff.mnemonicPrefix`/`diff.srcPrefix`/`diff.dstPrefix` (strict cascade and boolean validation, Git precedence, all mnemonic pairs, reverse/staged/relative/rename/plumbing behavior, binary `/dev/null`, CRLF and word-diff content isolation, fatal local/global read failures before output with system-scope skips); `format.pretty`/`log.date`/`log.follow` (log/show CLI precedence, strict errors, single-path human+JSON follow, subdirectory normalization, exact-blob rename traversal); and `commit.status` (default true, strict cascade and Git booleans, explicit CLI/non-editor/non-stripping bypass, config failures before auto-stage, collection failures before hook/editor/history including symlink/non-file stash refs, dry-run isolated-index/no-object behavior; preview side-effect suppression; streamed non-verbose hashing; symlink-safe real/preview auto-stage including dangling and LFS-pattern paths; pre-read changed HEAD/staged/auto-stage byte+count budgets including CLI aggregate/count rejection; linked-worktree-shared scratch quota/scavenging; exact streamed loose validation; complete bounded delta-chain charging; one-enumeration/one-open-per-index batched pack preflight without index rebuild and with early aggregate-budget termination; real-auto-stage object-valid regular/LFS retention after collection failure; contextual LFS atomic-persist failure with unchanged index) | `src/command/init.rs`, `src/command/pull.rs`, `src/command/fetch.rs`, `src/command/status.rs`, `src/command/branch.rs`, `src/command/tag.rs`, `src/command/diff.rs`, `src/command/log.rs`, `src/command/log/config.rs`, `src/command/show.rs`, `src/command/commit.rs`, `src/command/commit/config.rs`, `src/utils/atomic_stream.rs`, `src/utils/preview_object.rs`, `src/utils/preview_scratch.rs`, `src/utils/storage/local.rs`, `src/utils/storage/load_cost/*`, `src/internal/config.rs` |
| `compat_config_defaults_edge_cases` | 1 | Guards encrypted local/global defaults, unreadable/unsupported system-scope skip, Git conversion source-HEAD reporting, and encrypted default decryption | `src/command/init.rs`, `src/internal/config.rs` |
| `compat_config_history_defaults` | 1 | Guards `merge.ff`, `merge.log`, `merge.verifySignatures`, and `commit.gpgSign` history-changing defaults plus CLI override precedence | `src/command/merge.rs`, `src/command/commit.rs`, `src/internal/config.rs` |
| `compat_fetch_remote_refspec` | 1 | Guards explicit/configured fetch refspec destination mapping, FETCH_HEAD/remote HEAD metadata, remotes.default selection, atomic multi-ref rollback, remote rename namespace migration, and ls-remote --symref output | `src/command/fetch.rs`, `src/command/remote.rs`, `src/command/ls_remote.rs`, `src/internal/config.rs` |
| `compat_noninteractive_history_controls` | 1 | Guards P1-07a rebase controls, P1-07b merge controls, and P1-07c hunk-level cherry-pick/revert `-X`, revert cleanup persistence/fail-closed recovery, guarded reset merge/keep staged/unstaged preservation and refusal, untracked collisions, file/directory transitions, no-follow symlink safety, and rollback contracts (33 E2E cases on Unix) | `src/command/rebase.rs`, `src/command/merge.rs`, `src/command/merge_message.rs`, `src/command/cherry_pick.rs`, `src/command/revert.rs`, `src/command/reset.rs`, `src/command/stash.rs`, `src/command/maintenance.rs` |
| `agent_rpc_external_test` | 1 | AG-18 external `libra-agent-*` protocol v2 + security: info/v1 negotiation, protocol-version fail-closed, capability gate, timeout/oversize caps, stderr capture/cap/redaction, env_clear allowlist, built-in slug impersonation skip | `src/internal/ai/observed_agents/rpc.rs` |
| `agent_rpc_span_test` | 1 | `agent.rpc.invoke` span fake-sink assertion (required fields present, raw response absent) — own binary to avoid tracing callsite-cache races | `src/internal/ai/observed_agents/rpc.rs` |
| `agent_bridge_protocol_test` | 1 | plan-20260818 LB-01 bridge protocol v1: 20-method allowlist, v1 limits (256 KiB frame, 64 in-flight, 64-event batch, page 100), initialize/major-mismatch/unknown-method/parse rejection, stable LBR-AGENT-024..036 catalogue | `src/internal/ai/agent_bridge/protocol.rs` |
| `agent_bridge_stdio_test` | 1 | plan-20260818 LB-01 bridge NDJSON transport: stdout carries only parseable frames (GC-LB-04), errors never masquerade as success (ER-LB-09), oversized frame refused and connection survives | `src/internal/ai/agent_bridge/transport.rs` |
| `agent_bridge_migration_test` | 1 | plan-20260818 LB-02 bridge migration contract: fresh bootstrap, old→new upgrade, repeated apply, forward-only down/freeze (refuses while bridge rows exist, never deletes acked events), source/payload CHECK enforcement | `sql/migrations/2026081801_agent_bridge_capture{,_down}.sql`, `src/internal/db/migration.rs` |
| `agent_bridge_schema_test` | 1 | plan-20260818 LB-02 bridge storage semantics: idempotent session open, event append accepted/duplicate/digest-conflict, oversize rejection, operation idempotency + digest conflict, monotonic ack | `src/internal/ai/agent_bridge/storage.rs` |
| `agent_bridge_ingress_test` | 1 | plan-20260818 LB-03 session/event ingress: idempotent `session.open` scope-bound, `event.append` batch ack with duplicate/digest-conflict/oversize handling, `session.flush`/`close` durable, evidence/provenance link recording, v1 caps fail-closed | `src/internal/ai/agent_bridge/{ingress,storage}.rs` |
| `agent_bridge_crash_test` | 1 | plan-20260818 LB-03 crash-after-write-before-ack: durable events survive, reconnect replay is a no-op (no duplicate rows), ack cursor advances exactly to contiguous end; gap stops the cursor | `src/internal/ai/agent_bridge/storage.rs` |
| `agent_bridge_redaction_test` | 1 | plan-20260818 LB-03 server-side redaction fail-closed: secret-bearing payload persisted redacted (no raw token), non-JSON payload refused (no raw fallback, no ack), valid projection persists | `src/internal/ai/agent_bridge/redaction.rs` |
| `agent_bridge_span_test` | 1 | plan-20260818 LB-03 per-request tracing span (GC-LB-10): `agent.bridge.request` carries method/id/repository scope, never the raw payload (GC-LB-08); span is emitted and closed on both success and error paths | `src/command/agent/bridge.rs` |
| `agent_bridge_read_methods_test` | 1 | plan-20260818 LB-04 typed read methods: `context.get`/`status.get`/`history.search`/`checkpoint.list`/`checkpoint.show` over the bridge projection, unified `{schema_version,repository_id,workspace_id,operation_id,status,data,warnings}` envelope, bounded pagination, repository-scoped queries, missing-object error (not empty) | `src/internal/ai/agent_bridge/methods.rs` |
| `agent_bridge_security_test` | 1 | plan-20260818 LB-04/GC-LB-03/06 security: read-method registry rejects unknown/low-level/SQL/shell methods, search term is parameterized (no SQL injection), read results never leak raw secrets | `src/internal/ai/agent_bridge/{methods,protocol}.rs` |
| `agent_bridge_workspace_test` | 1 | plan-20260818 LB-06 workspace lease binding: `workspace.claim`/`renew`/`release` route through `WorkspaceStore` (owner+fence), actor-derived owner, stale-fence and missing-session fail closed, self-reported owner never honoured | `src/internal/ai/agent_bridge/workspace.rs`, `src/internal/workspace.rs` |
| `agent_bridge_mutation_test` | 1 | plan-20260818 LB-05 mutations: operation-id mandatory + idempotent, digest conflict fails closed, actor spoof rejected, `checkpoint.create` real, VCS side-effect mutations validate approval and typed params before touching the repository, `checkpoint.restore` requires an explicit head fence | `src/internal/ai/agent_bridge/{mutations,authorization,provenance}.rs` |
| `agent_bridge_approval_test` | 1 | plan-20260818 LB-05/成功定义+ADR-LB approval gate: dangerous actions default deny, approved passes admission, denied fails, malformed approval invalid-params, actor binding rejects spoof | `src/internal/ai/agent_bridge/authorization.rs` |
| `agent_bridge_checkpoint_test` | 1 | plan-20260818 LB-05 checkpoint round-trip: create via mutation registry then list/show via read registry; missing session fails closed; checkpoint reads are repository-scoped (a repo-2 context never sees repo-1's checkpoints) | `src/internal/ai/agent_bridge/{mutations,methods}.rs` |
| `agent_bridge_vcs_test` | 1 | plan-20260818 LB-04/LB-05 real VCS wiring against a temp repository: `diff.get` returns the actual worktree/staged patch and is path-scoped, untyped selectors refused; `commit.create` commits the index, moves HEAD, and replays without a second commit; head-drift and dirty-worktree fences refuse before any write (`LBR-AGENT-038`); `checkpoint.restore` restores the worktree without moving HEAD; `review.run` refuses unlaunchable reviewers with no run residue | `src/internal/ai/agent_bridge/{vcs,methods,mutations}.rs`, `src/command/diff.rs` |
| `agent_transcript_intelligence_test` | 1 | AG-21 transcript intelligence: first-batch adapters extract prompts/model/tokens/modified-files/subagent totals/skill events from fixtures (provenance manifest in tests/fixtures/agent_transcripts/MANIFEST.md); E6 wire-key mapping pinned; fail-open partial semantics | `src/internal/ai/observed_agents/{extract.rs,builtin/}` |
| `agent_audit_log_test` | 3 | AG-24a compliance: append-only `agent_audit_log` enforcement (UPDATE/DELETE rejected by triggers, INSERT/SELECT allowed, denials recorded); retention-default constants pinned | `sql/migrations/2026070803_agent_audit_log.sql`, `src/internal/ai/observed_agents/compliance.rs` |
| `agent_lifecycle_event_test` | 1 | AG-19 central hook dispatcher contract (plan.md Task A4): invalid-envelope rejection without stdin echo, first-writer-wins owner filtering (SessionStart exempt), unknown-event skip-and-log, verb/kind mismatch fail-closed — via `libra agent hooks <agent> <verb>` E2E | `src/internal/ai/hooks/runtime.rs` |
| `agent_coverage_gate_test` | 1 | plan-20260713 DR-05c-0 live coverage gate: repeated TurnEnd no-op, incomplete→complete revision advance (both checkpoints stay visible), concurrent writers single append, foreign in-flight reservation returns a replayable error instead of silent success, gate-unavailable fail-closed — `libra agent hooks claude-code stop` E2E + direct claim/revision table reads | `src/internal/ai/coverage_gate.rs`, `src/internal/ai/observed_agents/coverage.rs` |
| `agent_import_test` | 1 | plan-20260713 M4 consented historical import: repository ownership and ambiguous-cwd rejection, provider-specific explicit-session grammar/auto-detection, consent-before-read, descriptor-relative file/project-directory symlink refusal, configured transcript-cap enforcement + hard-cap diagnostics, idempotent coverage claims/checkpoints, failed-read cumulative byte charging, hard deadline/batched-reservation cleanup, marker/tombstone interleavings, crash-after-object lease takeover/resume, provisional-session rollback, partial-result accounting, typed allowlist secret exclusion, frozen list schema v1 plus explicit v2 methods | `src/command/agent/import.rs`, `src/internal/ai/agent_import.rs`, `src/internal/ai/observed_agents/transcript_source.rs`, `src/command/agent/list.rs` |
| `agent_subagent_content_test` | 1 | plan-20260713 M5 DR-06 Claude subagent disk content E2E: provider-root-relative source identity, malformed-line partial capture, independent unresolved content checkpoint, repeat-discovery single current leaf, deadline-killed durability probes, uncataloged traces ancestry, and existing-corrupt-object rejection for supplied cloud catalogs | `src/internal/ai/subagent_content.rs`, `src/internal/ai/hooks/runtime.rs`, `src/internal/ai/history.rs`, `sql/migrations/2026071406_agent_subagent_content.sql` |
| `agent_graph_test` | 1 | plan-20260713 M6 DR-07 read-only capture graph: indexed revision history, shared checkpoint visibility, legacy `unindexed`, resolved/unresolved subagents, erased/unknown distinction, `--repo` preflight, JSON/machine schema, privacy allowlist, W5-08 interactive-entry refusal with migration hint, and zero capture/import/export mutations | `src/command/agent/graph.rs`, `src/command/agent/mod.rs`, `src/cli.rs` |
| `agent_live_gate_test` | 3 | plan-20260713 live agent gate (feature `test-live-agent` + env `LIBRA_RUN_LIVE_AGENT_GATE=1`): real by-id lookups against ~/.claude and ~/.codex, real Required-bwrap OpenCode export, fail-closed M4 three-provider historical import, M5 real Claude subagent-file content/replay/unresolved attribution plus real Codex native boundary evidence, and M6 real capture graph JSON/privacy/non-TTY/zero-write validation; ordinary non-gated runs may skip, but gated provider absence fails | `src/internal/ai/{agent_import,subagent_content}.rs`, `src/internal/ai/observed_agents/{builtin,opencode_export.rs}`, `src/command/agent/{import,graph}.rs` |
| `agent_opencode_bridge_test` | 1 | plan-20260713 DR-04b / plan-20260830 SBX-03/04 OpenCode export-bridge e2e: bwrap-gated CLI hook path plus Darwin-non-skip `opencode_export_seatbelt_fake_exporter` (sandbox-exec gate, not bwrap skip) | `src/internal/ai/observed_agents/opencode_export.rs`, `src/internal/ai/export_job.rs`, `src/internal/ai/hooks/runtime.rs`, `src/internal/ai/sandbox/` |
| `agent_checkpoint_redaction_test` | 1 | AG-19 redaction-before-persist (plan.md Task A4): prompt and tool_response secrets scrubbed before the `agent_session` row lands, `redaction_report` records the rule hits, token absent from all `agent session` CLI JSON | `src/internal/ai/hooks/runtime.rs` |
| `agent_hook_span_test` | 1 | AG-19 `agent.hook.ingest` / `agent.redaction.apply` span fake-sink assertion (plan.md Task A4): required fields present (provider/verb/event_kind/frame_bytes/validated/partial), `rules_hit>=1` on a secret-bearing prompt, unknown-event `partial=true` + `unknown_event_type` warn, `validated=false` on a bad envelope, raw prompt/secret absent — own binary to avoid tracing callsite-cache races | `src/internal/ai/hooks/runtime.rs` |
| `agent_hook_crash_test` | 1 | AG-19 强制补强项 #10 crash regression (plan.md Task A4): SIGKILL before/mid stdin read, injected panic after read+validate (`LIBRA_TEST_HOOK_PANIC_AFTER_READ`), and SIGKILL racing a `stop` checkpoint write all leave no partial `agent_session`/`agent_checkpoint` state visible and never echo raw stdin | `src/internal/ai/hooks/runtime.rs` |
| `agent_enable_install_path_test` | 1 | AG-19 §765 install-path assertion (plan.md Task A4): `agent enable` embeds the canonical absolute binary path (OpenCode plugin `LIBRA_COMMAND`, Codex `<binary> hooks codex <verb>` handlers + 6 `[hooks.state]` trust entries), Codex trust-gap banner names one gap after hash tamper, disable removes only Libra-managed state | `src/internal/ai/hooks/providers/codex/settings.rs`, `src/internal/ai/hooks/providers/opencode/settings.rs` |
| `agent_checkpoint_export_test` | 1 | AG-20 E4-libra writer (plan.md Task A5): six-entry checkpoint tree with exact names (`transcript/<agent_kind>.jsonl` rename), manifest role/OID/byte-length agreement, `content_hash.txt` `sha256:<64hex>` format + recompute (reader tolerates bare hex), E5 line-safe chunking (single-file small / ordered `.jsonl.%03d` parts / oversize-line hard error, via `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD`), stage-(d) probe-first idempotent catalog insert, window A/B in-flight marker lifecycle + TTL expiry | `src/internal/ai/history.rs`, `src/internal/ai/hooks/runtime.rs` |
| `agent_checkpoint_span_test` | 1 | AG-20 `agent.checkpoint.write` span fake-sink assertion (plan.md Task A5): required fields (checkpoint_id/session_id/stage→done/cas_retries/object_count) present, transcript body + raw secret absent — own binary to avoid tracing callsite-cache races | `src/internal/ai/hooks/runtime.rs` |
| `agent_checkpoint_reader_test` | 1 | AG-20 reader slice (plan.md Task A5): keyset pagination for `session list`/`checkpoint list` (default 50, cap-500 clamp with stderr note, `--limit 0`→1, opaque `v1:<ts>:<id>` cursor, 120-row no-overlap/no-gap walk, malformed-cursor fail-closed), `checkpoint show` layout classification (E4-libra manifest roles + `content_hash` format check, legacy-v1 fixture fallback pinned to README OIDs, chunked parts in manifest order) and metadata-first discipline (deleted transcript blob → availability `missing`, never an error), plus EXPLAIN QUERY PLAN index-hit on the 2026070802 pagination indexes against a real `libra init` repo DB | `src/command/agent/checkpoint.rs`, `src/command/agent/session.rs`, `sql/migrations/2026070802_agent_checkpoint_paging.sql` |
| `agent_clean_span_test` | 1 | AG-20 `agent.clean.prune` span fake-sink assertion (plan.md Task A5): required fields (deleted_objects/deleted_sessions/window_guard/duration_ms) present with guards verified, raw repository path absent — own binary to avoid tracing callsite-cache races | `src/internal/ai/history.rs`, `src/command/agent/clean.rs` |
| `agent_doctor_repair_test` | 1 | AG-20 `agent doctor [--repair]` checkpoint/marker detection and repair (plan.md Task A5): window-B row re-INSERT with key-field equality, stale row rebuilt from `refs/libra/traces`, genuinely missing objects manual-only (no destructive action), expired valid in-flight marker root-fenced ownership retirement, malformed marker manual-required, full E4/object-index semantics, stale row + missing index fixed in one run, legacy-v1 exemption, session-without-checkpoint legality, gemini uninstall hint, repair span fields/no transcript leak, idempotent second run | `src/command/agent/doctor.rs` |
| `agent_review_workflow_test` | 1 | AG-22 review workflow (plan.md Task A7): pinned scenarios — fake `/bin/sh` reviewers (fixtures + provenance README in `tests/fixtures/agent_workflows/`) cover success/error/cancel/slow-output, flooding reviewer never blocks the sink (64 KiB cap + truncation marker, quiet sibling output intact), E8 `manifest.json` exactly the 12 keys with objectized `findings_oid` (content-addressed blob) and `manual_attach` (empty until `review attach`, then non-empty object entries) + spotlighting-delimited redacted `findings.md` + `reviewers/<slug>.{stdout,stderr}.redacted.log`; `review --fix` submits only its fixed trusted request through an authenticated active Code controller (no external reviewer seed or second queue), pins approval denial, sandbox denial, deterministic repair, and gated patch success, while a missing/unauthorized runtime still fails closed with `LBR-AGENT-010` (exit 128, JSON error surface); cancel marker kills the reviewer PID (kill -0 fails) and releases the workspace with idempotent second cancel — plus the plan.md:961 cancel-during-pending-output stress bound and the 强制补强项 #5 `review list --json --limit --cursor` keyset envelope (exact `{schema_version, items, next_cursor, has_more}`, no-dup/no-loss walk, `run_id DESC` tiebreak, malformed cursor fails closed at exit 129) through the real CLI | `src/internal/ai/review/`, `src/internal/ai/runtime/fix_{bridge,control,execution,protocol,response}.rs`, `src/command/agent/review.rs` |
| `agent_review_span_test` | 1 | AG-22 `agent.review.run` span fake-sink assertion (plan.md Task A7 / agent.md §6 :1334): required fields (run_id/agent_count/terminal_state/duration_ms) present on close, reviewer stdout text absent from the sink while provably present in `findings.md` — own binary to avoid tracing callsite-cache races | `src/internal/ai/review/runner.rs` |
| `agent_investigate_workflow_test` | 1 | AG-23 investigate workflow (plan.md Task A8): pinned scenarios — fake `/bin/sh` investigators (fixtures + provenance README in `tests/fixtures/agent_workflows/`) drive STRICT round-robin to terminal `quorum` (agent order preserved, per-stance sequence) and `max_turns` (round-robin wraps a,b,a), stall→paused `stalled` + `pending_turn` (non-terminal) then `continue` resumes to terminal, non-zero investigator→paused `agent_failure`, cancel→terminal `cancelled` (workspace released, no worktree mutation), E8 `manifest.json` exactly the 12 keys with `kind="investigate"` + objectized `findings_oid` / `manual_attach` (empty until `investigate attach`) + spotlighting-delimited redacted `findings.md` (seed topic persisted, fake `sk-` stance secret + ANSI scrubbed from findings/`*.redacted.log`), run-id flock makes a concurrent `continue` fail closed `RunLocked` (released→succeeds), `investigate fix` reuses the review controller/runtime helper with a fixed request that excludes run id/topic/findings and pins repair output without a patch, while no runtime still fails closed with `LBR-AGENT-010` (exit 128, JSON error surface), and the 强制补强项 #5 `investigate list --json --limit --cursor` keyset envelope (exact `{schema_version, items, next_cursor, has_more}`, no-dup/no-loss walk, `run_id DESC` tiebreak, malformed cursor fails closed at exit 129) through the real CLI | `src/internal/ai/investigate/`, `src/internal/ai/runtime/fix_{bridge,control,execution}.rs`, `src/command/agent/investigate.rs`, `src/command/agent/review.rs` |
| `agent_investigate_span_test` | 1 | AG-23 `agent.investigate.run` span fake-sink assertion (plan.md Task A8 / agent.md §6 :1335): required fields (run_id/turn/next_agent_idx/terminal_state) present on close, the untrusted seed topic and investigator stdout text absent from the sink while provably present in `findings.md` — own binary to avoid tracing callsite-cache races | `src/internal/ai/investigate/runner.rs` |
| `compat_help_examples_banner` | 1 | Every visible command in `src/cli.rs::Commands` renders an `EXAMPLES:` / `Examples:` section in `<cmd> --help` (cross-cutting item B) | `src/cli.rs`, `src/command/**` |
| `compat_error_codes_doc_sync` | 1 | Every `LBR-*-NNN` literal in `src/utils/error.rs` is documented in `docs/error-codes.md` | `src/utils/error.rs`, `docs/error-codes.md` |
| `compat_ledger_schema` | 24 | plan-20260729 CT2-01: the compatibility evidence ledger's structural contract — ADR-CT-03's closed field set, tier recomputed from `COMPATIBILITY.md` rather than self-reported, resolvable `surface_evidence` that must mention both command and surface, the `direct` threshold, `declined`/`blocked` obligations, and 17 named rejection fixtures; an empty tree passes | `tests/compat-ledger/**`, `COMPATIBILITY.md`, `docs/development/commands/_compatibility.md` |
| `compat_command_docs_examples_section` | 1 | Every `docs/commands/<name>.md` page carries an `## Examples` / `## Common Commands` heading | `docs/commands/**` |
| `compat_r0_9_doc_closeout` | 1 | plan-20260714 R0-9 docs close-out: the warning code/source table, the `io_blocked` JSON schema (with the `reason` enum parsed out of `io_blocked_reason_and_code` so a new variant cannot ship undocumented), the `{from, to}` typing of nested `staged.renamed` / `unstaged.renamed`, the three rename config keys across the status docs + `COMPATIBILITY.md`, and the `diff.renameLimit` degradation semantics (exact **and** unique-basename survive) across the diff docs + CHANGELOG — EN and zh asserted in parallel | `docs/commands/status.md`, `docs/commands/diff.md`, `COMPATIBILITY.md`, `CHANGELOG.md` |
| `compat_version_surface_sync` | 1 | plan-20260714 PD-00/PD-10 release-surface guard: `Cargo.toml`, `web/package.json`, `worker/package.json`, `install.sh` `DEFAULT_VERSION`, and `install.ps1` `$DefaultVersion` all carry the same version; both installer values keep their `v` prefix | `Cargo.toml`, `web/package.json`, `worker/package.json`, `install.sh`, `install.ps1` |
| `compat_serial_registry` | 1 | plan-20260729 CT3-07/FIX-04 serial-annotation guard over `tests/**`: every non-`none` `#[serial]` there has exactly one `tests/SERIAL_REGISTRY.tsv` row with a non-empty `global`/`lane:*` reason, registry and `tests/SERIAL_CLASSIFY.sh` verdicts match bidirectionally (missing/dangling/drift asserted, duplicate fn rows rejected, `<site:path:macro:<name>#<ordinal>>` content-anchored macro sites verified (TA-02: line numbers banned from keys, guard relocates by macro NAME + in-body ordinal so edits above the attribute cannot break the anchor; orphan sites keyed `<site:path:orphan#N>`; plus a line-drift regression and three injection counterexamples), string/same-line-attribute lexer counterexamples, named keys that do not cover process-wide pollution rejected, `inner_attrs`/`crate = <path>` config items not counted as lock keys), the classifier is deterministic on raw stdout, and (TA-01) `none` requires the whole call surface to be proven pollution-free — explicit allowlist with per-entry reasons, bounded helper expansion (cycles/unknowns fail closed to `global`) that propagates helper pollution lanes to callers, plus 18 guard tests (TA-03 adds the standing zero-unkeyed-#[serial] invariant over tests/** — conversion driven by the frozen tests/SERIAL_MANIFEST.tsv through the mechanical, idempotent tests/SERIAL_CONVERT.sh; plan-20260827 NP-01 adds the nextest-group drift guard — .config/nextest.toml must byte-match its generator tests/NEXTEST_GROUPS.sh and the union external group must equal the registry-derived membership, 208 anchored last-segment regex filters test(/(^|::)<fn>$/) — module-pathed names in aggregated binaries match, fn names are tree-unique so the anchor is exact — plus 7 binary(=target) filters) covering the full adversarial battery (rename/alias laundering incl. nested-brace full-path recovery, macro shadowing & metavariable fail-closed, Drop/callable-emitting macros, include! splicing, Drop-impl lane merging with alias/module canonicalization, fn-reference & qualified value-reference channels, the benign env-read gate with param-forwarding traces, and the path-argument proof engine that lanes unproven filesystem paths cwd) | `tests/SERIAL_CLASSIFY.sh`, `tests/SERIAL_REGISTRY.tsv`, `tests/NEXTEST_GROUPS.sh`, `.config/nextest.toml`, `tests/nextest_group_overlap_check.py`, `tests/compat/serial_registry.rs` |
| `compat_help_flag_descriptions` | 1 | Every visible flag and positional under `Options:` / `Arguments:` carries a non-empty description; covers 42 root commands + 53 sub/sub-sub-commands (110 surfaces) | `src/cli.rs`, `src/command/**` |
| `compat_help_no_impl_meta_leak` | 1 | No `libra <cmd> --help` body leaks contributor-facing rustdoc into clap's long_about; forbids 6 phrase classes (e.g. `Codex pass-`, raw markdown headings, code fences) | `src/cli.rs`, `src/command/**` |
| `verify_pack_multi_test` | 1 | Guards `verify-pack <idx>...` multi-index verification, JSON wrapping, and `--pack` argument rejection | `src/command/verify_pack*.rs` |
| `db_migration_test` | 1 | SQLite schema bootstrap + migration round-trip, including M4 import identity/tombstone and M5 subagent content empty up→down→up, non-empty recovery-state rollback refusal, the §C.8 workspace-lease down guard (non-terminal rows refuse, settled rows roll back, live leases block deeper W3 rollbacks transitively), and uniqueness/state failure matrix | `src/internal/db.rs`, `sql/` |
| `workspace_lease_test` | 1 | plan-20260714 §C.8 W4 workspace association/lease store: DB-arbitrated single winner per linked `(repo_id, worktree_id)` including a proven two-connection overlap (contender with a 50ms busy timeout must hit the uncommitted writer's lock, then lose to the index on retry) and failpoint-windowed takeovers (a parked doctor reports only the fence it wrote), canonical-path alias refusal (`.`/`..`, trailing separator, symlink, symlinked-parent `..` traversal, dangling-symlink parent) across kinds, repository identity resolved from `libra.repoid` (never the caller; padded/empty values refused as corrupt, write and read paths pinned to the same value, a rewrite with live rows fails closed) and cross-repository isolation, owner+monotonic-fence conditional renew/release/activate/abandon with stale-owner refusal (`LBR-AGENT-023`), no implicit steal of an expired lease, doctor reclaim only after expiry (`LBR-AGENT-022`) with successive cross-connection fence advancement and superseded-holder release refusal, provisioning→active publication (an `active` record requires an existing directory), released/orphaned identity release, foreign-identity recovery (bounded keyset listing plus adopt, with identity-drift refusals), bounded expired-lease sweep, keyset listing, and the association-IDs-only column pin | `src/internal/workspace.rs`, `sql/migrations/2026072501_workspace_record.sql` |

## Wave 2 — Code UI & local automation

### plan-20260715 W0-02 — workflow behavior baselines

Machine-readable inventory for Checkpoint A. W3-02 Web harness retargets these
rows onto the default Web launch HTTP/SSE (baselines live in
`src/internal/ai/workflow_baseline.rs`; the retired TUI's in-process re-exports
were removed with W5-03).

| TUI-owned behavior | baseline test name | expected output / assertion |
|---|---|---|
| IntentSpec review | `code_ui_scenarios::plan_workflow_baseline_pins_intent_and_post_plan_choices` | choices = `Confirm Intent` / `Modify Intent` / `Cancel` |
| Plan review (post-plan) | `code_ui_scenarios::plan_workflow_baseline_pins_intent_and_post_plan_choices` | choices = `Execute Plan` / `Modify Plan` / `Cancel` |
| Network policy | `code_ui_scenarios::plan_review_baseline_pins_network_policy_choices` | choices = `Network: Deny` / `Network: Allow` / `Back` |
| Repair loop threshold | `code_ui_remote_state_matrix::repair_loop_baseline_threshold_keeps_plan_continue_affordance` (also `code_ui_scenarios::repair_loop_baseline_threshold_keeps_plan_continue_affordance`) | message contains automatic-threshold stop text + `/plan continue` |
| Resume | `code_resume_test::resume_with_chat_session_id_restores_prior_transcript` | prior transcript restored after `--resume <session>` |
| Goal/task control | `code_ui_scenarios::goal_task_control_baseline_session_event_kind_tag_is_goal` (+ `ai_goal_state_test::session_event_goal_variant_round_trips_through_serde`) | SessionEvent kind tag `goal` round-trips |
| Approval | `code_ui_remote_approval_matrix::approval_accept_path_runs_shell_and_completes_assistant` | shell tool completes after `approved: true` |
| User-input | `code_ui_scenarios::user_input_baseline_interaction_kind_is_request_user_input` | wire kind = `request_user_input` |
| Generation / CLI entry | `code_ui_remote_generation_matrix::generation_sse_observes_tool_execution_and_final_completion`; `code_cli_dispatch_test::defaults_are_observe_control_and_deny_network` | generation settles idle; CLI defaults observe + deny-network |

### plan-20260715 W3-02 — Code UI Web harness

Default `CodeSession` spawn is flagless `libra code` (the default Web launch;
W5-07 removed the `--web-only` alias) with `--port 0` /
`--mcp-port 0`. Readiness waits on `control.json` (MCP URL populated) then an
HTTP `/session` snapshot — not sleep-only. W5-06 removed the legacy TUI startup path together with its PTY
control hooks (`CodeSessionOptions::with_pty_tui()`,
`CodeSession::write_tui_line`, `ScenarioStep::reclaim_via_tui_command`);
W5-04 then replaced the pseudo-terminal process launch and its log artifact with
a non-TTY `std::process::Command` launch and `process.log`; `CodeSession::shutdown`
always SIGTERMs the headless Web process. The legacy-TUI scenarios
(reclaim / legacy TUI lifecycle) were deleted
from `code_ui_scenarios`, and the remaining scenarios that used
`.with_pty_tui()` were converted to the headless Web driver. Always run Code UI harness
targets with
`LIBRA_ENABLE_TEST_PROVIDER=1`, `--features test-provider`, and
`--test-threads=1` (shared control-file / port contention otherwise).

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `harness_self_test` | 2 | Smoke-checks the Code UI Web harness (control files; SIGTERM release asserted through natural exit + control token/info removal — TA-04 dropped the racy rebind-specific-address probe) | `tests/harness/` |
| `code_ui_scenarios` | 2 | End-to-end Web-harness scenarios, including W2-03 `plan_review_modify_next_plain_text_opens_replacement_plan_gate`, `plan_review_empty_revision_note_is_typed_and_preserves_authority`, `plan_review_repository_replacement_after_modify_keeps_revision_note_retryable`, `plan_review_workspace_drift_survives_resume_and_modify_rearms_current_checkout`, `plan_review_metadata_only_drift_warns_but_exact_execute_recheck_succeeds`, `plan_review_head_drift_survives_resume_and_requires_explicit_modify`, `plan_review_repository_replacement_blocks_modify_and_preserves_gate`, `plan_review_network_allow_enters_runtime_queue`, `plan_review_network_allow_returns_conflict_and_preserves_pending_gate`, and network-gate crash/resume | `src/command/code.rs`, `src/internal/ai/web/`, `src/internal/ai/workflow_baseline.rs` |
| `code_ui_remote_lease_matrix` | 2 | Browser/automation lease lifecycle matrix | `src/command/code.rs` controller, `src/command/code_control.rs` |
| `code_ui_remote_sse_matrix` | 2 | SSE event stream matrix from web view, consuming only wire v2 (DF-08 removed v1: the two explicit-v1 compatibility cases were deleted with the wire, and a fixture-shape guard rejects any v1 openEvents step) | `src/internal/ai/web/`, `src/command/code.rs` (axum), `src/command/code_control.rs` |
| `code_ui_remote_state_matrix` | 2 | Cross-surface state replication matrix, including mid-turn detach/cancel settling; the tool-call fixture case runs on the v2 code_workflow wire since DF-08 (post-tool follow-up completion proves the tool phase; the transient executing_tool status is not durably recorded) | `src/internal/ai/web/code_ui.rs`, `src/command/code_control.rs`, `src/internal/ai/workflow_baseline.rs` |
| `code_ui_remote_security_matrix` | 2 | Auth/token/origin enforcement matrix | `src/command/code_control*.rs` |
| `code_ui_remote_generation_matrix` | 2 | Generation control across surfaces (no live LLM), with its SSE observation on the v2 code_workflow wire since DF-08 removed v1 | `src/internal/ai/web/`, `src/command/code.rs` |
| `code_ui_remote_approval_matrix` | 2 | Approval flow across Web/automation, including runtime-owned Plan review and network-policy gate parity | `src/internal/ai/agent/` approvals, `src/internal/ai/runtime/phase1.rs` |
| `code_cli_dispatch_test` | 2 | `libra code …` argv parsing & dispatch; W5-09 aggregate breaking guard `breaking_code_surface_migration` pins the whole family surface (removed `--web`/`--web-only` aliases + inert rollback env, unexposed `code-control`, refused interactive graph with `--json`/`--machine` pinned to the structured `LBR-REPO-001` path, rejected bare codex+resume) in one pre-push run | `src/command/code.rs` |
| `code_provider_resolution_test` | 22 | plan-20260825 PS-06 zero/one/many credential detection for `libra code` without `--provider`: zero candidates exit 128 (LBR-AUTH-001) with the a-mode checked-chain guidance and credential-free block; a single candidate auto-selects with the id/env/layer note on stderr only (stdout clean, key values never printed — GC-PS-01); multiple candidates exit 129 (LBR-CLI-002) listing id-sorted candidates with their hit layers. Isolated HOME + LIBRA_CONFIG_GLOBAL_DB per test; the six auto-selectable keys are env_remove'd so ambient credentials cannot leak. terra R1 additions: the --model pairing guard fires before env-file IO (129 with --env-file pointing at a directory), an explicit --provider structurally skips detection probes (poisoned LIBRA_CONFIG_GLOBAL_DB never reached), and mixed process-env + global-vault candidates carry distinct layer labels. terra R2 additions: `--repo B` detects against B's repo-local vault (caller repo A's key never selected), and `--machine` turns the auto-selection note into a structured JSON event (no prose). terra R4: empty-string keys (process env or --env-file) count as unconfigured and land on the zero-candidate guidance; terra R5: an empty upper layer falls through to the usable layer beneath (empty process env -> repo-local vault; empty env-file value -> process env), and boot agrees with the detection verdict. PS-03 additions: the persisted `code.defaultProvider` slot wins over a unique detection candidate (and a config-selected provider missing its key errors naming only itself), repo-local overrides global, an unrecognized stored id exits 129 with the sorted legal domain and never echoes the value, explicit `--provider` overrides the config key, and a config hit pairs with `--model` ahead of the pairing guard. PS-04 addition: an `--agent` profile binding labels the startup banner with the effective (binding) provider, with detection silent and no key leakage. PS-05 additions (5): resume inherits the recorded provider over both `code.defaultProvider` and detection (erroring on the recorded provider's own missing key), a metadata-less resume falls through to detection and gets stamped insert-if-absent, an explicit provider disagreeing with the record warns with both ids (prose on stderr, structured `provider_resume_mismatch` under `--machine`) and continues, a recorded ollama/model pair boots past the explicit-model guard, and a fresh boot records provider/model ids (never credential material) in the session metadata; terra R1: a corrupt recorded id never surfaces in output (even at LIBRA_LOG=warn) and falls through to detection | `tests/code_provider_resolution_test.rs`, `src/command/code.rs`, `src/internal/config.rs` |
| `code_provider_boot_test` | 2 | Provider/agent bootstrap inside `libra code`, including the shared env-file → process → Vault factory used by default Web and headless launch | `src/command/code.rs`, `src/internal/ai/providers/`, `src/internal/ai/runtime/services.rs` |
| `code_tool_acl_test` | 2 | Tool registry ACL & safety classification, consumed through the runtime-owned CodeAgentServices builder | `src/internal/ai/tools/`, `src/internal/ai/runtime/services.rs` |
| `code_mcp_dual_entry_test` | 2 | MCP stdio + http dual entry parity; since DF-08 the web-chat direction observes the v2 transcript_upsert projection and the MCP-write direction polls the session snapshot (out-of-band writes do not append durable workflow events) | `src/internal/ai/mcp/`, `src/command/code.rs` |
| `code_resume_test` | 2 | Session resume across restarts | `src/internal/ai/session/`, `src/command/code.rs` |
| `code_codex_default_web_test` | 2 | W4-01/W5-06/W5-07: default `libra code` routes to Web Code UI; `--provider codex` still uses managed runtime (legacy stdin loop unreachable; the legacy terminal resume driver was removed in W5-06 and bare `--provider codex --resume` is rejected with a usage error plus a migration hint) | `src/command/code.rs`, `src/internal/ai/codex/` |
| `code_codex_runtime_test` | 2 | `--provider codex` WS runtime boot: `--codex-port` validation, managed app-server initialize/thread-start, approval-interaction regression, W3-04 `AgentEvent` envelope normalize, W3-07 cancel/interrupt + sequential approval ownership | `src/command/code.rs`, `src/internal/ai/codex/` |
| `ai_code_ui_headless_test` | 2 | Headless Code UI runtime/projection coverage, including `active_revision_blocks_new_direct_but_preserves_exact_terminal_retry`, `slash_intent_cancel_durably_exits_revision_and_unblocks_direct_turns` (padded cancel 409), `oversized_intent_modify_note_is_typed_400_without_consuming_gate` (16 KiB pre-Claiming cap), `slash_intent_modify_provider_prompt_uses_only_the_change_suffix`, `slash_intent_modify_multiple_successful_drafts_preserve_the_revision`, stale cancel/replacement projection healing, and process-lifetime `headless_session_writer_lease_*` ownership/race tests | `src/internal/ai/web/{headless,web_admission}.rs`, `src/internal/ai/session/jsonl.rs` |
| `ai_code_ui_projection_test` | 2 | Projection snapshot replication; W3-14 10k-event fold bound + release p95 (`large_session_projection_smoke`); W4-04 null `thread_graph` delta fold | `src/internal/ai/history.rs`, `src/internal/ai/web/code_ui_projection.rs` |
| `ai_code_ui_wire_test` | 2 | Wire-format contract including built-in automation wire-v2 negotiation and the DF-06 server v2 omission default; DF-08: explicit v1 is a stable removal error (only v2 negotiates), camelCase `threadGraph`, digest-only/raw-note-free `intent_revision_recovery_is_additive_and_pins_sse_snake_case`, dedicated receipt `sse_wire_v2_intent_revision_consumed_uses_dedicated_payload`, catalogued 409 `plan_execution_not_available_is_a_catalogued_conflict`, and typed drift/revision errors in `phase1_workspace_and_revision_errors_are_catalogued` | `src/internal/ai/web/`, `src/internal/ai/agent/`, `src/command/code_control.rs` |

W2-03's focused `cargo test --lib --all-features` anchors are
`phase1_workspace_binding_uses_content_authority_and_legacy_fallback`,
`phase1_capture_rejects_change_between_content_and_metadata_scans`,
`metadata_fingerprint_changes_without_reading_file_bodies`,
`content_fingerprint_detects_same_length_change_with_restored_mtime`,
`fingerprint_budget_counts_directories_and_propagates_entry_limit`,
`fingerprint_budget_propagates_path_name_limit`,
`fingerprint_budget_timeout_is_typed_and_actionable`,
`fingerprint_budget_checks_manifest_post_blocking_and_return_boundaries`,
`fingerprint_budget_checks_exact_blocking_operations_after_return`,
`fingerprint_budget_checks_metadata_and_symlink_operations_after_return`,
`fingerprint_budget_streams_wide_directory_before_rejecting`,
`fingerprint_entry_cap_stops_before_poisoned_iterator_tail`,
`bounded_manifest_order_matches_legacy_sorted_walk`, and
`stable_fingerprint_pair_rejects_change_after_content_scan`. Crash-resume
lock recovery is pinned by `code_workflow_append_lock_uses_os_liveness_without_aba`
and `code_workflow_append_lock_rejects_symlink_without_touching_event_log`.
Strict ordinary Plan/Network recovery before context GC is pinned by
`phase1_recovery_rejects_sequence_gap_without_revision_sidecar_before_gc`.
The W2-03 shared replay index and permanent authority closure are pinned in
`src/internal/ai/session/jsonl.rs` by
`committed_revision_receipt_batch_index_visits_five_thousand_events_linearly`,
`claiming_and_consuming_revision_retry_index_visits_five_thousand_events_linearly`
(5,000 events / 2,000 retries across startup, Claiming, and Consuming),
`invalid_nonmutating_web_intent_after_receipt_fails_closed`,
`pending_revision_receipt_followed_by_web_intent_pins_display`,
`resolved_replacement_receipt_permanently_closes_its_retry_lineage`,
`intent_revision_terminal_retry_rejects_marker_appended_after_terminal`,
`replacement_marker_after_consumer_terminal_never_authorizes_a_receipt`,
`effectless_intent_revision_receipt_fails_closed_in_batch_validation`,
`duplicate_source_intent_after_its_terminal_fails_closed_in_batch_validation`,
`reused_web_command_id_across_scopes_fails_closed_before_marker_attribution`,
`conflicting_or_duplicate_intent_review_marker_ownership_fails_closed`, and
`intent_revision_consumption_is_first_writer_for_source_consumer_and_event`.
Headless claiming and admission anchors in `src/internal/ai/web/headless.rs` /
`tests/ai_code_ui_headless_test.rs` are
`claiming_without_command_rearms_active_before_mutation_recovery`,
`claiming_pending_command_promotes_before_generic_recovery`,
`claiming_canonical_cancel_and_double_attempt_remain_retryable`,
`active_revision_blocks_new_direct_but_preserves_exact_terminal_retry`,
`slash_intent_cancel_durably_exits_revision_and_unblocks_direct_turns`,
`oversized_intent_modify_note_is_typed_400_without_consuming_gate`,
`slash_intent_modify_provider_prompt_uses_only_the_change_suffix`,
`slash_intent_modify_multiple_successful_drafts_preserve_the_revision`,
`slash_intent_cancel_succeeded_with_stale_non_streaming_ack_recovers`,
`pending_cancel_receipt_before_later_web_never_rewrites_later_projection`, and
`restart_reconciles_stale_intent_projection_after_atomic_control_cancel`.
The distinct session-writer lease is pinned by
`headless_session_writer_lease_rejects_second_attach_and_reacquires_after_drop`,
`headless_session_writer_lease_cannot_be_rebound_to_another_session`,
`headless_session_writer_lease_clone_cannot_attach_a_second_persistence`,
`headless_session_writer_lease_rejects_symlink_without_touching_target`,
`headless_session_writer_lease_rejects_replaced_lock_inode_before_attach`,
`headless_session_writer_lease_rejects_fifo_without_blocking`, and
`headless_session_writer_lease_is_released_immediately_after_sigkill`.
Scanner race/platform anchors in `src/internal/ai/workspace_snapshot.rs` are
`content_fingerprint_rejects_file_swapped_to_external_symlink_before_read`,
`content_fingerprint_rejects_file_swapped_to_fifo_before_read`,
`content_fingerprint_rejects_parent_swapped_to_external_symlink_before_read`,
`content_fingerprint_rejects_symlink_parent_swapped_outside_before_readlink`,
`metadata_fingerprint_rejects_symlink_parent_swapped_outside_before_readlink`,
`metadata_fingerprint_reuses_pinned_root_after_workspace_path_replacement`,
`fingerprint_budget_checks_post_read_and_reopen_boundaries_by_occurrence`,
`windows_reparse_parser_uses_symlink_substitute_name`,
`windows_reparse_parser_accepts_mount_point_substitute_name`, and
`windows_reparse_parser_rejects_truncated_odd_and_out_of_range_records`;
post-content checkout identity is pinned by
`phase1_exact_validation_rejects_identity_change_after_content_scan` in
`src/internal/ai/runtime/phase1.rs`.

### plan-20260715 W3-15 — Playwright real-browser e2e

Not a cargo `--test` target. Owner suite is `pnpm --dir web test:e2e`
(`web/playwright.config.ts` + `web/e2e/**`). Specs assert only through
user-visible DOM/HTTP against an **already-started** deterministic Web runtime
(`libra code --browser-control loopback --provider fake` with
`--features test-provider` + `LIBRA_ENABLE_TEST_PROVIDER=1`).

**Startup**

```bash
./web/e2e/scripts/start-deterministic-runtime.sh   # Terminal A; prints LIBRA_E2E_BASE_URL
pnpm --dir web exec playwright install chromium    # once per machine
export LIBRA_E2E_BASE_URL=http://127.0.0.1:4410
export LIBRA_E2E_REQUIRE=1                         # fail-closed (completion evidence)
pnpm --dir web test:e2e
```

CI job `compat-web-e2e` (`.github/workflows/base.yml`) — **temporarily disabled
2026-08-31** (self-hosted runner `sudo` cannot complete
`playwright install --with-deps`; restore from git history once the runner
regains passwordless sudo) — builds `--features test-provider`, starts the
deterministic runtime, installs Chromium, and runs `test:e2e` with
`LIBRA_E2E_REQUIRE=1`. Soft-skip is refused there.

Without `LIBRA_E2E_REQUIRE=1` / `CI=true`, missing Chromium or unreachable
`/api/health` prints `skip: …` and exits 0 for local diagnosis only — soft-skip
is **not** W3-15 / Checkpoint C completion evidence.

**Artifacts / cleanup**

| path | contents |
|---|---|
| `web/test-results/` | per-test screenshots, traces, videos (`retain-on-failure`) |
| `web/playwright-report/` | HTML report (`playwright show-report`) |

Both dirs are gitignored. On failure: inspect report/trace, then
`rm -rf web/test-results web/playwright-report`. Stop the runtime with Ctrl-C
on the start script (or kill its printed pid); the script removes the temp
workdir on exit.

| suite | wave | one-line purpose | relevant src |
|---|---|---|---|
| `pnpm --dir web test:e2e` | 2 | Playwright main-chain: submit → approval/user-input → goal/task/skill → usage → execution/repair → resume/cancel → SSE reconnect | `web/e2e/`, `web/playwright.config.ts`, `web/e2e/scripts/start-deterministic-runtime.sh` |
| `intent_flow_test` | 2 | IntentSpec → Plan → Run pipeline (no live LLM) | `src/internal/ai/intentspec/`, `src/internal/ai/orchestrator/` |
| `e2e_mcp_flow` | 2 | End-to-end MCP server flow (TA-04: launches with OS-assigned ports `--port 0 --mcp-port 0`, endpoints read back from the startup banner; shutdown asserted by connect-refused, not rebind) | `src/internal/ai/mcp/` |
| `mcp_integration_test` | 2 | MCP integration tests | `src/internal/ai/mcp/` |
| `ai_automation_test` | 2 | `.libra/automations.toml` rule execution | `src/internal/ai/automation/`, `src/command/automation.rs` |
| `ai_dag_tool_loop_test` | 2 | DAG-based tool loop regression | `src/internal/ai/agent/` |
| `ai_mock_provider_test` | 2 | Mock provider used by `test-provider` feature | `src/internal/ai/providers/` (test-only) |
| `agent_capture_migration_test` | 2 | `agent_capture` / checkpoint-paging and W4 capture-workspace-scope migrations, including legacy bootstrap and guarded down→up compatibility via `MigrationRunner` | `src/internal/db.rs`, `sql/migrations/2026050303_agent_capture.sql`, `sql/migrations/2026080401_agent_capture_workspace_scope.sql` |
| `ai_agent_baseline_test` | 2 | Step 1.0 / CEX-00 single-agent baseline tests | `src/command/code.rs`, `src/internal/ai/agent/` |
| `ai_approval_ttl_test` | 2 | CEX-11 approval TTL and canonical key contract tests | `src/internal/ai/agent/` |
| `ai_subagent_permission_test` | 2 | GC-06 subagent permission mediation and fail-closed approval contracts | `src/internal/ai/agent/`, `src/internal/ai/runtime/` |
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
| `ai_libra_vcs_safety_test` | 2 | CEX-02 run_libra_vcs parameter-level safety tests, including P1-08b `diff -S/-G` allowlisting without weakening textconv/external-driver approval | `src/internal/ai/libra_vcs.rs`, `src/internal/ai/tools/` |
| `ai_memory_anchor_test` | 2 | Short-term/long-term memory anchor contract tests | `src/internal/ai/agent/` |
| `ai_multi_agent_e2e_test` | 2 | S7 multi-agent declarative config E2E | `src/internal/ai/agent/` |
| `ai_projection_resolver_test` | 2 | Phase B projection resolver and scheduler repository tests | `src/internal/ai/orchestrator/` |
| `ai_provider_context_overflow_compact_loop_test` | 2 | OC-Phase 4 context-overflow compaction loop integration tests | `src/internal/ai/providers/` |
| `ai_provider_error_taxonomy_test` | 2 | Integration fixtures for OC-Phase 4 provider error taxonomy | `src/internal/ai/providers/` |
| `ai_provider_retry_policy_test` | 2 | OC-Phase 4 retry-policy integration test | `src/internal/ai/providers/` |
| `ai_provider_transform_test` | 2 | Integration tests for OC-Phase 4 P4.1 provider transform pipeline | `src/internal/ai/providers/` |
| `ai_runtime_contract_test` | 2 | Runtime contracts for serialized worker state, durable intent-before-dispatch, and W2-03 Plan review/network-policy gate and restart behavior | `src/internal/ai/runtime/` |
| `ai_scheduler_plan_set_test` | 2 | Phase 0 selected plan set and task dependency tests | `src/internal/ai/orchestrator/` |
| `ai_schema_migration_test` | 2 | Phase 0 schema migration tests for AI runtime contract tables | `src/internal/db.rs`, `sql/` |
| `ai_security_runtime_test` | 2 | Phase 5 security runtime (authz, redaction, shell, audit) | `src/internal/ai/sandbox/` |
| `ai_semantic_rust_test` | 2 | Semantic Rust code indexing and structure extraction | `src/internal/ai/skills/` |
| `ai_semantic_tools_test` | 2 | Semantic tools registration and classification | `src/internal/ai/tools/` |
| `ai_session_jsonl_test` | 2 | Session JSONL sequence/dedup/gap recovery, command idempotency, W2-03 public receipt ordering (`intent_revision_receipt_orders_source_consumer_and_resyncs_exact_postwrite_retry`), and unit-level shared `ValidatedIntentRevisionReceiptIndex`/first-writer/permanent-lineage authority | `src/internal/ai/session/jsonl.rs` |
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
| `ai_usage_stats_test` | 2 | CEX-16 usage persistence plus W2-12 replay-safe runtime totals, uncertainty states, per-turn deltas, and child attribution | `src/internal/ai/usage/`, `src/internal/ai/agent/runtime/usage.rs` |
| `ai_usage_display_test` | 2 | CEX-16 usage display formatting tests | `src/internal/ai/usage/` |
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

## Wave 1F — Feature-gated deterministic (compile-time feature, no secrets)

Deterministic L1 targets excluded from a bare `cargo test --all` only because
they require a compile-time feature (not a runtime secret). CI runs them in
dedicated feature-on steps.

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `upgrade_auto_test` | 1 | plan-20260714 §A.11 auto-upgrade end-to-end: signature+decision chain, anti-rollback/revocation replay, real-binary `__upgrade-probe` self-check, install/rollback transaction (`--features test-upgrade`) | `src/internal/upgrade/`, `src/command/upgrade.rs` |
| `upgrade_publish_contract_test` | 1 | plan-20260714 §A.9/§A.11 manifest/publish contract: matrix coverage, URL binding, size bounds, renew preserves pause/revocations; plan-20260821 A1-06 Backend B1-02 transition contract vectors (cross-implementation verify, anti-vv placeholder) (`--features test-upgrade`) | `src/internal/upgrade/manifest.rs`, `tests/data/up01-transition-vectors-v1.json` |
| `install_smoke_test` | 2 | plan-20260821 A1-05 installer verification smoke: twenty-four `install.sh` scenarios (signed install, tampered signature/payload, sha/size mismatch, expired/paused/revoked, stale-replay floor, zero-size, min_key_generation + bounded-numeric grammar, key validity window, non-canonical/trailing-artifact serialization, calendar-invalid dates, pretty-printed envelope acceptance, oversized-response cutoff, multi-line payload smuggling, oversized SemVer components, manifest-404 and verifier-unavailable transitions ± `LIBRA_ALLOW_FALLBACK`) plus twenty-two `install.ps1` scenarios when `pwsh` exists; needs bash+python3+openssl, otherwise prints skipped | `install.sh`, `install.ps1`, `tests/data/install-smoke/` |

## Wave 4 — Live AI (test-live-ai / DEEPSEEK_API_KEY)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `ai_agent_test` | 4 | Live LLM agent loop smoke | `src/internal/ai/agent/`, `src/internal/ai/providers/` |
| `ai_chat_agent_test` | 4 | Live LLM chat-mode agent | `src/internal/ai/agent/` |
| `code_ui_remote_model_generation_matrix` | 4 | Live model generation matrix (ignored by default) | `src/internal/ai/providers/` |
| `ai_ollama_live_gate_test` | 4 | Ollama live-gate smoke | `src/internal/ai/providers/ollama/` |

## Wave 5 — Live Cloud (test-live-cloud / D1+R2)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `cloud_storage_backup_test` | 5 | D1/R2 backup + restore round-trip | `src/command/cloud.rs`, `src/utils/d1_client.rs`, `src/utils/client_storage.rs` |
| `agent_cloud_tombstone_test` | 5 | A0-10 deferral guard: a local agent-capture erase does not propagate a tombstone to the D1 mirror (skips without `test-live-cloud` + `LIBRA_D1_*`) | `src/utils/d1_client.rs`, `src/internal/ai/history.rs` |
| `publish_live_test` | 5 | Publish pipeline against live R2 | `src/publish/`, `src/command/publish.rs` |
| `storage_r2_test` | 5 | Object store R2 path | `src/utils/client_storage.rs` |

## Wave 6 — Performance smoke (LIBRA_RUN_PERF=1)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `code_ui_perf_smoke_test` | 6 | Code UI perf / default-v2 SSE soak with cursor/resync recovery | `src/command/code.rs`, `src/internal/ai/web/` |

---

## Wave 7 — Local agent capture smoke (LIBRA_RUN_LOCAL_AGENTS=1)

| target | wave | one-line purpose | relevant src |
|---|---|---|---|
| `agent_local_capture_smoke_test` | 7 | A6.5 first-batch hard gate: drives the real local `codex`/`claude`/`opencode` CLIs (one paid session each; `#[ignore]` + env-gate, serial) through hook install → capture → session/checkpoint/traces/doctor assertions → uninstall smoke; SBX-05 adds Darwin `local_capture_smoke_opencode_macos` (content capture) and `linux_a65_criteria_unchanged`; driver in `tests/harness/agent_local_capture.rs` | `src/command/agent/`, `src/command/hooks.rs`, `src/internal/ai/hooks/` |

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
