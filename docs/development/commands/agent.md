# Agent Command Development

## Removed dry-run prompt output (SC-03)

`--show-redacted-prompt` is not a flag. The promote subcommand stays deleted (RC-11), so a dry-run cannot print a prompt body in human output or JSON. The command is rejected instead of formatting a `<len=N>` line.

## Grok capture (GK-05)

Grok is supported and hook-installable with `support_wave=grok`, not `first_batch`, and is not a `review` or `investigate` launcher. **Automatic Thinking capture is paused.** Hook lifecycle events are recorded through the shared pipeline; decoding does not read session logs or write `GROK_HOME/libra-capture`. Existing readers for local `updates.jsonl` and `chat_history.jsonl` are not yet connected to the authorized snapshot/checkpoint writer. Offline observer JSON is scratch state, not a Libra checkpoint. The intended transcript retains Thinking text and unproven `encrypted_content`; Libra does not decrypt it. Current synthetic reader tests do not prove that live checkpoints contain it. Default `checkpoint show` does not print Thinking plaintext; an explicit transcript path returns only content actually stored.

## Hermes source reader (HM-03)

The internal reader in `builtin/hermes.rs` parses the official JSONL session export. It keeps a row only when `active` or `compacted` is true, treats a non-empty `reasoning` or `reasoning_content` string as readable thinking, and leaves the three JSON columns as raw strings. A missing, unusable, or thinking-column-free export falls back to a read-only SQLite transaction. That open does not set `immutable=1`, does not run Libra migrations, and must leave the provider database and WAL unchanged. An `-shm` file beside that database may be updated by SQLite. Gateway, remote, and cross-user sessions are refused. Hook install writes only a marker-and-sha256 owned plugin. Review and investigate launch stay disabled. Explicit import is `libra agent import --path <session-id>.jsonl --agent hermes --yes`. It orders messages by id, keeps thinking strings unproven and out of the assistant answer, leaves absent usage unknown, and refuses a non-default profile, a non-cli source, or an unproven parent. It does not scan a Hermes home, and a replay does not write a second checkpoint. The bounded worker frame carries the same thinking as unproven and partial, and refuses the frame when retained thinking text exceeds 8 MiB. A synthetic fixture does not pass the acquisition gate. The live bridge uses that same reader for a JSONL file or SQLite database, and runs the frozen `hermes sessions export --format jsonl --session-id <id> <output-file>` argv only after it holds the export-job lease. The deadline is 2 seconds. `libra agent rpc trust hermes` pins the Hermes executable; OpenCode trust does not authorize it. Doctor reports the bridge as source-qualified and does not claim native origin. `libra agent hooks hermes observe` accepts the proven observers. `on_session_end` records a turn and does not close the session. `on_session_finalize` is the teardown and uses the shared `CaptureFinalizePolicy`. Control hooks and subagent events are not registered. The plugin forwards only identity fields and does not read the Hermes database. Hook install stays disabled.

## Hermes identity (HM-01)

Hermes is registered and supported for local capture (`support_wave=hermes`). Migration `2026103001` extends the repository `agent_session.agent_kind` CHECK with `hermes` on a dedicated connection and keeps the previous eight values, including `grok` and `opencode`. `pi` is not added by that migration. Review and investigate launch stay closed. A missing transcript path still makes `read_transcript` return an error. Global, system, and derived databases do not receive this receipt. Down refuses while a `hermes` row exists and keeps that row. `cloud sync` still refuses the kind before any remote write: `CloudError::LocalCaptureKindRefused` (`LBR-CONFLICT-002`) carries `LOCAL_CAPTURE_KIND_REFUSAL_HINT` and `LOCAL_CAPTURE_KIND_BACKUP_HINT` (plan-20260911 PI-05, ADR-PI-RECOVERY; full Pi cloud mirror stays DEFER-PI-08).

