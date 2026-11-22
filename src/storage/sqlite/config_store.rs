//! SQLite implementation of [`ConfigStore`] and [`MetadataStore`] (`RB-03`).
//!
//! Covers the 4 repository config/metadata tables (`schema_versions`, `config`,
//! `config_kv`, `metadata_kv`) and the 3 configuration-database source tables
//! (`config`, `config_kv`, `configuration_schema_versions`).

use std::{path::Path, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, ModelTrait, QueryFilter, QueryOrder, Statement,
    sea_query::OnConflict,
};

use crate::{
    internal::{
        db::{
            DatabaseRole, begin_write_transaction,
            schema::{create_database_for_role, establish_connection_for_role, ledger_for_role},
            sqlite_pool_options,
        },
        model::{config, config_kv, metadata_kv},
    },
    storage::{
        backend::StorageBackendKind,
        config_store::{
            ConfigEntry, ConfigStore, LegacyConfigEntry, MetadataCasOutcome, MetadataEntryRecord,
            MetadataStore, SchemaVersionReceipt, key_in_section, validate_config_role,
            validate_metadata_role, validate_section_name,
        },
        error::{StorageBackendError, StorageResult},
        sqlite::map_sqlite_db_err,
    },
};

/// SQLite-backed implementation of [`ConfigStore`] (and [`MetadataStore`] for
/// [`DatabaseRole::Repository`]).
#[derive(Clone)]
pub struct SqliteConfigStore {
    conn: DatabaseConnection,
    role: DatabaseRole,
    read_only: bool,
}

impl std::fmt::Debug for SqliteConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteConfigStore")
            .field("role", &self.role)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl SqliteConfigStore {
    /// Create a brand-new SQLite database at `path` for `role` (`Repository`,
    /// `GlobalConfig`, or `SystemConfig`) and bootstrap its schema.
    ///
    /// Fails closed if `path` already exists (`AlreadyExists`) or if `role` is
    /// not a configuration-bearing role.
    pub async fn create(path: impl AsRef<Path>, role: DatabaseRole) -> StorageResult<Self> {
        validate_config_role(role)?;
        let path_str =
            path.as_ref()
                .to_str()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "sqlite config database path must be valid UTF-8".to_string(),
                })?;
        let conn = create_database_for_role(path_str, role)
            .await
            .map_err(|err| StorageBackendError::io("create sqlite config store", err))?;
        Ok(Self {
            conn,
            role,
            read_only: false,
        })
    }

    /// Open an existing writable SQLite database at `path` for `role`.
    pub async fn open(path: impl AsRef<Path>, role: DatabaseRole) -> StorageResult<Self> {
        validate_config_role(role)?;
        let path_str =
            path.as_ref()
                .to_str()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "sqlite config database path must be valid UTF-8".to_string(),
                })?;
        let conn = establish_connection_for_role(path_str, role)
            .await
            .map_err(|err| StorageBackendError::io("open sqlite config store", err))?;
        Ok(Self {
            conn,
            role,
            read_only: false,
        })
    }

    /// Open an existing SQLite database strictly read-only without running
    /// schema migrations or creating files.
    pub async fn open_read_only(path: impl AsRef<Path>, role: DatabaseRole) -> StorageResult<Self> {
        validate_config_role(role)?;
        let filename = std::path::absolute(path.as_ref())
            .map_err(|err| StorageBackendError::io("resolve sqlite read-only path", err))?;
        if filename.to_str().is_none() {
            return Err(StorageBackendError::BackendInternal {
                detail: "sqlite config database path must be valid UTF-8".to_string(),
            });
        }
        let mut options = ConnectOptions::new("sqlite://storage-config-readonly");
        options.max_connections(1);
        options.sqlx_logging(false);
        options.map_sqlx_sqlite_pool_opts(sqlite_pool_options);
        options.map_sqlx_sqlite_opts(move |opts| {
            opts.filename(&filename)
                .read_only(true)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(30))
        });
        let conn = Database::connect(options)
            .await
            .map_err(|err| map_sqlite_db_err(role, "open sqlite config store read-only", err))?;
        Ok(Self {
            conn,
            role,
            read_only: true,
        })
    }

    /// Wrap an already-opened SeaORM [`DatabaseConnection`] for `role`.
    pub fn from_connection(
        conn: DatabaseConnection,
        role: DatabaseRole,
        read_only: bool,
    ) -> StorageResult<Self> {
        validate_config_role(role)?;
        Ok(Self {
            conn,
            role,
            read_only,
        })
    }

    fn ensure_writable(&self) -> StorageResult<()> {
        if self.read_only {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot mutate a read-only config store".to_string(),
            });
        }
        Ok(())
    }

    fn schema_ledger_table(&self) -> StorageResult<&'static str> {
        ledger_for_role(self.role)
            .map(|ledger| ledger.table_name())
            .map_err(|err| StorageBackendError::io("resolve schema ledger table", err))
    }
}

