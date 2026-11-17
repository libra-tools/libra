//! Explicit database ownership, schema manifests and configuration migrations.
//!
//! Repository compatibility wrappers select Repository, never infer ownership
//! from a filename. Configuration migration receipts have their own namespace;
//! legacy repository receipts are retained for the later confirmed repair path.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::Path,
    sync::OnceLock,
    time::Duration,
};

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbErr,
    Statement, TransactionTrait,
};

use super::{SchemaCompatibility, SchemaUpgradeReport, migration::Migration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DatabaseRole {
    Repository,
    GlobalConfig,
    SystemConfig,
    Derived,
    /// Catalog file `.libra/private/capture.db`. Opened only by the capture store.
    Capture,
}

impl fmt::Display for DatabaseRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository => "repository",
            Self::GlobalConfig => "global configuration",
            Self::SystemConfig => "system configuration",
            Self::Derived => "derived",
            Self::Capture => "capture",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaLedger {
    Repository,
    Configuration,
    Capture,
}

impl SchemaLedger {
    pub fn table_name(self) -> &'static str {
        match self {
            Self::Repository | Self::Capture => "schema_versions",
            Self::Configuration => "configuration_schema_versions",
        }
    }
}

const REPOSITORY: &[DatabaseRole] = &[DatabaseRole::Repository];
const CONFIGURATION: &[DatabaseRole] = &[DatabaseRole::GlobalConfig, DatabaseRole::SystemConfig];
const PERSISTENT: &[DatabaseRole] = &[
    DatabaseRole::Repository,
    DatabaseRole::GlobalConfig,
    DatabaseRole::SystemConfig,
];

pub(crate) const CONFIG_KV_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS `config_kv` (
    `id` INTEGER PRIMARY KEY AUTOINCREMENT,
    `key` TEXT NOT NULL,
    `value` TEXT NOT NULL,
    `encrypted` INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_config_kv_key ON config_kv(`key`);
"#;
const LEGACY_CONFIG_SQL: &str =
    include_str!("../../../sql/migrations/2026090601_legacy_config_table.sql");
const CONFIGURATION_LEDGER_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS configuration_schema_versions (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
"#;

const CONFIGURATION_BASE_VERSION: i64 = 2026090601;
const CONFIGURATION_BASE_NAME: &str = "configuration_base";

/// Capture catalog schema. This receipt lives only in `capture.db`.
pub const CAPTURE_SCHEMA_VERSION: i64 = 2026111601;
pub const CAPTURE_SCHEMA_NAME: &str = "capture_database";
pub const CAPTURE_DATABASE_SQL: &str =
    include_str!("../../../sql/migrations/2026111601_capture_database.sql");
const CAPTURE_ROLES: &[DatabaseRole] = &[DatabaseRole::Capture];

/// An explicit-mutation-only compatibility migration, deliberately excluded
/// from automatic migrations and bootstraps. The legacy namespace must remain
/// future to every older Repository-only reader.
pub struct ConfigurationBarrier {
    pub roles: &'static [DatabaseRole],
    pub version: i64,
    pub name: &'static str,
    pub required_base_version: i64,
    pub required_base_name: &'static str,
    pub legacy_ledger_sql: &'static str,
}

const CONFIGURATION_BARRIER: ConfigurationBarrier = ConfigurationBarrier {
    roles: CONFIGURATION,
    version: i64::MAX,
    name: "configuration_legacy_reader_barrier",
    required_base_version: CONFIGURATION_BASE_VERSION,
    required_base_name: CONFIGURATION_BASE_NAME,
    legacy_ledger_sql: "CREATE TABLE IF NOT EXISTS schema_versions (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)",
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationSchemaIssueKind {
    Future,
    UnregisteredReceipt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationSchemaIssue {
    pub role: DatabaseRole,
    pub ledger: SchemaLedger,
    pub current_version: i64,
    pub latest_version: Option<i64>,
    pub kind: ConfigurationSchemaIssueKind,
}

impl ConfigurationSchemaIssue {
    pub fn reason(&self) -> &'static str {
        match self.kind {
            ConfigurationSchemaIssueKind::Future => {
                "schema is newer than this Libra binary supports"
            }
            ConfigurationSchemaIssueKind::UnregisteredReceipt => {
                "schema contains an unsupported migration receipt"
            }
        }
    }
}

impl fmt::Display for ConfigurationSchemaIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} database {} (ledger: {}, version: {}, latest supported: {}); install a compatible newer Libra binary; do not edit migration receipts manually",
            self.role,
            self.reason(),
            self.ledger.table_name(),
            self.current_version,
            super::format_schema_version(self.latest_version)
        )
    }
}

