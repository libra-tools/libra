# `libra cloud`

Cloud backup and restore operations (D1/R2).

## Synopsis

```
libra cloud sync [--force] [--batch-size <N>]
libra cloud restore [--repo-id <ID> | --name <NAME>] [--metadata-only]
libra cloud status [--verbose]
```

## Description

`libra cloud` provides backup and restore capabilities using Cloudflare D1 (serverless SQLite) for object indexes and metadata, and Cloudflare R2 (S3-compatible object storage) for git objects. This enables full repository backup to the cloud with incremental sync support.

The sync workflow tracks which objects have been uploaded via an `is_synced` flag in the local `object_index` table. A default sync selects only the indexed objects whose local flag is still unsynced (`is_synced = 0`), which keeps repeated syncs efficient. It does not compare against D1, so after pointing `LIBRA_D1_DATABASE_ID` at a new or replaced D1 database, run `libra cloud sync --force`: the `--force` flag re-syncs every indexed local object and is also the recovery path for R2 bucket-side data loss. After objects are synced, repository metadata (references/branches) is serialized to JSON and uploaded to R2, with a content hash check to avoid unnecessary uploads.

Clone and fetch index objects received in packs. For older repositories with gaps, `cloud status` counts missing index entries as pending. Both sync modes walk the history of every backed-up reference, including remote-tracking branches and tags, and add missing reachable objects to the index before selecting uploads. Existing semantic object types and sync flags are preserved. If a reachable object is missing locally, for example a parent outside a shallow clone or a promised blob in a partial clone, fetch the full history or hydrate the missing objects before syncing.

Each repository is identified by a UUID (`libra.repoid` config key) and optionally a human-readable project name (`cloud.name` config key or directory name). The project name is registered in a D1 `repositories` table for lookup during restore.

Restore can target a repository by UUID (`--repo-id`) or project name (`--name`). It downloads the object index from D1, optionally downloads objects from R2, restores metadata (references), and populates the working directory from HEAD.

The optional `agent.commit_trailer=true` commit association has a separate
repository-local catalog and, for `--no-agent-trailer`, a local
`.libra/agent-commit-links.v1` recovery journal. Cloud refs metadata and the
current D1 capture graph back up neither the commit-association table nor
this journal. A successful cloud restore therefore does not establish that
trailer-free agent links were restored; an empty link table means the evidence
may be unavailable. A reachable commit trailer can provide a later
reconstruction clue, subject to repository and session-scope checks.

`cloud sync` reads local `agent_session.agent_kind` before credentials, D1, or R2. A kind outside the current capture allowlist (`claude_code`, `cursor`, `codex`, `gemini`, `opencode`, `copilot`, `factory_ai`, `grok`) — including `hermes`, even after local capture is supported — stops the command with `LBR-CONFLICT-002` (exit 128). `--force`, `--json`, and `--progress=json` do not bypass that refusal, and the command does not print a successful sync. Kinds that are allowed keep the previous sync order. The allowlist is the same list the D1 `agent_kind` CHECK is built from, so a kind passes exactly when today's D1 tables accept it, and any later kind the cloud mirror does not support yet is refused too. The refusal carries two hints: while the repository holds such local-only capture sessions, `cloud sync` keeps refusing until a later Libra release adds complete cloud support for that kind; until then, back up the whole repository, including its `.libra` directory, with an external backup tool you choose. Disabling hooks or removing capture sessions is not a supported way around the refusal.

After D1 is initialized and before its first remote write, `cloud sync` reads the `agent_kind` CHECK of any existing D1 `agent_session` and `agent_capture_session_v2` tables with a read-only `sqlite_master` query. D1 tables are never rebuilt, so a table created before `grok` support still lists only the original seven kinds. The probe runs on every sync, even when the repository has no local capture sessions. If a local kind is not accepted by an existing table, or the CHECK cannot be read (an `agent_kind` constraint other than a single `agent_kind IN (...)` list of quoted kinds, or a failed schema query), sync stops with `LBR-CONFLICT-002` before uploading anything. The hint names the only recovery: create a new D1 database, point `LIBRA_D1_DATABASE_ID` at it, and run `libra cloud sync --force` to backfill it completely (a default sync only uploads objects not yet marked synced). Missing tables are created with today's DDL and are never refused.

