//! Config storage helpers backed by sea-orm.
//!
//! Two APIs exist side-by-side:
//!
//! 1. [`ConfigKv`] (preferred) — flat dotted keys like `remote.origin.url` stored
//!    in the `config_kv` table, with per-row encryption support and a richer
//!    set of CRUD primitives (`set`, `add`, `unset`, `unset_all`, regex/prefix
//!    queries). All new code should use this API.
//! 2. [`Config`] (deprecated) — three-column form `(configuration, name, key)`
//!    stored in the legacy `config` table. Retained for backwards-compatible
//!    repos that have not yet migrated.
//!
//! Both APIs follow the same `*_with_conn` transaction-safety convention used
//! by [`crate::internal::branch`]: callers inside an open transaction must use
//! the `_with_conn` variants to avoid acquiring a second pool connection
//! (which deadlocks under SQLite's writer-serialisation).
//!
//! Cross-cutting helpers in this module:
//! - [`resolve_env`] / [`resolve_env_for_target`]: cascading env-var resolution
//!   (process env > local repo config > global config).
//! - [`is_sensitive_key`] / [`is_vault_internal_key`]: heuristics that drive the
//!   encrypt-by-default policy in `libra config`.
//! - [`encrypt_value`] / [`decrypt_value`]: thin wrappers over the vault module.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};

use anyhow::{Context, Result, anyhow};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, ModelTrait,
    QueryFilter, QueryOrder, entity::ActiveModelTrait,
};

use crate::{
    internal::{
        db::{DatabaseRole, get_db_conn_instance, get_db_conn_instance_for_path, schema},
        head::Head,
        model::{
            config::{self, ActiveModel, Model},
            config_kv,
        },
        vault::{decrypt_token, encrypt_token, load_unseal_key_for_scope},
    },
    utils::util::{DATABASE, try_get_storage_path},
};

mod global_migration;

// ─────────────────────────────────────────────────────────────────────────────
// ConfigKv — new flat key/value API backed by the `config_kv` table
// ─────────────────────────────────────────────────────────────────────────────

/// One row from the `config_kv` table, decoded for application use.
///
/// `encrypted == true` means `value` is hex-encoded ciphertext that must be
/// decrypted via [`decrypt_value`] before display. The encrypt flag is stored
/// as INTEGER (0/1) in SQLite; this struct normalises it to `bool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKvEntry {
    /// Dotted config key, e.g. `remote.origin.url` or `vault.env.GEMINI_API_KEY`.
    pub key: String,
    /// Either plaintext or hex ciphertext depending on `encrypted`.
    pub value: String,
    /// `true` when `value` is hex-encoded ciphertext.
    pub encrypted: bool,
}

impl ConfigKvEntry {
    /// Convert a sea-orm row into the public [`ConfigKvEntry`] shape.
    fn from_model(m: &config_kv::Model) -> Self {
        Self {
            key: m.key.clone(),
            value: m.value.clone(),
            encrypted: m.encrypted != 0,
        }
    }
}

fn remote_namespace_variable<'a>(key: &'a str, remote: &str) -> Option<&'a str> {
    let (name, variable) = key.strip_prefix("remote.")?.rsplit_once('.')?;
    (name == remote).then_some(variable)
}

fn ssh_remote_namespace_variable<'a>(key: &'a str, remote: &str) -> Option<&'a str> {
    let (name, variable) = key.strip_prefix("vault.ssh.")?.rsplit_once('.')?;
    (name == remote).then_some(variable)
}

fn rewrite_fetch_refspec_destination(value: &str, old: &str, new: &str) -> String {
    let Some((source, destination)) = value.split_once(':') else {
        return value.to_string();
    };
    let old_prefix = format!("refs/remotes/{old}/");
    let Some(suffix) = destination.strip_prefix(&old_prefix) else {
        return value.to_string();
    };
    format!("{source}:refs/remotes/{new}/{suffix}")
}

/// Flat key/value configuration access backed by the `config_kv` table.
///
/// Marker struct; all methods are associated functions. Calling a method
/// without `_with_conn` acquires its own connection — do **not** call those
/// from inside a `db.transaction(|txn| { ... })` block (deadlock).
/// Parse Git's boolean config spellings (`git_parse_maybe_bool_text`):
/// `true`/`yes`/`on`/`1` → `Some(true)`, `false`/`no`/`off`/`0` →
/// `Some(false)` (trimmed, case-insensitive), anything else → `None`.
/// SHARED by every `core.bare` reader so a valid non-literal spelling can
/// never classify differently across commands.
pub fn parse_git_bool(value: &str) -> Option<bool> {
    match value.trim() {
        v if v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
            || v == "1" =>
        {
            Some(true)
        }
        v if v.eq_ignore_ascii_case("false")
            || v.eq_ignore_ascii_case("no")
            || v.eq_ignore_ascii_case("off")
            || v == "0" =>
        {
            Some(false)
        }
        _ => None,
    }
}

pub struct ConfigKv;

impl ConfigKv {
    // ── Core CRUD (_with_conn) ───────────────────────────────────────────

