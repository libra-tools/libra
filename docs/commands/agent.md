# `libra agent`

> **Coverage-v2 source version:** The coverage-v2 descriptions below refer to the Libra 0.30.50 source line and later compatible versions. This is not a stable-release announcement; check the [official releases](https://github.com/libra-tools/libra/releases) and `libra --version` for installed support. Other sections retain their own version requirements. Coverage-v2 does not prove native origin or authorize native reasoning artifacts.

Manage external-agent capture for Claude Code, Codex, OpenCode, and Grok.

Grok is supported and hook-installable with `support_wave=grok`, not `first_batch`, and is not a `review` or `investigate` launcher. **Automatic Thinking capture is paused.** Hook lifecycle events are recorded through the shared pipeline; decoding does not read session logs or write `GROK_HOME/libra-capture`. Existing readers for local `updates.jsonl` and `chat_history.jsonl` are not yet connected to the authorized snapshot/checkpoint writer. Offline observer JSON is scratch state, not a Libra checkpoint. The intended transcript retains Thinking text and unproven `encrypted_content`; Libra does not decrypt it. Current synthetic reader tests do not prove that live checkpoints contain it. Default `checkpoint show` does not print Thinking plaintext; an explicit transcript path returns only content actually stored.

Hermes is supported and hook-installable with `support_wave=hermes`, not `first_batch`, and is not a `review` or `investigate` launcher. Migration `2026103001` accepts `agent_kind=hermes` and keeps every previously accepted kind, including `grok` and `opencode`; it does not add `pi` (see the next paragraph). The internal reader accepts the official JSONL export: rows with both `active` and `compacted` false stay out, and a non-empty `reasoning` or `reasoning_content` string is kept. `reasoning_details`, `codex_reasoning_items`, and `codex_message_items` stay raw strings. When that export is unavailable, Libra reads a read-only SQLite view and does not migrate the provider database. `libra agent list` includes `hermes`. Install writes `HERMES_HOME/plugins/libra-hermes-capture` only when the file contains the `libra capture` marker and its sha256 matches the recorded hash. An unmanaged or drifted file is left unchanged. A bare `libra agent enable` skips Hermes when `hermes` on `PATH` is missing or is not v0.22.5 `bf0203a`, and an explicit enable in that case changes no files. Review and investigate launch stay closed. `cloud sync` still refuses `hermes` before any remote write (`LBR-CONFLICT-002`, also with `--force`): while such local-only capture sessions exist the refusal persists until a release adds cloud support for the kind, and the hint points to an external backup of the whole repository meanwhile (see [`cloud`](cloud.md)). `libra agent import --path <session-id>.jsonl --agent hermes --yes` imports one official export into the local repository and does not scan a Hermes home. Messages are ordered by id. Thinking strings stay attached and unproven, and are not copied into the assistant answer. Absent usage is unknown, not zero. A non-default profile, a non-cli source, and an unproven parent are refused. Repeating the import does not write another checkpoint. The bounded snapshot keeps that thinking as unproven and partial, and refuses retained thinking text above 8 MiB. It is not the assistant answer, and a synthetic export does not pass the real-session acquisition gate. The live bridge runs `hermes sessions export --format jsonl --session-id <id> <output-file>` only while it holds the export-job lease, with a 2 second deadline. `libra agent rpc trust hermes` pins that executable and does not accept OpenCode trust. A timeout is partial and can be retried. `libra agent doctor` reports the bridge as source-qualified and does not claim native origin. `libra agent hooks hermes observe` accepts the proven observers. `on_session_end` records a turn and does not close the session. `on_session_finalize` is the teardown and uses the shared capture finalizer. Control hooks and subagent events are not registered. The installed plugin forwards only identity fields and does not read the Hermes database. A local CLI export may use `source=cli` or Hermes's `source=local` together with `created_source=cli`, and may carry `user_id` of the form `uid:<current-euid>`. Every other source or non-empty user id stays refused. CI uses fake exports. The real acquisition check uses an isolated Hermes profile and does not read the operator's `~/.hermes` state. Coverage still does not include cloud sync: `cloud sync` keeps refusing `hermes`. Recovery is forward-only: a drifted capture file is not overwritten or deleted.

Pi is registered as a repository identity only. Migration `2026103101` extends the repository `agent_session.agent_kind` CHECK with `pi` on the dedicated migration connection and keeps every previously accepted kind, including `grok`, `opencode`, and `hermes`. `pi` is `supported=false`: `agent list` omits it, `add`/`enable` returns the unsupported error, `disable`/`remove` refuses because there are no hooks to remove, and `agent import --agent pi` is rejected. Discovery, capture, hook install, and review or investigate launch stay disabled, so no command writes a `pi` session yet. `pi` is now a built-in slug, so an external `libra-agent-pi` binary is never trusted or invoked in its place (`LBR-AGENT-006`). `cloud sync` refuses local `pi` sessions before any remote write, as it does for `hermes`. Rolling the migration back is refused while any `pi` session exists, and those rows are kept. A Libra binary older than this migration refuses to open a repository that carries it, changes no file, and asks for a newer binary.

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

In this development source, consented `agent import --path <rollout.jsonl> --agent codex --yes`
preserves the available Codex `response_item` reasoning source values, including
`summary_text`, `reasoning_text`, `encrypted_content`, and unknown associated
JSON fields. Credential patterns are redacted after decoding JSON string escapes;
other values, including large integers and precise decimals, are retained.
Missing, null, empty and nonempty values remain distinct. Retained source data
is **Unproven**; opaque/unsupported data and uncertain attribution are **Partial**.
A reasoning summary is provider-visible text, not proof of complete internal reasoning.

Default `agent checkpoint export <id>` returns the retained ordinary values
without `--raw --allow-raw`. Its JSONL may contain the existing per-turn semantic
projection followed by retained source records; consumers must read all lines.
When only an opaque/associated value changes after a turn is covered, import
writes a supplement checkpoint. Replaying the same retained data is idempotent;
old checkpoints and V1 semantic claims remain unchanged. The separate V2 claim
migration belongs to CX-05. No native artifact or decryption authority is granted.

The sidecar contains counts, explicit-turn token snapshots and partial status,
without thinking text or ciphertext. Large ordinary tool output and a damaged
trailing record do not erase valid thinking from earlier records; unreadable
string tokens are redacted locally and remain structurally partial. Existing
consent, repository/session checks, source byte caps and deadlines still apply.
These ordinary values are part of the checkpoint and travel with its export,
agent push or cloud backup; there is no separate thinking-exclusion switch.


The stored sidecar is a snapshot of explicit Codex provider turns (`turn_ordinal_namespace=codex_explicit_turn`); its ordinals are separate from V1 checkpoint attribution. Metadata-only provider updates do not create a checkpoint. The full turn table lives in the shared `redaction_report.json` object; each checkpoint's metadata carries bounded aggregate counts. A statistics/retention preparation failure is reported as partial or `retention_unavailable` while existing V1 projections remain usable.

At most 16 retention supplements are written in one import pass before fresh turns proceed. A partial result reports durable progress and releases owned reservations; rerunning resumes the remaining copies. Fresh retained checkpoints and supplements use repository-keyed IDs bound to semantic digest, completeness and retained values. Identical replay remains a no-op after repack. Changed-data writes after a complete traces repack still hit the existing packed-parent writer limitation; preserve the source and repository for repair. The local packed-transcript case uses a valid mixed store and does not claim whole-traces packed-parent support.

The CX-15 checks use synthetic files through the actual import/store/export
path. This is not a live model acceptance: system Codex hook installation,
rollout discovery and the TUI snake smoke remain separate plan gates.


## Codex turn attribution (CX-05 development)

The first nonempty persisted record pins the typed coverage preview's codec for the entire source snapshot. A `session_meta` header recording `cli_version: 0.162.1` or `0.163.0` selects the current structured codec; other or missing versions keep the legacy codec. Appending current markers to an older resumed session cannot reinterpret its prefix. Supply the complete source snapshot; an isolated tail is not an equivalent coverage source. The owning header remains in the ordinary source. Future automatic writer activation must persist this codec choice with the session rather than infer a new choice on retry.

The current codec groups inputs by validated `turn_id` from `turn_context`, lifecycle markers or accepted input metadata. Steering inputs in one turn share a group. Scoped responses and late input mirrors do not rewind the active stream context. An unscoped or matching-turn terminal marker closes it. A malformed terminal scope leaves the stream open and qualifies the projection. An id-less optional turn context preserves a lifecycle-established scope while qualifying the projection; without lifecycle proof it clears the context. An explicit null or invalid turn ID clears it. Fresh raw input scoped to an older turn stays in that group and qualifies ordering, while following unscoped output keeps the active turn. Missing or invalid turn IDs use incomplete ordinal compatibility keys based on group creation order, separately from dense projection ordinals; earlier empty groups becoming visible do not shift those keys.

Producer `content_item_kinds` and accepted-input events distinguish human requests from injected context. Developer/system messages, inherited history and foreign-thread records do not create human turns. Genuine text resembling a context wrapper is preserved. Repeated input on one surface remains repeated. Per-turn occurrence pairing collapses matching event/raw/completed surfaces; raw, client and completed-item IDs use independent namespaces; missing or unequal IDs leave a pair partial, so ordinary current sessions are normally partial. Non-text content, conflicts and unpaired event inputs also remain partial. Only annotated raw or scoped completed input can establish a user record; unscoped legacy accepted-input events corroborate matching inputs and never independently establish human turns; mismatched mirror text cannot add inferred user records beside the more direct source. Legacy role-only projections may include injected context and do not prove human origin. All source values remain available to the existing ordinary retention path under credential scanning.

This data-only path reuses the existing response codec and V1/V2 selector: typed readable reasoning selects V2; ordinary records still select V1. Historical V1 normalization, ordinary-retention writes and existing claims keep their prior keys and digests. Automatic Codex writer activation remains later work. Current-codec ordinary preview rows labelled V1 must not be persisted against legacy V1 claims: activation must explicitly use the existing ordinary V2 domain or keep sessions with V1 claims on the legacy codec, alongside a persisted codec choice. Explicit prepared ordinary V2 imports already use the shared replay gate. The reasoning sidecar keeps its separate `codex_explicit_turn` ordinal namespace; its counters must not be joined to coverage ordinals by position.

The existing snapshot hard cap is 16 MiB. The current codec also caps grouped bytes at 16 MiB, each JSONL record at 1 MiB and turn, input-occurrence and identity collections at 4096 entries. A large patch, tool output or inline image data URL can make this pure preview unavailable. Exact-version compatibility and large-record availability must be reviewed before automatic writer activation. Exhaustion or an expired deadline fails the preview; empty or unattributed partial groups retain a partial signal. Damage within an attributable group qualifies that group, while unassigned damage qualifies the snapshot. Ordinary retention follows its existing path. Source and synthetic checks do not establish live model capture or native provenance.

## Codex rollout metadata and capabilities (CX-04/CX-14 development)

Current source builds project envelope rollouts using the Codex 0.162.1 record schema; unknown records from other versions remain partial. Exact `token_usage_record` response usage is counted once, including a repeated compaction snapshot. Legacy cumulative token observations are differenced across turns; an unexplained decrease remains partial, and a reset at an explicit context boundary is qualified. Matching legacy observations are not added to exact response totals. Cached tokens aggregate cache reads and writes. Missing or invalid usage remains unknown in the parser, rather than a measured zero.

Envelope prompt projection uses event-side accepted input and producer `content_item_kinds`, preserving real user text that resembles a context wrapper. Unannotated or mismatched user-role response content is retained in ordinary source data but is not asserted to be a user prompt. Shared item identity is paired first, with completed input preferred; otherwise identical projected text is paired by turn and occurrence across surfaces. Repeated text on the same surface remains repeated. Pairing without a shared identity is a partial lower bound. The first `session_meta` binds the owning thread. Foreign headers make the projection partial; foreign records are not added to its usage, and all unscoped records after a foreign header remain unattributed until the owning header reappears; explicitly scoped owning-thread records can still be projected. The model is the latest explicit `turn_context` model. Modified paths require a nonempty call/item identity and a successful, completed structured patch record; proposed patch text and failed or partial attempts are not evidence of a file modification. Paths are lexical and relative to the recorded working directory, which may be a repository subdirectory.

The parser reads at most 16 MiB, caps full JSONL decoding at 1 MiB per record, and bounds retained metadata and identity state. For an oversized known record, capability projections may read only a bounded type/role/scope skeleton while ignoring the body; a large image or tool output therefore does not suppress unrelated exact usage or completed files. Affected or unclassifiable projections still fail the record limit, and the underlying parser marks skipped bodies partial. Whitespace-only lines do not contain records. The hook helper applies its existing tighter limits. Unknown variants, duplicate keys, damaged tails, unsupported old patch statuses and exhausted budgets yield partial metadata and content-free warnings. Flat historical exports retain a compatibility reader with bounded metadata; oversized, invalid or unclassifiable rows are skipped and qualified without erasing other usable flat rows. Flat role-only user projections may include host-injected context and are not evidence of a human request. Mixed formats are partial, and flat rows inside an envelope file are not projected. These rules do not grant raw storage permission, change coverage claims, or prove live thinking capture. The Codex adapter now exposes the transcript analyzer and reports completed structured file changes through `libra agent list --json` capabilities. Its token calculator returns an actionable error when usage is unavailable or usage-specific consistency checks prove a gap, rather than manufacturing zero or returning a known incomplete total. A qualified explicit reset may still yield a lower bound. For nonzero offsets in a legacy-first source, subsequent windows keep cumulative-counter deltas even when exact records appear later; absent newer cumulative observations remain unknown, which prevents late exact mirrors from being counted again. Prompt, skill, file and usage offset reads retain earlier context and cumulative baselines while emitting newly completed records; previously seen item/response mirrors and an added newline alone do not replay old data. Pass the complete append-only byte snapshot together with its previous byte length, rather than only a sliced tail; a truncated or replaced source requires restarting at offset zero. If exact records exist only before the offset, a later cumulative observation cannot establish new exact usage. These are bounded best-effort projections; the analyzer position is the supplied snapshot length, not proof that every byte produced complete metadata. Prefix prompt payloads are hashed under the record cap; earlier text does not consume new-output string or retained-text limits. Current scope and output context remain bounded. Skill projection hashes supported prompt text for pairing and retains only recognized slash commands and their context. Prompt projection does not retain skill events. Skill event IDs use source identity (or a qualified occurrence fallback), and flat compatibility reads retain physical record ordinals across offsets. Each capability projects only its own metadata: a prompt budget failure does not suppress otherwise valid tokens or completed files, and response/patch identity limits do not suppress prompts. Shared input and record limits still apply to every projection. Budget errors name the affected capability and limit and state that the session cannot be projected by that capability; waiting or restarting the same oversized snapshot will not repair it. The underlying parser still offers partial metadata. Missing usage gets the ordinary waiting diagnostic unless usage-specific counters, response identity or reset observations are unproven; an unrelated partial prompt does not make usage malformed. An offset-zero total replaces earlier totals and must not be added as another delta. Capture treats calculator errors as partial metadata and continues the shared checkpoint path. Other adapters retain their existing contracts.

## Native Windows capture

This support is available in **current main source builds** and is not included
in the **v0.30.50 release binaries**. Until a published release includes it, use
the [source build instructions](../../README.md#install).

Windows builds support Session Capture on local NTFS drives, without WSL.
Install **OpenCode 2.x** (the GitHub 1.19.0 CLI has an incompatible plugin API),
and place its configuration in `%USERPROFILE%\.config\opencode\opencode.json`.
The native CLI is available from
<https://opencode.ai/update/api/latest/cli/opencode/opencode-windows-x64.zip>.
In PowerShell, with `libra.exe` and `opencode.exe` on PATH:

```powershell
libra init
libra agent add opencode
libra agent rpc trust --dir C:\path\to\verified-opencode2
libra agent rpc trust opencode
opencode run --standalone "Reply with a short greeting."
libra agent session list
libra agent checkpoint list
libra agent checkpoint export <checkpoint-id>
```

Windows validates local paths with no-follow `CreateFileW` handles and rejects
reparse points (including junctions), alternate streams, device names, and UNC
capture storage. Repository-private keys use a current-user/SYSTEM ACL,
`LockFileEx` exclusion, and atomic no-replace publication. A missing or damaged
key is never silently replaced during authentication or recovery.

The Windows OpenCode exporter runs **without an OS filesystem/network sandbox**.
Only an explicitly trusted `opencode.exe` can run; its bytes and file identity
are revalidated, and the executable is held against replacement. A Windows Job
Object contains the process tree, with a 30-second deadline and a 16 MiB accepted
output limit. Output is held in an ACL-protected temporary file and removed
after reading; stderr and ambient credential variables are not forwarded.
The size is checked while the process runs and after it exits; Windows has no
Unix `RLIMIT_FSIZE` write-time limit, so temporary output can exceed the accepted
limit between checks. Linux bubblewrap and macOS Seatbelt requirements are
unchanged. Windows capability availability requires trusted exporter bytes;
it does not claim that an OS sandbox is present.

Windows checkpoint listing and ordinary transcript export are available.
Owner-only raw reasoning artifact export and the object/findings scans in
`libra agent doctor` remain unavailable on Windows; doctor reports these scans
as unavailable and applies no repairs to them.

## OpenCode ordinary capture on macOS (Libra 0.30.49)

Use the official OpenCode **2.0.29** for ordinary automatic capture. The frozen
2.0.26 vocabulary and private native producer sections below describe separate
historical/explicit native lanes; they are not prerequisites for ordinary capture.

Libra 0.30.50 adds session-directory filtering, native executable selection
behind supported package-manager launchers, and an explicit warning for empty
metadata-only checkpoint exports. The corresponding behavior below requires
Libra 0.30.50 or later.

From the repository you want to capture, check the installed versions and prepare
its exporter trust and observer:

```bash
libra --version
opencode --version
libra agent rpc trust --dir /absolute/directory/containing/opencode
libra agent rpc trust opencode
libra agent add opencode
libra --json agent list --schema-version 2
libra agent doctor
```

Use the real directory containing the reviewed executable. The current CLI takes
directory registration and binary trust as two separate commands; it does not
accept a binary filename as the `opencode` slug or combine `--dir` with that slug.
Run OpenCode from this repository after installing or refreshing the observer;
`--pure` / `OPENCODE_PURE=1` disables its capture plugin. After a captured turn,
inspect and export the stored checkpoint:

```bash
libra agent session list
libra agent checkpoint list
libra agent checkpoint show <checkpoint-id>
libra agent checkpoint export <checkpoint-id> -o capture.json
```

An authorized source with unsupported reasoning state may save its safe ordinary
copy as `Unproven` / `Partial`. This includes prompt, model, token projections,
readable reasoning text, and unproven reasoning fields. Unsupported reasoning
state and timing are written into that copy.
It grants no
native artifact, Complete coverage claim or native ACK. Any original `originProof`
or sealed candidate remains outside this fallback. Historical imports containing
unsupported reasoning state still fail with `LBR-AGENT-020`; ordinary capture
support does not enable that import or durable partial replay. Real native
`signature`, `redactedData` and `reasoningEncryptedContent` source and archival
remain separate gates; Source A remains UNPROVEN. OpenCode/account/model checks
run only locally on macOS, outside CI.

## Synopsis

```bash
libra agent status
libra agent list [--schema-version <1|2>] [--json]
libra agent import (--session <id> | --path <path> | --since <rfc3339> | --all) [--agent <name>] [--limit <n>] [--cursor <n>] --yes
libra --json agent graph <session> [--repo <path>]
libra agent enable [--agent <name>]... [--hook-timeout <seconds>]
libra agent add [<name>...]
libra agent disable [--agent <name>]...
libra agent remove [<name>...]
libra agent session <subcommand>
libra agent checkpoint <subcommand>
libra agent skill <subcommand>
libra agent clean [--all]
libra agent doctor [--repair]
libra agent workspace list [--limit <n>] [--cursor <token>] [--state <state>]...
libra agent workspace show <workspace-id>
libra agent push [--remote <name>] [--force-rewrite]
libra agent rpc <subcommand>
libra agent bridge --stdio
libra agent mcp --stdio
```

## Description

`libra agent` manages Libra's external-agent capture surface. It installs and
removes provider hooks, reports captured session/checkpoint state, exposes
read-only diagnostics, and can push `refs/libra/traces` to a remote.

`libra agent workspace list|show` is the read-only machine interface over
the workspace registry (`workspace_record`): every linked worktree, task
worktree, or remote workspace an agent runtime has associated, with its
lifecycle state (`provisioning`/`active`/`releasing`/`released`/
`orphaned`), owner, lease fence/expiry, and canonical path. `list` uses
keyset pagination (`workspace_id` ascending; default `--limit 50`, capped
at 500; round-trip `next_cursor` verbatim) and accepts repeatable
`--state` filters; `show <workspace-id>` returns one frozen
schema-v1 record. Lease mutation is never exposed here — it stays inside
the agent runtime's internal services.

The supported roster is `claude-code`, `codex` and `opencode` (first batch),
and all three are hook-installable: `claude-code` writes `.claude/settings.json`,
`codex` writes user-level `$CODEX_HOME/hooks.json` plus Libra-managed trust
entries in `$CODEX_HOME/config.toml` (untrusted Codex hooks are skipped
silently, so trust entries are part of the install), and `opencode` writes the
Libra-managed plugin `.opencode/plugin/libra-hooks.js` (note: `opencode --pure`
disables all external plugins, including capture).
`gemini` was demoted out of the supported roster and is uninstall-only:
`libra agent remove gemini` removes previously installed Libra-managed hooks
(idempotent), captured sessions stay readable, and `add`/`enable` for it — or
for any other non-roster agent — return an actionable unsupported error.

For Codex, `enable` and `disable` update the coupled `$CODEX_HOME/hooks.json`
and `config.toml` under a cooperative Libra-process lock. They prepare both
files first and reject an edit observed before a replacement, but an arbitrary
editor cannot be covered by a portable atomic compare-and-replace. Do not edit
either file while the command runs. A reported non-plaintext integrity
recovery-journal path means no further automatic overwrite is safe; preserve
that journal, resolve the edit manually, then remove the reported journal
before retrying. It records limited transaction metadata (schema version,
operation, phase, and a recovery fence) plus domain-separated file
fingerprints, never raw hook or config contents; an unknown fingerprint state
is intentionally zero-write rather than an automatic repair.
On Unix, Codex keys trust by the canonical `hooks.json` path. `enable` resolves an
existing `$CODEX_HOME` directory before writing trust entries, including
macOS aliases such as `/tmp` and `/private/tmp`, and removes older
Libra-managed entries written under the caller's alias. Re-run
`libra agent enable --agent codex` after upgrading if `agent doctor` reports
Codex hooks as untrusted; review any intentionally disabled entries first.

### OpenCode event input contract

The frozen OpenCode event contract below records the audited **2.0.26** vocabulary;
the current ordinary 2.0.29 baseline is described above and in its dedicated section. Its current
lifecycle inputs are `session.created`, user `session.inbox.delivered`,
`session.execution.succeeded`, `session.execution.failed`,
`session.execution.interrupted`, `session.deleted`, and
`session.compaction.ended`; `tool.execute.after` is a separate observation hook.
Delivery requires `role="user"` and a prompt string, including an empty string.
Only `user`, `superseded`, and `inactivity` interruptions end a turn; `shutdown`
keeps restart continuity. An enqueue is merged by the plugin and is never a
standalone lifecycle input.

Compatibility inputs are `session.status(idle)`, `message.updated`,
`session.compacted`, and `server.instance.disposed` with a tracked session ID.
Busy/retry status frames are rejected. The parser requires a prompt string for
legacy `message.updated`; an empty string and an omitted role are accepted,
and a declared role must be `user`. Neither `message` nor `user_prompt` fills a
missing prompt. The deprecated `session.idle` alias remains accepted, with a
content-free `legacy_event_alias` tracing warning.

The managed plugin uses the OpenCode 2.0.26 `setup(ctx)` API, an event
subscription and an observe-only tool hook. One OpenCode server (for example
`opencode serve --service`) delivers every project's events to every project's
plugin. In Libra 0.30.50 and later, sessions known to belong to another
`location.directory` are not forwarded, and a session moved out of the directory
is closed here. Events without a location are forwarded unless their session
was placed elsewhere. A tool call for a session not yet
placed is attributed through the host's `session.get` (when the host cannot
answer, it is forwarded as before and `session_location_unavailable` is reported
once). Refresh it with `libra agent enable --agent opencode`: status reports old
or edited templates, and duplicate managed copies, as not installed until
refreshed.
Node and Bun use the same `node:child_process` transport with the pinned
absolute Libra filename and `shell:false`. Session/turn boundaries run
synchronously; tool and compaction observations run asynchronously and never
change host arguments or return context/control output.

User inbox enqueue/delivery pairs open one turn per classified inbox ID within
the bounded live history window. Frames also carry a stable `message_id` for
Libra’s existing bounded receipt ledger; this is not an unlimited historical
replay guarantee. Legacy
user messages pair with their first text part, or send an explicit empty string
before the next unrelated event or cleanup. Assistant step events supply the
model. Buffers are bounded: 64 tracked sessions process-wide, 128 pending prompts, 128 early
text parts, 256 classified inbox/message IDs and 16 observation children;
prompts are limited to 16,384 UTF-16 units without a split surrogate. Capacity
or unavailable text produces a fixed, content-free diagnostic at most once per
reason per plugin instance; truncated
prompts carry `prompt_truncated=true`. ID history is a rolling 256-entry window and is pruned when a session ends;
new prompts keep flowing after the window fills. When the session window fills,
the least recently observed session is flushed with `server.instance.disposed`
and the new session is admitted. A later observation can reopen an evicted
session; this closes Libra tracking only and does not delete or stop the OpenCode
session. Early text also uses a rolling window.

Cleanup and process exit flush all tracked sessions using one shared deadline
per cleanup or process exit, respectively. Forwarding is best effort and fails
open for the OpenCode host. Nonzero child exit or timeout emits only a fixed
`forward_failed` / `forward_timeout` diagnostic, at most once per reason per plugin instance. Each child has the current export deadline plus
15 seconds (currently 45 seconds), with `SIGKILL` at timeout. Install checks both
plugin layouts, rejects symlinks and special files before reading, preserves
user-owned files and rolls back its canonical update if managed duplicate
removal fails. These checks assume stable directory names; concurrent directory
relocation and edits after the final ownership check remain deferred.
`opencode --pure` / `OPENCODE_PURE=1` disables all external plugins and hook
capture. Local Node/Bun tests use synthetic events and a controlled exporter;
real OpenCode content capture and native reasoning proof require their separate
acceptance gates.

OpenCode 2.0.26 export metadata uses `user.text`, assistant `model.id`,
`tokens` and successful file tools. The assistant token total is
`input + output + reasoning + cache.read + cache.write`; cached tokens are
`cache.read + cache.write`. Session-level totals are not added again. Classic
`info/parts` exports remain compatible: assistant aggregate tokens take
precedence over deduplicated step-finish details. User model fields are ignored.
Modified files combine successful edit/write/patch paths and snapshot files,
with validated patch move sources and destinations. Output paths are project
relative with forward slashes; location-relative tool paths use the session's
proven location, while snapshots already use the project root. Missing history,
external or invalid paths, unsupported shapes, invalid token numbers and
collection limits produce partial metadata with content-free warnings.
Idle, model/agent switches, synthetic/system/skill messages, shell and compaction
are recognized metadata and do not become human prompts. Internal ID, path and
alias limits also mark partial; at the ID cap later messages are omitted.
Extraction does not read files or archive reasoning content, and synthetic
fixtures do not prove real macOS content capture or native reasoning origin.

The legacy ordinary OpenCode coverage normalizer accepts native 2.0.26 messages and classic
`info/parts`. Each human message starts its own stable logical turn. A valid
native `idle` settles all pending human splits, including failed/interrupted
outcomes; Complete describes captured representation, not task success.
Semantic messages after the last valid idle stay Incomplete. Unknown/malformed
content and reasoning stay Incomplete and are never repaired by idle. The nine
classic metadata parts (step-finish, patch, step-start, snapshot, retry,
compaction, agent, subtask, file) are recognized without semantic projection.
Native user attachments, skill text, and tool file results are unsupported:
they mark Incomplete without copying attachment bytes or URIs into records.
Valid agent/skill metadata, model/usage and provider state do not change the
digest. Native tools use name/id/state.input and text state.content; an error
without content projects only its structured error.message, never response.body.
No reasoning bytes or provider state establish trusted native origin here.

OpenCode user prompts pass through the same reminder filter for native/classic
coverage and native/classic/flat-JSONL metadata extraction. Exact literal
`<system-reminder>...</system-reminder>` blocks, including nested blocks, are
removed wherever they occur; text outside them stays byte-for-byte unchanged.
An injection-only message adds no user prompt or user semantic record. The
filter also runs before skill matching; independent token metadata is retained.
An unfinished block suppresses its tail and marks partial/Incomplete; a stray
closing tag is retained and marks partial/Incomplete. Idle cannot clear these
violations. Only user text is filtered, and other providers keep their existing
semantics. This is a literal marker filter, not proof of producer origin.

OpenCode extraction is fail-visible for empty input, invalid JSON, and illegal
export shapes: `extraction.partial=true` and fixed payload-free warnings are
recorded. Valid independent flat model/token fields remain projected even when
the role is missing or unsupported. A valid empty session export is distinct
from missing transcript bytes (`extraction.present=false`). Noncritical
extraction errors do not block an otherwise admissible checkpoint; malformed
exports do not replace an existing checkpoint. Coverage, authorization, and
native-origin gates remain required.

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `status` | Report captured external-agent session status |
| `list` | List the supported agents with their capability matrix (roster, hooks, install state) |
| `import` | Discover and import historical Claude/Codex transcript files or one trusted, sandboxed OpenCode export after explicit consent |
| `graph <session>` | Inspect the read-only session → turn → revision → subagent capture graph; requires global `--json`/`--machine` (the interactive TUI entry was removed in the W5 breaking release) |
| `enable` | Enable one or more external agents and install hooks |
| `add` | Alias of `enable`: `add <name>` ≡ `enable --agent <name>` |
| `disable` | Disable one or more external agents and uninstall hooks |
| `remove` | Alias of `disable`: `remove <name>` ≡ `disable --agent <name>` |
| `session list` | List captured sessions |
| `session show <id>` | Show a captured session, including a non-sensitive retryable checkpoint-capture diagnostic when the last Codex Stop failed |
| `session stop <id>` | Mark a captured session as stopped |
| `session resume <id>` | Mark a stopped captured session active again |
| `session derive-tool-calls <id>` | Derive tool-call records from a captured session |
| `checkpoint list` | List captured checkpoints |
| `checkpoint show <id>` | Show a safe structural checkpoint summary (metadata and internal object IDs are withheld) |
| `checkpoint rewind <id>` | Inspect or apply a working-tree rewind for one checkpoint |
| `checkpoint export <id>` | Export a checkpoint's transcript. Redacted by default (no authorization); raw (un-redacted) export requires `--allow-raw --raw` and is recorded in the append-only `agent_audit_log` (`LBR-AGENT-013` when refused without it). A checkpoint recorded without a transcript (metadata-only capture — for OpenCode, the export bridge was unavailable or failed when it was written) exports zero bytes and emits a warning |
| `skill search` | Search captured skill events by `--skill`, `--provider`, `--session`, and RFC3339 `--since`/`--until` (keyset-paginated with `--limit`/`--cursor`, `--json`). A read-time projection over checkpoint metadata — no dedicated table |
| `skill list` | Alias of `skill search` (same filters) |
| `skill registry` | Show the curated per-agent discoverable-skill registry (`--provider <slug>` to scope; the public SkillDiscoverer surface) |
| `clean` | Clean up temporary checkpoints from stopped sessions (prune fails closed while a checkpoint write is in flight, the traces ref reaches uncataloged commits, or durable object-index repair remains pending; also drops `object_index` rows made unreachable) |
| `doctor` | Diagnose hook installation and capture state; detect (and with `--repair` fix) checkpoint-store inconsistencies. Its object checks deliberately inspect only this repository's local object directory: unavailable alternate/remote objects are reported conservatively rather than resolving an externally controlled alternate path while producing diagnostics. The read-only `legacy_code_residue` field (human: a "Frozen Code residue" line) reports whether the frozen Code-era paths `.libra/sessions/code/` and `.libra/code/` and the `libra/intent` ref still exist; it never deletes or rewrites that state — cleanup is tracked separately (plan-20260920 ADR-RC-04 / DEFER-RC-02) |
| `push` | Push `refs/libra/traces` to a remote (`--force-rewrite` for the non-fast-forward push after a `clean` prune, using force-with-lease) |
| `rpc list` | List discovered `libra-agent-*` binaries on `PATH` (with trusted/quarantined state); requires the external-agents opt-in |
| `rpc trust <slug>` | Trust a discovered binary — records path + sha256 + device/inode/mtime provenance (refused when its directory is world-writable, or when the binary is not under a trusted directory — `LBR-AGENT-005`). The provider-exporter slug `opencode` instead pins the provider's own CLI binary — resolved only from registered trusted directories, never `$PATH` — for the sandboxed export bridge; this form needs no external-agents opt-in. A mise (aube) or pnpm `.bin` launcher shim is followed to the native executable it declares, which must itself be under a trusted directory (reported as `launcher` in `--json`); a JavaScript launcher is refused (`LBR-AGENT-005`). Provenance is hashed in one streaming pass with no fixed size cap, so large single-file provider CLIs (the OpenCode Bun build is ~171 MiB) stay trustable; a file that yields more bytes than its reported size while being hashed is refused (`LBR-AGENT-005`) |
| `rpc trust --dir <path>` | Register a trusted directory (`agent.external_agents.trusted_dirs`, default `~/.libra/agents`): external binaries are only trustable when their canonical path lives under one. The path is canonicalized and must be an existing, non-world-writable directory |
| `rpc untrust <slug>` | Revoke binary trust; OpenCode also revokes native issuers atomically (see native-origin rotation and revocation below). Always available, even while external agents are disabled |
| `rpc invoke` | Invoke one JSON-RPC method on a trusted `libra-agent-*` binary |
| `bridge --stdio` | Run the repository-scoped DeepSeek Harness bridge over stdin/stdout (JSON-RPC 2.0 NDJSON). The **only** standard inbound write transport for Harness; it is not `libra code --control`. stdout carries exactly one protocol frame per response; diagnostics go to stderr. Protocol v1: 20-method allowlist, 256 KiB frame cap, 64 in-flight requests, 30 s default deadline. All 20 methods are implemented: the `initialize` handshake; session/event ingress `session.open`, `event.append` (batch ack / idempotent / digest-conflict / server-side redaction), `session.flush`, `session.close`, `evidence.append`, `provenance.append`; the read methods `context.get`, `status.get`, `history.search`, `checkpoint.list`, `checkpoint.show` and `diff.get`; the mutations `checkpoint.create`, `commit.create`, `checkpoint.restore` and `review.run` (each with `operation_id` idempotency, actor binding and approval gating); and workspace lease `workspace.claim` / `workspace.renew` / `workspace.release` (owner derived from the authenticated bridge session). `diff.get` takes a closed `mode` (`worktree` / `staged` / `checkpoint`) plus validated repository-relative `paths` — never a free-form revision or pathspec — and forces `--no-ext-diff` / `--no-textconv` so repository config cannot become process execution. `commit.create` commits the current index only (no `-a`, no pathspec, no amend, no author override) and records its association graph in `agent_bridge_link` rather than in the commit message. `checkpoint.restore` requires an explicit `expected_head` fence plus a clean index/worktree and never moves HEAD. `review.run` starts a read-only review and returns its `run_id`; replaying the same `operation_id` reports that run's state instead of starting a second one. A stale HEAD or dirty worktree is refused with `LBR-AGENT-038` before any write. An ack only means the redacted projection is durable, never that the Harness raw transcript has been migrated. Not a Git command. |
| `mcp --stdio` | Serve the repository Model Context Protocol endpoint on stdin/stdout (JSON-RPC 2.0, one message per line). Legacy protocol versions `2025-11-25` and `2025-06-18` answer `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`. Protocol failures are JSON-RPC errors (`-32700` parse, `-32600` invalid request or batch, `-32601` unknown method, `-32602` invalid params or unknown tool). A tool argument that fails the tool's input schema is a tool error `LBR-AGENT-027` (`isError: true`), not a protocol error. stdout carries only JSON-RPC messages. This is not `libra agent bridge` and not the removed `libra code` MCP. |

## Removed dry-run prompt output

`--show-redacted-prompt` is not a flag. The promote subcommand stays deleted, so a dry-run cannot print a prompt body. Human output and `--json` both omit the prompt. Clap rejects the invocation instead of formatting a `<len=N>` line.

## MCP server

`libra agent mcp --stdio` is the repository MCP server for external clients. The client configuration shape is `command` = `libra`, `args` = `["agent", "mcp", "--stdio"]`, and `"cwd"` set to the repository directory:

```json
{"command":"libra","args":["agent","mcp","--stdio"],"cwd":"/path/to/repo"}
```

The server speaks legacy MCP `2025-11-25` and `2025-06-18`. An unrecognized `protocolVersion` is answered as `2025-11-25`. `capabilities` is only `tools` with `listChanged: false`. JSON-RPC batches are rejected. Input that does not match a tool's schema returns tool error `LBR-AGENT-027`.

Tool results are redacted before they are written. The server redacts `structuredContent`, scrubs control characters other than newline and tab, and rebuilds the text copy from that scrubbed value. Known secret patterns are removed. If that redaction cannot be parsed again, the server returns tool error `LBR-AGENT-034` and does not fall back to the original result. A full JSON-RPC response larger than 256 KiB returns tool error `LBR-AGENT-028`. A read tool that runs longer than 30 s returns tool error `LBR-AGENT-036` with `retryable` set to true. Read tools run in at most 4 concurrent worker processes, each with a 512 MiB resident-memory budget. A detected budget violation ends the worker and returns `LBR-AGENT-028` with reason `worker_memory_limit`; a worker stopped by the Windows OS memory limit may instead return `LBR-AGENT-037`. On a deadline the server replies and kills the worker before another read can take its slot.

## Common Options

| Flag | Subcommand | Description |
|------|------------|-------------|
| `--agent <name>` | `enable`, `disable` | Select agent names; omit to target the supported roster (`add`/`remove` take the names positionally) |
| `--hook-timeout <seconds>` | `enable` | Host handler timeout written into the installed Claude Code and Codex hooks (default 30). The hidden `--capture-budget-ms` is derived as the timeout minus up to 1000ms of cleanup margin (`30 → 29000`, `60 → 59000`). Claude `SessionEnd` keeps no timeout and a `9000` budget; Codex `SessionEnd` and `Interrupt` stay capped at `3s → 2000`. Every selected agent must take a host timeout: combining it with OpenCode, Grok, or Hermes fails with `LBR-CLI-002` before any hook is written. `0` or a non-number is rejected. Re-running `enable` without the flag restores the default |
| `--schema-version <1\|2>` | `list` | Select the machine schema. Version 1 is the frozen legacy row; version 2 adds `methods[]` entries for `transcript_discoverable`, `importable`, and `export_bridge` availability |
| `--session <id>` / `--path <path>` / `--since <rfc3339>` / `--all` | `import` | Select exactly one historical-import scope. `--path` also requires `--agent`; OpenCode supports explicit `--session` through its export bridge |
| `--yes` | `import` | Required for JSON/non-TTY imports; confirms that Libra may read private provider session content, redact it, and write typed projections to this repository |
| `--restore-erased` | `import` | Explicitly remove a local anti-resurrection tombstone and retry import. Requires `--yes` and appends an audit row |
| `--limit <n>` / `--cursor <n>` | `import` | Bounded discovery page (default 20, hard maximum 100) and the next zero-based cursor returned by the preceding page; one invocation also has a 64 MiB cumulative raw-input cap. The per-source cap is `min(agent.max_transcript_read_bytes, 16 MiB adapter hard cap)`; an explicit larger config prints the actual effective cap |
| `--repo <path>` | `graph` | Read capture metadata from another Libra repository instead of discovering from the current directory |
| `--limit <n>` | `session list`, `checkpoint list` | Maximum rows per page (default 50, hard cap 500 — larger values clamp with a stderr note; `0` is treated as `1`) |
| `--cursor <cursor>` | `session list`, `checkpoint list` | Opaque keyset cursor from the previous page's `next_cursor`; do not construct by hand |
| `--extract-transcript <path>` | `session show` | Re-derive and copy at most 16 MiB from the currently verified Claude Code transcript source to a new local file. This never falls back to a captured metadata path, never overwrites an existing path, and rejects oversize sources without publishing output. With `--json`, the result is the safe `{ "session": <session row>, "extracted_transcript": { "output_path": <path>, "bytes": <count> } }` envelope; the extraction block contains only the destination path and byte count; neither JSON nor human output reveals the provider source path. |
| `--all` | `clean` | Clean all stopped-session checkpoints instead of only the most recent |
| `--repair` | `doctor` | Repair detected checkpoint-store inconsistencies (rebuild stale/missing catalog rows, re-enqueue missing `object_index` rows, safely drain valid expired ordinary writer markers, and immediately drain `cleanup_pending` markers regardless of ordinary TTL); malformed markers remain `manual_required`. Also repair currently authorized, reachable agent commit links; detection-only when omitted. Journal damage to a complete frame is preserved for inspection |
| `--commit-link-page <1..100>` | `doctor` | Inspect one deterministic page of at most 100 commits reachable from this worktree's HEAD for local link recovery evidence; completeness is always unknown |
| `--codex-legacy-after <checkpoint-id>` | `doctor` | Continue the Codex legacy reasoning revision scan after the checkpoint cursor reported by the preceding doctor page; use with `--repair` to repair that page |
| `--reset-commit-link-journal` | `doctor --repair` | Explicitly discard a corrupt or unrecoverably full local commit-link journal after inspection. A readable journal with capacity is refused. This loses any trailer-free recovery evidence still in the journal; commits, checkpoints and current catalog links remain |
| `--acknowledge-commit-link-loss` | `doctor --repair --reset-commit-link-journal` | After inspecting other pages and worktrees, permit reset of a readable full journal even when their recovery evidence remains. Does not override failed repair, lease checks, or unfinished 64-link batches |
| `--commit-page <1..100>` | `session show` | Show one page of at most 64 local commit links for a scoped Codex session; page 100 reports a limit if more links exist |
| `--remote <name>` | `push` | Select the remote used for pushing agent trace refs |
| `--force-rewrite` | `push` | Allow the non-fast-forward push that follows a local `clean` prune (the traces ref is Libra-managed and rewritten as a whole chain); uses force-with-lease against the last tip this repository pushed — never an unconditional force — so a remote rewritten elsewhere still fails closed |
| `--dry-run` | `checkpoint rewind` | Show the impact without modifying files; this is the default |
| `--allow-raw` / `--raw` | `checkpoint export` | Authorize + request a raw (un-redacted) export; without `--allow-raw` a `--raw` request is refused (`LBR-AGENT-013`) and audited |
| `--justification <text>` / `-o <path>` | `checkpoint export` | Audit justification and output file for a raw export |
| `--gc` / `--retention-days <n>` / `--dry-run` | `clean` | Retention GC across three windows: (1) drop checkpoints from stopped sessions older than `agent.retention.transcript_days` (default 90; override with `--retention-days`); (2) prune reviewer stderr diagnostic logs of terminal review/investigate runs older than `agent.retention.stderr_days` (default 30) while keeping each run's aggregate record; (3) **A0-09** remove whole terminal review/investigate run directories (`findings.md`, `manifest.json`, `state.json`, reviewer logs) older than `agent.retention.findings_days` (default 90). The objectized findings blob is content-addressed and reclaimed by the repo-level `libra maintenance run --task gc` reachability pass (PD-04), which prunes orphaned findings blobs together with their `object_index` rows while live-run manifests keep their OIDs alive (per-run retention never deletes a shared object). Non-terminal/undated runs are skipped fail-safe; `agent_audit_log` is never touched. `--dry-run` reports what each window and companion cleanup *would* remove (including JSON `findings_runs_pruned` and `import_identities_pruned`) without deleting anything |
| `--apply` | `checkpoint rewind` | Restore the working tree for the selected checkpoint |

## Review and investigation workspaces

New review and investigation workspaces live under the repository's common storage at `.libra/worktree/tasks/<run-id>/<attempt-id>/workspace/`. Checkpoint-scoped runs use the sibling `checkpoint-input/` location instead. A continued investigation allocates a fresh attempt; it never overwrites an earlier attempt's workspace. The same common root is used when a run starts from a linked worktree.

Run records, findings and logs stay under `.libra/sessions/agent-runs/<run-id>/`; moving workspace files does not move session metadata or the object store. New run state and manifests use schema version 2 and retain the workspace ownership descriptor. Known version-1 records remain readable without relocating old directories; unknown versions are not rewritten or cleaned automatically.

`agent clean --gc` skips a run whose workspace ownership cannot be verified, whose state or manifest cannot be parsed, or whose workspace removal fails, then continues with other eligible runs. Its text and JSON reports include `findings_runs_skipped` and bounded `findings_gc_warnings` so the refused run can be inspected and retried. A legacy run with `checkpoint-input` inside its run metadata directory remains eligible once all other ownership checks pass; a versionless legacy manifest remains readable.

Libra's internal task sandbox API can bind a verified attempt and limit writes to its workspace and cache paths, with sensitive repository metadata read restrictions. `review` and `investigate` launch external agent CLIs in the isolated copy and do not apply that internal OS sandbox to those child processes; the external CLI's own sandbox settings determine its filesystem access. `libra agent doctor` reports unknown or unowned directories in the managed task container without deleting them.

## Scoped checkpoint resume

`review --checkpoint <id>` and `investigate start --checkpoint <id>` materialize only proven ordinary checkpoint files, read-only, under `.libra/worktree/tasks/<run-id>/<attempt-id>/checkpoint-input/`. Every materialization, including `investigate continue`, revalidates the saved spec against the explicit repository catalog and the checkpoint manifest/tree roles before creating the new attempt input. Supported wrapped checkpoints without a manifest remain compatible. An old full-list spec may include a closed `reasoning/encrypted/<64 hex>` entry only when its path and blob id exactly match the manifest and tree; that entry is excluded without opening its payload. Altered entries, unknown paths, repeated paths and ordinary role mismatches are refused. A saved id that is not a blob, or whose bytes do not hash to that id, is refused before creating the new attempt input. A catalog transport failure leaves a paused continue's state and existing input unchanged. A review setup deadline ends as the existing infrastructure error; cancellation ends as cancelled. Setup has its own deadline and does not deduct time from later reviewers. Filesystem work cooperatively checks the caller's deadline and cancellation, and its owner finishes before returning. Ordinary payload limits remain 64 MiB per file and 256 MiB total. The materialization limits and stable error codes are unchanged.

## Scoped run cleanup

`libra review clean` and `libra investigate clean` remove only finished runs of their own kind. Cleanup verifies each saved workspace descriptor and removes the owned task workspace or checkpoint input before deleting run metadata. A cleanup failure keeps the ownership and retry record; a swapped directory, link, unknown descriptor version or ownership mismatch is refused. Directory names alone do not authorize deletion. Both `--run` and `--all` check lifecycle metadata in the run's own directory before touching read-only checkpoint input. The bulk sweep keeps foreign, unfinished, unknown and corrupt runs; its `removed` count includes only successful deletions. A specifically named run with corrupt or missing state can be removed only when its own valid terminal manifest proves the run id, kind and schema. A readable foreign or unfinished state, or conflicting metadata, is refused. `investigate clean` retains its existing run lock. Bounded input cleanup refuses linked roots or metadata and oversized input; a failure returns an actionable error and does not report a deletion. Keep the run and inspect the reported links, limits or filesystem access before retrying. If I/O or cancellation interrupts cleanup after it starts, some input may already have been removed; the failed operation does not imply rollback. A proved own-kind terminal state remains cleanable when its matching manifest has no terminal marker after an interrupted state-first finalization. Identity/schema/common-field conflicts and differing non-null terminal values remain refused. Bulk qualification I/O, link or size refusals return an error; already-removed earlier runs are not rolled back. A proved own-kind terminal state also remains cleanable when its manifest is missing. A terminal review infrastructure failure may have no reviewer outcome rows while its manifest retains the requested reviewers; that supported shape is cleanable. An unparseable manifest is still refused even beside a proved terminal state: preserve the run and repair its metadata before retrying. This does not add cleanup flags or stable error codes.

## Commit association (CX-23 development)

The repository database uses `agent_session_commit` for opt-in local
association in CX-23 source builds. Its `(session_id, hash_kind, commit_oid)` key is
idempotent and local to the repository; SHA-1, SHA-256, and BLAKE3 are distinct.
Historical import does not create these links. Ordinary commits also leave them
untouched by default. Set `libra config agent.commit_trailer true` to associate
sessions explicitly. If this setting cannot be read, association stays off and
the commit reports a warning; unrelated commit work can proceed when its other
configuration is valid. An invalid readable value still rejects the commit.
With the setting enabled, Libra can associate
up to 64 committed-checkpoint sessions scoped to this repository/worktree: sessions
anchored to the current parent commit, or still-active sessions already linked
to that parent. More than 64 eligible sessions aborts the commit rather than
silently truncating the selection. The command
adds one `Libra-Agent-Session` trailer per selected session after `commit-msg`
hooks and before optional vault signing, without duplicating a user trailer.
`libra commit --no-agent-trailer` omits automatic trailers for that invocation
but keeps explicitly enabled local association. Scope, catalog, 64-session
limit and workspace lease failures still abort before HEAD moves, even with
that flag. Dry-run neither links nor previews the automatic trailer. Links are written after the ref
update, with session scope and workspace lease rechecked in one database
transaction; a failure is reported as a warning. Amend carries forward
previously linked, still-authorized sessions and adds newly eligible ones,
including when `-m` replaces the message. A trailer-free commit first writes a
checksummed prepared intent to `.libra/agent-commit-links.v1` under the same
SQLite writer lock as the ref move. The journal is limited to 10 MiB,
owner-only on Unix, and synced before moving the ref; unsafe files and failed
writes abort the commit. A failed ref move may leave an uncommitted intent.
The journal is local recovery evidence; `libra agent doctor` reads one bounded
page of commits reachable from the current worktree's HEAD without changing it,
and `doctor --repair` explicitly
replays eligible, currently authorized links. Completed prepared/linked pairs
are reclaimed when the 10 MiB cap is reached. A damaged complete record is
preserved for inspection; only a genuinely incomplete final frame is removed.
`--commit-link-page <1..100>` selects a 100-commit page. The report always says
that evidence completeness is unknown, even when no candidate is found.
`session show <id> --commit-page <1..100>` lists at most 64 local links per page
for a scoped Codex session, with the same unknown-completeness caveat. A corrupt
complete frame or permanently full journal needs operator inspection;
`doctor --repair --reset-commit-link-journal` explicitly replaces it
with an empty journal (Unix mode 0600) and discards its remaining local link recovery
evidence. A readable journal with capacity cannot be reset; run ordinary repair
first. Permission, I/O, unsafe-entry and future-schema errors refuse reset
with a fatal error naming the category; ordinary (non-reset) doctor reports
it as `commit_links.journal_unavailable_reason`.
The reset option runs after available current-page evidence has been replayed
and compacted. A readable full journal also requires a live lease, successful
repair/cleanup and no unfinished batch (64 links or 128 candidates). If repair
frees enough space, reset is skipped and `journal_reset=false`. A corrupt or
oversized journal can be reset without a lease or successful repair after
operator inspection. A readable full journal also needs an empty
`next_commit_page` unless `--acknowledge-commit-link-loss` is given; even a
longer history with no known links on later pages needs that acknowledgement.
If another page/worktree has evidence or the current history scan is incomplete,
inspect it first and add
`--acknowledge-commit-link-loss` to authorize discarding that evidence.
`unmatched_scope_intents` counts current-scope records outside the selected
page or current branch, unreachable commits and hash-kind mismatches.
A partially completed erasure can then be retried with `libra agent erase`.
If post-repair journal
cleanup fails, `commit_links.cleanup_incomplete` is true and the catalog repair
remains visible in the report.
Cloud refs-only metadata does not back up
this local table or a trailer-free link; an opt-in trailer in a reachable commit
can only be used as a recovery clue after repository/session scope checks.

A root commit can select sessions anchored to the unborn parent. The
`agent.commit_trailer` setting affects ordinary `libra commit`, not bridge
`commit.create`, which keeps its separate `agent_bridge_link` graph. The
opt-in trailer exposes its raw session ID to anyone who can read the commit.
A trailer-free link means a link made with `--no-agent-trailer`; cloud sync
currently uploads refs but not this SQLite table. If
a post-ref association warning appears, keep the completed commit, journal and source
session; run `libra agent doctor --repair` in the same worktree to replay an
eligible reachable link. Do not amend solely to retry the link.
An amend that reuses its old message can retain a visible trailer for an erased
or out-of-scope session, but creates no new local link for that session;
`--no-agent-trailer` does not remove inherited or user-written message text.

## JSON Output

Subcommands that support structured output use the global `--json` and
`--machine` envelope. For example:

```bash
libra --json agent status
libra --json agent list
libra --json agent graph <session>
libra --json agent checkpoint list
libra --json agent rpc list
```

`agent list --json` carries a stable `schema_version` plus one row per
supported agent — the first-batch roster `claude-code`, `codex` and
`opencode`. Unsupported agents (`gemini`, `cursor`, `copilot`, `factory-ai`)
stay registered so historical sessions remain readable, but they are omitted
from the listing. Each row carries `slug`, `agent_kind`, `stability`,
`supported`, `support_wave`, `registered`, `transcript_readable`,
`hook_installable`, `installed`, `launchable_review`, `launchable_investigate`,
`external_binary`, `config_paths`, `protected_dirs`, `capabilities`. The row
shape is a frozen contract for automation.
Claude Code advertises `capabilities.transcript_preparer=true`: after Libra
securely opens and pins an authorized transcript descriptor, it may briefly
wait for a trailing JSONL record to finish flushing through that same
descriptor. The wait and tail probe are bounded; the preparer never reopens a
provider path.

Request `agent list --schema-version 2 --json` only when the caller understands
the extension. Its `methods[]` array reports support and current availability
for transcript discovery, historical import, and the OpenCode export bridge;
the default version 1 payload remains shape-compatible and never gains those
fields implicitly. OpenCode reports `transcript_discoverable` unsupported
because batch discovery is unavailable; explicit-ID `importable` and
`export_bridge` availability depend on its trusted offline exporter/sandbox.
On Unix, the exporter subprocess and its descendants run with both soft and
hard `RLIMIT_CORE` set to zero. This suppresses core files for intentional
limit enforcement and unexpected exporter crashes when the system core
handler honors that limit; signal events may still appear in system logs.
Libra retains the exporter exit status and bounded stderr diagnostics. This
child-only setting leaves the parent Libra process and system configuration
unchanged.

The default OpenCode export budget is 30 seconds; the export lease and plugin
forwarding timeout derive a 15-second margin (45 seconds). Run
`libra agent add opencode` again to refresh an older installed plugin. Schema v2
`importable` and `export_bridge` require a trusted exporter and a successful
platform sandbox probe on Unix: Seatbelt on macOS, the existing trusted bwrap probe on
Linux. Windows requires the trusted native exporter and uses the Job Object
boundary described above, without an OS sandbox. Availability reports those prerequisites; it does not prove that OpenCode
boots, that a session exports successfully, or that reasoning fields have a
trusted native origin. Timeout still rejects partial output and preserves the
existing metadata-only fallback; the 16 MiB export cap is unchanged.

On macOS, OpenCode export requires a **Required Seatbelt server/export pair**.
Both processes deny `process-fork`, use private HOME/XDG roots and a pinned
binary copy, and communicate only on one authenticated loopback port.
Cancellation and the absolute deadline terminate their owned process groups;
Libra never signals a group after reaping its leader. Trust, containment,
readiness, output-cap or shutdown failure remains **metadata-only**, with no
unsandboxed fallback. To enable the exporter, register the directory containing
the verified
`opencode` binary and then pin it: `libra agent rpc trust --dir <path>`
followed by `libra agent rpc trust opencode`. Neither step opens the
external-RPC surface (`agent.external_agents.enabled` stays untouched); the
binary is resolved only from registered trusted directories, never `$PATH`.
The sandbox maps only the pinned file, so it must be the native OpenCode
executable. When the trusted directory holds a package-manager launcher —
the `.bin/opencode` shim written by mise's npm backend (aube) or by pnpm —
`rpc trust opencode` pins the native executable that shim declares, provided
that executable's own directory is also registered (the refusal names the
exact `rpc trust --dir` to run); a JavaScript launcher (`#!/usr/bin/env node`)
is refused. A trust record that still pins a launcher reports `export_bridge`
unavailable. Register the native executable's directory if needed, then re-run
`libra agent rpc trust opencode`.
Linux content export additionally requires a trusted `bwrap` that passes
Libra's descriptor-native `--bind-fd` safety probe. Unsupported or legacy
`bwrap` is reported unavailable and fails closed to metadata-only capture;
install or upgrade the system `bwrap` package rather than enabling an
unsandboxed fallback.
Claude/Codex discovery and import are reported unavailable when the platform
cannot provide Libra's secure provider-root file-open primitive.
Unsupported schema versions fail as a usage error (exit 129, category `cli`)
with `LBR-AGENT-017`.