Right after that probe, still before any remote write, `cloud sync` freezes the object identities, the refs, the configured `cloud.name` and the agent-capture catalog it is about to upload into a private snapshot spool under `.libra/cloud-sync/snapshots/<run>`, using short local reads for its boundaries and project name and short page-by-page reads for the refs and the objects, so other local writers are not held up. Uploads, the metadata JSON, the project name registered in D1 and the `is_synced` flags then come only from that spool, so objects, refs or a `cloud.name` change made after the snapshot is taken wait for the next sync. The refs are written into the spool a page at a time in their upload order, and the metadata JSON is hashed and uploaded from that file in bounded chunks (a multipart upload once it exceeds 8 MiB), so a repository with very many refs never needs them all in memory; the uploaded bytes and the stored `cloud.metadata_hash` are the same as before. The bytes read for the upload are checked against that frozen length and hash before the upload takes effect, and the stored `cloud.metadata_hash` is cleared before an upload and recorded only after it succeeds, so it never describes bytes other than the uploaded ones (if recording fails, the sync reports it and the next sync uploads the metadata again). A multipart upload that fails is aborted (a failed abort is reported with the error), and one that is cancelled is aborted in the background on a best-effort basis (if that abort fails, a warning names the object — on stderr, or as a `cloud_sync.metadata.warning` event under `--progress=json`); a sync killed or cut off mid-upload can still leave its parts in the bucket, so a bucket that receives metadata larger than 8 MiB needs a lifecycle rule that aborts incomplete multipart uploads. If another Libra command commits to the repository database while the snapshot is being taken, the snapshot is taken again (up to three attempts), so such changes may be included; if the database keeps changing, the sync stops before sending anything and can simply be run again. An object is marked synced only after its upload and its local status update both succeed; otherwise it counts as failed and is retried next time. The run directory is created fresh for each sync with owner-only permissions (0700 directory, 0600 files) and an `owner.json` marker, and it is removed when the run ends, whether it succeeds or fails. `cloud sync` refuses to use `.libra`, `.libra/cloud-sync` or its `snapshots` directory when any of them is a symbolic link, resolves outside `.libra`, belongs to another user, or is writable by group or others (fix it with `chmod go-w`). On Windows, `.libra` and the spool directories must carry a protected access list that grants only you and SYSTEM; a `.libra` that inherits its folder's permissions is refused with the `icacls` command that makes it private. If a run directory cannot be removed, the sync result stands: a successful sync prints a warning naming the directory (`data.warnings` under `--json`), and a failed sync adds the same note to its error. The directory holds that run's object identities (id, type, size, repository id, timestamp), refs, project name, owner marker (pid, host, boot id) and frozen agent-capture catalog (`catalog.json`: sessions, checkpoints, prune and import tombstones, the traces head, subagent claims, revisions and links, legacy Claude mirror rows); every warning or error about a run directory left on disk that cloud sync could safely inspect lists the files it actually holds and what each contains (a leftover that is not private to you is reported as not inspected, without a listing). It is never reused and can be deleted by hand. At the start of each sync, leftover run directories of earlier syncs (left by a crash or a killed process) are checked: one is removed only when its owner marker proves the run's process has exited — written by `libra cloud sync` on this host, in this boot and (on Linux) in this PID namespace, for a process that no longer exists. Any other leftover — a missing or unreadable marker, another host, another boot, a process still running, or any leftover on Windows, where an exited owner cannot be proven — is kept and reported in the same places, with the reason; delete it by hand once you have confirmed that run has stopped. Cleanup reads only the owner marker, never follows a symbolic link, and does not inspect a directory or marker that is not private to you (it reports it as not inspected instead). On Windows the owner marker records the computer name from the operating system, and the `icacls` remedy names your account by its security identifier (`*S-1-5-…`). If the spool cannot be written, for example because the disk is full, the sync stops before anything is sent to D1 or R2.

### Object format (B3-09 / B3-14)

Backup persists the repository `core.objectformat` into D1 `repositories.object_format` (and tags `object_index` rows). Restore treats that metadata as the only authority for sha1/sha256/blake3 — it never guesses from `o_id` length. Snapshots without metadata that contain any 64-hex object id fail closed with `LBR-REPO-002` (exit 128) and a hint to re-run `libra cloud sync` on the source; all-40-hex catalogs without metadata remain sha1 for legacy compatibility. Clients older than REL-B3-02 must not restore new metadata-bearing backups.

## Global Config Schema Guard