pub struct ConfigurationSchemaInspection {
    pub compatibility: SchemaCompatibility,
    pub issue: Option<ConfigurationSchemaIssue>,
    /// Positively established metadata; unsupported inspections may stop early.
    pub base_receipt_present: bool,
    pub barrier_present: bool,
}

#[derive(Clone, Debug)]
pub struct ScopedMigration {
    pub roles: &'static [DatabaseRole],
    pub migration: Migration,
}

#[derive(Clone, Copy, Debug)]
pub struct BootstrapDefinition {
    pub name: &'static str,
    pub roles: &'static [DatabaseRole],
    pub sql: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaTopUp {
    ConfigKv,
    AiProjection,
    AiRuntimeContract,
    RebaseShape,
    BisectShape,
}

#[derive(Clone, Copy, Debug)]
pub struct TopUpDefinition {
    pub roles: &'static [DatabaseRole],
    pub action: SchemaTopUp,
}

const BOOTSTRAPS: &[BootstrapDefinition] = &[
    BootstrapDefinition {
        name: "repository_core",
        roles: REPOSITORY,
        sql: super::BOOTSTRAP_SQL,
    },
    BootstrapDefinition {
        name: "configuration_legacy",
        roles: CONFIGURATION,
        sql: LEGACY_CONFIG_SQL,
    },
    BootstrapDefinition {
        name: "configuration_kv",
        roles: CONFIGURATION,
        sql: CONFIG_KV_SQL,
    },
];

const TOP_UPS: &[TopUpDefinition] = &[
    TopUpDefinition {
        roles: PERSISTENT,
        action: SchemaTopUp::ConfigKv,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::AiProjection,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::AiRuntimeContract,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::RebaseShape,
    },
    TopUpDefinition {
        roles: REPOSITORY,
        action: SchemaTopUp::BisectShape,
    },
];

/// The single manifest for runtime migrations, bootstraps and top-ups.
pub struct SchemaManifest {
    pub migrations: Vec<ScopedMigration>,
    pub bootstraps: &'static [BootstrapDefinition],
    pub top_ups: &'static [TopUpDefinition],
    pub configuration_barrier: &'static ConfigurationBarrier,
}

pub fn schema_manifest() -> &'static SchemaManifest {
    static MANIFEST: OnceLock<SchemaManifest> = OnceLock::new();
    MANIFEST.get_or_init(build_schema_manifest)
}

fn build_schema_manifest() -> SchemaManifest {
    // All historical runtime migrations belong to the repository namespace.
    // A configuration-owned migration is registered separately, even when its
    // numeric ID and SQL also occur in the repository namespace.
    let mut migrations: Vec<_> = super::migration::repository_migrations()
        .into_iter()
        .map(|migration| ScopedMigration {
            roles: REPOSITORY,
            migration,
        })
        .collect();
    migrations.push(ScopedMigration {
        roles: CONFIGURATION,
        migration: Migration {
            version: CONFIGURATION_BASE_VERSION,
            name: CONFIGURATION_BASE_NAME,
            up: LEGACY_CONFIG_SQL,
            down: None,
        },
    });
    migrations.push(ScopedMigration {
        roles: CAPTURE_ROLES,
        migration: Migration {
            version: CAPTURE_SCHEMA_VERSION,
            name: CAPTURE_SCHEMA_NAME,
            up: CAPTURE_DATABASE_SQL,
            down: None,
        },
    });
    SchemaManifest {
        migrations,
        bootstraps: BOOTSTRAPS,
        top_ups: TOP_UPS,
        configuration_barrier: &CONFIGURATION_BARRIER,
    }
}

pub fn ledger_for_role(role: DatabaseRole) -> io::Result<SchemaLedger> {
    match role {
        DatabaseRole::Repository => Ok(SchemaLedger::Repository),
        DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => Ok(SchemaLedger::Configuration),
        DatabaseRole::Capture => Ok(SchemaLedger::Capture),
        DatabaseRole::Derived => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "derived databases are owned by their subsystem; use its schema API instead of runtime migrations",
        )),
    }
}

