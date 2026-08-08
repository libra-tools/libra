# `libra publish`

Prepare Libra's read-only Cloudflare Worker publish surface.

Current implementation status:

- `libra publish init` materialises the embedded Worker template under
  `worker/` and records `.libra/publish/worker-template-manifest.json`.
  The manifest lives in the repository's COMMON storage (the shared
  `.libra`, resolved through the `commondir` chain), so `init`, `status`,
  `deploy`, and `unpublish` read and write the same manifest from every
  worktree; the `worker/` directory itself is tracked working-tree content
  and stays per-worktree. The reported `manifest_path` is worktree-relative
  when the manifest sits under the invoking worktree (the main worktree)
  and an absolute path otherwise (a linked worktree).
- `libra publish status` reports the local Worker template state as
  `missing`, `current`, `modified`, `outdated`, or `conflicted`, and
  can compare local branch/tag refs with D1 `publish_refs` when a site
  id is configured.
- `libra publish sync --dry-run` scans local branch/tag refs, validates
  `--ref`, reports dirty-tree warnings, and emits the local publish
  plan without Cloudflare credentials.
- `libra publish sync` writes code snapshot and AI artifacts to R2,
  upserts `publish_sync_runs`, `publish_revisions`, `publish_files`,
  `publish_ai_objects`, `publish_ai_versions`, and `publish_refs` in
  D1, and advances `publish_sites.latest_revision_oid` only for a full
  all-refs sync. The built-in AI export planner reads local AI history,
  emits redacted snapshot/event objects, and adds projection objects
  for the publish AI index, graph, and bundle.
- `libra publish deploy` validates the local Worker template, requires
  the generated Worker config/bindings, runs `pnpm build`, and, unless
  `--skip-deploy` is set, applies D1 migrations and deploys the Worker
  through Wrangler/OpenNext.
- `libra publish unpublish --yes` disables a published site by setting
  `publish_sites.status = 'disabled'` through Wrangler D1 execute. The
  Worker already returns HTTP 410 for disabled sites.
- Worker API route tests cover private-site 403, disabled-site 410, and
  typed 404 envelopes for missing D1 file rows or missing R2 content.
- The Worker project uses `wrangler types --env-interface CloudflareEnv
  cloudflare-env.d.ts` as the binding type source. The committed
  `env.d.ts` only augments generated types with optional Cloudflare
  Access secret names.
- The Worker `build` script runs `cf-typegen` and OpenNext; OpenNext is
  configured to call `pnpm next:build` internally so `pnpm build` does
  not recursively invoke itself.
- The Worker e2e runner starts `next dev` with local fixture D1/R2
  bindings when `BASE_URL` is unset, and runs desktop plus mobile
  Chromium assertions for the publish landing page, code browser, file
  viewer, AI model page, refs, status, and empty/non-text states.
- `libra clone libra+cloud://<clone-domain>/<slug>` restores published
  Git objects, refs metadata, and publish AI index/graph/bundle/object
  envelopes from D1/R2 into a local Libra repository.
- The remaining live-only publish gate is tracked in
  `docs/development/commands/publish.md`; it requires a real all-refs sync,
  cloud clone restore, deployed Worker refs/tree/file API smoke, and
  Cloudflare credentials with deploy permissions.

## Synopsis

```
libra publish init      [OPTIONS]
libra publish sync      [OPTIONS]
libra publish status    [OPTIONS]
libra publish deploy    [OPTIONS]
libra publish unpublish [OPTIONS]
```

## Description

`libra publish` is the outward-facing counterpart to `libra cloud`.
The shipped slices cover local Worker-template initialisation and
status, offline sync dry-runs, cloud snapshot and AI artifact upload,
cloud ref status comparison, Worker build/deploy/unpublish
orchestration, and `libra+cloud://` clone restore for the published
read-only snapshot surface.

## Subcommands

### `libra publish init`

```
libra publish init \
    [--slug <slug>] \
    [--clone-domain <clone-domain>] \
    [--display-origin <origin>] \
    [--name <human-name>] \
    [--visibility public|private] \
    [--worker-name <name>] \
    [--max-preview-bytes <bytes>]
```

- Confirms the current directory is a Libra repo.
- Reuses the current repository root as the template target.
- Writes `.libra/publish/worker-template-manifest.json` with the
  embedded template version, render policy, and SHA-256 baseline for
  each managed file.
- Accepts the site-shaping flags listed above for forward
  compatibility. The current implementation does not persist those
  values to repository config; it only uses the CLI parser to validate
  flag shape.
- `--max-preview-bytes <bytes>`: must be `> 0`; the CLI rejects `0`
  before the template is written.