Configuration schema compatibility is role-scoped. Before `libra cloud` trusts
configuration, it inspects GlobalConfig and SystemConfig metadata read-only. A
future configuration schema or an unregistered/mismatched migration receipt
fails closed with `LBR-CONFIG-001` when that scope is required. Known
Repository-only receipts, including `2026090801` in the current manifest, do
not make a configuration store future; its supported values remain readable.
The configuration-owned legacy-reader barrier is recognized by this build;
see [configuration compatibility](config.md#configuration-schema-compatibility).

Global configuration uses `LIBRA_CONFIG_GLOBAL_DB` or the XDG configuration
directory (`$XDG_CONFIG_HOME/libra/config.db`, defaulting to
`<home>/.config/libra/config.db`), falling back to the legacy
`<home>/.libra/config.db` until it is migrated;
system configuration uses `LIBRA_CONFIG_SYSTEM_DB` or `/etc/libra/config.db`.
Complete process/repo-local storage settings can make GlobalConfig unnecessary
(`cloud` must also satisfy its D1 settings). They do not prove that SystemConfig
defaults are unnecessary. Diagnostics identify the affected scope, ledger and
version without printing configuration values or untrusted receipt names.

Unknown or unsupported state is upgrade-only here, not automatically repaired.
Install a compatible newer Libra binary:
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`.
Do not delete or edit SQLite receipts manually. Use `--offline` or
`LIBRA_READ_POLICY=offline|local` only for intentional local-only object access;
these modes warn and are not authorization for remote synchronization.

## Options

### Subcommand: `sync`

Sync local repository to cloud. Uploads objects to R2 and indexes to D1.

| Flag | Description |
|------|-------------|
| `--force` | Sync all indexed local objects, regardless of local/D1 sync state. Useful for deliberately re-upserting every object or recovering after R2 bucket-side data loss. |
| `--batch-size <N>` | Number of objects to process per batch. Default: `50`. Must be at least 1. Smaller batches produce more frequent progress output; larger batches reduce overhead. |

```bash
# Incremental repair sync
libra cloud sync

# Force re-sync everything
libra cloud sync --force

# Use smaller batches for verbose progress
libra cloud sync --batch-size 10
```

### Subcommand: `restore`

Restore repository from cloud. Downloads object indexes from D1, objects from R2, and restores metadata and working directory.

| Flag | Description |
|------|-------------|
| `--repo-id <ID>` | UUID of the repository to restore. Mutually exclusive with `--name`. One of `--repo-id` or `--name` is required. |
| `--name <NAME>` | Human-readable project name to restore. Looked up in the D1 `repositories` table. Mutually exclusive with `--repo-id`. |
| `--metadata-only` | Only restore the object index to the local database. Do not download objects from R2 or restore the working directory. Useful for inspecting what a repository contains before doing a full restore. |

```bash
# Restore by repository ID
libra cloud restore --repo-id a1b2c3d4-e5f6-7890-abcd-ef1234567890

# Restore by project name
libra cloud restore --name my-project

# Only restore metadata (object index)
libra cloud restore --name my-project --metadata-only
```

### Subcommand: `status`

Show the current cloud sync status for the repository.

| Flag | Description |
|------|-------------|
| `--verbose` | Show details of individual unsynced objects (up to 20). |

```bash
# Show sync status summary
libra cloud status

# Show detailed status with unsynced object list
libra cloud status --verbose
```

## Common Commands

```bash
# Initial sync to cloud
libra cloud sync

# Check sync progress
libra cloud status

# Detailed status showing pending objects
libra cloud status --verbose

# Force re-sync after a failed attempt
libra cloud sync --force

# Restore a repository by name into a fresh directory
libra init
libra cloud restore --name my-project

# Preview what would be restored without downloading objects
libra cloud restore --name my-project --metadata-only
```

## Human Output

**`cloud sync`** (with objects to sync):

```text
Starting cloud sync...
Found 42 objects to sync.
Progress: 42/42 synced, 0 failed
Objects: 42 synced, 0 failed
Syncing metadata...
Metadata synced (3 references).
Syncing agent capture catalog to D1...
Agent capture sync: 2 sessions (0 failed), 6 checkpoints (0 failed), 3 subagent companion rows (0 failed).
Sync complete.
```

**`cloud sync`** (nothing to sync):

```text
Starting cloud sync...
No objects to sync.
Syncing metadata...
Metadata unchanged, skipping upload.
Syncing agent capture catalog to D1...
Agent capture sync: 0 sessions (0 failed), 0 checkpoints (0 failed), 0 subagent companion rows (0 failed).
Sync complete.
```

**`cloud restore`**:

```text
Starting restore for repo: a1b2c3d4-e5f6-7890-abcd-ef1234567890
Found 42 objects in cloud for repo.
Restored 42 object indexes to local database.
Restore complete: 38 downloaded, 4 skipped (already exist), 0 failed
Restoring metadata...
Metadata restored.
Restoring working directory to HEAD (abc1234)
Successfully restored working directory files.
```

**`cloud restore --metadata-only`**:

```text
Starting restore for repo: a1b2c3d4-e5f6-7890-abcd-ef1234567890
Found 42 objects in cloud for repo.
Restored 42 object indexes to local database.
Metadata-only restore complete.
```

**`cloud status`**:

```text
Cloud Sync Status:
  Repo ID:       a1b2c3d4-e5f6-7890-abcd-ef1234567890
  Total objects: 42
  Synced:        40 (95%)
  Pending:       2

By object type:
  blob: 30/32 synced
  tree: 8/8 synced
  commit: 2/2 synced
```

**`cloud status --verbose`**:

```text
Cloud Sync Status:
  Repo ID:       a1b2c3d4-e5f6-7890-abcd-ef1234567890
  Total objects: 42
  Synced:        40 (95%)
  Pending:       2

By object type:
  blob: 30/32 synced
  tree: 8/8 synced
  commit: 2/2 synced

Unsynced objects:
  abc123def456... (blob, 1024 bytes)
  789012abc345... (blob, 512 bytes)
```

## Structured Output

`--json` and `--machine` are supported for `cloud status` and `cloud sync`.
`--json` emits a command envelope and `--machine` emits the same envelope as a
single NDJSON line.

```json
{
  "ok": true,
  "command": "cloud.status",
  "data": {
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "total_objects": 42,
    "synced": 40,
    "pending": 2,
    "synced_percent": 95,
    "by_type": [
      {
        "object_type": "blob",
        "total": 32,
        "synced": 30,
        "pending": 2
      }
    ]
  }
}
```

When `--verbose` is set, the status payload also includes up to 20
`unsynced_objects` entries with `oid`, `object_type`, and `size`.

`cloud sync --json` / `--machine` emits `cloud.sync` on successful sync runs:

```json
{
  "ok": true,
  "command": "cloud.sync",
  "data": {
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "project_name": "my-project",
    "total_unsynced": 42,
    "synced_count": 42,
    "failed_count": 0,
    "metadata": {
      "status": "synced",
      "references": 3
    },
    "agent_capture": {
      "status": "completed",
      "sessions_synced": 2,
      "sessions_failed": 0,
      "checkpoints_synced": 6,
      "checkpoints_failed": 0
    }
  }
}
```

`data.warnings` (an array of strings) appears only when the run left something to report: its own snapshot spool directory that could not be removed, and each leftover run directory of an earlier sync that was kept (owner not proven gone), not inspected (not private, or a link), or could not be removed.

`cloud restore --json` / `--machine` emits `cloud.restore` on successful restore runs:

```json
{
  "ok": true,
  "command": "cloud.restore",
  "data": {
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "metadata_only": false,
    "total_objects": 42,
    "indexes_restored": 42,
    "object_restore": {
      "downloaded": 30,
      "skipped": 12,
      "failed": 0
    },
    "metadata": {
      "status": "restored"
    },
    "agent_capture": {
      "status": "restored"
    }
  }
}
```

For `cloud restore --metadata-only`, the payload keeps `metadata_only: true`
and omits `object_restore`.

`cloud sync --progress=json` emits NDJSON progress events to stderr (no legacy
human progress text on stdout). Event names cover object, metadata, and
agent-capture phases, for example:

```json
{"event":"cloud_sync.start"}
{"event":"cloud_sync.objects.total","total":42}
{"event":"cloud_sync.objects.progress","synced":42,"total":42,"failed":0}
{"event":"cloud_sync.objects.finished","synced":42,"failed":0}
{"event":"cloud_sync.metadata.synced","references":3}
{"event":"cloud_sync.agent_capture.finished","sessions_synced":2,"sessions_failed":0,"checkpoints_synced":6,"checkpoints_failed":0,"subagent_rows_synced":3,"subagent_rows_failed":0}
{"event":"cloud_sync.complete"}
```

Only `cloud_sync.complete` — and, in human output, the final `Sync complete.`
line — announces a successful sync, and only after the objects, the metadata
and the agent-capture mirror have all succeeded; a sync that fails anywhere
never prints either, and its phase events say `finished` with their counts.
Earlier releases emitted `cloud_sync.objects.complete` and
`cloud_sync.agent_capture.complete` and printed
`Sync complete: N synced, M failed` at the end of the object phase, before the
metadata and the mirror ran.

An agent-capture row failure is reported as
`{"event":"cloud_sync.agent_capture.warning","error":"agent-capture catalog sync failed; inspect the local capture catalog and retry `libra cloud sync`"}`:
the message is fixed and carries no session or checkpoint id and no remote D1
text (earlier releases emitted per-id `cloud_sync.agent_capture.session_warning`
/ `checkpoint_warning` events that echoed them). Any other agent-capture
failure is also reported as exactly one such event, whose `error` is that
failure's fixed, content-free reason — for example a D1 request failure with
its D1 code and a connectivity remedy, or the specific rejected local catalog
shape. Every agent-capture failure then ends with the `LBR-CONFLICT-002` error
`agent capture mirror failed: <reason>` carrying the same reason.

The completed counts report only rows actually sent by that incremental sync;
the subagent companion count covers durable source claims, append-only source
revisions, and boundary/content link rows. The agent catalog is frozen with
the rest of the sync's snapshot, before any remote write: its rows (sessions,
checkpoints, prune and import tombstones, the traces head, subagent claims,
revisions and links, and legacy Claude mirror rows) are read one short statement
per page on the snapshot's own connection, inside the same unchanged-database
window as the object identities, refs and project name, and written as the
cloud projection (fields the cloud never receives are left out) to an
owner-only `catalog.json` in the run directory. A commit by another writer
during that read makes the snapshot be taken again, and no lock is held between
statements, so hook and import commits are never blocked; a writer whose
transaction has not committed is simply not in the snapshot. The frozen catalog
is published only when every check passes against the current database after
the object upload: no object anywhere in the repository may still be unsynced
(an object added after the snapshot and not yet uploaded refuses publication
too), the catalog shape, checkpoint reachability and durability from the
traces head, every required object in the completed upload generation, the
restore row budget and the remote object checks, and last, right before the
first remote catalog change, a re-read of the current catalog — taken in one
unchanged-database window, without holding a lock across pages — that must be
identical to the frozen one, followed by the unsynced-object check once more. A
session erased, a tombstone written or any catalog row changed
after the snapshot therefore refuses publication with a retry hint, so an
older snapshot never brings erased or revoked data back; a refusal writes no
remote catalog rows. On the ordinary path, the capture phase
queries object-index rows only for checkpoint-reachable OIDs, so unrelated
large repository history does not consume the 100,000-row capture safety bound.
Remote rows are read in bounded pages and
only missing/strictly newer generations are sent in bounded multi-row requests.
Session, checkpoint, mutable claim, and link ordering uses explicit transactionally incremented sync
revisions, not wall-clock timestamps. Because those counters are clone-local,
Libra also records the exact completed remote generation from the last successful
sync or restore; a changed existing row can advance only from that known ancestor,
otherwise sync fails closed and asks the user to restore first. The transition to
`publishing` uses a compare-and-swap on that same generation. An active writer
retains its fence; if it crashes, the server-timestamped publication lease
expires after five minutes and a later sync can atomically take over. Checkpoint generations advance when
pruning rewrites a retained traces/tree identity or verified doctor repair
corrects its tree/metadata/traces OIDs; immutable checkpoint fields
must still match, so a stale clone cannot restore the pre-prune row. A remote generation manifest is marked
`publishing` only after all remote conflict and object-durability preflight
succeeds and before the first remote capture-catalog mutation; every batch is fenced by its unique
writer token, and the manifest becomes `complete` only after the full companion
graph is re-read and validated. The manifest binds the fenced traces head, the
object-index catalog generation, and the canonical digest/count
of the **checkpoint-reachable object-index projection**; unrelated large Git
history is neither downloaded nor counted by the capture phase, while required
OIDs are queried in bounded batches and their R2 payloads are content-hashed in
fixed 32-object concurrency pages
before completion; missing or corrupt payloads are replaced from validated
local objects and read back before the manifest can complete. An interrupted run is therefore safely resumed
or taken over by the next sync. Immutable revision conflicts and
equal-generation checkpoint/claim/link divergence fail closed; revision and link
dependencies publish before the monotonic claim high-water. Every D1 request
has a 30-second timeout and the complete agent-capture phase has a 120-second
deadline. Any agent-capture mirror failure makes `cloud sync` exit nonzero and
machine output emits no preceding success envelope.

Repositories upgraded from earlier releases keep syncing and restoring. Legacy
(unversioned or V1) import-ownership fields are omitted from every session row
that sync mirrors or restore writes, while all other session metadata is kept
unchanged; rows earlier releases already mirrored are left as they are and
remain restorable. Legacy V1 `source/sha256/...` subagent claims and revisions
that the remote catalog does not already hold, together with the association
links of their content checkpoints, stay local-only because their unkeyed
source digest must not be added to new cloud catalog rows; their checkpoints
are still mirrored, and the immutable checkpoint metadata blob that restore
needs still carries that digest. While such evidence exists, each successful
sync also emits one
fixed warning,
`{"event":"cloud_sync.agent_capture.warning","error":"legacy V1 subagent evidence keyed by an unkeyed source digest stays local-only and was not mirrored; its checkpoints and all other agent-capture rows were synced"}`,
and still exits 0.

Ordinary `agent clean` checkpoint retention records a durable prune tombstone in
the same local transaction as the ref/catalog rewrite. Cloud sync publishes that
fence before removing a current claim, its revision, link, and checkpoint in
that resumable order; every interruption boundary remains a valid publishing
state, and a rewound local claim is recreated by the later monotonic claim batch.
D1 rejects later checkpoint writes for the tombstoned identity, so a stale clone
cannot resurrect it. Session erasure deliberately does not create this ordinary
prune fence. Restore checks local ordinary-prune tombstones against one stable
completed remote generation **before downloading objects or applying generic
refs metadata**; generic metadata defers the `traces` ref while capture
ownership is being validated. A completed generation installs its fenced ref;
when no generation or capture rows exist, restore applies the deferred legacy
metadata ref so pre-manifest traces remain reachable. Unmanifested capture rows
fail with a current-version sync instruction instead of selecting ambiguous
ownership. If the last completed remote generation still contains one of those
checkpoints, restore fails and asks the user to run cloud sync first instead of
resurrecting its objects, ref, catalog row, or companion rows. If D1 instead
retains an unmarked checkpoint absent locally because of session erasure,
sync fails before starting a new capture generation and preserves the previous
completed remote snapshot, matching the documented deferred cross-device erase
semantics.

Sync refuses to advertise a generation that restore could not read: one
100,000-row aggregate safety budget covers sessions, checkpoints, prune
tombstones, claims, revisions, links, and the fenced object-index projection.
Restore applies the same aggregate bound to the rows it consumes;
capture-scoped full object-index reads use keyset pagination and a
trigger-maintained catalog generation. Ordinary full-repository object restore
remains paginated but is not subject to the capture catalog's 100,000-row cap.
Exceeding the shared capture bound fails with an actionable error instead
of retaining unbounded remote input in memory. Any insert, update, or delete during paging invalidates the read and
retries it up to three times, preventing duplicate or omitted page-boundary
rows. During one-time v2 adoption, checkpoint rows left orphaned by the legacy
best-effort mirror are discarded before strict dependency validation; a current
local session/checkpoint pair can then publish coherently.

Restore reads the same completed manifest before and after all bounded pages
(retrying a changing generation up to three times), prevalidates the whole
companion generation, and applies sessions,
checkpoints, skeleton claims, revisions, links, and final claim advancement in
one local SQLite transaction. A conflict or FK/write error leaves the previous
local generation unchanged; newer local session/checkpoint/link generations are never
regressed; a newer checkpoint generation may update only the
traces/tree/metadata fields that pruning or verified doctor repair rewrites. An
empty checkpoint catalog is valid only with an empty fenced traces head; restore
rejects a crash-window head that has no matching catalog row.
Legacy remote capture rows
without a completed manifest must first be adopted by running `libra cloud sync`
with the current Libra version. A legacy remote with no capture rows remains a
valid Git-only backup and restore skips this optional layer. Restore only probes
remote capture schema; it never installs writer barriers or adopts rows, so a
failed/read-only restore cannot disable older writers. Current clients use versioned v2 remote
session/checkpoint tables; D1 installs legacy write barriers before taking the
one-time generation-0 adoption snapshot, so an old client cannot race a change
between the copy and its completion marker. Unfenced single-row v2 writes also
fail closed; current publication uses only writer-token-fenced batches. Restore also
reads the object index through a generation-stable snapshot and rejects catalog
state whose fenced projection no longer matches the manifest digest. Later
unrelated object-index additions do not invalidate an otherwise unchanged
checkpoint projection. Any agent-capture restore failure exits with
`LBR-NET-002` and `agent capture restore failed: <reason>`; the fixed reason
keeps its specific remedy (publish a prune tombstone with `libra cloud sync`,
retry when cloud sync is idle, the 120-second deadline, or verify cloud
connectivity and credentials) and never echoes remote rows or D1 text.
It validates reachability from the manifest-bound traces head and restores that
head atomically with the catalog rather than trusting separately uploaded generic
reference metadata. Existing local objects are
decoded and hash-checked; corrupt paths are re-downloaded atomically, and the
complete traces chain plus every checkpoint tree/blob is validated before the
catalog transaction starts. Session-erasure tombstones now propagate: `cloud sync`
publishes the local `agent_import_tombstone` rows to D1 under the same
generation fence and cascade-deletes the erased session's mirror rows, and
restore is tombstone-first — an erased session never restores, its fences are
persisted locally on the restoring machine, and repeated deletes/restores are
idempotent. R2 payload bytes are not physically deleted yet (the only remaining
deferral). An explicit local `--restore-erased` import still preserves a new
replication incarnation, so its session generation and child-source namespace
cannot collide with older retained rows.

`cloud sync` default mode still uses the legacy human progress output.
`cloud restore` and `cloud sync` failures continue through Libra's standard CLI
error machinery.

Before any cloud operation reads or changes the local `object_index`, CLI
preflight replays the bounded,
atomic repair markers retained by earlier object-writing commands whose
background index update failed. A marker is removed only after its exact row is
inserted or reconciled. Replay opens the repository database once and enumerates
at most 100,000 raw repair-directory entries per invocation using bounded
multi-row upserts. If more remain, that invocation makes durable progress but
cloud commands fail closed; later repository commands continue with the next
page. Replay and queued writers hold the same process-crash-safe ownership lock
from a bounded 65,536-shard OID namespace through the row update and marker
retirement. A delayed writer
therefore skips a marker already retired by replay instead of recreating a row
after destructive cleanup; a marker concurrently retired before replay opens it
is treated as completed work. Canonical final marker filenames and contents are
validated strictly. Current atomic writes use a sibling staging directory;
bounded replay removes legacy `.tmp*` remnants from the marker directory so
they cannot consume every page and permanently hide real repair work. Each
bounded staging scan examines at most 1,024 entries and removes at most 256
regular temp files older than 24 hours, preserving active writers while
eventually reclaiming crash remnants. Marker OID length must match the
repository's configured SHA-1 or SHA-256 object format. Because
the marker is created only after a successful
configured-backend object write, it remains sufficient provenance when a tiered
backend has evicted the local cache copy and the payload exists only remotely;
sync still reads and validates the payload before upload. The cloud operation
fails closed with `LBR-IO-002` before credentials,
uploads, progress success, or a JSON success envelope if a marker is malformed,
another page remains, or its database update still fails; therefore `--force` is not a substitute for
local catalog repair.

Background queue accounting is invocation-local: a concurrent direct
`ClientStorage` caller neither extends a CLI command's drain wait nor causes
that command to emit a warning or exit 9. Invocation-owned and direct-library
updates also use separate bounded FIFO lanes, so an older direct backlog cannot
delay cloud preflight behind the command's finite drain budget. Marker
publication is serialized with destructive cleanup by a repository-wide
generation fence; cleanup holds the fence from exact candidate revalidation
through its transaction commit. With `--sync-data`, marker retirement fsyncs
the containing directory.

## Environment Variables

Cloud operations require the following keys. Libra reads repo-local `vault.env.*`
entries first, then global `vault.env.*`, then the matching environment
variables. If all layers are missing for a required key, the command reports the
key and asks you to configure it before retrying.

### D1 (required for all operations)

| Key | Description |
|-----|-------------|
| `LIBRA_D1_ACCOUNT_ID` | Cloudflare account ID |
| `LIBRA_D1_API_TOKEN` | Cloudflare API token with D1 access |
| `LIBRA_D1_DATABASE_ID` | D1 database UUID |

### R2 (required for sync and full restore)

| Key | Description |
|-----|-------------|
| `LIBRA_STORAGE_ENDPOINT` | S3-compatible endpoint URL |
| `LIBRA_STORAGE_BUCKET` | Bucket name |
| `LIBRA_STORAGE_ACCESS_KEY` | Access key ID |
| `LIBRA_STORAGE_SECRET_KEY` | Secret access key |
| `LIBRA_STORAGE_REGION` | Region (defaults to `auto`) |

Note: When `--metadata-only` is used with `restore`, only D1 variables are required.

## Design Rationale

### Why D1/R2 specifically?

Libra targets Cloudflare's ecosystem for several reasons. D1 provides serverless SQLite, which aligns with Libra's local SQLite-based architecture: the same query patterns and data model work both locally and in the cloud. R2 provides S3-compatible object storage with no egress fees, which is critical for a VCS where objects are frequently downloaded. The combination provides a fully serverless backup backend with no infrastructure to manage.

### Why not generic cloud storage?

Libra already has generic S3-compatible storage support via `LIBRA_STORAGE_*` environment variables for tiered object caching. The `cloud` command serves a different purpose: full repository backup including metadata (references, HEAD, config). This requires a structured database (D1) for the object index, not just a blob store. A generic backend would require implementing a metadata layer on top of every storage provider, which adds complexity without clear benefit. Users who need backup to other providers can use the object-level storage tiering instead.

### Why a `batch-size` parameter?

Object sync involves uploading to R2 and then indexing in D1 for each object. For large repositories with thousands of objects, this can take significant time. The `--batch-size` parameter controls how many objects are processed before a progress report is printed. Smaller batches give more responsive feedback; larger batches reduce per-batch overhead. The default of 50 balances these concerns. A batch size of 1 is allowed for maximum granularity during debugging.

### Why `--repo-id` and `--name` as mutually exclusive options?

Repository UUIDs are stable and unambiguous but not human-friendly. Project names are human-friendly but can conflict or be renamed. Making them mutually exclusive with one required ensures the user explicitly chooses their lookup strategy. The UUID is stored in local config (`libra.repoid`) and is authoritative; the name is a convenience alias stored in D1's `repositories` table.

### Why does restore attempt to populate the working directory?

A bare object restore (indexes + objects) leaves the repository in a state where files exist in the object store but the working directory is empty. For most users, the goal of restore is to get back to a working state. Libra automatically checks out HEAD (or the `main` branch as fallback) after restoring objects. This matches user expectations and avoids an extra manual step. The `--metadata-only` flag skips this for users who only need the index.

## Parameter Comparison: Libra vs Git vs jj

| Operation | Libra | Git | jj |
|-----------|-------|-----|----|
| Sync to cloud | `cloud sync` | N/A (use `push` to remote) | N/A (use `push` to remote) |
| Force sync | `cloud sync --force` | N/A | N/A |
| Batch size | `cloud sync --batch-size <N>` | N/A | N/A |
| Restore from cloud | `cloud restore --name <N>` | `clone <url>` | `git clone <url>` |
| Restore by ID | `cloud restore --repo-id <ID>` | N/A | N/A |
| Metadata-only restore | `cloud restore --metadata-only` | N/A | N/A |
| Sync status | `cloud status` | N/A | N/A |
| Verbose status | `cloud status --verbose` | N/A | N/A |
| Backend | Cloudflare D1 + R2 | Git remotes (SSH/HTTPS) | Git remotes (SSH/HTTPS) |
| Incremental sync | Automatic (is_synced flag) | Automatic (pack negotiation) | Automatic (via Git) |
| Object verification | Hash check on restore | Hash check on transfer | Hash check on transfer |
| Metadata backup | Automatic (references JSON) | Included in push/fetch | Included in push/fetch |

Note: Neither Git nor jj have a built-in cloud backup command. They rely on pushing to remote repositories for backup and collaboration. Libra's `cloud` command fills a different niche: backing up the full repository state (including local branches, config, and object index) to a serverless cloud backend without requiring a Git server.

## Error Handling

| Code | Condition |
|------|-----------|
| `LBR-REPO-001` | Not a libra repository |
| `LBR-CLI-002` | Missing required Vault/env credential keys (lists which ones) |
| `LBR-CLI-002` | Batch size must be at least 1 |
| `LBR-CLI-002` | Neither `--repo-id` nor `--name` provided for restore |
| `LBR-CLI-003` | Repository with given name not found in D1 |
| `LBR-CONFLICT-002` | Project name already taken by another repository |
| `LBR-CONFLICT-002` | Agent-capture mirror failed during sync (`agent capture mirror failed: <reason>`) |
| `LBR-CONFLICT-002` | A local capture session uses a kind outside the cloud capture allowlist (for example `hermes`); refused before credentials, D1, or R2, also with `--force`, until a release adds cloud support for that kind (back up the whole repository with an external tool meanwhile) |
| `LBR-CONFLICT-002` | An existing D1 capture table's `agent_kind` CHECK rejects a local kind or cannot be read; refused before any remote write (create a new D1 database and run `libra cloud sync --force`) |
| `LBR-NET-002` | Agent-capture restore failed (`agent capture restore failed: <reason>`) |
| `LBR-IO-001` | D1 client initialization failure |
| `LBR-IO-001` | Failed to create D1 tables |
| `LBR-IO-001` | Database query failure |
| `LBR-IO-002` | R2 upload failure |
| `LBR-IO-002` | R2 download failure |
| `LBR-IO-002` | Hash mismatch on restored object |
| `LBR-IO-002` | Failed to save restored object to local storage |
| `LBR-IO-002` | Metadata sync/restore failure |
| `LBR-IO-002` | Durable local object-index repair marker could not be replayed before a cloud operation; when the cause is a lock timeout, the message names the lock holder (pid and purpose) or says the holder could not be determined — wait for that process and retry (lock waits use a Git-style quadratic backoff and give up after 10 seconds), and never delete lock files under `.libra/object-index-repair-locks`. Read-only commands skip the replay silently while the lock is busy and retry on the next command; `cloud sync` keeps the blocking wait and fails closed |