`agent graph --json` emits the frozen capture-graph schema version 1. Its `data`
object contains exactly `schema_version`, `state`, `session`, `turns`, and
`subagents`. Present sessions expose only the session id, agent kind, state,
and timestamps. Indexed turns expose their logical key, derived zero-based
ordinal, coverage schema/completeness/current revision, checkpoint id,
source channel, and append-only revision history. A whole-transcript
checkpoint may appear under several turns; this shared evidence is never
hidden as superseded. Pre-coverage captures are returned as
`coverage_state="unindexed"` checkpoint chronology without invented revision
facts. Subagent nodes expose only checkpoint/link structure and retain
`resolved` or `unresolved` explicitly.

In Libra 0.30.50 and later, graph keeps
separate rows and revision histories for coverage versions 1 and 2, even when
they share a logical turn key or checkpoint. This does not change the public
JSON schema version 1. An unsupported stored coverage version fails the query
with `LBR-AGENT-009`, without returning a successful graph or candidate content.
Use a Libra release that supports the stored version and run `libra agent doctor`
to inspect the catalog; do not rewrite version numbers or delete evidence.
Doctor reports each version's conflict separately, without raw identities or
candidate content, and leaves conflict resolution manual, including with
`--repair`. These coverage-v2 reader guarantees require Libra 0.30.50 or later.

