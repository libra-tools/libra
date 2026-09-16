# Database migration scope and configuration isolation

This document is the implementation contract for
[`plan-20260910.md`](../plan/plan-20260910.md), especially ADR-MIG-01,
ADR-MIG-02, and task MIG-00. It applies whenever Libra opens, creates,
inspects, upgrades, or repairs a SQLite database through the runtime database
layer.

## Database roles

Callers must select a `DatabaseRole` explicitly. A path is never sufficient
evidence for a role.

| `DatabaseRole` | Owner | Allowed persistent schema | Migration receipt | Compatibility rule |
|---|---|---|---|---|
| `Repository` | repository/worktree runtime | repository objects, refs, index, operation, workspace, and repository AI/runtime tables | existing repository `schema_versions` ledger | compare only with the Repository manifest |
| `GlobalConfig` | user-scoped configuration runtime | `config`/`config_kv` and configuration-owned metadata | configuration ledger selected by the GlobalConfig manifest | compare only with the configuration manifest |
| `SystemConfig` | system-scoped configuration runtime | the same configuration-owned schema as GlobalConfig, stored at the system scope | configuration ledger selected by the SystemConfig manifest | compare only with the configuration manifest |
| `Derived` | the subsystem that owns the rebuildable database | only schema declared by that subsystem | subsystem-owned or no persistent migration ledger | the generic runtime migration writer must refuse this role |

`GlobalConfig` and `SystemConfig` may share migration definitions when their
schema is identical, but role selection remains explicit so a caller cannot
silently fall back to Repository behavior. `Derived` is an explicit denial
boundary: declaring it does not authorize the repository migration runner to
write a derived or publish database.

## Role-aware manifest

There is one authoritative schema manifest. Every entry identifies:

- a stable migration identifier and its owning database role allowlist;
- the up/forward action and the ledger in which its receipt is recorded;
- any fresh-database bootstrap DDL that represents the same schema;
- any non-migration idempotent top-up and the roles for which it is allowed;
- the compatibility barrier, if applying the change makes an older reader
  unsafe.

The open, inspect, create, and upgrade APIs consume the same selected manifest.
They must not derive compatibility from the maximum identifier in an
unfiltered `builtin_migrations()` registry. A compatibility wrapper retained
for older repository call sites is fixed to `DatabaseRole::Repository` and is
not permitted in global/system configuration code.

Repository-only migrations, bootstrap statements, and top-ups must never:

- create repository tables in a GlobalConfig or SystemConfig database;
- add a repository receipt to a configuration ledger;
- advance a configuration compatibility version; or
- cause a configuration reader to classify the database as future solely
  because a Repository receipt is present.

Configuration schema may advance only through a configuration-owned manifest
entry. The writer that creates a compatibility barrier is the single
configuration barrier owner described by ADR-MIG-02; callers may request that
operation but may not duplicate its ledger or DDL writes.

## Connection and writer ownership

All database entry points accept a role before opening the database:

1. resolve an explicit role at the call site;
2. open without running another role's writer;
3. inspect only the selected role's ledger and manifest;
4. fail closed if that role has a truly unsupported future schema;
5. run only pending migrations, bootstrap DDL, and top-ups allowlisted for the
   selected role, within their documented transaction boundary.

Configuration reads must remain reads. A strict cascade lookup cannot create a
database, run DDL, add a receipt, or use a generic repository connection as a
fallback. Configuration writes and `libra config --global/--system` creation
must select `GlobalConfig` or `SystemConfig` respectively.

### MIG-03 routing and MIG-04 compatibility policy

The role-only APIs delivered by MIG-02 retain the manifest rules above.
Configuration command writers use `schema::{create_configuration_database,
open_configuration_database,ensure_configuration_schema_is_current}` and
revalidate cached handles on every acquisition. The central
`inspect_configuration_schema` composes the own-role classifier with bounded
version/name allowlists for both ledgers. Unknown, duplicate, mismatched and
unpaired receipts fail closed, including unknown entries below the maximum.
Known Repository receipts (including 0801) are accepted without granting
permission to execute Repository DDL or repair legacy state. Validation occurs
before DDL and again under the migration writer lock.

The manifest separately registers an explicit-mutation-only barrier at
`i64::MAX`, named `configuration_legacy_reader_barrier`. It is accepted only
alongside the exact configuration base receipt, never as an automatic
migration. `db::write_configuration_barrier` is the sole writer: its first SQL
acquires the shared SQLite writer lock, then rechecks metadata and appends the
marker to the legacy ledger without deleting existing receipts. Scoped
set/add/unset/import and section edits share a caller-owned transaction with
the marker; a failure rolls back both. Repeated writes are idempotent. Cached
handles are revalidated, with another locked check before each mutation.

