# Schema migrations directory

This directory holds **versioned SQL migrations** (idempotent by default,
with a narrow claim-first RENAME-rebuild exception — see below) managed by
`crate::internal::db::migration::MigrationRunner` (CEX-12.5).

## Filename convention

```
YYYYMMDDNN_<snake_case_name>.sql         # forward (up) migration
YYYYMMDDNN_<snake_case_name>_down.sql    # optional matching rollback
```

- `YYYYMMDDNN` is a 10-digit monotonic version derived from the calendar date
  the migration was authored, suffixed with a 2-digit ordinal so multiple
  migrations on the same day stay ordered (e.g., `2026050301`, `2026050302`,
  `2026050303`). The runner enforces strictly increasing versions at
  registration time.
- `<snake_case_name>` mirrors the migration's `name` field passed to
  `Migration { name: "...", .. }`.
- Forward migrations are required; `_down.sql` files are optional. A
  migration without a matching `_down.sql` cannot be rolled back through.

> Older docs referenced a 4-digit `NNNN` scheme; the codebase has standardised
> on `YYYYMMDDNN` since the very first registered migration (`2026050301`).

## Idempotency requirement

Forward DDL **should be idempotent** at the SQL level:

- `CREATE TABLE IF NOT EXISTS ...` (never bare `CREATE TABLE`)
- `CREATE INDEX IF NOT EXISTS ...`
- `ALTER TABLE ... ADD COLUMN` is OK only when guarded by a column-exists
  check (sqlite-specific) or scoped behind a feature flag.

Exception: RENAME-based table rebuilds (e.g. `2026072101`, `2026072301`)
are inherently non-idempotent. They are safe only because the runner claims
the `schema_versions` row *before* executing the up DDL inside the same
transaction (claim-first), so the DDL can never run twice — including under
concurrent upgraders. Plain additive migrations must still be written
idempotently (see the rationale below).

Rationale: legacy databases initialized via `sqlite_20260309_init.sql` may
already contain tables that an early migration tries to create. Idempotent
DDL means the explicit upgrade command can safely apply every pending
migration over such pre-existing shapes. For plain additive migrations the
`schema_versions` table is bookkeeping; for the RENAME-rebuild exception it
is also the safety layer — the claim-first transaction is what prevents a
second execution. Normal command connections do not apply migrations implicitly.
They check compatibility first and ask the user to run `libra db upgrade` when
the repository is stale.

## Transaction-unsafe DDL is forbidden

The runner wraps every `up` and `down` DDL body in a SQLite transaction so
the schema change and the `schema_versions` insert/delete are atomic. SQLite
does not allow these statement types inside a transaction:

- `VACUUM` and `VACUUM INTO ...`
- Explicit `BEGIN` / `COMMIT` / `ROLLBACK` (the runner already manages this
  layer)
- `PRAGMA journal_mode = ...`, `PRAGMA wal_checkpoint`, and any other
  PRAGMA documented as transaction-sensitive

If a future CEX needs one of these, it must run the statement **outside**
the migration runner (e.g., in a dedicated maintenance command) and have
the migration only flip schema state.

## Don't reuse legacy `ensure_*_schema` table names without verification

The four legacy helpers in `src/internal/db.rs`
(`ensure_config_kv_schema`, `ensure_ai_projection_schema`,
`ensure_ai_runtime_contract_schema`), plus the two bootstrap files
(`sqlite_20260309_init.sql` for core git + AI baseline, and
`sqlite_20260415_ai_runtime_contract.sql` for the AI runtime-contract
extension), own their tables. A new migration whose `up` DDL targets one
of those tables but ships a different shape will silently no-op against
legacy DBs (because of `IF NOT EXISTS`) and create a hidden schema drift
between fresh and legacy installs.

If a CEX must touch a legacy-owned table, it should:

1. First run a `PRAGMA table_info(<name>)` (or sea-orm equivalent) inside
   the migration to detect the shape; bail out with a clear error if it
   differs from what the migration assumes.
2. Or, preferred: leave the table alone and create a *new* table that
   joins back to the legacy one by id. Future CEX-15 / CEX-16 should
   default to this pattern.

## Registering migrations in code