Pi is registered as a repository identity only (plan-20260911 PI-01). Migration `2026103101` extends the same CHECK with `pi` through the PI-07 dedicated rebuild (`PI_KIND_CHECK`, staging table `agent_session__pi_new`) and keeps the previous nine values, including `grok`, `opencode`, and `hermes`. `AgentKind::Pi` (db and CLI `pi`) pairs the enum, `from_db_str`, `from_cli_slug`, and `AgentKind::all()` with that CHECK. The registry row is `supported=false`, `transcript_readable=false`, `hook_installable=false`, and not launchable; the `PiObservedAgent` stub (now in `observed_agents/builtin/pi.rs`) refuses `read_transcript`, has no hooks, and is not a `PREVIEW_SPECS` row, so `agent doctor` lists no new provider. PI-02 adds internal, restricted discovery there, `discover_pi_sessions(root, layout, limits)`: the root must be absolute, free of `..`, and inside the configured provider root (`~/.pi`, under `LIBRA_TEST_HOME` in tests), so a path from a session context or the `sessionDir` of a project's `.pi/settings.json` (which Pi reads before trust) is refused with zero reads. The walk is descriptor-pinned and no-follow over the default `--<encoded-cwd>--/` layout or a flat directory. A symlinked root (or a symlink between it and the provider root) refuses the walk; symlinked entries, and entries whose descriptor-relative type is not a regular file (FIFO, device), are refused unopened. Only directories the current user owns, that neither group nor others can write, and (macOS) that carry no extended ACL object at all are walked (`UnsafeDirectory` otherwise; an ACL lookup that fails other than with `ENOENT`/`ENOTSUP`/`EOPNOTSUPP` fails the discovery; on Linux a POSIX ACL write grant is capped by the mask the group bits show; on Windows the owner must be the user, SYSTEM or Administrators and no ACE that applies to the directory may give anyone else write rights, `windows_fs::directory_entries_private`), so after an entry's type is read only that user can swap it; each session file is then opened no-follow and non-blocking and `fstat`-checked as a regular file before any read, and an entry that vanished or changed after enumeration (including before its type was read, which the shared pinned iterator's opt-in reporting mode yields as a `Vanished` entry counted against the entry bound; the default iterator used elsewhere is unchanged) is recorded (`ChangedEntry`/`SymlinkEntry`) without stopping the walk. Headers are read in 512-byte chunks, so up to 511 bytes after the newline may enter the transient read buffer; they are dropped at once, never kept, parsed, or logged, and are charged to the byte limits. Entries (checked before the next one is fetched), per-file header bytes and total bytes are bounded (`PI_DISCOVERY_MAX_*`), and any refused entry or directory, or a reached bound, reports `partial`. Each candidate carries its header line only. PI-15 adds `attribute_pi_sessions(discovery, repo_root)`, which keeps a discovered session only when its header `cwd` is the repository root or beneath it, in both layouts (the `--<encoded-cwd>--` directory name is a lossy encoding and never places a session; an explicit directory holds sessions of any cwd side by side). The header `cwd` must be an absolute string with no `.` or `..` component and is compared lexically, component by component, against the canonical repository root (on Windows without the `\\?\` verbatim prefix `canonicalize` adds, since Pi records the plain form) or the root as given; no path a header names is resolved or touched. The session id is the header `id`, never a guess from the `<timestamp>_<id>.jsonl` file name; it must match Pi's id grammar `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`, so a non-UUID `--session-id` id is kept. The header line is parsed with duplicate keys rejected. A file is skipped as `NotASession` (an object that is not a `type: "session"` header with a string `id`, which Pi does not list either) or `OutsideRepo`; it is skipped and makes the result `partial` as `UnreadableHeader` (a blank, torn, non-object, or repeated-key first line, which Pi would skip while scanning on for a header), `NoCwd`, `InvalidSessionId`, or `DuplicateSessionId` (two files of the repository with the same id; neither is returned). The walk's refusal and partial reasons carry through, and a refused root makes the result `partial`. The `.`/`..` check runs on the raw separator-split text, since `Path::components` drops an interior `.`. PI-03 adds the pure v3 normalizer `normalize_pi_session(bytes, limits)`, which keeps no content: each branch entry carries its `id`, `parentId`, entry kind, and 1-based line number. It reads only a header whose `version` is the integer 3 (`PI_SESSION_VERSION`; Pi migrates v1/v2 files in place on open, and Libra never rewrites one). Lines follow Pi's loader: a line `JSON.parse` rejects and a falsy JSON value are skipped (`partial`), a repeated key keeps its last value (`partial`), and every other non-session entry is indexed, including one without a `type`. The file is refused when its first parsed line is not a session header, when it exceeds `PI_NORMALIZE_MAX_BYTES`/`PI_NORMALIZE_MAX_ENTRIES`, when two entries share an `id`, when a `parentId` names no entry, when any parent cycle exists (on or off the branch), and when a parsed entry's place in Pi's tree cannot be reproduced (a non-object line, an `id` that is not a non-empty string, or a `parentId` that is neither absent, null, nor a non-empty string), rather than reporting an earlier leaf. A second header, an entry type outside the PI-20 frozen table, and a `message` role outside the PI-21 table (or none) make the result `partial`. The disk leaf is the last indexed entry in file order (`_buildIndex`, `session-manager.ts:1103-1121`) and the active branch is its walk to the root. Turns open only at `user` role messages; entries before the first one are a preamble that opens none. Only the latest compaction on the branch sets the boundary, as in `buildContextEntries`: its kept entry earlier on the branch, the compaction itself for retain-none, and the compaction with `partial` when the kept entry is neither. A turn that starts before it is `Incomplete`. PI-11 adds the pure projection `project_pi_content(bytes, branch)` over that branch, read from the same bytes (a line that is missing or holds another `id` fails with `BranchMismatch`). Every kept value is the file's own bytes (`PiRawJson`, borrowed `RawValue` spans), also under a repeated key, where the last value is kept as `JSON.parse` keeps it. It projects user text; assistant `text` blocks as the answer; each `thinking` block as a Pi-local `Reasoning` record that keeps the whole block (every field, including `thinkingSignature` and `redacted`), with its text readable only when `thinking` is a non-empty string and `redacted` is absent or `false`, so thinking never joins the answer; each `toolCall` block kept whole, with `thoughtSignature` exactly as written; and tool results with their text blocks, `isError` (`None` when missing, `null`, or ill-typed), `details` as written (a `null` included), and `nestedCalls` as bounded metadata of that result only (call count, `complete`, and the raw value within 256 calls and a Libra cap of 256 KiB that a Pi-written record can exceed; nested calls never become tool calls of the turn). Readable entries that open no turn become `Context` records with their source kept as written: system content and sections (in JavaScript enumeration order: array-index names ascending, then the rest in file order; removed sections skipped), a bash execution's command and output, an extension message's content, branch and compaction summaries, and a compaction's `systemMessage` (before its summary, as Pi orders them). A `message` entry with an unknown role or no `message` object, and an entry type outside PI-20, are kept whole as `Unprojected` and are partial (`UnprojectedEntry`); a present `null` in a typed field counts as ill-typed, while a `null` content reads as empty, as in Pi. `context_edit` is never applied, so a target's recorded text is unchanged. Partial: a tool result whose `toolCallId` matches no tool call earlier on the branch, `nestedCalls.complete` that is `false` or not a boolean, `nestedCalls` past the bound, image blocks (unsupported, kept unprojected), unknown blocks, unprojected entries, and missing, `null`, or ill-typed fields of a projected record (`MalformedField`). A `content` of a JSON type the message does not allow is kept whole as `Unprojected` (partial). `nestedCalls` calls are counted in constant memory before the bounds decide whether its raw value is kept. User records keep block order; a tool result's or context record's unprojected blocks follow it. Turn assignment is a single indexed pass, and section names are indexed, so neither grows quadratically. Raw JSON and reasoning text show only their length in `Debug`. Registering Pi as a reasoning provider for the coverage domain is PI-08. PI-16 adds `project_pi_session_metadata(bytes, limits)`, refused exactly when the normalizer refuses the file: like Pi's `getSessionStats`, its token totals (`input`, `output`, `cacheRead`, `cacheWrite`) and `cost.total` sum cover every entry in the file, on the active branch or not and before a compaction or not: `usage` entries of any `kind`, the `usage` of `branch_summary`, `compaction`, and tool-result entries when present, and every assistant message's `usage`. Each assistant message is attributed to `responseModel ?? model` (an absent or `null` `responseModel` falls back), never to a `model_change`, whose `modelId` can be a virtual model. A missing or ill-shaped required usage (non-integer or negative counters, no finite `cost.total`), an ill-typed model or a missing provider, a token overflow of any counter or of their sum (saturating at `u64::MAX`), a cost total that is no longer finite, and a line the normalizer skipped (it may have held usage) are partial. PI-08 registers Pi as a readable reasoning provider: `ReasoningProvider::Pi` carries the serde and transport-digest tag `pi`, the coverage wire tag (`reasoning_provider_tag`) is `pi`, and the coverage import runtime encoder and decoder now carry each import kind's own readable pair only, OpenCode `reasoning` and Pi `thinking`: the encoder refuses a reasoning record of any other provider or source kind for that kind, and the decoder restores a `pi`/`thinking` record only under `pi`. The four legacy provider tags and the frozen v2 golden digest are unchanged. Pi gets no opaque or decrypt path: every production constructor of `OpaqueArchived`/`OpaqueEncryptedBytes` checks the provider first (`restore_pending` through `validate_shape`, which requires Claude Code; `verify_import_transport` through the exhaustive `native_opaque_provider`, before any value is built, even under a valid MAC; `verify_claude_encrypted_field` builds Claude Code values only), no Pi artifact locator is valid, and `reasoning_decryptor` returns none. No command calls discovery, attribution, normalization, projection, metadata, or the Pi codec yet. `agent import` and every prepared-import entry refuse `pi` (it is outside `IMPORTABLE_AGENT_KINDS`, PI-29), and an external `libra-agent-pi` binary is refused as a built-in impersonation (`LBR-AGENT-006`). No writer stores a `pi` session yet. Global, system, and derived databases do not receive this receipt. Down refuses while a `pi` row exists, keeps that row, and points at a forward fix (the shared CHECK-rollback refusal no longer suggests deleting rows). A binary older than the migration refuses the repository without writes: the `#[ignore]` oracle gates `pi_role_oracle_refuses_a_fixture_receipt_without_writes` and `pi_role_oracle_refuses_the_migrated_repository_without_writes` run with `--ignored` against the signed v0.30.118 release named by `LIBRA_TEST_PI_ORACLE_BINARY`, pinned by its published SHA-256. `cloud sync` keeps refusing `pi` before any remote write (PI-05).

## Claude Code hook registry (CC-01)

`src/internal/ai/hooks/providers/claude/events.rs` is the only Claude event taxonomy. The parser reads lifecycle kinds from that table and does not keep a second name list. Safely installable names match the CC-00 freeze. `WorktreeCreate`, `WorktreeRemove`, and `PreModelSwitch` are control-sensitive, stay off the current ingress, and are not installed. `SessionStop`, `ModelUpdate`, and `Compaction` remain parsable deprecated aliases and are excluded from the installable set. Unknown names and unknown extra keys are partial; diagnostics are static and do not echo the payload. `agent enable` writes every safely installable event. `SessionEnd` omits the host timeout. `FileChanged` omits the matcher and does not emit `watchPaths`. `agent doctor` lists known events as installable or excluded and whether a Libra-owned entry is configured. `agent hooks claude-code observe` feeds safely installable names through the existing capture coordinator, prints nothing, and exits 0 for controllable success and failure. MessageDisplay admits 128 batches per session. The in-process gate in `events.rs` bounds one process; `HookProvider::display_batch_limit` carries the session-wide budget, which `live_pipeline::admit_display_batch` enforces with a conditional UPSERT on `metadata_kv` scope `agent_display_batch_count` (`kept` / `dropped`) before any catalog revision. Session erase deletes those rows. Control-sensitive names stay off that ingress. `normalize_claude_transcript` is the live and import reader for Claude thinking: a non-empty decoded `thinking` string becomes one `SemanticRecord::Reasoning`, while `thinking.signature` and `redacted_thinking.data` remain in the ordinary transcript bytes and are Unproven without native proof. Empty and missing stay distinct. Duplicate keys reject that line. Coverage schema stays v1 unless a readable reasoning record selects the existing v2. Authorized `checkpoint show --json` and ordinary export return the redacted thinking text and signature. List and graph do not. Live capture, import, and snapshot recovery read those attached fields from the original JSONL, with provider, session, turn, record, and content index. A duplicate key rejects that record and leaves retained bytes in place. A mismatched session file is not associated. Ordinary retention does not create a native artifact. An explicit `libra agent doctor --repair` appends one coverage revision for an authorized legacy Claude checkpoint. The revision key binds that checkpoint and the ordinary transcript content. Repeating the repair with the same content leaves the revision and the stored objects in place. Erase and the cloud mirror keep that replacement together with the old checkpoint reference, and a tombstone blocks revival.

> **Coverage-v2 source version:** The coverage-v2 descriptions below refer to the Libra 0.30.50 source line and later compatible versions. This is not a stable-release announcement; check the [official releases](https://github.com/libra-tools/libra/releases) and `libra --version` for installed support. Other sections retain their own version requirements. Coverage-v2 does not prove native origin or authorize native reasoning artifacts.

## Final OpenCode delivery scope (2026-10-18)

[plan-20260902 final C](../plan/plan-20260902.md#ordinary-final-c-20261017)
closes this wave around ordinary OpenCode capture and safe partial results.
The current ordinary source reference is official OpenCode **2.0.29**, commit
`042d0832f5f3a73eaa039a58a7d565776f6d4290`; the frozen 2.0.26 fixtures and
private `.1`/`.2` registration recipes retain their historical applicability.
Neither those fixtures nor one successful session establish support for every
export shape or model. The actual current CLI and
[ordinary source limitations](../../commands/agent.md#current-official-opencode-ordinary-source-baseline)
remain authoritative, including rejected unsupported-state historical imports
(`LBR-AGENT-020`) and bounded structural/deadline failures.

Ordinary events, prompt/model/tokens/files and supported provider-visible
reasoning use the existing typed redaction and coverage projection. An
unproven `signature`, `redactedData` or `reasoningEncryptedContent` is a private
candidate: retain partial/Incomplete classification, remove its content from
ordinary sinks and create **zero native artifacts**. A safe ordinary partial
checkpoint is possible only where the existing consumer permits it; a refused
source or pending receipt is not a successful empty checkpoint. Source A
remains UNPROVEN. No producer install, new issuer or real native-field archive
is promised by this plan. Previously published verifier/carrier and legacy
registration compatibility remain available under their existing constraints.

The native startup issue was **closed-by-scope-decision, unfixed and non-PASS**,
not rescheduled. See the [gap ledger](../gap/agent-trace.md#opencode-final-c-ledger)
for all six withdrawn cards and the original deferred inventory. Native work
requires a new explicit product decision; it has no automatic restart queue.
Linux functional acceptance remains with the user. Installed OpenCode,
exporter/hook-to-Session and account/model acceptance, including controlled
fake-exporter Session tests, run manually on macOS outside CI. Pure parser,
codec, cryptographic and sandbox/static safety tests remain eligible for CI.


`libra agent` is an intentionally different external-agent capture extension,
not a Git-compatible command.

OpenCode export on macOS requires a **Required Seatbelt server/export pair**.
Both deny `process-fork`, use private HOME/XDG roots and the exact pinned
OpenCode store, and share one authenticated loopback port. The same absolute
deadline covers trust, sealing, readiness, bounded export and clean server
shutdown; failure remains **metadata-only**, without an unsandboxed fallback.
The direct leader is disarmed after reap, preventing signals to a recycled PGID.
See [`../tracing/agent.md`](../tracing/agent.md) §5 for the platform boundary.
Linux content export is available only when the trusted system `bwrap` passes
Libra's descriptor-native `--bind-fd`/`--ro-bind-fd` safety probe; unsupported
or legacy bwrap is unavailable and remains metadata-only. Install or upgrade
the system bwrap package—there is no unsandboxed fallback.

The shared `run_bounded_exporter` Unix `pre_exec` sets both soft and hard
`RLIMIT_CORE` to zero next to the existing `RLIMIT_FSIZE`. That file-size
limit is a coarse 8 GiB backstop on every platform, not the 16 MiB export
cap: it is process-wide, so a cap-sized limit also failed OpenCode's own
SQLite writes once the store outgrew the cap (macOS FIX-SBX-01; on Linux the
`--standalone` server then exited before readiness and every capture was
metadata-only). The export cap stays enforced on stdout by the size poll and
the post-exit recheck.
Failure to set either limit fails spawn with context. Core limits propagate
to exporter descendants without changing the parent process; SIGXFSZ
disposition, byte caps, deadlines and sandbox controls are unchanged.
This also suppresses cores from unexpected exporter crashes. Piped core
handlers decide whether to honor the limit. systemd-coredump v259 honors
it when `core_pattern` passes the limit through `%c`; brief signal metadata
can remain. The regression
`opencode_export_core_limits_are_zero_in_child_and_descendants` exercises
the real runner and confirms child/descendant limits and unchanged parent
limits. It does not establish the cause of any historical linker SIGKILL.

The active development contract, backlog, and compatibility guardrails live in
[`../tracing/agent.md`](../tracing/agent.md). Keep this file as the command
development index entry so `docs/development/commands/README.md` can list every
public CLI command without duplicating the Agent planning document.

## Deferred / Non-goal parity

The following external-agent parity surfaces are decided **non-goals** for the
current wave. Each is recorded — with its handling and restart condition — in
the 「还未实现的功能」 table of [`../tracing/agent.md`](../tracing/agent.md)
(the canonical Agent contract); they are surfaced to users in
[`docs/commands/agent.md`](../../commands/agent.md) and in the `agent` row of
[`COMPATIBILITY.md`](../../../COMPATIBILITY.md):

1. **`agent add`/`remove` `--local-dev` / `--force`** — unpublished; canonical
   `status` / `enable` / `disable` (+ `add` / `remove` aliases) only. If
   implemented, each must hang on both the canonical verb and its alias.
2. **Provider-specific transcript compaction/reassemble trait** — deferred parity
   on top of the landed manifest-relative chunking (no provider-specific
   compactor yet).
3. **Optional capability traits** (`ProtectedFilesProvider`, `TranscriptCompactor`,
   `HookResponseWriter`, `RestoredSessionPathResolver`, …) beyond the landed
   `DeclaredAgentCaps` set — no public behavior yet.
4. **External-RPC method family beyond the v2 `info`/capability gate** —
   undeclared capabilities stay fail-closed.
5. **Non-first-batch supported roster** — `gemini` / `cursor` / `copilot` /
   `factory-ai` stay `supported=false` (unsupported, not hook-installable, not
   launchable) and are omitted from `agent list` entirely; the first batch is
   `claude-code` / `codex` / `opencode`. `grok` and `hermes` are supported outside that batch and are not review or investigate launchers. `pi` is a CHECK value since migration `2026103101` but stays `supported=false` and is omitted the same way. The omission of the unsupported rows is pinned by
   `tests/command/agent_roster_test.rs::agent_roster_surface`; the unsupported
   registry classification stays pinned by
   `tests/compat/agent_capability_matrix_pin.rs`.

## Private capture erasure

Doctor's terminal-finalizer cold scan uses keyset pagination in one read-only
snapshot: 32 sessions/page, 512 sessions and 128 receipt findings/run, with a
1 MiB SQL byte cap on each metadata ledger before hydration. Bad ledger rows
do not abort later valid rows. Fixed content-free notes disclose skipped keys
or bounded truncation; neither is evidence that unlisted receipts are clean.
`pending_source` survives later stop/resume as superseded manual-only evidence.
Session-quarantined receipts also remain visible; repair never overwrites a
newer session or calls quarantine successful terminal capture. Artifact-bound
budget exhaustion moves only the header, preserving the original receipt.
Eligible artifacts use the same bounded doctor executor: alias, receipt,
marker, and coverage fences are revalidated before local checkpoint replay.
Each `doctor --repair` invocation counts at most five replay attempts.
Automatic replays of eligible artifacts share one 2-second cooperative
deadline; each audited manual attempt gets its own fresh 2-second deadline.
Deferred findings (batch cap or retry-later) are not counted and require
another bounded invocation. Success requires both a durable checkpoint and strict
completion of the original receipt. An expired artifact gets one audited
manual attempt after quarantine; failures remain visible and do not reset the
original receipt policy.

Private recovery erasure uses the existing two-stage tombstone/catalog workflow:
tombstone commit and checkpoint prune precede one transaction deleting the
session plus attributable headers/chunks/aliases. Attribution is the local
canonical-repo catalog PK and capture incarnation; no key or receipt-ledger
proof is required, and old worktree/workspace scope does not block deletion.
Foreign/ambiguous corrupt evidence is retained; a content-free lost-capacity
warning is logged after commit. Without a consistent original DB backup the
capacity is lost, not repairable through a force-discard option. DB-copy backups
retain sensitive associations and can undo the local tombstone on restoration.
Receipt completion instead removes the exact artifact and only unreferenced
aliases in that same writer transaction. A trusted `SessionEnd` whose budget
expires after a complete redacted snapshot persists this artifact (ACF-13); a
later `SessionStart` may launch the bounded detached recovery worker (ACF-11)
and `libra agent doctor --repair` replays it (ACF-12).

Same-checkpoint pending/quarantine collisions are not broad deletion authority:
only owner-attributed headers are removed. Unknown/foreign headers and shared
chunks survive; explicit erase reports retained capacity, not complete removal
of every private copy.

Unknown or mismatched incarnation for a known alias refuses final deletion;
restore consistent session/alias metadata before retrying. The earlier
tombstone remains committed. One undecodable retained header blocks all
new/re-delivered recovery artifacts and destructive GC/root-walking
maintenance, and can conservatively retain completion aliases. Explicit
attributable erase still works. Out-of-band reads are capped at 17 headers and
17 associations; unexamined data may include the erased session's content,
so the warning does not certify removal of every private copy.

## Historical import contract

The active DR-05/M4 contract is implemented by `libra agent import` and is
specified canonically in [`../tracing/agent.md`](../tracing/agent.md): explicit
consent before content access/export, provider-root descriptor authorization,
typed redaction before persistence, current-repository ownership, coverage
claim + import identity fencing, and local erase tombstones. The default
`agent list --json` remains schema v1; callers opt into the method matrix with
`--schema-version 2`. The importable kinds are one set,
`agent_import::IMPORTABLE_AGENT_KINDS` (`claude_code`, `codex`, `opencode`,
`hermes`; plan-20260911 PI-29): `agent import --agent` parses against it, and
every prepared-import entry point (`import_prepared`,
`import_prepared_with_coverage_versions`, the retention and shared persistence
paths) refuses any other kind before a lease, catalog row, checkpoint or object
is written, so an embedding caller cannot store a session the CLI refuses.
Registry readability (`transcript_readable`) does not make a kind importable.
Batch limits charge bytes actually read from the held
source even when a candidate later fails validation, and the absolute deadline
begins before discovery, bounds reservation/object/CAS work, and releases every
owned uncommitted import lease on expiry. Transaction commit awaits are not
cancelled: the deadline is checked immediately before commit and the resulting
success is authoritative even if observation finishes after the deadline. A
failed abandonment is chained into the surfaced error with a doctor-repair hint.
New V2 import records retain a source only as the repository-keyed,
domain-separated `source/hmac-v2/<64 lower-hex>` commitment. New V2 subagent
content records use their separately domain-separated
`source/subagent-hmac-v2/<64-lower-hex>` commitment. Raw locators and unkeyed
source SHA-256 values are not durable metadata, claims, markers, logs, errors,
or cloud fields. A V2 snapshot digest is a distinct-domain, repository-keyed
`source/hmac-v2/<64 lower-hex>` commitment over redacted snapshot content; the
helper's transient SHA-256 is never durable. A tagged unkeyed SHA-256 remains read-only immutable V1 proof; a bare unkeyed SHA-256 has the same legacy-only status. The migration path verifies the exact scoped legacy proof and atomically moves only committed, quiescent
identity/catalog/repair-marker state. Live, partial, or repair-pending V1
state remains V1 rather than creating parallel V2 state.
Repository ownership is the canonical shared Libra
storage identity, so sibling linked worktrees are accepted while cross-repo
sources remain rejected. Import attempt markers are created in the reservation
transaction; live, export, and subagent writers use the same fail-closed
pre-object registration. The effective per-source read cap is
`min(agent.max_transcript_read_bytes, 16 MiB)`; explicit larger settings emit
the actual effective value. Discovery and held-descriptor read helper phases
run in private, kill-on-timeout processes under the command's absolute
deadline; the reader receives the already-pinned descriptor and no locator.
The command process still synchronously classifies, canonicalizes, opens, and
rewinds a source before that handoff. Thus these are helper-phase bounds, not a
strict end-to-end wall-clock deadline when NFS/FUSE stalls. A strict deadline,
FD-only wire, and autonomous timeout replay together require a long-lived owner
or a provider ABI that supplies a pre-authorized descriptor; that remains the
unresolved blocker in [`plan-20260924.md`](../plan/plan-20260924.md). Provider roots are
opened component-by-component; Claude sources and each nested Codex date
directory are opened relative to pinned no-follow descriptors before consent. Each new OID is added
as a durable provisional preclaim before its loose-object write, but becomes
deletion-eligible only after this writer wins publication and records it in
`created_oids`; a crash between those steps leaks safely instead of claiming a
concurrent writer's object. The object is compressed to a unique file in the
shared private `objects/info/libra-tmp` directory and promoted without overwrite,
with any existing final object fully validated before reuse. Fsync is conditional
on `--sync-data`/`LIBRA_SYNC_DATA`. A 64-entry bounded, 24-hour scavenger removes
only exact `.<40-or-64-lowercase-hex-oid>.tmp-<decimal-pid>-<uuid>` regular files and retains
unrelated entries. Expired construction attempts and `cleanup_pending` jobs are
retired by explicit `agent doctor --repair`/GC maintenance, not append or erase;
cleanup-pending ownership ignores the ordinary writer TTL, blocks same-session
erasure until retired, and is immediately repairable by doctor. Malformed
markers are surfaced as manual-required rather than silently skipped.
Each marker has a random writer generation, and every ownership mutation,
final ref CAS, and clear operation compares it exactly so same-checkpoint
takeover cannot be confused with the expired writer.
Rejected-object diagnostic reachability covers loose, packed, and alternate
objects under a 64 MiB per-object load-cost cap, full OID verification, a
250,000-object traversal cap, and a 30-second per-read/total deadline. Its roots
include refs, reflogs, registered worktree indexes, and sequencer state; roots
are snapshotted outside the writer transaction and revalidated before ownership retirement.
Index snapshotting runs in a killable helper under the same aggregate deadline;
no-follow/nonblocking regular-file opens, held-descriptor `limit + 1` reads, and
checksum/parsing of those exact bytes close special-file, growth, and path-reopen races.
Index enumeration is capped at 256 files/64 MiB aggregate. Any refusal preserves
durable cleanup ownership and fails closed. Inline recovery never unlinks a
shared loose object or deletes its object-index row; physical reclamation is
delegated to repository GC because index writers do not share the SQLite lock.
Rejected-writer ownership registration is O(1), durable, and blocks empty-catalog erasure until
the ownership job is safely retired. Persisted provisional ownership,
not process-local "created" state, controls zero-progress session reaping.
Retention GC physically deletes terminal ownerless import identities once no
coverage claim remains, including zero-checkpoint rows. Dry-run obtains the
same identity count by simulating coverage removal in a rolled-back transaction.

## MCP server (plan-20261019 MC-01)

`src/internal/ai/mcp_server/` is a second front end beside the Harness bridge (ADR-MC-02). It does not change the bridge allowlist, protocol version, limits, error catalog, NDJSON transport, or `agent_bridge_*` tables. MCP protocol versions, JSON-RPC codes, the 256 KiB line cap, and the tool registry live only in `mcp_server` (ADR-MC-03, ADR-MC-08).

`libra agent mcp --stdio` speaks legacy MCP `2025-11-25` and `2025-06-18`: `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`. An unknown `protocolVersion` falls back to `2025-11-25`. Batches are rejected. Unknown tools are protocol errors. Arguments that fail a tool's input schema are tool error `LBR-AGENT-027`. stdout is JSON-RPC only.

Production tool calls are translated into an internal `BridgeRequest` built by the server. Client parameters enter only allowlisted `params` fields; the client does not choose repository scope. MC-01 registers no production tool. The debug-only `libra_test_echo` tool exists when `LIBRA_TEST=1` so the tool-error path and `outputSchema` fixture can be checked. Each tool's `outputSchema` is `{"type":"object","anyOf":[<success>, TOOL_ERROR_SCHEMA]}`.

Tool results pass through `mcp_server/output.rs` before they are written (MC-16, GC-MC-03). The envelope scrubs control characters in `structuredContent`, serializes that value, runs `review::redact_untrusted`, parses it again, and scrubs once more. The first scrub keeps a JSON `\u` escape from gluing to a following secret. It then rebuilds `content[0].text` from the scrubbed value so that copy cannot keep a pre-scrub encoding. A failure is tool error `LBR-AGENT-034` with no raw fallback. The same envelope measures the full JSON-RPC response, including both copies, and returns tool error `LBR-AGENT-028` above 256 KiB (MC-19). `tools/call` waits at most 30 s for a read tool (`output.rs` `READ_DEADLINE`); a deadline is tool error `LBR-AGENT-036` with `retryable: true`. Debug builds with `LIBRA_TEST=1` also register `libra_test_blob` and `libra_test_sleep` for those gates. Read tools run in `libra agent mcp --read-worker` (MC-29), at most four at a time. The parent kills the worker process group with `SIGKILL` (Windows: `TerminateProcess`). The JSON-RPC reply returns after at most one second of reaping. If the worker is still alive, a reaper holds the slot until `wait` observes the exit, so another read cannot reuse it. The worker does a read-only hash-kind preflight and does not migrate schema, repair the object index, or start an upgrade check.

Each MCP request emits one `agent.mcp.request` span (MC-22). `method` is the original name only when it is in the server's known set, otherwise `unknown`. `id_hash` is the first 16 hex characters of SHA-256 over the canonical JSON encoding of the JSON-RPC id; the raw id is not recorded. `repository_id` is `libra.repoid`. A `tools/call` span also has `tool`, the registered tool name or `unknown`. `clientInfo`, `params`, and result content are not span fields. This is not the bridge span, which records the raw method and id.

## Bridge (plan-20260818 LB-01)

`libra agent bridge --stdio` is the repository-scoped DeepSeek Harness ingress.

- **Transport:** JSON-RPC 2.0 over newline-delimited frames on stdin/stdout.
  stdout carries exactly one protocol frame per response; diagnostics go to
  stderr (GC-LB-04). The command requires `--stdio` and has no other transport.
- **Protocol authority:** the frame/method/limit/error contract lives only in
  `src/internal/ai/agent_bridge/{protocol,transport}.rs` (GC-LB-02). The
  TypeScript plugin consumes the fixture generated from it; it must not define
  a second schema. Protocol v1: 20-method allowlist, 256 KiB frame cap, 64
  in-flight requests, 64-event/256 KiB batch, 30 s default deadline.
- **Scope:** repository/worktree/workspace/actor scope is derived from the
  trusted context at handshake (GC-LB-06/07); self-reported identity is never
  a credential. `deepseek-harness` is NOT an `AgentKind` (ADR-LB-02).
- **Non-goals:** not `libra code --control stdio`; a plain JSON-RPC 2.0
  NDJSON bridge, not a tool-serving protocol (ADR-LB-01). Implemented: the CLI + protocol + transport (LB-01), the durable
  session/event/operation storage (LB-02), the session/event ingress (LB-03),
  the typed read methods (LB-04), mutation admission/approval/actor binding
  (LB-05) and workspace lease claim/renew/release over `WorkspaceStore`
  (LB-06). As of `v0.21.1` all 20 v1 methods are implemented:
  `diff.get`/`commit.create`/`review.run`/`checkpoint.restore` reach the real
  services through the typed `agent_bridge/vcs.rs` adapter instead of failing
  closed behind the admission/approval gate.
- **Preflight:** the bridge uses the standard repository preflight (it is
  repo-scoped). It must never start without `--stdio`.
- **Publish lock:** `commit.create` takes `MaintenanceLock::shared` itself, for
  the span of that one commit. `command_holds_shared_maintenance_lock`
  (`src/cli.rs`) excludes the whole `agent` surface because an agent's VCS
  mutations are supposed to spawn `libra` as a subprocess and let the child
  hold the lock — which stopped being true once LB-05 called `run_commit`
  in-process. The hold is per mutation, never for the session lifetime (§C.10),
  so a bridge session cannot starve a deletion phase while still being ordered
  against one (§C.4.3 writer-vs-deleter). Regression:
  `agent_bridge_vcs_test::commit_create_waits_for_a_deletion_phase_before_publishing`.

## FIX-RG-SCOPED-04 confined checkpoint-input API

`checkpoint_input.rs` owns `open_scoped_run_root`, `read_run_metadata`, and `cleanup_checkpoint_input`. The helper reads `state.json` and `manifest.json` as regular no-follow leaves (64 MiB each, 128 MiB together) and clears `checkpoint-input` under a trusted absolute runs root while the directory namespace remains stable. Budgets are 8192 entries, 4096 files, 4096 directories, depth 64, 4096 path bytes, and 8 MiB total path bytes. It does not decide run kind or terminal state.

On Unix, held descriptors pin objects, not their current ancestry. The helper rechecks identities but cannot atomically bind those checks to chmod/unlink. A concurrent directory move in that last gap can affect an object after it leaves the trusted root; detecting the move afterward does not undo the effect. This unresolved guarantee and the complete ownership/exit protocol are outside this plan as `DEFER-RG-SCOPED-04`, by the user's 2026-10-10 decision. Callers must keep the namespace stable; the API does not enforce that precondition. This qualification also applies to scoped resume and store cleanup consumers.

The original `fix_rg_scoped_04_preflight_budget` combined regression remains present and ignored/UNRUN. The active `fix_rg_scoped_04_preflight_limits` retains its size-boundary and detectable input-change assertions, without running the deferred relocation cases. Neither its PASS nor a default full-suite PASS qualifies concurrent relocation. Windows execution is the `compat-scoped-input-windows` job on the actual main push SHA or verified PR merge/head. Its four active fixtures must be present, nonignored and actually pass; the original deferred fixture must remain listed as ignored. A Windows release build is not that evidence. Linux operator review stays UNRUN.

## FIX-RG-SCOPED-02 scoped resume caller

`materialize_validated_checkpoint_input` is the production entry for investigate start/continue and review scoped setup. It passes the absolute storage path, saved spec and actual caller budget to the shared checkpoint reader. The dedicated read-only catalog pool closes before a joined blocking owner validates the exact manifest/tree roles, loads typed ordinary blobs, invokes SCOPED-04 cleanup and writes ordinary files. Legacy full-list specs may name closed `reasoning/encrypted/<64 hex>` entries only as exact manifest/tree artifact tuples; those entries are excluded without opening their payload. Wrapped-v1 checkpoints without a manifest remain supported. Unknown, altered, duplicate or aliased entries, an id that is not a blob, and a blob content-hash mismatch are refused before cleanup. Ordinary files retain the 64 MiB per-file and 256 MiB total caps. The shared traversal separately bounds ordinary leaf trees to 16 MiB and ordinary child trees to a charged 16 MiB aggregate; the combined 32 MiB does not include wrapper, manifest or reasoning-tree proofs, which have their own bounded reads. The query phase uses the smaller of the caller's absolute deadline and its initial 200 ms phase budget, with awaited pool closure before filesystem handoff. Investigate samples its persisted remaining run budget once, before deriving its absolute deadline. Review setup starts immediately before validation and uses `phase_started + request.reviewer_timeout`; later reviewers retain their full original timeout. Both callers use their actual cancellation handle and existing run cancel marker. A paused continue with `CatalogUnavailable` returns the original store error before changing state or input; deterministic proof failures fail closed. The filesystem owner is cooperatively bounded and joined on cancel/deadline. SCOPED-04's documented concurrent namespace relocation limitation remains `DEFER-RG-SCOPED-04`; this card does not strengthen that guarantee. No schema, serialized field or stable error code changes.

## Reasoning type contract (RG-01)

The provider-neutral type contract defines `provider_visible`,
`encrypted_unavailable`, `opaque_archived`, `not_present`, and
`unsupported_shape`. `encrypted_unavailable` is not a capture failure: it
means a provider-declared encrypted field lacks an authorized decryptor.
Unknown reasoning-like source shapes produce a payload-free warning; ordinary
unrecognized high-entropy or base64 content remains `not_present`. A block
selected for encrypted-field verification must itself be a declared reasoning
block: selected text/tool blocks fail as `unsupported_shape`, even if they
carry an undeclared signature-like key. Adapters classify ordinary text/tool
content before calling the encrypted-field verifier.
`ReasoningRecord` contains safe enum metadata only. The reasoning module
validates the record, block type and JSON-string field type before producing
an in-memory `OpaqueEncryptedBytes` for authorized Claude assistant
`thinking.signature` / `redacted_thinking.data` sources. Escaped UTF-8 wire
bytes are not JSON re-serialized or base64-decoded. A matching JSON shape
is not a provider credential; source authorization precedes verification.
OpenCode 2.0.24 `reasoning.state` is open-ended and is not a declared
ciphertext field. `OpaqueEncryptedBytes` cannot be serialized, displayed, or
converted to `RedactedBytes`; this type contract does not yet capture or store
encrypted reasoning. RG-04 owns readable reasoning projection and RG-02/03/05
own artifact storage, export, mirroring, and erasure. No live provider adapter
is connected by RG-01.

This type-contract verifier accepts only the minimal synthetic envelope; real session records and additional metadata are deferred to the reviewed provider adapter contract.

## Readable reasoning projection (RG-04)

RG-04 adds the readable-reasoning projection path:
`ProviderVisible` reasoning text is the only reasoning content that can enter
the coverage projection, and it always passes typed redaction first; canary
secrets inside reasoning text are counted in the redaction report and removed
before projection, checkpoint persistence, or any metadata/digest write.
Readable reasoning projects under its own `reasoning` record type with
`provider` and `source_kind` metadata; it never impersonates an assistant
answer and is never written to unredacted metadata or logs.

Direct serde serialization/deserialization of `ProviderVisibleText` is deliberately rejected. Callers must use classification and typed redaction followed by `canonical_turn_bytes` or `safe_turn_projection`. RG-04 does not wire a live provider adapter.


RG-02 adds the artifact archive: verified ciphertext is stored byte-exact under
`reasoning/encrypted/<sha256>` and declared in the manifest's `reasoning_artifacts[]`
array; dedup by sha256, duplicate-locator rejection, and 512/256 KiB/32 MiB
fan-out budgets are all fail-closed. `content_hash` coverage stays four-role.

Opaque artifact writes alone use standard zlib stored blocks (level 0).
Canonical blob bytes and Git OIDs are unchanged. High-entropy ciphertext uses
more disk and mirror bytes; ordinary objects keep default compression, and
valid existing objects are reused without rewriting.

### Controlled opaque reasoning export

Libra preserves provider-encrypted bytes; it neither owns nor guesses decryption keys. Readable reasoning text passes typed redaction; a provider signature authenticates provider output, redactedData represents a provider-redacted reasoning block, and reasoningEncryptedContent is opaque provider ciphertext. None of these metadata labels alone proves native origin. The existing native source requirement remains in force.

`checkpoint show` keeps its fixed human-readable safe summary. `checkpoint show --json` adds `reasoning_artifacts[]` only when a checkpoint declares artifacts. Each item exposes only `provider`, `source_kind`, `locator`, `byte_len`, `sha256`, `availability`, and `decrypt_capability`; it never exposes artifact paths, object ids, arbitrary metadata or bytes. Default show, transcript export, scoped input and skill readers do not read artifact bodies. An invalid checkpoint layout or manifest fails closed without reflecting its contents. A valid catalog root whose object is not yet available keeps the legacy safe summary, omits the optional artifact array and emits a fixed content-free diagnostic; present corrupt roots still refuse. This compatibility fallback never authorizes export.

```sh
libra agent checkpoint show <checkpoint-id> --json
libra agent checkpoint export <checkpoint-id> \
  --reasoning-artifact <lowercase-sha256> \
  --allow-raw --justification "operator recovery" -o <new-private-path>
```

Opaque export requires all three authorization/output options, a lowercase SHA-256 and a new destination. Do not combine it with `--raw`. Authorization is checked and an independent `reasoning_artifact_export` audit row is appended before checkpoint lookup, artifact body access or file creation. A grant records the authorized attempt; later lookup or file failure retains that audit row. Audit failure stops the operation before body access.

The selected artifact must belong to the selected checkpoint. Export verifies the closed manifest/tree role, complete blob object identity/type, SHA-256 and any declared byte length. The maximum is 32 MiB; truncated prefixes are refused. Bytes bypass redaction and are never decoded, sent to stdout, used as model input or decrypted. macOS/Unix writes a 0600 temporary file beside the destination and publishes it without overwriting an existing file, symlink or directory. Missing parents are not created; failed writes remove the private temporary file. Platforms without this owner-only permission primitive refuse with `LBR-AGENT-013` and a denial audit. Windows private ACL export is deferred; default metadata and ordinary transcript readers remain available.

The `ReasoningDecryptor` registry returns `None` for Codex, Claude Code and OpenCode. `decrypt_capability` stays `none` and encrypted bytes remain `encrypted_unavailable`; a future official, explicitly authorized provider capability must enforce checkpoint/provider scope. This release implements no decryptor and accesses no decryption keys.

Implementation owners: the CLI selector, fixed audit action, metadata-only helper, separately audited complete-body verifier, private no-clobber publication and provider-scoped empty decryptor registry. Shared role/proof budgets and existing body-read observer are reused; no SQL migration or key/config/environment/network discovery.

RG-06 adds artifact tree reachability; RG-03 adds controlled read and export.


## Shared checkpoint input proof (FIX-RG-SCOPED-01)

`internal::ai::checkpoint_reader` owns the typed header and content-addressed identity check, ordinary-role traversal, and manifest/tree closure. Command readers use this owner; the fresh scoped resolver queries `storage.join(util::DATABASE)` through a dedicated SQLx readonly connection with creation disabled. The query phase has one absolute deadline capped at 200 ms and the caller's remaining budget; it does not initialize schema or reuse the cwd-selected writer pool. Filesystem work receives authority only after the query lease and dedicated pool have closed. Failures refuse the input and cannot authorize materialization.

SQLite contention is refused immediately (`busy_timeout(0)`). Acquired connection and pool closure are always awaited; an authority that finishes after the deadline is refused. The 200 ms phase deadline does not promise a 200 ms bound on awaited cleanup or that a timed-out lazy connection establishment thread has already exited. Default reader budget refusals keep their existing store-inconsistent message; fresh scoped layout, integrity and payload-cap failures retain the checkpoint-specific fatal context and inferred error code.

Fresh scoped traversal-limit refusals keep the existing store-inconsistent message and `LBR-AGENT-009`, while an empty wrapped leaf keeps its original scoped fatal message. Shared role proof returns its complete map; both fresh and saved consumers refuse an empty map before granting materialization authority.

The saved-spec proof API reserves at most 4608 saved entries, 4096 UTF-8 bytes per path and 8431616 cumulative path bytes before path normalization or map allocation. It then requires the complete ordinary path-to-OID map of the named wrapped checkpoint. Ordinary files retain their separate 4096-file and 8 MiB path budgets. A legacy full list may exclude at most 512 entries only when each exact 84-byte `reasoning/encrypted/<sha256>` path and OID agrees with both the manifest and artifact tree. Prefixes, OIDs alone and ordinary subsets are insufficient. Proof metadata reads are recorded separately from ordinary-body reads, and proof never reads artifact bodies.

This card supplies the shared internal API. Persisted review/investigate recovery is connected by FIX-RG-SCOPED-02, and filesystem ownership is supplied by FIX-RG-SCOPED-04. Their acceptance and the final aggregation full suite remain separate requirements. Synthetic role fixtures do not establish a native producer origin or authorize OpenCode reasoning archival.

The ordinary `checkpoint show`/`list` summary remains a closed whitelist. RG-03 enables the optional artifact metadata projection for JSON show using bounded role proofs, without reading artifact bodies; human show and list retain their existing summaries. Skill projection now passes checkpoint id, tree oid and metadata oid to the shared role proof; corrupt or unbound metadata is skipped without exposing its body. Raw export authorization and audit are unchanged.

The macOS local content gate is `local_capture_smoke_opencode_macos` in
`agent_local_capture_smoke_test`. It requires an explicit local-agent opt-in,
pins OpenCode 2.0.26, selects `volcengine-agent-plan/glm-5.3-flash`, and checks
the disposable project before requesting a single fixed file write. The gate
checks captured prompt/model/tokens/files against the source projection,
persisted file metadata, stopped sessions, doctor and uninstall retention.
A failed or timed-out preflight is a failure, with no model request started.
The historical 2.0.26 run failed server readiness and remains recorded as
`DEFER-OG-OPENCODE-BOOT-01`, not a passing content gate. Current 2.0.29 ordinary
automatic-capture evidence has its own source/version boundaries in the plan;
it does not retroactively pass that old smoke or prove native reasoning.

### Scoped run cleanup consumers

FIX-RG-SCOPED-03 consumes the immutable checkpoint_input bounded metadata and cleanup APIs in both stores, at clean_run and direct clean_all. Type/schema/run-id/terminal proof happens before input mutation; parseable foreign, nonterminal or incomplete state vetoes manifest fallback. Only explicit corrupt/missing-state cleanup may use a valid own terminal manifest. Bulk skips corrupt/unknown/foreign/nonterminal candidates and counts actual removals. Investigate CLI retains its existing lock; library bulk acquires a per-run lock, while review makes no OS-lock claim. Each input cleanup uses one cooperative five-second deadline. Each of two fixed metadata snapshots is bounded to 64 MiB/file and 128 MiB/pair (at most 256 MiB read work); initial bodies are dropped before rereading, identities must match before deleting the run. Input/root metadata no-follow checks remain solely in the delivered helper. Preflight limits fail before chmod; I/O/deadline or changed metadata after cleanup can leave partial input, return an error and never count a removal. Whole-run filesystem deletion retains the existing remove_dir_all behavior; synchronous filesystem calls are not forcefully preempted. Namespace concurrency remains the user-deferred limitation, no new ownership guarantee. No schema, target, dependency or serial-lane changes.

A proved own-kind terminal state remains cleanable when its matching manifest has no terminal marker after an interrupted state-first finalization. Identity/schema/common-field conflicts and differing non-null terminal values remain refused. Bulk qualification I/O, link or size refusals return an error; already-removed earlier runs are not rolled back. A proved own-kind terminal state also remains cleanable when its manifest is missing. A terminal review infrastructure failure may have no reviewer outcome rows while its manifest retains the requested reviewers; that supported shape is cleanable. An unparseable manifest is still refused even beside a proved terminal state: preserve the run and repair its metadata before retrying.


### RG-05 reasoning artifact lifecycle

Cloud publication validates manifest role, canonical tree, `object_index`, decoded `byte_len` and full SHA-256 before beginning a capture generation; existing upload repair and bounded artifact readback preserve original bytes. Exact checkpoint-tree and artifact-index evidence is rechecked against the final fenced generation. The registered object helper carries metadata evidence only and shares the caller's absolute deadline.

Session erasure hides artifact references; `agent clean --gc` and repository GC reclaim unreachable local objects. Doctor emits the read-only `inconsistent_reasoning_artifact` finding with canonical `checkpoint_id`, `manual_required=true`, and no persistent status column. Physical R2 payload deletion remains deferred; tombstones remove mirror catalog rows and fence normal restore. Non-quiet `agent enable` and `agent import --yes` disclose archival of provider-encrypted reasoning original bytes and checkpoint cloud mirroring without decryption. Synthetic fixtures remain `encrypted_unavailable` while visible, with no references after erasure; native source proof and the unresolved OpenCode Source A gate remain unchanged.


### Authorized reasoning source carrier (OG-16)

The selected registered adapter has an optional pre-redaction source preprocessor, disabled by default. It separates only an already authorized, bounded in-memory source; it cannot reopen provider files or access the checkpoint store. With no capability or no candidate, ordinary snapshot, live and store bytes retain their existing behavior. When enabled, candidates are removed before the ordinary Redactor. Unproved or unmapped candidates retain a payload-free partial classification, including a partial with zero artifacts; they cannot be converted to ordinary complete content.

A sealed native carrier moves original opaque bytes and their source-bound proof together into the shared checkpoint payload. Native issuer, provider, source session, message, part, field, wire SHA-256 and authorized capture/turn association are validated before object or index writes. The metadata-only identity adapter cannot expose payload bytes or mint native authority. Debug output does not contain native coordinates or payload. Source, artifact-byte and artifact-count limits remain enforced.

The authenticated private import transport carries a native-proven typed collection from the authorized descriptor helper to the parent. A one-request key binds the helper response to the request, authorized source, repository scope and collection; the parent verifies that binding and retains the complete native proof before assigning owned items to reserved turns and the shared checkpoint store. An unauthenticated or unproved nonempty collection fails closed, without treating a wire DTO as native authority or returning candidate bytes to the ordinary transcript. Empty preparation keeps its existing response and source projection bytes. Pending v1 still refuses nonordinary payloads before writing an envelope; pending v2 accepts only a validated borrowed native carrier and its prepared identity plan under the existing receipt, fence and lease rules. For an enabled Claude deadline-FD route, preparation is rejected before source resolution, without an ordinary fallback. An existing Pending receipt prevents the wrapper failure diagnostic; NotTracked retains only its existing bounded diagnostic attempt and does not advance sync_revision.

Production provider capabilities remain disabled and no production native issuer is admitted. Synthetic positive fixtures prove the carrier and import transport mechanism only. Source A remains UNPROVEN and real native field admission is outside this plan’s final scope; unproved OpenCode fields are not archived. Durable reasoning replay still requires its later gate. This mechanism adds no decryption capability or public metadata enum value.


### Pending reasoning v2 codec (OG-17)

The private pending codec pairs its closed inner payload v2 with the live SessionEnd writer and authenticated reader. When an admitted producer supplies a bound native-proven collection, the writer seals its source record and explicit per-turn `verified_nonempty`/`verified_empty` identities. Ordinary pending payloads remain byte-for-byte v1; the outer header and envelope remain v1 with the existing MAC domain, 64 MiB envelope, 128 chunks and 512 KiB-per-chunk limits. A pre-v2 reader rejects v2 and retains its evidence. The current reader checks the original whole-envelope MAC, canonical member/claim association and current catalog, claim and marker fences before the lower reasoning issuer validates the fixed source proof. It restores only reserved-turn artifacts and commits their original stable identities with the checkpoint, claim and ref transaction. A failed append retains the pending artifact and claims; each retry reauthenticates the original bytes and current fences without reopening a provider source or minting a replacement key. Missing or damaged private-key/source evidence requires diagnosis and manual recovery. A decoded DTO or envelope MAC alone never proves native provider origin: unproved fields remain partial and are not archived. Production native issuers and real OpenCode Source A evidence remain disabled; synthetic tests exercise this transport without claiming real native archival.

### Private reasoning identity and garbage collection

Garbage collection excludes the reserved `agent_reasoning_identity` scope from generic metadata object-reference scanning. This applies to the entire stored value, including a damaged value containing a JSON object ID or a nested object ID; those values do not trigger object hydration. This exclusion requires no private reasoning key. Ordinary metadata object roots, private capture recovery exclusions, and capture pending parent roots retain their existing rules. Forward recovery must retain both the private rows and this exclusion while a reasoning identity writer is stopped or repaired. This guard does not create identity records, issue native provenance, or enable OpenCode reasoning archival.

<!-- Compatibility anchor for earlier links; not a release-status assertion. -->
<a id="versioned-graphdoctor-readers-cov2-09-unreleased"></a>

## Versioned graph/doctor readers (COV2-09, Libra 0.30.50)

The graph reader decodes each selected claim and revision through the existing
closed `CoverageSchemaVersion` enum before joining or skipping unmatched rows.
Both revision history and current claims use `(logical_turn_key, coverage_schema_version)`;
v1 and v2 rows sharing a key/checkpoint remain distinct. The public graph JSON
schema remains 1. Unknown stored versions return the existing
`LBR-AGENT-009` error with upgrade/doctor guidance, without an indexed-success
response or candidate content. No version coercion, catalog mutation or blob
read is added. An unmatched revision is still validated before it can be ignored.

Doctor already joins conflicts by both key and version; its production code is
unchanged. Each conflict remains a distinct content-free, manual-required finding
in JSON and human output, with or without `--repair`. Synthetic catalog fixtures
check version separation, envelope stability, unknown-version failure and raw
identity/content canaries. They do not run an OpenCode Session, exporter or model,
and confer no native-origin proof. These reader guarantees require Libra 0.30.50 or later; upgrading the installed
binary is necessary before relying on them.

<!-- Compatibility anchor for earlier links; not a release-status assertion. -->
<a id="coverage-v2-capture-unreleased"></a>

## Coverage-v2 capture (Libra 0.30.50)

From the first stable release containing coverage-v2, OpenCode export capture
and historical import retain supported provider-visible reasoning as typed,
redacted turn content. Turns containing that content use coverage schema 2;
ordinary turns keep schema 1 and their existing canonical bytes. Redaction runs
before either digest or checkpoint projection. This does not authenticate
`signature`, `redactedData`, or `reasoningEncryptedContent`: unproven native
fields remain partial and cannot be archived as trusted artifacts.

After upgrading, an interrupted action whose checkpoint was already committed
with v1 (or an empty legacy revision set) first settles that existing receipt
without adding claims or rewriting its checkpoint. A subsequent **new action**
may capture the visible reasoning once under v2; repeated capture then deduplicates.
The old v1 objects, digests, and claims remain readable and unchanged.

If source classification is rejected after that session already has v2 claims,
the entire new capture/import attempt stops, including new ordinary turns.
Its pending receipt/export work stays retryable; no fallback claims or success
acknowledgment are created. An unchanged over-limit or unsupported source can
keep that session blocked. Keep the recorded data, inspect `libra agent doctor`,
and start a new OpenCode session to continue supported capture. A candidate or
unverified reasoning source cannot turn this failure into complete coverage.
These changes add no command flags and require Libra 0.30.50 or later. OpenCode Session/exporter/model acceptance runs locally on
macOS; it is not a CI integration test.

## Codex parent/child usage snapshots (CX-26 development)

Unreleased source builds may add `usage_attribution` to checkpoint metadata
for Codex UUID sessions. Its `scope=session_snapshot` and `as_of=live_capture`
or `as_of=import_pass` describe the observed whole-session snapshot, not a billing
delta. Import checkpoints from one pass share the same observation. Do not add
these values across checkpoints. `parent_token_usage` and `child_token_usage`
retain known counts; missing usage stays null. Optional cached/reasoning/total
counters stay unknown unless every contributing operand supplies them.

The projection rechecks current scoped child claims, revisions, digests, links
and capture incarnations. It includes at most 16 source/revision/checkpoint
references. Each reference's `parent` is null for the current thread or the
parent source commitment for a nested descendant; original native identities
remain in the separately retained ordinary child source. Partial source counts
describe bounded observations, not an
exhaustive inventory. Unknown, busy, stale, damaged or missing sources never
become measured zero. Child thinking remains independently exportable when
usage is unknown. Legacy session labels and other providers omit this field.

An available, exhaustive descendant scan with current durable claims and complete
parent/child usage can produce `children_coverage=complete` and a checked
`aggregate_token_usage`. Otherwise `partial=true`, `aggregate_token_usage=null`
and `aggregate_includes_child_usage=false`; known subtotals remain available.
Import saves children before parent metadata and preserves independent durable
progress on either writer's failure. Replays do not refresh an old parent's
usage snapshot or create a checkpoint solely because a child grew.

`excluded_legacy_count` counts only older capture namespaces proved by the
bounded commit/tree/metadata role chain and their recorded source commitment.
Unprovable older records remain unknown. The scan examines at most 16 claims
plus one saturation witness, with 64 KiB per role object and 1 MiB total;
large historical catalogs, unavailable parent spawn evidence (including its
1 MiB cap), or an exhausted discovery window can keep coverage partial.
Existing checkpoints and sealed recovery artifacts are not rewritten. New
artifacts carrying this optional metadata need a build that recognizes it for
recovery; keep the original artifact when using an older build.

Credential redaction can hide records or nested usage fields. Such usage is
partial or unavailable even when the retained JSON remains valid. Ordinary
readable thinking and opaque values keep the existing retention/export
contract. All new Codex functional checks remain feature-gated, ignored, local
tests; they are not CI requirements.

## Codex ordinary thinking retention (CX-15 development)

Codex `response_item` reasoning is ordinary authorized source data. Preserve
`summary_text`, `reasoning_text`, `encrypted_content` and unknown associated JSON
values after decoded credential scanning; do not turn ordinary retention into
native proof. Unproven/Partial sidecars contain counts and explicit-turn token
snapshots, never text or ciphertext. Preserve missing/null/empty distinctions.

Historical import uses a closed, bounded `CodexRetentionDto` beside its existing
semantic/native handoff. The frozen V1 normalizer attributes source slices to
turns without including them in claim digests. Each import snapshot retains a
compaction record once in its first V1 slice and leaves it unattributed in
statistics. A later changed snapshot can append a replacement slice containing
earlier history; previous checkpoints remain immutable. The ordinary transcript contains the V1
projection line plus source JSONL; default export reads every line. Previously
covered turns can receive append-only supplement checkpoints when ordinary values
change. Supplement IDs derive from a repository-keyed content commitment, and
the current gate-issued committed claim is checked at marker bind and inside
the ref/catalog transaction. The same import lease, workspace/tombstone fences,
checkpoint writer and object-index barrier apply; no semantic revision advances.
V1 objects/digests/claims remain immutable. CX-05 owns V2 claim migration.

The data-only V2 preview includes provider-visible reasoning using the same
credential-scanned text that ordinary retention saves. It is not an activation
of Codex live V2 claims. Token statistics use the latest explicit turn snapshot,
not a sum of cumulative counters; resets or unknown association stay partial.
Unknown/opaque fields remain ordinary data, without native artifacts or issuers.
A large non-reasoning line cannot invalidate the whole snapshot. Malformed
string literals are masked locally while leaving structural partial evidence;
statistics exhaustion cannot reject otherwise available ordinary data.

Synthetic acceptance covers actual CLI import/store/default export, source
immutability, opaque-only updates, replay, unchanged semantic claims, transaction
failure and recovery, decoded credential escapes, numeric fidelity and bad tails.
This is not a live model acceptance. System Codex hooks, current rollout parser,
terminal handling and real TUI smoke retain their own later CX gates.


The stored sidecar is a snapshot of explicit Codex provider turns (`turn_ordinal_namespace=codex_explicit_turn`); its ordinals are separate from V1 checkpoint attribution. Metadata-only provider updates do not create a checkpoint. The full turn table lives in the shared `redaction_report.json` object; each checkpoint's metadata carries bounded aggregate counts. A statistics/retention preparation failure is reported as partial or `retention_unavailable` while existing V1 projections remain usable.

At most 16 retention supplements are written in one import pass before fresh turns proceed. A partial result reports durable progress and releases owned reservations; rerunning resumes the remaining copies. Fresh retained checkpoints and supplements use repository-keyed IDs bound to semantic digest, completeness and retained values. Identical replay remains a no-op after repack. Changed-data writes after a complete traces repack still hit the existing packed-parent writer limitation; preserve the source and repository for repair. The local packed-transcript case uses a valid mixed store and does not claim whole-traces packed-parent support.

Codex CX acceptance tests are operator-local, including synthetic cases; they
are not scheduled in CI. Run `cargo test --features test-live-agent --test
codex_capture_local_test -- --ignored --test-threads=1` and the companion
`cargo test --features test-live-agent --lib codex_summary_ -- --ignored
--test-threads=1` explicitly on the operator machine. Ignored/unrun cases
provide no acceptance evidence.

The revised legacy Codex boundary case is also operator-local:
`cargo test --features test-live-agent --test agent_checkpoint_redaction_test capture_foundation_codex_reasoning_boundaries_live_and_import -- --ignored --exact --test-threads=1`.

Empty semantic turns omitted by the frozen V1 projection do not shift another turn's retained thinking. Surviving source slices follow their original turns when ordinals are renumbered. Thinking from an omitted turn is preserved once with the first ordinary checkpoint, with `turn_attribution_unproven=true` and `partial=true`; this does not claim that the text belongs to that checkpoint's semantic turn.

The sidecar statistics budget is 4096 distinct explicit turns or reasoning identities; duplicate replay does not consume another identity. Exceeding that budget marks statistics unavailable and partial; the ordinary source still preserves the thinking records.

Current source builds use the Codex 0.162.1 twelve-event contract, including `Interrupt`. Re-run `libra agent enable --agent codex` to refresh only Libra-owned handlers and trust entries. `Interrupt` and `SessionEnd` use a 3-second host timeout and a 2000ms capture budget; other default handlers use 30 seconds and 29000ms. `Interrupt` shares the installed `stop` command but remains `turn_interrupted`: it preserves the current session phase and writes no normal completed-turn checkpoint. `PostCompact` shares `compaction` while retaining `compaction_completed`. Existing event identity ordinals and user trust/configuration remain unchanged. This event support does not prove rollout thinking has been captured.

The minimum Codex CLI version for this twelve-event installation is **0.162.1**. Upgrade older Codex clients before re-running enable. Existing eleven-event installs report not installed until refreshed; their already-approved entries do not become trust gaps. This also fixes earlier rejection of `PostCompact` on the shared `compaction` verb. Late interrupts preserve an already stopped session instead of reactivating it.

Codex hook settings reads accept regular files only: `hooks.json` and `config.toml` are limited to 16 MiB each, and the integrity recovery journal to 64 KiB. Installation, removal and status checks refuse symlink/special-file leaves; Unix FIFO opens cannot block the reader. An unsafe or oversized file is left in place; move it aside or reduce its size before retrying. A generated update over the settings limit is rejected before either paired file is published. Parse diagnostics identify the file without printing configuration contents. Parent directory aliases selected by `CODEX_HOME` remain supported, while Unix trust keys use the canonical `hooks.json` path; rerunning enable removes older Libra-managed entries under the current alias. This is not a strict whole-process filesystem deadline.


Codex installation diagnostics in `libra agent doctor` include all 12 managed events. Each event has a fixed `reason`: `configured`, `missing`, `drifted`, `untrusted` or `disabled`; JSON exposes these rows under `data.provider_hooks[].events`. Aggregate `installed=true` requires every managed event to match the current handler and be enabled and trusted. Edited or stale duplicates carrying Libra's status message keep that event unconfigured; marker-less user handlers are outside this diagnostic. An intentional `enabled=false` is reported as disabled without adding a trust-gap banner when its hash is current. These read-only settings checks never print hook commands, hashes or configuration bodies and do not prove that the host executed a hook. Use Codex 0.162.1 or newer; refresh missing or untrusted Libra handlers with `libra agent enable --agent codex`, and review intentional disables before running enable. For drifted entries, inspect `hooks.json` and the matching `[hooks.state]` entry in `config.toml` first: enable refreshes handlers whose ownership is still provable, but preserves malformed handlers that only carry the status message. Those require manual correction or removal after review; repeating enable alone may not clear their diagnostic.


## Codex turn attribution (CX-05 development)

The first nonempty persisted record pins the typed coverage preview's codec for the entire source snapshot. A `session_meta` header recording `cli_version: 0.162.1` or `0.163.0` selects the current structured codec; other or missing versions keep the legacy codec. Appending current markers to an older resumed session cannot reinterpret its prefix. Supply the complete source snapshot; an isolated tail is not an equivalent coverage source. The owning header remains in the ordinary source. Future automatic writer activation must persist this codec choice with the session rather than infer a new choice on retry.

The current codec groups inputs by validated `turn_id` from `turn_context`, lifecycle markers or accepted input metadata. Steering inputs in one turn share a group. Scoped responses and late input mirrors do not rewind the active stream context. An unscoped or matching-turn terminal marker closes it. A malformed terminal scope leaves the stream open and qualifies the projection. An id-less optional turn context preserves a lifecycle-established scope while qualifying the projection; without lifecycle proof it clears the context. An explicit null or invalid turn ID clears it. Fresh raw input scoped to an older turn stays in that group and qualifies ordering, while following unscoped output keeps the active turn. Missing or invalid turn IDs use incomplete ordinal compatibility keys based on group creation order, separately from dense projection ordinals; earlier empty groups becoming visible do not shift those keys.

Producer `content_item_kinds` and accepted-input events distinguish human requests from injected context. Developer/system messages, inherited history and foreign-thread records do not create human turns. Genuine text resembling a context wrapper is preserved. Repeated input on one surface remains repeated. Per-turn occurrence pairing collapses matching event/raw/completed surfaces; raw, client and completed-item IDs use independent namespaces; missing or unequal IDs leave a pair partial, so ordinary current sessions are normally partial. Non-text content, conflicts and unpaired event inputs also remain partial. Only annotated raw or scoped completed input can establish a user record; unscoped legacy accepted-input events corroborate matching inputs and never independently establish human turns; mismatched mirror text cannot add inferred user records beside the more direct source. Legacy role-only projections may include injected context and do not prove human origin. All source values remain available to the existing ordinary retention path under credential scanning.

This data-only path reuses the existing response codec and V1/V2 selector: typed readable reasoning selects V2; ordinary records still select V1. Historical V1 normalization, ordinary-retention writes and existing claims keep their prior keys and digests. Automatic Codex writer activation remains later work. Current-codec ordinary preview rows labelled V1 must not be persisted against legacy V1 claims: activation must explicitly use the existing ordinary V2 domain or keep sessions with V1 claims on the legacy codec, alongside a persisted codec choice. Explicit prepared ordinary V2 imports already use the shared replay gate. The reasoning sidecar keeps its separate `codex_explicit_turn` ordinal namespace; its counters must not be joined to coverage ordinals by position.

The existing snapshot hard cap is 16 MiB. The current codec also caps grouped bytes at 16 MiB, each JSONL record at 1 MiB and turn, input-occurrence and identity collections at 4096 entries. A large patch, tool output or inline image data URL can make this pure preview unavailable. Exact-version compatibility and large-record availability must be reviewed before automatic writer activation. Exhaustion or an expired deadline fails the preview; empty or unattributed partial groups retain a partial signal. Damage within an attributable group qualifies that group, while unassigned damage qualifies the snapshot. Ordinary retention follows its existing path. Source and synthetic checks do not establish live model capture or native provenance.

## Codex rollout metadata and capabilities (CX-04/CX-14 development)

CX-10 historical import discovers both date-partitioned `sessions` and flat
`archived_sessions` under the configured Codex home. The descriptor-pinned
discovery helper reads the first record only for `--since` (1 MiB per file,
256 MiB cumulative; 4096 unfilterable-record cap), requires a matching `session_meta.payload.id`, validates
an optional bounded `cli_version`, and uses its RFC3339 session start time.
Invalid/missing per-file headers and read failures produce hashed-id `skipped`
entries with distinct fixed reason keys while other sessions continue. Each
skip records a warning for `--exit-code-on-warning` and JSON warnings. A bad
file later found by child-content discovery, including an unselected archived
duplicate, may still make the parent import partial with `LBR-AGENT-018`. An
exhausted cumulative or entry budget stops discovery with `LBR-AGENT-018`.
The `--since` window cannot safely prune files before the header read; active
and archived scans have separate 20,000-entry bounds. Use a closed Codex index
backup to page absent sessions or `--path` for a specific or existing session
when the bounded batch scan is too large. The archive-aware `--session` finder
has its own 20,000-entry directory cap after at most 10,000 active rollout
files; live hooks retain the original 4096
per-directory cap. `--all` defers header validation
to the normal import reader. File mtime never supplies Codex `--since` time;
Claude retains its mtime filter.
Duplicate IDs choose the active root, then the lexically newest rollout in a
root. Interactive batch consent precedes that header read, and the existing
authorized file reader reopens and fences the selected source before full
transcript bytes. Codex-specific new regressions stay feature-gated and
ignored for operator-local execution only.

CX-11 adds a Repository-only, forward-only `2026102201_agent_session_commit`
schema and a transactional DAO. Its local `(session_id, hash_kind, commit_oid)`
key distinguishes SHA-1, SHA-256 and BLAKE3, references an existing session,
and is safe to retry. A batch is limited to 64 sessions and rolls back in full
on a failed FK write. Global/System/Derived roles get no table or receipt.
Historical import does not write links. CX-23 makes ordinary commits opt-in via
`agent.commit_trailer=true`; it snapshots at most 64 scoped sessions with
committed checkpoints anchored to the current parent (or active sessions
already linked to it), appends deduplicated `Libra-Agent-Session` trailers after the message hook and
before signing, and writes the local DAO link after the ref update, rechecking each session
scope and the workspace lease in one SQLite transaction. Amend also carries
forward authorized links from the replaced commit.
`--no-agent-trailer` omits automatic trailers but keeps the local link.
CX-27 writes the local `.libra/agent-commit-links.v1` prepared intent before
the ref update under its SQLite writer transaction, then records linked stage
after a successful DAO link. The 10 MiB checksum-framed journal refuses
symlinks and is synced before the ref move. A failed ref may leave an intent;
only a reachable commit can be recovered by explicit `doctor --repair`.
Detection is read-only and paged from the current worktree HEAD by `--commit-link-page` (100 commits, pages
1–100); replay rechecks current scope, lease, tombstone and committed
checkpoint, and never rewrites a signed commit. `session show --commit-page`
lists at most 64 scoped Codex links per page. Missing evidence is never
reported as complete. Matched prepared/linked stages are reclaimed at the
10 MiB cap; unresolved intents remain until repaired or erased. If corrupt or
unrecoverably full, explicit `doctor --repair --reset-commit-link-journal`
atomically replaces the journal after operator inspection, discarding its
remaining trailer-free recovery evidence but no checkpoint content. A readable
journal with capacity refuses reset; ordinary repair must run first. Post-repair
cleanup failure is reported as `commit_links.cleanup_incomplete` without hiding
the catalog rows already repaired. Reset runs after replay/compaction and is
refused for permission, I/O, unsafe-entry and future-schema failures;
`commit_links.journal_unavailable_reason` exposes only the category. A readable
full journal also requires a live lease, successful current-page repair and
cleanup, and no unfinished 64-link batch. An incomplete scan or foreign-scope
intent requires inspection of the other pages/worktrees and the separate
`--acknowledge-commit-link-loss` flag before discarding remaining evidence.
`unmatched_scope_intents` also counts current-scope records outside the
selected page or current branch.
Erasure tombstones precede locked journal compaction; cleanup failure reports
partial and can be retried. Refs-only cloud metadata backs up neither this
journal nor the association table.

Current source builds project envelope rollouts using the Codex 0.162.1 record schema; unknown records from other versions remain partial. Exact `token_usage_record` response usage is counted once, including a repeated compaction snapshot. Legacy cumulative token observations are differenced across turns; an unexplained decrease remains partial, and a reset at an explicit context boundary is qualified. Matching legacy observations are not added to exact response totals. Cached tokens aggregate cache reads and writes. Missing or invalid usage remains unknown in the parser, rather than a measured zero.

Envelope prompt projection uses event-side accepted input and producer `content_item_kinds`, preserving real user text that resembles a context wrapper. Unannotated or mismatched user-role response content is retained in ordinary source data but is not asserted to be a user prompt. Shared item identity is paired first, with completed input preferred; otherwise identical projected text is paired by turn and occurrence across surfaces. Repeated text on the same surface remains repeated. Pairing without a shared identity is a partial lower bound. The first `session_meta` binds the owning thread. Foreign headers make the projection partial; foreign records are not added to its usage, and all unscoped records after a foreign header remain unattributed until the owning header reappears; explicitly scoped owning-thread records can still be projected. The model is the latest explicit `turn_context` model. Modified paths require a nonempty call/item identity and a successful, completed structured patch record; proposed patch text and failed or partial attempts are not evidence of a file modification. Paths are lexical and relative to the recorded working directory, which may be a repository subdirectory.

The parser reads at most 16 MiB, caps full JSONL decoding at 1 MiB per record, and bounds retained metadata and identity state. For an oversized known record, capability projections may read only a bounded type/role/scope skeleton while ignoring the body; a large image or tool output therefore does not suppress unrelated exact usage or completed files. Affected or unclassifiable projections still fail the record limit, and the underlying parser marks skipped bodies partial. Whitespace-only lines do not contain records. The hook helper applies its existing tighter limits. Unknown variants, duplicate keys, damaged tails, unsupported old patch statuses and exhausted budgets yield partial metadata and content-free warnings. Flat historical exports retain a compatibility reader with bounded metadata; oversized, invalid or unclassifiable rows are skipped and qualified without erasing other usable flat rows. Flat role-only user projections may include host-injected context and are not evidence of a human request. Mixed formats are partial, and flat rows inside an envelope file are not projected. These rules do not grant raw storage permission, change coverage claims, or prove live thinking capture. The Codex adapter now exposes the transcript analyzer and reports completed structured file changes through `libra agent list --json` capabilities. Its token calculator returns an actionable error when usage is unavailable or usage-specific consistency checks prove a gap, rather than manufacturing zero or returning a known incomplete total. A qualified explicit reset may still yield a lower bound. For nonzero offsets in a legacy-first source, subsequent windows keep cumulative-counter deltas even when exact records appear later; absent newer cumulative observations remain unknown, which prevents late exact mirrors from being counted again. Prompt, skill, file and usage offset reads retain earlier context and cumulative baselines while emitting newly completed records; previously seen item/response mirrors and an added newline alone do not replay old data. Pass the complete append-only byte snapshot together with its previous byte length, rather than only a sliced tail; a truncated or replaced source requires restarting at offset zero. If exact records exist only before the offset, a later cumulative observation cannot establish new exact usage. These are bounded best-effort projections; the analyzer position is the supplied snapshot length, not proof that every byte produced complete metadata. Prefix prompt payloads are hashed under the record cap; earlier text does not consume new-output string or retained-text limits. Current scope and output context remain bounded. Skill projection hashes supported prompt text for pairing and retains only recognized slash commands and their context. Prompt projection does not retain skill events. Skill event IDs use source identity (or a qualified occurrence fallback), and flat compatibility reads retain physical record ordinals across offsets. Each capability projects only its own metadata: a prompt budget failure does not suppress otherwise valid tokens or completed files, and response/patch identity limits do not suppress prompts. Shared input and record limits still apply to every projection. Budget errors name the affected capability and limit and state that the session cannot be projected by that capability; waiting or restarting the same oversized snapshot will not repair it. The underlying parser still offers partial metadata. Missing usage gets the ordinary waiting diagnostic unless usage-specific counters, response identity or reset observations are unproven; an unrelated partial prompt does not make usage malformed. An offset-zero total replaces earlier totals and must not be added as another delta. Capture treats calculator errors as partial metadata and continues the shared checkpoint path. Other adapters retain their existing contracts.



## Codex ordinary thinking and attached data (CX-16/CX-06)

CX-06 indexes ordinary `encrypted_content` from both `response_item` and
`compacted.payload.replacement_history` only after rollout filename,
`session_meta` id/version and turn binding. The indexed value is the decoded
JSON string, not a claim of lexical ciphertext or native provenance; the
credential-scanned ordinary JSONL remains the durable carrier. Duplicate-key,
wrong-type and unsupported-version rows add no index entry and cannot erase
previously retained bytes. Operator-local synthetic cases use
`--features test-live-agent --test agent_reasoning_artifact_test codex_source_ -- --ignored`.

In this development source, `libra agent checkpoint show <id> --json` returns
credential-scanned ordinary Codex thinking in `data.reasoning.readable`, and
nonempty retained opaque values in `data.reasoning.encrypted_content`.
`availability` is `unproven`, or `partial` for truncated, malformed or unsupported
records; `readable_obtained` reports whether this view contains readable text.
These values do not prove native origin or decryption. The checkpoint metadata
must belong to its ordinary tree and agree with the session's provider when it
contains `agent_kind`; older metadata may omit that field. Failed binding or
transcript reads return a content-free `unavailable` facet, never the raw blob.
An empty transcript also returns `unavailable` with `reason=no_transcript` and
`readable_obtained=false`; metadata alone is not acquired thinking.

Default `agent checkpoint export <id>` also scans decoded JSON string tokens
in old stored transcripts, including escaped credentials, while preserving
unknown associated values, precise numeric tokens and duplicate keys. It keeps
the configured `agent.max_transcript_read_bytes` limit (256 MiB by default);
the new-capture 16 MiB admission limit does not shorten existing exports.
`redacted=true` signals a credential mask or a locally replaced undecodable
string token in the JSON view.
JSON show is a bounded view of at most 16 MiB and parses records up to 1 MiB;
use default export to retrieve ordinary data beyond that view. A truncated view
is partial and never establishes that no later thinking exists. The configured export
limit still bounds the source prefix. Complete legacy strings containing lone
surrogates or invalid UTF-8 use a tolerant decoded view only for credential
scanning: tokens without matches keep their exact original bytes. JSON show
keeps this source partial even if masking makes its output parseable. Tokens that
still cannot be decoded, including strings cut inside an escape or UTF-8
sequence, are replaced locally with `?`, leaving structural partial evidence. Increase
the configured read limit when retrieving a larger checkpoint in full. Only
JSON string tokens are decoded; arbitrary escapes outside a string or in an
unsupported malformed envelope do not acquire a second interpretation.

CX-25 doctor scans only current-scope, ref-reachable committed Codex
checkpoints, then recognizes a strict normalized-turn envelope followed by
ordinary reasoning source records. A bare legacy key cannot mint data or a
native artifact. `codex_legacy_reasoning_revision` is metadata-only and
content-digest bound; the writer rechecks provider, session scope and
tombstone in a transaction. Read-only doctor never writes. Repair is
idempotent, preserves source objects, and scans keyset pages of 64 candidates
with a 128 MiB scan read budget and a 16 MiB per-checkpoint cap. Repair may
re-read eligible bodies under a separate 128 MiB page budget. The note
provides `--codex-legacy-after <checkpoint-id>` for the next page; no schema
change is required. Its
operator-local end-to-end case is
`cargo test --locked --features test-live-agent --test agent_doctor_repair_test codex_legacy_ -- --ignored --test-threads=1`.

Human show, checkpoint/session lists, graph and doctor do not expand these
bodies. Reads do not rewrite objects, traces or coverage claims. Raw export
still requires `--raw --allow-raw` and records granted or denied access; default
export creates no raw-access audit. Run the synthetic reader cases locally with
`cargo test --features test-live-agent --test codex_capture_local_test codex_legacy_ -- --ignored --test-threads=1`.
They do not prove live model capture, and are excluded from CI.


## Codex terminal budget (CX-02 development)

Managed Codex `SessionEnd` hooks have a 3 s host window and a 2000 ms primary
capture budget. Capture carries the same monotonic/absolute deadline through
ingress, database waits, lifecycle reservation, source acquisition and writing.
The existing terminal finalizer has a 250 ms content-free settlement slice,
derived from that original deadline and never restarted within a callback.
A later authorized callback has its own bounded I/O window; recovery preserves
the original receipt's finalizer deadline and retry accounting.
An asynchronous source exporter is now cancelled at the primary deadline even
if it ignores the deadline it receives; the content-free log reason is
`export_deadline_elapsed`. Cancellation takes effect when the future yields;
blocking work inside a poll is not preempted. This cancels only source acquisition:
runner settlement and already-dispatched SQL/ref commit acknowledgements are
still awaited. A terminal receipt stays pending when the checkpoint cannot
become durable, and must not publish `stopped` prematurely.

A callback that ends before a trusted session/scope frame exists keeps Codex's
advisory no-evidence exit behavior; it creates no invented receipt or completed
session. After scope validation, failure to retain required terminal evidence
is a fatal capture failure. If the host kills the process before any receipt
exists, no ledger can reconstruct that unseen callback. Retain the rollout and
use the existing owner-expiry/doctor paths when evidence exists, or an explicitly
authorized `agent import --path <rollout.jsonl> --agent codex --yes` to recover
available source data. Successful metadata-only lifecycle capture is not
thinking acquisition. Receipt completion and a durable checkpoint determine
terminal completion; the retained finalizer object records pending/quarantine
recovery evidence and has no separate `complete` status.

These changes reuse the existing finalizer and ledger; they do not activate a
Codex live source locator, exporter or model capture. Operator-local cases cover
CLI database contention, a 2 s stdin wait, host termination, native replay,
post-marker cancellation and owner-expiry recovery, and the real pipeline's
slow-source deadline using a
scripted test capability. Build the current binary first, then run:

```bash
cargo build --features test-live-agent --bin libra
cargo test --features test-live-agent --test codex_capture_local_test codex_terminal_ -- --ignored --test-threads=1
cargo test --features test-live-agent --lib codex_terminal_ -- --ignored --test-threads=1
```

Every new Codex case requires `test-live-agent` and is ignored by default. These
are local synthetic lifecycle/budget checks, not proof of real thinking data;
they are not scheduled in CI. Existing CI tests retain their original scope.

## Codex terminal diagnostics (CX-17 development)

`libra agent session show <session-id> --json` adds a `terminal_capture` object for Codex. Human output shows the same status and owner. It reads the existing catalog and writer marker; viewing a session never advances lifecycle state, consumes a retry, or repairs a checkpoint.

- `status` is `pending`, `completed`, or `unavailable` for the latest stored terminal receipt. `completed` describes receipt completion, not readable thinking, transcript availability, or an object integrity check. Inspect checkpoint show/export for content.
- `owner` is `live`, `expired`, `absent`, `cleanup_pending`, or `unavailable`. It uses the original marker generation and absolute expiry. A session being active for over 24 hours never overrides a live owner.
- `pending_receipts` and `completed_receipts` count terminal receipts in the bounded ledger. `superseded` compares the existing revision fence for pending receipts, and the stored stop phase/time for completed receipts. Counts may include older receipts. Distinct operations with the same stored stop timestamp may be indistinguishable; replay completions may also be marked superseded when they retain an earlier stop time. This diagnostic grants no recovery authority.
- `reason` is a fixed, content-free diagnostic. Missing, malformed, oversized, ambiguous or inaccessible evidence is explicitly unavailable. Released or changed workspace leases cannot authorize recovery of pending work. A completed receipt remains visible after workspace release or lease expiry; its historical fact does not require live repair authority. The diagnostic includes no source path, transcript, marker payload or replay credential.

`libra agent doctor --json` remains detection-only and reports existing pending-finalizer findings and live-marker counts. Session show is a repository-wide view of the selected session, including sessions in sibling worktrees. Pending/owner fields describe that stored session; they do not promise that this worktree can repair it. Only `libra agent doctor --repair` from the original worktree/workspace attempts the existing fenced finalizer recovery. A live owner is left alone; a missing durable checkpoint/artifact requires provider redelivery or explicit import. Repair does not reopen arbitrary provider sources, bypass a changed workspace lease, or create another checkpoint for an already durable terminal action. Repeated repair is idempotent.

The session diagnostic is additive; session list and other providers keep their existing output. Non-text or over-1-MiB Codex metadata omits the legacy capture_status/error fields and explicitly reports terminal_capture as unavailable; there is no raw fallback. This is a local development feature, with synthetic fixtures rather than a real Codex model result. Build the current binary and run the feature-gated, ignored cases locally:

```sh
cargo build --locked --features test-live-agent --bin libra
source .env.test
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_finalize_ -- --ignored --test-threads=1
```

These new Codex cases do not run in CI.


## Codex hook observation journal (CX-03 development)

Both `libra hooks codex` and the compatibility `libra agent hooks codex` entry record the same fixed result after validated ingress and repository binding. Their existing host exit policies remain intact. Unknown or rejected ingress has no trusted journal scope and creates no journal; its existing fixed diagnostics remain available. Other providers are unchanged.

The journal contains only schema version, local sequence number, the fixed provider name `codex`, a fixed hook command in `event`, one of `runtime_ok`, `runtime_failed`, or `terminal_failed`, and `count=1`. It stores no session identity, transcript, model, source/repository path, credentials, or rendered error. `runtime_ok` means that the runtime returned without error; it does not establish that thinking or a checkpoint was saved. Consult the session/checkpoint catalog for capture facts and terminal recovery.

Journal IO belongs to the shared capture owner. Under `.libra/private/capture-diagnostics/`, two fixed slots (`0.jsonl`, `1.jsonl`) hold at most 5 MiB each, plus an empty `lock` file. Rotation discards only the older slot. Unix directories/files require the current owner and mode 0700/0600; files must be regular and single-link. Windows uses the existing protected owner ACL and handle-relative no-reparse primitives. Unsafe, oversized, or damaged artifacts are left in place and reported as unavailable. No Windows runtime result is claimed by the macOS tests.

`libra agent status --json` and `libra agent doctor --json` add `data.capture_diagnostics`; human output shows the same retained-window counts. `state` is `absent`, `available`, or `unavailable`; `window=retained_records` and `may_have_gaps=true` apply throughout. Counts and optional `first_sequence`/`last_sequence` describe only valid retained records, never lifetime attempts or capture success. Readers never create directories or locks, repair artifacts, or return journal contents. Partial initialization, lock contention, invalid records, and unsafe files yield unavailable with no partial counts.

Logging is best-effort and nonrecursive. Its nonblocking lock and bounded tail reads use the earlier of the original capture deadline and a 20 ms soft IO window. These checks do not reset or extend the capture budget. A blocked filesystem call itself cannot be preempted by these checks. Lock contention, exhausted budgets, process death, and IO failure may omit observations; none changes the original runtime result, pending receipt, or host exit policy. The 10 MiB cap applies to this journal, independently of the existing global tracing logger.

If diagnostics remain unavailable after contention clears, first stop hook callbacks and check the directory itself for unsafe links or permissions. Move the journal directory aside without following links, then let the next admitted hook recreate it; keep the moved bytes if needed for inspection. This resets only the observation window. Do not remove the parent private directory, catalog, or pending capture receipts.

On Unix, the parent `.libra/private` must also belong to the current user with mode 0700. After confirming it is the intended directory and checking its ownership and contents, restore its owner-only permissions in place; do not delete it. The journal reader does not correct that parent. The existing capture-key writer has its own policy: it refuses foreign owners/group-or-other writes and normalizes permitted owner directories to 0700 before capture.

Synthetic regressions run only on the operator's machine, with no CI registration:

```sh
cargo test --locked --features test-live-agent --test codex_capture_local_test capture_diagnostic_ -- --ignored --test-threads=1
```


## Codex session summary (CX-07 development)

`libra agent session show <session-id> --json` adds `data.summary` for Codex. It contains `status`, `source_status=unproven`, `as_of_checkpoint`, `model`, `title`, `prompt_count`, `token_usage`, and `modified_file_count`. Human output includes the same source status and counts, including cached, reasoning and total tokens when known. Display text removes terminal controls and the common bidirectional, zero-width, tag and interlinear controls covered by the display filter; it does not perform full Unicode normalization. Session list and other providers keep their existing output.

The summary belongs to one stored checkpoint snapshot. Selection uses the existing committed-checkpoint order (`created_at` descending, then checkpoint ID descending); `as_of_checkpoint` identifies its source. These are source-reported snapshot facts, not live usage, lifetime aggregates, native provenance, or proof that every observed turn was captured. The session lifecycle, pending probe and checkpoint identity are pinned in one database read snapshot before the object read. The metadata reader proves the ordinary metadata role, type and content hash under its work budget; it never reads the provider source, transcript bodies or reasoning artifact bodies to fill missing fields.

`status` is `available`, `partial`, `pending`, `unknown`, or `unavailable`. Fields without established values are JSON null and display as unknown. A pending receipt or an active import masks token usage as unknown even if an earlier checkpoint has measured tokens. Zero is displayed only when stored metadata actually measured zero. Missing legacy extraction metadata stays unknown; a damaged role/object, malformed catalog or exhausted read budget is unavailable. Title stays unknown unless the stored typed extraction supplies it; it is not inferred from a prompt. Model/title strings are bounded, credential-redacted and stripped of terminal control characters before display.

Live metadata already has an extraction slot. Historical Codex imports now carry bounded optional facts from the same redacted source snapshot through the existing private retention response into that slot. Invalid optional helper facts are omitted while the ordinary retained transcript survives. Replay and append keep completed checkpoint objects and normalized V1 records/digests intact; no summary cache, table, source reread or fallback path is added.

Managed Codex hooks now locate the session rollout under the absolute `CODEX_HOME` directory, or the configured home’s `.codex` directory, within the original capture budget. The held source must begin with a `session_meta` whose ID and working directory match the verified hook scope. Readable ordinary thinking and optional summary facts are retained together in the checkpoint, with `source_status=unproven`. Deleting the provider source does not remove the stored export or make the summary reader reopen it. The local synthetic hook/store/export cases establish this path; a real Codex model and Windows runtime remain unverified. A hook-supplied arbitrary path cannot authorize a fallback, and a hook without a capture deadline does not read a Codex source.

Pending includes the current `capture_status=retryable` value and legacy `retryable_error`. CX-22 shares this receipt predicate with the CX-07 summary, so a retryable receipt masks summary usage even without a ledger row. A crashed import remains pending until a subsequent import recovers its persisted lease state; lease expiry alone does not prove data capture completed. `partial` also covers sanitized display fields. Pending views may contain partial snapshot counts; pending is not evidence that those counts are complete.

Summary regressions stay operator-local and are never registered in CI:

```sh
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_session_summary_ -- --ignored --test-threads=1
cargo build --locked --features test-live-agent --bin libra
cargo test --locked --features test-live-agent --lib codex_session_summary_ -- --ignored --test-threads=1
```


## Codex source acquisition (CX-08 development)

Use the managed hooks installed by `libra agent enable --agent codex`; they already supply the capture budget. Keep `CODEX_HOME` absolute if overriding the default home. Discovery is bounded and searches newest rollout directories first. Scope mismatch, unsafe links, oversized sources and an exhausted budget produce a partial/unavailable capture, never proof of completed thinking capture. The first header must match the same working directory, including a worktree subdirectory; known macOS system-directory aliases compare equivalently.

The ordinary checkpoint retains the redacted rollout, including readable `reasoning.summary[].text`. Decoded credential values are scanned before persistence. Opaque encrypted content stays opaque, and this path adds no native provenance. Existing V1 coverage records and completed checkpoint objects keep their identities.

The internal index reader accepts only an explicitly supplied, already closed and consistent single-file backup with no hard links. It does not automatically read a live `state_5.sqlite` or copy its WAL. Sidecars, changed/truncated data, failed or unsupported migrations, virtual-table schemas and exceeded limits mean unavailable, never zero sessions. CX-08 introduced this internal reader. CX-22 adds the explicit doctor backup option documented below.

New Codex regressions remain operator-local, feature-gated and ignored:

```sh
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_index_ -- --ignored --test-threads=1
cargo build --locked --features test-live-agent --bin libra
cargo test --locked --features test-live-agent --lib codex_index_ -- --ignored --test-threads=1
```

The index IO owner pins the explicit backup and its parent, checks sidecars and the bounded SQLite header before and after the worker, and retains an owner-only functional scratch directory until helper reaping. The worker creates one fixed owner-only copy (0600 on Unix), opens only that static copy read-only/immutable with query-only and trusted-schema disabled, and validates integrity plus migration checksums/schema/rows in one read transaction. It loads no extensions and takes no SQL input. Source files are never changed. Normal cancellation retains the scratch in the reaper; forcibly killing the parent or tearing down its runtime may leave private scratch behind. No sweeper is added. Windows uses the existing handle/ACL primitives, but its runtime and Job teardown/cleanup race still need Windows verification.



### 从已关闭的 Codex 索引备份对账（CX-22 开发中）

```sh
libra agent doctor --codex-index-backup ./closed-index.sqlite --json
libra agent doctor --codex-index-backup ./closed-index.sqlite --codex-index-page 2
```

显式提供已关闭且一致的备份。相对路径按调用目录作词法解析；
backup 与 `--repair` 互斥，page 取值 1–10 且必须提供 backup。
未提供 backup 时原 doctor 输出不变。此视图不打开 home 热数据库、不读 rollout
正文、不导入、修复或 hydrate 对象。

JSON 的 `data.codex_reconciliation` 区分备份覆盖区间
`cutoff_ms` 至 `coverage_until_ms` 与本次窗口锚点。每次调用重新锚定七天窗口，
分页不是稳定 cursor。`scope_candidate_count` 为全部 scope 候选，行状态和
`page_totals` 仅对应本页，null 表示未计算。超出候选集的页给出
`page_out_of_range` 和 null totals；过旧备份为 `outside_window`，
totals 也为 null。`empty_snapshot` 只表示提供的备份为空。

状态为 `stored_source/missing/missing_data/partial/pending/erased/unknown`。
stored_source 要求具名 metadata、非空本地 ordinary 对象及角色、哈希、大小和
manifest 声明的已存长度均验证通过。snapshot 的整源字节数可以与 import 的单 turn
内容长度不同。它不证明可读 thinking 或所有 turn 完整；旧/不明 metadata、本地缺失、
损坏或仅在 pack 中的对象保持 unknown。坏 Libra catalog 行不拖垮其余行；provider 索引的坏行或结构错误会令整个 section unavailable。
已验证空 transcript 为 missing_data/stored_transcript_empty；manifest 长度不一致为
unknown/stored_size_mismatch，不给 import hint。quarantined 会话、committed identity
却没有 session 的坏账本为 unknown；schema-pending 和 retryable 收据为 pending。
优先级为 erased、当前 Libra scope 的 pending 证据、目录及数据证明。reason 使用固定
snake_case key，如 scope_root_unproven、response_inconsistent、row_invalid、
timestamp_schema、budget_exhausted。

只有 proven scope 的 missing 与 missing_data 给出不带 `--yes` 的显式 import
提示；erased 不给提示。无 session 的 pending 也检查；过期 import lease 仍为 pending，
另标 `import_lease_expired`。若先前采集在 SHA-256/BLAKE3 仓库因对象格式不匹配
失败，请使用包含 helper 格式修复的构建重试原采集或 import；写入成功前仍为 pending，
doctor 不自动修复或清理先前 loose objects。预算不足明确标 `budget_exhausted`。

`scope_unproven` 表示目录归属未证明；`child_attribution_unproven` 表示 child 没有
独立已验证 source，edge 也不能证明其正文归属 parent。archived 行仍参与对账。
active 会话在首个 checkpoint 前可能为 missing_data；import hint 仅为显式预览，
不自动写入。大小写/NFC-NFD 与 macOS Data volume 可识别别名只列为 ambiguous，
不授予 source 读取权限；无法识别的 root 外 symlink 拼写排除，root 内遇到链接为 unknown。
备份 parent 目录的 sidecar 枚举超过 4096 项时令整个 section unavailable。

任何毫秒时间超出当前时间五分钟都会令备份 unavailable。上限为所有仓库合计 10000
近期候选、thread 文本 1 MiB、edge 独立上限、每个 scope 目录 4096 项及 1024 个目录
身份缓存。大目录可能因此 unknown/unavailable。一次对账共用两秒预算，catalog/
checkpoint metadata 及解压后的 tree/manifest 证明字节合计 8 MiB、ordinary 验证 64 MiB；原 doctor 扫描和
worker 收尾保留原行为。失败或截断不报可靠的零缺失。所有新增 Codex 测试仅在本机
以 `test-live-agent` + `--ignored` 运行，不加入 CI。

实现：command owner 在进入 blocking worker 前固定 request.workdir/root；catalog 同一
read transaction 返回 tombstone、scoped identity 与 pending/预算事实，事务关闭后再由
checkpoint reader 一次证明 roles、对本地 loose 对象 stream 校验。stored byte_len 来自
manifest，不能把整源 projection 长度直接当作单 turn 存储长度。SQLite 副本设置
ignore_check_constraints，schema-wide 生成列拒绝后再做结构完整性检查；SQLite 每值上限
16 MiB，原 deadline 的进度回调始终有效。任意 CHECK bytecode 不作为可执行验证规则。
目录 memo 仅保存身份/结果，不常驻保存每个 FD；重开必须 no-follow 且身份一致。
Windows 只增加 Disk/VerbatimDisk 已知词法别名，UNC/device 拒绝，运行验证尚未完成。

CX-22 的窗口只使用 updated_at_ms；不把 created_at_ms<=updated_at_ms 当作未有
官方证明的额外数据库不变量，clock skew 下的创建/更新顺序不用于猜会话归属或完整性。

Schema, migration and integrity refusals use `schema_unsupported`,
`migration_unsupported` and `integrity_failed`. Invocation scope, worker and
transaction failures use `scope_unavailable`, `worker_unavailable` and
`catalog_unavailable`; other catalog/scope failures use
`scope_or_catalog_unavailable`. Catalog deadlines and typed work-budget errors
use `budget_exhausted`. Unclassified backup IO errors use `backup_unreadable`.


### 完整历史索引补导（CX-19 开发中）

公开合同见[命令指南](../../commands/agent.md#codex-full-history-index-backfill-cx-19-development)。
新--codex-index-backup为exclusive Codex selector，完整历史/归档，不复用Recent七天DTO。
先目录scope证明、Rust bytewise排序与分页，再同一Libra读事务判catalog；Missing复用
原prepare/commit/fence，existing只做catalog事实且不刷新原source。旧BatchSkip/结果类型
保留，新增非空existing数组及人类/错误details；partial双报告保持。坏索引source形状
在reader之前020；whole拒绝018加固定reason_key，不打印成功batch。旧selector稳定。

SQL/parser与DTO在codex_index；provider_index_io是唯一新FS owner，在原copy mode中
以optional import_scope版本字段区分查询，和recent_scope互斥。None旧wire/三migration
保持，old full query先count/type/octet_length sentinel预检后fetch strings，thread+edge
合计1MiB，重复ID拒绝。共享ScopeProjection复用同一个rooted walker；parent验证计数/
排序/唯一ID、source_state关系表及edge双端Proven。RootPin跨同意和catalog保持，读源前
及prepare后在原command deadline内重验；事务先rollback再进入源授权，不留下后台writer。
import入口优先使用已pin的request.worktree_root/storage配对；未pin才调用原fallible
repository discovery，避免嵌入调用或linked worktree被ambient cwd替换。

source_state：eligible=>Proven+Some合法绝对rollout叶；invalid_path=>Proven+None；
scope_unproven=>非Proven+None。UI backup参数可作词法../归一，但数据来源path不能
相对cwd解析或剥离ParentDir。叶谓词与原live locator共享，archive不限制四段日期。
IO helper从不stat/open rollout_path。调用者不能把edge送writer或据它猜parent。
源码/CLI实现不改变原data-floor ordinary retention或引入native认证前置。

全部新增Codex回归只在本机，以test-live-agent+ignored运行，绝不加入CI：

```sh
cargo build --locked --features test-live-agent --bin libra
cargo test --locked --features test-live-agent --test codex_capture_local_test codex_uncaptured_ -- --ignored --test-threads=1
cargo test --locked --features test-live-agent --lib codex_index_full_scope_ -- --ignored --test-threads=1
```

正式验收需先列实际匹配名，再执行非零新门；zero-reader oracle必须真实per-slot loop、
入口task-local计数与eligible正控制，不能只测pure projection而空过。日志观察使用
/dev/fd/2的内存captured stderr，不新建报告/log文件。Windows仅窄编译不能冒充运行；
current源码的focused/strict/guard/source+docs review/site/full仍须取得实际结果。

CX19的目录4096项/memo1024项耗尽可将本scope行标layout unknown并逐slot020；
不是foreign证明或全snapshot预算错误。索引根在per-slot失效时中止后续读源，将未尝试
slot记018，并保留已完成results及固定reason_key。catalog begin/tombstone失败使用
catalog_unavailable，逐slot失败不会抹掉前一slot的durable结果；human partial也计existing。
一致备份示例先退出Codex，再通过Python SQLite backup API读取mode=ro的源并写入新目标，实际示例见公开指南。

本机系统SQLite -readonly .backup对closed WAL/no-sidecar夹具实际失败；公开示例
采用已实测的Python SQLite mode=ro backup API，不对源使用immutable，也不覆盖既有目标。
这只是功能SQLite夹具的本机验证，不冒充真实quit-Codex索引验证。

pre-loop catalog snapshot(begin/read/rollback)不可用是whole 018且无成功batch；
循环中的tombstone lookup失败保留此前已完成slot。Python备份示例先打开只读源，
再排他创建目标；备份失败会删除本次新建的目标，已有文件不覆盖/删除。

## Codex child content (CX-09 development)

Current source builds also retain verified Codex child rollouts as independent
content checkpoints. Live child collection runs at `SubagentStop` and parent
checkpoint boundaries; consented import and pending recovery use the same
collector. The collector uses the absolute `CODEX_HOME` directory, or the
configured home's .codex directory (also the fallback for a relative
CODEX_HOME value), and validates the opened header's root,
current thread, immediate parent, child and repository working directory.
For a nested child, its immediate parent can differ from its root session.
A legacy root header may omit session_id and inherit its own id; a child
must still supply its root. A supplied transcript path or child ID alone
does not authorize a source.

Child content keeps returned readable reasoning summaries, `encrypted_content`
and unknown ordinary fields under the existing bounded retention contract.
Use `agent checkpoint export <content-checkpoint-id>` to read the stored
transcript; show remains a summary. An opaque value is preserved without
decryption and is not readable thinking. Active/archive moves and unchanged
live/import/recovery replays reuse the same logical content revision.
New bytes can append a revision. Stored bytes survive deletion of the
provider source.

A live `SubagentStop` can associate content with its boundary only after the
current scope, source digest, settled claim and observed turn have been
verified together. A matching child ID in another turn cannot establish
that association. Late content can be collected on the same callback's
replay without rewriting the original boundary objects. Libra does not
redeliver host callbacks; later parent capture, import or recovery can still
save late content without establishing that old boundary association. Missing proof
leaves content unresolved and still exportable. Historical import alone
does not establish a live association or native provenance.

For UUID-shaped Codex session IDs, a parent `SessionEnd` waits for its known
required child claims to settle. An unavailable provider root preserves
pending recovery; finding some partial data without a known unsettled
claim can finish with an explicit partial diagnostic. A managed parent can
carry an incomplete-extraction warning even when zero children were found;
that fixed omission warning does not establish that a deadline expired.
Old arbitrary-label
callbacks keep their original parent-only live route without new child dispatch
or claims. An already-elapsed discovery window can still add a fixed
skip/incomplete-extraction diagnostic. This live predicate adds no identifier
restriction to stored import or recovery; their existing source and selector
validation remains in force, including UUID filenames for public `--path` import. Neither a UUID nor a
partial diagnostic proves native origin. Inspect `agent session show` and
`agent doctor` for pending state; retry after restoring required sources.
A retry cannot reconstruct a deleted source. The local recovery worker
retains its original two-second batch and at most five artifact attempts,
so recovery is not a promise of eventual completion.

To request readable summaries from Codex, its `config.toml` supports
`model_reasoning_summary = "detailed"` and
`model_supports_reasoning_summaries = true`. Whether text is returned still
depends on the selected model and provider. Empty summaries do not imply
that Libra lost text; compare the actual source with decoded export JSON.
All new Codex capture tests remain operator-local, feature-gated and ignored;
they are not scheduled in CI. These source-build changes remain under local
acceptance and are not a released-runtime guarantee.

Development routing remains provider-neutral: the existing LiveCaptureBinding
selects one LiveSubagentDiscovery for live, Import-current and Recovery-root.
All provider file I/O stays in subagent_content and its killable helper.
Header probes, rechecks and bodies share the same byte/source/entry budgets;
metadata is not a second uncharged read allowance. Claude keeps its original
wire and default incomplete-parent extraction behavior. Codex opts into the
existing managed extraction omission flag, including a zero-source incomplete
discovery; its fixed warning does not imply that a deadline expired.

The existing receipt child gate stores only Preparing and required source
revision floors. Recovery does not invent a second ledger or reset the
original action, reserved revision, attempt policy or Manual repair authority.
Every completion transaction rechecks settled leaves. Retryable error progress
can be recorded while the gate is pending; real checkpoint failure is not
reclassified as a successful child wait. Import/recovery may preserve ordinary
partial content without minting a fresh live proof.

Local verification commands (build the current helper first):

```sh
source .env.test && cargo build --locked --features test-live-agent --bin libra
source .env.test && cargo test --locked --features test-live-agent --test codex_capture_local_test codex_child_content_ -- --ignored --test-threads=1
source .env.test && cargo test --locked --features test-live-agent --lib codex_child_ -- --ignored --test-threads=1
source .env.test && cargo test --locked --features test-live-agent --lib codex_stored_ -- --ignored --test-threads=1
```

macOS native smoke on codex-cli 0.162.1 used a temporary HOME/CODEX_HOME,
the current compiled Libra installer and all twelve persisted hook trust entries.
With gpt-6.1-sol/medium and the explicit summary settings above, a real child
returned a 376-character readable summary and 2444 characters of opaque
reasoning. Both matched decoded default content exports exactly. The parent
stopped; the growing child produced two content revisions. Child metadata
remained partial/unresolved, so this proves ordinary retention rather than
native provenance or a successful native boundary association. The default
summary run had returned opaque data only. No global Codex configuration was
changed. The current full-card regression and delivery gates remain open.

Summary controls are documented in the
[official Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

The subsequent real TUI Snake smoke used the current source binary and
system Codex 0.162.1 with the same isolated trust setup. The generated curses
game compiled, passed its self-test and rendered/accepted controls in a real
PTY. Three child summary segments (1598 UTF-8 bytes total), three opaque
values (8764 characters) and their complete reasoning JSON objects matched
default checkpoint exports. doctor/status succeeded and the parent stopped;
metadata stayed partial/unresolved, with no native association claim. This
does not replace the pending final full regression or delivery gates.