pub fn migrations_for_role(role: DatabaseRole) -> Vec<Migration> {
    schema_manifest()
        .migrations
        .iter()
        .filter(|entry| entry.roles.contains(&role))
        .map(|entry| entry.migration.clone())
        .collect()
}

pub fn bootstraps_for_role(
    role: DatabaseRole,
) -> impl Iterator<Item = &'static BootstrapDefinition> {
    BOOTSTRAPS
        .iter()
        .filter(move |entry| entry.roles.contains(&role))
}

pub fn top_ups_for_role(role: DatabaseRole) -> impl Iterator<Item = SchemaTopUp> {
    TOP_UPS
        .iter()
        .filter(move |entry| entry.roles.contains(&role))
        .map(|entry| entry.action)
}

pub fn latest_schema_version_for_role(role: DatabaseRole) -> io::Result<Option<i64>> {
    // Validate once per role, not on every cache hit or configuration lookup.
    // All successful subsequent inspections perform only the two SQL queries.
    static LATEST: [OnceLock<Result<Option<i64>, String>>; 4] = [const { OnceLock::new() }; 4];
    let index = match role {
        DatabaseRole::Repository => 0,
        DatabaseRole::GlobalConfig => 1,
        DatabaseRole::SystemConfig => 2,
        DatabaseRole::Capture => 3,
        DatabaseRole::Derived => return ledger_for_role(role).map(|_| None),
    };
    LATEST[index]
        .get_or_init(|| {
            let mut runner = super::migration::MigrationRunner::new();
            runner
                .extend(migrations_for_role(role))
                .map_err(|error| format!("invalid {role} migration manifest: {error}"))?;
            Ok(runner.max_registered_version())
        })
        .as_ref()
        .copied()
        .map_err(|error| io::Error::other(error.clone()))
}

/// Metadata lookup and indexed MAX, with no DDL or configuration-value reads.
pub async fn current_schema_version_for_role<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
) -> io::Result<Option<i64>> {
    let table = ledger_for_role(role)?.table_name();
    let exists = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            [table.into()],
        ))
        .await
        .map_err(|error| io::Error::other(format!("failed to inspect {role} ledger: {error}")))?;
    if exists.is_none() {
        return Ok(None);
    }
    // `table` is selected from a closed enum, never a path or user input.
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT MAX(version) FROM {table}"),
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!("failed to read {role} schema version: {error}"))
        })?;
    let row = row.ok_or_else(|| {
        io::Error::other(format!("{role} schema version query returned no result"))
    })?;
    row.try_get_by_index(0)
        .map_err(|error| io::Error::other(format!("invalid {role} schema version: {error}")))
}

pub async fn inspect_schema_for_connection<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
) -> io::Result<SchemaCompatibility> {
    let current = current_schema_version_for_role(conn, role).await?;
    let latest = latest_schema_version_for_role(role)?;
    Ok(classify_schema_versions(current, latest))
}

fn classify_schema_versions(current: Option<i64>, latest: Option<i64>) -> SchemaCompatibility {
    match (current, latest) {
        (Some(current), latest) if latest.is_none_or(|latest| current > latest) => {
            SchemaCompatibility::UnsupportedFuture {
                current_version: current,
                latest_version: latest,
            }
        }
        (current, Some(latest)) if current != Some(latest) => {
            SchemaCompatibility::UpgradeRequired {
                current_version: current,
                latest_version: latest,
            }
        }
        (current, latest) => SchemaCompatibility::Compatible {
            current_version: current,
            latest_version: latest,
        },
    }
}