The runner does **not** auto-load files from this directory at runtime
(SQLite migrations are compile-time critical and we want them embedded in
the binary). Instead, every migration is registered in
`crate::internal::db::migration::builtin_migrations` via
`include_str!("../../sql/migrations/<file>.sql")`.

When adding a new migration:

1. Drop the SQL into `sql/migrations/YYYYMMDDNN_<name>.sql` (and optionally
   `YYYYMMDDNN_<name>_down.sql`).
2. Add a corresponding entry to `builtin_migrations()` in
   `src/internal/db/migration.rs`, with the SQL embedded via
   `include_str!`. **Path**: from `src/internal/db/migration.rs` the
   correct relative path is `../../../sql/migrations/<file>.sql` (three
   `..` segments to escape `src/internal/db/`, then descend into
   `sql/migrations/`). Compare against the existing
   `src/internal/db.rs:include_str!("../../sql/sqlite_20260309_init.sql")`
   which sits one directory shallower and uses two `..` segments. The
   version number must be strictly greater than the previous one (the
   runner enforces this at registration time).
3. Add a unit / integration test under `tests/db_migration_test.rs`
   verifying the new table / column appears after `run_pending` and that a
   second `run_pending` is a no-op.

## CEX-12.5 initial state

CEX-12.5 shipped the framework with **zero registered migrations**. The
`builtin_migrations()` registry was empty; the existing legacy schema
remained owned by `sqlite_20260309_init.sql` and the `ensure_*_schema`
helpers in `db.rs`. Subsequent CEXes have populated this directory.

## Current registry