Strict global/system cascade and storage-credential readers open through
`schema::open_readonly_connection_for_role`, then validate through
`schema::check_configuration_schema`. The connection uses an absolute literal
SQLite filename, read-only mode and no-create; URI metacharacters cannot
redirect it. Non-UTF-8 filenames are rejected with contextual errors. Missing
stores remain absent; legacy-only pre-ledger stores remain readable without
bootstrap. A malformed modern table, or a receipted configuration store missing
its required modern table, remains an error rather than a fallback value.
Local repository readers keep their existing Repository behavior.

The fresh best-effort reader uses the same read-only opener but deliberately
retains its existing query-based compatibility and failure-isolation policy:
only proven absence falls through; unreadable or encrypted local state cannot
be replaced by a global value. The opener itself does not impose strict policy.
Remote/cloud preflight inspects Global and System independently before any
Global credential bypass. True Config future and unsupported legacy receipts
retain `LBR-CONFIG-001`; complete Global env/local credentials cannot bypass
System defaults. JSON retains its existing fields and adds scope/ledger/reason,
without untrusted receipt names or values. All preflight and cascade reads
remain physically read-only and never append the barrier.

## Read-only schema diagnosis

`libra config doctor --global-schema` routes directly to GlobalConfig metadata
inspection, bypassing Repository/System policy, operation recording and both
startup recovery and auto-upgrade. It reuses the literal read-only/no-create
opener and centralized configuration classifier inside one SQLite read
transaction. It never reads config/vault values, migrates, writes a barrier or
creates a backup. A fixed SQLite header probe refuses WAL-mode opens without
regular WAL/SHM sidecars; `immutable` is not safe for live databases.

`report_version=1` exposes string receipt versions, observed/latest ledger
metadata, UTC mtime and a controlled producer disposition. Manifest membership
for `2026090801` is not writer attestation: `repair_eligible` is always false.
Before/after DB/WAL identity, size and mtime fence observable changes but cannot
exclude external rotation races; OS access time and SQLite coordination are
outside the stable-target byte-invariance promise. An unavailable target is
`unreadable`, never implicitly compatible. See the command documentation for
all successful diagnostic classifications and the non-health-check exit code.

## Fixture isolation

Tests that can reach global or system configuration must route every ambient
configuration path into one temporary sandbox. The guard owns temporary values
for `LIBRA_CONFIG_GLOBAL_DB`, `LIBRA_CONFIG_SYSTEM_DB`, `HOME`, `USERPROFILE`,
and `XDG_CONFIG_HOME`, and restores each caller value on drop, including panic
and early-return paths.

Fixtures must assert that:

- SQLite files are created only beneath the sandbox's canonical path;
- no fallback opens a real user or system configuration path;
- a Repository-only migration leaves configuration receipts and
  `config`/`config_kv` semantics unchanged; and
- tests that mutate process environment use the repository's serialized
  environment lane.

Tests and ordinary diagnostic tools must not enumerate or print configuration
values, tokens, credentials, or schema dumps from an ambient database.

### Repository context in acceptance tests

The mutable-state inventory covers Repository/Worktree/Composite state, not a
second configuration ownership catalog. Its whole-source DDL guard recognizes
the exact configuration ledger through `SchemaLedger::Configuration` and checks
the GlobalConfig/SystemConfig role mapping. The ledger is neither Repository
state nor migration scratch. Unknown tables still fail closed; both source-scan
directions and real schema materialization remain required. Fresh Repository
materialization must not contain the configuration-only ledger.

Synthetic merge tests still call the real attribute resolver. Each affected
test therefore owns a temporary Libra repository and `ConfigDbFixture`, even
when all merged blobs stay in memory. `MergeTestRepository` initializes once per
test, keeps its setup runtime alive, and restores CWD before deleting temporary
directories. Repository paths are canonicalized so symlinked temporary roots
work on all platforms. Before dropping its runtime it evicts and closes its
cached repository connection and clears the object-storage cache, including
when initialization or the test body panics. An outer CWD lock remains held
through environment and hash-kind restoration. Normal-return and panic
regression cases verify restoration and cleanup, including an explicit
closed-pool assertion; tests use named CWD/environment/hash-kind serialization
rather than depending on the checkout being a Libra repository.

The current serial classifier traverses `tests/`, not `src/` unit modules. These
unit tests still need their named serial annotations, but adding them to the TSV
would create dangling registry rows. Run the unchanged registry guard and
nextest generator to verify synchronization; do not widen or weaken the
classifier to accommodate a fixture-only change.

## Legacy receipt classification and repair boundary