- Materialises `worker/` from the embedded Worker template. Missing
  files are written fresh, byte-identical template files are left as
  current, and user-modified or symlinked paths fail closed with
  `LBR-CONFLICT-002`; no conflict markers are written.
- Does **not** require Cloudflare connectivity.

### `libra publish sync`

```
libra publish sync [--ref <branch|tag|full-ref>]
                   [--dry-run]
                   [--fail-on-dirty]
                   [--ai-redaction default|strict]
                   [--allow-sensitive-path <path>]…
                   [--force]
                   [--json]
```

Current behavior:

- `--dry-run` scans local `refs/heads/*` and `refs/tags/*`, dedupes by
  revision oid, counts files in each unique commit tree, and emits a
  plan. It does not read or write Cloudflare D1/R2 and does not require
  Cloudflare credentials.
- Dry-run loads each planned revision's committed `.librapublishignore`
  and applies the built-in publish deny rules. Denied paths are reported
  as warnings with `builtin_credential` or `user_ignore` reasons.
- `--ref <branch|tag|full-ref>` filters the dry-run to one branch or
  tag. If a short name exists as both a branch and a tag, the command
  fails with `LBR-CLI-003` and asks for `refs/heads/<name>` or
  `refs/tags/<name>`.
- Without `--dry-run`, the command requires `publish.site_id` plus
  `LIBRA_D1_ACCOUNT_ID`, `LIBRA_D1_API_TOKEN`, `LIBRA_D1_DATABASE_ID`,
  `LIBRA_STORAGE_ENDPOINT`, `LIBRA_STORAGE_BUCKET`,
  `LIBRA_STORAGE_ACCESS_KEY`, and `LIBRA_STORAGE_SECRET_KEY`. Libra reads
  repo-local `vault.env.*` first, then global `vault.env.*`, then exported
  environment variables. It loads the matching `publish_sites` row from D1
  for `repo_id`, visibility, max preview bytes, and `refs_generation`.
- Full sync writes one code snapshot per unique local branch/tag
  revision, uploads text previews and `code-manifest.json` to R2, writes
  binary, too-large, and ignored files as D1 metadata only, uploads
  `refs.json` and `latest.json`, and advances `publish_sites` through a
  refs-generation CAS. After that CAS succeeds, stale `publish_refs`
  rows from older sync runs for the same site are deleted.
- Repeated sync skips existing revision `code-manifest.json`, text
  preview objects, `ai/index.json`, AI object JSON, AI graph, and AI
  bundle objects unless `--force` is passed.
- `--ref` on non-dry-run sync writes only the selected ref and its
  revision snapshot. It does not upload `refs.json`/`latest.json` and
  does not advance the complete refs generation.
- Dirty worktrees emit a warning because sync plans committed refs
  only. `--fail-on-dirty` converts that condition into
  `LBR-REPO-003`.
- `--json` returns `siteId`, `refsCount`, `revisionCount`,
  `defaultRef`, `latestRevisionOid`, `fileCount`, `aiObjectCount`,
  `aiBundleCount`, `warnings`, and the selected ref/revision details.
  During dry-run `siteId` is `null`; each revision entry also includes
  `preflightDeniedCount`.

### `libra publish status`

```
libra publish status [--site-id <uuid>] [--json]
```

Current behavior: this subcommand always inspects the local Worker
template and manifest. If `--site-id <uuid>` is passed, or
`publish.site_id` exists in repository config, it also reads D1
`publish_refs` and compares published branch/tag refs with local
`refs/heads/*` and `refs/tags/*`.

The status is:

- `missing`: the manifest or one or more embedded template files are
  absent.
- `current`: every embedded template file matches the current Libra
  template and the manifest exists.
- `modified`: a managed template file differs from both the current
  embedded template and the manifest baseline.
- `outdated`: a managed template file still matches the manifest
  baseline, but Libra embeds a newer template version.
- `conflicted`: the `worker/` root or a managed template path is a
  symlink or non-file path.

`--json` returns counts for total, current, missing, modified,
outdated, and conflicted files. It also includes `publishedRefs`. When
no site id is available, `publishedRefs.state` is `unconfigured`. When
comparison runs, `publishedRefs.state` is `compared` and the object
contains matching, changed, local-only, and published-only ref counts
plus the affected ref rows. The same object reports
`snapshotIssueCount`, `snapshotMissingCount`, `snapshotUnpublishedCount`,
and `snapshotIssues` when a D1 `publish_refs` row points at a missing or
non-`published` `publish_revisions` snapshot.

D1 comparison requires `LIBRA_D1_ACCOUNT_ID`, `LIBRA_D1_API_TOKEN`, and
`LIBRA_D1_DATABASE_ID` using the same env/vault resolution as
`libra cloud`. Missing or unreachable D1 configuration fails the
command instead of silently reporting stale publish state.

