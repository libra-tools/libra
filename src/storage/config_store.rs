//! Object-safe [`ConfigStore`] and [`MetadataStore`] storage traits and shared
//! domain records (`ADR-RB-04`, `RB-03`).
//!
//! Covers the 4 repository configuration/metadata tables (`schema_versions`,
//! `config`, `config_kv`, `metadata_kv`) and the 3 configuration-database
//! source tables (`config`, `config_kv`, `configuration_schema_versions`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        backend::StorageBackendKind,
        error::{StorageBackendError, StorageResult},
    },
};

/// Environment variable overriding the global configuration database path (`GC-13`).
pub const LIBRA_CONFIG_GLOBAL_DB_ENV: &str = "LIBRA_CONFIG_GLOBAL_DB";

/// Environment variable overriding the system configuration database path (`GC-13`).
pub const LIBRA_CONFIG_SYSTEM_DB_ENV: &str = "LIBRA_CONFIG_SYSTEM_DB";

/// Canonical `redb` configuration filename inside a `GlobalConfig` or `SystemConfig` directory.
pub const CONFIG_REDB_FILE_NAME: &str = "config.redb";

/// Canonical SQLite configuration filename inside a `GlobalConfig` or `SystemConfig` directory.
pub const CONFIG_SQLITE_FILE_NAME: &str = "config.db";

/// Required Unix permission mode for repository `redb` configuration/git stores (`0600`).
#[cfg(unix)]
pub const REPOSITORY_CONFIG_FILE_MODE: u32 = 0o600;

/// Required Unix permission mode for `GlobalConfig` `config.redb` (`0600`, private to owner).
#[cfg(unix)]
pub const GLOBAL_CONFIG_FILE_MODE: u32 = 0o600;

/// Required Unix permission mode for `SystemConfig` `config.redb` (`0644`, world-readable).
#[cfg(unix)]
pub const SYSTEM_CONFIG_FILE_MODE: u32 = 0o644;

/// Return the exact Unix POSIX mode bits required for a configuration-bearing
/// [`DatabaseRole`], or `None` for non-config roles (`GC-13`, `RB-03j`).
#[cfg(unix)]
#[must_use]
pub const fn expected_config_file_mode(role: DatabaseRole) -> Option<u32> {
    match role {
        DatabaseRole::Repository => Some(REPOSITORY_CONFIG_FILE_MODE),
        DatabaseRole::GlobalConfig => Some(GLOBAL_CONFIG_FILE_MODE),
        DatabaseRole::SystemConfig => Some(SYSTEM_CONFIG_FILE_MODE),
        DatabaseRole::Capture | DatabaseRole::Derived => None,
    }
}

/// Extract the parent scope directory from a global/system configuration DB
/// path (such as `LIBRA_CONFIG_GLOBAL_DB` or `LIBRA_CONFIG_SYSTEM_DB`).
pub fn config_scope_dir_from_db_path(db_path: &Path) -> StorageResult<PathBuf> {
    let parent = db_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "configuration database path must include a parent scope directory".to_string(),
        })?;
    Ok(parent.to_path_buf())
}

/// Return `<scope_dir>/config.redb`.
#[must_use]
pub fn config_redb_path_in_scope_dir(scope_dir: &Path) -> PathBuf {
    scope_dir.join(CONFIG_REDB_FILE_NAME)
}

/// Return `<scope_dir>/config.db`.
#[must_use]
pub fn config_sqlite_path_in_scope_dir(scope_dir: &Path) -> PathBuf {
    scope_dir.join(CONFIG_SQLITE_FILE_NAME)
}

/// One flat configuration entry stored in `config_kv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    /// Full dotted config key, e.g. `remote.origin.fetch` or `user.name`.
    pub key: String,
    /// Plaintext value, or hex-encoded vault ciphertext when `encrypted == true`.
    pub value: String,
    /// Whether `value` is vault-encrypted ciphertext.
    pub encrypted: bool,
}

/// One legacy 3-column configuration row stored in `config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyConfigEntry {
    /// Section name (`[configuration "name"]`).
    pub configuration: String,
    /// Optional subsection name.
    pub name: Option<String>,
    /// Variable name within the section.
    pub key: String,
    /// Variable value.
    pub value: String,
}

/// One schema migration receipt stored in `schema_versions` (repository) or
/// `configuration_schema_versions` (global/system configuration).
///
/// Initial migration receipts written at store creation record an RFC 3339 UTC
/// timestamp (`chrono::Utc::now().to_rfc3339()`), matching
/// `src/internal/db/migration.rs` and `src/internal/db/schema.rs`. Explicit
/// receipts persisted via [`ConfigStore::record_schema_receipt`] round-trip
/// `applied_at` verbatim across both SQLite and `redb`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaVersionReceipt {
    /// Monotonic numeric schema migration version.
    pub version: i64,
    /// Human-readable migration name.
    pub name: String,
    /// RFC 3339 UTC timestamp string recording when the receipt was applied.
    pub applied_at: String,
}

/// One scoped metadata record stored in `metadata_kv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataEntryRecord {
    /// Metadata scope (e.g. `"branch"`, `"agent_traces_inflight"`).
    pub scope: String,
    /// Opaque scope target (e.g. local branch short name or session ID).
    pub target: String,
    /// Metadata key within `(scope, target)`.
    pub key: String,
    /// Metadata value payload.
    pub value: String,
    /// Type tag (e.g. `"text"`, `"integer"`).
    pub value_type: String,
    /// ISO-8601 UTC creation timestamp.
    pub created_at: String,
    /// ISO-8601 UTC last-update timestamp.
    pub updated_at: String,
}

/// Result of an atomic compare-and-swap operation on `metadata_kv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataCasOutcome {
    /// The expected value matched and the mutation committed atomically.
    Swapped {
        /// Prior record before the swap (`None` for insert-if-absent).
        previous: Option<MetadataEntryRecord>,
        /// Resulting record after the swap (`None` for compare-and-delete).
        current: Option<MetadataEntryRecord>,
    },
    /// The expected value did not match; no write occurred.
    Conflict {
        /// Actual record currently stored in `metadata_kv`.
        actual: Option<MetadataEntryRecord>,
    },
}

/// Validate that `role` is allowed to back a [`ConfigStore`] (`Repository`,
/// `GlobalConfig`, or `SystemConfig`).
pub(crate) fn validate_config_role(role: DatabaseRole) -> StorageResult<()> {
    match role {
        DatabaseRole::Repository | DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => {
            Ok(())
        }
        other => Err(StorageBackendError::BackendInternal {
            detail: format!("{other} database role cannot back a ConfigStore"),
        }),
    }
}

/// Validate that `role` is [`DatabaseRole::Repository`] before accessing
/// repository-only `metadata_kv`.
pub(crate) fn validate_metadata_role(role: DatabaseRole) -> StorageResult<()> {
    if role != DatabaseRole::Repository {
        return Err(StorageBackendError::RoleMismatch {
            expected: DatabaseRole::Repository,
            found: role,
        });
    }
    Ok(())
}

/// Validate a Git section or `section.subsection` identifier for
/// `remove_section` / `rename_section`.
pub(crate) fn validate_section_name(section: &str) -> StorageResult<()> {
    if section.is_empty() || section.starts_with('.') || section.ends_with('.') {
        return Err(StorageBackendError::BackendInternal {
            detail: "invalid config section name".to_string(),
        });
    }
    Ok(())
}

/// Returns whether `key` belongs to Git `section` (`section` or
/// `section.subsection`), matching Git's exact section/subsection identity
/// (`src/command/config.rs::key_in_section`).
///
/// Bare section `"branch"` matches `"branch.autosetup"` but NOT
/// `"branch.main.remote"` (which belongs to subsection `"branch.main"`).
#[must_use]
pub fn key_in_section(key: &str, section: &str) -> bool {
    if section.is_empty() || section.starts_with('.') || section.ends_with('.') {
        return false;
    }
    let Some((key_sec, key_rest)) = key.split_once('.') else {
        return false;
    };
    let (target_sec, target_sub) = match section.split_once('.') {
        Some((s, sub)) => (s, Some(sub)),
        None => (section, None),
    };
    if key_sec != target_sec {
        return false;
    }
    match key_rest.rsplit_once('.') {
        Some((key_sub, var_name)) => !var_name.is_empty() && target_sub == Some(key_sub),
        None => !key_rest.is_empty() && target_sub.is_none(),
    }
}

