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

The sync workflow tracks which objects have been uploaded via an `is_synced` flag in the local `object_index` table. Before selecting work, sync reconciles the local `.libra/objects` store into `object_index` so older loose or packed objects are not skipped. On each default sync, objects are selected when they are locally unsynced or missing from D1, making repeated syncs efficient while still repairing stale local sync flags after a D1 database change. A `--force` flag allows re-syncing all indexed local objects and is the recovery path for R2 bucket-side data loss. After objects are synced, repository metadata (references/branches) is serialized to JSON and uploaded to R2, with a content hash check to avoid unnecessary uploads.

Each repository is identified by a UUID (`libra.repoid` config key) and optionally a human-readable project name (`cloud.name` config key or directory name). The project name is registered in a D1 `repositories` table for lookup during restore.

Restore can target a repository by UUID (`--repo-id`) or project name (`--name`). It downloads the object index from D1, optionally downloads objects from R2, restores metadata (references), and populates the working directory from HEAD.

## Global Config Schema Guard

`libra cloud` reads the global storage configuration (`~/.libra/config.db`, or
`LIBRA_CONFIG_GLOBAL_DB`) before trusting remote/tiered object storage settings. If that
database has a schema version newer than this binary supports, cloud commands fail closed
with `LBR-CONFIG-001` instead of silently ignoring global storage config and falling back
to local objects. The diagnostic includes the binary path and version, config DB path,
schema versions, and the update command:
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`.

Use `libra --offline cloud ...` or `LIBRA_READ_POLICY=offline|local libra cloud ...` only when
you intentionally want local-only object access. Libra will warn once and ignore the
global storage config for that run.

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
Sync complete: 42 synced, 0 failed
Syncing metadata...
Metadata synced (3 references).
```

**`cloud sync`** (nothing to sync):

```text
Starting cloud sync...
No objects to sync.
Syncing metadata...
Metadata unchanged, skipping upload.
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
{"event":"cloud_sync.metadata.synced","references":3}
{"event":"cloud_sync.agent_capture.complete","sessions_synced":2,"sessions_failed":0,"checkpoints_synced":6,"checkpoints_failed":0}
```

`cloud sync` default mode still uses the legacy human progress output.
`cloud restore` and `cloud sync` failures continue through Libra's standard CLI
error machinery.

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
| `LBR-IO-001` | D1 client initialization failure |
| `LBR-IO-001` | Failed to create D1 tables |
| `LBR-IO-001` | Database query failure |
| `LBR-IO-002` | R2 upload failure |
| `LBR-IO-002` | R2 download failure |
| `LBR-IO-002` | Hash mismatch on restored object |
| `LBR-IO-002` | Failed to save restored object to local storage |
| `LBR-IO-002` | Metadata sync/restore failure |
