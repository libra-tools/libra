# Agent transcript fixtures — provenance

Consumed by `tests/agent_transcript_intelligence_test.rs` (AG-21 / E6 / E7 /
A0-07). Assertion failures should be triaged against this table:
implementation regression vs upstream format drift.

**Provenance note (2026-07-15)**: the original fixture files were referenced
by the test suite but never committed to the repository, so a fresh checkout
could not run the target. These files are **synthetic reconstructions**
hand-authored against the frozen parser shapes in
`src/internal/ai/observed_agents/extract.rs` — they are *not* captures of
real agent sessions. Each file exercises exactly the values the test pins.

| File | Format modeled | Pinned expectations |
|---|---|---|
| `claude_code.jsonl` | Claude Code session JSONL (`type`/`uuid`/`timestamp` envelope, `message.content` string or block array, Claude-native `usage` keys `cache_creation_input_tokens`/`cache_read_input_tokens`, `tool_use` blocks with `input.file_path`) | 2 prompts; 1 `/review` skill event (`input_slash_command`); merged usage input=200 output=65 cached=40; model `claude-sonnet-5`; modified files `src/lib.rs`, `docs/readme.md` (in order); no `Task` tool so subagent-aware total equals session usage |
| `codex.jsonl` | Codex rollout JSONL (generic `message` wrapper records; E6 wire `usage` keys) | 2 prompts; 1 `/review` skill event; merged `total_tokens=260` (155+105); model `gpt-5.3-codex` |
| `opencode.json` | Synthetic OpenCode 2.0.26 native `{info,messages}` export; fixed official commit `9b4ec5714d481559990db0a816d5dec19541a814`, schema session-message.ts/session-transfer.ts, core session/transfer.ts and token-usage.ts; never live/native-origin proof | 2 user.text prompts; 1 `/review` skill event; assistant model.id `claude-sonnet-5`; tokens input=2 output=3 reasoning=4 cache.read=5 cache.write=6, cached=11 total=20 api_call_count=1; modified files `src/lib.rs`, `docs/readme.md`; snapshot files are project-relative |

Editing rules:

- Values are load-bearing: the test asserts exact prompt counts, token sums,
  model ids, file lists, and single-skill-event extraction. Update the test
  and this table together with any fixture change.
- The second user prompt in each fixture must NOT start with a curated skill
  command (`/review`, `/security-review`, `/simplify`) or contain a
  `<command-name>` tag, so exactly one skill event is extracted per fixture.
- Keep `claude_code.jsonl` free of `Task` tool_use blocks — introducing one
  flips `subagent_usage` to the session aggregate and marks the summary
  partial, breaking `total_token_usage_including_subagents` expectations.

## OG-04 current reasoning field audit applicability

opencode_pin: 2.0.26
opencode_commit: 9b4ec5714d481559990db0a816d5dec19541a814

The current field-shape contract is the plan-20260902 inline OG-04 source
record, read together with the bounded OG-18 native-origin audit. The three
ordinary candidate fields are reasoning `state.signature`, `state.redactedData`
and `state.reasoningEncryptedContent`; names and string shapes do not prove
native origin. Source A remains NO-GO for unproved opaque archive. Compaction
`providerContext.messages` is outside the ordinary field scope.

The RG-04 `reasoning/opencode.json` is an unchanged historical synthetic
open-state / UnsupportedShape counterexample, applicable to the current pin
only for that semantic decision. Its historical note is not a current native
three-field sample. All four RG-04 fixtures and the embedded contract/digest
expectations stay frozen in OG-04. OG-25 owns the additive fail-closed state
matrix and independent canaries; OG-05 retains final native proof and archive
integration. Current pin/commit declarations must agree with OG-00, OG-04 and
the nested `opencode/MANIFEST.md`; drift, absence or duplication must fail
visibly. JSON string value UTF-8 bytes define the downstream opaque candidate
boundary, without base64 decoding, trimming or normalization. No fixture or
synthetic label grants trusted native provenance.

OG-25 adds `reasoning/opencode_og25.json` as a separate synthetic privacy
fixture. It combines all three named `state` canaries, an untrusted
`originProof`, and `messages[].providerContext.messages`. The pre-redaction
classifier must remove these from the ordinary copy and keep the snapshot
partial with zero artifact. It does not change the frozen RG-04 fixture or
prove any model's native field.