### `libra publish deploy`

```
libra publish deploy [--skip-deploy]
```

Current behavior:

- Requires `worker/` and `.libra/publish/worker-template-manifest.json`
  from `libra publish init`.
- Fails before running commands when the template is missing,
  conflicted, outdated, or when `worker/wrangler.jsonc` still contains
  `REPLACE_WITH_D1_DATABASE_ID` or `REPLACE_WITH_R2_BUCKET_NAME`.
- Allows a `modified` template status so user-owned Worker edits can be
  deployed intentionally.
- Runs `pnpm build` from `worker/`.
- Without `--skip-deploy`, runs
  `pnpm exec wrangler d1 migrations apply LIBRA_PUBLISH_DB --remote`
  and then `pnpm exec opennextjs-cloudflare deploy`.
- Parses the deploy output and prints/returns the first deployment URL.
  If deploy succeeds but no URL is present, the command fails so scripts
  do not silently lose the published endpoint.
- With `--skip-deploy`, only the local build runs; D1 migrations and the
  Worker deploy step are skipped. This is the safe CI smoke path when
  Cloudflare credentials are not available.

### `libra publish unpublish`

```
libra publish unpublish --yes [--site-id <uuid>]
```

Current behavior:

- Requires `--yes`; without it the command fails before reading config
  or running cloud commands.
- Uses `--site-id <uuid>` when provided; otherwise reads
  `publish.site_id` from repository config.
- Validates that the site id is a UUID before constructing SQL.
- Requires the local Worker template and configured `worker/wrangler.jsonc`
  for the `LIBRA_PUBLISH_DB` binding.
- Runs `pnpm exec wrangler d1 execute LIBRA_PUBLISH_DB --remote --yes
  --command <UPDATE>` from `worker/`, setting
  `publish_sites.status = 'disabled'` for the selected site.
- Does not delete D1 rows, R2 objects, Worker routes, or Worker
  deployments. The published Worker returns HTTP 410 for disabled sites.

## Configuration

`libra publish init` currently does not write publish keys into
`ConfigKv`. It records only the Worker template manifest described in
the Files section.

The publish commands and cloud clone restore use these repository
configuration keys when they are present:

| Key | Description |
|-----|-------------|
| `publish.site_id` | UUIDv4 minted at init. Stable. |
| `publish.slug` | Human-readable slug; unique within a clone domain. |
| `publish.clone_domain` | Namespace inside which `slug` resolves. |
| `publish.display_origin` | HTTPS origin browsers visit (e.g. `https://code.example.com`). |
| `publish.name` | Display name for the site. |
| `publish.visibility` | `public` or `private`. |
| `publish.worker_name` | Wrangler worker name. |
| `publish.max_preview_bytes` | Per-file preview size cap. |

`libra publish sync`, `libra publish status --site-id`, and
`libra+cloud://` clone restore read Cloudflare account ids, API tokens,
and R2 S3 credentials from the same `LIBRA_D1_*` / `LIBRA_STORAGE_*`
keys that `libra cloud` uses, with `vault.env.*` taking priority over the
process environment. These secrets are never written into the Worker template.

## Files

- `sql/publish/0001_publish.sql` — the D1 schema source of truth.
- `sql/publish/0002_publish_digest_check.sql` — additive trigger
  migration that enforces lowercase 64-char hex on every digest
  column for tenants who already applied 0001. Required because
  SQLite's `CREATE TABLE IF NOT EXISTS` is a no-op when the table
  already exists, so column-level CHECK additions in 0001 never
  reach existing databases.
- `worker/migrations/<NNNN>_*.sql` — byte-equal mirrors of every
  file under `sql/publish/`; `libra publish deploy` applies them with
  `wrangler d1 migrations apply` unless `--skip-deploy` is set. The
  `publish_schema_contract_worker_mirror_is_byte_equal` test walks both
  directories and refuses any drift.
- `worker/` — Next.js + React + OpenNext-for-Cloudflare project. Ships
  embedded in the Libra binary; `libra publish init` materialises it
  in the target repository's root.
- `.libra/publish/worker-template-manifest.json` — local manifest
  recording which template version was rendered and which files the
  user modified.
- `.librapublishignore` — per-repo ignore list applied on top of the
  built-in deny rules.

## D1 schema migrations

The publish D1 schema source already lives under `sql/publish/`, and
each `.sql` file has a byte-equal mirror under `worker/migrations/`
(the `publish_schema_contract_worker_mirror_is_byte_equal` Rust test
walks both directories and refuses any drift). Current
`libra publish deploy` applies these migrations through Wrangler before
the Worker deploy step unless `--skip-deploy` is set.

Current chain:

1. `sql/publish/0001_publish.sql` — initial schema. Tables:
   `publish_sites`, `publish_revisions`, `publish_refs`,
   `publish_files`, `publish_ai_objects`, `publish_ai_versions`,
   `publish_sync_runs`. Adds composite FKs, ref-name shape CHECKs,
   sync-run state-machine CHECK, lowercase-hex digest CHECKs.
2. `sql/publish/0002_publish_digest_check.sql` — additive trigger
   migration. SQLite's `CREATE TABLE IF NOT EXISTS` is a no-op when
   the table already exists, so column-level CHECK additions in
   0001 do not reach existing databases. Migration 0002 adds
   `BEFORE INSERT`/`BEFORE UPDATE` triggers that re-enforce:
   * lowercase 64-char hex shape on `publish_files.content_sha256`,
     `publish_ai_objects.payload_sha256`, and
     `publish_ai_versions.bundle_sha256`;
   * `publish_sites.max_preview_bytes > 0` (the 0001 column-level
     CHECK only enforces `>= 0`; the 0002 trigger pins the
     stricter publish-semantic invariant on existing tenants and
     fires on row-level UPDATE so statements that omit the
     column from the SET list still re-validate).

   All triggers are idempotent (`CREATE TRIGGER IF NOT EXISTS`)
   and safe to re-apply.

Future schema changes go into `0003_<topic>.sql`, etc. Each
migration MUST be additive (`CREATE TABLE … IF NOT EXISTS`,
`ALTER TABLE … ADD COLUMN …`, `CREATE INDEX … IF NOT EXISTS`,
`CREATE TRIGGER … IF NOT EXISTS`) so reapplying on a fresh shard
yields the same end state as applying the chain incrementally.

## Live Gate

The live publish gate is opt-in because it writes to real Cloudflare
D1/R2 resources and, when deploy-smoke variables are present, probes a
deployed Worker:

```bash
LIBRA_ENABLE_TEST_LIVE_CLOUD=1 \
cargo test --features test-live-cloud publish_live -- --test-threads=1
```

Required for the D1/R2 prerequisite portion, either as `vault.env.*` config
entries, exported environment variables, or key/value lines in the repository
`.env.test` file:

- `LIBRA_D1_ACCOUNT_ID`
- `LIBRA_D1_API_TOKEN`
- `LIBRA_D1_DATABASE_ID`
- `LIBRA_STORAGE_ENDPOINT`
- `LIBRA_STORAGE_BUCKET`
- `LIBRA_STORAGE_ACCESS_KEY`
- `LIBRA_STORAGE_SECRET_KEY`

The deployed Worker refs/tree/file API smoke additionally requires
`LIBRA_PUBLISH_LIVE_WORKER_ORIGIN`, pointing at a Worker deployed with
the same D1/R2 bindings. By default the live gate seeds a fresh slug in
that Worker's host namespace and probes it. Set
`LIBRA_PUBLISH_LIVE_CLONE_DOMAIN` when the Worker host differs from
the D1 `clone_domain`, set `LIBRA_PUBLISH_LIVE_SLUG` only when probing
a pre-existing deployed site, and set `LIBRA_PUBLISH_LIVE_FILE_PATH`
when the root tree has no direct file entry to probe.

## Examples

```bash
# Materialise the local Worker template scaffold
libra publish init --slug my-site --clone-domain code.example.com

# Inspect local Worker template / D1 ref drift
libra publish status

# Inspect a specific published site by UUID
libra publish status --site-id <uuid>

# Plan the publish without writing to D1/R2
libra publish sync --dry-run

# Sync default refs to D1/R2
libra publish sync

# Sync a single named ref
libra publish sync --ref refs/heads/main

# Re-upload every file/object regardless of CAS revision
libra publish sync --force

# Allow a path the deny list normally blocks (private sites)
libra publish sync --allow-sensitive-path docs/private.md

# Build the Worker and deploy to Cloudflare
libra publish deploy

# Build only; skip Cloudflare mutation
libra publish deploy --skip-deploy

# Disable a published site without deleting D1/R2 data
libra publish unpublish --site-id <uuid> --yes

# Structured JSON envelope for agents
libra publish --json sync --dry-run
```

The same banner is rendered by `libra publish --help` so the doc and the
CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout, see
`docs/development/commands/_general.md` item B).

## See also

- `libra clone` — restores Cloudflare D1 / R2 publish snapshots via
  the `libra+cloud://<clone-domain>/<slug>` source scheme.
- `libra cloud` — private Cloudflare backup that `publish` builds on
  top of.
- `docs/development/commands/publish.md` — internal design + phased rollout.
- `docs/ai/object-model-reference.md` — the AI object model
  contract `publish` exports.
