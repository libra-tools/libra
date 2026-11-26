//! Scoped configuration access layer (`ConfigScope` and `ScopedConfig`) sunk
//! from `command::config`.

use std::path::PathBuf;

use once_cell::sync::Lazy;
use sea_orm::{DatabaseConnection, DatabaseTransaction};
use tokio::sync::Mutex;

use crate::{
    internal::{
        config::{ConfigKv, ConfigKvEntry},
        db::{
            DatabaseRole, begin_write_transaction, get_db_conn_instance,
            schema::{
                create_configuration_database, ensure_configuration_schema_is_current,
                open_configuration_database,
            },
            write_configuration_barrier,
        },
    },
    utils::util::{DATABASE, try_get_storage_path},
};

/// Cached database connection for Global scope, paired with the resolved DB path.
static GLOBAL_CONFIG_CONN: Lazy<Mutex<Option<(PathBuf, DatabaseConnection)>>> =
    Lazy::new(|| Mutex::new(None));

/// Cached database connection for System scope, paired with the resolved DB path.
static SYSTEM_CONFIG_CONN: Lazy<Mutex<Option<(PathBuf, DatabaseConnection)>>> =
    Lazy::new(|| Mutex::new(None));

fn scope_name(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Local => "local",
        ConfigScope::Global => "global",
        ConfigScope::System => "system",
    }
}

fn is_core_objectformat_key(key: &str) -> bool {
    let key = key.trim();
    let Some((section, variable)) = key.split_once('.') else {
        return false;
    };
    if variable.contains('.') {
        return false;
    }
    section == "core" && variable.eq_ignore_ascii_case("objectformat")
}

/// Configuration scope that determines where values are stored and retrieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// Repository-specific (`.libra/libra.db`). Default for writes.
    Local,
    /// User-level (`<config dir>/libra/config.db`, falling back to the legacy
    /// `~/.libra/config.db` until the migration lands).
    Global,
    /// System-wide (`/etc/libra/config.db`, overridable via
    /// `LIBRA_CONFIG_SYSTEM_DB`). Lowest precedence; writing it usually needs
    /// elevated privileges, like Git's `/etc/gitconfig`.
    System,
}

impl ConfigScope {
    pub fn database_role(self) -> DatabaseRole {
        match self {
            Self::Local => DatabaseRole::Repository,
            Self::Global => DatabaseRole::GlobalConfig,
            Self::System => DatabaseRole::SystemConfig,
        }
    }

    /// Cascade order for reads (highest to lowest precedence): local overrides
    /// global, which overrides system — matching Git.
    pub const CASCADE_ORDER: [ConfigScope; 3] =
        [ConfigScope::Local, ConfigScope::Global, ConfigScope::System];

    /// Read/list order (lowest to highest precedence, outer scopes before inner scopes):
    /// system -> global -> local. Matching Git's listing order.
    pub const SYSTEM_TO_LOCAL_ORDER: [ConfigScope; 3] =
        [ConfigScope::System, ConfigScope::Global, ConfigScope::Local];

    /// Get the config database path for this scope.
    pub fn get_config_path(&self) -> Option<PathBuf> {
        match self {
            ConfigScope::Local => None,
            ConfigScope::Global => crate::internal::config::global_config_path(),
            ConfigScope::System => {
                if let Some(p) = std::env::var_os("LIBRA_CONFIG_SYSTEM_DB") {
                    return Some(PathBuf::from(p));
                }
                Some(PathBuf::from("/etc/libra/config.db"))
            }
        }
    }

    pub async fn ensure_config_exists(&self) -> Result<(), String> {
        if *self != ConfigScope::Local {
            ScopedConfig::get_connection(*self).await?;
        }
        Ok(())
    }
}

/// Scoped config access layer — resolves the correct database for each scope.
pub struct ScopedConfig;

impl ScopedConfig {
    /// Get a database connection for the specified scope.
    pub async fn get_connection(scope: ConfigScope) -> Result<DatabaseConnection, String> {
        Self::get_connection_internal(scope, true).await
    }

    /// Get a database connection for read-only access (does not create the database file if missing).
    pub async fn get_connection_for_read(scope: ConfigScope) -> Result<DatabaseConnection, String> {
        Self::get_connection_internal(scope, false).await
    }

