# Agent Command Development

`libra agent` is an intentionally different external-agent capture extension,
not a Git-compatible command.

OpenCode **content export is unsupported on macOS**. A forking exporter can
escape Seatbelt's direct-child lifecycle, so Libra fails closed before it
spawns either `sandbox-exec` or the exporter; hook capture remains
metadata-only. Linux uses the required bwrap containment backend. See
[`../tracing/agent.md`](../tracing/agent.md) §5 for the platform boundary.
Linux content export is available only when the trusted system `bwrap` passes
Libra's descriptor-native `--bind-fd`/`--ro-bind-fd` safety probe; unsupported
or legacy bwrap is unavailable and remains metadata-only. Install or upgrade
the system bwrap package—there is no unsandboxed fallback.

The shared `run_bounded_exporter` Unix `pre_exec` sets both soft and hard
`RLIMIT_CORE` to zero next to the existing per-platform `RLIMIT_FSIZE`.
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
   `claude-code` / `codex` / `opencode`. The omission is pinned by
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
`--schema-version 2`. Batch limits charge bytes actually read from the held
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

`materialize_validated_checkpoint_input` is the production caller for investigate `drive` and review scoped setup. It canonicalizes the absolute object store, opens `.libra/libra.db` read-only (`create_if_missing(false)`, one connection), and closes that connection before `cleanup_checkpoint_input`. The catalog query budget is the caller deadline capped at 200 ms from the query phase. After close, ordinary blob leaves under `checkpoint/<id[:2]>/<id[2:]>` must equal the saved path-to-id map. Those blobs are read with the typed blob reader (blob type, declared size, and content hash) before `cleanup_checkpoint_input`. A saved id that is not a blob, a hash mismatch, a repeated path, or a byte cap stops before that cleanup. `reasoning/encrypted/<64 hex>` is omitted from that map and refused if the saved spec names it; the typed reader is not asked to open it. A paused investigate continue that cannot open, query, or close the catalog returns `InvestigateRunError::Store` before any state or input mutation. Review setup expiry stays the infrastructure error; cancel stays cancelled. The reviewer timeout after a successful setup is still `request.reviewer_timeout`. Investigate uses the persisted remaining run budget and records expiry as timeout. No new `StableErrorCode` is added.

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

RG-06 adds artifact tree reachability; RG-03 adds controlled read and export.


## Shared checkpoint input proof (FIX-RG-SCOPED-01)

`internal::ai::checkpoint_reader` owns the typed header and content-addressed identity check, ordinary-role traversal, and manifest/tree closure. Command readers use this owner; the fresh scoped resolver queries `storage.join(util::DATABASE)` through a dedicated SQLx readonly connection with creation disabled. The query phase has one absolute deadline capped at 200 ms and the caller's remaining budget; it does not initialize schema or reuse the cwd-selected writer pool. Filesystem work receives authority only after the query lease and dedicated pool have closed. Failures refuse the input and cannot authorize materialization.

SQLite contention is refused immediately (`busy_timeout(0)`). Acquired connection and pool closure are always awaited; an authority that finishes after the deadline is refused. The 200 ms phase deadline does not promise a 200 ms bound on awaited cleanup or that a timed-out lazy connection establishment thread has already exited. Default reader budget refusals keep their existing store-inconsistent message; fresh scoped layout, integrity and payload-cap failures retain the checkpoint-specific fatal context and inferred error code.

Fresh scoped traversal-limit refusals keep the existing store-inconsistent message and `LBR-AGENT-009`, while an empty wrapped leaf keeps its original scoped fatal message. Shared role proof returns its complete map; both fresh and saved consumers refuse an empty map before granting materialization authority.

The saved-spec proof API reserves at most 4608 saved entries, 4096 UTF-8 bytes per path and 8431616 cumulative path bytes before path normalization or map allocation. It then requires the complete ordinary path-to-OID map of the named wrapped checkpoint. Ordinary files retain their separate 4096-file and 8 MiB path budgets. A legacy full list may exclude at most 512 entries only when each exact 84-byte `reasoning/encrypted/<sha256>` path and OID agrees with both the manifest and artifact tree. Prefixes, OIDs alone and ordinary subsets are insufficient. Proof metadata reads are recorded separately from ordinary-body reads, and proof never reads artifact bodies.

This card supplies the shared internal API. Persisted review/investigate recovery is connected by FIX-RG-SCOPED-02, and filesystem ownership is supplied by FIX-RG-SCOPED-04. Their acceptance and the final aggregation full suite remain separate requirements. Synthetic role fixtures do not establish a native producer origin or authorize OpenCode reasoning archival.

The existing public `checkpoint show`/`list` summary remains a closed whitelist and does not load checkpoint payloads. The R4 metadata projection gate is preserved as a test-only helper until RG-03 enables its public metadata contract. Skill projection now passes checkpoint id, tree oid and metadata oid to the shared role proof; corrupt or unbound metadata is skipped without exposing its body. Raw export authorization and audit are unchanged.