The JSON/machine graph query never opens transcript or object blobs and never
selects working directories, descriptions, metadata JSON, redaction reports,
or coverage digests. **Breaking change (W5-08):** the interactive
capture-graph TUI was removed in the W5 breaking release — its bounded,
redacted content previews (event counts and compact user/assistant message
previews read from the linked checkpoint, at most 256 KiB each) went away with
it, and the frozen JSON v1 schema above never carried them. A locally erased
session succeeds with `state="erased"`, a null session, empty turns, and
unavailable subagents; it is not recreated. An id absent from both the session
catalog and erasure tombstones fails with `LBR-AGENT-021`. Without global
`--json` or `--machine`, `libra agent graph` exits with a usage error and a
migration hint (exit 129, `LBR-CLI-002`) before reading any capture state.

`agent import` has its own schema version 1 result with `results`, `skipped`,
`partial_results`, `failures`, and `next_cursor`. Every import-attempt item has one status:
`imported`, `noop`, `partial`, `skipped`, or `failed`. `results` contains only
fully completed selections; discovered cross-repository or erased candidates
are reported under `skipped` with a hashed session id and stable reason code,
while the same condition for an explicit selector remains a failure. A failed selection that made durable turn progress
is reported under `partial_results` and is never included in `succeeded`. A batch that commits some selections but cannot import all
of them exits non-zero with `LBR-AGENT-018`; the structured error details keep
the successful summaries and a per-selection failure list whose session ids
are hashed, preserve `schema_version`, and preserve a nullable `next_cursor`
instead of coercing `null` to zero. Single-selection ownership, cwd, erased, and source-authorization
failures retain `LBR-AGENT-015`, `016`, `019`, and `020` respectively.
Provider timestamps (and derived turn times) more than five minutes past the
importing host's clock are rejected before they can influence session
chronology: that selection fails with the fixed message `the transcript
contains timestamps beyond the permitted clock skew` (`LBR-AGENT-018`) and
writes no durable state.
Codex batch discovery covers both `sessions/YYYY/MM/DD` and flat
`archived_sessions` under the configured Codex home. `--since` compares the
bounded first `session_meta` record's RFC3339 session timestamp, not the
file's modification time. A session started before the boundary is excluded
even if it was resumed later; Claude discovery still uses file modification
time. For `--since`, a missing timestamp or invalid first record is reported
under `skipped` with a hashed session id and `reason_key` of
`codex_session_time_unknown` or `codex_session_header_invalid`; a header read
failure uses `codex_session_source_unreadable`. Other sessions
can still import. A missing or invalid rollout found again by child-content
discovery may make its parent import partial (`LBR-AGENT-018`), including a bad
archived duplicate that batch discovery did not select. A discovery skip also
records a warning: `--exit-code-on-warning` returns 9 if the batch would
otherwise succeed, and JSON includes `data.warnings[]` alongside each reason.
Cursor pages repeat these hashed skips so a resumed page cannot hide them;
de-duplicate by hashed session id when aggregating page counts.
Inspect the local rollout and retry it with `--session` or `--path`. Each first record
is limited to 1 MiB, with a 256 MiB cumulative header budget and at most 4096
unfilterable records. Reaching either cumulative limit stops discovery with
`LBR-AGENT-018`. The `--since` scan is bounded over the whole local history:
the time window does not reduce header work, because filesystem dates cannot
prove a session's start time. Active and archive trees each have a 20,000-entry
scan budget; an archived `--session` lookup scans at most 10,000 active rollout
files, then at most 20,000 entries in its flat directory. Reaching a scan limit
also returns `LBR-AGENT-018` rather
than silently omitting sessions. For a large history, use a closed Codex SQLite
index backup with `--codex-index-backup` to page absent sessions, or import a
specific rollout with `--path` (including an existing session that needs a
refresh). `--all` does not read headers during
discovery; each selected source is validated by the normal import reader. An
active copy wins over an
archived copy with the same session id; within a root, the newest rollout
filename wins. `--since` validates the header id and bounded CLI-version
field before reading the full transcript. JSON/non-TTY calls require `--yes`;
interactive batch calls ask for consent before the header read. The ordinary
checkpoint export retains authorized reasoning summary data, while import
status, metadata, and errors do not print its text or source path.
Successful and partial summaries report parent turn writes in
`checkpoints_written` and independently discovered child-content writes in
`subagent_checkpoints_written`; a child-only mutation is `imported`, not
`noop`. A platform warning that secure child discovery is unavailable does not
make an otherwise complete parent import fail, because it is not evidence that
child content exists.