fn reject_future(role: DatabaseRole, compatibility: &SchemaCompatibility) -> io::Result<()> {
    if let SchemaCompatibility::UnsupportedFuture {
        current_version,
        latest_version,
    } = compatibility
    {
        return Err(io::Error::other(format!(
            "{role} database schema version {current_version} is newer than this Libra binary supports (latest supported: {}); install a newer Libra binary",
            super::format_schema_version(*latest_version),
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SchemaPolicy {
    RoleOnly,
    Configuration,
}

fn require_configuration_role(role: DatabaseRole) -> io::Result<()> {
    match role {
        DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{role} is not a configuration scope; use its role-specific database API"),
        )),
    }
}

fn registered_receipts(ledger: SchemaLedger) -> &'static BTreeMap<i64, &'static str> {
    static REPOSITORY_RECEIPTS: OnceLock<BTreeMap<i64, &'static str>> = OnceLock::new();
    static CONFIGURATION_RECEIPTS: OnceLock<BTreeMap<i64, &'static str>> = OnceLock::new();
    static CAPTURE_RECEIPTS: OnceLock<BTreeMap<i64, &'static str>> = OnceLock::new();
    let (cache, role) = match ledger {
        SchemaLedger::Repository => (&REPOSITORY_RECEIPTS, DatabaseRole::Repository),
        SchemaLedger::Configuration => (&CONFIGURATION_RECEIPTS, DatabaseRole::GlobalConfig),
        SchemaLedger::Capture => (&CAPTURE_RECEIPTS, DatabaseRole::Capture),
    };
    cache.get_or_init(|| {
        migrations_for_role(role)
            .into_iter()
            .map(|migration| (migration.version, migration.name))
            .collect()
    })
}

/// Read only bounded metadata. An extra row beyond the manifest plus its one
/// reserved barrier necessarily contains an unknown or duplicate receipt.
async fn configuration_receipts<C: ConnectionTrait>(
    conn: &C,
    ledger: SchemaLedger,
) -> io::Result<Vec<(i64, String)>> {
    let table = ledger.table_name();
    let exists = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT type FROM sqlite_master WHERE name = ? LIMIT 1",
            [table.into()],
        ))
        .await
        .map_err(|error| {
            io::Error::other(format!(
                "failed to inspect configuration ledger {table}: {error}"
            ))
        })?;
    let Some(metadata) = exists else {
        return Ok(Vec::new());
    };
    let kind: String = metadata.try_get_by_index(0).map_err(|_| {
        io::Error::other(format!("invalid metadata for configuration ledger {table}"))
    })?;
    if kind != "table" {
        return Err(io::Error::other(format!(
            "configuration ledger {table} is not a table; restore a verified backup"
        )));
    }
    let limit = registered_receipts(ledger).len() + 2;
    // The identifier and limit come only from the closed manifest. Truncating
    // names also bounds hostile metadata allocation; no registered name is
    // this long, so a truncated name can never be accepted accidentally.
    conn.query_all_raw(Statement::from_string(
        conn.get_database_backend(),
        format!(
            "SELECT version, CASE WHEN typeof(name) = 'text' THEN substr(name, 1, 256) ELSE '' END FROM {table} ORDER BY version DESC LIMIT {limit}"
        ),
    ))
    .await
    .map_err(|error| {
        io::Error::other(format!(
            "failed to read configuration ledger {table}: {error}"
        ))
    })?
    .into_iter()
    .map(|row| {
        Ok((
            row.try_get_by_index(0).map_err(|_| {
                io::Error::other(format!("invalid version in configuration ledger {table}"))
            })?,
            row.try_get_by_index(1).map_err(|_| {
                io::Error::other(format!(
                    "invalid receipt name in configuration ledger {table}"
                ))
            })?,
        ))
    })
    .collect()
}

/// Configuration policy composes its own manifest with a strict legacy receipt
/// allowlist. It never mistakes a known Repository receipt for Config future,
/// and never treats support as permission to repair legacy metadata.
pub async fn inspect_configuration_schema<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
) -> io::Result<ConfigurationSchemaInspection> {
    require_configuration_role(role)?;
    let compatibility = inspect_schema_for_connection(conn, role).await?;
    // Once the own-role maximum proves future, unrelated malformed legacy
    // metadata must not demote it to an ignorable System I/O error.
    if let SchemaCompatibility::UnsupportedFuture {
        current_version,
        latest_version,
    } = compatibility
    {
        return Ok(ConfigurationSchemaInspection {
            compatibility: compatibility.clone(),
            issue: Some(ConfigurationSchemaIssue {
                role,
                ledger: SchemaLedger::Configuration,
                current_version,
                latest_version,
                kind: ConfigurationSchemaIssueKind::Future,
            }),
            base_receipt_present: false,
            barrier_present: false,
        });
    }
    let own = configuration_receipts(conn, SchemaLedger::Configuration).await?;
    let barrier = schema_manifest().configuration_barrier;
    let base_receipt_present = own.iter().any(|(version, name)| {
        *version == barrier.required_base_version && name == barrier.required_base_name
    });
    let mut issue = None;
    let mut barrier_present = false;
    for (ledger, preloaded) in [
        (SchemaLedger::Configuration, Some(own)),
        (SchemaLedger::Repository, None),
    ] {
        if issue.is_some() {
            break;
        }
        let receipts = match preloaded {
            Some(receipts) => receipts,
            None => configuration_receipts(conn, ledger).await?,
        };
        let registered = registered_receipts(ledger);
        let mut seen = BTreeSet::new();
        for (version, name) in receipts {
            let is_barrier = ledger == SchemaLedger::Repository
                && version == barrier.version
                && name == barrier.name
                && base_receipt_present;
            let supported = (is_barrier
                || registered
                    .get(&version)
                    .is_some_and(|expected| *expected == name))
                && seen.insert(version);
            if supported && is_barrier {
                barrier_present = true;
            }
            if !supported && issue.is_none() {
                issue = Some(ConfigurationSchemaIssue {
                    role,
                    ledger,
                    current_version: version,
                    latest_version: registered.last_key_value().map(|(version, _)| *version),
                    kind: ConfigurationSchemaIssueKind::UnregisteredReceipt,
                });
            }
        }
    }
    Ok(ConfigurationSchemaInspection {
        compatibility,
        issue,
        base_receipt_present,
        barrier_present,
    })
}