/// Object-safe configuration store abstraction backed by SQLite or `redb 5.0`
/// (`ADR-RB-04` #1).
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// Owner [`DatabaseRole`] (`Repository`, `GlobalConfig`, or `SystemConfig`).
    fn role(&self) -> DatabaseRole;

    /// Active storage engine backend.
    fn backend(&self) -> StorageBackendKind;

    /// Return the last-written value for exact `key` (`None` when absent).
    async fn get(&self, key: &str) -> StorageResult<Option<ConfigEntry>>;

    /// Return all values for exact `key` in ascending insertion order.
    async fn get_all(&self, key: &str) -> StorageResult<Vec<ConfigEntry>>;

    /// Return all entries whose key starts with `prefix`, ordered by `(key ASC, id ASC)`.
    async fn get_by_prefix(&self, prefix: &str) -> StorageResult<Vec<ConfigEntry>>;

    /// Return all entries in ascending insertion order.
    async fn list_all(&self) -> StorageResult<Vec<ConfigEntry>>;

    /// Upsert a single-valued key, replacing any other-cased ASCII variants.
    ///
    /// Fails closed if multiple exact values already exist, or if `encrypted ==
    /// false` while an existing exact or other-cased variant is already marked
    /// `encrypted == true` (callers must encrypt first and pass `encrypted =
    /// true` so plaintext is never persisted under an encrypted flag).
    async fn set(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()>;

    /// Append a value for `key` (supports multi-value keys).
    ///
    /// Enforces strict same-key encryption homogeneity: fails closed if
    /// `encrypted` conflicts with existing entries for `key` (both plaintext
    /// appended to an encrypted key and encrypted appended to a plaintext key
    /// are rejected).
    async fn add(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()>;

    /// Delete the single entry for `key` (returns `0` or `1`; fails closed if
    /// multiple rows exist).
    async fn unset(&self, key: &str) -> StorageResult<usize>;

    /// Delete every entry for exact `key` and return the number removed.
    async fn unset_all(&self, key: &str) -> StorageResult<usize>;

    /// Atomically remove every entry belonging to `section` (`key_in_section`).
    /// Returns `Err(NotFound)` when no keys belong to `section`.
    async fn remove_section(&self, section: &str) -> StorageResult<usize>;

    /// Atomically rename every entry in `old_section` to `new_section`,
    /// preserving values, multi-value order, and encryption flags.
    async fn rename_section(&self, old_section: &str, new_section: &str) -> StorageResult<usize>;

    /// List legacy 3-column `config` rows in insertion order.
    async fn list_legacy_entries(&self) -> StorageResult<Vec<LegacyConfigEntry>>;

    /// Insert one legacy 3-column `config` row.
    async fn insert_legacy_entry(&self, entry: &LegacyConfigEntry) -> StorageResult<()>;

    /// List schema ledger receipts (`schema_versions` or `configuration_schema_versions`)
    /// ordered by `version ASC`. Initial migration receipts carry RFC 3339 UTC
    /// `applied_at` timestamps; caller-supplied receipts preserve `applied_at`
    /// verbatim.
    async fn list_schema_receipts(&self) -> StorageResult<Vec<SchemaVersionReceipt>>;

    /// Upsert a schema ledger receipt, preserving `applied_at` verbatim.
    async fn record_schema_receipt(&self, receipt: &SchemaVersionReceipt) -> StorageResult<()>;
}

/// Object-safe scoped metadata KV store abstraction (`metadata_kv`, `ADR-RB-04` #1).
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Get one `(scope, target, key)` metadata entry, or `None` when absent.
    async fn get_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
    ) -> StorageResult<Option<MetadataEntryRecord>>;

    /// Upsert `(scope, target, key)` and return the previous value if one existed.
    async fn set_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        value: &str,
        value_type: &str,
    ) -> StorageResult<Option<String>>;

    /// Atomically compare `current.value` against `expected` and either upsert
    /// `new_value = Some((value, value_type))` or delete `new_value = None`.
    async fn cas_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        expected: Option<&str>,
        new_value: Option<(&str, &str)>,
    ) -> StorageResult<MetadataCasOutcome>;

    /// Delete `(scope, target, key)` and return whether a row was removed.
    async fn unset_metadata(&self, scope: &str, target: &str, key: &str) -> StorageResult<bool>;

    /// List entries for `(scope, target)` ordered by `key ASC`, optionally
    /// filtered by `key_prefix`.
    async fn list_metadata(
        &self,
        scope: &str,
        target: &str,
        key_prefix: Option<&str>,
    ) -> StorageResult<Vec<MetadataEntryRecord>>;

    /// List every entry in `scope` across all targets ordered by `(target ASC, key ASC)`.
    async fn list_metadata_scope(&self, scope: &str) -> StorageResult<Vec<MetadataEntryRecord>>;

    /// Delete every entry for `(scope, target)` and return the removed count.
    async fn delete_metadata_target(&self, scope: &str, target: &str) -> StorageResult<u64>;

    /// Atomically move all entries from `old_target` to `new_target` (replacing
    /// any pre-existing entries on `new_target`).
    async fn rename_metadata_target(
        &self,
        scope: &str,
        old_target: &str,
        new_target: &str,
    ) -> StorageResult<()>;

    /// Atomically copy all entries from `from_target` to `to_target` (replacing
    /// any pre-existing entries on `to_target`; self-copy is a no-op).
    async fn copy_metadata_target(
        &self,
        scope: &str,
        from_target: &str,
        to_target: &str,
    ) -> StorageResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn dual_backend_config_and_metadata_equivalence() {
        use std::sync::Arc;

        use tempfile::tempdir;

        use crate::{
            internal::vault::{decrypt_token, encrypt_token},
            storage::{redb::RedbConfigStore, sqlite::SqliteConfigStore},
        };

        let test_unseal_key = [0x5Au8; 32];
        let encrypt_hex = |plaintext: &str| -> String {
            let ct =
                encrypt_token(&test_unseal_key, plaintext.as_bytes()).expect("encrypt test secret");
            hex::encode(ct)
        };
        let decrypt_hex = |ciphertext_hex: &str| -> String {
            let bytes = hex::decode(ciphertext_hex).expect("decode hex ciphertext");
            decrypt_token(&test_unseal_key, &bytes).expect("decrypt persisted ciphertext")
        };

        let ct_case_mix_1 = encrypt_hex("super-secret-case-1");
        let ct_case_mix_2 = encrypt_hex("super-secret-case-2");
        let ct_vault_tok_1 = encrypt_hex("api-token-secret-one");
        let ct_vault_tok_2 = encrypt_hex("api-token-secret-two");

        let dir = tempdir().expect("create temp dir for dual-backend equivalence");
        let sqlite_path = dir.path().join("libra.db");
        let redb_path = dir.path().join("git.gen-00000001.redb");

        let repo_before_create = chrono::Utc::now();
        let sqlite_store = SqliteConfigStore::create(&sqlite_path, DatabaseRole::Repository)
            .await
            .expect("create SqliteConfigStore");
        let redb_store = RedbConfigStore::create(&redb_path, DatabaseRole::Repository)
            .expect("create RedbConfigStore");
        let repo_after_create = chrono::Utc::now();

        // Verify object-safety (`ADR-RB-04` #1): both backends work uniformly behind `Arc<dyn Trait>`.
        let cfg_backends: Vec<Arc<dyn ConfigStore>> =
            vec![Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];
        let meta_backends: Vec<Arc<dyn MetadataStore>> =
            vec![Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];

        assert_eq!(cfg_backends[0].role(), DatabaseRole::Repository);
        assert_eq!(cfg_backends[1].role(), DatabaseRole::Repository);
        assert_eq!(cfg_backends[0].backend(), StorageBackendKind::Sqlite);
        assert_eq!(cfg_backends[1].backend(), StorageBackendKind::Redb);

        // -----------------------------------------------------------------
        // AC1: Multi-value append (`add`), single-value `set` (including
        // case-variant replacement & encryption inheritance), and exact
        // read equivalence across `get` / `get_all` / `get_by_prefix` / `list_all`.
        // -----------------------------------------------------------------
        for store in &cfg_backends {
            store
                .set("user.name", "Alice", false)
                .await
                .expect("set user.name");
            // Case-variant replacement replaces `user.name` with `user.Name`.
            store
                .set("user.Name", "Alice Updated", false)
                .await
                .expect("set user.Name");

            // When BOTH an exact key AND an encrypted other-cased variant
            // coexist (e.g. via `add`), `set` MUST reject plaintext replacement
            // (never persisting plaintext under `encrypted = true`), and once
            // the caller encrypts the replacement (`encrypted = true`), `set`
            // removes the other-cased variant and persists decryptable ciphertext.
            store
                .add("case.mix", "v1", false)
                .await
                .expect("add case.mix plaintext");
            store
                .add("Case.Mix", &ct_case_mix_1, true)
                .await
                .expect("add Case.Mix encrypted");
            let downgrade_err = store
                .set("case.mix", "leaked-plaintext-case", false)
                .await
                .expect_err(
                    "set with plaintext must fail closed when other-cased variant is encrypted",
                );
            assert_eq!(downgrade_err.stable_code_str(), "LBR-IO-001");
            // Existing rows must remain untouched after rejected downgrade.
            assert_eq!(
                store
                    .get_all("case.mix")
                    .await
                    .expect("case.mix untouched after rejected set"),
                vec![ConfigEntry {
                    key: "case.mix".to_string(),
                    value: "v1".to_string(),
                    encrypted: false,
                }]
            );
            assert_eq!(
                store
                    .get_all("Case.Mix")
                    .await
                    .expect("Case.Mix untouched after rejected set"),
                vec![ConfigEntry {
                    key: "Case.Mix".to_string(),
                    value: ct_case_mix_1.clone(),
                    encrypted: true,
                }]
            );

            // Caller encrypts before calling `set(..., true)`: succeeds, removes
            // `Case.Mix`, and stores decryptable ciphertext.
            store.set("case.mix", &ct_case_mix_2, true).await.expect(
                "set case.mix with encrypted ciphertext must succeed and collapse Case.Mix",
            );
            let case_mix_after = store
                .get_all("case.mix")
                .await
                .expect("get_all case.mix after encrypted set");
            assert_eq!(
                case_mix_after,
                vec![ConfigEntry {
                    key: "case.mix".to_string(),
                    value: ct_case_mix_2.clone(),
                    encrypted: true,
                }]
            );
            assert_eq!(decrypt_hex(&case_mix_after[0].value), "super-secret-case-2");
            assert_eq!(
                store
                    .get_all("Case.Mix")
                    .await
                    .expect("Case.Mix must be removed by set(case.mix)"),
                Vec::new()
            );

            // Direct plaintext overwrite of an exact encrypted key must also fail closed.
            let exact_downgrade_err = store
                .set("case.mix", "leaked-exact-plaintext", false)
                .await
                .expect_err("set with plaintext must fail closed on exact encrypted key");
            assert_eq!(exact_downgrade_err.stable_code_str(), "LBR-IO-001");

            store
                .add("case.plain", "p1", false)
                .await
                .expect("add case.plain");
            store
                .add("CASE.PLAIN", "p2", false)
                .await
                .expect("add CASE.PLAIN");
            store
                .set("case.plain", "p3", false)
                .await
                .expect("set case.plain must remove CASE.PLAIN");
            assert_eq!(
                store
                    .get_all("case.plain")
                    .await
                    .expect("get_all case.plain"),
                vec![ConfigEntry {
                    key: "case.plain".to_string(),
                    value: "p3".to_string(),
                    encrypted: false,
                }]
            );
            assert_eq!(
                store
                    .get_all("CASE.PLAIN")
                    .await
                    .expect("CASE.PLAIN must be removed"),
                Vec::new()
            );

            store
                .add("remote.origin.url", "https://example.com/repo.git", false)
                .await
                .expect("add remote.origin.url");
            store
                .add(
                    "remote.origin.fetch",
                    "+refs/heads/*:refs/remotes/origin/*",
                    false,
                )
                .await
                .expect("add fetch 1");
            store
                .add("remote.origin.fetch", "+refs/tags/*:refs/tags/*", false)
                .await
                .expect("add fetch 2");
            store
                .add("remote.origin.fetch", "+refs/notes/*:refs/notes/*", false)
                .await
                .expect("add fetch 3");

            // `set` must reject multi-valued keys on both backends.
            let multi_set_err = store
                .set("remote.origin.fetch", "single", false)
                .await
                .expect_err("set on multi-valued key must fail");
            assert_eq!(multi_set_err.stable_code_str(), "LBR-IO-001");

            // Mixed plaintext/encrypted `add` must fail closed in BOTH
            // directions (encrypted onto plaintext AND plaintext onto encrypted),
            // while caller-encrypted `add(..., true)` succeeds and round-trips.
            let mix_enc_on_plain_err = store
                .add("remote.origin.url", &ct_vault_tok_1, true)
                .await
                .expect_err("mixing encrypted onto plaintext key must fail");
            assert_eq!(mix_enc_on_plain_err.stable_code_str(), "LBR-IO-001");

            store
                .add("vault.token.api", &ct_vault_tok_1, true)
                .await
                .expect("add encrypted 1");
            let mix_plain_on_enc_err = store
                .add("vault.token.api", "leaked-plaintext-token", false)
                .await
                .expect_err("adding plaintext to an encrypted key must fail closed");
            assert_eq!(mix_plain_on_enc_err.stable_code_str(), "LBR-IO-001");
            store
                .add("vault.token.api", &ct_vault_tok_2, true)
                .await
                .expect("add encrypted 2 with caller ciphertext");

            // Bare section vs subsection isolation fixture for AC2/AC3.
            store
                .set("branch.autosetuprebase", "always", false)
                .await
                .expect("set bare branch key");
            store
                .set("branch.main.remote", "origin", false)
                .await
                .expect("set subsection branch.main.remote");
            store
                .set("branch.main.merge", "refs/heads/main", false)
                .await
                .expect("set subsection branch.main.merge");
        }

        assert_eq!(
            cfg_backends[0].get("user.name").await.expect("sqlite get"),
            cfg_backends[1].get("user.name").await.expect("redb get")
        );
        assert_eq!(
            cfg_backends[0].get("user.Name").await.expect("sqlite get"),
            cfg_backends[1].get("user.Name").await.expect("redb get")
        );
        assert_eq!(
            cfg_backends[0]
                .get("remote.origin.fetch")
                .await
                .expect("sqlite last-one-wins"),
            cfg_backends[1]
                .get("remote.origin.fetch")
                .await
                .expect("redb last-one-wins")
        );
        assert_eq!(
            cfg_backends[0]
                .get_all("remote.origin.fetch")
                .await
                .expect("sqlite get_all"),
            cfg_backends[1]
                .get_all("remote.origin.fetch")
                .await
                .expect("redb get_all")
        );
        assert_eq!(
            cfg_backends[0]
                .get_all("vault.token.api")
                .await
                .expect("sqlite encrypted get_all"),
            cfg_backends[1]
                .get_all("vault.token.api")
                .await
                .expect("redb encrypted get_all")
        );
        let rb_vault_entries = cfg_backends[1]
            .get_all("vault.token.api")
            .await
            .expect("redb encrypted");
        assert!(
            rb_vault_entries.iter().all(|e| e.encrypted),
            "all vault.token.api entries must remain marked encrypted"
        );
        let decrypted_tokens: Vec<String> = rb_vault_entries
            .iter()
            .map(|e| decrypt_hex(&e.value))
            .collect();
        assert_eq!(
            decrypted_tokens,
            vec![
                "api-token-secret-one".to_string(),
                "api-token-secret-two".to_string()
            ],
            "persisted encrypted entries must decrypt cleanly to original secrets"
        );
        // Verify raw database files on disk NEVER contain any secret plaintext
        // or any rejected plaintext downgrade payload.
        for db_file in [&sqlite_path, &redb_path] {
            let raw_bytes = std::fs::read(db_file).expect("read raw db bytes for secret audit");
            for forbidden in [
                b"super-secret-case-1".as_slice(),
                b"super-secret-case-2".as_slice(),
                b"leaked-plaintext-case".as_slice(),
                b"leaked-exact-plaintext".as_slice(),
                b"api-token-secret-one".as_slice(),
                b"api-token-secret-two".as_slice(),
                b"leaked-plaintext-token".as_slice(),
            ] {
                assert!(
                    !raw_bytes.windows(forbidden.len()).any(|w| w == forbidden),
                    "raw storage file {:?} must never contain plaintext secret {:?}",
                    db_file.file_name(),
                    String::from_utf8_lossy(forbidden)
                );
            }
        }
        assert_eq!(
            cfg_backends[0]
                .get_by_prefix("remote.")
                .await
                .expect("sqlite prefix"),
            cfg_backends[1]
                .get_by_prefix("remote.")
                .await
                .expect("redb prefix")
        );
        assert_eq!(
            cfg_backends[0].list_all().await.expect("sqlite list_all"),
            cfg_backends[1].list_all().await.expect("redb list_all")
        );

        // -----------------------------------------------------------------
        // AC2: `rename_section` equivalence (multi-value order preservation,
        // encrypted flag preservation, bare-vs-subsection isolation, and
        // identical rejection of missing source / existing destination / same name).
        // -----------------------------------------------------------------
        let mut rename_counts = Vec::new();
        for store in &cfg_backends {
            let same_err = store
                .rename_section("remote.origin", "remote.origin")
                .await
                .expect_err("identical rename_section must fail");
            assert_eq!(same_err.stable_code_str(), "LBR-IO-001");

            let missing_err = store
                .rename_section("remote.doesnotexist", "remote.upstream")
                .await
                .expect_err("missing source section must fail");
            assert!(matches!(missing_err, StorageBackendError::NotFound { .. }));

            let dest_exists_err = store
                .rename_section("remote.origin", "branch.main")
                .await
                .expect_err("existing destination section must fail");
            assert_eq!(dest_exists_err.stable_code_str(), "LBR-IO-001");

            // Rename `remote.origin` -> `remote.upstream` (1 url + 3 fetch values).
            let moved = store
                .rename_section("remote.origin", "remote.upstream")
                .await
                .expect("rename_section remote.origin -> remote.upstream");
            rename_counts.push(moved);

            // Rename bare section `branch` -> `branchdefaults` must NOT touch `branch.main.*`.
            let bare_moved = store
                .rename_section("branch", "branchdefaults")
                .await
                .expect("rename bare section branch -> branchdefaults");
            assert_eq!(bare_moved, 1);
        }
        assert_eq!(rename_counts, vec![4, 4]);
        assert_eq!(
            cfg_backends[0]
                .get_all("remote.origin.fetch")
                .await
                .expect("sqlite old section empty"),
            Vec::new()
        );
        assert_eq!(
            cfg_backends[0]
                .get_all("remote.upstream.fetch")
                .await
                .expect("sqlite renamed multi-value"),
            cfg_backends[1]
                .get_all("remote.upstream.fetch")
                .await
                .expect("redb renamed multi-value")
        );
        assert_eq!(
            cfg_backends[0]
                .get_by_prefix("branch.")
                .await
                .expect("sqlite subsection intact"),
            cfg_backends[1]
                .get_by_prefix("branch.")
                .await
                .expect("redb subsection intact")
        );
        assert_eq!(
            cfg_backends[0].list_all().await.expect("sqlite list_all"),
            cfg_backends[1].list_all().await.expect("redb list_all")
        );

        // -----------------------------------------------------------------
        // AC3: `unset`, `unset_all`, and `remove_section` equivalence.
        // -----------------------------------------------------------------
        for store in &cfg_backends {
            // `unset` rejects multi-valued key.
            let multi_unset_err = store
                .unset("remote.upstream.fetch")
                .await
                .expect_err("unset on multi-valued key must fail");
            assert_eq!(multi_unset_err.stable_code_str(), "LBR-IO-001");

            // `unset` removes singleton key and returns 0 when already absent.
            assert_eq!(store.unset("user.Name").await.expect("unset user.Name"), 1);
            assert_eq!(store.unset("user.Name").await.expect("unset absent"), 0);

            // `unset_all` removes all multi-values for key.
            assert_eq!(
                store
                    .unset_all("remote.upstream.fetch")
                    .await
                    .expect("unset_all fetch"),
                3
            );
            assert_eq!(
                store
                    .unset_all("remote.upstream.fetch")
                    .await
                    .expect("unset_all absent"),
                0
            );

            // `remove_section` deletes exact section members only and fails with NotFound if absent.
            assert_eq!(
                store
                    .remove_section("branch.main")
                    .await
                    .expect("remove_section branch.main"),
                2
            );
            let missing_rm = store
                .remove_section("branch.main")
                .await
                .expect_err("second remove_section must return NotFound");
            assert!(matches!(missing_rm, StorageBackendError::NotFound { .. }));
        }

        assert_eq!(
            cfg_backends[0]
                .list_all()
                .await
                .expect("sqlite post-delete"),
            cfg_backends[1].list_all().await.expect("redb post-delete")
        );

        // Literal case-sensitive and SQL-wildcard (`%`, `_`) prefix,
        // `rename_section`, and `remove_section` equivalence (`Sqlite` LIKE
        // wildcards/case-folding must NEVER leak into any section or prefix op).
        for store in &cfg_backends {
            store
                .set("prefix.lower", "v-lower", false)
                .await
                .expect("set prefix.lower");
            store
                .set("Prefix.upper", "v-upper", false)
                .await
                .expect("set Prefix.upper");
            store
                .set("a_b.exact", "v-under", false)
                .await
                .expect("set a_b.exact");
            store
                .set("axb.other", "v-x", false)
                .await
                .expect("set axb.other");
            store
                .set("a%b.exact", "v-pct", false)
                .await
                .expect("set a%b.exact");
            store
                .set("aZZZb.other", "v-zzz", false)
                .await
                .expect("set aZZZb.other");

            // Section fixtures exercising ASCII case, `_`, and `%` in
            // `rename_section` and `remove_section`. Use `add` for `Sec_a.k1`
            // so both `sec_a.k1` and `Sec_a.k1` coexist literally (`set`
            // replaces other-cased variants of the same key).
            store
                .set("sec_a.k1", "v-under-1", false)
                .await
                .expect("set sec_a.k1");
            store
                .set("sec_a.k2", "v-under-2", false)
                .await
                .expect("set sec_a.k2");
            store
                .set("secXa.k1", "v-x", false)
                .await
                .expect("set secXa.k1");
            store
                .add("Sec_a.k1", "v-upper-sec", false)
                .await
                .expect("add Sec_a.k1");
            store
                .set("sec%b.k1", "v-pct", false)
                .await
                .expect("set sec%b.k1");
            store
                .set("secZZZb.k1", "v-zzz", false)
                .await
                .expect("set secZZZb.k1");
            store
                .set("SEC_NEW.k1", "v-upper-dest", false)
                .await
                .expect("set SEC_NEW.k1");
        }
        for prefix in ["prefix.", "Prefix.", "a_b.", "a%b."] {
            let sq = cfg_backends[0]
                .get_by_prefix(prefix)
                .await
                .expect("sqlite literal prefix");
            let rb = cfg_backends[1]
                .get_by_prefix(prefix)
                .await
                .expect("redb literal prefix");
            assert_eq!(sq, rb, "prefix `{prefix}` diverged between SQLite and Redb");
            assert_eq!(
                sq.len(),
                1,
                "literal prefix `{prefix}` must match exactly 1 key (no case/wildcard leak): {sq:?}"
            );
        }

        for store in &cfg_backends {
            // Exact destination collision must fail closed.
            let dest_collision = store
                .rename_section("sec_a", "sec%b")
                .await
                .expect_err("rename_section onto existing literal sec%b must fail");
            assert_eq!(dest_collision.stable_code_str(), "LBR-IO-001");

            // Case-distinct destination `sec_new` must NOT collide with `SEC_NEW`,
            // and `_` in `sec_a` must NOT touch `secXa` or `Sec_a`.
            assert_eq!(
                store
                    .rename_section("sec_a", "sec_new")
                    .await
                    .expect("rename_section sec_a -> sec_new"),
                2
            );
            // `%` in `sec%b` must rename ONLY `sec%b` and NOT touch `secZZZb`.
            assert_eq!(
                store
                    .rename_section("sec%b", "sec%c")
                    .await
                    .expect("rename_section sec%b -> sec%c"),
                1
            );
        }
        assert_eq!(
            cfg_backends[0]
                .list_all()
                .await
                .expect("sqlite list_all after case/wildcard rename_section"),
            cfg_backends[1]
                .list_all()
                .await
                .expect("redb list_all after case/wildcard rename_section")
        );
        for store in &cfg_backends {
            assert_eq!(
                store
                    .get("secXa.k1")
                    .await
                    .expect("get secXa.k1")
                    .map(|e| e.value),
                Some("v-x".to_string())
            );
            assert_eq!(
                store
                    .get("Sec_a.k1")
                    .await
                    .expect("get Sec_a.k1")
                    .map(|e| e.value),
                Some("v-upper-sec".to_string())
            );
            assert_eq!(
                store
                    .get("SEC_NEW.k1")
                    .await
                    .expect("get SEC_NEW.k1")
                    .map(|e| e.value),
                Some("v-upper-dest".to_string())
            );
            assert_eq!(
                store
                    .get("secZZZb.k1")
                    .await
                    .expect("get secZZZb.k1")
                    .map(|e| e.value),
                Some("v-zzz".to_string())
            );
            assert_eq!(
                store
                    .get("sec%c.k1")
                    .await
                    .expect("get sec%c.k1")
                    .map(|e| e.value),
                Some("v-pct".to_string())
            );

            // `remove_section` must delete ONLY exact case & literal `_`/`%` section members.
            assert_eq!(
                store
                    .remove_section("sec_new")
                    .await
                    .expect("remove_section sec_new"),
                2
            );
            assert_eq!(
                store
                    .remove_section("sec%c")
                    .await
                    .expect("remove_section sec%c"),
                1
            );
            assert_eq!(
                store
                    .remove_section("Sec_a")
                    .await
                    .expect("remove_section Sec_a"),
                1
            );
            assert!(
                store
                    .get("SEC_NEW.k1")
                    .await
                    .expect("SEC_NEW must survive remove_section(sec_new)")
                    .is_some()
            );
            assert!(
                store
                    .get("secZZZb.k1")
                    .await
                    .expect("secZZZb must survive remove_section(sec%c)")
                    .is_some()
            );
            assert!(
                store
                    .get("secXa.k1")
                    .await
                    .expect("secXa must survive remove_section(Sec_a)")
                    .is_some()
            );
        }
        assert_eq!(
            cfg_backends[0]
                .list_all()
                .await
                .expect("sqlite list_all after case/wildcard remove_section"),
            cfg_backends[1]
                .list_all()
                .await
                .expect("redb list_all after case/wildcard remove_section")
        );

        // Legacy `config` table and COMPLETE initial + updated schema receipts
        // equivalence across Repository, GlobalConfig, and SystemConfig roles.
        let validate_initial_receipts = |receipts: &[SchemaVersionReceipt],
                                         before: chrono::DateTime<chrono::Utc>,
                                         after: chrono::DateTime<chrono::Utc>|
         -> Vec<(i64, String)> {
            let min_ts = before - chrono::Duration::seconds(2);
            let max_ts = after + chrono::Duration::seconds(2);
            assert!(
                !receipts.is_empty(),
                "fresh store must record initial schema migration receipts"
            );
            for r in receipts {
                let parsed = chrono::DateTime::parse_from_rfc3339(&r.applied_at).unwrap_or_else(
                    |err| {
                        panic!(
                            "schema receipt version={} applied_at `{}` is not valid RFC 3339: {err}",
                            r.version, r.applied_at
                        )
                    },
                );
                assert_eq!(
                    parsed.offset().local_minus_utc(),
                    0,
                    "schema receipt applied_at `{}` must be UTC",
                    r.applied_at
                );
                let utc = parsed.with_timezone(&chrono::Utc);
                assert!(
                    utc >= min_ts && utc <= max_ts,
                    "schema receipt applied_at `{}` outside creation window [{min_ts}, {max_ts}]",
                    r.applied_at
                );
            }
            receipts
                .iter()
                .map(|r| (r.version, r.name.clone()))
                .collect()
        };

        let sq_repo_init = cfg_backends[0]
            .list_schema_receipts()
            .await
            .expect("sqlite initial repo receipts");
        let rb_repo_init = cfg_backends[1]
            .list_schema_receipts()
            .await
            .expect("redb initial repo receipts");
        assert_eq!(
            validate_initial_receipts(&sq_repo_init, repo_before_create, repo_after_create),
            validate_initial_receipts(&rb_repo_init, repo_before_create, repo_after_create),
            "fresh Repository stores must have identical initial schema receipts"
        );

        let legacy_row = LegacyConfigEntry {
            configuration: "remote".to_string(),
            name: Some("upstream".to_string()),
            key: "url".to_string(),
            value: "https://example.com/repo.git".to_string(),
        };
        let extra_repo_receipt = SchemaVersionReceipt {
            version: 2026112201,
            name: "rb03_test_receipt".to_string(),
            applied_at: "2026-11-22T00:00:00Z".to_string(),
        };
        for store in &cfg_backends {
            store
                .insert_legacy_entry(&legacy_row)
                .await
                .expect("insert legacy row");
            store
                .record_schema_receipt(&extra_repo_receipt)
                .await
                .expect("record schema receipt");
        }
        assert_eq!(
            cfg_backends[0]
                .list_legacy_entries()
                .await
                .expect("sqlite legacy"),
            cfg_backends[1]
                .list_legacy_entries()
                .await
                .expect("redb legacy")
        );
        let sq_repo_after = cfg_backends[0]
            .list_schema_receipts()
            .await
            .expect("sqlite repo receipts after append");
        let rb_repo_after = cfg_backends[1]
            .list_schema_receipts()
            .await
            .expect("redb repo receipts after append");
        assert_eq!(
            sq_repo_after.len(),
            sq_repo_init.len() + 1,
            "appending one receipt must grow Repository ledger by exactly 1"
        );
        assert_eq!(sq_repo_after.len(), rb_repo_after.len());
        assert_eq!(
            sq_repo_after.last(),
            Some(&extra_repo_receipt),
            "SQLite must persist recorded SchemaVersionReceipt verbatim including applied_at"
        );
        assert_eq!(
            rb_repo_after.last(),
            Some(&extra_repo_receipt),
            "Redb must persist recorded SchemaVersionReceipt verbatim including applied_at"
        );
        assert_eq!(
            sq_repo_after.last(),
            rb_repo_after.last(),
            "appended Repository receipt must match full struct across backends"
        );
        assert_eq!(
            validate_initial_receipts(
                &sq_repo_after[..sq_repo_init.len()],
                repo_before_create,
                repo_after_create,
            ),
            validate_initial_receipts(
                &rb_repo_after[..rb_repo_init.len()],
                repo_before_create,
                repo_after_create,
            ),
            "initial Repository schema receipts must remain identical after append"
        );

        // -----------------------------------------------------------------
        // AC4: `MetadataStore` `metadata_kv` CAS read/write equivalence.
        // -----------------------------------------------------------------
        let normalize_meta = |r: &MetadataEntryRecord| {
            (
                r.scope.clone(),
                r.target.clone(),
                r.key.clone(),
                r.value.clone(),
                r.value_type.clone(),
            )
        };

        let mut cas_summaries = Vec::new();
        for store in &meta_backends {
            // 1. Assert-absent no-op (None -> None) succeeds on absent key.
            let noop = store
                .cas_metadata("branch", "main", "protect", None, None)
                .await
                .expect("cas None->None on absent");
            assert!(matches!(
                noop,
                MetadataCasOutcome::Swapped {
                    previous: None,
                    current: None
                }
            ));

            // 2. Compare-and-update fails when key is absent.
            let miss = store
                .cas_metadata(
                    "branch",
                    "main",
                    "protect",
                    Some("false"),
                    Some(("true", "text")),
                )
                .await
                .expect("cas Some->Some on absent");
            assert!(matches!(
                miss,
                MetadataCasOutcome::Conflict { actual: None }
            ));

            // 3. Insert-if-absent (None -> Some) succeeds once, conflicts second time.
            let inserted = store
                .cas_metadata("branch", "main", "protect", None, Some(("false", "text")))
                .await
                .expect("cas insert-if-absent");
            let created_ts = match inserted {
                MetadataCasOutcome::Swapped {
                    previous: None,
                    current: Some(rec),
                } => {
                    assert_eq!(rec.value, "false");
                    assert_eq!(rec.value_type, "text");
                    rec.created_at
                }
                other => panic!("expected Swapped on insert-if-absent, got {other:?}"),
            };

            let dup = store
                .cas_metadata("branch", "main", "protect", None, Some(("true", "text")))
                .await
                .expect("duplicate cas insert-if-absent");
            match dup {
                MetadataCasOutcome::Conflict { actual: Some(rec) } => {
                    assert_eq!(rec.value, "false");
                }
                other => panic!("expected Conflict on duplicate insert-if-absent, got {other:?}"),
            }

            // 4. Compare-and-update with wrong expected conflicts; with matching expected swaps
            //    and preserves original `created_at`.
            let wrong = store
                .cas_metadata(
                    "branch",
                    "main",
                    "protect",
                    Some("wrong"),
                    Some(("true", "text")),
                )
                .await
                .expect("cas wrong expected");
            assert!(matches!(
                wrong,
                MetadataCasOutcome::Conflict { actual: Some(_) }
            ));

            let updated = store
                .cas_metadata(
                    "branch",
                    "main",
                    "protect",
                    Some("false"),
                    Some(("true", "text")),
                )
                .await
                .expect("cas matching update");
            match updated {
                MetadataCasOutcome::Swapped {
                    previous: Some(prev),
                    current: Some(curr),
                } => {
                    assert_eq!(prev.value, "false");
                    assert_eq!(curr.value, "true");
                    assert_eq!(
                        curr.created_at, created_ts,
                        "CAS update must preserve created_at"
                    );
                }
                other => panic!("expected Swapped on matching CAS update, got {other:?}"),
            }

            // 5. Additional keys (including case & SQL wildcard literals), target
            //    list/copy/rename, and compare-and-delete.
            store
                .set_metadata("branch", "main", "lineage.parent", "develop", "text")
                .await
                .expect("set lineage.parent");
            store
                .set_metadata("branch", "main", "archive", "false", "text")
                .await
                .expect("set archive");
            store
                .set_metadata("branch", "main", "p_1", "under", "text")
                .await
                .expect("set p_1");
            store
                .set_metadata("branch", "main", "px1", "x", "text")
                .await
                .expect("set px1");
            store
                .set_metadata("branch", "main", "P_1", "upper", "text")
                .await
                .expect("set P_1");
            store
                .set_metadata("branch", "main", "p%1", "pct", "text")
                .await
                .expect("set p%1");
            store
                .set_metadata("branch", "main", "pZZZ1", "zzz", "text")
                .await
                .expect("set pZZZ1");
            store
                .copy_metadata_target("branch", "main", "release")
                .await
                .expect("copy target main -> release");
            store
                .rename_metadata_target("branch", "release", "release-v1")
                .await
                .expect("rename target release -> release-v1");

            let bad_del = store
                .cas_metadata("branch", "main", "protect", Some("false"), None)
                .await
                .expect("cas delete mismatch");
            assert!(matches!(
                bad_del,
                MetadataCasOutcome::Conflict { actual: Some(_) }
            ));

            let good_del = store
                .cas_metadata("branch", "main", "protect", Some("true"), None)
                .await
                .expect("cas delete match");
            assert!(matches!(
                good_del,
                MetadataCasOutcome::Swapped {
                    previous: Some(_),
                    current: None
                }
            ));
            assert_eq!(
                store
                    .get_metadata("branch", "main", "protect")
                    .await
                    .expect("get after cas delete"),
                None
            );

            let scope_rows: Vec<_> = store
                .list_metadata_scope("branch")
                .await
                .expect("list_metadata_scope")
                .iter()
                .map(normalize_meta)
                .collect();
            let prefix_rows: Vec<_> = store
                .list_metadata("branch", "main", Some("lineage."))
                .await
                .expect("list_metadata prefix")
                .iter()
                .map(normalize_meta)
                .collect();
            let under_rows: Vec<_> = store
                .list_metadata("branch", "main", Some("p_"))
                .await
                .expect("list_metadata p_")
                .iter()
                .map(normalize_meta)
                .collect();
            let pct_rows: Vec<_> = store
                .list_metadata("branch", "main", Some("p%"))
                .await
                .expect("list_metadata p%")
                .iter()
                .map(normalize_meta)
                .collect();
            assert_eq!(under_rows.len(), 1, "p_ must not match px1 or P_1");
            assert_eq!(under_rows[0].2, "p_1");
            assert_eq!(pct_rows.len(), 1, "p% must not match pZZZ1");
            assert_eq!(pct_rows[0].2, "p%1");
            cas_summaries.push((scope_rows, prefix_rows, under_rows, pct_rows));
        }

        assert_eq!(
            cas_summaries[0], cas_summaries[1],
            "SqliteConfigStore and RedbConfigStore metadata_kv CAS & CRUD results must match item-for-item"
        );
        assert_eq!(cas_summaries[0].1.len(), 1);
        assert_eq!(cas_summaries[0].1[0].2, "lineage.parent");

        // -----------------------------------------------------------------
        // Concurrent CAS race-freedom regression on BOTH SQLite and Redb:
        // 8 contending tasks race insert-if-absent (`None -> Some`) and then
        // compare-and-update (`Some(winner) -> Some(v2)`) across independent
        // handles, asserting exactly 1 Swapped, 7 Conflicts with matching
        // `actual`, and exact final record.
        // -----------------------------------------------------------------
        const CAS_CONTENDERS: usize = 8;
        let cas_sqlite_path = dir.path().join("cas-race.db");
        let cas_redb_path = dir.path().join("cas-race.redb");
        let _seed_sq = SqliteConfigStore::create(&cas_sqlite_path, DatabaseRole::Repository)
            .await
            .expect("create sqlite db for CAS race");
        let _seed_rb = RedbConfigStore::create(&cas_redb_path, DatabaseRole::Repository)
            .expect("create redb db for CAS race");
        drop(_seed_sq);
        drop(_seed_rb);

        for is_redb in [false, true] {
            let mut handles: Vec<Arc<dyn MetadataStore>> = Vec::with_capacity(CAS_CONTENDERS);
            for _ in 0..CAS_CONTENDERS {
                if is_redb {
                    let h =
                        RedbConfigStore::open_read_write(&cas_redb_path, DatabaseRole::Repository)
                            .expect("open concurrent redb handle for CAS race");
                    handles.push(Arc::new(h));
                } else {
                    let h = SqliteConfigStore::open(&cas_sqlite_path, DatabaseRole::Repository)
                        .await
                        .expect("open concurrent sqlite handle for CAS race");
                    handles.push(Arc::new(h));
                }
            }

            // Phase A: Concurrent insert-if-absent (`None -> Some`).
            let barrier_a = Arc::new(tokio::sync::Barrier::new(CAS_CONTENDERS));
            let mut join_a = tokio::task::JoinSet::new();
            for (idx, handle) in handles.iter().cloned().enumerate() {
                let b = Arc::clone(&barrier_a);
                join_a.spawn(async move {
                    b.wait().await;
                    let candidate = format!("owner-{idx}");
                    let outcome = handle
                        .cas_metadata(
                            "branch",
                            "cas-race",
                            "lease",
                            None,
                            Some((&candidate, "text")),
                        )
                        .await
                        .expect("concurrent cas insert-if-absent must not error");
                    (candidate, outcome)
                });
            }
            let mut swapped_a = Vec::new();
            let mut conflicts_a = Vec::new();
            while let Some(res) = join_a.join_next().await {
                let (candidate, outcome) = res.expect("CAS insert task panicked");
                match outcome {
                    MetadataCasOutcome::Swapped { previous, current } => {
                        assert_eq!(previous, None);
                        let curr = current.expect("Swapped insert must return current record");
                        assert_eq!(curr.value, candidate);
                        swapped_a.push(curr);
                    }
                    MetadataCasOutcome::Conflict { actual } => {
                        conflicts_a.push(actual.expect("loser must observe winning record"));
                    }
                }
            }
            assert_eq!(
                swapped_a.len(),
                1,
                "exactly 1 contender must win insert-if-absent (is_redb={is_redb})"
            );
            assert_eq!(conflicts_a.len(), CAS_CONTENDERS - 1);
            let winner_a = swapped_a.into_iter().next().expect("winner_a");
            for loser_actual in &conflicts_a {
                assert_eq!(
                    loser_actual.value, winner_a.value,
                    "every insert loser must observe winner value"
                );
            }
            let stored_a = handles[0]
                .get_metadata("branch", "cas-race", "lease")
                .await
                .expect("read after insert race")
                .expect("winning insert record must exist");
            assert_eq!(stored_a, winner_a);

            // Phase B: Concurrent compare-and-update (`Some(winner_a) -> Some(v2)`).
            let barrier_b = Arc::new(tokio::sync::Barrier::new(CAS_CONTENDERS));
            let mut join_b = tokio::task::JoinSet::new();
            for (idx, handle) in handles.iter().cloned().enumerate() {
                let b = Arc::clone(&barrier_b);
                let expected_val = winner_a.value.clone();
                join_b.spawn(async move {
                    b.wait().await;
                    let candidate = format!("v2-owner-{idx}");
                    let outcome = handle
                        .cas_metadata(
                            "branch",
                            "cas-race",
                            "lease",
                            Some(&expected_val),
                            Some((&candidate, "text")),
                        )
                        .await
                        .expect("concurrent cas compare-and-update must not error");
                    (candidate, outcome)
                });
            }
            let mut swapped_b = Vec::new();
            let mut conflicts_b = Vec::new();
            while let Some(res) = join_b.join_next().await {
                let (candidate, outcome) = res.expect("CAS update task panicked");
                match outcome {
                    MetadataCasOutcome::Swapped { previous, current } => {
                        let prev = previous.expect("Swapped update must return previous record");
                        assert_eq!(prev.value, winner_a.value);
                        let curr = current.expect("Swapped update must return current record");
                        assert_eq!(curr.value, candidate);
                        assert_eq!(curr.created_at, winner_a.created_at);
                        swapped_b.push(curr);
                    }
                    MetadataCasOutcome::Conflict { actual } => {
                        conflicts_b.push(actual.expect("update loser must observe v2 winner"));
                    }
                }
            }
            assert_eq!(
                swapped_b.len(),
                1,
                "exactly 1 contender must win compare-and-update (is_redb={is_redb})"
            );
            assert_eq!(conflicts_b.len(), CAS_CONTENDERS - 1);
            let winner_b = swapped_b.into_iter().next().expect("winner_b");
            for loser_actual in &conflicts_b {
                assert_eq!(
                    loser_actual.value, winner_b.value,
                    "every update loser must observe v2 winner value"
                );
            }
            let stored_b = handles[0]
                .get_metadata("branch", "cas-race", "lease")
                .await
                .expect("read after update race")
                .expect("winning update record must exist");
            assert_eq!(stored_b, winner_b);
        }

        // Complete initial + updated schema receipts equivalence AND MetadataStore role
        // rejection across BOTH GlobalConfig and SystemConfig roles.
        for (label, role) in [
            ("global", DatabaseRole::GlobalConfig),
            ("system", DatabaseRole::SystemConfig),
        ] {
            let scoped_before_create = chrono::Utc::now();
            let scoped_sqlite =
                SqliteConfigStore::create(dir.path().join(format!("{label}.db")), role)
                    .await
                    .expect("create scoped sqlite");
            let scoped_redb =
                RedbConfigStore::create(dir.path().join(format!("{label}.redb")), role)
                    .expect("create scoped redb");
            let scoped_after_create = chrono::Utc::now();

            let sq_init = scoped_sqlite
                .list_schema_receipts()
                .await
                .expect("sqlite initial scoped receipts");
            let rb_init = scoped_redb
                .list_schema_receipts()
                .await
                .expect("redb initial scoped receipts");
            assert_eq!(
                validate_initial_receipts(&sq_init, scoped_before_create, scoped_after_create),
                validate_initial_receipts(&rb_init, scoped_before_create, scoped_after_create),
                "fresh {label} stores must have identical initial schema receipts"
            );

            let extra = SchemaVersionReceipt {
                version: 2026112202,
                name: format!("rb03_{label}_receipt"),
                applied_at: "2026-11-22T00:00:00Z".to_string(),
            };
            scoped_sqlite
                .record_schema_receipt(&extra)
                .await
                .expect("record sqlite scoped receipt");
            scoped_redb
                .record_schema_receipt(&extra)
                .await
                .expect("record redb scoped receipt");
            let sq_after = scoped_sqlite
                .list_schema_receipts()
                .await
                .expect("sqlite updated scoped receipts");
            let rb_after = scoped_redb
                .list_schema_receipts()
                .await
                .expect("redb updated scoped receipts");
            assert_eq!(sq_after.len(), sq_init.len() + 1);
            assert_eq!(sq_after.len(), rb_after.len());
            assert_eq!(
                sq_after.last(),
                Some(&extra),
                "SQLite {label} store must preserve appended SchemaVersionReceipt verbatim"
            );
            assert_eq!(
                rb_after.last(),
                Some(&extra),
                "Redb {label} store must preserve appended SchemaVersionReceipt verbatim"
            );
            assert_eq!(
                validate_initial_receipts(
                    &sq_after[..sq_init.len()],
                    scoped_before_create,
                    scoped_after_create,
                ),
                validate_initial_receipts(
                    &rb_after[..rb_init.len()],
                    scoped_before_create,
                    scoped_after_create,
                ),
                "initial {label} schema receipts must remain identical after append"
            );

            for store in [
                Arc::new(scoped_sqlite) as Arc<dyn MetadataStore>,
                Arc::new(scoped_redb) as Arc<dyn MetadataStore>,
            ] {
                let err = store
                    .get_metadata("branch", "main", "protect")
                    .await
                    .expect_err("non-Repository ConfigStore must reject MetadataStore operations");
                assert!(matches!(
                    err,
                    StorageBackendError::RoleMismatch {
                        expected: DatabaseRole::Repository,
                        found
                    } if found == role
                ));
            }
        }
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_config_mvcc_concurrent_readers() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };

        use tempfile::tempdir;

        use crate::storage::{
            codec::encode_versioned,
            opener::{RedbRepairContext, begin_write_bounded_blocking, open_redb_writable},
            redb::{
                RedbConfigStore,
                config_store::{
                    CONFIG_KV_BY_KEY_INDEX, CONFIG_KV_TABLE, CONFIG_SCHEMA_VERSIONS_TABLE,
                    CONFIG_SEQ_CODEC_VERSION, CONFIG_SEQ_TABLE, METADATA_KV_TABLE,
                    SCHEMA_VERSIONS_TABLE,
                },
            },
        };

        const READER_COUNT: usize = 8;
        const MID_ROUND: usize = 15;
        const WRITER_ROUNDS: usize = 30;
        const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

        // Bounded cleanup guard ensuring no orphan child processes remain on panic.
        struct ChildBatchGuard {
            children: Vec<(String, Child)>,
        }
        impl Drop for ChildBatchGuard {
            fn drop(&mut self) {
                for (_, child) in &mut self.children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }

        let validate_snapshot = |snapshot: &[ConfigEntry]| -> usize {
            assert_eq!(
                snapshot.len(),
                4,
                "MVCC snapshot must always contain exactly 4 entries (no torn rename), got {snapshot:?}"
            );
            let mut snapshot_gen: Option<usize> = None;
            let mut saw_seq = false;
            let mut saw_tag = false;
            let mut items = Vec::new();
            for entry in snapshot {
                let rest = entry
                    .key
                    .strip_prefix("mvcc.g")
                    .expect("key must begin with mvcc.g");
                let (gen_str, var_name) = rest
                    .split_once('.')
                    .expect("key must contain generation and variable");
                let g: usize = gen_str.parse().expect("generation must be integer");
                match snapshot_gen {
                    None => snapshot_gen = Some(g),
                    Some(prev_g) => assert_eq!(
                        g, prev_g,
                        "torn MVCC snapshot observed mixed generations {prev_g} and {g}: {snapshot:?}"
                    ),
                }
                match var_name {
                    "seq" => {
                        assert_eq!(entry.value, "atomic-v1");
                        saw_seq = true;
                    }
                    "tag" => {
                        assert_eq!(entry.value, "consistent");
                        saw_tag = true;
                    }
                    "item" => items.push(entry.value.clone()),
                    other => panic!("unexpected variable in snapshot: {other}"),
                }
            }
            let current_gen = snapshot_gen.expect("snapshot must have a generation");
            assert!(
                saw_seq && saw_tag,
                "snapshot missing paired keys: {snapshot:?}"
            );
            assert_eq!(
                items,
                vec!["alpha".to_string(), "beta".to_string()],
                "multi-value order must remain intact in every MVCC snapshot"
            );
            current_gen
        };

        // Isolated child-process mode: 1 writer + 8 concurrent `open_read_only` readers.
        if let Some(role_mode) = env::var_os("LIBRA_RB03_MVCC_ROLE") {
            let db_path = PathBuf::from(
                env::var_os("LIBRA_RB03_MVCC_DB").expect("LIBRA_RB03_MVCC_DB must be set"),
            );
            let sync_dir = PathBuf::from(
                env::var_os("LIBRA_RB03_MVCC_SYNC_DIR")
                    .expect("LIBRA_RB03_MVCC_SYNC_DIR must be set"),
            );
            let mode_str = role_mode.to_str().expect("role mode must be utf-8");

            let wait_for_all_readers = |suffix: &str| {
                let wait_start = Instant::now();
                loop {
                    let count = (0..READER_COUNT)
                        .filter(|idx| sync_dir.join(format!("reader-{idx}.{suffix}")).exists())
                        .count();
                    if count == READER_COUNT {
                        return;
                    }
                    assert!(
                        wait_start.elapsed() < Duration::from_secs(15),
                        "timed out waiting for all {READER_COUNT} MVCC readers at barrier `{suffix}` (got {count})"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            };

            if mode_str == "writer" {
                let store = RedbConfigStore::open_read_write(&db_path, DatabaseRole::Repository)
                    .expect("writer open_read_write");

                // Phase 1 barrier: wait until all 8 readers have ALREADY taken and
                // verified their initial gen-0 snapshot.
                wait_for_all_readers("init_snap");

                // Phase 2a: commit rounds 1..=MID_ROUND while all 8 readers loop concurrently.
                for round in 1..=MID_ROUND {
                    let old_sec = format!("mvcc.g{}", round - 1);
                    let new_sec = format!("mvcc.g{round}");
                    let moved = store
                        .rename_section(&old_sec, &new_sec)
                        .await
                        .expect("writer atomic rename_section (first half)");
                    assert_eq!(moved, 4, "each atomic round must move exactly 4 entries");
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }

                // Mid-write overlap barrier: hold writer alive at MID_ROUND until every
                // reader has observed an intermediate state (`MID_ROUND < WRITER_ROUNDS`).
                fs::write(sync_dir.join("writer.mid_reached"), b"mid")
                    .expect("signal writer.mid_reached");
                wait_for_all_readers("saw_mid");

                // Phase 2b: commit remaining rounds (MID_ROUND+1..=WRITER_ROUNDS) while all
                // 8 readers keep reading lock-free MVCC snapshots.
                for round in (MID_ROUND + 1)..=WRITER_ROUNDS {
                    let old_sec = format!("mvcc.g{}", round - 1);
                    let new_sec = format!("mvcc.g{round}");
                    let moved = store
                        .rename_section(&old_sec, &new_sec)
                        .await
                        .expect("writer atomic rename_section (second half)");
                    assert_eq!(moved, 4, "each atomic round must move exactly 4 entries");
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }

                fs::write(sync_dir.join("writer.done"), b"done").expect("signal writer.done");
                println!("RB03_MVCC_OK:writer");
                return;
            }

            if let Some(reader_id) = mode_str.strip_prefix("reader-") {
                let store = RedbConfigStore::open_read_only(&db_path, DatabaseRole::Repository)
                    .expect("reader open_read_only");
                assert!(
                    store.is_read_only(),
                    "reader handle must be strictly read-only"
                );

                // Phase 1: take and verify the initial gen-0 snapshot BEFORE releasing writer.
                let init_snapshot = store
                    .list_all()
                    .await
                    .expect("initial MVCC snapshot must succeed");
                let init_gen = validate_snapshot(&init_snapshot);
                assert_eq!(
                    init_gen, 0,
                    "initial snapshot before writer starts must be generation 0"
                );
                fs::write(
                    sync_dir.join(format!("reader-{reader_id}.init_snap")),
                    b"ready",
                )
                .expect("signal reader init_snap");

                let mut last_gen: usize = 0;
                let mut reads_completed: usize = 1;
                let mut distinct_gens = std::collections::BTreeSet::from([0usize]);
                let mut signaled_mid = false;
                let loop_start = Instant::now();

                loop {
                    let snapshot = store
                        .list_all()
                        .await
                        .expect("MVCC open_read_only snapshot must never hit lock errors");
                    let current_gen = validate_snapshot(&snapshot);
                    assert!(
                        current_gen >= last_gen,
                        "MVCC reader observed generation regression {current_gen} < {last_gen}"
                    );
                    last_gen = current_gen;
                    distinct_gens.insert(current_gen);
                    reads_completed += 1;

                    if !signaled_mid
                        && (MID_ROUND..WRITER_ROUNDS).contains(&current_gen)
                        && sync_dir.join("writer.mid_reached").exists()
                    {
                        fs::write(sync_dir.join(format!("reader-{reader_id}.saw_mid")), b"mid")
                            .expect("signal reader saw_mid");
                        signaled_mid = true;
                    }

                    if signaled_mid
                        && sync_dir.join("writer.done").exists()
                        && current_gen == WRITER_ROUNDS
                    {
                        break;
                    }
                    assert!(
                        loop_start.elapsed() < Duration::from_secs(25),
                        "reader-{reader_id} timed out waiting for writer overlap/completion (last_gen={last_gen}, signaled_mid={signaled_mid})"
                    );
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }

                assert!(
                    signaled_mid
                        && distinct_gens.contains(&0)
                        && distinct_gens.contains(&MID_ROUND)
                        && distinct_gens.contains(&WRITER_ROUNDS)
                        && distinct_gens.len() >= 3,
                    "reader-{reader_id} must prove live overlap across gen 0 -> mid ({MID_ROUND}) -> final ({WRITER_ROUNDS}), saw {distinct_gens:?}"
                );
                println!(
                    "RB03_MVCC_OK:reader-{reader_id}:reads={reads_completed}:gens={}",
                    distinct_gens.len()
                );
                return;
            }

            panic!("unexpected LIBRA_RB03_MVCC_ROLE: {mode_str}");
        }

        // Verify versioned codec enforcement on `CONFIG_KV_BY_KEY_INDEX` and `CONFIG_SEQ_TABLE`
        // (`ADR-RB-04` #2), non-overwriting concurrent `RedbConfigStore::create` safety, and
        // fail-closed missing-table checks.
        let codec_dir = tempdir().expect("create temp dir for codec and fault tests");

        // Concurrent `RedbConfigStore::create` race: exactly 1 creator succeeds,
        // all others fail with `AlreadyExists`, winner's data and receipts remain
        // untouched, and no staging temp files leak.
        {
            const CREATORS: usize = 4;
            let race_path = codec_dir.path().join("create-race.redb");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(CREATORS));
            let mut handles = Vec::with_capacity(CREATORS);
            for _ in 0..CREATORS {
                let p = race_path.clone();
                let b = std::sync::Arc::clone(&barrier);
                handles.push(std::thread::spawn(move || {
                    b.wait();
                    RedbConfigStore::create(&p, DatabaseRole::Repository)
                }));
            }
            let mut winners = Vec::new();
            let mut already_exists = 0usize;
            for h in handles {
                match h.join().expect("create race thread panicked") {
                    Ok(store) => winners.push(store),
                    Err(StorageBackendError::Io {
                        kind: std::io::ErrorKind::AlreadyExists,
                        ..
                    }) => already_exists += 1,
                    Err(other) => {
                        panic!("expected Io(AlreadyExists) for create loser, got {other:?}")
                    }
                }
            }
            assert_eq!(winners.len(), 1, "exactly 1 concurrent creator must win");
            assert_eq!(already_exists, CREATORS - 1);
            let winner = winners.into_iter().next().expect("winner store");
            winner
                .set("race.owner", "first", false)
                .await
                .expect("winner set key");
            let winner_receipts = winner
                .list_schema_receipts()
                .await
                .expect("winner initial receipts");

            let late_err = RedbConfigStore::create(&race_path, DatabaseRole::Repository)
                .expect_err("second create on existing file must fail with AlreadyExists");
            assert!(
                matches!(
                    late_err,
                    StorageBackendError::Io {
                        kind: std::io::ErrorKind::AlreadyExists,
                        ..
                    }
                ),
                "expected Io(AlreadyExists) for late create, got {late_err:?}"
            );
            assert_eq!(
                winner
                    .get("race.owner")
                    .await
                    .expect("winner key must survive late create")
                    .map(|e| e.value),
                Some("first".to_string())
            );
            assert_eq!(
                winner
                    .list_schema_receipts()
                    .await
                    .expect("winner receipts must survive late create"),
                winner_receipts
            );
            let leaked_temps: Vec<_> = fs::read_dir(codec_dir.path())
                .expect("read codec_dir")
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                .collect();
            assert!(
                leaked_temps.is_empty(),
                "create race must not leave staging temp files: {leaked_temps:?}"
            );

            // Unix permissions regression: Repository and GlobalConfig stores
            // must be created strictly `0600`; SystemConfig must be `0644`.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let repo_mode = fs::metadata(&race_path)
                    .expect("stat race_path")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    repo_mode, 0o600,
                    "Repository RedbConfigStore must be created with mode 0600, got {repo_mode:04o}"
                );
                let global_perm_path = codec_dir.path().join("perm-global.redb");
                let _global_store =
                    RedbConfigStore::create(&global_perm_path, DatabaseRole::GlobalConfig)
                        .expect("create GlobalConfig redb store");
                let global_mode = fs::metadata(&global_perm_path)
                    .expect("stat global_perm_path")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    global_mode, 0o600,
                    "GlobalConfig RedbConfigStore must be created with mode 0600, got {global_mode:04o}"
                );
                let system_perm_path = codec_dir.path().join("perm-system.redb");
                let _system_store =
                    RedbConfigStore::create(&system_perm_path, DatabaseRole::SystemConfig)
                        .expect("create SystemConfig redb store");
                let system_mode = fs::metadata(&system_perm_path)
                    .expect("stat system_perm_path")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    system_mode, 0o644,
                    "SystemConfig RedbConfigStore must be created with mode 0644, got {system_mode:04o}"
                );

                // Exclusive creation, symlink refusal, parent-directory swap, and
                // post-commit target replacement regressions (Unix):
                // 1) Pre-existing symlink at target MUST fail closed with
                //    `AlreadyExists` (`openat(O_CREAT | O_EXCL | O_NOFOLLOW)`)
                //    without modifying `victim.secret` (`0600`).
                // 2) Renaming/replacing the parent directory during `create` MUST
                //    fail closed (`LBR-IO-001`) because the requested pathname no
                //    longer resolves to the pinned parent & created inode.
                // 3) Unlinking target, replacing target with a symlink to victim,
                //    or replacing target with an attacker DB before `create`
                //    returns MUST fail closed (`LBR-IO-001`), NEVER unlink or
                //    modify the replacement/victim, and NEVER return `Ok` with a
                //    detached store.
                let victim_path = codec_dir.path().join("victim.secret");
                fs::write(&victim_path, b"top-secret-contents").expect("write victim");
                fs::set_permissions(&victim_path, fs::Permissions::from_mode(0o600))
                    .expect("chmod 0600 victim");

                // Case 1: Pre-existing symlink at target fails with AlreadyExists; victim untouched.
                let pre_symlink_target = codec_dir.path().join("preexisting-symlink.redb");
                std::os::unix::fs::symlink(&victim_path, &pre_symlink_target)
                    .expect("create preexisting symlink");
                let pre_sym_err =
                    RedbConfigStore::create(&pre_symlink_target, DatabaseRole::SystemConfig)
                        .expect_err("create over preexisting symlink must fail closed");
                assert!(matches!(
                    pre_sym_err,
                    StorageBackendError::Io {
                        kind: std::io::ErrorKind::AlreadyExists,
                        ..
                    }
                ));
                assert_eq!(pre_sym_err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    fs::metadata(&victim_path)
                        .expect("stat victim after preexisting symlink create")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::read(&victim_path).expect("read victim after preexisting symlink create"),
                    b"top-secret-contents"
                );

                // Case 2: Parent directory renamed and replaced during `create` must fail closed.
                let parent_swap_root = codec_dir.path().join("parent-swap-case");
                let orig_parent = parent_swap_root.join("target-parent");
                let moved_parent = parent_swap_root.join("moved-orig-parent");
                fs::create_dir_all(&orig_parent).expect("create orig_parent");
                let parent_swap_target = orig_parent.join("pinned-parent.redb");
                let parent_swap_err = RedbConfigStore::create_with_test_hooks(
                    &parent_swap_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |_| {
                        fs::rename(&orig_parent, &moved_parent)
                            .expect("rename parent dir away before create returns");
                        fs::create_dir(&orig_parent)
                            .expect("place replacement dir at old parent path");
                    },
                )
                .expect_err("create must fail closed when requested parent dir was replaced");
                assert!(matches!(
                    parent_swap_err,
                    StorageBackendError::BackendInternal { .. }
                ));
                assert_eq!(parent_swap_err.stable_code_str(), "LBR-IO-001");
                assert!(
                    !parent_swap_target.exists(),
                    "substituted replacement parent dir must NOT receive the database"
                );

                // Case 3a: Target unlinked after publish and before return must fail closed.
                let unlinked_target = codec_dir.path().join("post-commit-unlinked.redb");
                let unlink_err = RedbConfigStore::create_with_test_hooks(
                    &unlinked_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |created_path| {
                        fs::remove_file(created_path).expect("unlink created file");
                    },
                )
                .expect_err("unlinking target before create returns must fail closed");
                assert!(matches!(
                    unlink_err,
                    StorageBackendError::Io {
                        kind: std::io::ErrorKind::NotFound,
                        ..
                    }
                ));
                assert_eq!(unlink_err.stable_code_str(), "LBR-IO-001");

                // Case 3b: Target replaced with a symlink to `victim.secret` before return must
                // fail closed WITHOUT modifying or deleting the symlink or victim.
                let swap_symlink_target = codec_dir.path().join("post-commit-symlink-swap.redb");
                let symlink_err = RedbConfigStore::create_with_test_hooks(
                    &swap_symlink_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |created_path| {
                        fs::remove_file(created_path).expect("unlink created file");
                        std::os::unix::fs::symlink(&victim_path, created_path)
                            .expect("replace target path with symlink to victim");
                    },
                )
                .expect_err("replacing target with symlink before create returns must fail closed");
                assert!(matches!(
                    symlink_err,
                    StorageBackendError::Io { .. } | StorageBackendError::BackendInternal { .. }
                ));
                assert_eq!(symlink_err.stable_code_str(), "LBR-IO-001");
                assert_eq!(
                    fs::metadata(&victim_path)
                        .expect("stat victim after post-commit symlink swap")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::read(&victim_path).expect("read victim after post-commit symlink swap"),
                    b"top-secret-contents"
                );

                // Case 3c: Target replaced with another process's valid DB before return must
                // fail closed WITHOUT deleting or modifying that other process's database.
                let other_proc_seed = codec_dir.path().join("other-proc-seed.redb");
                {
                    let seed =
                        RedbConfigStore::create(&other_proc_seed, DatabaseRole::SystemConfig)
                            .expect("create other-process seed DB");
                    seed.set("other.proc", "preserved", false)
                        .await
                        .expect("seed other.proc");
                }
                let swap_db_target = codec_dir.path().join("post-commit-other-db.redb");
                let db_swap_err = RedbConfigStore::create_with_test_hooks(
                    &swap_db_target,
                    DatabaseRole::SystemConfig,
                    || Ok(()),
                    |created_path| {
                        fs::remove_file(created_path).expect("unlink original file");
                        fs::copy(&other_proc_seed, created_path)
                            .expect("place another process's valid redb DB at target path");
                    },
                )
                .expect_err(
                    "replacing target with another DB before create returns must fail closed",
                );
                assert!(matches!(
                    db_swap_err,
                    StorageBackendError::BackendInternal { .. }
                ));
                assert_eq!(db_swap_err.stable_code_str(), "LBR-IO-001");
                // Crucially, error path MUST NOT delete the other process's replacement database.
                let surviving_other_db =
                    RedbConfigStore::open_read_only(&swap_db_target, DatabaseRole::SystemConfig)
                        .expect("error path must NOT unlink another process's replacement DB");
                assert_eq!(
                    surviving_other_db
                        .get("other.proc")
                        .await
                        .expect("read preserved other-process DB")
                        .map(|e| e.value),
                    Some("preserved".to_string())
                );
            }

            // Failure-injection during creation before publication MUST leave
            // zero file at `target` and zero temp residue, so immediate retry
            // succeeds cleanly across all platforms.
            {
                let retry_dir = tempdir().expect("create temp dir for create failure/retry test");
                let retry_target = retry_dir.path().join("retry-after-fail.redb");
                let injected_err = RedbConfigStore::create_with_test_hooks(
                    &retry_target,
                    DatabaseRole::Repository,
                    || {
                        Err(StorageBackendError::BackendInternal {
                            detail: "simulated ENOSPC before publication".to_string(),
                        })
                    },
                    |_| {},
                )
                .expect_err("injected failure before publication must error");
                assert!(matches!(
                    injected_err,
                    StorageBackendError::BackendInternal { .. }
                ));
                assert!(
                    fs::symlink_metadata(&retry_target).is_err(),
                    "failed creation must NEVER leave an incomplete file at target"
                );
                let leftovers: Vec<_> = fs::read_dir(retry_dir.path())
                    .expect("read retry_dir")
                    .filter_map(|e| e.ok())
                    .collect();
                assert!(
                    leftovers.is_empty(),
                    "failed creation must leave zero residue in parent dir: {leftovers:?}"
                );

                // Immediate retry must succeed cleanly and be fully usable.
                let retried = RedbConfigStore::create(&retry_target, DatabaseRole::Repository)
                    .expect("retry create after failed attempt must succeed");
                retried
                    .set("retry.verified", "yes", false)
                    .await
                    .expect("write to retried store");
                assert_eq!(
                    retried
                        .get("retry.verified")
                        .await
                        .expect("read back from retried store")
                        .map(|e| e.value),
                    Some("yes".to_string())
                );
            }

            // Async write cancellation safety: cancelling/timing out a write
            // while waiting on the write lock OR while executing before commit
            // MUST abort cleanly with zero committed mutations.
            {
                let cancel_path = codec_dir.path().join("cancel-safety.redb");
                let cancel_store = RedbConfigStore::create(&cancel_path, DatabaseRole::Repository)
                    .expect("create cancel-safety store");

                // 1. Cancel while queued/waiting on an external write lock.
                {
                    let raw_db = open_redb_writable(
                        &cancel_path,
                        DatabaseRole::Repository,
                        RedbRepairContext::WriteCommand,
                    )
                    .expect("open raw db to hold write lock");
                    let held_wtx = begin_write_bounded_blocking(
                        &raw_db,
                        DatabaseRole::Repository,
                        Duration::from_secs(5),
                    )
                    .expect("acquire external write lock");

                    let timed_out = tokio::time::timeout(
                        Duration::from_millis(60),
                        cancel_store.add("cancel.queued", "must-not-commit", false),
                    )
                    .await;
                    assert!(
                        timed_out.is_err(),
                        "write waiting on held lock must time out and cancel"
                    );
                    drop(held_wtx);
                }
                // Give the cancelled blocking worker ample time to observe cancellation and exit.
                tokio::time::sleep(Duration::from_millis(80)).await;
                assert_eq!(
                    cancel_store
                        .get("cancel.queued")
                        .await
                        .expect("read after queued write cancellation"),
                    None,
                    "cancelled queued write must never commit after lock release"
                );

                // 2. Cancel while executing `op(&wtx)` inside `spawn_blocking` before commit.
                let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let store_for_task = cancel_store.clone();
                let write_task = tokio::spawn(async move {
                    store_for_task
                        .add_with_pre_commit_hook(
                            "cancel.inflight",
                            "must-not-commit",
                            false,
                            move || {
                                let _ = entered_tx.send(());
                                let _ = release_rx.recv();
                            },
                        )
                        .await
                });

                // Wait until the blocking worker has already mutated the uncommitted `WriteTransaction`.
                tokio::task::spawn_blocking(move || {
                    entered_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("write task must reach pre-commit hook")
                })
                .await
                .expect("wait for pre-commit hook");

                // Abort the async task (dropping its future and running `CancelOnDrop`).
                write_task.abort();
                let _ = write_task.await;

                // Release the paused worker so it checks cancellation and aborts `wtx`.
                release_tx.send(()).expect("release paused write worker");
                tokio::time::sleep(Duration::from_millis(50)).await;

                assert_eq!(
                    cancel_store
                        .get("cancel.inflight")
                        .await
                        .expect("read after in-flight pre-commit cancellation"),
                    None,
                    "cancelled in-flight write must abort and never commit"
                );
                // Store remains healthy for subsequent writes.
                cancel_store
                    .add("cancel.after", "healthy", false)
                    .await
                    .expect("subsequent write after cancellation must succeed");
                assert_eq!(
                    cancel_store
                        .get("cancel.after")
                        .await
                        .expect("read cancel.after")
                        .map(|e| e.value),
                    Some("healthy".to_string())
                );
            }

            // Windows permissive-parent ACL regression: verify that creating
            // `Repository` and `GlobalConfig` stores inside a directory with an
            // explicit Everyone (`WD`) Full Access inherited ACL still produces
            // a protected (`SE_DACL_PROTECTED`) owner-only file passing
            // `windows_fs::validate_private`, and that `SystemConfig` also
            // blocks inherited write access.
            #[cfg(windows)]
            {
                use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};

                use windows_sys::Win32::{
                    Foundation::{LocalFree, PSID},
                    Security::{
                        Authorization::{
                            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                            SE_FILE_OBJECT, SetNamedSecurityInfoW,
                        },
                        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl,
                        GetSecurityDescriptorDacl, SE_DACL_PROTECTED,
                    },
                };

                let perm_parent = codec_dir.path().join("win-permissive-parent");
                fs::create_dir_all(&perm_parent).expect("create win permissive parent");
                let sddl: Vec<u16> = std::ffi::OsStr::new("D:(A;OICI;FA;;;WD)")
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let mut raw_sd: *mut std::ffi::c_void = std::ptr::null_mut();
                let ok = unsafe {
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        1,
                        &mut raw_sd,
                        std::ptr::null_mut(),
                    )
                };
                assert_ne!(ok, 0, "convert permissive parent SDDL");
                let mut dacl_present = 0;
                let mut dacl = std::ptr::null_mut();
                let mut dacl_defaulted = 0;
                let got_dacl = unsafe {
                    GetSecurityDescriptorDacl(
                        raw_sd,
                        &mut dacl_present,
                        &mut dacl,
                        &mut dacl_defaulted,
                    )
                };
                assert_ne!(got_dacl, 0, "extract permissive DACL");
                let mut wide_parent: Vec<u16> = perm_parent
                    .as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let set_rc = unsafe {
                    SetNamedSecurityInfoW(
                        wide_parent.as_mut_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut() as PSID,
                        std::ptr::null_mut() as PSID,
                        dacl,
                        std::ptr::null_mut(),
                    )
                };
                unsafe { LocalFree(raw_sd) };
                assert_eq!(set_rc, 0, "apply permissive Everyone ACL to parent dir");

                for (name, role) in [
                    ("repo-private.redb", DatabaseRole::Repository),
                    ("global-private.redb", DatabaseRole::GlobalConfig),
                ] {
                    let target = perm_parent.join(name);
                    let _store =
                        RedbConfigStore::create(&target, role).expect("create private redb store");
                    let f = fs::File::open(&target).expect("open created redb store");
                    crate::utils::windows_fs::validate_private(&f).expect(
                        "Repository/GlobalConfig store under permissive parent must enforce protected owner-only DACL",
                    );
                }

                let sys_target = perm_parent.join("system-protected.redb");
                let _sys_store = RedbConfigStore::create(&sys_target, DatabaseRole::SystemConfig)
                    .expect("create SystemConfig redb store");
                let sys_file = fs::File::open(&sys_target).expect("open system redb store");
                let mut sd_out = std::ptr::null_mut();
                let rc = unsafe {
                    GetSecurityInfo(
                        sys_file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut sd_out,
                    )
                };
                assert_eq!(rc, 0, "query SystemConfig DACL");
                let mut control = 0u16;
                let mut rev = 0u32;
                let ctrl_ok =
                    unsafe { GetSecurityDescriptorControl(sd_out, &mut control, &mut rev) };
                unsafe { LocalFree(sd_out) };
                assert_ne!(ctrl_ok, 0, "read SystemConfig SD control flags");
                assert_ne!(
                    u32::from(control) & SE_DACL_PROTECTED,
                    0,
                    "SystemConfig store must set SE_DACL_PROTECTED to block inherited permissive parent write ACLs"
                );
            }
        }

        // Missing required table fault tests: deleting any required table from a
        // valid `META_SCHEMA` DB must fail closed on fresh open AND on already-open
        // read/write handles (never silently returning empty or recreating tables).
        for (case_label, role) in [
            ("missing_config_kv", DatabaseRole::Repository),
            ("missing_metadata_kv", DatabaseRole::Repository),
            ("missing_schema_versions", DatabaseRole::Repository),
            ("missing_config_schema_versions", DatabaseRole::GlobalConfig),
        ] {
            let fault_path = codec_dir.path().join(format!("{case_label}.redb"));
            {
                let seed = RedbConfigStore::create(&fault_path, role)
                    .expect("create missing-table fault fixture");
                seed.set("fault.key", "present", false)
                    .await
                    .expect("seed fault.key");
                if role == DatabaseRole::Repository {
                    seed.set_metadata("branch", "main", "fault", "present", "text")
                        .await
                        .expect("seed fault metadata");
                }
            }
            let open_ro = RedbConfigStore::open_read_only(&fault_path, role)
                .expect("open pre-fault read-only handle");
            let open_rw = RedbConfigStore::open_read_write(&fault_path, role)
                .expect("open pre-fault read-write handle");

            {
                let raw_db = open_redb_writable(&fault_path, role, RedbRepairContext::Fsck)
                    .expect("open raw db for missing-table fault");
                let wtx = begin_write_bounded_blocking(&raw_db, role, Duration::from_secs(5))
                    .expect("begin raw wtx for missing-table fault");
                let deleted = match case_label {
                    "missing_config_kv" => wtx.delete_table(CONFIG_KV_TABLE),
                    "missing_metadata_kv" => wtx.delete_table(METADATA_KV_TABLE),
                    "missing_schema_versions" => wtx.delete_table(SCHEMA_VERSIONS_TABLE),
                    "missing_config_schema_versions" => {
                        wtx.delete_table(CONFIG_SCHEMA_VERSIONS_TABLE)
                    }
                    _ => unreachable!(),
                }
                .expect("delete required table");
                assert!(deleted, "required table must have existed before deletion");
                wtx.commit().expect("commit required table deletion");
            }

            let fresh_ro_err = RedbConfigStore::open_read_only(&fault_path, role)
                .expect_err("fresh open_read_only must fail closed when required table is missing");
            assert!(
                matches!(fresh_ro_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on fresh open_read_only, got {fresh_ro_err:?}"
            );
            assert_eq!(fresh_ro_err.stable_code_str(), "LBR-IO-001");

            let fresh_rw_err = RedbConfigStore::open_read_write(&fault_path, role).expect_err(
                "fresh open_read_write must fail closed when required table is missing",
            );
            assert!(
                matches!(fresh_rw_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on fresh open_read_write, got {fresh_rw_err:?}"
            );
            assert_eq!(fresh_rw_err.stable_code_str(), "LBR-IO-001");

            // Already-open read handle must fail closed on BOTH direct-table AND
            // cross-table reads rather than treating a missing table as empty or
            // returning partial results from surviving tables.
            let open_read_err = match case_label {
                "missing_config_kv" => open_ro
                    .get("absent.key")
                    .await
                    .expect_err("already-open RO handle must fail closed on missing config_kv"),
                "missing_metadata_kv" => open_ro
                    .get_metadata("branch", "main", "fault")
                    .await
                    .expect_err("already-open RO handle must fail closed on missing metadata_kv"),
                "missing_schema_versions" | "missing_config_schema_versions" => open_ro
                    .list_schema_receipts()
                    .await
                    .expect_err("already-open RO handle must fail closed on missing schema ledger"),
                _ => unreachable!(),
            };
            assert!(
                matches!(open_read_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on already-open direct read, got {open_read_err:?}"
            );
            assert_eq!(open_read_err.stable_code_str(), "LBR-IO-001");

            let cross_table_err = match case_label {
                "missing_config_kv" => open_ro
                    .get_metadata("branch", "main", "fault")
                    .await
                    .expect_err("already-open RO metadata read must fail closed when config_kv is missing"),
                "missing_metadata_kv"
                | "missing_schema_versions"
                | "missing_config_schema_versions" => open_ro
                    .list_all()
                    .await
                    .expect_err("already-open RO config_kv list_all must fail closed when another required table is missing"),
                _ => unreachable!(),
            };
            assert!(
                matches!(cross_table_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on already-open cross-table read, got {cross_table_err:?}"
            );
            assert_eq!(cross_table_err.stable_code_str(), "LBR-IO-001");

            // Already-open write handle must fail closed rather than silently recreating deleted table.
            let open_write_err = open_rw
                .set("after.damage", "must-fail", false)
                .await
                .expect_err("already-open RW handle must refuse write on damaged DB");
            assert!(
                matches!(open_write_err, StorageBackendError::BackendInternal { .. }),
                "{case_label}: expected BackendInternal on already-open write, got {open_write_err:?}"
            );
            assert_eq!(open_write_err.stable_code_str(), "LBR-IO-001");
        }

        // Sequence counter fault tests for BOTH `config_kv` and legacy `config`:
        // a deleted or regressed sequence counter on a non-empty table must fail
        // closed rather than reusing ID 1 and silently overwriting existing data.
        let regressed_zero_bytes =
            encode_versioned(CONFIG_SEQ_CODEC_VERSION, &0u64).expect("encode zero sequence");
        for (seq_fault, delete_entry) in [("missing_seq", true), ("regressed_seq", false)] {
            // 1. `config_kv` sequence fault.
            let kv_seq_path = codec_dir.path().join(format!("kv-{seq_fault}.redb"));
            {
                let seed = RedbConfigStore::create(&kv_seq_path, DatabaseRole::Repository)
                    .expect("create kv sequence fault fixture");
                seed.add("seq.original", "must-not-overwrite", false)
                    .await
                    .expect("seed seq.original");
            }
            {
                let raw_db = open_redb_writable(
                    &kv_seq_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for kv seq fault");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin wtx for kv seq fault");
                {
                    let mut seq_table = wtx.open_table(CONFIG_SEQ_TABLE).expect("open seq table");
                    if delete_entry {
                        assert!(
                            seq_table
                                .remove("config_kv")
                                .expect("remove config_kv seq")
                                .is_some()
                        );
                    } else {
                        seq_table
                            .insert("config_kv", regressed_zero_bytes.as_slice())
                            .expect("regress config_kv seq to 0");
                    }
                }
                wtx.commit().expect("commit kv seq fault");
            }
            let kv_store = RedbConfigStore::open_read_write(&kv_seq_path, DatabaseRole::Repository)
                .expect("open kv seq fault fixture");
            let kv_err = kv_store
                .add("seq.new", "would-overwrite-id-1", false)
                .await
                .expect_err("config_kv insert must fail closed on missing/regressed sequence");
            assert!(
                matches!(kv_err, StorageBackendError::BackendInternal { .. }),
                "{seq_fault} config_kv: expected BackendInternal, got {kv_err:?}"
            );
            assert_eq!(kv_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                kv_store
                    .list_all()
                    .await
                    .expect("existing config_kv rows must stay intact"),
                vec![ConfigEntry {
                    key: "seq.original".to_string(),
                    value: "must-not-overwrite".to_string(),
                    encrypted: false,
                }]
            );

            // 2. Legacy `config` sequence fault.
            let leg_seq_path = codec_dir.path().join(format!("legacy-{seq_fault}.redb"));
            let original_legacy = LegacyConfigEntry {
                configuration: "core".to_string(),
                name: None,
                key: "bare".to_string(),
                value: "false".to_string(),
            };
            {
                let seed = RedbConfigStore::create(&leg_seq_path, DatabaseRole::Repository)
                    .expect("create legacy sequence fault fixture");
                seed.insert_legacy_entry(&original_legacy)
                    .await
                    .expect("seed original legacy row");
            }
            {
                let raw_db = open_redb_writable(
                    &leg_seq_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for legacy seq fault");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin wtx for legacy seq fault");
                {
                    let mut seq_table = wtx.open_table(CONFIG_SEQ_TABLE).expect("open seq table");
                    if delete_entry {
                        assert!(
                            seq_table
                                .remove("config")
                                .expect("remove legacy config seq")
                                .is_some()
                        );
                    } else {
                        seq_table
                            .insert("config", regressed_zero_bytes.as_slice())
                            .expect("regress legacy config seq to 0");
                    }
                }
                wtx.commit().expect("commit legacy seq fault");
            }
            let leg_store =
                RedbConfigStore::open_read_write(&leg_seq_path, DatabaseRole::Repository)
                    .expect("open legacy seq fault fixture");
            let leg_err = leg_store
                .insert_legacy_entry(&LegacyConfigEntry {
                    configuration: "core".to_string(),
                    name: None,
                    key: "filemode".to_string(),
                    value: "true".to_string(),
                })
                .await
                .expect_err("legacy config insert must fail closed on missing/regressed sequence");
            assert!(
                matches!(leg_err, StorageBackendError::BackendInternal { .. }),
                "{seq_fault} legacy config: expected BackendInternal, got {leg_err:?}"
            );
            assert_eq!(leg_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                leg_store
                    .list_legacy_entries()
                    .await
                    .expect("existing legacy row must stay intact"),
                vec![original_legacy]
            );
        }

        for (case_name, bad_bytes, expect_future) in [
            ("corrupt_trailing_bytes", vec![1u8, 0u8, 0xFF, 0xEE], false),
            ("future_codec_version", vec![0xFFu8, 0xFFu8], true),
        ] {
            // 1. Corrupt/future secondary index value (`CONFIG_KV_BY_KEY_INDEX`).
            let idx_path = codec_dir.path().join(format!("idx-{case_name}.redb"));
            {
                let store = RedbConfigStore::create(&idx_path, DatabaseRole::Repository)
                    .expect("create idx fault fixture");
                store
                    .set("codec.check", "ok", false)
                    .await
                    .expect("seed codec.check");
            }
            {
                let raw_db = open_redb_writable(
                    &idx_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for index fault injection");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin raw wtx for index fault injection");
                {
                    let mut idx_table = wtx
                        .open_table(CONFIG_KV_BY_KEY_INDEX)
                        .expect("open index table");
                    idx_table
                        .insert(("codec.check", 1u64), bad_bytes.as_slice())
                        .expect("inject bad index codec bytes");
                }
                wtx.commit().expect("commit bad index codec bytes");
            }
            let reopened = RedbConfigStore::open_read_only(&idx_path, DatabaseRole::Repository)
                .expect("reopen idx fixture");
            let idx_err = reopened
                .get("codec.check")
                .await
                .expect_err("corrupt/future index marker must fail closed");
            assert_eq!(idx_err.stable_code_str(), "LBR-IO-001");
            if expect_future {
                assert!(
                    matches!(
                        idx_err,
                        StorageBackendError::UnsupportedSchemaVersion { .. }
                    ),
                    "expected UnsupportedSchemaVersion for index {case_name}, got {idx_err:?}"
                );
            } else {
                assert!(
                    matches!(idx_err, StorageBackendError::CorruptPayload { .. }),
                    "expected CorruptPayload for index {case_name}, got {idx_err:?}"
                );
            }

            // 2. Corrupt/future sequence value (`CONFIG_SEQ_TABLE`).
            let seq_path = codec_dir.path().join(format!("seq-{case_name}.redb"));
            {
                let store = RedbConfigStore::create(&seq_path, DatabaseRole::Repository)
                    .expect("create seq fault fixture");
                store
                    .set("codec.seq", "ok", false)
                    .await
                    .expect("seed codec.seq");
            }
            {
                let raw_db = open_redb_writable(
                    &seq_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Fsck,
                )
                .expect("open raw db for seq fault injection");
                let wtx = begin_write_bounded_blocking(
                    &raw_db,
                    DatabaseRole::Repository,
                    Duration::from_secs(5),
                )
                .expect("begin raw wtx for seq fault injection");
                {
                    let mut seq_table = wtx.open_table(CONFIG_SEQ_TABLE).expect("open seq table");
                    seq_table
                        .insert("config_kv", bad_bytes.as_slice())
                        .expect("inject bad seq codec bytes");
                }
                wtx.commit().expect("commit bad seq codec bytes");
            }
            let reopened_rw = RedbConfigStore::open_read_write(&seq_path, DatabaseRole::Repository)
                .expect("reopen seq fixture");
            let seq_err = reopened_rw
                .add("codec.seq2", "next", false)
                .await
                .expect_err("corrupt/future sequence record must fail closed");
            assert_eq!(seq_err.stable_code_str(), "LBR-IO-001");
            if expect_future {
                assert!(
                    matches!(
                        seq_err,
                        StorageBackendError::UnsupportedSchemaVersion { .. }
                    ),
                    "expected UnsupportedSchemaVersion for seq {case_name}, got {seq_err:?}"
                );
            } else {
                assert!(
                    matches!(
                        seq_err,
                        StorageBackendError::CorruptPayload { .. }
                            | StorageBackendError::TruncatedPayload { .. }
                    ),
                    "expected CorruptPayload/TruncatedPayload for seq {case_name}, got {seq_err:?}"
                );
            }
        }

        // Parent orchestrator: create fixture with generation 0 (4 entries: 2 singletons + 2 multi-value).
        let dir = tempdir().expect("create temp dir for MVCC test");
        let db_path = dir.path().join("git.gen-00000001.redb");
        let sync_dir = dir.path().join("sync");
        fs::create_dir_all(&sync_dir).expect("create sync dir");

        {
            let store = RedbConfigStore::create(&db_path, DatabaseRole::Repository)
                .expect("create initial RedbConfigStore");
            store
                .set("mvcc.g0.seq", "atomic-v1", false)
                .await
                .expect("seed seq");
            store
                .set("mvcc.g0.tag", "consistent", false)
                .await
                .expect("seed tag");
            store
                .add("mvcc.g0.item", "alpha", false)
                .await
                .expect("seed item 1");
            store
                .add("mvcc.g0.item", "beta", false)
                .await
                .expect("seed item 2");
        }

        let exe = env::current_exe().expect("locate current test executable");
        let mut guard = ChildBatchGuard {
            children: Vec::with_capacity(READER_COUNT + 1),
        };

        let spawn_worker = |role_name: String| -> (String, Child) {
            let child = Command::new(&exe)
                .arg("--exact")
                .arg("storage::config_store::tests::redb_config_mvcc_concurrent_readers")
                .arg("--nocapture")
                .env("LIBRA_RB03_MVCC_ROLE", &role_name)
                .env("LIBRA_RB03_MVCC_DB", &db_path)
                .env("LIBRA_RB03_MVCC_SYNC_DIR", &sync_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn MVCC worker child");
            (role_name, child)
        };

        for idx in 0..READER_COUNT {
            guard.children.push(spawn_worker(format!("reader-{idx}")));
        }
        guard.children.push(spawn_worker("writer".to_string()));

        // Bounded wait loop keeping `guard` armed until each child exits cleanly.
        let deadline = Instant::now() + CHILD_TIMEOUT;
        let mut completed = Vec::with_capacity(READER_COUNT + 1);
        while !guard.children.is_empty() {
            let mut idx = 0;
            while idx < guard.children.len() {
                if let Some(_status) = guard.children[idx]
                    .1
                    .try_wait()
                    .expect("poll MVCC child status")
                {
                    let (name, child) = guard.children.swap_remove(idx);
                    let output = child.wait_with_output().expect("collect MVCC child output");
                    completed.push((name, output));
                } else {
                    idx += 1;
                }
            }
            if !guard.children.is_empty() {
                assert!(
                    Instant::now() < deadline,
                    "MVCC child processes exceeded {CHILD_TIMEOUT:?} timeout"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        assert_eq!(completed.len(), READER_COUNT + 1);
        for (name, output) in completed {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let sentinel = format!("RB03_MVCC_OK:{name}");
            assert!(
                output.status.success()
                    && stdout.contains(&sentinel)
                    && stdout.contains("1 passed; 0 failed"),
                "MVCC child `{name}` did not succeed cleanly (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        }

        // Verify final database state via read-only opener matches WRITER_ROUNDS.
        let verify_store = RedbConfigStore::open_read_only(&db_path, DatabaseRole::Repository)
            .expect("reopen read-only after MVCC stress");
        let final_entries = verify_store.list_all().await.expect("final list_all");
        assert_eq!(final_entries.len(), 4);
        let expected_prefix = format!("mvcc.g{WRITER_ROUNDS}.");
        assert!(
            final_entries
                .iter()
                .all(|e| e.key.starts_with(&expected_prefix)),
            "final entries must all be at generation {WRITER_ROUNDS}: {final_entries:?}"
        );
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_config_gc13_repo_isolation_and_scope_file_modes() {
        use std::{
            env, fs,
            path::PathBuf,
            process::{Command, Stdio},
        };

        use tempfile::tempdir;

        use crate::storage::redb::RedbConfigStore;

        // Isolated child mode so `LIBRA_CONFIG_GLOBAL_DB`, `LIBRA_CONFIG_SYSTEM_DB`,
        // and Unix `umask` mutations never affect parallel tests in the parent process.
        if let Some(child_mode) = env::var_os("LIBRA_RB03J_CHILD_MODE") {
            let mode_str = child_mode.to_string_lossy();
            match mode_str.as_ref() {
                "repo_isolation" => {
                    let repo_db = PathBuf::from(
                        env::var_os("LIBRA_RB03J_REPO_DB").expect("repo db path env"),
                    );
                    let store = RedbConfigStore::create(&repo_db, DatabaseRole::Repository)
                        .expect("create Repository RedbConfigStore");
                    store
                        .set("user.name", "Repo Isolated User", false)
                        .await
                        .expect("set repo user.name");
                    store
                        .set("user.global", "repo-must-not-touch-global", false)
                        .await
                        .expect("set repo user.global");
                    store
                        .set("core.system", "repo-must-not-touch-system", false)
                        .await
                        .expect("set repo core.system");
                    store
                        .add(
                            "remote.origin.fetch",
                            "+refs/heads/*:refs/remotes/origin/*",
                            false,
                        )
                        .await
                        .expect("add repo fetch");
                    store
                        .rename_section("remote.origin", "remote.upstream")
                        .await
                        .expect("rename repo section");
                    store
                        .remove_section("remote.upstream")
                        .await
                        .expect("remove repo section");
                    store
                        .insert_legacy_entry(&LegacyConfigEntry {
                            configuration: "core".to_string(),
                            name: None,
                            key: "bare".to_string(),
                            value: "false".to_string(),
                        })
                        .await
                        .expect("insert repo legacy entry");
                    store
                        .record_schema_receipt(&SchemaVersionReceipt {
                            version: 99,
                            name: "rb03j_isolation_receipt".to_string(),
                            applied_at: "2026-11-22T00:00:00Z".to_string(),
                        })
                        .await
                        .expect("record repo receipt");
                    store
                        .set_metadata("branch", "main", "note", "repo-only", "text")
                        .await
                        .expect("set repo metadata");
                    let cas = store
                        .cas_metadata(
                            "branch",
                            "main",
                            "note",
                            Some("repo-only"),
                            Some(("repo-only-v2", "text")),
                        )
                        .await
                        .expect("cas repo metadata");
                    assert!(matches!(cas, MetadataCasOutcome::Swapped { .. }));
                    println!("RB03J_CHILD_OK:repo_isolation");
                    return;
                }
                "global_mode_permissive_umask" => {
                    #[cfg(unix)]
                    // SAFETY: Single-threaded isolated child process; setting permissive umask
                    // proves `GlobalConfig` enforces `0600` explicitly rather than via umask.
                    unsafe {
                        libc::umask(0o000);
                    }
                    let global_db_env = PathBuf::from(
                        env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV).expect("global db env"),
                    );
                    let scope_dir = config_scope_dir_from_db_path(&global_db_env)
                        .expect("resolve global scope dir");
                    let store = RedbConfigStore::create_in_scope_dir(
                        &scope_dir,
                        DatabaseRole::GlobalConfig,
                    )
                    .expect("create GlobalConfig RedbConfigStore in empty scope dir");
                    assert_eq!(store.role(), DatabaseRole::GlobalConfig);
                    store
                        .set("user.email", "global@example.com", false)
                        .await
                        .expect("write global config key");
                    store
                        .add("alias.st", "status", false)
                        .await
                        .expect("add global alias");
                    // GlobalConfig must reject Repository-only `metadata_kv` mutations (`GC-13`).
                    let meta_err = store
                        .set_metadata("branch", "main", "k", "v", "text")
                        .await
                        .expect_err("GlobalConfig must reject metadata_kv writes");
                    assert!(matches!(
                        meta_err,
                        StorageBackendError::RoleMismatch {
                            expected: DatabaseRole::Repository,
                            found: DatabaseRole::GlobalConfig,
                        }
                    ));
                    println!("RB03J_CHILD_OK:global_mode_permissive_umask");
                    return;
                }
                "system_mode_restrictive_umask" => {
                    #[cfg(unix)]
                    // SAFETY: Single-threaded isolated child process; setting restrictive umask
                    // `0o077` proves `SystemConfig` enforces exact `0644` explicitly despite umask.
                    unsafe {
                        libc::umask(0o077);
                    }
                    let system_db_env = PathBuf::from(
                        env::var_os(LIBRA_CONFIG_SYSTEM_DB_ENV).expect("system db env"),
                    );
                    let scope_dir = config_scope_dir_from_db_path(&system_db_env)
                        .expect("resolve system scope dir");
                    let store = RedbConfigStore::create_in_scope_dir(
                        &scope_dir,
                        DatabaseRole::SystemConfig,
                    )
                    .expect("create SystemConfig RedbConfigStore in empty scope dir");
                    assert_eq!(store.role(), DatabaseRole::SystemConfig);
                    store
                        .set("core.editor", "vim", false)
                        .await
                        .expect("write system config key");
                    store
                        .add("http.extraHeader", "X-Test: 1", false)
                        .await
                        .expect("add system multi-value key");
                    // SystemConfig must reject Repository-only `metadata_kv` mutations (`GC-13`).
                    let meta_err = store
                        .set_metadata("branch", "main", "k", "v", "text")
                        .await
                        .expect_err("SystemConfig must reject metadata_kv writes");
                    assert!(matches!(
                        meta_err,
                        StorageBackendError::RoleMismatch {
                            expected: DatabaseRole::Repository,
                            found: DatabaseRole::SystemConfig,
                        }
                    ));
                    println!("RB03J_CHILD_OK:system_mode_restrictive_umask");
                    return;
                }
                other => panic!("unexpected LIBRA_RB03J_CHILD_MODE: {other}"),
            }
        }

        let exe = env::current_exe().expect("locate current test executable");
        let run_child = |mode: &str, envs: &[(&str, PathBuf)]| {
            let mut cmd = Command::new(&exe);
            cmd.arg("--exact")
                .arg("storage::config_store::tests::redb_config_gc13_repo_isolation_and_scope_file_modes")
                .arg("--nocapture")
                .env("LIBRA_RB03J_CHILD_MODE", mode)
                .env("LIBRA_TEST", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in envs {
                cmd.env(k, v);
            }
            let output = cmd.output().expect("spawn RB-03j child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let sentinel = format!("RB03J_CHILD_OK:{mode}");
            assert!(
                output.status.success()
                    && stdout.contains(&sentinel)
                    && stdout.contains("1 passed; 0 failed"),
                "RB-03j child `{mode}` failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        };

        let root = tempdir().expect("create RB-03j temp root");

        // -----------------------------------------------------------------
        // AC1 & AC2 (Part A): Absent `LIBRA_CONFIG_GLOBAL_DB` and
        // `LIBRA_CONFIG_SYSTEM_DB` fixtures must NEVER be created when a
        // `Repository` `RedbConfigStore` is created and written to.
        // -----------------------------------------------------------------
        let repo_a_dir = root.path().join("repo-a");
        fs::create_dir_all(&repo_a_dir).expect("create repo-a dir");
        let repo_a_db = repo_a_dir.join("git.gen-00000001.redb");
        let absent_global_dir = root.path().join("absent-global");
        let absent_global_db = absent_global_dir.join(CONFIG_SQLITE_FILE_NAME);
        let absent_system_dir = root.path().join("absent-system");
        let absent_system_db = absent_system_dir.join(CONFIG_SQLITE_FILE_NAME);

        run_child(
            "repo_isolation",
            &[
                ("LIBRA_RB03J_REPO_DB", repo_a_db.clone()),
                (LIBRA_CONFIG_GLOBAL_DB_ENV, absent_global_db.clone()),
                (LIBRA_CONFIG_SYSTEM_DB_ENV, absent_system_db.clone()),
            ],
        );

        assert!(
            !absent_global_dir.exists() && !absent_global_db.exists(),
            "GC-13 violation: Repository write created absent LIBRA_CONFIG_GLOBAL_DB fixture"
        );
        assert!(
            !absent_system_dir.exists() && !absent_system_db.exists(),
            "GC-13 violation: Repository write created absent LIBRA_CONFIG_SYSTEM_DB fixture"
        );
        assert!(
            !config_redb_path_in_scope_dir(&absent_global_dir).exists()
                && !config_redb_path_in_scope_dir(&absent_system_dir).exists(),
            "GC-13 violation: Repository write created config.redb in absent global/system scope"
        );

        // Verify `create_in_scope_dir` refuses `DatabaseRole::Repository`.
        let bad_scope_err =
            RedbConfigStore::create_in_scope_dir(&repo_a_dir, DatabaseRole::Repository)
                .expect_err("create_in_scope_dir must reject DatabaseRole::Repository");
        assert_eq!(bad_scope_err.stable_code_str(), "LBR-IO-001");

        // -----------------------------------------------------------------
        // AC3 & AC4: Empty `GlobalConfig` scope dir creates exact `0600`
        // `config.redb` (even under permissive `umask(0o000)`), and empty
        // `SystemConfig` scope dir creates exact `0644` `config.redb` (even
        // under restrictive `umask(0o077)`).
        // -----------------------------------------------------------------
        let global_scope_dir = root.path().join("empty-global-scope");
        let system_scope_dir = root.path().join("empty-system-scope");
        fs::create_dir_all(&global_scope_dir).expect("create empty global scope dir");
        fs::create_dir_all(&system_scope_dir).expect("create empty system scope dir");

        let global_env_db = config_sqlite_path_in_scope_dir(&global_scope_dir);
        let system_env_db = config_sqlite_path_in_scope_dir(&system_scope_dir);
        let global_redb = config_redb_path_in_scope_dir(&global_scope_dir);
        let system_redb = config_redb_path_in_scope_dir(&system_scope_dir);

        run_child(
            "global_mode_permissive_umask",
            &[(LIBRA_CONFIG_GLOBAL_DB_ENV, global_env_db.clone())],
        );
        run_child(
            "system_mode_restrictive_umask",
            &[(LIBRA_CONFIG_SYSTEM_DB_ENV, system_env_db.clone())],
        );

        assert!(
            global_redb.is_file(),
            "GlobalConfig config.redb must be created in empty scope dir"
        );
        assert!(
            system_redb.is_file(),
            "SystemConfig config.redb must be created in empty scope dir"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let global_mode = fs::symlink_metadata(&global_redb)
                .expect("stat GlobalConfig config.redb")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                global_mode, GLOBAL_CONFIG_FILE_MODE,
                "GlobalConfig config.redb must have exact 0600 permissions, got {global_mode:04o}"
            );

            let system_mode = fs::symlink_metadata(&system_redb)
                .expect("stat SystemConfig config.redb")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                system_mode, SYSTEM_CONFIG_FILE_MODE,
                "SystemConfig config.redb must have exact 0644 permissions, got {system_mode:04o}"
            );

            let repo_mode = fs::symlink_metadata(&repo_a_db)
                .expect("stat Repository redb store")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                repo_mode, REPOSITORY_CONFIG_FILE_MODE,
                "Repository redb store must have exact 0600 permissions, got {repo_mode:04o}"
            );

            assert_eq!(
                expected_config_file_mode(DatabaseRole::Repository),
                Some(0o600)
            );
            assert_eq!(
                expected_config_file_mode(DatabaseRole::GlobalConfig),
                Some(0o600)
            );
            assert_eq!(
                expected_config_file_mode(DatabaseRole::SystemConfig),
                Some(0o644)
            );
            assert_eq!(expected_config_file_mode(DatabaseRole::Capture), None);
            assert_eq!(expected_config_file_mode(DatabaseRole::Derived), None);
        }

        // Verify persisted values and cross-role rejection on GlobalConfig and SystemConfig.
        let global_ro = RedbConfigStore::open_read_only(&global_redb, DatabaseRole::GlobalConfig)
            .expect("open GlobalConfig read-only");
        assert_eq!(
            global_ro.get("user.email").await.expect("read global key"),
            Some(ConfigEntry {
                key: "user.email".to_string(),
                value: "global@example.com".to_string(),
                encrypted: false,
            })
        );
        let wrong_global_role =
            RedbConfigStore::open_read_only(&global_redb, DatabaseRole::Repository)
                .expect_err("opening GlobalConfig store as Repository must fail closed");
        assert!(matches!(
            wrong_global_role,
            StorageBackendError::RoleMismatch {
                expected: DatabaseRole::Repository,
                found: DatabaseRole::GlobalConfig,
            }
        ));
        drop(global_ro);

        let system_ro = RedbConfigStore::open_read_only(&system_redb, DatabaseRole::SystemConfig)
            .expect("open SystemConfig read-only");
        assert_eq!(
            system_ro.get("core.editor").await.expect("read system key"),
            Some(ConfigEntry {
                key: "core.editor".to_string(),
                value: "vim".to_string(),
                encrypted: false,
            })
        );
        let wrong_system_role =
            RedbConfigStore::open_read_only(&system_redb, DatabaseRole::GlobalConfig)
                .expect_err("opening SystemConfig store as GlobalConfig must fail closed");
        assert!(matches!(
            wrong_system_role,
            StorageBackendError::RoleMismatch {
                expected: DatabaseRole::GlobalConfig,
                found: DatabaseRole::SystemConfig,
            }
        ));
        drop(system_ro);

        // -----------------------------------------------------------------
        // AC1 & AC2 (Part B): Pre-existing populated `LIBRA_CONFIG_GLOBAL_DB`
        // and `LIBRA_CONFIG_SYSTEM_DB` fixtures (both `config.db` and
        // `config.redb`) must remain byte-for-byte untouched after full
        // `Repository` `RedbConfigStore` creation and mutations.
        // -----------------------------------------------------------------
        fs::write(&global_env_db, b"PREEXISTING_GLOBAL_SQLITE_FIXTURE\n")
            .expect("seed preexisting global config.db fixture");
        fs::write(&system_env_db, b"PREEXISTING_SYSTEM_SQLITE_FIXTURE\n")
            .expect("seed preexisting system config.db fixture");

        let global_db_before = fs::read(&global_env_db).expect("read global config.db before");
        let global_redb_before = fs::read(&global_redb).expect("read global config.redb before");
        let system_db_before = fs::read(&system_env_db).expect("read system config.db before");
        let system_redb_before = fs::read(&system_redb).expect("read system config.redb before");

        let repo_b_dir = root.path().join("repo-b");
        fs::create_dir_all(&repo_b_dir).expect("create repo-b dir");
        let repo_b_db = repo_b_dir.join("git.gen-00000001.redb");

        run_child(
            "repo_isolation",
            &[
                ("LIBRA_RB03J_REPO_DB", repo_b_db.clone()),
                (LIBRA_CONFIG_GLOBAL_DB_ENV, global_env_db.clone()),
                (LIBRA_CONFIG_SYSTEM_DB_ENV, system_env_db.clone()),
            ],
        );

        assert_eq!(
            fs::read(&global_env_db).expect("read global config.db after"),
            global_db_before,
            "GC-13 violation: Repository write modified preexisting LIBRA_CONFIG_GLOBAL_DB"
        );
        assert_eq!(
            fs::read(&global_redb).expect("read global config.redb after"),
            global_redb_before,
            "GC-13 violation: Repository write modified preexisting GlobalConfig config.redb"
        );
        assert_eq!(
            fs::read(&system_env_db).expect("read system config.db after"),
            system_db_before,
            "GC-13 violation: Repository write modified preexisting LIBRA_CONFIG_SYSTEM_DB"
        );
        assert_eq!(
            fs::read(&system_redb).expect("read system config.redb after"),
            system_redb_before,
            "GC-13 violation: Repository write modified preexisting SystemConfig config.redb"
        );

        // Confirm subsequent writable reopen & mutation on GlobalConfig and SystemConfig
        // preserves exact `0600` / `0644` modes on Unix.
        let global_rw = RedbConfigStore::open_read_write(&global_redb, DatabaseRole::GlobalConfig)
            .expect("reopen GlobalConfig rw");
        global_rw
            .set("user.name", "Global Mutated", false)
            .await
            .expect("mutate GlobalConfig");
        drop(global_rw);

        let system_rw = RedbConfigStore::open_read_write(&system_redb, DatabaseRole::SystemConfig)
            .expect("reopen SystemConfig rw");
        system_rw
            .set("core.pager", "less", false)
            .await
            .expect("mutate SystemConfig");
        drop(system_rw);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::symlink_metadata(&global_redb)
                    .expect("stat GlobalConfig after reopen write")
                    .permissions()
                    .mode()
                    & 0o777,
                GLOBAL_CONFIG_FILE_MODE,
                "GlobalConfig config.redb must retain exact 0600 mode after writes"
            );
            assert_eq!(
                fs::symlink_metadata(&system_redb)
                    .expect("stat SystemConfig after reopen write")
                    .permissions()
                    .mode()
                    & 0o777,
                SYSTEM_CONFIG_FILE_MODE,
                "SystemConfig config.redb must retain exact 0644 mode after writes"
            );
        }
    }
}