Historical import is repository-scoped and fail-closed. Libra requires one
unambiguous transcript `cwd`, resolves its Libra storage, and imports only when
it is the current repository. A sibling linked worktree is valid because it
shares that canonical Libra storage; a different repository is not. File sources must stay under the selected
provider's protected root and are opened once with descriptor-relative
no-follow semantics on Unix. Provider roots are opened component-by-component,
and batch discovery opens each Claude source and every Codex year/month/day
component relative to pinned directory descriptors before consent, so a root,
nested directory, or source-file symlink cannot escape the provider root. Platforms without an
equivalent secure open fail closed. Only typed coverage-v1 user/assistant/tool records are serialized,
after field-level redaction. Raw provider envelopes, provider-home source
paths, and unknown fields are not persisted; the verified repository
`working_dir` remains the documented compatibility exception. Replays are idempotent; an incomplete turn
may advance to one complete revision without changing the checkpoint's
structural repository parent. If a different complete payload later claims
the same logical turn, the claim is parked as `conflicted` and Libra retains
exactly the first challenger in `agent_coverage_conflict`: its typed canonical
payload is redacted before persistence and stored with its digest, source
channel, observation time, and deterministic redaction report. Later
challengers do not replace that first evidence; raw provider envelopes and
secret-shaped matched bytes are never stored. The incumbent revision remains
append-only and current until an operator resolves the conflict. Local session erasure writes a durable
anti-resurrection tombstone before deleting the catalog; automatic discovery
and in-flight writers cannot rebuild it. `--restore-erased --yes` is the only
local bypass and is audited.

New V2 imports retain a source only as the repository-keyed, domain-separated
`source/hmac-v2/<64 lower-hex>` commitment. New V2 subagent content records
use their separately domain-separated
`source/subagent-hmac-v2/<64-lower-hex>` commitment. A raw source locator or
an unkeyed source SHA-256 never enters import or subagent catalog metadata,
claims, markers, diagnostics, or cloud records. The optional V2 snapshot
digest is a distinct-domain, repository-keyed
`source/hmac-v2/<64 lower-hex>` commitment over the redacted snapshot content;
the helper's transient SHA-256 is never durable. A bare or tagged unkeyed
SHA-256 is immutable legacy evidence only and cannot be introduced into V2 or
cloud state. V1 rows are readable proof, not a conversion shortcut: Libra
migrates them atomically only after the exact scoped proof matches and the
identity, catalog, and any repair marker are committed and quiescent. Active,
partial, or repair-pending V1 state remains V1 and never mints a parallel V2
record.

Before the first content read/export, interactive confirmation identifies the
selected agent scope, current-repository-only boundary, candidate count/limit,
redaction write, and the fact that a later `libra agent push` may upload the
redacted traces. `--yes` acknowledges only that privacy disclosure; it does
not relax source-root, repository, size, deadline, or platform checks. Import
batch processing is best-effort across sessions and reports exact durable
per-session progress when a later turn fails. The 64 MiB batch budget is
charged from bytes actually read through the held descriptor for successful,
malformed, unauthorized, and oversized candidates alike, including file
growth after authorization. If bounded child discovery fails before it can
return a trustworthy byte count, Libra conservatively charges that child's
entire remaining per-source allowance; retries therefore cannot turn a helper
failure into a batch-cap bypass. The 120-second absolute deadline starts before
discovery and is checked before and during helper phases, traversal, parsing,
reservation, object building, and CAS persistence. Libra does not cancel an
SQLite commit after it starts:
it checks the deadline immediately before each commit, then observes that
commit to a definite outcome. A fully committed final turn is therefore
reported as success even if the clock crosses the deadline while the commit
result is being observed; every uncommitted lease/marker is abandoned on a
deadline failure. If that recovery transaction itself fails, the command
chains the cleanup error and an actionable `agent doctor --repair` hint instead
of reporting only the original failure.
Discovery and authorized-file read helpers are private, kill-on-timeout
processes. The reader consumes the exact descriptor already opened by the
parent; its control wire has fixed safe bounds and commitments, never a
locator, path, provider session value, or raw bytes. Those helper phases share
the absolute deadline. The current command process still classifies,
canonicalizes, securely opens, and rewinds a source synchronously before that
handoff. A stalled NFS/FUSE operation there cannot be forcibly cancelled, so
the 120-second setting is not a strict end-to-end wall-clock deadline. Child
discovery receives a shorter sub-deadline that reserves time for the
independently valid parent checkpoint. Parent-side helper response decoding and
completeness validation recheck that sub-deadline while walking the bounded
response, so a late large child result degrades the parent to partial evidence
instead of suppressing its checkpoint.
After consent, the held-descriptor preparation phase performs parsing and
redaction in its killable helper and cannot reopen a source locator. Source-root
authorization and the initial secure open occur before that handoff. Checkpoint
loose-object writes and the commit/tree reads used while splicing the traces ref
retain their own bounded paths. This preserves descriptor-bound raw reads, but
does not yet provide a strict deadline, FD-only wire, and autonomous replay
after timeout in one per-command/per-hook process; that combination requires a
long-lived owner or a provider ABI that supplies a pre-authorized descriptor,
as recorded in the [Session Capture plan](../development/plan/plan-20260924.md).
Object reads accept only the requested `commit`/`tree` type, verify the full OID
and declared length, and enforce a 16 MiB inflated-payload cap before
allocation, so a hostile compressed object cannot turn bounded helper work into
unbounded memory use.

Before advancing to the next batch candidate, import waits until the completed
candidate's queued object-index writes have reached SQLite. If that drain
exhausts the command deadline, the completed candidate is reported as partial;
the not-yet-started next candidate is never charged with that progress.
Terminal background update errors are barrier failures too, not log-only
successes. Import acquires a session-wide durable repair barrier before
checkpoint writes. Its owner, generation, and lease serialize concurrent
processes; only that exact generation may retire it, and an index failure may
downgrade only the exact import identity/fence that produced the result.
Timeout or update failure marks the barrier repair-pending and the exact
identity partial. A process crash leaves its active generation until the lease
expires; takeover then repairs before writing. Replay runs an idempotent, killable foreground repair of
the marked session's complete E4 object set before it may return `noop`. The
helper holds the SQLite writer slot while it revalidates the session and erase
tombstone, updates the index, and preserves the owned barrier, so repair cannot
reinsert cloud-eligible rows after concurrent erasure. A successful candidate
then retires only its own generation. If bounded repair cannot complete, run
`libra agent doctor --repair` and retry.

Each potentially new OID is recorded as a provisional preclaim in the durable
attempt marker before its loose-object write. It becomes deletion-eligible
ownership only after this writer wins the no-clobber publish and durably moves
the OID into the marker's `created_oids` set. A crash in between is deliberately
leak-safe: an unresolved preclaim is never deleted. Loose objects are compressed into a unique
file in the shared private `objects/info/libra-tmp` directory and promoted without overwrite; an
already-present final path is decompressed and byte-validated before reuse.
A bounded scavenger examines at most 64 entries in that private directory and
removes only regular files older than 24 hours whose names exactly match
`.<40-or-64-lowercase-hex-oid>.tmp-<decimal-pid>-<uuid>`; unrelated files and directories are
retained. File and directory fsyncs are performed only with `--sync-data` or
`LIBRA_SYNC_DATA`; no-clobber atomic publication is always enforced.
Before deleting unreachable `object_index` rows, `agent clean` acquires the
repository-wide repair-marker generation fence, revalidates every candidate
OID, and holds the fence through the prune transaction. A marker published
after the earlier command preflight therefore makes cleanup fail closed instead
of allowing its delayed queue update to resurrect a deleted row. With
`--sync-data`, retiring a repair marker also fsyncs its containing directory.
Append, failure finalization, and erase do not run a repository-wide
reachability drain. A rejected append durably marks its exact generation
`cleanup_pending`; `agent doctor --repair` performs bounded root-fenced
ownership retirement immediately without
waiting for the ordinary writer TTL. A same-session cleanup job makes erase
refuse until that maintenance retires the job; erase never runs the drain
itself. Inline recovery never
unlinks a shared loose object or deletes its `object_index` row: worktree-index
writers do not share the SQLite lock, so physical reclamation is left to
repository GC and its grace/locking/reachability policy. Doctor snapshots refs,
reflogs, every registered worktree index, active sequencer state, and marker/catalog
state outside the SQLite writer transaction, then revalidates the complete snapshot
under the final ownership-retirement transaction. It does not traverse unrelated
object history because this path performs no payload or object-index deletion.
Every marker registration also carries a random writer `generation`; object
preclaims, ownership finalization, the final ref CAS, and marker cleanup all
compare that generation so an expired writer cannot adopt or delete a
same-checkpoint takeover marker.
Root collection is limited to 250,000 reference/reflog rows. Registered index reads
run in a killable helper under a 30-second aggregate deadline;
registry/index files are opened no-follow/nonblocking, required to be regular,
read once from the held descriptor with a `limit + 1` growth check, and parsed/checksummed
from those exact bytes. They are limited to 256 files and 64 MiB aggregate. Hitting any bound is a
fail-closed deferral: candidate ownership remains durable for diagnosis and a
later retry; a completed drain retires attempt ownership while leaving orphaned
payload reclamation to repository GC.
Zero-progress provisional sessions are reaped from their persisted
`import_provisional` flag after lease takeover, and live/export failures after
marker registration release their claims/job lease and clear only ordinary
markers (never a `cleanup_pending` job).

`agent clean --gc` physically deletes terminal, ownerless import identities
after their final coverage state is pruned, including zero-checkpoint identity
rows. It does not reset them to a replayable `discovered` state. Dry-run uses a
rolled-back coverage simulation so `import_identities_pruned` matches the
identical real GC run without mutating catalog state.
Conflict evidence follows its claim rather than becoming an independent
retention root. Erasing a session or pruning its final coverage claim cascades
the retained challenger away; dry-run simulates the same deletion. When a
checkpoint-history rebuild/prune rewinds the current claim to an older
surviving revision, stale challenger evidence is deleted and the claim returns
to its non-conflicted committed state.

The tombstone is propagated. `libra cloud sync` publishes it to D1 under the
generation fence and removes the erased session's mirrored catalog rows, and
`libra cloud restore` is tombstone-first in BOTH directions: it filters rows
carrying a remote tombstone AND rows matching this repository's own
`agent_import_tombstone`, inside the restore transaction. Erasing locally and
then restoring from a mirror that has not yet seen the tombstone therefore
does not bring the session back. `cloud sync` freezes the capture catalog with
the rest of its snapshot before any remote write and publishes that frozen copy
only if the current catalog still matches it after the object upload, so a
session erased, a checkpoint pruned or any catalog row changed while a sync is
running is never published from the older snapshot: the agent-capture phase is
refused with a retry hint and writes no remote catalog rows (see
[`cloud`](cloud.md)).

What is still deferred is R2 physical payload deletion: the erased content's
objects remain in R2, so another machine that has never seen the tombstone can
still fetch them. Treat `agent erase` as irreversible for THIS repository, not
as a cross-machine guarantee that every copy is gone.

Private capture recovery evidence is repository-only. Local erasure first
commits the anti-resurrection tombstone, then prunes checkpoint history, and
finally deletes attributable recovery headers/chunks and private session
aliases with the catalog row in one transaction. Deletion uses local
catalog-PK/incarnation ownership, not a recovery MAC or an evictable receipt:
a lost key does not prevent attributable erasure. Foreign or ambiguous corrupt
evidence is retained, without exposing its contents; a lost-capacity diagnostic
is logged after the deletion commits. Such retained evidence still consumes
capacity and may prevent new capture associations. Without a consistent
original repository DB backup, that capacity is lost: doctor/retry is not a
force-discard facility. A DB-copy backup contains sensitive alias associations
and is not anonymized; restoring an old full DB backup can undo local deletion
and its tombstone. Normal receipt completion removes the matching artifact and
only an alias with no remaining reference, in the same transaction.

If pending and quarantine headers collide on one checkpoint key, cleanup
removes only headers attributed to this session. Unknown/foreign headers and
their shared chunks are retained unchanged; explicit erase reports retained
capacity and does not certify that every private copy has been removed.

An unreadable session incarnation, or known aliases from a different
incarnation of that PK, refuses the final deletion: restore consistent
session/alias metadata and retry; the first-phase tombstone remains in effect.
One retained undecodable header blocks every new or re-delivered recovery
artifact and destructive GC/root-walking maintenance for the repository, not
just its own slot. Completion may retain aliases while that header prevents
proving them unreferenced; explicit attributable erasure is still possible.
Out-of-band over-capacity states read at most 17 headers and 17 associations:
unexamined evidence, possibly this session's own content, may remain. The
warning is not proof that every private copy was deleted.