async fn inspect_supported_schema<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
    policy: SchemaPolicy,
) -> io::Result<SchemaCompatibility> {
    if matches!(policy, SchemaPolicy::Configuration) {
        let inspection = inspect_configuration_schema(conn, role).await?;
        if let Some(issue) = inspection.issue {
            return Err(io::Error::other(issue.to_string()));
        }
        return Ok(inspection.compatibility);
    }
    let compatibility = inspect_schema_for_connection(conn, role).await?;
    reject_future(role, &compatibility)?;
    Ok(compatibility)
}

/// Validate configuration compatibility without migrating or reading values.
pub(crate) async fn check_configuration_schema<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
) -> io::Result<()> {
    inspect_supported_schema(conn, role, SchemaPolicy::Configuration)
        .await
        .map(|_| ())
}

/// A pre-ledger configuration file may contain only the legacy table. Do not
/// bootstrap it during a read. A receipted file missing config_kv is corrupt,
/// not an absent setting; views are queried normally so query errors surface.
pub(crate) async fn configuration_has_kv(
    conn: &DatabaseConnection,
    role: DatabaseRole,
) -> io::Result<bool> {
    require_configuration_role(role)?;
    let inspect_error = |error| {
        io::Error::other(format!(
            "failed to inspect {role} config_kv schema: {error}"
        ))
    };
    if super::sqlite_schema_contains(conn, "table", "config_kv")
        .await
        .map_err(inspect_error)?
        || super::sqlite_schema_contains(conn, "view", "config_kv")
            .await
            .map_err(inspect_error)?
    {
        return Ok(true);
    }
    if current_schema_version_for_role(conn, role).await?.is_some() {
        return Err(io::Error::other(format!(
            "{role} database is missing its required config_kv table; restore a verified backup"
        )));
    }
    Ok(false)
}

/// Revalidate cached configuration handles as well as newly opened writers.
pub(crate) async fn ensure_configuration_schema_is_current(
    conn: &DatabaseConnection,
    role: DatabaseRole,
) -> io::Result<()> {
    let compatibility = inspect_supported_schema(conn, role, SchemaPolicy::Configuration).await?;
    if matches!(compatibility, SchemaCompatibility::UpgradeRequired { .. }) {
        upgrade_connection_with_policy(conn, role, SchemaPolicy::Configuration).await?;
    }
    Ok(())
}

/// Open an existing literal filename without creation, DDL or compatibility
/// policy. Strict readers must call `check_configuration_schema`; best-effort
/// readers retain their query-based failure-isolation contract.
pub(crate) async fn open_readonly_connection_for_role(
    db_path: &Path,
    busy_timeout: Duration,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    open_literal_connection(db_path, busy_timeout, role, true).await
}

/// A private repair handle: no file creation, automatic migration, cached
/// connection or Repository policy. The caller supplies confirmation, format
/// attestation and backup, and proves physical connection continuity itself.
#[cfg(unix)]
pub(crate) async fn open_configuration_repair_connection(
    db_path: &Path,
) -> io::Result<DatabaseConnection> {
    open_literal_connection(
        db_path,
        Duration::from_millis(200),
        DatabaseRole::GlobalConfig,
        false,
    )
    .await
}