NPR-18 keeps the historical official `2.0.26` / `9b4ec5714d481559990db0a816d5dec19541a814` source record and all existing JSON bytes frozen. The additional `opencode/native-origin-build.json` (named `native-origin-build.json` in this directory) is the separate current public identity descriptor for the reviewed private `2.0.26-libra-native.1` darwin-arm64 artifact, build `bac21cc4027896b8d1836133c347d20f`. Its official base commit is not the patched source identity. The source-ready audit is actual single Claude PASS; Source A remains UNPROVEN, archive/native GO is false. It contains no issuer, key or per-installation policy. Neither old synthetic fixtures nor this public descriptor is real native provider proof. macOS acceptance explicitly selects `LIBRA_OPENCODE_NATIVE_BINARY`; no global fallback or Linux functional claim.

The separate `native_origin_endpoint_anthropic_v2.json` is an independently encoded, synthetic 111-byte endpoint descriptor vector for the exact first-party Anthropic Messages URL. `opencode/native-origin-build-anthropic.json` is the public, uninstalled and unregistered `2.0.26-libra-native.2` candidate identity. Its source archive and manifest describe the complete reviewed private `.2` tree; its patch and changed-path manifest describe the complete official 2.0.26-to-`.2` delta, not the incremental `.1`-to-`.2` review diff. Both fixtures contain only public hashes and synthetic protocol data. They do not contain account output, issuer keys or evidence that real `signature`/`redactedData` bytes are native; Source A and archive admission remain unproved.

## OG-17 frozen pre-v2 pending reader

`reasoning/pending_v1_decoder.rs` is the compiled pre-v2 pending reader oracle,
frozen before any v2 edits from Libra commit
`7efcf868d49a5183e7f0851a89abebee91357155`.
The original `src/internal/ai/capture/pending_payload.rs` SHA-256 was
`87f2e8879831dae5fa83e3ffac3e916e23159dfb6a08f33a865bd1ae9f5d93fb`;
the original `src/internal/ai/capture/pending.rs` SHA-256 was
`393c9c7877c15680246a2ebcd1ce8c560ed2f61881dd0586d8f7871e7488743a`.
The fixture SHA-256 at freeze is
`e8578fce3419fdb7a70b69242f9c3220767897e3fda9ddc1f61e6a9d014a450a`
(updated when the frozen decoder gained explicit key canonicalization for the
GPUI-forced `serde_json/preserve_order` feature; the canonical order matches
the original `BTreeMap` behavior).
The copied payload decoder/schema is production lines 1–674 (only its two
inner doc comments became ordinary comments for `include!`). The following
`pending.rs` source fragments from the same commit are compiled in the same
oracle: header lines 128–164 (SHA-256
`9435d9a6e32fc76a477cc00b5060f2f4611a8548760c9772ff8fb7663eed125a`,
only the type name changes to `FrozenPendingHeader`), closed envelope lines
166–173 (SHA-256
`38cc9f32fdf2350848036f72f9691512083d768e0ab09499d12b0a6f09dc30d5`),
capacity/depth validator lines 432–475 (SHA-256
`8450bf919640d460469705f40b8ecf3d9b29227c57f10626d966c648948f63cd`),
and the complete authenticated recovery dispatch lines 533–602 (SHA-256
`84edadb81ea26c0e429fefa4d20e5075c803cc433d5efef53c0ef84013f69335`).
It compiles through `capture::pending::frozen_pre_v2`; OG-17 VER11 and the
named route test persist a signed v2 envelope, call this old
`load_verified_payload` with the persisted header, and check that the old
reader rejects it without deleting pending bytes. Its unchanged
`load_chunks`, `PendingHeader` (the route parameter), `PendingBinding`,
`VerifiedPendingPayload`, deadline and sidecar checks, payload-binding checks,
and the scope/key-backed `AuthenticatedPendingEnvelope::verify` still come
from the live implementation. The copied payload reader also calls the live
`CaptureSnapshotProjection`, `CheckpointRedactedPayload`, `PreparedPendingAlias`,
`Redactor`, `parse_canon_value`, `LiveClaimCommitPlan`, `ReservedTurnClaim`,
`CaptureCatalogError` and `retryable_load_failure` APIs; their behavior is not
frozen here. The frozen header decoder is exercised separately against the
actual `metadata_kv` header text persisted by the signed v2 test. The oracle
therefore pins the exact
old payload decoder and old recovery dispatch, but is not an independent
snapshot of the old storage, MAC or catalog implementations. OG-17's named
route test runs the frozen dispatch against a signed v2 artifact. Do not
regenerate this oracle from a later decoder. This is
historical reader compatibility evidence, never native provider-origin
evidence.