`agent session list --json` and `agent checkpoint list --json` return one
page per call: `data` carries a `schema_version`, the rows under `sessions`
/ `checkpoints` (per-row shape unchanged), and `next_cursor` — an opaque
token to pass back via `--cursor`, `null` once the listing is exhausted.
Pages are ordered newest-first (`started_at` / `created_at` descending,
with the row id as tiebreaker).

The human `agent session list` table renders `started_at` as a relative age
against the current machine clock (for example, `2 hours ago`). JSON output
keeps the raw Unix timestamp for automation.

`agent checkpoint show <id>` is intentionally not a metadata dump. Its default
human and JSON output contains only the fixed checkpoint summary:
`checkpoint_id`, the closed `scope` vocabulary, Unix `created_at`, and whether
a parent snapshot was recorded. It never reads or renders `metadata.json`, a session identifier,
source locator or commitment, redaction detail, or catalog object identifier. Use the explicit checkpoint export path for transcript
content, which applies its own authorization and redaction policy; do not
depend on default `show` output for internal metadata.

Each checkpoint row carries a `scope`. `committed` checkpoints are written at
turn/session boundaries (`Stop` / `SessionEnd`) and carry the redacted
transcript snapshot. There are two deliberately distinct `subagent` evidence
types. Codex `SubagentStart` / `SubagentEnd` hooks create empty-transcript
**boundary** checkpoints linked structurally through `parent_checkpoint_id`.
Claude `<session>/subagents/*.jsonl` files create independent **content**
checkpoints whose `parent_checkpoint_id` remains null. New content claims use a
repository-keyed, domain-separated HMAC V2 commitment
(`source/subagent-hmac-v2/<64-lower-hex>`) derived only after the source is
securely opened and its root, storage binding, scope, and workspace fence have
been revalidated. Append-only revisions select one current content leaf without
persisting the local project slug or filename and without rewriting physical
history. Historical V1 `source/sha256/...` claims, revisions, and checkpoint
metadata remain immutable, read-only evidence; a matching legacy claim is not
silently shadowed by a new V2 row and must stay on its recovery/doctor path.
Claude does not emit a stable boundary identifier,
so its content normally remains `link_state=unresolved`; `agent doctor` reports
that fact and never guesses a match. A provider-stable identifier links content
to a unique boundary through a separate association row, without changing the
checkpoint's immutable traces commit. Both evidence types remain listable,
showable, exportable, prunable, and doctor-visible. An empty or metadata-only
child file has no normalized turn evidence and is therefore recorded as
`partial`, never as a complete child transcript.

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
These source-build changes remain under local acceptance and are not a
released-runtime guarantee.

The current Codex capture installer registers these lifecycle events: `SessionStart`,
`SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
`PostToolUse`, `PreCompact`, `PostCompact`, `Stop`, `Interrupt`, `SubagentStart`, and
`SubagentStop`. The checkpoint event projection preserves provider correlation
fields such as `turn_id`, `tool_use_id`, `agent_id`, `agent_type`, compaction
trigger, permission mode, tool input, and tool response. The hook event
projection does not fetch rollout reasoning. Consented historical import uses
the ordinary retention contract above; it does not require a decryption key to
preserve an opaque value.

Current source builds use the Codex 0.162.1 twelve-event contract, including `Interrupt`. Re-run `libra agent enable --agent codex` to refresh only Libra-owned handlers and trust entries. `Interrupt` and `SessionEnd` use a 3-second host timeout and a 2000ms capture budget; other default handlers use 30 seconds and 29000ms. `Interrupt` shares the installed `stop` command but remains `turn_interrupted`: it preserves the current session phase and writes no normal completed-turn checkpoint. `PostCompact` shares `compaction` while retaining `compaction_completed`. Existing event identity ordinals and user trust/configuration remain unchanged. This event support does not prove rollout thinking has been captured.

Durable checkpoint metadata keeps only fields Libra can classify safely. Live
hook capture drops the provider-supplied `model`, `source`, `tool_name`, and
session-reference (transcript pointer) fields before anything is persisted, so
consumers of `checkpoint export` output or `refs/libra/traces` sidecars must not
expect them. In a hook-captured checkpoint, `metadata.json` always records
`"model": "unknown"`, the `events/lifecycle.jsonl` lines never carry `model` or
`tool_name` and record `"source": null`, and Codex subagent boundary metadata
records a null `subagent.tool` / `subagent.source`. A hook-captured lifecycle
line's `provenance.hook_event_name` is the canonical snake_case lifecycle kind
(for example `tool_use` or `turn_end`, matching `kind`), not the provider's
native event name. `events/lifecycle.jsonl` is schema v2: every line carries a
mandatory `identity_scheme` (`native_replay_hmac_v2`,
`fallback_action_hmac_v1`, `generic_lifecycle_uuid_v5`, or `import_uuid_v5`)
that declares how its `event_id` was derived; it is a declaration, not a
verifiable replay credential.

Cloud mirroring publishes the capture catalog as dependency-ordered batches
(`session → checkpoint → revision → link → claim`) under a token-fenced
generation. Session, checkpoint, link, and mutable claim projections have independent
monotonic sync generations, so pruning a current child leaf cannot create an
equal-generation conflict and pruning a retained traces chain cannot be undone
by a stale clone. An explicit `--restore-erased` import starts a new durable
replication incarnation for both the session and opaque child-source namespace;
this avoids key reuse while only R2 physical payload deletion remains deferred; D1 tombstone propagation is already active. The generation also binds a canonical digest of the
remote object index; sync rechecks every checkpoint object in D1 and R2, and
restore accepts the catalog only when the manifest and object-index digest are
unchanged before and after the read. Current clients use versioned v2 remote
session/checkpoint tables. Once v2 is activated, D1 triggers reject legacy
unfenced writers with an upgrade error rather than allowing them to invalidate
a coherent restore snapshot.

## Claude Code hook event registry

Libra keeps one Claude Code hook-event registry. A name is safely installable when empty stdout and exit 0 do not take over the host action. `WorktreeCreate`, `WorktreeRemove`, and `PreModelSwitch` are control-sensitive and are not installed. `SessionStop`, `ModelUpdate`, and `Compaction` are deprecated aliases and are not in the current installable set. An unknown event name or an unknown field is partial, and the diagnostic does not include the payload. `libra agent enable` writes every safely installable event. `SessionEnd` has no timeout. `FileChanged` has no matcher and does not emit `watchPaths`. `libra agent doctor` lists each known event as installable or excluded, and whether a Libra-owned entry is configured. `libra agent hooks claude-code observe` accepts one JSON hook frame on stdin, records safely installable events through the existing capture coordinator, and prints nothing. Controllable success and failure both exit 0. `WorktreeCreate`, `WorktreeRemove`, and `PreModelSwitch` are not installed and produce no path or decision. MessageDisplay keeps at most 128 batches per session across hook processes. Later batches write no capture revision and only raise a dropped count, which session erase removes. Requires Libra 0.30.66 or later. This does not claim that every known event is installed.

An authorized Claude Code transcript keeps a non-empty `thinking` string in the ordinary snapshot and in the coverage projection. `thinking.signature` and `redacted_thinking.data` stay in the ordinary transcript bytes and are labeled Unproven when they are not native proof. An empty thinking string is not a missing key, and an empty shell is not a successful read. The coverage schema version stays 1 for turns without reasoning and 2 when a readable thinking record is present. Session metadata and redaction reports do not include the thinking text. `libra agent checkpoint show --json` and the ordinary checkpoint export return that thinking text and `thinking.signature` for an authorized Claude checkpoint, labeled unproven. `checkpoint list`, `session list`, and `agent graph` do not print the body. Live capture, import, and snapshot recovery read those attached fields from the original JSONL, with provider, session, turn, record, and content index. A duplicate key rejects that record and leaves retained bytes in place. A mismatched session file is not associated. Ordinary retention does not create a native artifact. An explicit `libra agent doctor --repair` appends one coverage revision for an authorized legacy Claude checkpoint. The revision key binds that checkpoint and the ordinary transcript content. Repeating the repair with the same content leaves the revision and the stored objects in place. Erase and the cloud mirror keep that replacement together with the old checkpoint reference, and a tombstone blocks revival.

## Examples

```bash
# Show captured-session counts and recent checkpoint summary
libra agent status

# Show the agent capability matrix (supported roster, hooks, install state)
libra agent list

# Negotiate the versioned import/export method matrix
libra agent list --schema-version 2 --json

# Import one historical Claude Code session after explicit privacy consent
libra agent import --session <provider-session-id> --agent claude-code --yes

# Import a bounded page of active or archived Codex sessions by session_meta time
libra agent import --since 2026-07-01T00:00:00Z --agent codex --limit 20 --yes --json

# Read one captured session's turn/revision/subagent structure as frozen JSON v1
libra --json agent graph <session-id>

# Read the same graph safely in automation or from another repository
libra --json agent graph <session-id> --repo /path/to/repo

# Enable Claude Code capture and install its hooks (alias of enable)
libra agent add claude-code

# Enable Claude Code capture and install its hooks
libra agent enable --agent claude

# Install Claude Code hooks with a 60-second host timeout (capture budget 59000ms)
libra agent enable --agent claude --hook-timeout 60

# Enable every supported agent at once
libra agent enable

# Disable Claude Code capture and uninstall its hooks (alias of disable)
libra agent remove claude-code

# Remove legacy gemini hooks (uninstall-only channel; idempotent)
libra agent remove gemini

# Disable Claude Code capture and uninstall its hooks
libra agent disable --agent claude

# List captured sessions
libra agent session list

# Show a session and copy its currently verified Claude Code transcript source
libra agent session show <session-id> --extract-transcript /tmp/session.jsonl

# Stop a captured session
libra agent session stop <session-id>

# Resume a stopped captured session
libra agent session resume <session-id>

# List captured checkpoints
libra agent checkpoint list

# Page through checkpoints (default 50 per page; JSON carries next_cursor)
libra agent checkpoint list --limit 100
libra agent checkpoint list --cursor <next_cursor>

# Show a checkpoint's safe structural summary
libra agent checkpoint show <id>

# Replay a checkpoint as a JSONL transcript
libra agent checkpoint rewind <id>

# Drop temporary checkpoints from the most recent stopped session
libra agent clean

# Drop temporary checkpoints from every stopped session
libra agent clean --all

# Diagnose hook installation and capture state
libra agent doctor

# Push refs/libra/traces to the default remote
libra agent push

# Push refs/libra/traces to a named remote
libra agent push --remote origin

# Re-push after `libra agent clean` rewrote the traces chain (force-with-lease)
libra agent push --force-rewrite

# Discover libra-agent-<name> RPC binaries on PATH
libra agent rpc list

# Invoke a single JSON-RPC method on a libra-agent-<slug> binary
libra agent rpc invoke <slug> <method> --params '<json>'

# Run the DeepSeek Harness bridge over stdio (JSON-RPC 2.0 NDJSON);
# feed it a fixture and read one frame per response on stdout
libra agent bridge --stdio < bridge-initialize.ndjson

# Serve legacy MCP on stdio. stdout carries only JSON-RPC.
libra agent mcp --stdio