    async fn get_connection_internal(
        scope: ConfigScope,
        create_if_missing: bool,
    ) -> Result<DatabaseConnection, String> {
        match scope {
            ConfigScope::Local => {
                let storage = try_get_storage_path(None).map_err(|_| {
                    "fatal: not a libra repository (or any of the parent directories): .libra"
                        .to_string()
                })?;
                let db_path = storage.join(DATABASE);
                if !db_path.exists() {
                    return Err(format!(
                        "fatal: libra database not found at '{}'",
                        db_path.display()
                    ));
                }
                Ok(get_db_conn_instance().await.clone())
            }
            ConfigScope::Global => {
                Self::get_or_create_cached_connection(
                    &GLOBAL_CONFIG_CONN,
                    scope,
                    "global",
                    create_if_missing,
                )
                .await
            }
            ConfigScope::System => {
                Self::get_or_create_cached_connection(
                    &SYSTEM_CONFIG_CONN,
                    scope,
                    "system",
                    create_if_missing,
                )
                .await
            }
        }
    }

    async fn get_or_create_cached_connection(
        cache: &Lazy<Mutex<Option<(PathBuf, DatabaseConnection)>>>,
        scope: ConfigScope,
        scope_name: &str,
        create_if_missing: bool,
    ) -> Result<DatabaseConnection, String> {
        let config_path = match scope {
            ConfigScope::Global => crate::internal::config::global_config_path_for_read().await,
            _ => scope.get_config_path(),
        };
        let Some(config_path) = config_path else {
            return Err(format!(
                "Could not determine config path for {scope_name} scope"
            ));
        };
        let mut guard = cache.lock().await;
        let role = scope.database_role();
        if let Some((cached_path, cached_conn)) = guard.as_ref() {
            if cached_path == &config_path {
                if !config_path.exists() {
                    *guard = None;
                } else {
                    ensure_configuration_schema_is_current(cached_conn, role)
                        .await
                        .map_err(|error| {
                            format!("Failed to validate {scope_name} config database: {error}")
                        })?;
                    return Ok(cached_conn.clone());
                }
            } else {
                *guard = None;
            }
        }

        let exists = match std::fs::File::open(&config_path) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(format!(
                    "UNREADABLE:{}:Permission denied",
                    config_path.display()
                ));
            }
            Err(e) => match config_path.try_exists() {
                Ok(true) => {
                    return Err(format!("UNREADABLE:{}:{e}", config_path.display()));
                }
                Ok(false) => false,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    return Err(format!(
                        "UNREADABLE:{}:Permission denied",
                        config_path.display()
                    ));
                }
                Err(err) => {
                    return Err(format!("UNREADABLE:{}:{err}", config_path.display()));
                }
            },
        };

        if !exists && !create_if_missing {
            return Err(format!("NOT_FOUND:{}", config_path.display()));
        }

        let conn = if exists {
            match open_configuration_database(&config_path, role).await {
                Ok(conn) => conn,
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::PermissionDenied {
                        return Err(format!(
                            "UNREADABLE:{}:Permission denied",
                            config_path.display()
                        ));
                    }
                    return Err(format!(
                        "Failed to connect to {scope_name} config database: {error}"
                    ));
                }
            }
        } else {
            if let Some(parent) = config_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "Failed to create {scope_name} config directory '{}': {error}{}",
                        parent.display(),
                        if scope == ConfigScope::System {
                            " (writing system config usually requires elevated privileges)"
                        } else {
                            ""
                        }
                    )
                })?;
            }
            match create_configuration_database(&config_path, role).await {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_configuration_database(&config_path, role).await
                }
                result => result,
            }
            .map_err(|error| {
                format!("Failed to connect to {scope_name} config database: {error}")
            })?
        };
        *guard = Some((config_path, conn.clone()));
        Ok(conn)
    }

    #[doc(hidden)]
    pub async fn begin_mutation(scope: ConfigScope) -> Result<DatabaseTransaction, String> {
        if scope == ConfigScope::Global {
            crate::internal::config::global_config_path_for_write().await?;
        }
        let conn = Self::get_connection(scope).await?;
        let txn = begin_write_transaction(&conn).await.map_err(|error| {
            format!(
                "failed to start {} config transaction: {error}",
                scope_name(scope)
            )
        })?;
        if scope != ConfigScope::Local {
            write_configuration_barrier(&txn, scope.database_role())
                .await
                .map_err(|error| {
                    format!(
                        "failed to protect {} config before writing: {error}",
                        scope_name(scope)
                    )
                })?;
        }
        Ok(txn)
    }

    pub async fn get(scope: ConfigScope, key: &str) -> Result<Option<ConfigKvEntry>, String> {
        let conn = Self::get_connection_for_read(scope).await?;
        ConfigKv::get_with_conn(&conn, key)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_all(scope: ConfigScope, key: &str) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection_for_read(scope).await?;
        ConfigKv::get_all_with_conn(&conn, key)
            .await
            .map_err(|e| e.to_string())
    }

    #[doc(hidden)]
    pub async fn get_set_replacement_entries(
        scope: ConfigScope,
        key: &str,
    ) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection(scope).await?;
        ConfigKv::select_set_replacement_with_conn(&conn, key)
            .await
            .map(|replacement| replacement.entries())
            .map_err(|error| error.to_string())
    }

    pub async fn set(
        scope: ConfigScope,
        key: &str,
        value: &str,
        encrypted: bool,
    ) -> Result<(), String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        if let Err(error) = ConfigKv::set_prepared_with_conn(&txn, key, value, encrypted).await {
            txn.rollback().await.map_err(|rollback| {
                format!("failed to roll back rejected configuration update ({error}): {rollback}")
            })?;
            return Err(format!("failed to set config '{key}': {error}"));
        }
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))
    }

    pub async fn add(
        scope: ConfigScope,
        key: &str,
        value: &str,
        encrypted: bool,
    ) -> Result<(), String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        ConfigKv::add_with_conn(&txn, key, value, encrypted)
            .await
            .map_err(|e| format!("failed to add config '{key}': {e}"))?;
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))
    }

    pub async fn unset(scope: ConfigScope, key: &str) -> Result<usize, String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        let removed = ConfigKv::unset_with_conn(&txn, key)
            .await
            .map_err(|e| format!("failed to unset config '{key}': {e}"))?;
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))?;
        Ok(removed)
    }

    pub async fn unset_matching(
        scope: ConfigScope,
        key: &str,
        matcher: impl Fn(&str) -> bool,
    ) -> Result<usize, String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        let removed = ConfigKv::unset_matching_with_conn(&txn, key, matcher)
            .await
            .map_err(|e| format!("failed to unset config '{key}': {e}"))?;
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))?;
        Ok(removed)
    }

    pub async fn replace_all(scope: ConfigScope, key: &str, value: &str) -> Result<(), String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        ConfigKv::unset_all_with_conn(&txn, key)
            .await
            .map_err(|e| format!("failed to replace config '{key}': {e}"))?;
        ConfigKv::set_with_conn(&txn, key, value, false)
            .await
            .map_err(|e| format!("failed to replace config '{key}': {e}"))?;
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))
    }

    /// Git `--replace-all <key> <value> <pattern>`: replace the first matching
    /// value in place and drop any later matches, leaving other values in order.
    pub async fn replace_matching(
        scope: ConfigScope,
        key: &str,
        matcher: impl Fn(&str) -> bool,
        value: &str,
        encrypted: bool,
    ) -> Result<usize, String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        let entries = ConfigKv::get_all_with_conn(&txn, key)
            .await
            .map_err(|e| format!("failed to read config '{key}': {e}"))?;
        ConfigKv::unset_all_with_conn(&txn, key)
            .await
            .map_err(|e| format!("failed to replace config '{key}': {e}"))?;
        let mut replaced = 0;
        for entry in entries {
            if matcher(&entry.value) {
                if replaced == 0 {
                    ConfigKv::add_with_conn(&txn, key, value, encrypted)
                        .await
                        .map_err(|e| format!("failed to replace config '{key}': {e}"))?;
                }
                replaced += 1;
            } else {
                ConfigKv::add_with_conn(&txn, key, &entry.value, entry.encrypted)
                    .await
                    .map_err(|e| format!("failed to replace config '{key}': {e}"))?;
            }
        }
        if replaced == 0 {
            ConfigKv::add_with_conn(&txn, key, value, encrypted)
                .await
                .map_err(|e| format!("failed to replace config '{key}': {e}"))?;
        }
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))?;
        Ok(replaced)
    }

    pub async fn unset_all(scope: ConfigScope, key: &str) -> Result<usize, String> {
        if scope == ConfigScope::Local && is_core_objectformat_key(key) {
            return Err(
                "core.objectformat cannot be changed after init; the object format is fixed at fresh init"
                    .to_string(),
            );
        }
        let txn = Self::begin_mutation(scope).await?;
        let removed = ConfigKv::unset_all_with_conn(&txn, key)
            .await
            .map_err(|e| format!("failed to unset config '{key}': {e}"))?;
        txn.commit()
            .await
            .map_err(|e| format!("failed to commit config update: {e}"))?;
        Ok(removed)
    }

    pub async fn list_all(scope: ConfigScope) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection_for_read(scope).await?;
        ConfigKv::list_all_with_conn(&conn)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_by_prefix(
        scope: ConfigScope,
        prefix: &str,
    ) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection_for_read(scope).await?;
        ConfigKv::get_by_prefix_with_conn(&conn, prefix)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_regexp(
        scope: ConfigScope,
        pattern: &str,
    ) -> Result<Vec<ConfigKvEntry>, String> {
        let conn = Self::get_connection_for_read(scope).await?;
        ConfigKv::get_regexp_with_conn(&conn, pattern)
            .await
            .map_err(|e| e.to_string())
    }
}