An unknown or newer receipt is not repairable merely because a later source
tree is known to have emitted the same numeric identifier. Until the producer
revision, producer binary fingerprint, migration manifest/DDL, role ownership,
and fixed old-reader behavior are all attested, the receipt is
**known-unsupported and ineligible for repair**. This includes the observed
`2026090801` convergence receipt for builds whose manifest ends at
`2026090601`.

The default doctor path is read-only. It may report role, supported/observed
receipt metadata, eligibility, and secret-free provenance, but it must not run
migrations, create a backup, or alter file metadata.

An operator-confirmed GlobalConfig repair is the sole exception. It must:

1. classify the target read-only and reject unattested or ambiguous receipts;
2. canonicalize the target and obtain explicit confirmation for that exact
   path without revealing configuration values;
3. reject unsafe paths before repair side effects, lock the target and re-check
   identity, fingerprint, role, receipt and eligibility; reject observed path
   replacement, without claiming protection against malicious same-uid/root actors;
4. create and reopen-verify a SQLite-consistent backup before changing the
   primary database;
5. perform the allowlisted forward transformation and audit write in one
   atomic transaction;
6. reopen with the new reader and verify the fixed old reader fails closed
   without writing; and
7. leave source data unchanged on classification/backup rejection and roll
   back pre-commit transaction failures. Preserve the verified backup on any
   uncertain commit/durability outcome; never claim a failed status write proves
   that SQLite did not commit.

Manual receipt deletion, in-place downgrade, byte-copy backup of a live
journaled database, path-based role inference, and automatic repair are
forbidden.

### MIG-06 explicit repair implementation

`config::repair` accepts only an exact canonical GlobalConfig confirmation and
the registered v0.22.19 producer-format cohort. It compares bounded canonical
JSON digests for all 293 schema objects and 60 receipts, reuses the central
manifest classifier, and excludes Repository storage/data. Only configuration
rows and the two known bootstrap metadata classes may be nonempty. This
registers a format, not the historical writer of an arbitrary user file.

Repair is limited to verified private Unix local paths; unsupported platforms,
filesystems, ownership, writable ancestors, symlinks, hardlinks and unsafe
sidecars fail closed. The persistent advisory lock only serializes repairs;
SQLite locking arbitrates other writers. Path/inode checks are fences, not a
filesystem-wide freeze. Operators must stop external file replacement tools.

`schema::open_configuration_repair_connection` opens an existing literal file
without creation or schema management. Its unshared one-connection pool and
TEMP nonce guard establish physical handle continuity for `data_version`.
`VACUUM INTO ?` runs before the source write transaction, producing a private,
flushed, integrity-checked and attestation-checked backup. After
`db::begin_write_transaction`, continuity, data_version, file identity and full
eligibility are rechecked before the transaction calls
`schema::initialize_configuration_ledger_for_repair` and the existing sole
`db::write_configuration_barrier`. No Repository bootstrap/top-up runs, no
original receipt is removed, and no business value is enumerated by the app.

Recovery directories retain `backup.sqlite` and atomic `recovery.json` status.
Failed/unverified copies cannot authorize restore. A commit can succeed before
status persistence fails, so uncertain/crashed outcomes require read-only
diagnosis and preserved evidence, not an automatic rollback assumption.
`test-upgrade` plus `LIBRA_TEST=1` enables bounded fault checkpoints and a SQLite
progress callback for the copying-stage lock test; release binaries exclude
these hooks. Command and EN/zh recovery documentation define the public limits.

## Migration author checklist

Before landing a schema-related change, the author records all of the
following in the task card and tests:

- [ ] The affected `DatabaseRole` values and explicit role allowlist.
- [ ] The owning ledger and latest/current compatibility comparison.
- [ ] Migration, bootstrap, and idempotent top-up allowlists, including
      explicit `N/A` entries.
- [ ] The open/create/inspect/upgrade call sites and the single writer owner.
- [ ] Transaction, idempotence, failure, recovery, and old-reader behavior.
- [ ] Isolated GlobalConfig and SystemConfig fixtures with environment
      restoration and real-path canaries.
- [ ] A regression proving Repository-only schema work cannot advance or
      populate either configuration database.
- [ ] User-facing error, command, compatibility, migration, and recovery
      documentation when behavior is externally visible.
- [ ] Attestation, consistent backup, atomic forward repair, and zero-write
      rejection evidence for any legacy repair; otherwise mark it ineligible.
- [ ] Focused, formatting, lint, full-suite, release, and remote evidence
      required by the governing plan.

This checklist is the concrete role map required by GC-13. Any implementation
that cannot fill it out must remain fail-closed until its ownership and
compatibility contract are explicit.