    /// Get the last value for a key (last-one-wins for multi-value keys).
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` if no row exists.
    /// - When multiple rows share the key (multi-value config like
    ///   `remote.origin.fetch`), the row with the highest `id` wins,
    ///   matching git's "last write" rule.
    /// - The returned value is *not* decrypted; callers must inspect
    ///   `encrypted` and call [`decrypt_value`] themselves.
    pub async fn get_with_conn<C: ConnectionTrait>(
        db: &C,
        key: &str,
    ) -> Result<Option<ConfigKvEntry>> {
        let row = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .order_by_desc(config_kv::Column::Id)
            .one(db)
            .await
            .context("failed to query config_kv")?;
        Ok(row.as_ref().map(ConfigKvEntry::from_model))
    }

    /// Get all values for a key (preserves insertion order via ascending `id`).
    ///
    /// Used by multi-value keys (e.g. `remote.origin.fetch` may have several
    /// refspec entries). Returns an empty `Vec` when no rows match.
    pub async fn get_all_with_conn<C: ConnectionTrait>(
        db: &C,
        key: &str,
    ) -> Result<Vec<ConfigKvEntry>> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .order_by_asc(config_kv::Column::Id)
            .all(db)
            .await
            .context("failed to query config_kv")?;
        Ok(rows.iter().map(ConfigKvEntry::from_model).collect())
    }

    /// Get every value for a config variable while matching only the variable
    /// name case-insensitively. The section/subsection prefix remains
    /// case-sensitive, matching Git's config rules, and insertion order is
    /// preserved for multi-valued variables such as `remote.<name>.fetch`.
    pub async fn get_var_all_case_insensitive_with_conn<C: ConnectionTrait>(
        db: &C,
        prefix: &str,
        variable: &str,
    ) -> Result<Vec<ConfigKvEntry>> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(prefix))
            .order_by_asc(config_kv::Column::Id)
            .all(db)
            .await
            .context("failed to query case-insensitive multi-value config variable")?;
        Ok(rows
            .iter()
            .filter(|row| {
                row.key
                    .strip_prefix(prefix)
                    .is_some_and(|name| name.eq_ignore_ascii_case(variable))
            })
            .map(ConfigKvEntry::from_model)
            .collect())
    }

    /// Count values for a key.
    ///
    /// Returns `Ok(0)` when no rows exist. Used by callers that need to decide
    /// between `set` (single-value) and `add` (multi-value) semantics.
    pub async fn count_values_with_conn<C: ConnectionTrait>(db: &C, key: &str) -> Result<usize> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .all(db)
            .await
            .context("failed to count config_kv entries")?;
        Ok(rows.len())
    }

    /// Set a config value (upsert).
    ///
    /// Functional scope:
    /// - If exactly one row exists for `key`, updates it in place.
    /// - If no row exists, inserts a fresh row.
    /// - When the existing row is encrypted but `encrypted == false` is
    ///   passed, the encryption flag is *inherited* (preserved). This avoids
    ///   accidentally downgrading a sensitive value to plaintext.
    ///
    /// Boundary conditions:
    /// - Returns `Err` if multiple rows already exist for `key` — the caller
    ///   must explicitly `unset_all` first or use `add`. Mirrors `git config`'s
    ///   exit code 5.
    pub async fn set_with_conn<C: ConnectionTrait>(
        db: &C,
        key: &str,
        value: &str,
        encrypted: bool,
    ) -> Result<()> {
        // Git config variable names are case-insensitive, so `set` replaces
        // ANY casing of the same logical key (e.g. a default `remote.<n>.fetch`
        // written by `remote add` is superseded by `config set remote.<n>.Fetch`)
        // and stores a single row with the exact key spelling supplied. SQLite
        // `LIKE` matches ASCII case-insensitively; config keys never contain
        // `%`/`_`.
        let existing = config_kv::Entity::find()
            .filter(config_kv::Column::Key.like(key))
            .all(db)
            .await
            .context("failed to query config_kv for set")?;
        let inherit_encrypted = existing.iter().any(|e| e.encrypted != 0);
        for row in existing {
            row.delete(db)
                .await
                .context("failed to remove superseded config_kv row")?;
        }
        let entry = config_kv::ActiveModel {
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            encrypted: Set(if encrypted || inherit_encrypted { 1 } else { 0 }),
            ..Default::default()
        };
        entry.save(db).await.context("failed to insert config_kv")?;
        Ok(())
    }

    /// Add a value for a key (allows duplicates, for multi-value keys).
    ///
    /// Enforces same-key-same-state: if existing entries for this key have a
    /// different encryption state, the insert is rejected. If existing entries
    /// are encrypted and `encrypted` is false, the encryption state is
    /// inherited (auto-promoted to encrypted).
    ///
    /// Boundary conditions:
    /// - First-write (no rows yet) is always accepted with the requested flag.
    /// - Returns `Err` when mixing plaintext and encrypted values would result.
    ///   This is a hard invariant of `config_kv`; callers cannot opt out.
    pub async fn add_with_conn<C: ConnectionTrait>(
        db: &C,
        key: &str,
        value: &str,
        encrypted: bool,
    ) -> Result<()> {
        // Check existing entries for encryption state inheritance / conflict
        let existing = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .all(db)
            .await
            .context("failed to query config_kv for add")?;

        let has_encrypted = existing.iter().any(|e| e.encrypted != 0);
        let has_plaintext = existing.iter().any(|e| e.encrypted == 0);

        // Inherit encryption from existing entries
        let effective_encrypted = encrypted || has_encrypted;

        // Reject mixed encryption states
        if !existing.is_empty()
            && ((effective_encrypted && has_plaintext) || (!effective_encrypted && has_encrypted))
        {
            return Err(anyhow!(
                "cannot mix encrypted and plaintext values for the same key"
            ));
        }

        let entry = config_kv::ActiveModel {
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            encrypted: Set(if effective_encrypted { 1 } else { 0 }),
            ..Default::default()
        };
        entry
            .save(db)
            .await
            .context("failed to add config_kv entry")?;
        Ok(())
    }

    /// Delete the first matching entry for a key.
    /// Returns the number of rows deleted (0 or 1).
    ///
    /// Boundary conditions: returns `Err` if multiple rows match — caller must
    /// use [`Self::unset_all_with_conn`] explicitly to remove every row.
    pub async fn unset_with_conn<C: ConnectionTrait>(db: &C, key: &str) -> Result<usize> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .all(db)
            .await
            .context("failed to query config_kv for unset")?;

        if rows.len() > 1 {
            return Err(anyhow!(
                "cannot unset '{}': {} values exist for this key",
                key,
                rows.len()
            ));
        }

        if let Some(row) = rows.into_iter().next() {
            row.delete(db)
                .await
                .context("failed to delete config_kv entry")?;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    /// Delete all matching entries for a key.
    /// Returns the number of rows deleted (0 if none matched).
    pub async fn unset_all_with_conn<C: ConnectionTrait>(db: &C, key: &str) -> Result<usize> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.eq(key))
            .all(db)
            .await
            .context("failed to query config_kv for unset_all")?;

        let count = rows.len();
        for row in rows {
            row.delete(db)
                .await
                .context("failed to delete config_kv entry")?;
        }
        Ok(count)
    }

    /// List all config entries, sorted by key.
    ///
    /// Useful for `libra config --list`. Encrypted values are returned as
    /// hex ciphertext; the CLI is responsible for redaction.
    pub async fn list_all_with_conn<C: ConnectionTrait>(db: &C) -> Result<Vec<ConfigKvEntry>> {
        let rows = config_kv::Entity::find()
            .order_by_asc(config_kv::Column::Key)
            .all(db)
            .await
            .context("failed to list config_kv")?;
        Ok(rows.iter().map(ConfigKvEntry::from_model).collect())
    }

    /// Get all entries whose key starts with the given prefix.
    ///
    /// Used by domain helpers (`all_remote_configs`, etc.) to scope searches
    /// without having to enumerate every section name. Empty prefix returns
    /// all rows in key order.
    pub async fn get_by_prefix_with_conn<C: ConnectionTrait>(
        db: &C,
        prefix: &str,
    ) -> Result<Vec<ConfigKvEntry>> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(prefix))
            .order_by_asc(config_kv::Column::Key)
            // Stable tie-breaker so multi-value keys keep insertion order — e.g.
            // `--rename-section` must preserve the order of duplicate values.
            .order_by_asc(config_kv::Column::Id)
            .all(db)
            .await
            .context("failed to query config_kv by prefix")?;
        Ok(rows.iter().map(ConfigKvEntry::from_model).collect())
    }

    /// Resolve a config variable whose **name** is matched case-insensitively,
    /// matching Git semantics (config variable names are case-insensitive; the
    /// subsection between dots is *not*). `prefix` is the case-sensitive
    /// `section[.subsection].` part (including the trailing dot) and `variable`
    /// is the variable name in any case; among rows whose key equals
    /// `<prefix><variable>` (variable compared ASCII-case-insensitively) the
    /// highest-`id` (most recently inserted) match is returned.
    ///
    /// In normal use a logical variable has exactly **one** row — `set` updates
    /// it in place — so the case folding is what matters: a value written under
    /// either the camelCase spelling (`pushRemote`) or the lowercase form
    /// emitted by `git config --list` / imports (`pushremote`) resolves to that
    /// single value. The only case the `id` ordering disambiguates is the config
    /// *anomaly* where two distinct case-variant rows coexist (Libra stores keys
    /// case-sensitively, so this is possible when a variable is written under two
    /// different spellings, but never when one spelling is used consistently or
    /// via Git imports); there the result is deterministic (most recently inserted
    /// spelling) but not a true cross-spelling last-write, which the `config_kv`
    /// schema (no write-order column) cannot represent.
    pub async fn get_var_case_insensitive_with_conn<C: ConnectionTrait>(
        db: &C,
        prefix: &str,
        variable: &str,
    ) -> Result<Option<ConfigKvEntry>> {
        let rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(prefix))
            // Newest first so the first case-insensitive match is the most
            // recently inserted variant (see doc note on the anomaly case).
            .order_by_desc(config_kv::Column::Id)
            .all(db)
            .await
            .context("failed to query config_kv for case-insensitive variable")?;
        Ok(rows
            .iter()
            .find(|row| {
                row.key
                    .strip_prefix(prefix)
                    .map(|var| var.eq_ignore_ascii_case(variable))
                    .unwrap_or(false)
            })
            .map(ConfigKvEntry::from_model))
    }

    /// Get all entries whose key matches a regex pattern.
    ///
    /// Boundary conditions:
    /// - Returns `Err` for invalid regex syntax.
    /// - SQLite has no native `REGEXP`, so we fetch every row and filter in
    ///   Rust. Acceptable cost given config tables are small.
    pub async fn get_regexp_with_conn<C: ConnectionTrait>(
        db: &C,
        pattern: &str,
    ) -> Result<Vec<ConfigKvEntry>> {
        // SQLite doesn't have native regex, so we fetch all and filter in Rust.
        let re = regex::Regex::new(pattern)
            .map_err(|e| anyhow!("invalid regex pattern '{}': {}", pattern, e))?;
        let rows = config_kv::Entity::find()
            .order_by_asc(config_kv::Column::Key)
            .all(db)
            .await
            .context("failed to query config_kv for regexp")?;
        Ok(rows
            .iter()
            .filter(|r| re.is_match(&r.key))
            .map(ConfigKvEntry::from_model)
            .collect())
    }

    // ── Convenience wrappers (acquire DB conn from pool) ─────────────────
    // Each of these pairs with a `*_with_conn` variant above. They acquire
    // a connection from the global pool; do not call them inside a
    // `db.transaction(|txn| { ... })` block — that deadlocks. Use the
    // `_with_conn` variant instead.

    /// Pool-acquiring counterpart of [`Self::get_with_conn`].
    pub async fn get(key: &str) -> Result<Option<ConfigKvEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_with_conn(&db, key).await
    }

    /// Non-panicking counterpart of [`Self::get`].
    ///
    /// [`Self::get`] resolves its connection through [`get_db_conn_instance`],
    /// which **panics** when the repository database is missing or its schema
    /// is out of date. That is unacceptable for best-effort / background
    /// reads — for example the SSH transport setup performed during
    /// `clone`/`fetch`, which may walk up into an *enclosing* repository whose
    /// schema this binary no longer supports. This variant resolves the
    /// database path fallibly and surfaces any open/compatibility failure as
    /// an `Err`, so callers can degrade gracefully instead of dumping a panic
    /// to stderr.
    pub async fn get_best_effort(key: &str) -> Result<Option<ConfigKvEntry>> {
        let db_path = try_get_storage_path(None)
            .map_err(|err| anyhow!("not inside a libra repository: {err}"))?
            .join(DATABASE);
        let db = get_db_conn_instance_for_path(&db_path)
            .await
            .map_err(|err| {
                anyhow!(
                    "failed to open repository database {}: {err}",
                    db_path.display()
                )
            })?;
        Self::get_with_conn(&db, key).await
    }

    /// Pool-acquiring counterpart of [`Self::get_all_with_conn`].
    pub async fn get_all(key: &str) -> Result<Vec<ConfigKvEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_all_with_conn(&db, key).await
    }

    /// Pool-acquiring counterpart of
    /// [`Self::get_var_all_case_insensitive_with_conn`].
    pub async fn get_var_all_case_insensitive(
        prefix: &str,
        variable: &str,
    ) -> Result<Vec<ConfigKvEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_var_all_case_insensitive_with_conn(&db, prefix, variable).await
    }

    /// Pool-acquiring counterpart of [`Self::set_with_conn`].
    pub async fn set(key: &str, value: &str, encrypted: bool) -> Result<()> {
        let db = get_db_conn_instance().await;
        Self::set_with_conn(&db, key, value, encrypted).await
    }

    /// Pool-acquiring counterpart of [`Self::add_with_conn`].
    pub async fn add(key: &str, value: &str, encrypted: bool) -> Result<()> {
        let db = get_db_conn_instance().await;
        Self::add_with_conn(&db, key, value, encrypted).await
    }

    /// Pool-acquiring counterpart of [`Self::unset_with_conn`].
    pub async fn unset(key: &str) -> Result<usize> {
        let db = get_db_conn_instance().await;
        Self::unset_with_conn(&db, key).await
    }

    /// Pool-acquiring counterpart of [`Self::unset_all_with_conn`].
    pub async fn unset_all(key: &str) -> Result<usize> {
        let db = get_db_conn_instance().await;
        Self::unset_all_with_conn(&db, key).await
    }

    /// Pool-acquiring counterpart of [`Self::list_all_with_conn`].
    pub async fn list_all() -> Result<Vec<ConfigKvEntry>> {
        let db = get_db_conn_instance().await;
        Self::list_all_with_conn(&db).await
    }

    /// Pool-acquiring counterpart of [`Self::get_by_prefix_with_conn`].
    pub async fn get_by_prefix(prefix: &str) -> Result<Vec<ConfigKvEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_by_prefix_with_conn(&db, prefix).await
    }

    /// Pool-acquiring counterpart of [`Self::get_var_case_insensitive_with_conn`].
    pub async fn get_var_case_insensitive(
        prefix: &str,
        variable: &str,
    ) -> Result<Option<ConfigKvEntry>> {
        let db = get_db_conn_instance().await;
        Self::get_var_case_insensitive_with_conn(&db, prefix, variable).await
    }

    // ── Type helpers ─────────────────────────────────────────────────────

    /// Get a boolean config value. Normalises `true/yes/on/1` -> `true`,
    /// `false/no/off/0` -> `false`.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the key is absent.
    /// - Returns `Err` if the value is present but does not match any of the
    ///   recognised tokens.
    /// - Encrypted values display as `<REDACTED>` in the error message so
    ///   ciphertext is not echoed back to the user.
    pub async fn get_bool_with_conn<C: ConnectionTrait>(db: &C, key: &str) -> Result<Option<bool>> {
        let entry = Self::get_with_conn(db, key).await?;
        match entry {
            None => Ok(None),
            Some(e) => {
                let v = e.value.to_ascii_lowercase();
                match v.as_str() {
                    "true" | "yes" | "on" | "1" => Ok(Some(true)),
                    "false" | "no" | "off" | "0" => Ok(Some(false)),
                    _ => Err(anyhow!(
                        "invalid value '{}' for key '{}': expected bool (true/false)",
                        if e.encrypted { "<REDACTED>" } else { &e.value },
                        key
                    )),
                }
            }
        }
    }

    /// Get an integer config value. Supports `k`/`m`/`g` suffixes.
    ///
    /// Multipliers are 1024-based (KiB/MiB/GiB) to mirror `git config --int`
    /// behaviour. Returns `Ok(None)` for missing keys, `Err` for unparseable
    /// values, with the same `<REDACTED>` policy as [`Self::get_bool_with_conn`].
    pub async fn get_int_with_conn<C: ConnectionTrait>(db: &C, key: &str) -> Result<Option<i64>> {
        let entry = Self::get_with_conn(db, key).await?;
        match entry {
            None => Ok(None),
            Some(e) => {
                let s = e.value.trim().to_ascii_lowercase();
                let (num_str, multiplier) = if s.ends_with('k') {
                    (&s[..s.len() - 1], 1024i64)
                } else if s.ends_with('m') {
                    (&s[..s.len() - 1], 1024 * 1024)
                } else if s.ends_with('g') {
                    (&s[..s.len() - 1], 1024 * 1024 * 1024)
                } else {
                    (s.as_str(), 1i64)
                };
                let n: i64 = num_str.parse().map_err(|_| {
                    anyhow!(
                        "invalid value '{}' for key '{}': expected integer",
                        if e.encrypted { "<REDACTED>" } else { &e.value },
                        key
                    )
                })?;
                Ok(Some(n * multiplier))
            }
        }
    }

    // ── Domain helpers (replace old Config methods) ──────────────────────

    /// Get the value of `remote.<remote>.url`.
    ///
    /// Returns a user-friendly `fatal:` error when the key is absent —
    /// commands like `push`/`fetch` rely on this exact message format.
    pub async fn get_remote_url_with_conn<C: ConnectionTrait>(
        db: &C,
        remote: &str,
    ) -> Result<String> {
        let key = format!("remote.{remote}.url");
        match Self::get_with_conn(db, &key).await? {
            Some(entry) => Ok(entry.value),
            None => Err(anyhow!("fatal: No URL configured for remote '{remote}'.")),
        }
    }

    /// Pool-acquiring counterpart of [`Self::get_remote_url_with_conn`].
    pub async fn get_remote_url(remote: &str) -> Result<String> {
        let db = get_db_conn_instance().await;
        Self::get_remote_url_with_conn(&db, remote).await
    }

    /// Get remote name for a branch from `branch.<branch>.remote`.
    ///
    /// Returns `Ok(None)` for branches that have no upstream configured.
    pub async fn get_remote_with_conn<C: ConnectionTrait>(
        db: &C,
        branch: &str,
    ) -> Result<Option<String>> {
        let key = format!("branch.{branch}.remote");
        Ok(Self::get_with_conn(db, &key).await?.map(|e| e.value))
    }

    /// Pool-acquiring counterpart of [`Self::get_remote_with_conn`].
    pub async fn get_remote(branch: &str) -> Result<Option<String>> {
        let db = get_db_conn_instance().await;
        Self::get_remote_with_conn(&db, branch).await
    }

    /// Get remote for the current HEAD branch.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when HEAD points to a valid branch but no upstream.
    /// - Returns `Err` when HEAD is detached, since "the current branch's
    ///   remote" is undefined in that state.
    pub async fn get_current_remote_with_conn<C: ConnectionTrait>(
        db: &C,
    ) -> Result<Option<String>> {
        match Head::current_with_conn(db).await {
            Head::Branch(name) => Self::get_remote_with_conn(db, &name).await,
            Head::Detached(_) => Err(anyhow!("fatal: HEAD is detached, cannot get remote")),
        }
    }

    /// Pool-acquiring counterpart of [`Self::get_current_remote_with_conn`].
    pub async fn get_current_remote() -> Result<Option<String>> {
        let db = get_db_conn_instance().await;
        Self::get_current_remote_with_conn(&db).await
    }

    /// Get remote URL for the current HEAD branch.
    ///
    /// Returns `Ok(None)` when no upstream is configured. Returns `Err` if
    /// the upstream is set to a remote that itself has no `url` configured
    /// — this is treated as repository corruption.
    pub async fn get_current_remote_url_with_conn<C: ConnectionTrait>(
        db: &C,
    ) -> Result<Option<String>> {
        match Self::get_current_remote_with_conn(db).await? {
            Some(remote) => Ok(Some(Self::get_remote_url_with_conn(db, &remote).await?)),
            None => Ok(None),
        }
    }

    /// Pool-acquiring counterpart of [`Self::get_current_remote_url_with_conn`].
    pub async fn get_current_remote_url() -> Result<Option<String>> {
        let db = get_db_conn_instance().await;
        Self::get_current_remote_url_with_conn(&db).await
    }

    /// Enumerate every configured remote and its URL.
    ///
    /// Discovery rule: walks rows under the `remote.` prefix, treating any
    /// key of the form `remote.<name>.url` as a remote definition. Other keys
    /// (`fetch`, `push`, etc.) are ignored here. Returns each remote at most
    /// once, preserving discovery order.
    pub async fn all_remote_configs_with_conn<C: ConnectionTrait>(
        db: &C,
    ) -> Result<Vec<RemoteConfig>> {
        let entries = Self::get_by_prefix_with_conn(db, "remote.").await?;
        let mut remote_names: Vec<String> = Vec::new();
        for e in &entries {
            // Parse "remote.<name>.url" to extract <name>
            if let Some(rest) = e.key.strip_prefix("remote.")
                && let Some((name, suffix)) = rest.rsplit_once('.')
                && suffix == "url"
                && !remote_names.contains(&name.to_string())
            {
                remote_names.push(name.to_string());
            }
        }
        let mut configs = Vec::new();
        for name in remote_names {
            let url_key = format!("remote.{name}.url");
            if let Some(entry) = entries.iter().find(|e| e.key == url_key) {
                configs.push(RemoteConfig {
                    name: name.clone(),
                    url: entry.value.clone(),
                });
            }
        }
        Ok(configs)
    }

    /// Pool-acquiring counterpart of [`Self::all_remote_configs_with_conn`].
    pub async fn all_remote_configs() -> Result<Vec<RemoteConfig>> {
        let db = get_db_conn_instance().await;
        Self::all_remote_configs_with_conn(&db).await
    }

    /// Get a specific remote's config (`Ok(None)` when no `remote.<name>.url`).
    pub async fn remote_config_with_conn<C: ConnectionTrait>(
        db: &C,
        name: &str,
    ) -> Result<Option<RemoteConfig>> {
        let url_key = format!("remote.{name}.url");
        match Self::get_with_conn(db, &url_key).await? {
            Some(entry) => Ok(Some(RemoteConfig {
                name: name.to_owned(),
                url: entry.value,
            })),
            None => Ok(None),
        }
    }

    /// Pool-acquiring counterpart of [`Self::remote_config_with_conn`].
    pub async fn remote_config(name: &str) -> Result<Option<RemoteConfig>> {
        let db = get_db_conn_instance().await;
        Self::remote_config_with_conn(&db, name).await
    }

    /// Get branch tracking configuration (the upstream remote and merge ref).
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when either `branch.<name>.remote` or
    ///   `branch.<name>.merge` is missing. Both must be set together for
    ///   tracking to be valid.
    /// - The returned `merge` field has `refs/heads/` stripped if present so
    ///   callers can compare it directly against short branch names.
    pub async fn branch_config_with_conn<C: ConnectionTrait>(
        db: &C,
        name: &str,
    ) -> Result<Option<BranchConfig>> {
        let remote_key = format!("branch.{name}.remote");
        let merge_key = format!("branch.{name}.merge");
        let remote = Self::get_with_conn(db, &remote_key).await?;
        let merge = Self::get_with_conn(db, &merge_key).await?;
        match (remote, merge) {
            (Some(r), Some(m)) => {
                let mut merge_val = m.value;
                // Strip refs/heads/ prefix if present
                if let Some(stripped) = merge_val.strip_prefix("refs/heads/") {
                    merge_val = stripped.to_string();
                }
                Ok(Some(BranchConfig {
                    name: name.to_owned(),
                    merge: merge_val,
                    remote: r.value,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Pool-acquiring counterpart of [`Self::branch_config_with_conn`].
    pub async fn branch_config(name: &str) -> Result<Option<BranchConfig>> {
        let db = get_db_conn_instance().await;
        Self::branch_config_with_conn(&db, name).await
    }

    /// Remove all config entries for a remote, including its SSH credentials.
    ///
    /// Cascading deletes:
    /// 1. Every `remote.<name>.*` row.
    /// 2. Every `vault.ssh.<name>.*` row (private keys, host fingerprints).
    ///
    /// Boundary condition: returns `Err("fatal: No such remote ...")` when the
    /// `remote.<name>.*` namespace is empty. The SSH cleanup never errors on
    /// its own — orphan vault rows are tolerated.
    pub async fn remove_remote_with_conn<C: ConnectionTrait>(db: &C, name: &str) -> Result<()> {
        let prefix = format!("remote.{name}.");
        let entries = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(&prefix))
            .all(db)
            .await
            .context("failed to query remote entries for removal")?;

        if entries.is_empty() {
            return Err(anyhow!("fatal: No such remote: {name}"));
        }

        for entry in entries {
            entry
                .delete(db)
                .await
                .context("failed to delete remote entry")?;
        }

        // Also clean up SSH keys for this remote
        let ssh_prefix = format!("vault.ssh.{name}.");
        let ssh_entries = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(&ssh_prefix))
            .all(db)
            .await
            .context("failed to query SSH key entries for removal")?;
        for entry in ssh_entries {
            entry
                .delete(db)
                .await
                .context("failed to delete SSH key entry")?;
        }

        Ok(())
    }

    /// Pool-acquiring counterpart of [`Self::remove_remote_with_conn`].
    pub async fn remove_remote(name: &str) -> Result<()> {
        let db = get_db_conn_instance().await;
        Self::remove_remote_with_conn(&db, name).await
    }

    /// Rename a remote, updating all related config entries atomically.
    ///
    /// Performs three cascading rewrites:
    /// 1. `remote.<old>.*` keys are renamed to `remote.<new>.*`.
    ///    Fetch refspec destinations under `refs/remotes/<old>/` are rewritten
    ///    to the new tracking namespace at the same time.
    /// 2. Any `branch.*.remote = <old>` value is updated to `<new>`.
    /// 3. `vault.ssh.<old>.*` SSH key namespace is renamed to
    ///    `vault.ssh.<new>.*` so credentials follow the rename.
    ///
    /// Boundary conditions:
    /// - Returns `Err` if `<old>` does not exist or `<new>` already exists,
    ///   matching git's "fatal: ..." error format.
    /// - This function is *not* atomic across rewrites. Wrap in a sea-orm
    ///   transaction (and call this `_with_conn` variant with `txn`) when
    ///   atomicity matters.
    pub async fn rename_remote_with_conn<C: ConnectionTrait>(
        db: &C,
        old: &str,
        new: &str,
    ) -> Result<()> {
        // Validate the complete namespaces, not only `.url`: a push-only
        // remote is still renameable, and any target-side key must block the
        // rename instead of being silently merged into the new section.
        let old_prefix = format!("remote.{old}.");
        let new_prefix = format!("remote.{new}.");
        let entries = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(&old_prefix))
            .all(db)
            .await
            .context("failed to query source remote entries for rename")?
            .into_iter()
            .filter(|entry| remote_namespace_variable(&entry.key, old).is_some())
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Err(anyhow!("fatal: No such remote: {old}"));
        }
        let target_entries = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(&new_prefix))
            .all(db)
            .await
            .context("failed to query target remote entries for rename")?
            .into_iter()
            .filter(|entry| remote_namespace_variable(&entry.key, new).is_some())
            .collect::<Vec<_>>();
        if !target_entries.is_empty() {
            return Err(anyhow!("fatal: remote {new} already exists."));
        }
        let ssh_old_prefix = format!("vault.ssh.{old}.");
        let ssh_new_prefix = format!("vault.ssh.{new}.");
        let existing_target_ssh_entries = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(&ssh_new_prefix))
            .all(db)
            .await
            .context("failed to query target SSH key entries for rename")?
            .into_iter()
            .filter(|entry| ssh_remote_namespace_variable(&entry.key, new).is_some())
            .collect::<Vec<_>>();
        if !existing_target_ssh_entries.is_empty() {
            return Err(anyhow!(
                "fatal: SSH key namespace for remote '{new}' already exists"
            ));
        }

        // Rename remote.old.* → remote.new.*
        for entry in entries {
            let new_key = entry.key.replacen(&old_prefix, &new_prefix, 1);
            let new_value = if remote_namespace_variable(&entry.key, old)
                .is_some_and(|variable| variable.eq_ignore_ascii_case("fetch"))
            {
                rewrite_fetch_refspec_destination(&entry.value, old, new)
            } else {
                entry.value.clone()
            };
            let mut active: config_kv::ActiveModel = entry.into();
            active.key = Set(new_key);
            active.value = Set(new_value);
            active
                .update(db)
                .await
                .context("failed to rename remote entry")?;
        }

        // Update branch.*.remote and branch.*.pushRemote values that reference
        // the old name (Git rewrites both; the pushRemote suffix is matched
        // case-insensitively because `git config --list` lowercases it).
        let branch_entries = Self::get_by_prefix_with_conn(db, "branch.").await?;
        for be in branch_entries {
            let is_upstream = be.key.ends_with(".remote");
            let is_push_remote = be
                .key
                .strip_prefix("branch.")
                .and_then(|k| k.rsplit_once('.'))
                .map(|(_, var)| var.eq_ignore_ascii_case("pushRemote"))
                .unwrap_or(false);
            if (is_upstream || is_push_remote) && be.value == old {
                let rows = config_kv::Entity::find()
                    .filter(config_kv::Column::Key.eq(&be.key))
                    .filter(config_kv::Column::Value.eq(old))
                    .all(db)
                    .await
                    .context("failed to query branch remote entries")?;
                for row in rows {
                    let mut active: config_kv::ActiveModel = row.into();
                    active.value = Set(new.to_owned());
                    active
                        .update(db)
                        .await
                        .context("failed to update branch remote")?;
                }
            }
        }

        // Rewrite a repository-scoped `remote.pushDefault` that names the old
        // remote (Git rewrites local-scope pushDefault; global/system are left
        // for the caller to warn about).
        let push_default_rows = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with("remote."))
            .all(db)
            .await
            .context("failed to query pushDefault entries for rename")?
            .into_iter()
            .filter(|e| {
                e.key
                    .strip_prefix("remote.")
                    .map(|var| var.eq_ignore_ascii_case("pushDefault"))
                    .unwrap_or(false)
                    && e.value == old
            })
            .collect::<Vec<_>>();
        for row in push_default_rows {
            let mut active: config_kv::ActiveModel = row.into();
            active.value = Set(new.to_owned());
            active
                .update(db)
                .await
                .context("failed to update remote.pushDefault")?;
        }

        // Cascade SSH key rename: vault.ssh.old.* → vault.ssh.new.*
        let ssh_entries = config_kv::Entity::find()
            .filter(config_kv::Column::Key.starts_with(&ssh_old_prefix))
            .all(db)
            .await
            .context("failed to query SSH key entries for rename")?
            .into_iter()
            .filter(|entry| ssh_remote_namespace_variable(&entry.key, old).is_some())
            .collect::<Vec<_>>();
        for entry in ssh_entries {
            let new_key = entry.key.replacen(&ssh_old_prefix, &ssh_new_prefix, 1);
            let mut active: config_kv::ActiveModel = entry.into();
            active.key = Set(new_key);
            active
                .update(db)
                .await
                .context("failed to rename SSH key entry")?;
        }

        Ok(())
    }

    /// Pool-acquiring counterpart of [`Self::rename_remote_with_conn`].
    pub async fn rename_remote(old: &str, new: &str) -> Result<()> {
        let db = get_db_conn_instance().await;
        Self::rename_remote_with_conn(&db, old, new).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Environment variable resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Decrypt a hex-encoded ciphertext using the vault unseal key for the given scope.
///
/// `scope` should be `"local"` (current repo's `.libra/libra.db`) or `"global"`
/// (`~/.libra/config.db`). Returns `Err` if the vault for that scope is sealed
/// or the ciphertext is malformed.
pub async fn decrypt_value(hex_ciphertext: &str, scope: &str) -> Result<String> {
    let unseal_key = load_unseal_key_for_scope(scope)
        .await
        .ok_or_else(|| anyhow!("vault not initialized for {scope} scope — cannot decrypt value"))?;
    decrypt_value_with_unseal_key(hex_ciphertext, &unseal_key)
}

/// Decrypt a value using the unseal key tied to a specific local target.
///
/// Used when the resolution chain points at a non-default repository (for
/// example when `libra config --file path/to/db get`). Returns `Err` if the
/// requested vault is sealed or has no unseal key.
async fn decrypt_value_for_local_target(
    hex_ciphertext: &str,
    local_target: LocalIdentityTarget<'_>,
) -> Result<String> {
    let unseal_key = match local_target {
        LocalIdentityTarget::CurrentRepo => {
            crate::internal::vault::load_unseal_key_for_scope("local").await
        }
        LocalIdentityTarget::ExplicitDb(db_path) => {
            crate::internal::vault::load_unseal_key_for_db_path(db_path).await
        }
        LocalIdentityTarget::None => None,
    }
    .ok_or_else(|| anyhow!("vault not initialized for local scope — cannot decrypt value"))?;

    decrypt_value_with_unseal_key(hex_ciphertext, &unseal_key)
}

/// Hex-decode `hex_ciphertext` and pass the bytes to [`decrypt_token`].
///
/// Centralised here so that scope-aware decrypt paths share the same hex
/// parsing and error wrapping.
fn decrypt_value_with_unseal_key(hex_ciphertext: &str, unseal_key: &[u8]) -> Result<String> {
    let ciphertext =
        hex::decode(hex_ciphertext).context("failed to decode encrypted config value hex")?;
    decrypt_token(unseal_key, &ciphertext)
}

/// Encrypt a value using the vault unseal key for the given scope.
/// Returns the hex-encoded ciphertext.
///
/// Used by `libra config set`/`add` when the key is sensitive
/// (see [`is_sensitive_key`]) or `--encrypted` was passed.
pub async fn encrypt_value(value: &str, scope: &str) -> Result<String> {
    let unseal_key = load_unseal_key_for_scope(scope)
        .await
        .ok_or_else(|| anyhow!("vault not initialized for {scope} scope — cannot encrypt value"))?;
    let ciphertext = encrypt_token(&unseal_key, value.as_bytes())?;
    Ok(hex::encode(ciphertext))
}

/// Resolve an environment variable by priority chain.
///
/// Functional scope:
/// 1. System environment variable (`std::env::var`)
/// 2. Local config (`vault.env.<name>` in `.libra/libra.db`)
/// 3. Global config (`vault.env.<name>` in `~/.libra/config.db`)
///
/// Boundary conditions:
/// - `name` is the raw env var name (e.g. `"GEMINI_API_KEY"`).
/// - Returns `Ok(None)` only when *all three* sources are exhausted.
/// - Returns `Err` if a vault/DB query fails (a hard error — not the same
///   as "not configured").
pub async fn resolve_env(name: &str) -> Result<Option<String>> {
    resolve_env_for_target(name, LocalIdentityTarget::CurrentRepo).await
}

/// Synchronous wrapper around [`resolve_env`] for call sites that cannot become
/// async (e.g. sync constructors inside otherwise-async pipelines, or
/// closures threaded through `Fn(&str) -> Option<String>` lookup helpers).
///
/// Functional scope:
/// - Checks `std::env::var(name)` first — the common fast path that does not
///   need a tokio runtime.
/// - When the env var is unset, spawns a private std-thread that owns a
///   single-purpose tokio runtime, drives the async [`resolve_env_for_target`]
///   call against [`LocalIdentityTarget::CurrentRepo`], and returns the
///   resolved value to the caller. This mirrors the pattern in
///   `src/utils/client_storage.rs::resolve_env_sync` and is intentionally
///   isolated from any caller-owned tokio runtime.
///
/// Returns `Ok(None)` only when the process env, the local repo's
/// `.libra/libra.db`, and the global `~/.libra/config.db` all lack the value.
/// Returns `Err` when the worker thread crashed before sending OR when the
/// underlying async resolver returned an error (e.g. corrupt SQLite, or a
/// schema *newer* than this binary supports — pending migrations are now
/// applied automatically on connect, but an unsupported-future schema still
/// bubbles up here so storage / provider init paths can surface an
/// "install a newer Libra" hint rather than silently treating a
/// vault-configured key as missing).
///
/// Prefer the async [`resolve_env`] when the caller is already inside an
/// async context — that avoids the per-call thread spawn.
pub fn resolve_env_sync(name: &str) -> anyhow::Result<Option<String>> {
    if let Ok(val) = std::env::var(name)
        && !val.trim().is_empty()
    {
        return Ok(Some(val));
    }

    let owned = name.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Option<String>> {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|err| anyhow::anyhow!("failed to create tokio runtime: {err}"))?;
            runtime.block_on(resolve_env_for_target(
                &owned,
                LocalIdentityTarget::CurrentRepo,
            ))
        })();
        let _ = tx.send(result);
    });
    rx.recv()
        .map_err(|_| anyhow::anyhow!("resolve_env_sync worker for '{name}' exited unexpectedly"))?
}

/// Required-value wrapper over [`resolve_env_sync`]: returns `Ok(value)`
/// when the variable is set in the process env, the local repo's
/// `.libra/libra.db`, or the global `~/.libra/config.db`, and a single
/// actionable error otherwise. Provider clients use this for the
/// API-key class of variables where missing means the provider cannot
/// initialise.
/// [`resolve_env_sync`] with an explicit repository directory for the
/// repo-local layer (plan-20260825 PS-06 terra R2): the session target's
/// vault (`--repo` / `--cwd`) participates instead of whatever repository
/// the process cwd happens to be inside. Falls back to the global/process
/// layers when the directory holds no repository.
pub fn resolve_env_sync_for_dir(
    name: &str,
    dir: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    if let Ok(val) = std::env::var(name)
        && !val.trim().is_empty()
    {
        return Ok(Some(val));
    }
    let local_db = crate::utils::util::try_get_storage_path(Some(dir.to_path_buf()))
        .ok()
        .map(|storage| storage.join(crate::utils::util::DATABASE));
    let owned = name.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Option<String>> {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|err| anyhow::anyhow!("failed to create tokio runtime: {err}"))?;
            let target = match &local_db {
                Some(db) => LocalIdentityTarget::ExplicitDb(db),
                None => LocalIdentityTarget::None,
            };
            runtime.block_on(resolve_env_for_target(&owned, target))
        })();
        let _ = tx.send(result);
    });
    rx.recv().map_err(|_| {
        anyhow::anyhow!("resolve_env_sync_for_dir worker for '{name}' exited unexpectedly")
    })?
}

pub fn resolve_required_env_sync(name: &str) -> anyhow::Result<String> {
    match resolve_env_sync(name)? {
        Some(value) => Ok(value),
        None => Err(anyhow::anyhow!(
            "environment variable `{name}` is not set — export it or store it in libra config (`libra config set vault.env.{name} <value>`)"
        )),
    }
}

/// Optional-value wrapper over [`resolve_env_sync`]. Identical to
/// [`resolve_env_sync`]; provided as a named alias so callers can
/// document at the call site that the variable is optional and
/// `Ok(None)` is the success path.
pub fn resolve_optional_env_sync(name: &str) -> anyhow::Result<Option<String>> {
    resolve_env_sync(name)
}

/// Resolve an environment variable using an explicit local config target.
///
/// Same priority chain as [`resolve_env`] but lets callers point at a
/// non-default repo (e.g. when running `libra config --file ...`). The local
/// scope can also be skipped entirely with [`LocalIdentityTarget::None`].
pub async fn resolve_env_for_target(
    name: &str,
    local_target: LocalIdentityTarget<'_>,
) -> Result<Option<String>> {
    Ok(locate_env_for_target(name, local_target)
        .await?
        .map(|(value, _layer)| value))
}

/// Which layer of the credential chain produced a value
/// (plan-20260825 PS-06). Layer names are user-facing (auto-selection
/// notes and ambiguity listings) and never carry the value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnvHitLayer {
    ProcessEnvironment,
    RepoLocalVault,
    GlobalVault,
}

impl EnvHitLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvHitLayer::ProcessEnvironment => "process environment",
            EnvHitLayer::RepoLocalVault => "repo-local vault",
            EnvHitLayer::GlobalVault => "global vault",
        }
    }
}

/// [`resolve_env_for_target`] with the hit layer attached — the priority
/// chain is identical (process env → repo-local vault → global vault) and
/// the resolver delegates here, so the two can never drift.
pub async fn locate_env_for_target(
    name: &str,
    local_target: LocalIdentityTarget<'_>,
) -> Result<Option<(String, EnvHitLayer)>> {
    // An empty value can never authenticate: every source treats it as a
    // MISS and falls through to the next layer (plan-20260825 PS-06 terra
    // R5) — filtering only the final result would hide a usable key in a
    // lower layer behind an empty upper one.
    // 1. System environment variable — per-process override (12-Factor)
    if let Ok(val) = std::env::var(name)
        && !val.trim().is_empty()
    {
        return Ok(Some((val, EnvHitLayer::ProcessEnvironment)));
    }

    let vault_key = format!("vault.env.{name}");

    // 2. Local config (vault.env.*)
    if let Some(value) = local_env_value_for_target(local_target, &vault_key).await?
        && !value.trim().is_empty()
    {
        return Ok(Some((value, EnvHitLayer::RepoLocalVault)));
    }

    // 3. Global config — lowest priority
    Ok(global_env_value(name, &vault_key)
        .await?
        .filter(|value| !value.trim().is_empty())
        .map(|value| (value, EnvHitLayer::GlobalVault)))
}

/// Where the active global configuration database came from.
///
/// The string forms are part of the `libra config path --json` and
/// `libra config doctor --global-schema` contracts (ADR-GCX-06); do not rename
/// an existing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalConfigPathSource {
    /// `LIBRA_CONFIG_GLOBAL_DB` was set (tests/sandboxes; never migrated).
    EnvOverride,
    /// An absolute `XDG_CONFIG_HOME` was used.
    XdgConfigHome,
    /// `<home>/.config/libra` was used.
    HomeConfig,
    /// The legacy `<home>/.libra/config.db` is still active (migration pending).
    LegacyHome,
}

impl GlobalConfigPathSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnvOverride => "LIBRA_CONFIG_GLOBAL_DB",
            Self::XdgConfigHome => "xdg",
            Self::HomeConfig => "home",
            Self::LegacyHome => "legacy",
        }
    }
}

/// Fully resolved global configuration state.
///
/// `path` is the active database; `new_path` is where the XDG-based layout
/// expects it. When `migration_pending` is true the active path is the legacy
/// `~/.libra/config.db` and a future release migrates it automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfigResolution {
    pub path: std::path::PathBuf,
    pub source: GlobalConfigPathSource,
    pub new_path: Option<std::path::PathBuf>,
    pub new_exists: bool,
    pub legacy_path: Option<std::path::PathBuf>,
    pub legacy_exists: bool,
    pub migration_pending: bool,
}

/// The platform-consistent global configuration directory (`<config>/libra`).
///
/// Rule (ADR-GCX-01): an absolute `XDG_CONFIG_HOME` wins; otherwise
/// `<home>/.config/libra` on every platform (Linux, macOS and Windows alike).
/// A relative `XDG_CONFIG_HOME` is ignored per the XDG basedir spec.
fn global_config_dir_with_source() -> Option<(std::path::PathBuf, GlobalConfigPathSource)> {
    config_dir_from(
        dirs::home_dir(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
    )
}

/// Pure form of [`global_config_dir_with_source`] so the no-home branch is
/// unit-testable without depending on the process passwd database.
fn config_dir_from(
    home: Option<std::path::PathBuf>,
    xdg_config_home: Option<&std::ffi::OsStr>,
) -> Option<(std::path::PathBuf, GlobalConfigPathSource)> {
    if let Some(raw) = xdg_config_home {
        let path = std::path::PathBuf::from(raw);
        if path.is_absolute() && !path.as_os_str().is_empty() {
            return Some((path.join("libra"), GlobalConfigPathSource::XdgConfigHome));
        }
        tracing::debug!(
            value = %path.display(),
            "ignoring relative XDG_CONFIG_HOME for the global config directory"
        );
    }
    home.map(|home| {
        (
            home.join(".config").join("libra"),
            GlobalConfigPathSource::HomeConfig,
        )
    })
}

/// The XDG-based global configuration directory, independent of legacy state.
pub fn global_config_dir() -> Option<std::path::PathBuf> {
    global_config_dir_with_source().map(|(dir, _)| dir)
}

/// The legacy per-home database (`<home>/.libra/config.db`).
pub fn legacy_global_config_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(".libra").join("config.db"))
}

/// Resolve the global config database plus its provenance.
///
/// Boundary conditions (ADR-GCX-01):
/// - A non-empty `LIBRA_CONFIG_GLOBAL_DB` wins verbatim and disables any legacy
///   fallback (integration tests / sandboxes rely on this).
/// - Otherwise the XDG-based path is active when it exists, or when no legacy
///   database exists yet.
/// - A new-layout path that is missing while `<home>/.libra/config.db` exists
///   falls back to the legacy file (read and write stay on one file) and flags
///   `migration_pending`.
/// - Returns `None` when no config directory can be discovered.
pub fn global_config_resolution() -> Option<GlobalConfigResolution> {
    let legacy_path = legacy_global_config_path();
    let legacy_exists = legacy_path.as_deref().is_some_and(std::path::Path::exists);

    if let Some(raw) = std::env::var_os("LIBRA_CONFIG_GLOBAL_DB")
        && !raw.is_empty()
    {
        let path = std::path::PathBuf::from(raw);
        let new_path = global_config_dir().map(|dir| dir.join("config.db"));
        let new_exists = new_path.as_deref().is_some_and(std::path::Path::exists);
        return Some(GlobalConfigResolution {
            path,
            source: GlobalConfigPathSource::EnvOverride,
            new_path,
            new_exists,
            legacy_path,
            legacy_exists,
            migration_pending: false,
        });
    }

    let (dir, dir_source) = global_config_dir_with_source()?;
    let new_path = dir.join("config.db");
    let new_exists = new_path.exists();

    if !new_exists && legacy_exists {
        let path = legacy_path.clone()?;
        return Some(GlobalConfigResolution {
            path,
            source: GlobalConfigPathSource::LegacyHome,
            new_path: Some(new_path),
            new_exists,
            legacy_path,
            legacy_exists,
            migration_pending: true,
        });
    }

    Some(GlobalConfigResolution {
        path: new_path.clone(),
        source: dir_source,
        new_path: Some(new_path),
        new_exists,
        legacy_path,
        legacy_exists,
        migration_pending: false,
    })
}

/// One-line actionable notice for the legacy global config layout, or `None`
/// when the active layout is already the XDG one.
pub fn legacy_global_config_notice(resolution: &GlobalConfigResolution) -> Option<String> {
    if !resolution.migration_pending {
        return None;
    }
    let legacy = resolution.path.display();
    let new = resolution
        .new_path
        .as_deref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<config dir>/libra/config.db".to_string());
    Some(format!(
        "global configuration is still at '{legacy}'; the next command that reads or writes it \
         moves it to '{new}' automatically (the legacy file is kept as a downgrade backup)"
    ))
}

/// Resolve the active global config database path (see [`global_config_resolution`]).
pub(crate) fn global_config_path() -> Option<std::path::PathBuf> {
    global_config_resolution().map(|resolution| resolution.path)
}

/// Why the global configuration database is being resolved.
///
/// The distinction only matters when the one-time legacy migration (GCX-02)
/// fails: a read may continue against the untouched legacy file, while a write
/// must fail closed so the two layouts can never diverge (ADR-GCX-02 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalConfigAccess {
    Read,
    Write,
}

/// The first migration failure seen in this process.
///
/// A migration that failed once has failed for the rest of the process: the
/// configuration directory does not become writable mid-command. Remembering
/// the reason keeps every later access cheap — retrying could otherwise wait
/// on the migration lock again on every single cascaded read — and keeps the
/// advice to one warning instead of one per config value.
static MIGRATION_FAILURE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Resolve the global configuration database for an actual access, running the
/// one-time legacy migration first when it is pending.
///
/// Boundary conditions (ADR-GCX-02):
/// - `LIBRA_CONFIG_GLOBAL_DB` never migrates — the resolution is already final.
/// - A successful (or concurrently completed) migration re-resolves, so the
///   caller receives the new XDG path and never the legacy one.
/// - A failed migration leaves the legacy database authoritative: [`Read`]
///   warns once and continues, [`Write`] returns an actionable error.
///
/// [`Read`]: GlobalConfigAccess::Read
/// [`Write`]: GlobalConfigAccess::Write
pub(crate) async fn resolve_global_config_for_access(
    access: GlobalConfigAccess,
) -> Result<Option<GlobalConfigResolution>, String> {
    let Some(resolution) = global_config_resolution() else {
        return Ok(None);
    };
    if !resolution.migration_pending {
        return Ok(Some(resolution));
    }
    let (Some(legacy), Some(target)) = (
        resolution.legacy_path.as_deref(),
        resolution.new_path.as_deref(),
    ) else {
        return Ok(Some(resolution));
    };

    use global_migration::GlobalConfigMigration;
    let reason = match MIGRATION_FAILURE.get() {
        Some(reason) => reason.clone(),
        None => match global_migration::migrate_legacy_global_config(legacy, target).await {
            GlobalConfigMigration::Migrated | GlobalConfigMigration::AlreadyPresent => {
                // Re-resolve rather than patching the struct: the freshly
                // published file decides the source (`xdg` or `home`) and
                // clears the pending flag through the one resolver
                // (GC-GCX-01).
                return Ok(global_config_resolution().or(Some(resolution)));
            }
            GlobalConfigMigration::NotNeeded => return Ok(Some(resolution)),
            GlobalConfigMigration::Failed(reason) => {
                // The first failure owns the warning; `set` returning `Ok`
                // is exactly "this call is the first one".
                if MIGRATION_FAILURE.set(reason.clone()).is_ok() {
                    crate::utils::error::emit_warning(format!(
                        "could not move the global configuration to '{}': {reason}; still reading \
                         '{}', which is unchanged. Fix the configuration directory, or set \
                         LIBRA_CONFIG_GLOBAL_DB to choose a database explicitly",
                        target.display(),
                        legacy.display()
                    ));
                }
                reason
            }
        },
    };

    match access {
        // The legacy database is still authoritative and still readable, so a
        // read is correct — it simply happens on the old file for now.
        GlobalConfigAccess::Read => Ok(Some(resolution)),
        GlobalConfigAccess::Write => Err(format!(
            "{} '{}': {reason}; refusing to write so '{}' stays the single source of truth. \
             Fix the configuration directory, or set LIBRA_CONFIG_GLOBAL_DB to choose a \
             database explicitly",
            crate::utils::error::GLOBAL_CONFIG_MIGRATION_FAILURE_MARKER,
            target.display(),
            legacy.display()
        )),
    }
}

/// Active global config path for a read, migrating the legacy database first.
pub(crate) async fn global_config_path_for_read() -> Option<std::path::PathBuf> {
    match resolve_global_config_for_access(GlobalConfigAccess::Read).await {
        Ok(resolution) => resolution.map(|resolution| resolution.path),
        // `Read` never fails closed; keep the cascade alive on the active path.
        Err(_) => global_config_path(),
    }
}

/// Active global config path for a write, failing closed on a failed migration.
pub(crate) async fn global_config_path_for_write() -> Result<Option<std::path::PathBuf>, String> {
    Ok(resolve_global_config_for_access(GlobalConfigAccess::Write)
        .await?
        .map(|resolution| resolution.path))
}

fn system_config_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("LIBRA_CONFIG_SYSTEM_DB") {
        return Some(std::path::PathBuf::from(path));
    }
    Some(std::path::PathBuf::from("/etc/libra/config.db"))
}

/// Identity sources resolved for commands that need name/email defaults.
///
/// `config_*` contains the cascaded local/global result for each field, while
/// `env_*` preserves the environment fallback separately so callers like
/// `commit` can still enforce `user.useConfigOnly`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserIdentitySources {
    /// `user.name` from local-then-global config (encrypted values are
    /// transparently decrypted before populating this field).
    pub config_name: Option<String>,
    /// `user.email` from local-then-global config.
    pub config_email: Option<String>,
    /// First non-empty value from the env var list (`GIT_COMMITTER_NAME`,
    /// `GIT_AUTHOR_NAME`, `LIBRA_COMMITTER_NAME`).
    pub env_name: Option<String>,
    /// First non-empty value from the env var list (`GIT_COMMITTER_EMAIL`,
    /// `GIT_AUTHOR_EMAIL`, `EMAIL`, `LIBRA_COMMITTER_EMAIL`).
    pub env_email: Option<String>,
}

/// Which local repository, if any, should participate in config resolution.
///
/// Used as a parameter to [`resolve_env_for_target`] and friends so callers
/// can bypass the implicit "discover from cwd" lookup when needed (tests,
/// `--file path` flags).
#[derive(Debug, Clone, Copy)]
pub enum LocalIdentityTarget<'a> {
    /// Read local config from the current repository discovered from cwd.
    CurrentRepo,
    /// Read local config from an explicit repository database path.
    ExplicitDb(&'a Path),
    /// Skip local scope entirely and only read global/env values.
    None,
}

/// Return the first non-empty environment variable value from `keys`.
///
/// Whitespace-only values are treated as empty so users can clear an env
/// var by setting it to a single space.
pub fn env_first_non_empty(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Read a config value for the given target using local-first, then global.
///
/// Encrypted values are transparently decrypted via the appropriate vault.
/// Returns `Ok(None)` when both local and global are absent or empty.
pub async fn read_cascaded_config_value(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<String>> {
    if let Some(value) = local_config_value_for_target(local_target, key).await? {
        return Ok(Some(value));
    }
    global_config_value(key).await
}

/// Resolve `core.fileMode` (plan issues/470 FM-03, ADR-FM-04).
///
/// Unix defaults to `true`, every other platform to `false`. The key is read
/// case-insensitively (`core.fileMode` and `core.filemode`), and an invalid
/// value fails closed with the same `CliError::command_usage` mapping as
/// `commit.verbose`: `bad boolean config value '<value>' for 'core.filemode'`.
pub async fn core_file_mode() -> crate::utils::error::CliResult<bool> {
    use crate::utils::error::{CliError, StableErrorCode};

    let mut raw = None;
    for key in ["core.fileMode", "core.filemode"] {
        match read_cascaded_config_value(LocalIdentityTarget::CurrentRepo, key).await {
            Ok(Some(value)) => {
                raw = Some(value);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                return Err(
                    CliError::fatal(format!("failed to read core.fileMode: {error}"))
                        .with_stable_code(StableErrorCode::RepoStateInvalid),
                );
            }
        }
    }
    resolve_core_file_mode(raw.as_deref()).map_err(CliError::command_usage)
}

/// Pure core of [`core_file_mode`]: apply Git's boolean parsing to the raw
/// value, defaulting to `true` on Unix and `false` elsewhere. Kept separate so
/// the default/case/invalid cases are unit-testable without a repository.
pub fn resolve_core_file_mode(raw: Option<&str>) -> Result<bool, String> {
    match raw {
        Some(value) => parse_git_bool(value)
            .ok_or_else(|| format!("bad boolean config value '{value}' for 'core.filemode'")),
        None => Ok(cfg!(unix)),
    }
}

/// Parse a Git-compatible boolean config value (`git_config_bool` semantics):
/// `true`/`yes`/`on` (case-insensitive) and any non-zero integer — with an
/// optional `k`/`m`/`g` unit suffix, as Git's int parser accepts — are true;
/// `false`/`no`/`off` and `0` (or `0k` …) are false. Returns `None` for
/// anything else, INCLUDING the empty string: the strict-cascade config
/// family (plan-20260708 P1-05) deliberately rejects present-but-empty
/// values instead of adopting Git's implicit-bool reading of them.
pub fn parse_git_config_bool(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "true" | "yes" | "on" => return Some(true),
        "false" | "no" | "off" => return Some(false),
        _ => {}
    }
    parse_git_config_int(&normalized).map(|number| number != 0)
}

/// Parse a Git-compatible integer config value: an optional sign, digits, and
/// an optional `k`/`m`/`g` unit suffix (×1024 steps). `None` on anything else
/// or on overflow. Expects pre-trimmed, pre-lowercased input.
pub(crate) fn parse_git_config_int(value: &str) -> Option<i64> {
    let (digits, multiplier) = match value.as_bytes().last()? {
        b'k' => (&value[..value.len() - 1], 1024i64),
        b'm' => (&value[..value.len() - 1], 1024i64 * 1024),
        b'g' => (&value[..value.len() - 1], 1024i64 * 1024 * 1024),
        _ => (value, 1),
    };
    digits.parse::<i64>().ok()?.checked_mul(multiplier)
}

/// Read a Git-compatible default value across all config scopes.
///
/// Unlike [`read_cascaded_config_value`], this helper preserves a present empty
/// value so callers can reject it as invalid, decrypts encrypted local/global
/// values, includes the system scope, matches section and variable names
/// case-insensitively (while preserving subsection case), and falls back to the
/// legacy `config` table. System-scope read failures are intentionally skipped,
/// matching the system-config contract documented by `libra config`.
pub async fn read_cascaded_config_value_strict(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<String>> {
    if let Some(entry) = local_config_entry_for_target_case_insensitive(local_target, key).await? {
        return Ok(Some(
            decrypt_strict_config_entry(entry, StrictConfigScope::Local(local_target)).await?,
        ));
    }

    if let Some(path) = global_config_path_for_read().await {
        match read_config_entry_from_db_path_case_insensitive(
            &path,
            key,
            DatabaseRole::GlobalConfig,
        )
        .await
        {
            Ok(Some(entry)) => {
                return Ok(Some(
                    decrypt_strict_config_entry(entry, StrictConfigScope::Global).await?,
                ));
            }
            Ok(None) => {}
            Err(error) => skip_global_scope_if_schema_future(&path, error).await?,
        }
    }

    if let Some(path) = system_config_path() {
        match read_config_entry_from_db_path_case_insensitive(
            &path,
            key,
            DatabaseRole::SystemConfig,
        )
        .await
        {
            Ok(Some(entry)) => {
                match decrypt_strict_config_entry(entry, StrictConfigScope::System).await {
                    Ok(value) => return Ok(Some(value)),
                    Err(error) => {
                        tracing::debug!(
                            key,
                            path = %path.display(),
                            error = %format!("{error:#}"),
                            "skipping unsupported system config default"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(
                    key,
                    path = %path.display(),
                    error = %format!("{error:#}"),
                    "skipping unreadable system config scope"
                );
            }
        }
    }

    Ok(None)
}

/// Read the names of all configuration keys under `prefix` through the strict
/// local → global → system cascade, without exposing their values.
///
/// This is for feature gates that must reject an unsupported configuration
/// namespace rather than silently ignore a setting. Section and variable names
/// are compared case-insensitively, as in Git configuration; subsection text
/// remains part of the returned key so the caller can apply its own policy.
pub(crate) async fn read_cascaded_config_keys_by_prefix_strict(
    local_target: LocalIdentityTarget<'_>,
    prefix: &str,
) -> Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    let local_path = match local_target {
        LocalIdentityTarget::CurrentRepo => match try_get_storage_path(None) {
            Ok(storage) => Some(storage.join(DATABASE)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).context("failed to resolve current repository storage");
            }
        },
        LocalIdentityTarget::ExplicitDb(path) => Some(path.to_path_buf()),
        LocalIdentityTarget::None => None,
    };
    if let Some(path) = local_path {
        keys.extend(
            read_config_keys_by_prefix_from_db_path(&path, prefix, DatabaseRole::Repository)
                .await?,
        );
    }

    if let Some(path) = global_config_path_for_read().await {
        match read_config_keys_by_prefix_from_db_path(&path, prefix, DatabaseRole::GlobalConfig)
            .await
        {
            Ok(scope_keys) => keys.extend(scope_keys),
            Err(error) => skip_global_scope_if_schema_future(&path, error).await?,
        }
    }

    if let Some(path) = system_config_path() {
        match read_config_keys_by_prefix_from_db_path(&path, prefix, DatabaseRole::SystemConfig)
            .await
        {
            Ok(scope_keys) => keys.extend(scope_keys),
            Err(error) => {
                tracing::debug!(
                    prefix,
                    path = %path.display(),
                    error = %format!("{error:#}"),
                    "skipping unreadable system config namespace"
                );
            }
        }
    }

    Ok(keys)
}

async fn read_config_keys_by_prefix_from_db_path(
    db_path: &Path,
    prefix: &str,
    role: DatabaseRole,
) -> Result<BTreeSet<String>> {
    let exists = db_path
        .try_exists()
        .with_context(|| format!("failed to inspect config database '{}'", db_path.display()))?;
    if !exists {
        return Ok(BTreeSet::new());
    }

    let conn = open_scoped_config_reader(db_path, role)
        .await
        .with_context(|| format!("failed to open config database '{}'", db_path.display()))?;
    let mut keys = BTreeSet::new();
    if scoped_config_has_kv(&conn, role).await? {
        for row in config_kv::Entity::find()
            .order_by_asc(config_kv::Column::Key)
            .all(&conn)
            .await
            .with_context(|| format!("failed to query config keys in '{}'", db_path.display()))?
        {
            if config_key_has_prefix(&row.key, prefix) {
                keys.insert(row.key);
            }
        }
    }

    if crate::internal::db::sqlite_schema_contains(&conn, "table", "config").await? {
        for row in config::Entity::find()
            .order_by_asc(config::Column::Id)
            .all(&conn)
            .await
            .with_context(|| {
                format!(
                    "failed to query legacy config keys in '{}'",
                    db_path.display()
                )
            })?
        {
            let key = match row.name {
                Some(name) => format!("{}.{}.{}", row.configuration, name, row.key),
                None => format!("{}.{}", row.configuration, row.key),
            };
            if config_key_has_prefix(&key, prefix) {
                keys.insert(key);
            }
        }
    }

    Ok(keys)
}

fn config_key_has_prefix(key: &str, prefix: &str) -> bool {
    key.get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

/// Read every `section.<subsection>.<variable>` value through the strict
/// local → global → system cascade.
///
/// Subsection names remain case-sensitive, while the section and variable
/// follow Git's case-insensitive matching rules. The highest-priority scope
/// wins independently for each subsection. This is intentionally a snapshot:
/// callers that dispatch per path can resolve arbitrary subsection names
/// without rescanning config databases on the hot path.
pub(crate) async fn read_cascaded_subsection_values_strict(
    local_target: LocalIdentityTarget<'_>,
    section: &str,
    variable: &str,
) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();

    let local_path = match local_target {
        LocalIdentityTarget::CurrentRepo => match try_get_storage_path(None) {
            Ok(storage) => Some(storage.join(DATABASE)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).context("failed to resolve current repository storage");
            }
        },
        LocalIdentityTarget::ExplicitDb(path) => Some(path.to_path_buf()),
        LocalIdentityTarget::None => None,
    };
    if let Some(path) = local_path {
        for (subsection, entry) in
            read_subsection_entries_from_db_path(&path, section, variable, DatabaseRole::Repository)
                .await?
        {
            let value =
                decrypt_strict_config_entry(entry, StrictConfigScope::Local(local_target)).await?;
            values.entry(subsection).or_insert(value);
        }
    }

    if let Some(path) = global_config_path_for_read().await {
        match read_subsection_entries_from_db_path(
            &path,
            section,
            variable,
            DatabaseRole::GlobalConfig,
        )
        .await
        {
            Ok(entries) => {
                for (subsection, entry) in entries {
                    let value =
                        decrypt_strict_config_entry(entry, StrictConfigScope::Global).await?;
                    values.entry(subsection).or_insert(value);
                }
            }
            Err(error) => skip_global_scope_if_schema_future(&path, error).await?,
        }
    }

    if let Some(path) = system_config_path() {
        match read_subsection_entries_from_db_path(
            &path,
            section,
            variable,
            DatabaseRole::SystemConfig,
        )
        .await
        {
            Ok(entries) => {
                for (subsection, entry) in entries {
                    match decrypt_strict_config_entry(entry, StrictConfigScope::System).await {
                        Ok(value) => {
                            values.entry(subsection).or_insert(value);
                        }
                        Err(error) => {
                            tracing::debug!(
                                section,
                                variable,
                                path = %path.display(),
                                error = %format!("{error:#}"),
                                "skipping unsupported system subsection config"
                            );
                        }
                    }
                }
            }
            Err(error) => {
                tracing::debug!(
                    section,
                    variable,
                    path = %path.display(),
                    error = %format!("{error:#}"),
                    "skipping unreadable system subsection config"
                );
            }
        }
    }

    Ok(values)
}

enum StrictConfigScope<'a> {
    Local(LocalIdentityTarget<'a>),
    Global,
    System,
}

async fn read_subsection_entries_from_db_path(
    db_path: &Path,
    section: &str,
    variable: &str,
    role: DatabaseRole,
) -> Result<Vec<(String, ConfigKvEntry)>> {
    let exists = db_path
        .try_exists()
        .with_context(|| format!("failed to inspect config database '{}'", db_path.display()))?;
    if !exists {
        return Ok(Vec::new());
    }

    let conn = open_scoped_config_reader(db_path, role)
        .await
        .with_context(|| format!("failed to open config database '{}'", db_path.display()))?;
    let rows = if scoped_config_has_kv(&conn, role).await? {
        config_kv::Entity::find()
            .order_by_desc(config_kv::Column::Id)
            .all(&conn)
            .await
            .with_context(|| {
                format!(
                    "failed to query {section}.*.{variable} from config database '{}'",
                    db_path.display()
                )
            })?
    } else {
        Vec::new()
    };
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for row in &rows {
        let Some((stored_section, Some(subsection), stored_variable)) =
            split_git_config_key(&row.key)
        else {
            continue;
        };
        if stored_section.eq_ignore_ascii_case(section)
            && stored_variable.eq_ignore_ascii_case(variable)
            && seen.insert(subsection.to_string())
        {
            entries.push((subsection.to_string(), ConfigKvEntry::from_model(row)));
        }
    }

    let has_legacy_table = crate::internal::db::sqlite_schema_contains(&conn, "table", "config")
        .await
        .with_context(|| {
            format!(
                "failed to inspect legacy config schema in database '{}'",
                db_path.display()
            )
        })?;
    if has_legacy_table {
        let legacy_rows = config::Entity::find()
            .order_by_desc(config::Column::Id)
            .all(&conn)
            .await
            .with_context(|| {
                format!(
                    "failed to query legacy {section}.*.{variable} config from database '{}'",
                    db_path.display()
                )
            })?;
        for row in legacy_rows {
            let Some(subsection) = row.name else {
                continue;
            };
            if row.configuration.eq_ignore_ascii_case(section)
                && row.key.eq_ignore_ascii_case(variable)
                && seen.insert(subsection.clone())
            {
                entries.push((
                    subsection.clone(),
                    ConfigKvEntry {
                        key: format!("{section}.{subsection}.{variable}"),
                        value: row.value,
                        encrypted: false,
                    },
                ));
            }
        }
    }
    Ok(entries)
}

async fn decrypt_strict_config_entry(
    entry: ConfigKvEntry,
    scope: StrictConfigScope<'_>,
) -> Result<String> {
    if !entry.encrypted {
        return Ok(entry.value);
    }

    match scope {
        StrictConfigScope::Local(local_target) => {
            decrypt_value_for_local_target(&entry.value, local_target)
                .await
                .context("failed to decrypt encrypted local config default")
        }
        StrictConfigScope::Global => decrypt_value(&entry.value, "global")
            .await
            .context("failed to decrypt encrypted global config default"),
        StrictConfigScope::System => {
            Err(anyhow!("encrypted system config defaults are unsupported"))
        }
    }
}

/// Read a config value for the given target using local-first, then global, and
/// decrypt encrypted entries with the matching vault.
///
/// Use this for non-env config keys whose names still trigger sensitive-key
/// encryption, for example credential/profile selectors that are stored through
/// `libra config set`.
pub async fn read_cascaded_config_value_decrypted(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<String>> {
    if let Some(value) = local_config_decrypted_value_for_target(local_target, key).await? {
        return Ok(Some(value));
    }
    global_config_decrypted_value(key).await
}

async fn local_config_decrypted_value_for_target(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<String>> {
    let Some(entry) = local_config_entry_for_target(local_target, key).await? else {
        return Ok(None);
    };

    let value = if entry.encrypted {
        decrypt_value_for_local_target(&entry.value, local_target)
            .await
            .context(format!("failed to decrypt {key} from local config"))?
    } else {
        entry.value
    };
    Ok(trim_non_empty_config_value(value))
}

async fn global_config_decrypted_value(key: &str) -> Result<Option<String>> {
    let Some(db_path) = global_config_path_for_read().await else {
        return Ok(None);
    };
    if !db_path.exists() {
        return Ok(None);
    }

    let Some(entry) =
        read_config_entry_from_db_path(&db_path, key, DatabaseRole::GlobalConfig).await?
    else {
        return Ok(None);
    };
    let value = if entry.encrypted {
        decrypt_value(&entry.value, "global")
            .await
            .context(format!("failed to decrypt {key} from global config"))?
    } else {
        entry.value
    };
    Ok(trim_non_empty_config_value(value))
}

fn trim_non_empty_config_value(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Resolve user identity values from config and environment while preserving
/// the source boundary between the two.
///
/// The returned [`UserIdentitySources`] keeps config-derived and env-derived
/// values in separate fields so callers (notably `libra commit`) can apply
/// `user.useConfigOnly` semantics — refusing to fall back to env vars when
/// the user has explicitly opted into config-only identity.
///
/// Failures while reading the config DB (missing file, stale schema, locked
/// SQLite) are downgraded to `tracing::warn!` + `None` rather than hard
/// errors. Identity is auxiliary at vault-init time (the caller falls back
/// to env vars or hard-coded defaults), and at `commit` time the missing
/// value still surfaces as a clear `IdentityMissing` error — so a corrupted
/// `~/.libra/config.db` no longer blocks `libra init` / `libra clone`.
pub async fn resolve_user_identity_sources(
    local_target: LocalIdentityTarget<'_>,
) -> Result<UserIdentitySources> {
    Ok(UserIdentitySources {
        config_name: read_identity_field_with_warning(local_target, "user.name").await,
        config_email: read_identity_field_with_warning(local_target, "user.email").await,
        env_name: env_first_non_empty(&[
            "GIT_COMMITTER_NAME",
            "GIT_AUTHOR_NAME",
            "LIBRA_COMMITTER_NAME",
        ]),
        env_email: env_first_non_empty(&[
            "GIT_COMMITTER_EMAIL",
            "GIT_AUTHOR_EMAIL",
            "EMAIL",
            "LIBRA_COMMITTER_EMAIL",
        ]),
    })
}

async fn read_identity_field_with_warning(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Option<String> {
    match read_cascaded_config_value(local_target, key).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                key = key,
                error = %format!("{error:#}"),
                "failed to read identity field from config; treating as unset"
            );
            None
        }
    }
}

/// Read a `vault.env.*` entry from the local target, decrypting if needed.
///
/// Boundary condition: encrypted entries with no available unseal key
/// produce `Err`. A missing row produces `Ok(None)`.
async fn local_env_value_for_target(
    local_target: LocalIdentityTarget<'_>,
    vault_key: &str,
) -> Result<Option<String>> {
    let Some(entry) = local_config_entry_for_target(local_target, vault_key).await? else {
        return Ok(None);
    };

    if entry.encrypted {
        let plaintext = decrypt_value_for_local_target(&entry.value, local_target)
            .await
            .context(format!("failed to decrypt {vault_key}"))?;
        return Ok(Some(plaintext));
    }

    Ok(Some(entry.value))
}

/// Resolve the storage path for the given local target and read a single key.
///
/// Returns `Ok(None)` when the target's `.libra/libra.db` does not exist
/// (pre-init repos) or [`LocalIdentityTarget::None`] is selected.
async fn local_config_entry_for_target(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<ConfigKvEntry>> {
    match local_target {
        LocalIdentityTarget::CurrentRepo => {
            let storage = match crate::utils::util::try_get_storage_path(None) {
                Ok(storage) => storage,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).context("failed to resolve current repository storage");
                }
            };
            let db_path = storage.join(crate::utils::util::DATABASE);
            read_config_entry_from_db_path(&db_path, key, DatabaseRole::Repository).await
        }
        LocalIdentityTarget::ExplicitDb(db_path) => {
            read_config_entry_from_db_path(db_path, key, DatabaseRole::Repository).await
        }
        LocalIdentityTarget::None => Ok(None),
    }
}

async fn local_config_entry_for_target_case_insensitive(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<ConfigKvEntry>> {
    match local_target {
        LocalIdentityTarget::CurrentRepo => {
            let storage = match crate::utils::util::try_get_storage_path(None) {
                Ok(storage) => storage,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).context("failed to resolve current repository storage");
                }
            };
            let db_path = storage.join(crate::utils::util::DATABASE);
            read_config_entry_from_db_path_case_insensitive(&db_path, key, DatabaseRole::Repository)
                .await
        }
        LocalIdentityTarget::ExplicitDb(db_path) => {
            read_config_entry_from_db_path_case_insensitive(db_path, key, DatabaseRole::Repository)
                .await
        }
        LocalIdentityTarget::None => Ok(None),
    }
}

/// Look up a `vault.env.<name>` value from the global config DB.
///
/// Returns `Ok(None)` if the global DB does not exist (user has never
/// configured global settings). Otherwise behaves like
/// [`local_env_value_for_target`].
async fn global_env_value(name: &str, vault_key: &str) -> Result<Option<String>> {
    let Some(global_path) = global_config_path_for_read().await else {
        return Ok(None);
    };
    if !global_path.exists() {
        return Ok(None);
    }

    let Some(entry) =
        read_config_entry_from_db_path(&global_path, vault_key, DatabaseRole::GlobalConfig).await?
    else {
        return Ok(None);
    };

    if entry.encrypted {
        let plaintext = decrypt_value(&entry.value, "global")
            .await
            .context(format!(
                "failed to decrypt vault.env.{name} from global config"
            ))?;
        return Ok(Some(plaintext));
    }

    Ok(Some(entry.value))
}

/// Read a (non-vault) config value scoped to the given local target.
///
/// Used by [`read_cascaded_config_value`]; differs from
/// [`local_env_value_for_target`] in that it skips vault decryption and
/// trims whitespace-only values to `None`.
async fn local_config_value_for_target(
    local_target: LocalIdentityTarget<'_>,
    key: &str,
) -> Result<Option<String>> {
    match local_target {
        LocalIdentityTarget::CurrentRepo => {
            let storage = match try_get_storage_path(None) {
                Ok(storage) => storage,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).context("failed to resolve current repository storage");
                }
            };
            let db_path = storage.join(DATABASE);
            read_config_value_from_db_path(&db_path, key, DatabaseRole::Repository).await
        }
        LocalIdentityTarget::ExplicitDb(db_path) => {
            read_config_value_from_db_path(db_path, key, DatabaseRole::Repository).await
        }
        LocalIdentityTarget::None => Ok(None),
    }
}

/// Read a single key from the global config DB, returning `Ok(None)` if no
/// global DB exists or the key is missing.
async fn global_config_value(key: &str) -> Result<Option<String>> {
    let Some(db_path) = global_config_path_for_read().await else {
        return Ok(None);
    };
    if !db_path.exists() {
        return Ok(None);
    }
    global_config_value_at(&db_path, key).await
}

/// Path-parameterised body of [`global_config_value`] so the P0-12
/// future-schema carve-out is unit-testable without mutating process env.
async fn global_config_value_at(db_path: &Path, key: &str) -> Result<Option<String>> {
    match read_config_value_from_db_path(db_path, key, DatabaseRole::GlobalConfig).await {
        Ok(value) => Ok(value),
        Err(error) => {
            skip_global_scope_if_schema_future(db_path, error).await?;
            Ok(None)
        }
    }
}

/// Decide whether a failed global-config read may be skipped (plan-20260708
/// P0-12): a global config DB whose schema is newer than this binary must not
/// hard-fail commands that only consult it for config defaults — the
/// dispatch-level guard has already fail-closed the commands that genuinely
/// need global storage config, and every other command continues with the
/// global scope ignored after the deduplicated schema warning. Any other read
/// failure stays fail-closed with the original error.
async fn skip_global_scope_if_schema_future(db_path: &Path, error: anyhow::Error) -> Result<()> {
    match crate::utils::client_storage::inspect_global_config_schema_future_at_path(db_path).await {
        Some(future) => {
            crate::utils::client_storage::emit_global_config_schema_future_warning(
                &future,
                "ignoring global config values for this command",
            );
            Ok(())
        }
        None => Err(error),
    }
}

/// Best-effort local→global cascade read over FRESH short-lived connections,
/// for sync callers that drive it from a helper thread with its own runtime
/// ([`crate::utils::util::optional_cascaded_config_path`]'s worker). The
/// shared per-path connection cache is deliberately BYPASSED here: a cached
/// pool's return/notify tasks live on the runtime that created it, and the
/// sync wrapper blocks that runtime's only thread while this read runs — an
/// acquire through the cache can then only end by exhausting sqlx's 30 s
/// acquire timeout (observed as two ~28.7 s stalls per `libra add` under the
/// single-threaded test harness; the same stranding class
/// `internal::db`'s pool-size invariant documents). Fresh connections make
/// the read self-contained on the worker's runtime.
///
/// Semantics vs [`read_cascaded_config_value`]: same trim/empty-is-missing
/// value mapping and the same local→global order. Failure isolation
/// (Codex r12): only a scope that PROVABLY has no value (store absent, key
/// absent, or empty after trim) falls through — a local store that exists
/// but cannot be read (lock contention, corrupt/incompatible schema) or
/// holds an ENCRYPTED entry ends the whole lookup with `None`, never
/// letting global override a failing local scope. Schema posture: the open
/// neither inspects nor migrates — a store whose `config_kv` is still
/// queryable (including a future schema, whose additive migrations keep the
/// table readable) answers normally, and a store whose shape broke the
/// query answers `Unusable` → `None`, silently (the dispatch-level policy
/// owns the deduplicated future-schema user warning). This path serves
/// optional plaintext keys such as `core.excludesFile` and is best-effort
/// by contract.
pub async fn read_cascaded_config_value_fresh_conn(key: &str) -> Option<String> {
    let local_db = match try_get_storage_path(None) {
        Ok(storage) => Some(storage.join(DATABASE)),
        // Not inside a repository: local scope is genuinely absent.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        // Repository resolution itself failed: same fail-quiet rule as a
        // failing local store — do not let global answer for it.
        Err(_) => return None,
    };
    let global_db = global_config_path_for_read().await;
    read_cascaded_fresh_conn_at(local_db.as_deref(), global_db.as_deref(), key).await
}

/// Path-parameterised body of [`read_cascaded_config_value_fresh_conn`] so
/// the failure-isolation matrix is unit-testable without cwd games.
async fn read_cascaded_fresh_conn_at(
    local_db: Option<&Path>,
    global_db: Option<&Path>,
    key: &str,
) -> Option<String> {
    /// Per-scope outcome — the three-way split is the point (Codex r12):
    /// only a scope that PROVABLY holds no value falls through to the next
    /// one. A failing local store must not let a configured global value
    /// take over (the cached-pool cascade would have errored the whole
    /// lookup there), and an encrypted local entry terminates the cascade
    /// the same way the old reader's ciphertext value did (the value was
    /// unusable, but global was never consulted).
    enum ScopeRead {
        Value(String),
        Absent,
        Unusable,
    }

    async fn fresh_conn_read(db_path: &Path, key: &str, role: DatabaseRole) -> ScopeRead {
        // `try_exists`, not `exists` (Codex r13): `exists()` answers false
        // for metadata/access ERRORS too, which would misclassify an
        // unreadable store as Absent and let global answer for it. Only a
        // proven NotFound is Absent; any other metadata failure is
        // Unusable.
        match db_path.try_exists() {
            Ok(true) => {}
            Ok(false) => return ScopeRead::Absent,
            Err(_) => return ScopeRead::Unusable,
        }
        // No schema inspection/migration on open: this best-effort READ
        // path must never mutate the store it reads (a 200 ms busy timeout
        // against a concurrent writer could half-apply migrations). An
        // incompatible schema surfaces as a query error → Unusable.
        let conn = match schema::open_readonly_connection_for_role(
            db_path,
            std::time::Duration::from_millis(200),
            role,
        )
        .await
        {
            Ok(conn) => conn,
            Err(_) => return ScopeRead::Unusable,
        };
        match ConfigKv::get_with_conn(&conn, key).await {
            Ok(Some(entry)) if entry.encrypted => ScopeRead::Unusable,
            Ok(Some(entry)) => {
                let trimmed = entry.value.trim();
                if trimmed.is_empty() {
                    ScopeRead::Absent
                } else {
                    ScopeRead::Value(trimmed.to_string())
                }
            }
            Ok(None) => ScopeRead::Absent,
            Err(_) => ScopeRead::Unusable,
        }
    }

    if let Some(local) = local_db {
        match fresh_conn_read(local, key, DatabaseRole::Repository).await {
            ScopeRead::Value(value) => return Some(value),
            ScopeRead::Absent => {}
            ScopeRead::Unusable => return None,
        }
    }
    match fresh_conn_read(global_db?, key, DatabaseRole::GlobalConfig).await {
        ScopeRead::Value(value) => Some(value),
        ScopeRead::Absent | ScopeRead::Unusable => None,
    }
}

/// Local repository reads retain their existing cache/upgrade behavior.
/// Global and system reads have no creation or migration authority.
async fn open_scoped_config_reader(
    db_path: &Path,
    role: DatabaseRole,
) -> Result<DatabaseConnection> {
    match role {
        DatabaseRole::Repository => get_db_conn_instance_for_path(db_path)
            .await
            .map_err(Into::into),
        DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig => {
            let conn = schema::open_readonly_connection_for_role(
                db_path,
                std::time::Duration::from_secs(30),
                role,
            )
            .await?;
            schema::check_configuration_schema(&conn, role).await?;
            Ok(conn)
        }
        DatabaseRole::Derived => Err(anyhow!(
            "derived databases cannot supply configuration defaults; select the owning configuration scope"
        )),
    }
}

async fn scoped_config_has_kv(conn: &DatabaseConnection, role: DatabaseRole) -> Result<bool> {
    if role == DatabaseRole::Repository {
        // Do not hide a broken repository table behind configuration fallback.
        return Ok(true);
    }
    schema::configuration_has_kv(conn, role)
        .await
        .map_err(Into::into)
}

/// Read a config value from `db_path`, trimming whitespace and treating empty
/// strings as missing. Used for non-vault keys where surrounding whitespace
/// is almost certainly a typo.
async fn read_config_value_from_db_path(
    db_path: &Path,
    key: &str,
    role: DatabaseRole,
) -> Result<Option<String>> {
    let entry = read_config_entry_from_db_path(db_path, key, role).await?;
    Ok(entry.and_then(|entry| {
        let trimmed = entry.value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }))
}

/// Open the SQLite DB at `db_path` and read a single `config_kv` entry.
///
/// Returns `Ok(None)` when the file does not exist (so callers can probe
/// optional config locations cheaply). Errors are wrapped with the path so
/// the user can diagnose `permission denied`/`schema mismatch` problems.
async fn read_config_entry_from_db_path(
    db_path: &Path,
    key: &str,
    role: DatabaseRole,
) -> Result<Option<ConfigKvEntry>> {
    if !db_path
        .try_exists()
        .with_context(|| format!("failed to inspect config database '{}'", db_path.display()))?
    {
        return Ok(None);
    }

    let conn = open_scoped_config_reader(db_path, role)
        .await
        .with_context(|| format!("failed to open config database '{}'", db_path.display()))?;
    if !scoped_config_has_kv(&conn, role).await? {
        return Ok(None);
    }
    ConfigKv::get_with_conn(&conn, key).await.with_context(|| {
        format!(
            "failed to query '{key}' from config database '{}'",
            db_path.display()
        )
    })
}

async fn read_config_entry_from_db_path_case_insensitive(
    db_path: &Path,
    key: &str,
    role: DatabaseRole,
) -> Result<Option<ConfigKvEntry>> {
    let exists = db_path
        .try_exists()
        .with_context(|| format!("failed to inspect config database '{}'", db_path.display()))?;
    if !exists {
        return Ok(None);
    }

    let Some((section, subsection, variable)) = split_git_config_key(key) else {
        return Ok(None);
    };
    let conn = open_scoped_config_reader(db_path, role)
        .await
        .with_context(|| format!("failed to open config database '{}'", db_path.display()))?;

    let entries = if scoped_config_has_kv(&conn, role).await? {
        config_kv::Entity::find()
            .order_by_desc(config_kv::Column::Id)
            .all(&conn)
            .await
            .with_context(|| {
                format!(
                    "failed to query '{key}' from config database '{}'",
                    db_path.display()
                )
            })?
    } else {
        Vec::new()
    };
    if let Some(entry) = entries
        .iter()
        .find(|entry| git_config_key_matches(&entry.key, key))
    {
        return Ok(Some(ConfigKvEntry::from_model(entry)));
    }

    // A database missing the legacy `config` table entirely has no legacy rows
    // by construction. Probing first keeps a store whose bootstrap omitted the
    // table from failing every lookup with `no such table: config` (which made
    // `libra init` unusable from `$HOME`, where the config cascade reads the
    // home DB as the local scope).
    let has_legacy_table = crate::internal::db::sqlite_schema_contains(&conn, "table", "config")
        .await
        .with_context(|| {
            format!(
                "failed to inspect legacy config schema in database '{}'",
                db_path.display()
            )
        })?;
    if !has_legacy_table {
        return Ok(None);
    }
    let legacy_entries = config::Entity::find()
        .order_by_desc(config::Column::Id)
        .all(&conn)
        .await
        .with_context(|| {
            format!(
                "failed to query legacy config for '{key}' from database '{}'",
                db_path.display()
            )
        })?;
    Ok(legacy_entries
        .iter()
        .find(|entry| {
            entry.configuration.eq_ignore_ascii_case(section)
                && entry.name.as_deref() == subsection
                && entry.key.eq_ignore_ascii_case(variable)
        })
        .map(|entry| ConfigKvEntry {
            key: key.to_string(),
            value: entry.value.clone(),
            encrypted: false,
        }))
}

/// Split a Git-style dotted config key into section, optional subsection, and
/// variable. The final dot separates the variable so branch names containing
/// dots remain intact.
fn split_git_config_key(key: &str) -> Option<(&str, Option<&str>, &str)> {
    let (section, remainder) = key.split_once('.')?;
    if let Some((subsection, variable)) = remainder.rsplit_once('.') {
        Some((section, Some(subsection), variable))
    } else {
        Some((section, None, remainder))
    }
}

fn git_config_key_matches(stored: &str, requested: &str) -> bool {
    let Some((requested_section, requested_subsection, requested_variable)) =
        split_git_config_key(requested)
    else {
        return false;
    };
    let Some((stored_section, stored_subsection, stored_variable)) = split_git_config_key(stored)
    else {
        return false;
    };

    stored_section.eq_ignore_ascii_case(requested_section)
        && stored_subsection == requested_subsection
        && stored_variable.eq_ignore_ascii_case(requested_variable)
}

// ─────────────────────────────────────────────────────────────────────────────
// Sensitive key detection
// ─────────────────────────────────────────────────────────────────────────────

/// Returns `true` if the key holds sensitive material that should be
/// encrypted and redacted by default.
///
/// Detection rules (applied case-insensitively):
/// 1. `vault.env.*` — every entry under the env vault namespace.
/// 2. Anything ending in `.privkey` — SSH/PGP private keys.
/// 3. Hardcoded vault internals (`vault.unsealkey`, `vault.roottoken`).
/// 4. Substring match on the *last* dotted segment (after stripping `_`/`-`):
///    `secret`, `token`, `password`, `credential`, `privatekey`, `accesskey`,
///    `apikey`, `secretkey`.
/// 5. Explicit exemption: keys ending in `pubkey` / `publickey` are treated
///    as non-sensitive even though they contain `key`.
pub fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();

    // Exact-match vault internals
    if lower.starts_with("vault.env.") {
        return true;
    }
    // Host-token records (lore.md 1.6): owned exclusively by `libra auth` —
    // config get/set/list/unset must neither dump nor forge nor delete them.
    if lower.starts_with("auth.token.") {
        return true;
    }
    if lower.ends_with(".privkey") {
        return true;
    }
    if lower == "vault.unsealkey" || lower == "vault.roottoken" || lower == "vault.roottoken_enc" {
        return true;
    }

    // Normalize the last segment: remove `_` and `-`, lowercase
    let last_segment = lower.rsplit('.').next().unwrap_or(&lower);
    let normalized: String = last_segment
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect();

    // Explicit exclusion for public keys
    if normalized.ends_with("pubkey") || normalized.ends_with("publickey") {
        return false;
    }

    // Check for sensitive substrings in the normalized last segment
    const SENSITIVE_SUBSTRINGS: &[&str] = &[
        "secret",
        "token",
        "password",
        "credential",
        "privatekey",
        "accesskey",
        "apikey",
        "secretkey",
    ];
    SENSITIVE_SUBSTRINGS.iter().any(|s| normalized.contains(s))
}

/// Returns `true` if the key is a vault internal credential that cannot
/// be `--reveal`ed or stored with `--plaintext`.
///
/// Vault internals (unseal key, root token, repo private key) must remain
/// encrypted at all times. The CLI consults this predicate before honouring
/// `--reveal` or `--plaintext` flags.
pub fn is_vault_internal_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.ends_with(".privkey")
        || lower == "vault.unsealkey"
        || lower == "vault.roottoken"
        || lower == "vault.roottoken_enc"
        // Imported GPG secret key (plan-20260921 ADR-VG-02): must be redacted
        // on every read path and can never be revealed.
        || lower == "vault.gpg.seckey_enc"
        // `libra auth` token records: unset via config would be an unaudited
        // logout outside the owner API.
        || lower.starts_with("auth.token.")
}

// ─────────────────────────────────────────────────────────────────────────────
// Legacy Config API (deprecated)
// ─────────────────────────────────────────────────────────────────────────────
//
// The methods below are retained for backwards compatibility with the original
// three-column `config` table. New code should use [`ConfigKv`] instead, which
// supports encryption and richer multi-value semantics.
//
// Many of these legacy helpers `unwrap()` on storage errors. That's deliberate
// for the deprecation period: once a migration is complete the table will be
// dropped, and surfacing failures loudly is preferable to silent fallback.

/// Marker type for the deprecated three-column config API. Use [`ConfigKv`].
#[deprecated(note = "use ConfigKv instead")]
pub struct Config;

/// Internal helper: lets us treat both `DatabaseConnection` and
/// `&DatabaseConnection` uniformly when wiring legacy `Config::*` methods.
/// Avoids extra clones inside the deprecated layer.
trait DatabaseConnectionRef {
    fn as_db_conn_ref(&self) -> &DatabaseConnection;
}

impl DatabaseConnectionRef for DatabaseConnection {
    fn as_db_conn_ref(&self) -> &DatabaseConnection {
        self
    }
}

impl DatabaseConnectionRef for &DatabaseConnection {
    fn as_db_conn_ref(&self) -> &DatabaseConnection {
        self
    }
}

/// Resolved view of a `remote.<name>.*` section.
///
/// Carries only the bare minimum needed by `push`/`fetch`/`clone` flows; the
/// raw URL is whatever the user typed (no scheme normalisation).
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    /// Remote alias, e.g. `origin`.
    pub name: String,
    /// Fetch URL exactly as configured.
    pub url: String,
}
/// Resolved view of `branch.<name>.{remote,merge}` for upstream tracking.
///
/// `merge` is normalised to a short branch name (no `refs/heads/` prefix).
#[allow(dead_code)]
pub struct BranchConfig {
    /// Local branch name.
    pub name: String,
    /// Upstream branch name (e.g. `main`), already stripped of `refs/heads/`.
    pub merge: String,
    /// Upstream remote alias (e.g. `origin`).
    pub remote: String,
}

/*
 * =================================================================================
 * NOTE: Transaction Safety Pattern (`_with_conn`)
 * =================================================================================
 *
 * This module follows the `_with_conn` pattern for transaction safety.
 *
 * - Public functions (e.g., `get`, `update`) acquire a new database
 *   connection from the pool and are suitable for single, non-transactional operations.
 *
 * - `*_with_conn` variants (e.g., `get_with_conn`, `update_with_conn`)
 *   accept an existing connection or transaction handle (`&C where C: ConnectionTrait`).
 *
 * **WARNING**: To use these functions within a database transaction (e.g., inside
 * a `db.transaction(|txn| { ... })` block), you MUST call the `*_with_conn`
 * variant, passing the transaction handle `txn`. Calling a public version from
 * inside a transaction will try to acquire a second connection from the pool,
 * leading to a deadlock.
 *
 * Correct Usage (in a transaction): `Config::update_with_conn(txn, ...).await;`
 * Incorrect Usage (in a transaction): `Config::update(...).await;` // DEADLOCK!
 */
#[allow(deprecated)]
impl Config {
    /// Insert a row into the legacy `config` table without checking for
    /// existing entries. Panics on storage errors — this is the deprecated
    /// path; new code should call [`ConfigKv::add`] / [`ConfigKv::set`].
    pub async fn insert_with_conn<C: ConnectionTrait>(
        db: &C,
        configuration: &str,
        name: Option<&str>,
        key: &str,
        value: &str,
    ) {
        let config = ActiveModel {
            configuration: Set(configuration.to_owned()),
            name: Set(name.map(|s| s.to_owned())),
            key: Set(key.to_owned()),
            value: Set(value.to_owned()),
            ..Default::default()
        };
        // INVARIANT (deprecated lossy API): storage failures here are
        // unrecoverable for this legacy path. ConfigKv::add / ConfigKv::set
        // surface the same failure as a typed error.
        config
            .save(db)
            .await
            .expect("legacy Config::insert_with_conn: DB save failed");
    }

    /// Update an existing config row's value. Panics if no matching row
    /// exists. Deprecated; prefer [`ConfigKv::set`].
    pub async fn update_with_conn<C: ConnectionTrait>(
        db: &C,
        configuration: &str,
        name: Option<&str>,
        key: &str,
        value: &str,
    ) -> Model {
        // INVARIANT (deprecated lossy API): callers must have verified the
        // (configuration, name, key) tuple exists before calling. The
        // SeaORM `find().one()` returns `Result<Option<Model>, DbErr>`, so
        // the outer .expect() surfaces query failures and the inner
        // .expect() surfaces the missing-row case. Both are unrecoverable
        // for this legacy path; ConfigKv::set replaces the whole sequence
        // with an upsert and explicit errors.
        let mut config: ActiveModel = config::Entity::find()
            .filter(config::Column::Configuration.eq(configuration))
            .filter(match name {
                Some(str) => config::Column::Name.eq(str),
                None => config::Column::Name.is_null(),
            })
            .filter(config::Column::Key.eq(key))
            .one(db)
            .await
            .expect("legacy Config::update_with_conn: DB query failed")
            .expect("legacy Config::update_with_conn: target config row missing (use ConfigKv::set for upsert semantics)")
            .into();
        config.value = Set(value.to_owned());
        config
            .update(db)
            .await
            .expect("legacy Config::update_with_conn: DB update failed")
    }

    /// Internal: list every legacy row matching `(configuration, name, key)`.
    /// Used by `get*`/`get_all*` and the delete pipeline.
    async fn query_with_conn<C: ConnectionTrait>(
        db: &C,
        configuration: &str,
        name: Option<&str>,
        key: &str,
    ) -> Vec<Model> {
        config::Entity::find()
            .filter(config::Column::Configuration.eq(configuration))
            .filter(match name {
                Some(str) => config::Column::Name.eq(str),
                None => config::Column::Name.is_null(),
            })
            .filter(config::Column::Key.eq(key))
            .all(db)
            .await
            .expect("legacy Config::query_with_conn: DB query failed")
    }

    /// Get the first matching value (insertion order). Returns `None` for
    /// missing keys. Deprecated; prefer [`ConfigKv::get`].
    pub async fn get_with_conn<C: ConnectionTrait>(
        db: &C,
        configuration: &str,
        name: Option<&str>,
        key: &str,
    ) -> Option<String> {
        let values = Self::query_with_conn(db, configuration, name, key).await;
        values.first().map(|c| c.value.to_owned())
    }

    /// Legacy `branch.<branch>.remote` lookup. Deprecated;
    /// prefer [`ConfigKv::get_remote_with_conn`].
    pub async fn get_remote_with_conn<C: ConnectionTrait>(db: &C, branch: &str) -> Option<String> {
        Config::get_with_conn(db, "branch", Some(branch), "remote").await
    }

    /// Legacy upstream-remote lookup. Returns `Err(())` (note: unit error,
    /// not anyhow) when HEAD is detached. Deprecated; prefer
    /// [`ConfigKv::get_current_remote_with_conn`].
    pub async fn get_current_remote_with_conn<C: ConnectionTrait>(
        db: &C,
    ) -> Result<Option<String>> {
        match Head::current_with_conn(db).await {
            Head::Branch(name) => Ok(Config::get_remote_with_conn(db, &name).await),
            Head::Detached(_) => {
                anyhow::bail!("HEAD is detached, cannot get remote")
            }
        }
    }

    /// Legacy fetch-URL lookup. **Panics** when the URL is missing — this
    /// pre-dates the structured error path and is preserved for compatibility
    /// only. Deprecated; prefer [`ConfigKv::get_remote_url_with_conn`].
    pub async fn get_remote_url_with_conn<C: ConnectionTrait>(db: &C, remote: &str) -> String {
        match Config::get_with_conn(db, "remote", Some(remote), "url").await {
            Some(url) => url,
            None => panic!("fatal: No URL configured for remote '{remote}'."),
        }
    }

    /// Legacy "URL of the current branch's upstream" lookup.
    pub async fn get_current_remote_url_with_conn<C: ConnectionTrait>(db: &C) -> Option<String> {
        // INVARIANT (deprecated lossy API): `get_current_remote_with_conn`
        // returns Err(()) only when HEAD is detached, after already
        // printing a `fatal: HEAD is detached, cannot get remote` message
        // to stderr. The legacy contract is to panic in that case rather
        // than silently treat it as "no remote"; callers that need
        // graceful handling should use `ConfigKv::get_current_remote_url_with_conn`.
        match Config::get_current_remote_with_conn(db)
            .await
            .expect("legacy Config::get_current_remote_url_with_conn: HEAD is detached")
        {
            Some(remote) => Some(Config::get_remote_url_with_conn(db, &remote).await),
            None => None,
        }
    }

    /// Legacy multi-value getter. Returns every `value` for the matching
    /// triple in insertion order. Deprecated.
    pub async fn get_all_with_conn<C: ConnectionTrait>(
        db: &C,
        configuration: &str,
        name: Option<&str>,
        key: &str,
    ) -> Vec<String> {
        Self::query_with_conn(db, configuration, name, key)
            .await
            .iter()
            .map(|c| c.value.to_owned())
            .collect()
    }

    /// Legacy `git config --list` equivalent: emits `(dotted_key, value)`
    /// pairs for every row in the table. Deprecated.
    pub async fn list_all_with_conn<C: ConnectionTrait>(db: &C) -> Vec<(String, String)> {
        config::Entity::find()
            .all(db)
            .await
            .expect("legacy Config::list_all_with_conn: DB query failed")
            .iter()
            .map(|m| {
                (
                    match &m.name {
                        Some(n) => m.configuration.to_owned() + "." + n + "." + &m.key,
                        None => m.configuration.to_owned() + "." + &m.key,
                    },
                    m.value.to_owned(),
                )
            })
            .collect()
    }

    /// Delete one or all matching legacy config rows.
    ///
    /// Boundary conditions:
    /// - `valuepattern` filters by substring match against the row's value.
    /// - `delete_all = false` stops after the first deletion (mirrors
    ///   `git config --unset`).
    /// - Returns the underlying `DbErr` on failure; rows already deleted
    ///   before the failure remain deleted (no implicit transaction).
    pub async fn remove_config_with_conn<C: ConnectionTrait>(
        db: &C,
        configuration: &str,
        name: Option<&str>,
        key: &str,
        valuepattern: Option<&str>,
        delete_all: bool,
    ) -> Result<(), sea_orm::DbErr> {
        let entries: Vec<Model> = Self::query_with_conn(db, configuration, name, key).await;
        for e in entries {
            match valuepattern {
                Some(vp) => {
                    if e.value.contains(vp) {
                        e.delete(db).await?;
                    } else {
                        continue;
                    }
                }
                None => {
                    e.delete(db).await?;
                }
            };
            if !delete_all {
                break;
            }
        }
        Ok(())
    }

    /// Legacy "remove every `remote.<name>.*` row" helper. Returns
    /// `Err(String)` (note: not anyhow) when the remote does not exist.
    pub async fn remove_remote_with_conn<C: ConnectionTrait>(
        db: &C,
        name: &str,
    ) -> Result<(), String> {
        let remote = config::Entity::find()
            .filter(config::Column::Configuration.eq("remote"))
            .filter(config::Column::Name.eq(name))
            .all(db)
            .await
            .expect("legacy Config::remove_remote_with_conn: DB query failed");
        if remote.is_empty() {
            return Err(format!("fatal: No such remote: {name}"));
        }
        for r in remote {
            let r: ActiveModel = r.into();
            r.delete(db)
                .await
                .expect("legacy Config::remove_remote_with_conn: DB delete failed");
        }
        Ok(())
    }

    /// Pool-acquiring counterpart of [`Self::insert_with_conn`]. Deprecated.
    pub async fn insert(configuration: &str, name: Option<&str>, key: &str, value: &str) {
        let db = get_db_conn_instance().await;
        Self::insert_with_conn(&db, configuration, name, key, value).await;
    }

    /// Update one configuration entry in database using given configuration, name, key and value.
    pub async fn update(configuration: &str, name: Option<&str>, key: &str, value: &str) -> Model {
        let db = get_db_conn_instance().await;
        Self::update_with_conn(&db, configuration, name, key, value).await
    }

    /// Get one configuration value (legacy table). Deprecated.
    pub async fn get(configuration: &str, name: Option<&str>, key: &str) -> Option<String> {
        let db = get_db_conn_instance().await;
        Self::get_with_conn(&db, configuration, name, key).await
    }

    /// Get remote repo name by branch name (legacy).
    /// - Returns `None` when `branch.<name>.remote` is unset; callers usually
    ///   need to `branch --set-upstream` first.
    pub async fn get_remote(branch: &str) -> Option<String> {
        let db = get_db_conn_instance().await;
        Self::get_remote_with_conn(&db, branch).await
    }

    /// Get remote repo name of current branch (legacy).
    /// Returns `Err(())` when HEAD is detached.
    pub async fn get_current_remote() -> Result<Option<String>> {
        let db = get_db_conn_instance().await;
        Self::get_current_remote_with_conn(&db).await
    }

    /// Pool-acquiring counterpart of [`Self::get_remote_url_with_conn`].
    /// Panics when no URL is configured (legacy behaviour).
    pub async fn get_remote_url(remote: &str) -> String {
        let db = get_db_conn_instance().await;
        Self::get_remote_url_with_conn(&db, remote).await
    }

    /// Returns `None` if no remote is set on the current branch.
    pub async fn get_current_remote_url() -> Option<String> {
        let db = get_db_conn_instance().await;
        Self::get_current_remote_url_with_conn(&db).await
    }

    /// Get all configuration values (legacy multi-value reader).
    /// e.g. `remote.origin.fetch` may have multiple entries.
    pub async fn get_all(configuration: &str, name: Option<&str>, key: &str) -> Vec<String> {
        let db = get_db_conn_instance().await;
        Self::get_all_with_conn(&db, configuration, name, key).await
    }

    /// Get literally all the entries in database without any filtering.
    pub async fn list_all() -> Vec<(String, String)> {
        let db = get_db_conn_instance().await;
        Self::list_all_with_conn(&db).await
    }

    /// Delete one or all configuration entries using given key and value pattern.
    pub async fn remove_config(
        configuration: &str,
        name: Option<&str>,
        key: &str,
        valuepattern: Option<&str>,
        delete_all: bool,
    ) -> Result<(), sea_orm::DbErr> {
        let db = get_db_conn_instance().await;
        Self::remove_config_with_conn(
            db.as_db_conn_ref(),
            configuration,
            name,
            key,
            valuepattern,
            delete_all,
        )
        .await
    }

    /// Remove every row matching the given `(configuration, name, key)` triple.
    pub async fn remove(
        configuration: &str,
        name: Option<&str>,
        key: &str,
    ) -> Result<(), sea_orm::DbErr> {
        Self::remove_config(configuration, name, key, None, true).await
    }

    // NOTE: `remove_by_section` was once contemplated as a `--remove-section`
    // implementation but never landed; new section-wide deletion goes through
    // [`ConfigKv::get_by_prefix`] + per-row delete.

    /// Pool-acquiring counterpart of [`Self::remove_remote_with_conn`].
    pub async fn remove_remote(name: &str) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        Self::remove_remote_with_conn(&db, name).await
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::Statement;

    use super::*;

    mod env_restore_tests {
        include!("config/env_restore_tests.rs");
    }

    /// plan-20260825 PS-06: `locate_env_for_target` tags the hit layer and
    /// `resolve_env_for_target` delegates to it, so value and layer can
    /// never disagree. Uses the process-env layer (deterministic, no DB) and
    /// an unset name against an isolated global DB for the miss path.
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn locate_env_reports_the_hit_layer_and_resolver_delegates() {
        use crate::utils::test::{ConfigDbFixture, ScopedEnvVar};

        let name = "LIBRA_PS06_LOCATE_TEST_KEY";
        let _process_probe = ScopedEnvVar::set(name, "layer-probe");
        let located = locate_env_for_target(name, LocalIdentityTarget::None)
            .await
            .expect("locate over process env");
        assert_eq!(
            located,
            Some(("layer-probe".to_string(), EnvHitLayer::ProcessEnvironment))
        );
        let resolved = resolve_env_for_target(name, LocalIdentityTarget::None)
            .await
            .expect("resolver delegates");
        assert_eq!(resolved.as_deref(), Some("layer-probe"));
        // Drops before _process_probe, which then restores the caller's value.
        let _missing_probe = ScopedEnvVar::unset(name);

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_fixture = ConfigDbFixture::new().expect("create config DB fixture");
        let db = config_fixture.global_db().to_path_buf();
        let missing = locate_env_for_target(name, LocalIdentityTarget::None)
            .await
            .expect("miss path");
        assert_eq!(missing, None, "unset name must locate nowhere");

        // Repo-local and global layers, and local-over-global precedence
        // (PS-06 terra R1: swapping the two labels must turn a test red).
        let global_conn =
            crate::internal::db::create_database(db.to_str().expect("utf8 global db path"))
                .await
                .expect("create isolated global db");
        ConfigKv::set_with_conn(
            &global_conn,
            &format!("vault.env.{name}"),
            "from-global",
            false,
        )
        .await
        .expect("write global vault value");
        drop(global_conn);

        let located = locate_env_for_target(name, LocalIdentityTarget::None)
            .await
            .expect("global layer");
        assert_eq!(
            located,
            Some(("from-global".to_string(), EnvHitLayer::GlobalVault)),
            "global vault hit must carry the GlobalVault label"
        );

        let local_db = tmp.path().join("isolated-local.db");
        let local_conn =
            crate::internal::db::create_database(local_db.to_str().expect("utf8 local db path"))
                .await
                .expect("create isolated local db");
        ConfigKv::set_with_conn(
            &local_conn,
            &format!("vault.env.{name}"),
            "from-local",
            false,
        )
        .await
        .expect("write local vault value");
        drop(local_conn);

        let located = locate_env_for_target(name, LocalIdentityTarget::ExplicitDb(&local_db))
            .await
            .expect("local layer");
        assert_eq!(
            located,
            Some(("from-local".to_string(), EnvHitLayer::RepoLocalVault)),
            "repo-local must win over global and carry the RepoLocalVault label"
        );
    }

    /// plan-20260825 PS-06 terra R3: the factory's sync lookup must read the
    /// vault of the DIRECTORY it is given, not the process cwd — two sibling
    /// repositories with different keys prove the routing at the exact
    /// function the factory calls.
    #[test]
    #[serial_test::serial(env)]
    fn resolve_env_sync_for_dir_reads_the_target_repos_vault() {
        use crate::utils::test::{ConfigDbFixture, ScopedEnvVar};

        let name = "LIBRA_PS06_AB_FACTORY_KEY";
        let _probe = ScopedEnvVar::unset(name);
        let tmp = tempfile::tempdir().expect("tempdir");
        let _config_fixture = ConfigDbFixture::new().expect("create config DB fixture");
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        for (repo, value) in [("a", "from-repo-a"), ("b", "from-repo-b")] {
            let storage = tmp.path().join(repo).join(".libra");
            std::fs::create_dir_all(&storage).expect("storage dir");
            let db = storage.join(crate::utils::util::DATABASE);
            let conn = runtime
                .block_on(crate::internal::db::create_database(
                    db.to_str().expect("utf8"),
                ))
                .expect("create repo db");
            runtime
                .block_on(ConfigKv::set_with_conn(
                    &conn,
                    &format!("vault.env.{name}"),
                    value,
                    false,
                ))
                .expect("write repo vault value");
        }
        drop(runtime);

        let from_a = resolve_env_sync_for_dir(name, &tmp.path().join("a")).expect("resolve A");
        assert_eq!(from_a.as_deref(), Some("from-repo-a"));
        let from_b = resolve_env_sync_for_dir(name, &tmp.path().join("b")).expect("resolve B");
        assert_eq!(
            from_b.as_deref(),
            Some("from-repo-b"),
            "the factory chain must follow the given directory, not the cwd"
        );
    }

    async fn write_schema_version(db_path: &Path, version: i64) {
        let conn = crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
            .await
            .expect("create config db");
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM schema_versions",
            [],
        ))
        .await
        .expect("clear schema versions");
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
            [
                version.into(),
                "test_future_schema".into(),
                "2026-07-15T00:00:00Z".into(),
            ],
        ))
        .await
        .expect("insert schema version");
        conn.close().await.expect("close config db");
    }

    /// P0-12 carve-out on the non-strict cascade: a global config DB whose
    /// schema is newer than this binary reads as "scope unset" instead of
    /// erroring, while any other unreadable store keeps the original error.
    /// Pins `global_config_value_at` directly so the regression cannot hide
    /// behind the CLI dispatch warning or a strict-cascade read (Codex
    /// re-review P1, 2026-07-15).
    #[tokio::test]
    async fn global_config_value_skips_future_schema_and_keeps_other_errors() {
        let temp = tempfile::tempdir().expect("create tempdir");

        let future_db = temp.path().join("future-config.db");
        let latest = crate::internal::db::migration::latest_builtin_schema_version()
            .expect("read latest schema version")
            .expect("built-in migrations have a latest version");
        write_schema_version(&future_db, latest + 1).await;
        let value = global_config_value_at(&future_db, "commit.cleanup")
            .await
            .expect("future schema must degrade to unset, not error");
        assert_eq!(value, None);

        let corrupt_db = temp.path().join("corrupt-config.db");
        std::fs::write(&corrupt_db, b"this is not a sqlite database").expect("write corrupt db");
        let error = global_config_value_at(&corrupt_db, "commit.cleanup")
            .await
            .expect_err("a corrupt global store must keep failing");
        assert!(
            !format!("{error:#}").contains("newer than this Libra binary"),
            "corruption must not be classified as future schema: {error:#}"
        );
    }

    /// The legacy `config`-table fallback answers "no value" — not
    /// `no such table: config` — when the store lacks the legacy table
    /// entirely. A store that still carries the table keeps serving its
    /// legacy rows through the same reader.
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn legacy_config_fallback_tolerates_missing_table() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let latest = crate::internal::db::migration::latest_builtin_schema_version()
            .expect("read latest schema version")
            .expect("built-in migrations have a latest version");

        // A store whose bootstrap omitted the legacy table: full canonical DB,
        // then `config` dropped and the schema version pinned at latest so the
        // migrating open cannot silently re-create it.
        let missing_table_db = temp.path().join("missing-legacy.db");
        {
            let conn =
                crate::internal::db::create_database(missing_table_db.to_str().expect("utf8 path"))
                    .await
                    .expect("create config db");
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "DROP TABLE `config`",
            ))
            .await
            .expect("drop legacy table");
            conn.close().await.expect("close config db");
        }
        // Pin the schema version at latest IN PLACE (the db already exists, so
        // `create_database` refuses it): the migrating open must not re-create
        // the dropped table through the self-heal migration.
        {
            let conn = crate::internal::db::open_connection_without_schema_management(
                missing_table_db.to_str().expect("utf8 path"),
                std::time::Duration::from_millis(200),
            )
            .await
            .expect("open db to pin schema version");
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "DELETE FROM `schema_versions`",
            ))
            .await
            .expect("clear schema versions");
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO `schema_versions` (`version`, `name`, `applied_at`) VALUES (?, ?, ?)",
                [
                    latest.into(),
                    "test_missing_legacy_table".into(),
                    "2026-09-06T00:00:00Z".into(),
                ],
            ))
            .await
            .expect("pin schema version");
            conn.close().await.expect("close db");
        }

        let entry = read_config_entry_from_db_path_case_insensitive(
            &missing_table_db,
            "init.defaultBranch",
            DatabaseRole::Repository,
        )
        .await
        .expect("a missing legacy table means no legacy rows, not a lookup failure");
        assert!(entry.is_none(), "no legacy table must read as no value");

        // Control: a store that still carries the legacy table keeps serving
        // legacy rows (NULL subsection matches Git's section.variable keys).
        let legacy_db = temp.path().join("with-legacy.db");
        {
            let conn = crate::internal::db::create_database(legacy_db.to_str().expect("utf8 path"))
                .await
                .expect("create config db");
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "INSERT INTO `config` (`configuration`, `name`, `key`, `value`) \
                 VALUES ('init', NULL, 'defaultBranch', 'legacy-main')",
            ))
            .await
            .expect("insert legacy row");
            conn.close().await.expect("close config db");
        }

        let entry = read_config_entry_from_db_path_case_insensitive(
            &legacy_db,
            "init.defaultBranch",
            DatabaseRole::Repository,
        )
        .await
        .expect("legacy rows remain readable");
        assert_eq!(entry.map(|entry| entry.value), Some("legacy-main".into()));
    }

    /// Helper for the fresh-conn cascade matrix: a config DB with one
    /// `config_kv` row (optionally flagged encrypted).
    async fn write_config_db(db_path: &Path, key: &str, value: &str, encrypted: bool) {
        let conn = crate::internal::db::create_database(db_path.to_str().expect("utf8 db path"))
            .await
            .expect("create config db");
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO config_kv (key, value, encrypted) VALUES (?, ?, ?)",
            [key.into(), value.into(), (encrypted as i32).into()],
        ))
        .await
        .expect("insert config value");
        conn.close().await.expect("close config db");
    }

    async fn append_plain_config(db_path: &Path, key: &str, value: &str) {
        let conn = crate::internal::db::open_connection_without_schema_management(
            db_path.to_str().expect("utf8 db path"),
            std::time::Duration::from_millis(200),
        )
        .await
        .expect("open config db");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO config_kv (key, value, encrypted) VALUES (?, ?, 0)",
            [key.into(), value.into()],
        ))
        .await
        .expect("append config value");
        conn.close().await.expect("close config db");
    }

    #[tokio::test]
    #[serial_test::serial(env)]
    async fn strict_subsection_snapshot_cascades_each_case_sensitive_name() {
        use crate::utils::test::ConfigDbFixture;

        let config_fixture = ConfigDbFixture::new().expect("create config DB fixture");
        let local_db = config_fixture.root().join("local.db");
        let global_db = config_fixture.global_db().to_path_buf();
        let system_db = config_fixture.system_db().to_path_buf();
        write_config_db(&local_db, "Merge.Shared.Driver", "local-shared", false).await;
        append_plain_config(&local_db, "merge.Local.driver", "local-only").await;
        write_config_db(&global_db, "merge.Shared.driver", "global-shadowed", false).await;
        append_plain_config(&global_db, "MERGE.Global.DRIVER", "global-only").await;
        append_plain_config(&global_db, "merge.local.driver", "lowercase-subsection").await;
        write_config_db(&system_db, "merge.System.driver", "system-only", false).await;
        let values = read_cascaded_subsection_values_strict(
            LocalIdentityTarget::ExplicitDb(&local_db),
            "merge",
            "driver",
        )
        .await
        .expect("read strict subsection snapshot");

        assert_eq!(
            values,
            BTreeMap::from([
                ("Global".to_string(), "global-only".to_string()),
                ("Local".to_string(), "local-only".to_string()),
                ("Shared".to_string(), "local-shared".to_string()),
                ("System".to_string(), "system-only".to_string()),
                ("local".to_string(), "lowercase-subsection".to_string()),
            ])
        );
    }

    #[tokio::test]
    #[serial_test::serial(env)]
    async fn strict_prefix_key_snapshot_includes_all_scopes_and_legacy_rows() {
        use crate::utils::test::ConfigDbFixture;

        let config_fixture = ConfigDbFixture::new().expect("create config DB fixture");
        let local_db = config_fixture.root().join("local.db");
        let global_db = config_fixture.global_db().to_path_buf();
        let system_db = config_fixture.system_db().to_path_buf();
        write_config_db(&local_db, "MERGETOOL.local.cmd", "local-tool", false).await;
        write_config_db(
            &global_db,
            "mergetool.global.futurePreference",
            "true",
            false,
        )
        .await;
        write_config_db(&system_db, "unrelated.key", "ignored", false).await;
        {
            let conn = crate::internal::db::open_connection_without_schema_management(
                system_db.to_str().expect("utf8 system db path"),
                std::time::Duration::from_millis(200),
            )
            .await
            .expect("open system db");
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "INSERT INTO `config` (`configuration`, `name`, `key`, `value`) VALUES ('mergetool', 'legacy', 'path', '/usr/bin/legacy-tool')",
            ))
            .await
            .expect("insert legacy mergetool key");
            conn.close().await.expect("close system db");
        }
        let keys = read_cascaded_config_keys_by_prefix_strict(
            LocalIdentityTarget::ExplicitDb(&local_db),
            "mergetool.",
        )
        .await
        .expect("read prefix key snapshot");

        assert_eq!(
            keys,
            BTreeSet::from([
                "MERGETOOL.local.cmd".to_string(),
                "mergetool.global.futurePreference".to_string(),
                "mergetool.legacy.path".to_string(),
            ])
        );
    }

    async fn max_schema_version(db_path: &Path) -> i64 {
        let conn = crate::internal::db::open_connection_without_schema_management(
            db_path.to_str().expect("utf8 db path"),
            std::time::Duration::from_millis(200),
        )
        .await
        .expect("open db");
        let backend = conn.get_database_backend();
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT MAX(version) AS v FROM schema_versions",
                [],
            ))
            .await
            .expect("query schema version")
            .expect("schema_versions row");
        row.try_get::<Option<i64>>("", "v")
            .expect("read version column")
            .expect("non-empty schema_versions")
    }

    /// W5-09 / Codex r12: the fresh-connection best-effort cascade pins the
    /// full failure-isolation matrix — only a scope that PROVABLY has no
    /// value falls through, and nothing on this READ path may mutate the
    /// store (no migration on open).
    #[tokio::test]
    async fn fresh_conn_cascade_pins_precedence_and_failure_isolation() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let key = "core.excludesFile";

        let global_db = temp.path().join("global.db");
        write_config_db(&global_db, key, "/global/excludes", false).await;

        // (1) Local value wins over global.
        let local_db = temp.path().join("local-value.db");
        write_config_db(&local_db, key, "/local/excludes", false).await;
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&local_db), Some(&global_db), key).await,
            Some("/local/excludes".to_string()),
            "local value must win"
        );

        // (2) Local file absent → global answers.
        let missing = temp.path().join("no-such.db");
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&missing), Some(&global_db), key).await,
            Some("/global/excludes".to_string()),
            "an absent local store falls through"
        );

        // (3) Local key absent → global answers.
        let local_other = temp.path().join("local-other-key.db");
        write_config_db(&local_other, "core.attributesFile", "/x", false).await;
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&local_other), Some(&global_db), key).await,
            Some("/global/excludes".to_string()),
            "a missing key falls through"
        );

        // (4) Local empty-after-trim → global answers (trim semantics).
        let local_empty = temp.path().join("local-empty.db");
        write_config_db(&local_empty, key, "   ", false).await;
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&local_empty), Some(&global_db), key).await,
            Some("/global/excludes".to_string()),
            "an empty value is missing, not a veto"
        );

        // (5) ENCRYPTED local entry terminates the cascade: global must NOT
        // override a local scope that holds (unusable) state.
        let local_encrypted = temp.path().join("local-encrypted.db");
        write_config_db(&local_encrypted, key, "deadbeef", true).await;
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&local_encrypted), Some(&global_db), key).await,
            None,
            "an encrypted local entry must not fall through to global"
        );

        // (6) A local store that exists but cannot be read terminates the
        // cascade — a failing local scope never lets global answer for it.
        let local_corrupt = temp.path().join("local-corrupt.db");
        std::fs::write(&local_corrupt, b"this is not a sqlite database").expect("write corrupt db");
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&local_corrupt), Some(&global_db), key).await,
            None,
            "a corrupt local store must not fall through to global"
        );

        // (7) No local scope at all (outside a repo) → global answers; and
        // with no global either, the lookup is None.
        assert_eq!(
            read_cascaded_fresh_conn_at(None, Some(&global_db), key).await,
            Some("/global/excludes".to_string())
        );
        assert_eq!(read_cascaded_fresh_conn_at(None, None, key).await, None);

        // (8) A local path whose existence cannot even be DETERMINED
        // (metadata error — here ENOTDIR from a file used as a directory)
        // is Unusable, not Absent: global must not answer for it (Codex
        // r13: `exists()` would have swallowed this into false/Absent).
        let not_a_dir = temp.path().join("plain-file");
        std::fs::write(&not_a_dir, b"just a file").expect("write plain file");
        let unreachable_db = not_a_dir.join("config.db");
        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&unreachable_db), Some(&global_db), key).await,
            None,
            "a metadata error on the local store must not fall through to global"
        );
    }

    /// W5-09 / Codex r12: the fresh-conn reader must not apply pending
    /// migrations on open — reading through it leaves `schema_versions`
    /// exactly as found (the 200 ms busy-timeout open would otherwise race
    /// concurrent writers with a half-applied migration).
    #[tokio::test]
    async fn fresh_conn_reader_does_not_migrate_on_open() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let key = "core.excludesFile";
        let db = temp.path().join("pending-migration.db");
        write_config_db(&db, key, "/kept/excludes", false).await;

        // Roll the version stamp BACK so a migrating open would have pending
        // work to apply (the runner would bump MAX(version) right back).
        let conn = crate::internal::db::open_connection_without_schema_management(
            db.to_str().expect("utf8 db path"),
            std::time::Duration::from_millis(200),
        )
        .await
        .expect("open db");
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM schema_versions WHERE version = (SELECT MAX(version) FROM schema_versions)",
            [],
        ))
        .await
        .expect("drop latest schema stamp");
        let downgraded = max_schema_version(&db).await;
        conn.close().await.expect("close db");

        assert_eq!(
            read_cascaded_fresh_conn_at(Some(&db), None, key).await,
            Some("/kept/excludes".to_string()),
            "the value stays readable without migrating"
        );
        assert_eq!(
            max_schema_version(&db).await,
            downgraded,
            "the read must not have applied pending migrations"
        );
    }

    // ── ADR-GCX-01: global config path resolution ──

    #[test]
    fn config_dir_from_ignores_relative_xdg_and_missing_home() {
        use std::{ffi::OsStr, path::PathBuf};

        assert_eq!(config_dir_from(None, None), None);
        assert_eq!(
            config_dir_from(None, Some(OsStr::new("/xdg"))),
            Some((
                PathBuf::from("/xdg/libra"),
                GlobalConfigPathSource::XdgConfigHome,
            ))
        );
        assert_eq!(
            config_dir_from(Some(PathBuf::from("/home/u")), Some(OsStr::new("relative"))),
            Some((
                PathBuf::from("/home/u/.config/libra"),
                GlobalConfigPathSource::HomeConfig,
            ))
        );
        assert_eq!(
            config_dir_from(Some(PathBuf::from("/home/u")), Some(OsStr::new(""))),
            Some((
                PathBuf::from("/home/u/.config/libra"),
                GlobalConfigPathSource::HomeConfig,
            ))
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn global_config_resolution_prefers_legacy_then_new_then_xdg_then_env() {
        use crate::utils::test::ScopedEnvVar;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let xdg = tmp.path().join("xdg");
        std::fs::create_dir_all(&home).expect("home dir");
        std::fs::create_dir_all(xdg.join("libra")).expect("xdg dir");

        let _env = ScopedEnvVar::unset("LIBRA_CONFIG_GLOBAL_DB");
        let _home = ScopedEnvVar::set("HOME", &home);
        let _profile = ScopedEnvVar::set("USERPROFILE", &home);
        let _xdg_unset = ScopedEnvVar::unset("XDG_CONFIG_HOME");

        // Fresh home: the XDG-style default under <home>/.config/libra.
        let fresh = global_config_resolution().expect("fresh resolution");
        assert_eq!(
            fresh.path,
            home.join(".config").join("libra").join("config.db")
        );
        assert_eq!(fresh.source, GlobalConfigPathSource::HomeConfig);
        assert!(!fresh.migration_pending);
        assert!(!fresh.new_exists);

        // Legacy-only: the legacy file stays active and migration is pending.
        std::fs::create_dir_all(home.join(".libra")).expect("legacy dir");
        let legacy = home.join(".libra").join("config.db");
        std::fs::write(&legacy, b"legacy").expect("legacy db");
        let pending = global_config_resolution().expect("pending resolution");
        assert_eq!(pending.path, legacy);
        assert_eq!(pending.source, GlobalConfigPathSource::LegacyHome);
        assert!(pending.migration_pending);
        assert!(pending.legacy_exists);

        // Once the new-layout file exists it wins and migration is not pending.
        let new_config_dir = home.join(".config").join("libra");
        std::fs::create_dir_all(&new_config_dir).expect("new config dir");
        std::fs::write(new_config_dir.join("config.db"), b"new").expect("new db");
        let migrated = global_config_resolution().expect("migrated resolution");
        assert_eq!(migrated.path, new_config_dir.join("config.db"));
        assert_eq!(migrated.source, GlobalConfigPathSource::HomeConfig);
        assert!(!migrated.migration_pending);

        // XDG set, but its database missing while legacy exists: the legacy
        // file stays active (one file, no split state) until migration.
        let _xdg = ScopedEnvVar::set("XDG_CONFIG_HOME", &xdg);
        let xdg_pending = global_config_resolution().expect("xdg pending resolution");
        assert_eq!(xdg_pending.path, legacy);
        assert_eq!(xdg_pending.source, GlobalConfigPathSource::LegacyHome);
        assert!(xdg_pending.migration_pending);

        // Once the XDG database exists it wins over both legacy and home.
        std::fs::write(xdg.join("libra").join("config.db"), b"xdg").expect("xdg db");
        let xdg_resolution = global_config_resolution().expect("xdg resolution");
        assert_eq!(xdg_resolution.path, xdg.join("libra").join("config.db"));
        assert_eq!(xdg_resolution.source, GlobalConfigPathSource::XdgConfigHome);
        assert!(!xdg_resolution.migration_pending);

        // A relative XDG_CONFIG_HOME is ignored per the XDG basedir spec.
        let _xdg_relative = ScopedEnvVar::set("XDG_CONFIG_HOME", "relative-config");
        let relative = global_config_resolution().expect("relative xdg resolution");
        assert_eq!(relative.source, GlobalConfigPathSource::HomeConfig);
        assert_eq!(relative.path, new_config_dir.join("config.db"));

        // LIBRA_CONFIG_GLOBAL_DB wins verbatim and never migrates.
        let override_path = tmp.path().join("custom").join("config.db");
        let _override = ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &override_path);
        let overridden = global_config_resolution().expect("env resolution");
        assert_eq!(overridden.path, override_path);
        assert_eq!(overridden.source, GlobalConfigPathSource::EnvOverride);
        assert!(!overridden.migration_pending);
    }

    #[test]
    fn legacy_global_config_notice_mentions_both_paths_and_only_fires_when_pending() {
        let resolution = GlobalConfigResolution {
            path: std::path::PathBuf::from("/home/u/.libra/config.db"),
            source: GlobalConfigPathSource::LegacyHome,
            new_path: Some(std::path::PathBuf::from("/home/u/.config/libra/config.db")),
            new_exists: false,
            legacy_path: Some(std::path::PathBuf::from("/home/u/.libra/config.db")),
            legacy_exists: true,
            migration_pending: true,
        };
        let notice = legacy_global_config_notice(&resolution).expect("pending notice");
        assert!(
            notice.contains("/home/u/.libra/config.db")
                && notice.contains("/home/u/.config/libra/config.db"),
            "notice must name both paths: {notice}"
        );

        let migrated = GlobalConfigResolution {
            migration_pending: false,
            ..resolution
        };
        assert!(legacy_global_config_notice(&migrated).is_none());
    }

    /// FM-03 (ADR-FM-04, M-CFG K1/K6/K7): the pure `core.fileMode` resolver.
    #[test]
    fn resolve_core_file_mode_default_case_and_invalid() {
        // K1/K7: absent value defaults by platform.
        assert_eq!(super::resolve_core_file_mode(None), Ok(cfg!(unix)));
        // Case-insensitive Git booleans.
        for value in ["true", "TRUE", "Yes", "on", "1"] {
            assert_eq!(
                super::resolve_core_file_mode(Some(value)),
                Ok(true),
                "{value}"
            );
        }
        for value in ["false", "FALSE", "No", "off", "0"] {
            assert_eq!(
                super::resolve_core_file_mode(Some(value)),
                Ok(false),
                "{value}"
            );
        }
        // K6: invalid values fail closed with the commit.verbose message shape.
        assert_eq!(
            super::resolve_core_file_mode(Some("notabool")),
            Err("bad boolean config value 'notabool' for 'core.filemode'".to_string())
        );
    }
}

#[cfg(test)]
mod gpg_internal_key_tests {
    use super::is_vault_internal_key;

    /// plan-20260921 ADR-VG-02: the imported GPG secret slot is an internal
    /// credential on every read path, while the public metadata stays readable.
    #[test]
    fn vault_gpg_seckey_enc_is_internal_key() {
        for key in [
            "vault.gpg.seckey_enc",
            "VAULT.GPG.SECKEY_ENC",
            "Vault.Gpg.Seckey_Enc",
        ] {
            assert!(is_vault_internal_key(key), "{key} must be internal");
        }
        for key in [
            "vault.gpg.pubkey",
            "vault.gpg.source",
            "vault.gpg.fingerprint",
            "vault.gpg.signing_key_id",
            "vault.gpg.generated_key_name",
            "vault.gpg.generated_pubkey",
            "vault.gpg.history.ABC.pubkey",
            "vault.gpg.imported_at",
            "vault.gpg.uid",
        ] {
            assert!(!is_vault_internal_key(key), "{key} must stay readable");
        }
    }
}