## Current official ordinary-export source extension

current_official_opencode_pin: 2.0.29
current_official_opencode_commit: 042d0832f5f3a73eaa039a58a7d565776f6d4290

These current-source fields are distinct from the frozen `opencode_pin` and
`opencode_commit` above. All existing JSON bytes and private `.1`/`.2`
descriptors remain historical; none is relabeled as a live 2.0.29 capture or
native Source A. The new inline deterministic `opencode29_idle_fixture` in
`opencode_reasoning.rs` follows the public 2.0.29 optional idle SessionError
shape: required string type/message, optional integer status 100..599 and
optional response body. Response/unknown/malformed/opaque error branches stay
partial after safe projection. These synthetic mechanism cases are not
provider or account evidence; real Session/model checks stay macOS local.


## CX-12 Codex 0.162.1 source-pinned synthetic corpus (2026-10-19)

codex_current_source_pin: 0.162.1
codex_current_source_commit: 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d

`codex-rollout-01621.jsonl` is **synthetic**, hand-authored against the fixed
source matrix in `docs/development/plan/plan-20260904.md#cx00-source-audit-20261019`.
It is a corpus of independent source slices and explicit fault cases, **not a
single natural or import-ready model session**. It contains no account output,
credentials, cryptographic provenance, or evidence of a live Codex hook/model.
The original `codex.jsonl` and its old table row above remain historical generic
parser fixtures; this additive section does not relabel or rewrite them.
The original 8725-byte OpenCode provenance prefix stays byte-for-byte frozen.

The first segment declares paginated mode. The later session_meta begins an
independent legacy-mode segment. Lines 41-44 omit ordinal, following
rollout/ordinal.rs:23-42 for new legacy files. A legacy mirror deliberately placed in the
paginated segment is a compatibility/fault case, not a claim that the current
writer naturally persists both mirror forms. Duplicate replay, wrong types,
unknown variants, duplicate keys, and the incomplete final line are deliberate.
The last line has no trailing newline; do not repair it. Valid preceding records
remain evidence when a bounded reader reports the corrupt tail as partial.
Reasoning `content` positive data contains reasoning_text; pure text-only arrays
are not presented as natural writer output. Compaction encrypted values are
negative thinking-classification cases, not substitutes for reasoning data.
Every CX12-SYNTHETIC string and opaque value is a canary, never a real token,
signature, authenticated envelope, or complete internal reasoning.

Corpus SHA-256: `bb1453259ac10a91c9bfd2529eb4b64ab1c963c70bf8e67fb5941d4d65b1fe69`; 12433 bytes; 46 physical lines.