# Structured JSON envelope for agents
libra agent --json status
```

The same banner is rendered by `libra agent --help` so the doc and the
CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout, see
`docs/development/commands/_general.md` item B).

## Deferred parity (non-goals)

The following external-agent parity surfaces are intentionally **not** exposed
in this wave. They are recorded — with their handling and restart condition —
in the Agent tracing contract
([`../development/tracing/agent.md`](../development/tracing/agent.md), section
「还未实现的功能」), and are called out here so scripts and users do not depend
on them:

- **`agent add`/`remove` `--local-dev` / `--force` flags** are unpublished — use
  the canonical `enable` / `disable` (and their `add` / `remove` aliases) only.
  If they ever ship, each will be wired onto both the canonical verb and its
  alias.
- **Provider-specific transcript compaction / reassemble traits** are a future
  parity item. The writer already stores large transcripts as manifest-relative
  chunks, but there is no provider-specific compactor/reassembler yet.
- **Optional capability traits** (`ProtectedFilesProvider`, `TranscriptCompactor`,
  `HookResponseWriter`, `RestoredSessionPathResolver`, …) beyond the landed
  `DeclaredAgentCaps` matrix have no public behavior yet.
- **External-RPC method families beyond the v2 `info` / capability gate** are not
  implemented; an agent that does not declare a capability continues to fail
  closed.
- **The non-first-batch roster is unsupported.** Only `claude-code`, `codex` and
  `opencode` are supported, hook-installable and launchable for
  review/investigate, plus `grok` as described above. `gemini` (uninstall-only, see the Description above),
  `cursor`, `copilot`, and `factory-ai` are `supported=false`, are omitted from
  `agent list`, and `add`/`enable` returns an actionable unsupported error;
  they are not launchable. `hermes` is supported for local capture, as described above, and is not launchable. `pi` is a registered identity with `supported=false`, as described above: the repository CHECK accepts it, but nothing writes a `pi` session yet.

## Notes

- External `libra-agent-*` agents are **disabled by default**. Opt in with
  `libra config set agent.external_agents.enabled true` (repo-local); until
  then `rpc list`/`libra-agent-*` `rpc trust`/`rpc invoke` refuse with
  `LBR-AGENT-002` (`rpc untrust` stays available — revoking trust only
  tightens security; `rpc trust --dir <path>` and provider-exporter trust,
  e.g. `rpc trust opencode`, are preparation-only — they never scan `$PATH`
  and arm nothing by themselves, so they work while gated).
  Discovered binaries stay quarantined until `rpc trust <slug>` records
  their provenance (trust is refused for a binary in a world-writable
  directory), every invoke revalidates it (drift revokes trust,
  `LBR-AGENT-005`), the child environment is cleared to an allowlist, and
  stderr is captured/capped/redacted — never inherited. Invoke timeouts,
  broken pipes and malformed frames map to `LBR-AGENT-012`; IO hard-cap
  violations map to `LBR-AGENT-007`.


- Hook configs that `libra agent enable` installs for Claude Code and Codex
  invoke the top-level `libra hooks claude|codex <verb>` commands with an installer-owned bounded
  `--capture-budget-ms` argument derived from the handler timeout (default
  Claude and Codex ordinary events `30s → 29000`; Claude `SessionEnd` has no
  host timeout and keeps `9000`; Codex
  `SessionEnd` and `Interrupt` `3s → 2000` because that host event has a three-second
  maximum; a one-second legacy handler keeps a usable `500ms` capture slice
  and up to 1000ms remains for terminal finalizer cleanup). Choose another
  handler timeout with `libra agent enable --hook-timeout <seconds>`; hooks
  written by the previous Claude default (`10s → 9000`) or another timeout
  stay Libra-owned and are replaced in place. Re-running enable refreshes older configs; the hidden
  alias accepts the same argument for legacy installed commands. The installed
  OpenCode plugin (`.opencode/plugin/libra-hooks.js`) instead forwards every
  event through the hidden `libra agent hooks opencode <verb>` entry, without an
  installer budget; its capture deadline is the OpenCode export deadline. Claude and the hidden alias reject a hook
  envelope that fails size / UTF-8 / JSON / schema / reported-working-directory
  / transcript-path validation with `LBR-AGENT-008` (exit 128), and never echo
  raw stdin. Both path-like fields are capped at 4096 bytes before storage
  resolution. The installed
  Codex surface intentionally acknowledges malformed/unbound callbacks and
  advisory nonterminal capture failures to avoid failing the host task. A
  well-formed callback invoked outside any Libra repository has no scope to
  bind: installed Codex hooks acknowledge it (exit `0`, including
  `SessionEnd`), while Claude and the hidden alias return the fixed,
  path-free repository-not-found error `LBR-REPO-001` (exit 128). A damaged
  active repository is not "outside" a repository: Claude, the hidden alias
  (Claude Code / OpenCode / Codex), the gemini entries and an installed Codex
  `SessionEnd` (which still exits non-zero) report the same
  path-free `LBR-REPO-003` (unresolvable linked-worktree storage, with the
  `libra worktree repair --confirm --path <worktree-path>` remedy), `LBR-REPO-002`
  (missing database or unsupported object format) or `LBR-IO-001`
  (unopenable database, including one written by a newer Libra, or
  unreadable object format) as every other repository command, never the
  generic `LBR-INTERNAL-001` capture failure (see `libra hooks`). Like every
  other repository command, these entries apply pending repository schema
  migrations when they open the database. A validated, scope-bound `SessionEnd`, however, must persist or observe a
  durable terminal completion/pending receipt; a bounded database/finalizer
  failure before that evidence exists is non-zero rather than silently
  acknowledged. After an existing catalog reservation, narrow nonterminal
  checkpoint or maintenance-lock failures may persist only a non-sensitive
  retryable catalog diagnostic. The budget bounds killable-helper, database,
  and other deadline-aware stages; it is not a strict whole-process host
  deadline because source classification, canonicalization, secure open, and
  rewind currently happen synchronously before descriptor handoff. A stalled
  NFS/FUSE operation therefore cannot be forcibly cancelled, and this path does
  not promise FD-only autonomous replay after timeout. A checkpoint operation
  (e.g. `checkpoint rewind`) on an inconsistent
  store — a catalog row whose `parent_commit` is malformed or points at a
  missing traces object — fails with `LBR-AGENT-009` (exit 128); run `libra
  agent doctor` to inspect the store.
- Managed Session Capture supports Unix and native Windows secure repository-private
  deduplication-key initialization. On other platforms it fails closed
  before creating key files, with an actionable diagnostic, rather than using
  a weaker path-based publication fallback. The fixed capability diagnostic is
  explicit for both Codex entry points: nonterminal `hooks codex` callbacks
  (including the hidden legacy alias) print a path-free platform remedy to
  stderr but exit `0`; `SessionEnd` returns non-zero with that same remedy.
  Unsupported non-Windows platforms use the Unix-host remedy; a native Windows
  initialization failure points to the matching `libra.exe` helper and local storage.
  The fail-closed installed Claude surface and the hidden `agent hooks`
  Claude Code / OpenCode alias return non-zero for every event with that same
  remedy and `LBR-UNSUPPORTED-001`. This exception does not change the alias's fail-closed malformed-envelope
  contract. It applies only to callbacks invoked inside a Libra repository:
  a callback outside any Libra repository follows the outside-repository
  contract above on every platform, Unix or not.
- `checkpoint rewind --apply` restores working-tree files only; the agent's own
  transcript file is not rewritten.
- Hook and capture diagnostics are best-effort and are designed to report
  actionable installation state rather than silently ignoring missing providers.

### Doctor checkpoint-store repair (`--repair`)

`libra agent doctor` scans the checkpoint store and writer-marker registry
(AG-20 repair matrix); without `--repair` it is strictly read-only
and reports what `--repair` would do:


OpenCode hook status now includes `provider_hooks[].events[]` in JSON. Each
entry has an `event` and the same nullable `installed` status as the managed
plugin: `true` means the exact current template is installed, `false` means it
is missing/outdated, and `null` means inspection was unavailable. Human output
lists these mappings as `(managed template)`. The native bus, merged inbox and
accepted compatibility mappings come from the existing event registry;
`session.error` is skipped. This describes installed mappings, not proof that
OpenCode ran or emitted each event. Reinstall an outdated managed template with
`libra agent add opencode` before starting a new session.

Before opening plugin files, doctor checks `.opencode`, both `plugin`/`plugins`
parents and both managed leaves with no-follow metadata checks. A symlink,
FIFO/socket/device, file parent or directory leaf produces a fixed slot/type
reason and unknown installation status. It does not open or modify that node,
print its contents or target, or mark it installed, including with `--repair`.
These checks retain the existing stable-directory-name assumption; they do not
resolve the deferred concurrent directory-relocation problem.

`checkpoint_store.opencode_legacy_reasoning_checkpoints` lists canonical IDs
whose bound local OpenCode metadata has no non-null `reasoning_state` field.
Human output marks them `legacy/unrecorded reasoning state; not evidence of
complete capture or native origin`. This is an absence diagnostic, not a date
or schema classification: current checkpoints can also lack that field, and
its presence alone proves neither valid reasoning nor native provenance.
Existing coverage completeness is not promoted or rewritten. The scan examines
at most 64 OpenCode catalog rows and 64 KiB of validated metadata per row
(4 MiB worst-case payload). Unavailable, oversized/unbound metadata or excess
rows produce a content-free incomplete-scan note; an empty list does not
certify complete capture. It reads no transcript or reasoning-artifact bodies.
The marker is read-time only, unchanged by `--repair`; removing this diagnostic
requires no data migration. These diagnostics first ship in the release
containing OG-06; the source change alone is not a release announcement.


| `inconsistency_type` | Meaning | `--repair` action |
|----------------------|---------|-------------------|
| `stale_catalog_row` | An `agent_checkpoint` row's `traces_commit`/`tree_oid`/`metadata_blob_oid` disagree with the checkpoint still reachable from `refs/libra/traces` | Rebuild the row's OID columns from the ref (idempotent UPDATE) |
| `missing_objects` | Checkpoint objects genuinely missing from the store (and the ref cannot rebuild them) — the best-effort object/manifest enumeration covers the E4 protocol tree: `manifest.json`, `events/lifecycle.jsonl`, `transcript/<agent_kind>.jsonl` incl. chunks, `redaction_report.json`, `content_hash.txt`, and the intermediate trees. It descends only the protocol-owned `events`/`transcript` subtrees and bounds tree entries, total objects, and manifest declarations; a cap hit is `manual_required`, never partial repair. It does not parse or verify lifecycle-line schemas, UUID derivations, or `identity_scheme` values. | None — reported `manual_required`; doctor never takes destructive action (try `libra fsck --heal` or a cloud/backup restore) |
| `missing_catalog_row` | A checkpoint reachable from `refs/libra/traces` has no catalog row (crash window B) | Re-INSERT the row via the writer's probe-first idempotent path, reconstructed from the commit's `metadata.json` (v1 and v2 shapes) |
| `missing_object_index` | Checkpoint objects missing from `object_index` (invisible to `libra cloud sync`) — covers the traces commit plus the bounded E4 protocol object set | Idempotent re-insert with the writer's row semantics (trees as `tree`, transcript blobs as `agent_transcript`, sidecars as `blob`). Before repairing a size, doctor streams and integrity-checks the held local loose object under the transcript cap; it never adopts manifest `byte_len` or renders the payload. |
| `expired_inflight_marker` | A valid traces writer marker outlived its TTL, including provisional preclaims and proven-created loose-object OIDs | Fence the expired writer in the final ref transaction, run the serialized repository-root proof, and retire the marker; inline recovery never unlinks shared payloads or removes `object_index` rows, leaving physical reclamation to repository GC |
| `invalid_inflight_marker` | Marker JSON, row identity, commit, or OID is malformed | None — reported `manual_required`; automatic deletion is unsafe because ownership cannot be decoded |
| `conflicted_coverage_claim` | Two different complete payloads claimed the same logical turn; doctor reports only a per-report opaque ordinal, bounded schema/revision/completeness facts, and that redacted challenger evidence was retained | None — reported `manual_required`; inspect the durable sanitized candidates and choose an explicit recovery rather than silently discarding provenance. `--repair` never chooses a winner |
| `inconsistent_subagent_content` | A current subagent content claim is missing or disagrees with its immutable revision, checkpoint catalog row, or association link | None — reported `manual_required`; unchanged replay fails closed until the companion relation and checkpoint objects/ref reachability are restored |
| `unresolved_subagent_link` | A current subagent content checkpoint has no unique provider-stable boundary match (the normal Claude case) | None — reported `manual_required`; content and boundary evidence stay independently preserved and doctor never guesses an association |

Additional rules:

- Repository and database failures keep the shared repository contract
  (exit 128). Outside a repository doctor fails with `LBR-REPO-001` and the
  same `libra init` / Git-conversion hints as other repository commands; a
  detached, migrating, or corrupt linked worktree fails with `LBR-REPO-003`
  and its verbatim worktree remedy. Doctor opens the repository database
  itself: a missing database is `LBR-REPO-002`, a database that cannot be
  opened (including one written by a newer Libra) is `LBR-IO-001`, and an
  unsupported `core.objectformat` is `LBR-REPO-002`. These database messages
  omit the database path and the stored object-format value.
- Pending terminal-finalizer diagnostics use read-only **keyset pagination**:
  32 sessions per page, at most 512 sessions and 128 receipt findings per run;
  receipt metadata is capped at 1 MiB before decoding. Malformed rows are
  reported through a content-free note while later valid rows remain visible.
  A skipped malformed key or reached limit means the report is incomplete,
  not that unlisted receipts are healthy or repaired.
- A receipt without a durable artifact is **`pending_source`** evidence;
  doctor cannot reopen provider sources or manufacture a replay snapshot.
  After a later stop/resume, superseded receipts remain `manual_required`,
  as do session-quarantined receipts. Even `--repair` leaves the current
  session and original receipt unchanged; quarantine is not successful capture.
  For an artifact-bound receipt that exhausts its original retry budget,
  quarantine routing moves only the private artifact header and preserves the
  original receipt revision, status, counters, generation and source fence.
  An eligible, authenticated artifact may be replayed locally only after its
  alias, receipt, marker and coverage fences are revalidated. One
  `doctor --repair` invocation counts at most five replay attempts. Automatic
  replays share one 2-second cooperative deadline, while each audited manual
  attempt gets its own fresh 2-second deadline; findings deferred by the cap
  or by a retry-later outcome are not counted, remain unrepaired, and
  rerunning `doctor --repair` continues the bounded queue.
  A finding is repaired only after the checkpoint write is durable and the
  original terminal receipt completes. An expired artifact gets one audited
  manual replay attempt after quarantine; failed or stale-fence attempts stay
  visible for manual recovery. A permanently refused automatic replay parks
  only the artifact header in quarantine; doctor does not retry that parked
  header or spend replay budget on it before the original retry/window limit
  permits an explicit audited repair.
- **Legacy-v1 checkpoints** (pre-AG-20 layout without `manifest.json`) are
  counted in `legacy_v1_checkpoints`, never enter checkpoint-object repair classes, and are
  never rewritten by `--repair`.
- Schema-v1 lifecycle JSONL lines have no typed `identity_scheme`; they remain
  opaque legacy evidence. `doctor`, `checkpoint show`, and `checkpoint export`
  do not infer HMAC/replay trust from those lines or from an event UUID.
- Checkpoints named by a **live traces in-flight marker** are writers
  mid-flight, not inconsistencies, and are skipped.
- Finding identifiers stay actionable without echoing damaged input:
  checkpoint-store findings carry the canonical checkpoint UUID (for the
  in-flight marker classes, the writer attempt's checkpoint id, which may
  never have been published), and `findings_store` findings carry the
  review/investigate `run_id` accepted by `libra review show` /
  `libra investigate show`. A stored checkpoint identifier that is not a
  canonical UUID is shown only as `checkpoint-id-redacted` or a per-report
  opaque label; `conflicted_coverage_claim` always uses its per-report ordinal.
- An **expired terminal writer marker** is not permission to bind a later
  transcript to its pending receipt. `--repair` retires only the stale marker
  ownership; it never changes the receipt's source or marker fence. Re-run
  the original hook/replay only while the same authorized source remains
  available. A same-native-terminal replay with a changed source is
  quarantined as `source_digest_conflict` for manual recovery; it publishes
  no checkpoint, traces ref, or stopped state.
- A **session without checkpoints is legal** (an active session before its
  first stop) and is never flagged; only checkpoint-without-session counts
  as an orphan.
- Captured **gemini rows stay readable** and are never flagged; leftover
  gemini hook *configuration* produces a hint pointing at the
  uninstall-only channel (`libra agent remove gemini`).
- A catalog-row repair for a **scoped** captured session revalidates that
  session's durable workspace lease as the final database write. If the
  lease was released, expired, or re-fenced, doctor leaves the finding
  unrepaired and `manual_required`; only explicit `legacy_unknown` rows use
  the historical unfenced compatibility repair path.
- All repairs are idempotent — running `doctor --repair` twice performs no
  work the second time. With `--repair`, one `agent.doctor.repair` tracing
  span is emitted per repair attempt (`inconsistency_type`, `repaired`,
  `manual_required`); transcript content never reaches the log.

## Reasoning type contract (RG-01)

The RG-01 reasoning type contract is currently metadata-only for durable
capture; it does not change the capture/export behavior above. Its five states are
`provider_visible`, `encrypted_unavailable`, `opaque_archived`, `not_present`,
and `unsupported_shape`. `encrypted_unavailable` means a provider-declared
ciphertext has no authorized decryptor, not that capture failed. Unknown
reasoning-like shapes require a payload-free warning and a partial turn when
provider wiring is added. A text/tool block selected for encrypted-field
verification is rejected as `unsupported_shape`, even with an undeclared
signature-like key; ordinary content is classified before that verifier.
`OpaqueEncryptedBytes` cannot be serialized into an
ordinary transcript or converted into `RedactedBytes`. Only the reasoning
module can construct it, after verifying the shape and JSON-string type of a
Claude assistant `thinking.signature` or `redacted_thinking.data` field from
an already authorized source; its original escaped UTF-8 bytes are kept without
JSON re-serialization or base64 decoding. A syntactically valid record is not
automatically trusted — a matching JSON shape is not
proof of provider identity.
OpenCode 2.0.24 `reasoning.state` is open-ended and cannot establish a verified encrypted source.
No live provider adapter yet produces artifacts from this type contract.

This type-contract verifier accepts only the minimal synthetic envelope; real session records and additional metadata are deferred to the reviewed provider adapter contract.

## Readable reasoning projection (RG-04)

RG-04 adds the readable-reasoning projection path:
`ProviderVisible` reasoning text is the only reasoning content that can enter
the coverage projection, and it always passes typed redaction first — canary
secrets inside reasoning text are counted in the redaction report and removed
before projection, checkpoint persistence or any metadata/digest write. Readable
reasoning projects under its own `reasoning` record type with `provider` and
`source_kind` metadata; it never impersonates an assistant answer, and it is
never written to unredacted metadata or logs.

Direct serde serialization/deserialization of `ProviderVisibleText` is deliberately rejected. Callers must use classification and typed redaction followed by `canonical_turn_bytes` or `safe_turn_projection`. RG-04 does not wire a live provider adapter.


RG-02 archives verified ciphertext as opaque artifacts: each checkpoint
manifest may carry a `reasoning_artifacts[]` array (path/oid/sha256/byte_len/locator/
provider/source_kind/availability/decrypt_capability) storing byte-exact
objects under `reasoning/encrypted/<sha256>`; duplicate ciphertext bytes are
deduplicated to one object, duplicate locators are rejected, and fan-out is
bounded fail-closed (512 entries / 256 KiB manifest / 32 MiB total). The
`content_hash` still covers only the four plain roles; an empty artifact set
leaves the checkpoint tree byte-identical.

New opaque artifact objects use standard zlib stored blocks (level 0) to bound
compression CPU. Their canonical blob bytes and Git OID stay unchanged. This
uses more storage and mirror bandwidth for high-entropy ciphertext; ordinary
objects retain default compression, and valid existing objects are reused
without recompression.

### Controlled opaque reasoning export

Libra preserves provider-encrypted bytes; it neither owns nor guesses decryption keys. Readable reasoning text passes typed redaction; a provider signature authenticates provider output, redactedData represents a provider-redacted reasoning block, and reasoningEncryptedContent is opaque provider ciphertext. None of these metadata labels alone proves native origin. The existing native source requirement remains in force.

For OpenCode exports, Libra now classifies assistant reasoning before the
ordinary transcript Redactor. Visible reasoning text remains in the ordinary
transcript and is redacted. Reasoning-bearing turns remain Incomplete under
coverage v1; they do not gain a new digest record. The ordinary copy excludes
`messages[].providerContext.messages` and assistant reasoning metadata beyond its
visible `id`, `type`, and `text`.
Provider metadata on parts of every role, message envelopes, and the export
document/session info is also removed before redaction, except that completed
tool `state.metadata.files[].file`
strings retain a minimal modified-file projection. Other tool metadata fields
are omitted. The producer-owned Boolean `state.metadata.truncated` is recognized
and dropped without changing completeness, including `false` on routine
successful tools. In a completed tool, a wrong-typed `truncated`, another
metadata sibling such as the `outputPath` written for truncated output, an
unknown `files[]` child, or a wrong type makes capture partial; `files[].diff`
is omitted without changing completeness. Running/error tool metadata is
omitted without changing completeness unless it carries a named opaque hint.
Such a partial export is ineligible for live checkpoint writes
(`SourceRejected`) and verified imports (`ImportError::SourceAuthorization`);
partial is not merely a warning.
Native completed/error tool
`state.content` keeps clean visible text after nested metadata scrubbing; a
named opaque hint makes it partial. Classic tool `state.content` is unsupported
and omitted with partial status. Known classic
nonreasoning part identifiers, step fields, timestamps and safe tool
title/time/file attachments remain. Native model variants, compaction models
and safe error/retry headers remain, with schema-shaped native/classic fixtures
required to retain ordinary coverage and extractor projections. Provider
response bodies/headers, classic format/summary/structured payloads and
session fork, permission, revert or share metadata are omitted with partial
status even when those fields are valid upstream.
This includes classic user `info.summary` objects and assistant summary flags.
Nonempty native user `files`, native `skills[].text`, and additional native
location workspace identifiers are also omitted with partial status.
Retained message and part envelopes use closed field and type projections:
unknown child fields or wrong-type values make capture partial, and nested
provider metadata and named opaque hints cannot ride in user attachments,
classic `info.model`, assistant `model`, errors, or other envelope objects.
An unsafe non-assistant part is omitted as a unit; an independent safe prompt
part remains. Native tool `state.input` and classic tool
`state.input` are free-form ordinary user/tool content. Classic `state.output`
is retained as ordinary content only when it is a String; an object or array
is removed with partial status. Same-named JSON keys in free-form content
alone never prove native origin. Native
`state.output`, `state.raw`, `state.title`, `state.attachments` and
`state.time` are outside the 2.0.26 native schema and are removed with partial
status.
A candidate `state.signature`, `state.redactedData`, or
`state.reasoningEncryptedContent`, an unknown state shape, or a malformed proof
keeps capture partial with no artifact until native origin is verified. Its
reasoning `text` is also omitted from the ordinary copy because it may repeat
or contain fragments of the opaque value. Malformed
or unsupported export envelopes yield a content-free partial snapshot with the
existing generic `SourceAuthorizationMismatch` reason. A
same-named SDK metadata field on reasoning also stays partial and is removed
from the ordinary copy. Installed
OpenCode/model acceptance remains a local macOS check, outside CI.
The capture preprocessor accepts one JSON export object with a `messages`
array. Flat message records, JSONL, missing or wrong-typed `messages`,
duplicate keys, and malformed JSON are content-free partial captures with
`SourceAuthorizationMismatch`; they cannot become ordinary checkpoints.
The standalone OpenCode metadata extractor retains its best-effort flat/JSONL
compatibility, but it does not bypass this capture gate. A parseable export
with an unknown root, message, or classic-info key drops that key and remains
partial while preserving independently safe visible text. The fixed partial
reason does not print the rejected payload; use a supported export to retry.

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

Artifact tree reachability is added by RG-06; controlled read and export
surfaces are added by RG-03.


Checkpoint readers verify the object type, content-addressed identity and ordinary-role bindings before consuming a manifest or metadata body. Skill search skips metadata that does not belong to the catalog's named checkpoint. The fresh `--checkpoint` resolver uses the explicit repository catalog in read-only mode and refuses missing or inconsistent stores without creating a run. Saved-input resume revalidates the persisted spec against the same named catalog and manifest/tree role proof before creating a new input attempt. Default checkpoint show/list summaries and raw-export authorization/audit remain unchanged. Each skill candidate is checked independently: ordinary child trees are capped at 16 MiB and the selected metadata blob at 16 MiB; the checkpoint binding is checked before walking the ordinary closure.

For legacy OpenCode checkpoints, ordinary `checkpoint export` and the bound
metadata reader remove `metadata.anthropic.signature`,
`metadata.openai.reasoningEncryptedContent` and legacy `encrypted_content`
from reasoning parts and reserved export/checkpoint metadata. Readable reasoning,
other metadata and arbitrary tool input or user JSON remain intact. Provider
classification uses the checkpoint's session catalog; a present metadata kind
must agree. Missing session bindings, inconsistent metadata, duplicate keys,
malformed or unsupported OpenCode JSON, and truncated nonempty exports are
refused with a payload-free diagnostic. Empty transcripts remain valid.
`--raw --allow-raw` keeps the original bounded bytes and the existing audit;
`checkpoint show` remains a fixed safe summary. Stored objects are not rewritten,
and future fixes must retain this ordinary-output protection. This legacy
reader does not establish native provenance or implement the newer `state.*`
reasoning artifact channel.

The macOS local content gate is `local_capture_smoke_opencode_macos` in
`agent_local_capture_smoke_test`. It requires an explicit local-agent opt-in,
pins the private OpenCode `2.0.26-libra-native.1` via
`LIBRA_OPENCODE_NATIVE_BINARY`, selects `volcengine-agent-plan/glm-5.3-flash`, and checks
the disposable project before requesting a single fixed file write. The gate
checks captured prompt/model/tokens/files against the source projection,
persisted file metadata, stopped sessions, doctor and uninstall retention.
A failed or timed-out preflight is a failure, with no model request started.
At the historical NPR-18 acceptance checkpoint, OpenCode's private server did not report
readiness; real content acceptance is deferred as `DEFER-OG-OPENCODE-BOOT-01`.
The harness and synthetic tests do not prove real content or native reasoning.


### Private native-origin issuer registration (macOS)

The installation challenge runs with fresh, owner-only home, configuration, data, cache, state and temporary directories inside the owned private installation. Its initial environment is constructed explicitly; caller `TMPDIR`, runtime-loading overrides and host HOME/XDG settings are not inherited. The temporary directory must be canonical, owned by the current user, mode 0700 and empty before the producer starts. A failed precondition or cleanup refuses registration with the existing fixed challenge error.

Scratch is retained for the child lifetime and cleaned after normal exit, failure or timeout; cancellation cleanup is best effort. Inherited process-group descendants are terminated while the leader PID is still reserved. A parent crash or SIGKILL can leave an owner-only scratch directory, which later challenges never reuse. This isolation applies to the Libra-launched installation challenge. Separately launched OpenCode sessions retain their own runtime requirements, and the current `.3` SOURCE/Source A and real-field archive gates remain unproven.

Ordinary `agent rpc trust opencode` pins an exporter binary; it does not register an issuer or prove native reasoning. The historical native path requires a separately installed, reviewed macOS arm64 `2.0.26-libra-native.1` or `.2` producer and its exact frozen v1 observer recipe, rendered for the current canonical Libra executable path. Prepare that observer using the reviewed private bundle's instructions in an isolated owned setup. Global OpenCode remains independent.

The immutable catalog also pins a separate `2.0.26-libra-native.2` candidate (build `78c1f19e9249da8d0143a079f90aec9a`, macOS arm64 binary SHA-256 `28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c`). Its additional endpoint descriptor v2 permits only the first-party `anthropic-messages` route to the exact URL `https://api.anthropic.com/v1/messages?beta=true`; the receipt and Ed25519 attestation remain v1. Anthropic-compatible providers, proxies and changed URLs do not match this endpoint. Selecting `.2` still requires separately measured binary bytes, a fresh private installation/key, explicit exporter trust and explicit epoch-checked issuer registration. The `.1` issuer can remain historical verify-only; no key is automatically promoted. The `.1` build remains the frozen NPR-18 harness baseline, not the current official29 ordinary baseline. A catalog match or synthetic test does not prove real `signature`/`redactedData` source bytes or allow their archival; `reasoningEncryptedContent` remains deferred. Provider/account/model checks stay local manual gates, outside CI.