/// Initialize only configuration-owned receipt metadata inside the repair
/// caller's already locked transaction. No bootstrap/top-up or data migration
/// belongs here. The caller must subsequently use the sole barrier writer.
#[cfg(unix)]
pub(crate) async fn initialize_configuration_ledger_for_repair(
    txn: &DatabaseTransaction,
) -> io::Result<()> {
    let role = DatabaseRole::GlobalConfig;
    let inspection = inspect_configuration_schema(txn, role).await?;
    if inspection.issue.is_some()
        || inspection.base_receipt_present
        || inspection.barrier_present
        || latest_schema_version_for_role(role)? != Some(CONFIGURATION_BASE_VERSION)
    {
        return Err(io::Error::other(
            "configuration repair eligibility changed; rerun schema diagnosis",
        ));
    }
    txn.execute_unprepared(CONFIGURATION_LEDGER_SQL)
        .await
        .map_err(io::Error::other)?;
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "INSERT INTO configuration_schema_versions (version, name, applied_at) VALUES (?, ?, CURRENT_TIMESTAMP)",
        [CONFIGURATION_BASE_VERSION.into(), CONFIGURATION_BASE_NAME.into()],
    )).await.map_err(io::Error::other)?;
    Ok(())
}

fn reject_direct_capture_open(role: DatabaseRole) -> io::Result<()> {
    if role == DatabaseRole::Capture {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture databases open only through the capture store",
        ));
    }
    Ok(())
}

async fn open_literal_connection(
    db_path: &Path,
    busy_timeout: Duration,
    role: DatabaseRole,
    read_only: bool,
) -> io::Result<DatabaseConnection> {
    reject_direct_capture_open(role)?;
    ledger_for_role(role)?;
    // Absolute paths also prevent a relative filename starting with `file:`
    // from being interpreted as a SQLite URI (SQLITE_OPEN_URI is enabled).
    let filename = std::path::absolute(db_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot resolve {role} database path '{}': {error}",
                db_path.display()
            ),
        )
    })?;
    if filename.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{role} database path '{}' is not valid UTF-8; use a UTF-8 path",
                db_path.display()
            ),
        ));
    }
    // The URL is fixed: SQLite URI metacharacters in the caller's path must
    // not become query options or redirect the connection to another file.
    let mut options = ConnectOptions::new("sqlite://role-owned-database");
    options.max_connections(1);
    options.sqlx_logging(false);
    options.map_sqlx_sqlite_pool_opts(super::sqlite_pool_options);
    options.map_sqlx_sqlite_opts(move |opts| {
        opts.filename(&filename)
            .read_only(read_only)
            .create_if_missing(false)
            .busy_timeout(busy_timeout)
            .synchronous(sea_orm::sqlx::sqlite::SqliteSynchronous::Full)
    });
    Database::connect(options).await.map_err(|error| {
        io::Error::other(format!(
            "failed to open {role} database '{}': {error}; check the file and its permissions",
            db_path.display()
        ))
    })
}

pub(crate) async fn open_configuration_database(
    db_path: &Path,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    require_configuration_role(role)?;
    let conn = open_literal_connection(db_path, Duration::from_secs(30), role, false).await?;
    ensure_configuration_schema_is_current(&conn, role).await?;
    Ok(conn)
}

pub(crate) async fn create_configuration_database(
    db_path: &Path,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    require_configuration_role(role)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(db_path)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot create {role} database '{}': {error}",
                    db_path.display()
                ),
            )
        })?;
    open_configuration_database(db_path, role).await
}

pub async fn establish_connection_for_role(
    db_path: &str,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    establish_connection_with_busy_timeout_for_role(
        db_path,
        super::DEFAULT_CONNECTION_TIMEOUT,
        role,
    )
    .await
}

/// See [`super::establish_connection_with_busy_timeout`]: the timeout also
/// bounds opening the connection.
pub async fn establish_connection_with_busy_timeout_for_role(
    db_path: &str,
    busy_timeout: Duration,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    establish_connection_within_for_role(db_path, busy_timeout, busy_timeout, role).await
}