| Version       | Name                | Source                                          |
|---------------|---------------------|-------------------------------------------------|
| `2026050301`  | `automation_log`    | `2026050301_automation_log{,_down}.sql`         |
| `2026050302`  | `agent_usage_stats` | `2026050302_agent_usage_stats{,_down}.sql`      |
| `2026050303`  | `agent_capture`     | `2026050303_agent_capture{,_down}.sql`          |
| `2026050501`  | `agent_checkpoint_parent_nullable` | `2026050501_agent_checkpoint_parent_nullable{,_down}.sql` |
| `2026050601`  | `approved_permission` | `2026050601_approved_permission{,_down}.sql`  |
| `2026050801`  | `agent_usage_stats_agent_name` | `2026050801_agent_usage_stats_agent_name{,_down}.sql` |
| `2026052301`  | `source_call_log` | `2026052301_source_call_log{,_down}.sql` |
| `2026053101`  | `ai_final_decision` | `2026053101_ai_final_decision{,_down}.sql` |
| `2026060201`  | `source_call_log_agent_run_id` | `2026060201_source_call_log_agent_run_id{,_down}.sql` |
| `2026060401`  | `cherry_pick_state` | `2026060401_cherry_pick_state{,_down}.sql` |
| `2026060801`  | `revert_sequence` | `2026060801_revert_sequence{,_down}.sql` |
| `2026061401`  | `notes` | `2026061401_notes{,_down}.sql` |
| `2026062301`  | `rename_agent_traces_branch` | `2026062301_rename_agent_traces_branch{,_down}.sql` |
| `2026070201`  | `metadata_kv` | `2026070201_metadata_kv{,_down}.sql` |
| `2026070202`  | `working_dirty` | `2026070202_working_dirty{,_down}.sql` |
| `2026070301`  | `revision_ordinal` | `2026070301_revision_ordinal{,_down}.sql` |
| `2026070401`  | `sequence_state` | `2026070401_sequence_state{,_down}.sql` (lore.md 2.6: unified sequencer store; folds cherry-pick forward, drops the `cherry_pick_state`/`revert_sequence` legacy tables) |
| `2026070501`  | `layer` | `2026070501_layer{,_down}.sql` (lore.md 2.4: `layer`+`layer_path` local-overlay side-tables; owner `internal::layer`) |
| `2026070601`  | `object_obliteration` | `2026070601_object_obliteration{,_down}.sql` (lore.md 2.5: intentional-absence tombstone registry; owner `internal::obliteration`) |
| `2026070701`  | `sparse_view` | `2026070701_sparse_view{,_down}.sql` (lore.md 2.2: read-only sparse view include patterns; owner `internal::sparse`) |
| `2026070801`  | `worktree_isolation` | `2026070801_worktree_isolation{,_down}.sql` (lore.md 2.1: per-worktree HEAD/index/HEAD-reflog isolation — adds `worktree_id` to `reference` + `reflog`) |
| `2026070802`  | `agent_checkpoint_paging` | `2026070802_agent_checkpoint_paging{,_down}.sql` (AG-20: `agent_checkpoint(traces_commit)` probe index — deliberately NON-unique so legacy DBs with duplicate rows cannot brick the auto-upgrade; writer idempotency lives in code (probe-first + `ON CONFLICT(checkpoint_id) DO NOTHING`) — plus keyset pagination indexes `agent_session(started_at DESC, session_id)` and `agent_checkpoint(created_at DESC, checkpoint_id)`) |
| `2026070803`  | `agent_audit_log` | `2026070803_agent_audit_log{,_down}.sql` (AG-24a: append-only raw checkpoint access/export audit log; rollback freezes writes without dropping retained audit evidence) |
| `2026071301`  | `agent_coverage_gate` | `2026071301_agent_coverage_gate{,_down}.sql` (M1: per-turn coverage claim and append-only revision gate) |
| `2026071401`  | `agent_export_job` | `2026071401_agent_export_job{,_down}.sql` (M3: fenced OpenCode export-bridge job state) |
| `2026071402`  | `agent_import_identity` | `2026071402_agent_import_identity{,_down}.sql` (M4: crash-recoverable import identity and progress) |
| `2026071403`  | `agent_import_tombstone` | `2026071403_agent_import_tombstone{,_down}.sql` (M4: local anti-resurrection tombstone) |
| `2026071404`  | `agent_tombstone_compat_barrier` | `2026071404_agent_tombstone_compat_barrier{,_down}.sql` (M4: old-writer compatibility triggers for already-released tombstones) |
| `2026071405`  | `agent_coverage_conflict` | `2026071405_agent_coverage_conflict{,_down}.sql` (M4: bounded complete-coverage conflict evidence) |
| `2026071406`  | `agent_subagent_content` | `2026071406_agent_subagent_content{,_down}.sql` (M5 immutable base: opaque child-source claims, append-only revisions, and boundary/content links; down refuses durable attribution or active reservations) |
| `2026071407`  | `agent_subagent_replication` | `2026071407_agent_subagent_replication{,_down}.sql` (M5 compatibility: adds monotonic source/cloud generations, incarnation/cloud-base state, checkpoint-prune fences, and boundary-delete unlinking to repositories that already applied the immutable 1406 base schema) |
| `2026071901`  | `sequencer_worktree_scope` | `2026071901_sequencer_worktree_scope{,_down}.sql` (plan-20260714 §C.4.2: `sequence_state` re-keyed to one row per worktree; down fails closed on linked rows) |
| `2026072101`  | `rebase_state_worktree_scope` | `2026072101_rebase_state_worktree_scope{,_down}.sql` (§C.4.2: `rebase_state` re-keyed per worktree, lazy DDL retired; down fails closed on linked rows) |
| `2026072201`  | `operation_worktree_scope` | `2026072201_operation_worktree_scope{,_down}.sql` (§C.9: per-worktree operation dedup scope column) |
| `2026072301`  | `bisect_state_worktree_scope` | `2026072301_bisect_state_worktree_scope{,_down}.sql` (§C.4.2: `bisect_state` re-keyed per worktree — newest row per scope wins — lazy DDL retired; down fails closed on linked rows) |

All registered migrations are loaded via `include_str!`. New migrations must
follow the same pattern — inline SQL strings in `builtin_migrations()` are no
longer accepted.

## `include_str!` example

```rust
Migration {
    version: 2026050303,
    name: "agent_capture",
    up: include_str!("../../../sql/migrations/2026050303_agent_capture.sql"),
    down: Some(include_str!(
        "../../../sql/migrations/2026050303_agent_capture_down.sql"
    )),
}
```

The relative path is resolved by `rustc` from the source file containing
`include_str!`. From `src/internal/db/migration.rs`, three `..` segments
escape `src/internal/db/`, then `sql/migrations/<file>.sql` descends into
this directory.