The legacy v1 observer is distinct from the current ordinary v2 observer. It lacks per-directory filtering: never use it with a shared multi-project OpenCode server. Prefer current ordinary capture; any legacy use must isolate one repository per server. `agent doctor`/observer status reports this old v1 plugin as not installed because current detection requires the v2 header. `libra agent enable --agent opencode` replaces a managed v1 plugin with v2 and breaks the old private recipe match. Generic reinstall or `agent enable` is therefore not a repair for legacy issuer registration. This compatibility fix does not change the ordinary per-directory observer or add a legacy installer.

The following registration example assumes the exact reviewed historical observer is already in place; it deliberately omits `agent enable`. The published-binary compatibility gate uses a fresh owned repository, HOME, XDG directories and epoch-0 registry with the original `.2` public descriptor and issuer/key unchanged. Only the measured producer's registration challenge may access that key. It runs no Session, account/model request or OpenCode server, and revokes the fresh registry during cleanup. A successful gate proves registration compatibility only, not Source A, a current `.3` producer or real native-field archival.

```sh
libra agent rpc trust --dir /absolute/approved-private/bin
libra agent rpc trust opencode
libra agent rpc trust opencode \
  --native-origin-manifest /absolute/private-installation/installation.json \
  --native-origin-expected-epoch 0
```

The two native options must be supplied together and only with the literal `opencode` slug. Initial registration expects epoch 0; later registration requires the current repository registry epoch. Inspect `libra config --get agent.native_origin.opencode.v1` locally before an explicit retry. A conflict refuses without changing trust or registry. The registry includes local installation paths and should remain local.

The public descriptor must be a regular, non-symlink `installation.json`, owned by the current user with mode 0600 in a canonical owned 0700 directory, at most 64 KiB and in the private producer's closed minimal JSON format. Libra does not read the private key. Existing exporter trust, actual approved producer bytes, the current canonical Libra executable path and exact installed `.opencode/plugin/libra-hooks.js` bytes must match. A descriptor or export cannot authorize its own path, public key, endpoint or build.

The actual producer must successfully answer a new random, domain-separated Ed25519 challenge within 30 seconds, with at most 4096 stdout bytes and bounded captured stderr. Child output is withheld on failure. After the challenge, one write transaction rechecks current exporter trust and trusted directories, performs expected-epoch CAS, and writes one complete registry row. Duplicate rows, case aliases, encryption, malformed records and size overflow refuse without repair. Limits are 64 KiB and 128 issuer records; revoked history is never removed to make space.

A new independent installation key becomes active; earlier active keys become verify-only. Existing issuer keys cannot be reactivated by registration or ordinary binary re-trust. Historical records remain immutable. Successful registration proves installation key/binary association only. Receipt signature, current issuer permission, source coordinates and original native field checks remain required before any archive admission; unknown or unproven sessions stay partial. JSON registration output includes the transient current Libra executable SHA256 for local audit; this measurement is neither a signed policy field nor a pinned historical Libra version.

An unapproved row manually created against the unreleased development `cd53…` observer policy remains rejected. Preserve and back up the entire registry and repository, and suspend affected native registration/archive operations pending a separate reviewed repair. Do not delete rows, automatically migrate them or treat them as approved. The bounded compatibility checks do not establish that every user repository has been scanned; supported ordinary capture can be used in a fresh isolated workspace without claiming the affected repository is repaired.



### OpenCode native-origin rotation and revocation

To rotate an installation key, prepare a new independent private installation and explicitly register it with the current epoch. The earlier active issuer becomes verify-only. Verify-only and revoked issuers cannot authorize new native capture, import or pending replay. The current permission reader requires one transaction snapshot, fresh independently measured producer provenance, matching exporter trust and a trusted directory; it returns installation data, not a verified receipt or native artifact.

`libra agent rpc untrust opencode` now removes OpenCode exporter trust and revokes every remaining active or verify-only issuer in one bounded write transaction. It preserves all records and already stored artifacts. It remains available while external RPC is disabled. Malformed or duplicate registry rows, capacity/epoch failures and write faults refuse without a partial trust or registry update. Check and preserve the local registry before retrying; it is never automatically repaired. A repeated successful untrust is idempotent and does not advance an already revoked registry epoch.

JSON retains `slug` and `removed` (`removed` means an exporter trust row was removed). OpenCode additionally returns `native_revoked` (the number newly revoked) and `native_epoch` (the resulting epoch, or null when no registry exists). Other slugs retain their existing output and behavior.

If combined untrust refuses because the registry is damaged, suspend exporter access with `libra config unset agent.trust.opencode`. This removes binary trust only and preserves the registry, including all tombstones. A still-active issuer can regain permission after the same approved binary is explicitly retrusted. Repair the registry while preserving its records, then successfully run combined `libra agent rpc untrust opencode` for permanent native revocation.

Automatic provenance revalidation after an unreadable or changed binary or withdrawal of its trusted directory still removes only exporter trust. Native admission pauses while that trust is absent or mismatched; it leaves the issuer registry unchanged. Explicitly trusting the same reviewed build again can restore an active issuer that was never explicitly revoked. Ordinary binary trust never restores a verify-only or revoked issuer. Re-registering an old issuer is refused: rotation requires a fresh independent key. Registration/revocation do not rewrite historical receipts, private installation descriptors or immutable artifacts. Successful lifecycle operations do not prove native field origin; unproven content stays partial.


### Frozen historical private OpenCode acceptance baseline (NPR-18)

The frozen NPR-18 macOS arm64 harness uniquely accepts `2.0.26-libra-native.1`, build
`bac21cc4027896b8d1836133c347d20f`, SHA-256
`547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196`
and 187635570 bytes. It is based on official OpenCode `2.0.26` commit
`9b4ec5714d481559990db0a816d5dec19541a814`. That commit identifies the official
base; the separate source, patch, lock and build-input digests identify the
patched build. The public `native-origin-build.json` descriptor contains those
reviewed identities and no installation policy, issuer or private key.

Set `LIBRA_OPENCODE_NATIVE_BINARY` to the absolute path of that independently
installed private `opencode` before macOS local capture acceptance. The harness
requires the reviewed version, build digest and size before login or any model
request, bounds the version and login probes to 30 seconds, and puts the same
private binary directory first in each capture/export child's PATH. Missing
opt-in or drift fails the gate; it never falls back to global OpenCode.
The official global installation and existing observer ABI remain separate.

NPR-12's SOURCE_READY_GO covers this reviewed darwin-arm64 build and the current
OpenAI Responses transport mechanism. The existing official source record,
event/export fixtures and synthetic receipt/signature vectors remain historical
and unchanged. Source A is UNPROVEN: deterministic tests, a version string,
registration and this descriptor do not prove real native provider fields or
permit reasoning archival. Failed readiness or quota gates stay partial; the
only authorized paid smoke model is `volcengine-agent-plan/glm-5.3-flash`, with
no alternate-model fallback. This card makes no new model calls. Linux local
functional acceptance remains with the operator.


### Provider-encrypted reasoning lifecycle

`libra cloud sync` validates the reasoning artifact manifest role, canonical tree, `object_index`, decoded `byte_len`, and full SHA-256 before beginning a capture generation. A missing first upload is repaired normally; post-upload readback failure prevents completion. The validated checkpoint trees and artifact indexes must match the final fenced generation. No ciphertext is rendered by this validation.

Session erasure removes artifact references from checkpoint list/show and the session graph; `agent clean --gc` and repository GC reclaim unreachable local artifacts under the existing retention windows. `agent doctor` reports `inconsistent_reasoning_artifact` with the exact canonical `checkpoint_id`; this read-only finding requires manual repair and does not create a persistent checkpoint status column. Physical R2 payload deletion remains deferred; cloud tombstones remove mirror catalog rows and prevent resurrection through normal restore.

Non-quiet `agent enable` and `agent import --yes` disclose that provider-encrypted reasoning original bytes may be archived without decryption and mirrored to cloud storage with their checkpoint. Synthetic lifecycle fixtures remain `encrypted_unavailable` while visible and disappear after erasure; they prove neither native provenance nor a decryptor. Native source proof remains required; unproved OpenCode fields are not archived.


### Authorized reasoning source carrier (OG-16)

The selected registered adapter has an optional pre-redaction source preprocessor, disabled by default. It separates only an already authorized, bounded in-memory source; it cannot reopen provider files or access the checkpoint store. With no capability or no candidate, ordinary snapshot, live and store bytes retain their existing behavior. When enabled, candidates are removed before the ordinary Redactor. Unproved or unmapped candidates retain a payload-free partial classification, including a partial with zero artifacts; they cannot be converted to ordinary complete content.

A sealed native carrier moves original opaque bytes and their source-bound proof together into the shared checkpoint payload. Native issuer, provider, source session, message, part, field, wire SHA-256 and authorized capture/turn association are validated before object or index writes. The metadata-only identity adapter cannot expose payload bytes or mint native authority. Debug output does not contain native coordinates or payload. Source, artifact-byte and artifact-count limits remain enforced.

The authenticated private import transport carries a native-proven typed collection from the authorized descriptor helper to the parent. A one-request key binds the helper response to the request, authorized source, repository scope and collection; the parent verifies that binding and retains the complete native proof before assigning owned items to reserved turns and the shared checkpoint store. An unauthenticated or unproved nonempty collection fails closed, without treating a wire DTO as native authority or returning candidate bytes to the ordinary transcript. Empty preparation keeps its existing response and source projection bytes. Pending v1 still refuses nonordinary payloads before writing an envelope; pending v2 accepts only a validated borrowed native carrier and its prepared identity plan under the existing receipt, fence and lease rules. For an enabled Claude deadline-FD route, preparation is rejected before source resolution, without an ordinary fallback. An existing Pending receipt prevents the wrapper failure diagnostic; NotTracked retains only its existing bounded diagnostic attempt and does not advance sync_revision.

Production provider capabilities remain disabled and no production native issuer is admitted. Synthetic positive fixtures prove the carrier and import transport mechanism only. Source A remains UNPROVEN and real native field admission is deferred; unproved OpenCode fields are not archived. Durable reasoning replay still requires its later gate. This mechanism adds no decryption capability or public metadata enum value.

### OpenCode sealed candidate before native admission (OG-26)

For an authorized OpenCode export, capture checks JSON structure before building its parse tree, then privately separates a closed assistant reasoning `state` and the original `originProof` bytes before ordinary redaction. The private candidate retains decoded UTF-8 values and source session, message, part and field coordinates; it is bounded to the 16 MiB source cap, 65,536 JSON nodes, 64 nesting levels, 8 KiB per receipt and the existing 512-artifact count. The ordinary copy excludes these private values, and an unverified candidate remains a partial snapshot with no ordinary checkpoint consumer. Malformed or over-budget input yields a content-free partial result; structural budget failures report `source_oversize`, rather than an authorization mismatch. A long session can exceed the 65,536-node limit even below 16 MiB; start a smaller capture session and inspect the content-free partial reason before retrying. On rejected-source and sealed-candidate paths, owned raw source, candidate, and parsed-tree allocations are wiped, but Serde's internal scratch and partially constructed values during both successful and failed parsing cannot be individually wiped. This candidate cannot issue native proof or write a reasoning artifact; issuer verification, real field admission and replay remain separate gates. OpenCode CLI/hook/export-to-Session capture integration checks, including deterministic fake-exporter acceptance, are explicitly ignored in CI and run only as macOS local acceptance. Pure parser and sandbox unit tests remain deterministic CI checks. Installed OpenCode and model-response checks are also local macOS acceptance only.


### Pending reasoning v2 codec (OG-17)

The private pending codec pairs its closed inner payload v2 with the live SessionEnd writer and authenticated reader. When an admitted producer supplies a bound native-proven collection, the writer seals its source record and explicit per-turn `verified_nonempty`/`verified_empty` identities. Ordinary pending payloads remain byte-for-byte v1; the outer header and envelope remain v1 with the existing MAC domain, 64 MiB envelope, 128 chunks and 512 KiB-per-chunk limits. A pre-v2 reader rejects v2 and retains its evidence. The current reader checks the original whole-envelope MAC, canonical member/claim association and current catalog, claim and marker fences before the lower reasoning issuer validates the fixed source proof. It restores only reserved-turn artifacts and commits their original stable identities with the checkpoint, claim and ref transaction. A failed append retains the pending artifact and claims; each retry reauthenticates the original bytes and current fences without reopening a provider source or minting a replacement key. Missing or damaged private-key/source evidence requires diagnosis and manual recovery. A decoded DTO or envelope MAC alone never proves native provider origin: unproved fields remain partial and are not archived. Production native issuers and real OpenCode Source A evidence remain disabled; synthetic tests exercise this transport without claiming real native archival.

### Private reasoning identity and garbage collection

Garbage collection excludes the reserved `agent_reasoning_identity` scope from generic metadata object-reference scanning. This applies to the entire stored value, including a damaged value containing a JSON object ID or a nested object ID; those values do not trigger object hydration. This exclusion requires no private reasoning key. Ordinary metadata object roots, private capture recovery exclusions, and capture pending parent roots retain their existing rules. Forward recovery must retain both the private rows and this exclusion while a reasoning identity writer is stopped or repaired. This guard does not create identity records, issue native provenance, or enable OpenCode reasoning archival.

### Reasoning identity coverage and session limits

When reasoning coverage cannot be safely compared, live capture and export fail closed with `reasoning coverage is partial; run libra agent doctor before retrying capture`. A complete-turn identity conflict is reported by doctor with the reason `reasoning_identity_changed`; it requires manual review of the authorized reasoning artifacts, and doctor does not choose a winner.

The private identity namespace has separate `identity-v1` and `identity-v2` keys. Together they are limited to 4096 records and 1 MiB of stored encoded values per session. Reaching either limit stops the write and reports `private reasoning identity session map reached its safe capacity; start a new capture session`. An absent map is the legacy state: candidate-free ordinary turns retain their existing coverage behavior, and the first eligible identity-bearing turn may create a map. A malformed, oversized, unexpected, or unauthenticated map rejects the entire reservation with `private reasoning identity is invalid; start a new capture session` before any claim write, including candidate-free turns; an import receipt records this as failed. Doctor does not repair an invalid identity map, and a missing conflict finding does not make the session complete. Do not edit the private metadata scope manually.

These checks compare capture identity only; they do not prove native provider origin. Unproved OpenCode reasoning remains partial and is not archived.


### OpenCode native import directory binding

Native OpenCode exports bind the session directory in `info.location.directory`.
Import also accepts classic `info.directory` and `info.cwd` declarations. It
collects all declared string directories and requires them to resolve to one
canonical directory in the current Libra storage boundary; no field takes
precedence over a conflicting declaration. Directory fact collection does not
coerce non-string values; the existing source and schema validation still applies.
Equivalent lexical paths and symlink aliases keep the same canonical scope
checks. A missing or conflicting directory returns `LBR-AGENT-016`; a directory
belonging to another repository stays rejected. This compatibility fix does not
approve native reasoning origin.


### OpenCode 2.0.29 idle error projection

The public 2.0.29 idle-message schema adds an optional `error`. An absent error
keeps the ordinary projection. A present diagnostic header requires string
`type` and `message`; optional HTTP `status` is an integer from 100 to 599.
Missing required fields, a wrong shape or an invalid status removes the whole
error and marks capture partial. A `response` body and unknown members are
removed and keep capture partial; an opaque hint removes the entire error.
Existing compaction and retry error handling is unchanged.

These are ordinary typed diagnostics, not native producer or field proof. The
new regression fixtures are synthetic. Actual idle-error output from a live
model Session remains unproven; a successful ordinary session does not prove
that failure path. Real OpenCode/account/model checks run locally on macOS
and stay outside CI. Native reasoning Source A remains a separate gate.


### OpenCode ordinary partial live checkpoints

Automatic live capture can save a safe ordinary copy of an authorized OpenCode
export when unsupported reasoning was discarded. The checkpoint remains
`Unproven` / `Partial`; saved content is not complete reasoning coverage or native
provider evidence. At least one known non-reasoning text or message part with
usable content must remain. A source containing only metadata or erased reasoning
skeletons is refused. This route accepts the current native `messages` export;
legacy classic `info` / `parts` exports remain excluded.

This storage-only route requires strict single-JSON parsing, successful exporter
authorization, exact session mapping, bounded redaction and repository-scoped
source HMAC binding. The source session's `info.id` must equal the selected
provider session. The owning carrier records `repo_id`, Libra session, provider
session and agent kind at bind time and checks them against the actual checkpoint
request. The HMAC is an opaque string; its format alone does not verify the scope
or prove native origin. Generic ordinary consumers stay closed to `Unproven`.