/// See [`super::establish_connection_with_lock_timeout`]: the timeout bounds
/// only lock waits, while opening keeps the ordinary budget.
pub async fn establish_connection_with_lock_timeout_for_role(
    db_path: &str,
    lock_timeout: Duration,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    let open_timeout = super::DEFAULT_CONNECTION_TIMEOUT.max(lock_timeout);
    establish_connection_within_for_role(db_path, lock_timeout, open_timeout, role).await
}

async fn establish_connection_within_for_role(
    db_path: &str,
    busy_timeout: Duration,
    open_timeout: Duration,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    reject_direct_capture_open(role)?;
    ledger_for_role(role)?;
    let conn = super::open_connection_without_schema_management_within(
        db_path,
        busy_timeout,
        open_timeout,
    )
    .await?;
    let compatibility = inspect_schema_for_connection(&conn, role).await?;
    reject_future(role, &compatibility)?;
    if matches!(compatibility, SchemaCompatibility::UpgradeRequired { .. }) {
        upgrade_connection_for_role(&conn, role).await?;
    }
    Ok(conn)
}

pub async fn inspect_database_schema_for_role(
    db_path: &Path,
    role: DatabaseRole,
) -> io::Result<SchemaCompatibility> {
    reject_direct_capture_open(role)?;
    ledger_for_role(role)?;
    let conn = super::open_database_without_migrations(db_path).await?;
    let result = inspect_schema_for_connection(&conn, role).await;
    conn.close().await.map_err(|error| {
        io::Error::other(format!("failed to close {role} schema inspection: {error}"))
    })?;
    result
}

pub async fn upgrade_database_schema_for_role(
    db_path: &Path,
    role: DatabaseRole,
) -> io::Result<SchemaUpgradeReport> {
    reject_direct_capture_open(role)?;
    ledger_for_role(role)?;
    let conn = super::open_database_without_migrations(db_path).await?;
    let result = upgrade_connection_for_role(&conn, role).await;
    conn.close().await.map_err(|error| {
        io::Error::other(format!("failed to close {role} schema upgrade: {error}"))
    })?;
    result
}

pub async fn upgrade_connection_for_role(
    conn: &DatabaseConnection,
    role: DatabaseRole,
) -> io::Result<SchemaUpgradeReport> {
    upgrade_connection_with_policy(conn, role, SchemaPolicy::RoleOnly).await
}

async fn upgrade_connection_with_policy(
    conn: &DatabaseConnection,
    role: DatabaseRole,
    policy: SchemaPolicy,
) -> io::Result<SchemaUpgradeReport> {
    reject_direct_capture_open(role)?;
    let ledger = ledger_for_role(role)?;
    inspect_supported_schema(conn, role, policy).await?;
    if ledger == SchemaLedger::Repository {
        return super::apply_database_schema_upgrades(conn).await;
    }

    let migrations = migrations_for_role(role);
    let latest = latest_schema_version_for_role(role)?;
    // Ledger creation, the write lock, the under-lock version recheck, DDL and
    // receipts share one transaction. Failure cannot leave an empty new ledger
    // or advance a receipt without its corresponding schema.
    conn.transaction::<_, _, DbErr>(|txn| {
        Box::pin(upgrade_configuration_transaction(
            txn, role, migrations, latest, policy,
        ))
    })
    .await
    .map_err(|error| {
        io::Error::other(format!(
            "failed to upgrade {role} schema; transaction rolled back: {error}"
        ))
    })
}