fn config_entry_from_model(row: &config_kv::Model) -> ConfigEntry {
    ConfigEntry {
        key: row.key.clone(),
        value: row.value.clone(),
        encrypted: row.encrypted != 0,
    }
}

fn metadata_record_from_model(row: metadata_kv::Model) -> MetadataEntryRecord {
    MetadataEntryRecord {
        scope: row.scope,
        target: row.target,
        key: row.key,
        value: row.value,
        value_type: row.value_type,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

async fn find_metadata_row<C: ConnectionTrait>(
    conn: &C,
    role: DatabaseRole,
    scope: &str,
    target: &str,
    key: &str,
) -> StorageResult<Option<MetadataEntryRecord>> {
    let row = metadata_kv::Entity::find()
        .filter(metadata_kv::Column::Scope.eq(scope))
        .filter(metadata_kv::Column::Target.eq(target))
        .filter(metadata_kv::Column::Key.eq(key))
        .one(conn)
        .await
        .map_err(|err| map_sqlite_db_err(role, "query metadata_kv", err))?;
    Ok(row.map(metadata_record_from_model))
}

#[async_trait]
impl ConfigStore for SqliteConfigStore {
    fn role(&self) -> DatabaseRole {
        self.role
    }

    fn backend(&self) -> StorageBackendKind {
        StorageBackendKind::Sqlite
    }

    async fn get(&self, key: &str) -> StorageResult<Option<ConfigEntry>> {
        let row = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .order_by_desc(config_kv::Column::Id)
            .one(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "get config_kv", err))?;
        Ok(row.as_ref().map(config_entry_from_model))
    }

    async fn get_all(&self, key: &str) -> StorageResult<Vec<ConfigEntry>> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .order_by_asc(config_kv::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "get_all config_kv", err))?;
        Ok(rows.iter().map(config_entry_from_model).collect())
    }

    async fn get_by_prefix(&self, prefix: &str) -> StorageResult<Vec<ConfigEntry>> {
        // Use binary `>=` lower bound (case-sensitive, wildcard-free) plus exact
        // Rust `starts_with` so `%`, `_`, and ASCII case behave identically to redb.
        let mut query = config_kv::Entity::find();
        if !prefix.is_empty() {
            query = query.filter(config_kv::Column::Key.gte(prefix));
        }
        let rows = query
            .order_by_asc(config_kv::Column::Key)
            .order_by_asc(config_kv::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "get_by_prefix config_kv", err))?;
        Ok(rows
            .iter()
            .filter(|row| row.key.starts_with(prefix))
            .map(config_entry_from_model)
            .collect())
    }

    async fn list_all(&self) -> StorageResult<Vec<ConfigEntry>> {
        let rows = config_kv::Entity::find()
            .order_by_asc(config_kv::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "list_all config_kv", err))?;
        Ok(rows.iter().map(config_entry_from_model).collect())
    }

    async fn set(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()> {
        self.ensure_writable()?;
        let role = self.role;
        let key_owned = key.to_owned();
        let value_owned = value.to_owned();
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin set transaction", err))?;

        let all_rows = config_kv::Entity::find()
            .order_by_asc(config_kv::Column::Id)
            .all(&txn)
            .await
            .map_err(|err| {
                map_sqlite_db_err(role, "scan config_kv for case-insensitive set", err)
            })?;
        let mut exact: Vec<config_kv::Model> = Vec::new();
        let mut other_cased: Vec<config_kv::Model> = Vec::new();
        for row in all_rows {
            if row.key == key_owned {
                exact.push(row);
            } else if row.key.eq_ignore_ascii_case(&key_owned) {
                other_cased.push(row);
            }
        }
        if exact.len() > 1 {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot set multi-valued config key; use unset_all or add".to_string(),
            });
        }

        let has_existing_encrypted = exact.iter().any(|row| row.encrypted != 0)
            || other_cased.iter().any(|row| row.encrypted != 0);
        if !encrypted && has_existing_encrypted {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot replace an encrypted configuration value with plaintext"
                    .to_string(),
            });
        }
        for row in other_cased {
            row.delete(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "delete other-cased config_kv", err))?;
        }

        if let Some(row) = exact.into_iter().next() {
            let mut active: config_kv::ActiveModel = row.into();
            active.value = Set(value_owned);
            active.encrypted = Set(i32::from(encrypted));
            active
                .update(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "update config_kv", err))?;
        } else {
            let active = config_kv::ActiveModel {
                key: Set(key_owned),
                value: Set(value_owned),
                encrypted: Set(i32::from(encrypted)),
                ..Default::default()
            };
            active
                .insert(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "insert config_kv", err))?;
        }

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit set transaction", err))?;
        Ok(())
    }

    async fn add(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()> {
        self.ensure_writable()?;
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin add transaction", err))?;

        let existing = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .all(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "query config_kv for add", err))?;
        let has_encrypted = existing.iter().any(|row| row.encrypted != 0);
        let has_plaintext = existing.iter().any(|row| row.encrypted == 0);
        if !existing.is_empty() && ((encrypted && has_plaintext) || (!encrypted && has_encrypted)) {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot mix encrypted and plaintext values for the same config key"
                    .to_string(),
            });
        }

        let active = config_kv::ActiveModel {
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            encrypted: Set(i32::from(encrypted)),
            ..Default::default()
        };
        active
            .insert(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "insert config_kv add", err))?;

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit add transaction", err))?;
        Ok(())
    }

    async fn unset(&self, key: &str) -> StorageResult<usize> {
        self.ensure_writable()?;
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin unset transaction", err))?;

        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .all(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "query config_kv for unset", err))?;
        if rows.len() > 1 {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot unset multi-valued config key; use unset_all".to_string(),
            });
        }
        let removed = if let Some(row) = rows.into_iter().next() {
            row.delete(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "delete config_kv for unset", err))?;
            1
        } else {
            0
        };

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit unset transaction", err))?;
        Ok(removed)
    }

    async fn unset_all(&self, key: &str) -> StorageResult<usize> {
        self.ensure_writable()?;
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin unset_all transaction", err))?;

        let result = config_kv::Entity::delete_many()
            .filter(config_kv::Column::Key.eq(key))
            .exec(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "delete_many config_kv for unset_all", err))?;

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit unset_all transaction", err))?;
        Ok(result.rows_affected as usize)
    }

    async fn remove_section(&self, section: &str) -> StorageResult<usize> {
        self.ensure_writable()?;
        validate_section_name(section)?;
        let role = self.role;
        let prefix = format!("{section}.");
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin remove_section transaction", err))?;

        let candidates = config_kv::Entity::find()
            .filter(config_kv::Column::Key.gte(&prefix))
            .order_by_asc(config_kv::Column::Id)
            .all(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "query section members for remove", err))?;
        let members: Vec<config_kv::Model> = candidates
            .into_iter()
            .filter(|row| row.key.starts_with(&prefix) && key_in_section(&row.key, section))
            .collect();
        if members.is_empty() {
            return Err(StorageBackendError::NotFound {
                resource: "config section".to_string(),
            });
        }
        let removed = members.len();
        for row in members {
            row.delete(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "delete section member", err))?;
        }

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit remove_section transaction", err))?;
        Ok(removed)
    }

    async fn rename_section(&self, old_section: &str, new_section: &str) -> StorageResult<usize> {
        self.ensure_writable()?;
        validate_section_name(old_section)?;
        validate_section_name(new_section)?;
        if old_section == new_section {
            return Err(StorageBackendError::BackendInternal {
                detail: "source and destination config sections are identical".to_string(),
            });
        }

        let role = self.role;
        let old_prefix = format!("{old_section}.");
        let new_prefix = format!("{new_section}.");
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin rename_section transaction", err))?;

        let source_candidates = config_kv::Entity::find()
            .filter(config_kv::Column::Key.gte(&old_prefix))
            .order_by_asc(config_kv::Column::Key)
            .order_by_asc(config_kv::Column::Id)
            .all(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "query source section for rename", err))?;
        let source: Vec<config_kv::Model> = source_candidates
            .into_iter()
            .filter(|row| row.key.starts_with(&old_prefix) && key_in_section(&row.key, old_section))
            .collect();
        if source.is_empty() {
            return Err(StorageBackendError::NotFound {
                resource: "config section".to_string(),
            });
        }

        let dest_candidates = config_kv::Entity::find()
            .filter(config_kv::Column::Key.gte(&new_prefix))
            .all(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "query destination section for rename", err))?;
        if dest_candidates
            .iter()
            .any(|row| row.key.starts_with(&new_prefix) && key_in_section(&row.key, new_section))
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "destination config section already exists".to_string(),
            });
        }

        let moved = source.len();
        for row in &source {
            let var_name = row.key.strip_prefix(&old_prefix).ok_or_else(|| {
                StorageBackendError::BackendInternal {
                    detail: "config key failed section prefix extraction".to_string(),
                }
            })?;
            let new_key = format!("{new_section}.{var_name}");
            let active = config_kv::ActiveModel {
                key: Set(new_key),
                value: Set(row.value.clone()),
                encrypted: Set(row.encrypted),
                ..Default::default()
            };
            active
                .insert(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "insert renamed section entry", err))?;
        }
        for row in source {
            row.delete(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "delete old section entry", err))?;
        }

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit rename_section transaction", err))?;
        Ok(moved)
    }

    async fn list_legacy_entries(&self) -> StorageResult<Vec<LegacyConfigEntry>> {
        let rows = config::Entity::find()
            .order_by_asc(config::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "list legacy config table", err))?;
        Ok(rows
            .into_iter()
            .map(|row| LegacyConfigEntry {
                configuration: row.configuration,
                name: row.name,
                key: row.key,
                value: row.value,
            })
            .collect())
    }

    async fn insert_legacy_entry(&self, entry: &LegacyConfigEntry) -> StorageResult<()> {
        self.ensure_writable()?;
        let active = config::ActiveModel {
            configuration: Set(entry.configuration.clone()),
            name: Set(entry.name.clone()),
            key: Set(entry.key.clone()),
            value: Set(entry.value.clone()),
            ..Default::default()
        };
        active
            .insert(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "insert legacy config row", err))?;
        Ok(())
    }

    async fn list_schema_receipts(&self) -> StorageResult<Vec<SchemaVersionReceipt>> {
        let table = self.schema_ledger_table()?;
        let sql = format!("SELECT version, name, applied_at FROM {table} ORDER BY version ASC");
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                self.conn.get_database_backend(),
                sql,
            ))
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "list schema ledger receipts", err))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let version: i64 = row
                .try_get_by("version")
                .map_err(|err| map_sqlite_db_err(self.role, "decode schema version", err))?;
            let name: String = row
                .try_get_by("name")
                .map_err(|err| map_sqlite_db_err(self.role, "decode schema name", err))?;
            let applied_at: String = row
                .try_get_by("applied_at")
                .map_err(|err| map_sqlite_db_err(self.role, "decode schema applied_at", err))?;
            out.push(SchemaVersionReceipt {
                version,
                name,
                applied_at,
            });
        }
        Ok(out)
    }

    async fn record_schema_receipt(&self, receipt: &SchemaVersionReceipt) -> StorageResult<()> {
        self.ensure_writable()?;
        let table = self.schema_ledger_table()?;
        let sql = format!(
            "INSERT INTO {table} (version, name, applied_at) VALUES (?, ?, ?) \
             ON CONFLICT(version) DO UPDATE SET name = excluded.name, applied_at = excluded.applied_at"
        );
        self.conn
            .execute_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                sql,
                [
                    receipt.version.into(),
                    receipt.name.clone().into(),
                    receipt.applied_at.clone().into(),
                ],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "record schema ledger receipt", err))?;
        Ok(())
    }
}