| Physical line | Scenario / expected contract |
|---|---|
| 1 | session identity and synthetic source pin |
| 2 | world baseline is not thinking |
| 3 | turn context summary none is not effective request setting |
| 4 | turn start |
| 5 | literal AGENTS prefix user prompt must survive |
| 6 | harness developer message has the same visible prefix |
| 7 | client-authored developer provenance |
| 8 | inherited user input is not child authorization |
| 9 | canonical readable reasoning Unicode escapes and opaque |
| 10 | identical ordinal8/record replay: deduplicate; next record deliberately jumps to ordinal10 (gap9), allowed by recorder_tests.rs:1029 |
| 11 | same reasoning as typed item-completed mirror |
| 12 | legacy mirror shape in paginated segment is a compatibility fault, not natural output |
| 13 | legitimate repeated text with distinct item identity |
| 14 | synthetic forward-extension reasoning JSON fields stay ordinary; not actual provider-source evidence |
| 15 | nonempty opaque only is data but not readable |
| 16 | empty summary and empty encrypted string are empty shell |
| 17 | null encrypted differs from empty and absent |
| 18 | absent encrypted field stays absent |
| 19 | malformed nonstring encrypted must be retained as unsupported |
| 20 | missing required summary is unsupported shape, not invented text |
| 21 | exact completed-response usage record |
| 22 | cumulative token_count persists in both modes and is not additive |
| 23 | cumulative token rollback marks partial |
| 24 | missing usage is unknown rather than zero |
| 25 | compaction retains reasoning and aligned metadata |
| 26 | compaction encrypted string is not reasoning data |
| 27 | context compaction encrypted string is not reasoning data |
| 28 | world patch does not manufacture prompt or thinking |
| 29 | unknown response variant is partial |
| 30 | unknown event variant is partial |
| 31 | unknown rollout variant is partial |
| 32 | error-bearing terminal is not successful completion |
| 33 | effective settings snapshot, not turn_context legacy placeholder |
| 34 | durable interrupted terminal with turn id |
| 35 | non-Interrupt aborted reason replaced |
| 36 | non-Interrupt aborted reason review_ended |
| 37 | non-Interrupt aborted reason budget_limited |
| 38 | durable aborted terminal without turn id must remain partial |
| 39 | rollback changes active turn projection without deleting source |
| 40 | camelCase goal state is neither user prompt nor thinking |
| 41 | independent legacy-mode session segment begins |
| 42 | legacy persisted reasoning mirror |
| 43 | legacy persisted raw mirror requires show_raw_agent_reasoning |
| 44 | legacy missing ordinal counterexample: physical offset locator, partial mixed corpus |
| 45 | duplicate summary key must be detected, never silently last-wins |
| 46 | intentionally incomplete final JSON record: preserve complete prefix and report partial |

Source mapping: history/lib.rs:64-123 (metadata), :362 (envelope/optional ordinal),
history/rollout_payload.rs:33,239 (metadata and replacement_history),
protocol/models.rs:1050,1229,1245,1626,1986-1995 (reasoning/compaction/content/tags),
protocol/items.rs:192 (typed Reasoning mirror), protocol/protocol.rs:794,
1424-1434,2229,2293,2305,2372,3159,3303,3324,3699,4134,4250 (modes, task aliases, settings, usage,
identity, world/turn context, rollback, goals and aborted turns), and
rollout/policy.rs:21-100,127-283 (persistence filtering, including Other), rollout/ordinal.rs:23-42
(legacy omits ordinals). All paths refer to codex-rs
at the full commit above, not a moving branch. Unknown/fault inputs stay
explicitly unsupported/partial. Downstream parser tests must select the listed
case/slice rather than treating this mixed corpus as one complete session.


## CX-14 Codex capability projection fixture (2026-10-19)

`codex-capabilities-01621.jsonl` is a separate **synthetic functional fixture**
against Codex 0.162.1 source `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`.
It is consumed only by feature `test-live-agent`, explicitly ignored tests in
`codex_capture_local_test`; it is never an account capture or native-origin proof.
It does not replace `codex.jsonl` or the CX-12 fault corpus, whose exact bytes and
damaged final line remain frozen. The original 8725-byte OpenCode prefix is unchanged.

The fixture declares paginated mode and monotonically increasing ordinals.
Both modified-file records are completed `FileChange` items, rather than mixing
legacy-only patch events into paginated history. `turn_context.summary=none` is
the upstream compatibility placeholder, not a claim about effective thinking mode.
All paths, model IDs, messages and opaque values are synthetic; nothing is executed.
Expected projection: two prompts (one `/review`), two response calls, input=17,
output=7, cached read+write=4, reasoning=1, total=24, latest model
`codex-fixture-model-b`, files `src/snake.rs` then `src/ui.rs`.
The duplicate user response surfaces and compaction usage snapshot must not add
prompts or tokens. Unknown-usage and offset behavior are separate local cases.

Functional fixture SHA-256: `b5adfa85de3001f7583999825c22027b85449a183e10d9bedf2710ac99005c6e`; 7370 bytes; 19 JSONL records.