async fn upgrade_configuration_transaction(
    txn: &DatabaseTransaction,
    role: DatabaseRole,
    migrations: Vec<Migration>,
    latest: Option<i64>,
    policy: SchemaPolicy,
) -> Result<SchemaUpgradeReport, DbErr> {
    txn.execute_unprepared(CONFIGURATION_LEDGER_SQL).await?;
    txn.execute_unprepared("UPDATE configuration_schema_versions SET version = version WHERE 0")
        .await?;
    let compatibility = inspect_supported_schema(txn, role, policy)
        .await
        .map_err(|error| DbErr::Custom(error.to_string()))?;
    let previous = match compatibility {
        SchemaCompatibility::Compatible {
            current_version, ..
        }
        | SchemaCompatibility::UpgradeRequired {
            current_version, ..
        } => current_version,
        SchemaCompatibility::UnsupportedFuture { .. } => {
            return Err(DbErr::Custom(format!(
                "{role} schema advanced concurrently; install a newer Libra binary"
            )));
        }
    };
    let mut applied = Vec::new();
    if previous != latest {
        for bootstrap in bootstraps_for_role(role) {
            txn.execute_unprepared(bootstrap.sql).await?;
        }
        for top_up in top_ups_for_role(role) {
            match top_up {
                SchemaTopUp::ConfigKv => {
                    txn.execute_unprepared(CONFIG_KV_SQL).await?;
                }
                _ => {
                    return Err(DbErr::Custom(format!(
                        "repository top-up is not permitted for {role}"
                    )));
                }
            }
        }
        for migration in migrations {
            if previous.is_some_and(|version| migration.version <= version) {
                continue;
            }
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO configuration_schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                [
                    migration.version.into(),
                    migration.name.into(),
                    chrono::Utc::now().to_rfc3339().into(),
                ],
            ))
            .await?;
            txn.execute_unprepared(migration.up).await?;
            applied.push(migration.version);
        }
    }
    Ok(SchemaUpgradeReport {
        previous_version: previous,
        current_version: latest,
        latest_version: latest,
        applied_versions: applied,
    })
}

pub async fn create_database_for_role(
    db_path: &str,
    role: DatabaseRole,
) -> io::Result<DatabaseConnection> {
    reject_direct_capture_open(role)?;
    ledger_for_role(role)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(db_path)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot create {role} database '{db_path}': {error}"),
            )
        })?;
    let conn = super::connect_database(db_path).await?;
    if role == DatabaseRole::Repository {
        super::setup_database_sql(&conn).await.map_err(|error| {
            io::Error::other(format!("failed to bootstrap {role} database: {error}"))
        })?;
    }
    upgrade_connection_for_role(&conn, role).await?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::db::migration::{MigrationRunner, WORKTREE_LAYOUT_CAPABILITY_VERSION};

    #[tokio::test]
    async fn worktree_layout_capability_rejects_simulated_old_binary() {
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        conn.execute_unprepared(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL); \
             INSERT INTO schema_versions VALUES (2026101902, 'worktree_layout_capability', 'fixture')",
        )
        .await
        .unwrap();

        let mut old_runner = MigrationRunner::new();
        old_runner
            .extend(
                migrations_for_role(DatabaseRole::Repository)
                    .into_iter()
                    .filter(|migration| migration.version < WORKTREE_LAYOUT_CAPABILITY_VERSION),
            )
            .unwrap();
        let current = old_runner.current_version_readonly(&conn).await.unwrap();
        let old_latest = old_runner.max_registered_version();
        assert_eq!(old_latest, Some(2026101901));
        let compatibility = classify_schema_versions(current, old_latest);
        let error = reject_future(DatabaseRole::Repository, &compatibility).unwrap_err();
        assert_eq!(
            error.to_string(),
            "repository database schema version 2026101902 is newer than this Libra binary supports (latest supported: 2026101901); install a newer Libra binary"
        );
        assert!(matches!(
            inspect_schema_for_connection(&conn, DatabaseRole::Repository)
                .await
                .unwrap(),
            SchemaCompatibility::UpgradeRequired {
                current_version: Some(WORKTREE_LAYOUT_CAPABILITY_VERSION),
                latest_version: 2026103101,
            }
        ));
    }

    #[tokio::test]
    async fn worktree_layout_capability_is_repository_only() {
        let version = WORKTREE_LAYOUT_CAPABILITY_VERSION;
        assert_eq!(
            registered_receipts(SchemaLedger::Repository).get(&version),
            Some(&"worktree_layout_capability")
        );
        assert!(!registered_receipts(SchemaLedger::Configuration).contains_key(&version));
        for role in [DatabaseRole::GlobalConfig, DatabaseRole::SystemConfig] {
            assert!(
                migrations_for_role(role)
                    .iter()
                    .all(|migration| migration.version != version)
            );
            let conn = Database::connect("sqlite::memory:").await.unwrap();
            upgrade_connection_for_role(&conn, role).await.unwrap();
            assert_eq!(
                current_schema_version_for_role(&conn, role).await.unwrap(),
                Some(CONFIGURATION_BASE_VERSION)
            );
            let forbidden = conn
                .query_all_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('schema_versions', 'worktree_registry_capability')",
                ))
                .await
                .unwrap();
            assert!(forbidden.is_empty());
        }
    }
}