#[async_trait]
impl MetadataStore for SqliteConfigStore {
    async fn get_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
    ) -> StorageResult<Option<MetadataEntryRecord>> {
        validate_metadata_role(self.role)?;
        find_metadata_row(&self.conn, self.role, scope, target, key).await
    }

    async fn set_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        value: &str,
        value_type: &str,
    ) -> StorageResult<Option<String>> {
        validate_metadata_role(self.role)?;
        self.ensure_writable()?;
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin set_metadata transaction", err))?;

        let previous = find_metadata_row(&txn, role, scope, target, key)
            .await?
            .map(|r| r.value);
        let now = Utc::now().to_rfc3339();
        let active = metadata_kv::ActiveModel {
            scope: Set(scope.to_owned()),
            target: Set(target.to_owned()),
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            value_type: Set(value_type.to_owned()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        };
        let on_conflict = OnConflict::columns([
            metadata_kv::Column::Scope,
            metadata_kv::Column::Target,
            metadata_kv::Column::Key,
        ])
        .update_columns([
            metadata_kv::Column::Value,
            metadata_kv::Column::ValueType,
            metadata_kv::Column::UpdatedAt,
        ])
        .to_owned();
        metadata_kv::Entity::insert(active)
            .on_conflict(on_conflict)
            .exec(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "upsert metadata_kv", err))?;

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit set_metadata transaction", err))?;
        Ok(previous)
    }

    async fn cas_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        expected: Option<&str>,
        new_value: Option<(&str, &str)>,
    ) -> StorageResult<MetadataCasOutcome> {
        validate_metadata_role(self.role)?;
        self.ensure_writable()?;
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin cas_metadata transaction", err))?;

        let outcome = match (expected, new_value) {
            (None, Some((val, vtype))) => {
                let now = Utc::now().to_rfc3339();
                let res = txn
                    .execute_raw(Statement::from_sql_and_values(
                        txn.get_database_backend(),
                        "INSERT OR IGNORE INTO metadata_kv \
                         (scope, target, key, value, value_type, created_at, updated_at) \
                         VALUES (?, ?, ?, ?, ?, ?, ?)",
                        [
                            scope.into(),
                            target.into(),
                            key.into(),
                            val.into(),
                            vtype.into(),
                            now.clone().into(),
                            now.into(),
                        ],
                    ))
                    .await
                    .map_err(|err| map_sqlite_db_err(role, "cas_metadata insert-if-absent", err))?;
                let actual = find_metadata_row(&txn, role, scope, target, key).await?;
                if res.rows_affected() == 1 {
                    MetadataCasOutcome::Swapped {
                        previous: None,
                        current: actual,
                    }
                } else {
                    MetadataCasOutcome::Conflict { actual }
                }
            }
            (Some(exp), Some((val, vtype))) => {
                let existing = find_metadata_row(&txn, role, scope, target, key).await?;
                if existing.as_ref().map(|r| r.value.as_str()) != Some(exp) {
                    MetadataCasOutcome::Conflict { actual: existing }
                } else {
                    let now = Utc::now().to_rfc3339();
                    let res = txn
                        .execute_raw(Statement::from_sql_and_values(
                            txn.get_database_backend(),
                            "UPDATE metadata_kv SET value = ?, value_type = ?, updated_at = ? \
                             WHERE scope = ? AND target = ? AND key = ? AND value = ?",
                            [
                                val.into(),
                                vtype.into(),
                                now.into(),
                                scope.into(),
                                target.into(),
                                key.into(),
                                exp.into(),
                            ],
                        ))
                        .await
                        .map_err(|err| {
                            map_sqlite_db_err(role, "cas_metadata conditional update", err)
                        })?;
                    let current = find_metadata_row(&txn, role, scope, target, key).await?;
                    if res.rows_affected() == 1 {
                        MetadataCasOutcome::Swapped {
                            previous: existing,
                            current,
                        }
                    } else {
                        MetadataCasOutcome::Conflict { actual: current }
                    }
                }
            }
            (Some(exp), None) => {
                let existing = find_metadata_row(&txn, role, scope, target, key).await?;
                if existing.as_ref().map(|r| r.value.as_str()) != Some(exp) {
                    MetadataCasOutcome::Conflict { actual: existing }
                } else {
                    let res = txn
                        .execute_raw(Statement::from_sql_and_values(
                            txn.get_database_backend(),
                            "DELETE FROM metadata_kv \
                             WHERE scope = ? AND target = ? AND key = ? AND value = ?",
                            [scope.into(), target.into(), key.into(), exp.into()],
                        ))
                        .await
                        .map_err(|err| {
                            map_sqlite_db_err(role, "cas_metadata conditional delete", err)
                        })?;
                    if res.rows_affected() == 1 {
                        MetadataCasOutcome::Swapped {
                            previous: existing,
                            current: None,
                        }
                    } else {
                        let actual = find_metadata_row(&txn, role, scope, target, key).await?;
                        MetadataCasOutcome::Conflict { actual }
                    }
                }
            }
            (None, None) => {
                let existing = find_metadata_row(&txn, role, scope, target, key).await?;
                if existing.is_none() {
                    MetadataCasOutcome::Swapped {
                        previous: None,
                        current: None,
                    }
                } else {
                    MetadataCasOutcome::Conflict { actual: existing }
                }
            }
        };

        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit cas_metadata transaction", err))?;
        Ok(outcome)
    }

    async fn unset_metadata(&self, scope: &str, target: &str, key: &str) -> StorageResult<bool> {
        validate_metadata_role(self.role)?;
        self.ensure_writable()?;
        let res = metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(target))
            .filter(metadata_kv::Column::Key.eq(key))
            .exec(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "unset metadata_kv", err))?;
        Ok(res.rows_affected > 0)
    }

    async fn list_metadata(
        &self,
        scope: &str,
        target: &str,
        key_prefix: Option<&str>,
    ) -> StorageResult<Vec<MetadataEntryRecord>> {
        validate_metadata_role(self.role)?;
        let mut query = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(target));
        if let Some(prefix) = key_prefix
            && !prefix.is_empty()
        {
            query = query.filter(metadata_kv::Column::Key.gte(prefix));
        }
        let rows = query
            .order_by_asc(metadata_kv::Column::Key)
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "list metadata_kv", err))?;
        Ok(rows
            .into_iter()
            .filter(|row| key_prefix.is_none_or(|prefix| row.key.starts_with(prefix)))
            .map(metadata_record_from_model)
            .collect())
    }

    async fn list_metadata_scope(&self, scope: &str) -> StorageResult<Vec<MetadataEntryRecord>> {
        validate_metadata_role(self.role)?;
        let rows = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .order_by_asc(metadata_kv::Column::Target)
            .order_by_asc(metadata_kv::Column::Key)
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "list_metadata_scope", err))?;
        Ok(rows.into_iter().map(metadata_record_from_model).collect())
    }

    async fn delete_metadata_target(&self, scope: &str, target: &str) -> StorageResult<u64> {
        validate_metadata_role(self.role)?;
        self.ensure_writable()?;
        let res = metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(target))
            .exec(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(self.role, "delete_metadata_target", err))?;
        Ok(res.rows_affected)
    }

    async fn rename_metadata_target(
        &self,
        scope: &str,
        old_target: &str,
        new_target: &str,
    ) -> StorageResult<()> {
        validate_metadata_role(self.role)?;
        self.ensure_writable()?;
        if old_target == new_target {
            return Ok(());
        }
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin rename_metadata_target", err))?;
        metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(new_target))
            .exec(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "clear destination metadata target", err))?;
        metadata_kv::Entity::update_many()
            .col_expr(
                metadata_kv::Column::Target,
                sea_orm::sea_query::Expr::value(new_target),
            )
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(old_target))
            .exec(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "rename metadata target", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit rename_metadata_target", err))?;
        Ok(())
    }

    async fn copy_metadata_target(
        &self,
        scope: &str,
        from_target: &str,
        to_target: &str,
    ) -> StorageResult<()> {
        validate_metadata_role(self.role)?;
        self.ensure_writable()?;
        if from_target == to_target {
            return Ok(());
        }
        let role = self.role;
        let txn = begin_write_transaction(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "begin copy_metadata_target", err))?;
        metadata_kv::Entity::delete_many()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(to_target))
            .exec(&txn)
            .await
            .map_err(|err| {
                map_sqlite_db_err(role, "clear copy destination metadata target", err)
            })?;
        let entries = metadata_kv::Entity::find()
            .filter(metadata_kv::Column::Scope.eq(scope))
            .filter(metadata_kv::Column::Target.eq(from_target))
            .order_by_asc(metadata_kv::Column::Key)
            .all(&txn)
            .await
            .map_err(|err| map_sqlite_db_err(role, "read source metadata target for copy", err))?;
        let now = Utc::now().to_rfc3339();
        for entry in entries {
            let active = metadata_kv::ActiveModel {
                scope: Set(entry.scope),
                target: Set(to_target.to_owned()),
                key: Set(entry.key),
                value: Set(entry.value),
                value_type: Set(entry.value_type),
                created_at: Set(now.clone()),
                updated_at: Set(now.clone()),
                ..Default::default()
            };
            active
                .insert(&txn)
                .await
                .map_err(|err| map_sqlite_db_err(role, "insert copied metadata entry", err))?;
        }
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(role, "commit copy_metadata_target", err))?;
        Ok(())
    }
}