Unsupported reasoning state, including `reasoningField`, is written with the readable
associated reasoning text and time. The ordinary transcript keeps that text, `time`,
`state`, and the other unproven fields. Opaque hints include `signature`,
`redactedData`, `reasoningEncryptedContent`, `thoughtSignature` and `originProof`.
An original `originProof` key anywhere in the parsed source, including malformed
or unfamiliar nested values, or a sealed candidate prevents this fallback.
Only the completed scrub and redaction result can enter storage; a metadata flag
cannot grant this capability.

For this verified storage-only route, `source_authorization_mismatch` also means
reasoning or unrecognized data was discarded; exporter authorization has already
passed. Other paths retain their existing rejection behavior. The marker still
means `Partial`, and Source A remains UNPROVEN. No reasoning artifacts, private
reasoning identity, Complete coverage claims or native ACK are created.

The source input cap bounds each snapshot independently (up to 16 MiB).
Different idle actions can duplicate whole-source snapshots in content-addressed
storage. Cumulative retained snapshots have no per-session total cap and can grow
with idle count × source size, before redaction and storage overhead. This route adds
no cross-idle deduplication protocol; existing action, generation, lease, scoped
tombstone and ref-CAS fences remain in force. A successful ordinary lifecycle
receipt follows a verified durable partial checkpoint write. A failed direct
write stays dirty or leaves a content-free pending intent, without a successful
ACK or generation advance. There is no durable partial payload replay.

Explicit import of this unsupported-state source remains refused with
`LBR-AGENT-020`; this live fix does not enable import or native admission. The
stored ordinary payload remains a single redacted JSON document, with no new
public reason or metadata enum value. Checkpoint/export readers in this build use
that ordinary payload. Older-binary compatibility requires a separate actual
readback and is not inferred from the source shape; this does not relax the V2 import rule requiring partial
projections to have zero bytes and zero redaction metrics.

The official 0.30.49 release includes this ordinary live fix and can save its safe
ordinary copy under the closed rules above. Release completion does not prove native source
or complete later COV family gates.
For historical OpenCode imports, sessions containing unsupported reasoning state
remain rejected with `LBR-AGENT-020`; their repair is deferred. Real
OpenCode Session/account/model checks run only locally on macOS and stay outside
CI; synthetic tests do not prove native source.


### Current official OpenCode ordinary source baseline

The current official ordinary source reference is OpenCode **2.0.29** at
`042d0832f5f3a73eaa039a58a7d565776f6d4290`. It uses an explicitly measured
official binary and a server/exporter of that same version for local acceptance.
The frozen 2.0.26 source record, existing JSON fixtures and separate private
`.1` / `.2` identities retain their original meanings. The old private `.1`
acceptance helper still accepts only its exact version, binary and build ID.

Official version/source matching does not approve a native producer, register
an issuer or prove any opaque field. Ordinary exporter trust is never native
producer approval or Source A. The separate native.3 registration and real
field-source gates remain independent and do not block ordinary capture;
151 mechanical producer cases are not
whole-producer source/full acceptance.

The 2.0.29 idle header requires string type/message and optional integer HTTP
status from 100 to 599; malformed or sensitive error members stay partial.
Offline synthetic tests cover that adapter; real idle-error failure output
remains unproven.

This ordinary baseline is scoped to automatic capture, a durable checkpoint and the
ordinary redacted export of that checkpoint, with prompt, model and token
projections compared to the authorized source. Unsupported reasoning state is
retained with its text. The source/snapshot status pair is `Unproven` / `Partial`: `Unproven`
is the internal source classification, while `Partial` is the persisted
completeness value. Readable reasoning text is retained. Associated reasoning time is retained with the unproven fields. The model comparison uses `extraction.model` from the
native assistant record. The top-level `model` is hook metadata and can differ.
Token totals use distinct native assistant records without counting the session
summary twice. A no-tool sample proves only empty-files consistency, not a
write/edit/apply_patch positive.

For historical OpenCode imports, sessions containing unsupported reasoning state
remain rejected with `LBR-AGENT-020`; their repair is deferred. This automatic
capture baseline does not enable historical import, pending replay or native
admission. Ordinary exporter trust is never native producer approval or Source A.
Source A remains UNPROVEN.

Reuse earlier automatic-capture evidence only after checking the final source
and configuration and verifying readback with the actual new binary. Unverified
checks remain pending. A successful ordinary sample does not establish native
fields, complete reasoning, real idle.error failures, automatic terminal
completion or full-suite acceptance. Real OpenCode Session/account/model checks
run locally on macOS and stay outside CI. Metadata-only, empty output and skipped
checks do not count as content acceptance.

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

Retained visible reasoning is part of the redacted checkpoint and follows its
export, `libra agent push`, and cloud-sync paths when used. There is no separate
flag to exclude that reasoning while retaining other captured content.

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

Repositories opened by a capability-4 build carry repository capability 4. Released binaries through 0.30.66 refuse every command on those repositories, including `agent clean`; install a capability-4 build before continuing. Published binaries first provide it in v0.30.67 on Linux and v0.30.68 on macOS/Windows; v0.30.83 includes it on all platforms. No existing agent workspace directories are migrated.

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

The minimum Codex CLI version for this twelve-event installation is **0.162.1**. Upgrade older Codex clients before re-running enable. Existing eleven-event installs report not installed until refreshed; their already-approved entries do not become trust gaps. This also fixes earlier rejection of `PostCompact` on the shared `compaction` verb. Late interrupts preserve an already stopped session instead of reactivating it.

Codex hook settings reads accept regular files only: `hooks.json` and `config.toml` are limited to 16 MiB each, and the integrity recovery journal to 64 KiB. Installation, removal and status checks refuse symlink/special-file leaves; Unix FIFO opens cannot block the reader. An unsafe or oversized file is left in place; move it aside or reduce its size before retrying. A generated update over the settings limit is rejected before either paired file is published. Parse diagnostics identify the file without printing configuration contents. Parent directory aliases selected by `CODEX_HOME` remain supported; this is not a strict whole-process filesystem deadline.


Codex installation diagnostics in `libra agent doctor` include all 12 managed events. Each event has a fixed `reason`: `configured`, `missing`, `drifted`, `untrusted` or `disabled`; JSON exposes these rows under `data.provider_hooks[].events`. Aggregate `installed=true` requires every managed event to match the current handler and be enabled and trusted. Edited or stale duplicates carrying Libra's status message keep that event unconfigured; marker-less user handlers are outside this diagnostic. An intentional `enabled=false` is reported as disabled without adding a trust-gap banner when its hash is current. These read-only settings checks never print hook commands, hashes or configuration bodies and do not prove that the host executed a hook. Use Codex 0.162.1 or newer; refresh missing or untrusted Libra handlers with `libra agent enable --agent codex`, and review intentional disables before running enable. For drifted entries, inspect `hooks.json` and the matching `[hooks.state]` entry in `config.toml` first: enable refreshes handlers whose ownership is still provable, but preserves malformed handlers that only carry the status message. Those require manual correction or removal after review; repeating enable alone may not clear their diagnostic.


## Codex ordinary thinking and attached data (CX-16/CX-06)

In this development source, `libra agent checkpoint show <id> --json` returns
credential-scanned ordinary Codex thinking in `data.reasoning.readable`, and
nonempty top-level reasoning `response_item` opaque values in
`data.reasoning.encrypted_content`.
`availability` is `unproven`, or `partial` for truncated, malformed or unsupported
records; `readable_obtained` reports whether this view contains readable text.
These values do not prove native origin or decryption.
The ordinary rollout also retains nonempty `encrypted_content` at a reasoning
`response_item` and in `compacted.payload.replacement_history`. The latter
remains available in the ordinary rollout and default export, but is not
listed in the JSON show reasoning facet. An internal eligibility check requires
the matching rollout filename, `session_meta` session and a supported Codex
writer version, then binds each decoded JSON string to its turn and record
ordinal. Show and export do not use that index. Duplicate keys and malformed
records are excluded from the eligibility check without deleting their
ordinary source bytes. Indexed values are unproven, neither decrypted nor
native artifacts.

The checkpoint metadata must belong to its ordinary tree and agree with the session's provider when it
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

`libra agent doctor --json` now reports `codex_legacy_reasoning_revision`
for a current-scope, ref-reachable committed checkpoint whose ordinary
transcript contains bound Codex thinking but lacks its content-bound revision.
Plain `doctor` only reports it; `doctor --repair` appends a metadata revision
keyed by checkpoint and content digest. The original checkpoint and transcript
stay unchanged, repeated repair is idempotent, and an erased session is never
recovered. Each scan page examines at most 64 candidate rows and reads at most
128 MiB of ordinary transcript data, with a 16 MiB cap per checkpoint;
`--repair` may read eligible bodies again under a separate 128 MiB page budget. When
the report gives a continuation cursor, pass it as
`--codex-legacy-after <checkpoint-id>`; add `--repair` on each page to append
eligible revisions. Ineligible, unreadable and skipped checkpoints are not
certified by later pages. This creates no native reasoning artifact and
requires no schema migration.

Human show, checkpoint/session lists, graph and doctor do not expand these
bodies. Reads do not rewrite objects, traces or coverage claims. Raw export
still requires `--raw --allow-raw` and records granted or denied access; default
export creates no raw-access audit. Run the synthetic reader cases locally with
`cargo test --features test-live-agent --test codex_capture_local_test codex_legacy_ -- --ignored --test-threads=1`.
They do not prove live model capture, and are excluded from CI.
Run the separate local legacy revision and cursor repair case with
`cargo test --locked --features test-live-agent --test agent_doctor_repair_test codex_legacy_ -- --ignored --test-threads=1`.


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



### Codex reconciliation from a closed index backup (CX-22 development)

```sh
libra agent doctor --codex-index-backup ./closed-index.sqlite --json
libra agent doctor --codex-index-backup ./closed-index.sqlite --codex-index-page 2
```

Supply an already closed, consistent Codex SQLite backup. Relative paths are
resolved lexically from the invocation directory.
The option conflicts with `--repair`; page accepts 1–10 and requires the
backup. Without the backup option, existing doctor output is unchanged.
This view never opens the live home database, reads rollout bodies, imports,
repairs, or hydrates objects.

JSON adds `data.codex_reconciliation`. Coverage is the provided backup's
`cutoff_ms` through `coverage_until_ms`, with a separate window anchor.
Each invocation re-anchors its seven-day window: pages are not a stable cursor.
`scope_candidate_count` covers all scope-filtered candidates; row statuses
and `page_totals` cover only this page. Null means not computed.
A page beyond the candidate set has `reason=page_out_of_range` and null
totals. Old backups give `outside_window` with null totals;
`empty_snapshot` means only that the supplied backup is empty.

Rows distinguish `stored_source`, `missing`, `missing_data`, `partial`, `pending`,
`erased` and `unknown`. Stored source requires current typed metadata and
nonempty local ordinary objects whose roles, hashes, sizes and manifest-declared
stored length are verified. The snapshot's full-source byte count can differ
from per-turn imported data. Presence does not certify readable thinking or
complete turn coverage. Legacy or unproved metadata, absent/corrupt/packed-only
local objects remain unknown.

A verified empty transcript is `missing_data/stored_transcript_empty`. A manifest
length mismatch is `unknown/stored_size_mismatch`, with no import hint.
Quarantined sessions and committed import identities without a session are
unknown. Schema-pending sessions and retryable receipts stay pending; erased
records take priority, then scoped pending evidence, then directory/data proof.
`reason` uses fixed snake_case keys, including `scope_root_unproven`,
`response_inconsistent`, `row_invalid`, `timestamp_schema` and `budget_exhausted`.
Schema, migration and integrity refusals use `schema_unsupported`,
`migration_unsupported` and `integrity_failed`. Invocation scope, worker and
transaction failures use `scope_unavailable`, `worker_unavailable` and
`catalog_unavailable`; other catalog/scope failures use
`scope_or_catalog_unavailable`. Catalog deadlines and typed work-budget errors
use `budget_exhausted`. Unclassified backup IO errors use `backup_unreadable`.

Only proven-scope `missing` and `missing_data` rows receive an explicit import
hint without `--yes`. Erased rows have no hint. Pending includes receipts and
import states without a session row; an expired import lease stays pending with
`detail=import_lease_expired`. If an earlier capture failed on a SHA-256 or
BLAKE3 repository because of an object-format mismatch, retry the original capture
or import with a build containing the helper-format fix. Doctor stays pending
until persistence succeeds and does not remove earlier loose objects.
Insufficient work allowance is explicitly
`budget_exhausted`. Bad Libra catalog rows remain unknown while other rows are diagnosed. A malformed
provider index row or shape makes the whole section unavailable.

`scope_unproven` means directory ownership could not be established;
`child_attribution_unproven` means a child has no independently verified source
and the edge does not prove that its content belongs to the parent. Archived
rows are included. An active session before its first checkpoint can be
`missing_data`; its import hint is an explicit preview, never an automatic write.
Case/NFC-NFD variants and macOS Data-volume aliases are reported as ambiguous,
not granted source access. Unrecognised symlink spellings outside the lexical
root are excluded; links encountered below that root remain unknown.

Any timestamp more than five minutes in the future makes the backup unavailable.
Limits are 10000 recent candidates across repositories, 1 MiB of thread text,
independently bounded edges, 4096 entries per scope-directory inspection and 1024
cached directory identities. The backup parent-directory sidecar scan also
fails as unavailable above 4096 entries. Limits can cause unknown/unavailable on large
repositories. One two-second reconciliation budget also covers up to 8 MiB of
catalog/checkpoint metadata and decoded tree/manifest proof bytes, plus 64 MiB
of ordinary-object verification;
normal doctor scans and worker cleanup keep their existing behavior.
No reliable zero is reported after failed or truncated work. New Codex tests run
only with local `test-live-agent` + `--ignored`; they are not scheduled in CI.


### Codex full-history index backfill (CX-19 development)

```sh
libra agent import --codex-index-backup ./closed-index.sqlite --limit 20 --yes --json
libra agent import --codex-index-backup ./closed-index.sqlite --limit 20 --cursor 20 --yes --json
```

This selector reads an explicitly supplied, closed and consistent Codex SQLite
backup and backfills sessions absent from the current worktree's catalog. It
covers the full supported history, including older and archived rows. It does
not require the later millisecond-index migration. Codex is implied; another
`--agent`, another selector or `--restore-erased` is rejected. Relative backup
paths use the invocation directory. The existing consent prompt or `--yes`
remains required before reading provider transcript bodies or writing traces.

The full scoped index is sorted by session ID before applying `--cursor` and
`--limit` (default 20, maximum 100). Catalog eligibility is checked after paging,
so importing page one does not move later slots. Completeness across pages
requires the same backup and unchanged directory layout. Restart at cursor zero
after either changes. Each page reports its own selected slots.

Only an absent catalog session enters the existing import writer. Stored
readable thinking follows the ordinary import/export path. Already cataloged
sessions appear separately in an additive, nonempty `data.existing` array:
`status=existing`, `agent_kind`, a hashed `session_id`, and
`selection_reason=existing_catalog_session`. They are not refreshed or counted
as imports, and the marker does not prove complete thinking capture. Retry an
existing partial, metadata-only or newly extended session with the original
`--session <id> --agent codex --yes`, `--all` or `--since` import path. A removed
provider file does not make an existing stored checkpoint disappear.

The original schema-v1 `results`, `partial_results`, `skipped`, `failures` and
`next_cursor` fields retain their meanings. Partial durable progress can appear
in both `partial_results` and `failures`. Existing markers also remain visible
in partial-error details and human counts. Old selectors do not emit the new
array. Erased slots remain skipped with `LBR-AGENT-019`. Pending, unknown or
budget-limited catalog slots fail with `LBR-AGENT-018`; ambiguous directories or
invalid source locators fail with `LBR-AGENT-020`. Legacy/unscoped rows and nested
worktrees can require manual action, so repeated index runs may remain nonzero.
Use the actual owning worktree and an explicit session import for recovery.

An index locator must be absolute, have normal path components, and end in a
`rollout-…-<same-session-id>.jsonl` leaf. Non-rollout auth/config/history/state
paths are rejected before source-reader dispatch. A valid leaf still needs the
existing configured-root, pinned-descriptor, header identity, cwd and permission
checks. Archived files under `CODEX_HOME/archived_sessions/` use those same checks.
A moved or other-host path is a per-slot 020 failure; use `--session` to retry
through the original authorized discovery path. This selector adds no locator
fallback. Index spawn edges supply no writer parent link or child authorization.

Before the import loop starts, backup, root, catalog snapshot, response or resource
failures return nonzero 018 with a
fixed `reason_key` and no successful batch document. If a root proof fails after
imports have committed, further source reads stop: error details retain completed
results and partial progress, with 018 failures for the unattempted slots and the
fixed root reason. A tombstone lookup that fails inside the import loop also keeps
earlier results; its reason is `catalog_unavailable`, or `budget_exhausted` after
the deadline.
Limits include a 16 MiB
backup, 10000 full-history rows/edges, one combined 1 MiB text allowance and a
4 MiB serialized helper response. JSON escaping can exhaust the latter even
when raw text fits. Paging does not reduce full-index validation work. The
initial index phase has a two-second budget within the original 120-second
import deadline; the cumulative source allowance remains 64 MiB. Use explicit
session/path imports for sources outside the index limits. Windows runtime and
real-model capture remain unverified for this development surface.
Scope walking also limits each scanned directory to 4096 entries and keeps at
most 1024 memo entries. Exhausting either can classify an in-scope row as layout
unknown and return per-slot 020; it does not prove that the session is foreign.
Use an explicit session/path import when this bounded projection is unavailable.

For example, first quit Codex, then use Python's SQLite backup API with a
read-only source and a fresh destination (the pinned Codex CLI uses `state_5.sqlite`):

```sh
python3 - <<'PYBACKUP'
import contextlib, os, pathlib, sqlite3

home = pathlib.Path(os.environ.get("CODEX_HOME", pathlib.Path.home() / ".codex"))
state = home / "state_5.sqlite"
destination = pathlib.Path("closed-index.sqlite")
with contextlib.closing(sqlite3.connect(state.resolve().as_uri() + "?mode=ro", uri=True)) as source:
    destination.touch(exist_ok=False)
    try:
        with contextlib.closing(sqlite3.connect(destination)) as target:
            source.backup(target)
    except BaseException:
        destination.unlink()
        raise
PYBACKUP
```

Use the state filename for your installed version. Do not pass a live provider
database directly: the reader refuses WAL/SHM/journal sidecars; their absence
alone does not prove that a database is closed.


Failure and existing IDs use `sha256:` plus the first 12 hex characters of the
provider ID's SHA-256. To map a hash using the same closed backup locally:

```sh
python3 - ./closed-index.sqlite sha256:0123456789ab <<'PYMAP'
import hashlib, pathlib, sqlite3, sys
uri = pathlib.Path(sys.argv[1]).resolve().as_uri() + '?mode=ro&immutable=1'
with sqlite3.connect(uri, uri=True) as db:
    for (sid,) in db.execute('SELECT id FROM threads'):
        if isinstance(sid, str) and 'sha256:' + hashlib.sha256(sid.encode()).hexdigest()[:12] == sys.argv[2]:
            print(sid)
PYMAP
```
