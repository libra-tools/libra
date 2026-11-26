//! Explicit repository-local issuer enrollment. Neither wire data nor a
//! catalog match grants receipt, field or artifact authority.

#[cfg(any(feature = "storage-sqlite", test))]
use std::time::Duration;
use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, bail};
use sea_orm::{ConnectionTrait, Statement};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{approved_build, attest, codec};
#[cfg(any(feature = "storage-sqlite", test))]
use crate::internal::db;
#[cfg(feature = "storage-sqlite")]
use crate::utils::util::DATABASE;
use crate::{
    internal::{
        ai::observed_agents::{coverage::parse_canon_value, trust},
        config::ConfigKv,
    },
    utils::util::try_get_request_paths,
};

pub const REGISTRY_KEY: &str = "agent.native_origin.opencode.v1";
pub const ISSUER_CAP: usize = 128;
pub const REGISTRY_BYTE_CAP: usize = 65536;

/// All diagnostics are fixed strings; untrusted descriptors and child output
/// are never interpolated into an error or a log.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NativeOriginError {
    #[error(
        "Invalid native-origin installation descriptor; use a new owned private installation with the reviewed producer"
    )]
    Descriptor,
    #[error(
        "OpenCode exporter trust is missing or changed; explicitly trust the approved private binary before registering an issuer"
    )]
    PriorTrust,
    #[error(
        "Native-origin observer binding differs from this Libra installation; enable the reviewed OpenCode observer and prepare a matching private installation"
    )]
    Observer,
    #[error(
        "Native-origin challenge failed; keep capture partial and check the approved private producer and its owned installation"
    )]
    Challenge,
    #[error(
        "Native-origin child exceeded its 30 second or output budget; keep capture partial and check the private producer"
    )]
    ChildBounds,
    #[error(
        "Invalid native-origin registry row, spelling, version or records; preserve the registry and repair its configuration before retrying"
    )]
    Registry,
    #[error(
        "Native-origin registry epoch changed; reread its epoch and retry the explicit registration with that expected epoch"
    )]
    Epoch,
    #[error(
        "Native-origin registry reached its 64 KiB or 128 issuer limit; preserve all revoked records and use a separately configured repository"
    )]
    Capacity,
    #[error(
        "Native-origin issuer already exists; do not reactivate an old key, prepare a new private installation for rotation"
    )]
    ExistingIssuer,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum IssuerState {
    Active,
    VerifyOnly,
    Revoked,
}

/// Persisted public key and local installation binding. These are still data:
/// later verification must check current trust, state and receipt coordinates.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IssuerRecord {
    pub issuer: String,
    pub public_key: String,
    pub key_generation: u32,
    pub build_identity: String,
    pub producer_version: String,
    pub producer_binary_digest: String,
    pub producer_path: String,
    pub policy_descriptor: codec::Policy,
    pub policy_digest: String,
    pub state: IssuerState,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub version: u32,
    pub epoch: u64,
    pub issuers: Vec<IssuerRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub epoch: u64,
    pub issuer: String,
    pub key_generation: u32,
    pub policy_digest: String,
    pub observer_executable_sha256: String,
}

/// Explicit revocation keeps every issuer tombstone. `removed` retains the
/// existing CLI meaning: whether an exporter trust entry was removed.
#[derive(Debug, Serialize)]
pub struct Revocation {
    pub removed: bool,
    pub native_revoked: usize,
    pub native_epoch: Option<u64>,
}

/// Minimal JSON spelling, without imposing sorted object-member order.
/// The producer's owned-file parser requires JSON.stringify(parsed) == input;
/// every number here is an integer and every string uses JSON's minimal
/// escaping. Serde then applies the closed field/type grammar to original bytes.
pub(super) fn decode_owned<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > REGISTRY_BYTE_CAP || parse_canon_value(bytes).is_err() {
        return Err(NativeOriginError::Descriptor.into());
    }
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => {
                let start = pos;
                pos += 1;
                loop {
                    let byte = *bytes.get(pos).ok_or(NativeOriginError::Descriptor)?;
                    pos += 1;
                    if byte == b'\\' {
                        pos += 1;
                    } else if byte == b'"' {
                        break;
                    }
                }
                let raw = bytes.get(start..pos).ok_or(NativeOriginError::Descriptor)?;
                let text: String =
                    serde_json::from_slice(raw).map_err(|_| NativeOriginError::Descriptor)?;
                if serde_json::to_vec(&text)? != raw {
                    return Err(NativeOriginError::Descriptor.into());
                }
            }
            byte if byte.is_ascii_whitespace() => {
                return Err(NativeOriginError::Descriptor.into());
            }
            b'-' | b'.' | b'+' => return Err(NativeOriginError::Descriptor.into()),
            b'0'..=b'9' => {
                let start = pos;
                while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                    pos += 1;
                }
                if bytes.get(pos).is_some_and(|b| matches!(b, b'e' | b'E'))
                    || (pos - start > 1 && bytes[start] == b'0')
                {
                    return Err(NativeOriginError::Descriptor.into());
                }
            }
            _ => pos += 1,
        }
    }
    serde_json::from_slice(bytes).map_err(|_| NativeOriginError::Descriptor.into())
}

pub(super) fn lower_hex(value: &str, bytes: usize) -> Result<Vec<u8>> {
    if value.len() != bytes * 2
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(NativeOriginError::Descriptor.into());
    }
    hex::decode(value).map_err(|_| NativeOriginError::Descriptor.into())
}

impl Registry {
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.epoch == 0 || self.issuers.is_empty() {
            return Err(NativeOriginError::Registry.into());
        }
        if self.issuers.len() > ISSUER_CAP {
            return Err(NativeOriginError::Capacity.into());
        }
        let mut seen = BTreeSet::new();
        let mut active = 0;
        for record in &self.issuers {
            let key = lower_hex(&record.public_key, 32)?;
            if record.issuer != hex::encode(Sha256::digest(&key))
                || !seen.insert(&record.issuer)
                || record.key_generation == 0
                || !Path::new(&record.producer_path).is_absolute()
                || record.producer_path.len() > 4096
                || record.producer_path.chars().any(char::is_control)
            {
                return Err(NativeOriginError::Registry.into());
            }
            let build = approved_build::lookup(
                &record.producer_version,
                &record.build_identity,
                &record.producer_binary_digest,
            )?;
            let rendered = build.render_observer(&record.policy_descriptor.libra_command_path)?;
            let expected = build.installation_policy(
                &record.policy_descriptor.libra_command_path,
                rendered.as_bytes(),
            )?;
            if record.policy_descriptor != expected
                || record.policy_digest
                    != hex::encode(Sha256::digest(codec::policy_bytes(&expected)?))
            {
                return Err(NativeOriginError::Registry.into());
            }
            if record.state == IssuerState::Active {
                active += 1;
            }
        }
        if active > 1 {
            return Err(NativeOriginError::Registry.into());
        }
        Ok(())
    }
}

/// Metadata is fetched before allocating a value. ASCII key aliases and
/// duplicate physical rows are refused, including a lone noncanonical alias.
/// Bound equality avoids the generic LIKE wildcard pitfall fixed by NPR-19.
pub async fn read_with_conn<C: ConnectionTrait>(conn: &C) -> Result<Option<Registry>> {
    let backend = conn.get_database_backend();
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT id,key,encrypted,length(CAST(value AS BLOB)) AS value_bytes FROM config_kv WHERE lower(key)=lower(?) ORDER BY id LIMIT 2",
            [REGISTRY_KEY.into()],
        ))
        .await
        .context("inspect reserved native-origin registry rows")?;
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1 {
        return Err(NativeOriginError::Registry.into());
    }
    let row = &rows[0];
    let id: i64 = row.try_get("", "id")?;
    let key: String = row.try_get("", "key")?;
    let encrypted: i32 = row.try_get("", "encrypted")?;
    let bytes: i64 = row.try_get("", "value_bytes")?;
    if key != REGISTRY_KEY || encrypted != 0 || bytes < 0 {
        return Err(NativeOriginError::Registry.into());
    }
    if bytes > REGISTRY_BYTE_CAP as i64 {
        return Err(NativeOriginError::Capacity.into());
    }
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT value FROM config_kv WHERE id=? AND key=? AND encrypted=0 AND length(CAST(value AS BLOB))<=?",
            [id.into(), REGISTRY_KEY.into(), (REGISTRY_BYTE_CAP as i64).into()],
        ))
        .await
        .context("read bounded native-origin registry value")?
        .ok_or(NativeOriginError::Registry)?;
    let value: String = row.try_get("", "value")?;
    let registry: Registry =
        decode_owned(value.as_bytes()).map_err(|_| NativeOriginError::Registry)?;
    registry.validate()?;
    Ok(Some(registry))
}

/// Backend-agnostic counterpart of [`read_with_conn`] using [`crate::storage::ConfigStore`].
pub async fn read_with_store(store: &dyn crate::storage::ConfigStore) -> Result<Option<Registry>> {
    let all_rows = store
        .list_all()
        .await
        .map_err(|err| anyhow::anyhow!("inspect reserved native-origin registry rows: {err}"))?;
    let matches: Vec<_> = all_rows
        .into_iter()
        .filter(|entry| entry.key.eq_ignore_ascii_case(REGISTRY_KEY))
        .collect();
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() != 1 {
        return Err(NativeOriginError::Registry.into());
    }
    let row = &matches[0];
    if row.key != REGISTRY_KEY || row.encrypted {
        return Err(NativeOriginError::Registry.into());
    }
    if row.value.len() > REGISTRY_BYTE_CAP {
        return Err(NativeOriginError::Capacity.into());
    }
    let registry: Registry =
        decode_owned(row.value.as_bytes()).map_err(|_| NativeOriginError::Registry)?;
    registry.validate()?;
    Ok(Some(registry))
}

/// Current permission data for a new native capture, import or pending replay.
/// The caller must independently measure approved executable provenance first,
/// then use this read on its permission snapshot connection. This returns
/// public installation data, never a native proof or an artifact capability.
/// Historical verify-only and revoked keys never authorize a new admission.
pub async fn active_issuer_with_conn(
    conn: &sea_orm::DatabaseTransaction,
    issuer: &str,
    provenance: &trust::Provenance,
) -> Result<Option<IssuerRecord>> {
    let Some(registry) = read_with_conn(conn).await? else {
        return Ok(None);
    };
    let Some(record) = registry
        .issuers
        .into_iter()
        .find(|record| record.issuer == issuer && record.state == IssuerState::Active)
    else {
        return Ok(None);
    };
    let Some(current) = trust::read_trust_with_conn(conn, trust::OPENCODE_EXPORTER_SLUG).await?
    else {
        return Ok(None);
    };
    let dirs = trust::read_trusted_dirs_with_conn(conn).await?;
    if current.path != Path::new(&record.producer_path)
        || current.sha256 != record.producer_binary_digest
        || trust::provenance_drifted(provenance, &current)
        || !trust::path_within_trusted_dirs(&current.path, &dirs)
    {
        return Ok(None);
    }
    Ok(Some(record))
}

/// Backend-agnostic counterpart of [`active_issuer_with_conn`] using [`crate::storage::ConfigStore`].
pub async fn active_issuer_with_store(
    store: &dyn crate::storage::ConfigStore,
    issuer: &str,
    provenance: &trust::Provenance,
) -> Result<Option<IssuerRecord>> {
    let Some(registry) = read_with_store(store).await? else {
        return Ok(None);
    };
    let Some(record) = registry
        .issuers
        .into_iter()
        .find(|record| record.issuer == issuer && record.state == IssuerState::Active)
    else {
        return Ok(None);
    };
    let Some(current) = trust::read_trust_with_store(store, trust::OPENCODE_EXPORTER_SLUG).await?
    else {
        return Ok(None);
    };
    let dirs = trust::read_trusted_dirs_with_store(store).await?;
    if current.path != Path::new(&record.producer_path)
        || current.sha256 != record.producer_binary_digest
        || trust::provenance_drifted(provenance, &current)
        || !trust::path_within_trusted_dirs(&current.path, &dirs)
    {
        return Ok(None);
    }
    Ok(Some(record))
}

#[cfg(any(feature = "storage-sqlite", test))]
async fn commit_revocation(conn: &sea_orm::DatabaseTransaction) -> Result<Revocation> {
    // Validate the bounded canonical row before any mutation. Both writes are
    // private to the existing write-lock-first transaction owned by `revoke`.
    let previous = read_with_conn(conn).await?;
    let removed = trust::revoke_trust_with_conn(conn, trust::OPENCODE_EXPORTER_SLUG).await?;
    let Some(mut registry) = previous else {
        return Ok(Revocation {
            removed,
            native_revoked: 0,
            native_epoch: None,
        });
    };
    let native_revoked = registry
        .issuers
        .iter()
        .filter(|record| record.state != IssuerState::Revoked)
        .count();
    if native_revoked > 0 {
        for record in &mut registry.issuers {
            record.state = IssuerState::Revoked;
        }
        registry.epoch = registry
            .epoch
            .checked_add(1)
            .ok_or(NativeOriginError::Epoch)?;
        registry.validate()?;
        let value = serde_json::to_vec(&registry).context("encode native-origin revocation")?;
        if value.len() > REGISTRY_BYTE_CAP {
            return Err(NativeOriginError::Capacity.into());
        }
        let text = std::str::from_utf8(&value).context("encode native-origin revocation text")?;
        ConfigKv::set_with_conn(conn, REGISTRY_KEY, text, false)
            .await
            .context("persist native-origin revocation on the same connection")?;
    }
    Ok(Revocation {
        removed,
        native_revoked,
        native_epoch: Some(registry.epoch),
    })
}

/// Backend-agnostic counterpart of [`revoke`] using [`crate::storage::ConfigStore`].
pub async fn revoke_with_store(store: &dyn crate::storage::ConfigStore) -> Result<Revocation> {
    let previous = read_with_store(store).await?;
    let removed = trust::revoke_trust_with_store(store, trust::OPENCODE_EXPORTER_SLUG).await?;
    let Some(mut registry) = previous else {
        return Ok(Revocation {
            removed,
            native_revoked: 0,
            native_epoch: None,
        });
    };
    let native_revoked = registry
        .issuers
        .iter()
        .filter(|record| record.state != IssuerState::Revoked)
        .count();
    if native_revoked > 0 {
        for record in &mut registry.issuers {
            record.state = IssuerState::Revoked;
        }
        registry.epoch = registry
            .epoch
            .checked_add(1)
            .ok_or(NativeOriginError::Epoch)?;
        registry.validate()?;
        let value = serde_json::to_vec(&registry).context("encode native-origin revocation")?;
        if value.len() > REGISTRY_BYTE_CAP {
            return Err(NativeOriginError::Capacity.into());
        }
        let text = std::str::from_utf8(&value).context("encode native-origin revocation text")?;
        ConfigKv::set_with_store(store, REGISTRY_KEY, text, false)
            .await
            .context("persist native-origin revocation in store")?;
    }
    Ok(Revocation {
        removed,
        native_revoked,
        native_epoch: Some(registry.epoch),
    })
}

/// Explicit combined OpenCode untrust. Automatic provenance revalidation still
/// removes binary trust alone; it suspends admission without revoking issuers.
pub async fn revoke() -> Result<Revocation> {
    let (storage, _, _) = try_get_request_paths(None)
        .context("native-origin revocation requires a Libra repository")?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|err| anyhow::anyhow!("detect repository storage backend: {err}"))?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_repo_config_store_read_write(&storage)
                    .await
                    .map_err(|err| {
                        anyhow::anyhow!("open repository store for native-origin revocation: {err}")
                    })?;
                revoke_with_store(store.as_ref()).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                bail!("this Libra binary was compiled without `storage-redb`");
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = db::get_db_conn_instance_for_path(&storage.join(DATABASE))
                    .await
                    .context("open repository for native-origin revocation")?;
                let txn = tokio::time::timeout(
                    Duration::from_secs(5),
                    db::begin_write_transaction(&conn),
                )
                .await
                .context("native-origin revocation writer is busy; retry the explicit untrust")?
                .context(
                    "acquire native-origin revocation writer lock; retry the explicit untrust",
                )?;
                match commit_revocation(&txn).await {
                    Ok(result) => {
                        txn.commit().await.context(
                            "commit native-origin revocation; inspect exporter trust and registry before retrying",
                        )?;
                        Ok(result)
                    }
                    Err(primary) => {
                        if let Err(rollback) = txn.rollback().await {
                            bail!(
                                "native-origin revocation refused: {primary}; rollback failed: {rollback}; inspect the repository before retrying"
                            );
                        }
                        Err(primary)
                    }
                }
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                bail!("this Libra binary was compiled without `storage-sqlite`");
            }
        }
    }
}

#[cfg(any(feature = "storage-sqlite", test))]
async fn commit_registration<C: ConnectionTrait>(
    conn: &C,
    prepared: &attest::PreparedRegistration,
    expected_epoch: u64,
) -> Result<Registration> {
    let current = trust::read_trust_with_conn(conn, trust::OPENCODE_EXPORTER_SLUG).await?;
    let dirs = trust::read_trusted_dirs_with_conn(conn).await?;
    if current.as_ref() != Some(&prepared.exporter_trust)
        || !trust::path_within_trusted_dirs(&prepared.exporter_trust.path, &dirs)
    {
        return Err(NativeOriginError::PriorTrust.into());
    }
    let previous = read_with_conn(conn).await?;
    let epoch = previous.as_ref().map_or(0, |registry| registry.epoch);
    if epoch != expected_epoch {
        return Err(NativeOriginError::Epoch.into());
    }
    let mut registry = previous.unwrap_or(Registry {
        version: 1,
        epoch: 0,
        issuers: Vec::new(),
    });
    if registry
        .issuers
        .iter()
        .any(|record| record.issuer == prepared.record.issuer)
    {
        return Err(NativeOriginError::ExistingIssuer.into());
    }
    if registry.issuers.len() >= ISSUER_CAP {
        return Err(NativeOriginError::Capacity.into());
    }
    for record in &mut registry.issuers {
        if record.state == IssuerState::Active {
            record.state = IssuerState::VerifyOnly;
        }
    }
    registry.epoch = epoch.checked_add(1).ok_or(NativeOriginError::Epoch)?;
    registry.issuers.push(prepared.record.clone());
    registry.validate()?;
    let value = serde_json::to_vec(&registry).context("encode complete native-origin registry")?;
    if value.len() > REGISTRY_BYTE_CAP {
        return Err(NativeOriginError::Capacity.into());
    }
    let text = std::str::from_utf8(&value).context("encode native-origin registry text")?;
    ConfigKv::set_with_conn(conn, REGISTRY_KEY, text, false)
        .await
        .context("persist native-origin registry on registration connection")?;
    Ok(Registration {
        epoch: registry.epoch,
        issuer: prepared.record.issuer.clone(),
        key_generation: prepared.record.key_generation,
        policy_digest: prepared.record.policy_digest.clone(),
        observer_executable_sha256: prepared.observer_executable_sha256.clone(),
    })
}

async fn commit_registration_with_store(
    store: &dyn crate::storage::ConfigStore,
    prepared: &attest::PreparedRegistration,
    expected_epoch: u64,
) -> Result<Registration> {
    let current = trust::read_trust_with_store(store, trust::OPENCODE_EXPORTER_SLUG).await?;
    let dirs = trust::read_trusted_dirs_with_store(store).await?;
    if current.as_ref() != Some(&prepared.exporter_trust)
        || !trust::path_within_trusted_dirs(&prepared.exporter_trust.path, &dirs)
    {
        return Err(NativeOriginError::PriorTrust.into());
    }
    let previous = read_with_store(store).await?;
    let epoch = previous.as_ref().map_or(0, |registry| registry.epoch);
    if epoch != expected_epoch {
        return Err(NativeOriginError::Epoch.into());
    }
    let mut registry = previous.unwrap_or(Registry {
        version: 1,
        epoch: 0,
        issuers: Vec::new(),
    });
    if registry
        .issuers
        .iter()
        .any(|record| record.issuer == prepared.record.issuer)
    {
        return Err(NativeOriginError::ExistingIssuer.into());
    }
    if registry.issuers.len() >= ISSUER_CAP {
        return Err(NativeOriginError::Capacity.into());
    }
    for record in &mut registry.issuers {
        if record.state == IssuerState::Active {
            record.state = IssuerState::VerifyOnly;
        }
    }
    registry.epoch = epoch.checked_add(1).ok_or(NativeOriginError::Epoch)?;
    registry.issuers.push(prepared.record.clone());
    registry.validate()?;
    let value = serde_json::to_vec(&registry).context("encode complete native-origin registry")?;
    if value.len() > REGISTRY_BYTE_CAP {
        return Err(NativeOriginError::Capacity.into());
    }
    let text = std::str::from_utf8(&value).context("encode native-origin registry text")?;
    ConfigKv::set_with_store(store, REGISTRY_KEY, text, false)
        .await
        .context("persist native-origin registry in store")?;
    Ok(Registration {
        epoch: registry.epoch,
        issuer: prepared.record.issuer.clone(),
        key_generation: prepared.record.key_generation,
        policy_digest: prepared.record.policy_digest.clone(),
        observer_executable_sha256: prepared.observer_executable_sha256.clone(),
    })
}

/// Backend-agnostic counterpart of [`register`] using [`crate::storage::ConfigStore`].
pub async fn register_with_store(
    store: &dyn crate::storage::ConfigStore,
    worktree: &Path,
    manifest: &Path,
    expected_epoch: u64,
) -> Result<Registration> {
    let before = read_with_store(store).await?;
    if before.as_ref().map_or(0, |registry| registry.epoch) != expected_epoch {
        return Err(NativeOriginError::Epoch.into());
    }
    let prepared = attest::prepare_with_store(store, worktree, manifest).await?;
    commit_registration_with_store(store, &prepared, expected_epoch).await
}

/// Explicit CLI owner. Handshake does not hold a database writer lock. The
/// final same-connection trust reread, epoch CAS and one row write linearize
/// only after the existing write-lock-first transaction has been acquired.
pub async fn register(manifest: &Path, expected_epoch: u64) -> Result<Registration> {
    let (storage, worktree, _) = try_get_request_paths(None)
        .context("native-origin registration requires a Libra repository")?;
    match crate::storage::runtime::detect_repo_backend(&storage)
        .map_err(|err| anyhow::anyhow!("detect repository storage backend: {err}"))?
    {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_repo_config_store_read_write(&storage)
                    .await
                    .map_err(|err| {
                        anyhow::anyhow!(
                            "open repository store for native-origin registration: {err}"
                        )
                    })?;
                register_with_store(store.as_ref(), &worktree, manifest, expected_epoch).await
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                bail!("this Libra binary was compiled without `storage-redb`");
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = db::get_db_conn_instance_for_path(&storage.join(DATABASE))
                    .await
                    .context("open repository for native-origin registration")?;
                // A preflight refusal avoids spawning a producer for corrupt state. This
                // read never substitutes for the transaction's authoritative reread.
                let before = read_with_conn(&conn).await?;
                if before.as_ref().map_or(0, |registry| registry.epoch) != expected_epoch {
                    return Err(NativeOriginError::Epoch.into());
                }
                let prepared = attest::prepare(&conn, &worktree, manifest).await?;
                let txn = tokio::time::timeout(
                    Duration::from_secs(5),
                    db::begin_write_transaction(&conn),
                )
                .await
                .context("native-origin registry writer is busy; reread the epoch and retry")?
                .context(
                    "acquire native-origin registry writer lock; reread the epoch and retry",
                )?;
                match commit_registration(&txn, &prepared, expected_epoch).await {
                    Ok(result) => {
                        txn.commit().await.context(
                            "commit native-origin registration; inspect the registry epoch before retrying",
                        )?;
                        Ok(result)
                    }
                    Err(primary) => {
                        if let Err(rollback) = txn.rollback().await {
                            bail!(
                                "native-origin registration refused: {primary}; rollback failed: {rollback}; inspect the repository before retrying"
                            );
                        }
                        Err(primary)
                    }
                }
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                bail!("this Libra binary was compiled without `storage-sqlite`");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{Database, DatabaseConnection, DbBackend};

    use super::*;

    async fn storage(url: &str) -> DatabaseConnection {
        let conn = Database::connect(url).await.unwrap();
        conn.execute_unprepared("PRAGMA busy_timeout=2000")
            .await
            .unwrap();
        conn.execute_unprepared("CREATE TABLE IF NOT EXISTS config_kv (id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL,value TEXT NOT NULL,encrypted INTEGER NOT NULL DEFAULT 0)").await.unwrap();
        conn
    }

    fn prepared() -> attest::PreparedRegistration {
        // Synthetic storage input only. It cannot pass the public registration
        // path, which measures an approved executable and verifies its challenge.
        let key = [7u8; 32];
        let build = approved_build::lookup(
            codec::PRODUCER_VERSION,
            "bac21cc4027896b8d1836133c347d20f",
            "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196",
        )
        .unwrap();
        let path = "/Applications/Libra.app/Contents/MacOS/libra";
        let rendered = build.render_observer(path).unwrap();
        let policy = build
            .installation_policy(path, rendered.as_bytes())
            .unwrap();
        attest::PreparedRegistration {
            exporter_trust: trust::TrustRecord {
                path: "/synthetic/producer/opencode".into(),
                sha256: "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196".into(),
                device: 1,
                inode: 1,
                mtime: 1,
            },
            record: IssuerRecord {
                issuer: hex::encode(Sha256::digest(key)),
                public_key: hex::encode(key),
                key_generation: 1,
                build_identity: "bac21cc4027896b8d1836133c347d20f".into(),
                producer_version: codec::PRODUCER_VERSION.into(),
                producer_binary_digest:
                    "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196".into(),
                producer_path: "/synthetic/producer/opencode".into(),
                policy_digest: hex::encode(Sha256::digest(codec::policy_bytes(&policy).unwrap())),
                policy_descriptor: policy,
                state: IssuerState::Active,
            },
            observer_executable_sha256: "a".repeat(64),
        }
    }

    async fn seed_trust(conn: &DatabaseConnection, input: &attest::PreparedRegistration) {
        ConfigKv::set_with_conn(
            conn,
            "agent.trust.opencode",
            &serde_json::to_string(&input.exporter_trust).unwrap(),
            false,
        )
        .await
        .unwrap();
        ConfigKv::set_with_conn(
            conn,
            trust::TRUSTED_DIRS_KEY,
            r#"["/synthetic/producer"]"#,
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn anthropic_rotation_keeps_old_issuer_verify_only_and_requires_new_key() {
        let conn = storage("sqlite::memory:").await;
        let original = prepared();
        seed_trust(&conn, &original).await;
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert_eq!(
            commit_registration(&txn, &original, 0).await.unwrap().epoch,
            1
        );
        txn.commit().await.unwrap();

        let build = approved_build::lookup(
            codec::ANTHROPIC_PRODUCER_VERSION,
            "78c1f19e9249da8d0143a079f90aec9a",
            "28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c",
        )
        .unwrap();
        let mut next = prepared();
        next.exporter_trust.path = "/synthetic/producer/opencode-anthropic".into();
        next.exporter_trust.sha256 = build.identity().binary_sha256.into();
        next.record.producer_path = next.exporter_trust.path.to_str().unwrap().into();
        next.record.producer_binary_digest = build.identity().binary_sha256.into();
        next.record.producer_version = build.identity().producer_version.into();
        next.record.build_identity = build.identity().build_identity.into();
        let command = "/Applications/Libra.app/Contents/MacOS/libra";
        let rendered = build.render_observer(command).unwrap();
        next.record.policy_descriptor = build
            .installation_policy(command, rendered.as_bytes())
            .unwrap();
        next.record.policy_digest = hex::encode(Sha256::digest(
            codec::policy_bytes(&next.record.policy_descriptor).unwrap(),
        ));
        next.record.public_key = hex::encode([8u8; 32]);
        next.record.issuer = hex::encode(Sha256::digest([8u8; 32]));
        seed_trust(&conn, &next).await;

        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert_eq!(commit_registration(&txn, &next, 1).await.unwrap().epoch, 2);
        txn.commit().await.unwrap();
        let registry = read_with_conn(&conn).await.unwrap().unwrap();
        assert_eq!(registry.issuers.len(), 2);
        assert_eq!(registry.issuers[0].state, IssuerState::VerifyOnly);
        assert_eq!(
            registry.issuers[0].producer_version,
            codec::PRODUCER_VERSION
        );
        assert_eq!(registry.issuers[1].state, IssuerState::Active);
        assert_eq!(
            registry.issuers[1].producer_version,
            codec::ANTHROPIC_PRODUCER_VERSION
        );
        next.record.public_key = original.record.public_key.clone();
        next.record.issuer = original.record.issuer.clone();
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert_eq!(
            commit_registration(&txn, &next, 2)
                .await
                .unwrap_err()
                .downcast_ref(),
            Some(&NativeOriginError::ExistingIssuer)
        );
        txn.rollback().await.unwrap();
        assert_eq!(read_with_conn(&conn).await.unwrap().unwrap(), registry);
    }

    #[test]
    fn npr_09_owned_json_grammar() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            version: u32,
            text: String,
        }
        let reordered: Fields = decode_owned(br#"{"text":"unchanged","version":1}"#).unwrap();
        assert_eq!(
            reordered,
            Fields {
                version: 1,
                text: "unchanged".into()
            }
        );
        for wire in [
            br#"{"version":1,"text":"x","text":"y"}"#.as_slice(),
            br#"{"version":1,"te\u0078t":"x"}"#,
            br#"{"version":1.0,"text":"x"}"#,
            br#"{"version":1e0,"text":"x"}"#,
            br#"{"version":-0,"text":"x"}"#,
            br#"{"version":1, "text":"x"}"#,
            br#"{"version":1,"text":"\u0078"}"#,
            br#"{"version":1,"text":"x","extra":0}"#,
            b"{\"version\":1,\"text\":\"x\"}\n",
        ] {
            assert!(
                decode_owned::<Fields>(wire).is_err(),
                "invalid owned spelling must fail"
            );
        }
        assert!(decode_owned::<Fields>(&vec![b' '; 65537]).is_err());
    }

    #[tokio::test]
    async fn npr_09_registry_storage_guards() {
        let conn = storage("sqlite::memory:").await;
        assert!(read_with_conn(&conn).await.unwrap().is_none());
        let input = prepared();
        let registry = Registry {
            version: 1,
            epoch: 1,
            issuers: vec![input.record],
        };
        let valid = serde_json::to_string(&registry).unwrap();
        for (keys, value, encrypted) in [
            (vec![REGISTRY_KEY, REGISTRY_KEY], valid.as_str(), 0),
            (
                vec![REGISTRY_KEY, "AGENT.NATIVE_ORIGIN.OPENCODE.V1"],
                valid.as_str(),
                0,
            ),
            (vec!["Agent.Native_Origin.Opencode.V1"], valid.as_str(), 0),
            (vec![REGISTRY_KEY], valid.as_str(), 1),
            (vec![REGISTRY_KEY], "{}", 0),
            (vec![REGISTRY_KEY], "not json", 0),
        ] {
            conn.execute_unprepared("DELETE FROM config_kv")
                .await
                .unwrap();
            for key in &keys {
                conn.execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "INSERT INTO config_kv(key,value,encrypted) VALUES (?,?,?)",
                    [(*key).into(), value.into(), encrypted.into()],
                ))
                .await
                .unwrap();
            }
            assert!(read_with_conn(&conn).await.is_err());
            let rows = conn
                .query_all_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    "SELECT key,value,encrypted FROM config_kv ORDER BY id",
                ))
                .await
                .unwrap();
            assert_eq!(rows.len(), keys.len());
            for (row, key) in rows.iter().zip(keys) {
                assert_eq!(row.try_get::<String>("", "key").unwrap(), key);
                assert_eq!(row.try_get::<String>("", "value").unwrap(), value);
                assert_eq!(row.try_get::<i32>("", "encrypted").unwrap(), encrypted);
            }
        }
        conn.execute_unprepared("DELETE FROM config_kv")
            .await
            .unwrap();
        ConfigKv::set_with_conn(&conn, REGISTRY_KEY, &valid, false)
            .await
            .unwrap();
        assert_eq!(read_with_conn(&conn).await.unwrap(), Some(registry));
        ConfigKv::set_with_conn(&conn, REGISTRY_KEY, &"x".repeat(65537), false)
            .await
            .unwrap();
        assert_eq!(
            read_with_conn(&conn).await.unwrap_err().downcast_ref(),
            Some(&NativeOriginError::Capacity)
        );
        ConfigKv::set_with_conn(
            &conn,
            "agent.trust.opencode",
            r#"{"mtime":"sensitive-test-canary"}"#,
            false,
        )
        .await
        .unwrap();
        let error = trust::read_trust_with_conn(&conn, "opencode")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("corrupt"));
        assert!(
            !error.contains("sensitive-test-canary"),
            "invalid trust JSON must not echo a wire value"
        );
    }

    #[tokio::test]
    async fn npr_09_linearized_admission() {
        let root = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite:{}?mode=rwc",
            root.path().join("registry.db").display()
        );
        let conn = storage(&url).await;
        let other = storage(&url).await;
        let input = prepared();
        seed_trust(&conn, &input).await;
        ConfigKv::set_with_conn(&conn, "canary.untouched", "preserve", false)
            .await
            .unwrap();
        // Separate real connections contend for the same writer lock. The
        // second expected-0 writer sees the first committed epoch, not a stale
        // outside-transaction read or a nested pooled connection.
        let winner = db::begin_write_transaction(&conn).await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let loser = tokio::spawn(async move {
            let input = prepared();
            started_tx.send(()).unwrap();
            let txn = db::begin_write_transaction(&other).await.unwrap();
            let result = commit_registration(&txn, &input, 0).await;
            txn.rollback().await.unwrap();
            result
        });
        started_rx.await.unwrap();
        assert_eq!(
            commit_registration(&winner, &input, 0).await.unwrap().epoch,
            1
        );
        winner.commit().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), loser)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.downcast_ref(), Some(&NativeOriginError::Epoch));
        let before = ConfigKv::get_with_conn(&conn, REGISTRY_KEY)
            .await
            .unwrap()
            .unwrap()
            .value;
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        trust::revoke_trust_with_conn(&txn, trust::OPENCODE_EXPORTER_SLUG)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert_eq!(
            commit_registration(&txn, &input, 1)
                .await
                .unwrap_err()
                .downcast_ref(),
            Some(&NativeOriginError::PriorTrust)
        );
        txn.rollback().await.unwrap();
        assert_eq!(
            ConfigKv::get_with_conn(&conn, REGISTRY_KEY)
                .await
                .unwrap()
                .unwrap()
                .value,
            before
        );
        assert_eq!(
            ConfigKv::get_with_conn(&conn, "canary.untouched")
                .await
                .unwrap()
                .unwrap()
                .value,
            "preserve"
        );
        seed_trust(&conn, &input).await;
        conn.execute_unprepared("CREATE TRIGGER reject_native_update BEFORE UPDATE ON config_kv WHEN NEW.key='agent.native_origin.opencode.v1' BEGIN SELECT RAISE(ABORT,'synthetic write fault'); END").await.unwrap();
        let mut new = prepared();
        new.record.public_key = hex::encode([8u8; 32]);
        new.record.issuer = hex::encode(Sha256::digest([8u8; 32]));
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert!(commit_registration(&txn, &new, 1).await.is_err());
        txn.rollback().await.unwrap();
        assert_eq!(
            ConfigKv::get_with_conn(&conn, REGISTRY_KEY)
                .await
                .unwrap()
                .unwrap()
                .value,
            before
        );
    }

    #[tokio::test]
    async fn npr_10_revocation_atomicity_and_tombstones() {
        // Synthetic prepared data checks storage mechanics only; it cannot
        // pass public challenge enrollment or confer native-field authority.
        let conn = storage("sqlite::memory:").await;
        let input = prepared();
        seed_trust(&conn, &input).await;
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        commit_registration(&txn, &input, 0).await.unwrap();
        txn.commit().await.unwrap();
        let original = ConfigKv::get_with_conn(&conn, REGISTRY_KEY)
            .await
            .unwrap()
            .unwrap()
            .value;
        let original_trust = trust::read_trust_with_conn(&conn, "opencode")
            .await
            .unwrap();
        conn.execute_unprepared("CREATE TRIGGER reject_revoke BEFORE UPDATE ON config_kv WHEN NEW.key='agent.native_origin.opencode.v1' BEGIN SELECT RAISE(ABORT,'synthetic revocation fault'); END").await.unwrap();
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert!(commit_revocation(&txn).await.is_err());
        txn.rollback().await.unwrap();
        assert_eq!(
            trust::read_trust_with_conn(&conn, "opencode")
                .await
                .unwrap(),
            original_trust
        );
        assert_eq!(
            ConfigKv::get_with_conn(&conn, REGISTRY_KEY)
                .await
                .unwrap()
                .unwrap()
                .value,
            original
        );
        conn.execute_unprepared("DROP TRIGGER reject_revoke")
            .await
            .unwrap();
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        let result = commit_revocation(&txn).await.unwrap();
        assert!(result.removed);
        assert_eq!(result.native_revoked, 1);
        assert_eq!(result.native_epoch, Some(2));
        txn.commit().await.unwrap();
        let tombstone = read_with_conn(&conn).await.unwrap().unwrap();
        assert_eq!(tombstone.issuers[0].state, IssuerState::Revoked);
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        let repeated = commit_revocation(&txn).await.unwrap();
        assert!(!repeated.removed);
        assert_eq!(repeated.native_revoked, 0);
        assert_eq!(repeated.native_epoch, Some(2));
        txn.commit().await.unwrap();
        assert_eq!(read_with_conn(&conn).await.unwrap().unwrap(), tombstone);
        seed_trust(&conn, &input).await;
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert_eq!(
            commit_registration(&txn, &input, 2)
                .await
                .unwrap_err()
                .downcast_ref(),
            Some(&NativeOriginError::ExistingIssuer)
        );
        txn.rollback().await.unwrap();
        assert_eq!(read_with_conn(&conn).await.unwrap().unwrap(), tombstone);
    }

    #[tokio::test]
    async fn npr_10_real_connections_linearize_both_orders() {
        let root = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite:{}?mode=rwc",
            root.path().join("lifecycle.db").display()
        );
        let conn = storage(&url).await;
        let other = storage(&url).await;
        let input = prepared();
        seed_trust(&conn, &input).await;
        let initial = db::begin_write_transaction(&conn).await.unwrap();
        commit_registration(&initial, &input, 0).await.unwrap();
        initial.commit().await.unwrap();
        // Untrust wins first; a competing real connection cannot reuse its
        // pre-lock snapshot. Inputs are private synthetic storage fixtures.
        let winner = db::begin_write_transaction(&conn).await.unwrap();
        commit_revocation(&winner).await.unwrap();
        let (send, recv) = tokio::sync::oneshot::channel();
        let contender = other.clone();
        let losing_registration = tokio::spawn(async move {
            send.send(()).unwrap();
            let txn = db::begin_write_transaction(&contender).await.unwrap();
            let result = commit_registration(&txn, &prepared(), 1).await;
            txn.rollback().await.unwrap();
            result
        });
        recv.await.unwrap();
        winner.commit().await.unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(5), losing_registration)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.downcast_ref(), Some(&NativeOriginError::PriorTrust));
        assert_eq!(read_with_conn(&conn).await.unwrap().unwrap().epoch, 2);
        // Registration wins first; queued untrust must see the new key and
        // revoke it while preserving the earlier tombstone.
        seed_trust(&conn, &input).await;
        let mut fresh = prepared();
        fresh.record.public_key = hex::encode([8u8; 32]);
        fresh.record.issuer = hex::encode(Sha256::digest([8u8; 32]));
        let winner = db::begin_write_transaction(&conn).await.unwrap();
        commit_registration(&winner, &fresh, 2).await.unwrap();
        let (send, recv) = tokio::sync::oneshot::channel();
        let queued_revocation = tokio::spawn(async move {
            send.send(()).unwrap();
            let txn = db::begin_write_transaction(&other).await.unwrap();
            let result = commit_revocation(&txn).await.unwrap();
            txn.commit().await.unwrap();
            result
        });
        recv.await.unwrap();
        winner.commit().await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), queued_revocation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.native_epoch, Some(4));
        assert_eq!(result.native_revoked, 1);
        let registry = read_with_conn(&conn).await.unwrap().unwrap();
        assert_eq!(registry.issuers.len(), 2);
        assert!(
            registry
                .issuers
                .iter()
                .all(|r| r.state == IssuerState::Revoked)
        );
        assert!(
            trust::read_trust_with_conn(&conn, "opencode")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn npr_10_record_ceiling_preserves_tombstones() {
        // Pure storage grammar, not synthetic-key enrollment. The independent
        // 64 KiB cap can bind before 128 records for the reviewed profile.
        let base = prepared().record;
        let mut records = Vec::new();
        for n in 0..ISSUER_CAP {
            let mut record = base.clone();
            record.public_key = hex::encode([n as u8; 32]);
            record.issuer = hex::encode(Sha256::digest([n as u8; 32]));
            record.state = IssuerState::Revoked;
            records.push(record);
        }
        let mut registry = Registry {
            version: 1,
            epoch: 128,
            issuers: records,
        };
        registry.validate().unwrap();
        let mut extra = base;
        extra.public_key = hex::encode([128u8; 32]);
        extra.issuer = hex::encode(Sha256::digest([128u8; 32]));
        extra.state = IssuerState::Revoked;
        registry.issuers.push(extra);
        assert_eq!(
            registry.validate().unwrap_err().downcast_ref(),
            Some(&NativeOriginError::Capacity)
        );
        assert_eq!(
            registry.issuers.len(),
            129,
            "no repair may silently remove tombstones"
        );
    }

    #[test]
    fn npr_09_error_display() {
        assert_eq!(
            NativeOriginError::Epoch.to_string(),
            "Native-origin registry epoch changed; reread its epoch and retry the explicit registration with that expected epoch"
        );
        assert_eq!(
            NativeOriginError::PriorTrust.to_string(),
            "OpenCode exporter trust is missing or changed; explicitly trust the approved private binary before registering an issuer"
        );
        assert_eq!(
            NativeOriginError::ChildBounds.to_string(),
            "Native-origin child exceeded its 30 second or output budget; keep capture partial and check the private producer"
        );
        assert_eq!(
            NativeOriginError::Registry.to_string(),
            "Invalid native-origin registry row, spelling, version or records; preserve the registry and repair its configuration before retrying"
        );
        assert_eq!(
            NativeOriginError::Descriptor.to_string(),
            "Invalid native-origin installation descriptor; use a new owned private installation with the reviewed producer"
        );
        assert_eq!(
            NativeOriginError::Observer.to_string(),
            "Native-origin observer binding differs from this Libra installation; enable the reviewed OpenCode observer and prepare a matching private installation"
        );
        assert_eq!(
            NativeOriginError::Challenge.to_string(),
            "Native-origin challenge failed; keep capture partial and check the approved private producer and its owned installation"
        );
        assert_eq!(
            NativeOriginError::Capacity.to_string(),
            "Native-origin registry reached its 64 KiB or 128 issuer limit; preserve all revoked records and use a separately configured repository"
        );
        assert_eq!(
            NativeOriginError::ExistingIssuer.to_string(),
            "Native-origin issuer already exists; do not reactivate an old key, prepare a new private installation for rotation"
        );
    }

    // Frozen public data from signed6685b82a's 9c672 legacy recipe, never from
    // the current catalog's render_observer/installation_policy. The first two
    // policies match its independent byte/digest vectors; the .2 policies use
    // its frozen public protocols/endpoints with the same rendered digests,
    // independently encoded before this regression was added. Keys and paths
    // below are synthetic data only: no registration, authority or Source A.
    // Tuple: closed old registry wire, rejected cd53 render digest/policy digest.
    const LEGACY_REGISTRY_ORIGINAL_RECIPE_GOLD: [(&str, &str, &str); 4] = [
        (
            r#"{"version":1,"epoch":7,"issuers":[{"issuer":"4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0","publicKey":"0707070707070707070707070707070707070707070707070707070707070707","keyGeneration":1,"buildIdentity":"bac21cc4027896b8d1836133c347d20f","producerVersion":"2.0.26-libra-native.1","producerBinaryDigest":"547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196","producerPath":"/Applications/LibraRegistryFixture.app/Contents/MacOS/opencode","policyDescriptor":{"version":1,"buildIdentity":"bac21cc4027896b8d1836133c347d20f","protocols":["openai-responses"],"endpointDigests":["061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531","253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3"],"builtinSetDigest":"137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82","libraTemplateDigest":"9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d","eventMapDigest":"3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe","exportDeadlineMillis":30000,"transportPolicy":"compiled-native-http-only-v1","libraCommandPath":"/Applications/Libra.app/Contents/MacOS/libra","libraRenderedDigest":"e116b0f36e73bc9b0201e3ed0e65743205193233d46f4077a5ffa8a008d6ec6b"},"policyDigest":"92ca0b7127e92865cfee1ad34b82c4deff63c18cfb8e80514311820ccd067fa5","state":"active"}]}"#,
            "23c03c773635b1015401c16dfcb240bbf36e698e3e3b45dcfe078639890b5b57",
            "435447c7779c8b028c62ca4adc89ea08490c1bad907cc106f8abf1caf77cf52d",
        ),
        (
            r#"{"version":1,"epoch":7,"issuers":[{"issuer":"4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0","publicKey":"0707070707070707070707070707070707070707070707070707070707070707","keyGeneration":1,"buildIdentity":"bac21cc4027896b8d1836133c347d20f","producerVersion":"2.0.26-libra-native.1","producerBinaryDigest":"547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196","producerPath":"/Applications/LibraRegistryFixture.app/Contents/MacOS/opencode","policyDescriptor":{"version":1,"buildIdentity":"bac21cc4027896b8d1836133c347d20f","protocols":["openai-responses"],"endpointDigests":["061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531","253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3"],"builtinSetDigest":"137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82","libraTemplateDigest":"9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d","eventMapDigest":"3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe","exportDeadlineMillis":30000,"transportPolicy":"compiled-native-http-only-v1","libraCommandPath":"/opt/homebrew/bin/libra","libraRenderedDigest":"be742723a06b5b26357d5c4a935eca2ee9caf0e377596afcac7605b9cd9938ac"},"policyDigest":"368d6c66e9c605a3989ba815aeb638e78d33834f73dd7192d62337274b1d5753","state":"active"}]}"#,
            "62b7798108e8f89839dc2a02663656926fbfe0a291e975c67297e996a665cdd8",
            "e7038ef1817ba77337f4d6c95b9f7462fcc747f9e5f2b2b4076b8986c6275234",
        ),
        (
            r#"{"version":1,"epoch":7,"issuers":[{"issuer":"4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0","publicKey":"0707070707070707070707070707070707070707070707070707070707070707","keyGeneration":1,"buildIdentity":"78c1f19e9249da8d0143a079f90aec9a","producerVersion":"2.0.26-libra-native.2","producerBinaryDigest":"28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c","producerPath":"/Applications/LibraRegistryFixture.app/Contents/MacOS/opencode","policyDescriptor":{"version":1,"buildIdentity":"78c1f19e9249da8d0143a079f90aec9a","protocols":["anthropic-messages","openai-responses"],"endpointDigests":["061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531","253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3","4d87d389c2853ef2ee92e297146b3576d59c343900f70d099f6ed977573bcae0"],"builtinSetDigest":"137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82","libraTemplateDigest":"9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d","eventMapDigest":"3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe","exportDeadlineMillis":30000,"transportPolicy":"compiled-native-http-only-v1","libraCommandPath":"/Applications/Libra.app/Contents/MacOS/libra","libraRenderedDigest":"e116b0f36e73bc9b0201e3ed0e65743205193233d46f4077a5ffa8a008d6ec6b"},"policyDigest":"794e9116fe4c244f6f553168f31a8cb54d9d004e66df274b15e4e7b49939f16a","state":"active"}]}"#,
            "23c03c773635b1015401c16dfcb240bbf36e698e3e3b45dcfe078639890b5b57",
            "a886b39e5f3de6e8fb35bdc95ed9930af34bd401a376920e4ac2db8629427b32",
        ),
        (
            r#"{"version":1,"epoch":7,"issuers":[{"issuer":"4bb06f8e4e3a7715d201d573d0aa423762e55dabd61a2c02278fa56cc6d294e0","publicKey":"0707070707070707070707070707070707070707070707070707070707070707","keyGeneration":1,"buildIdentity":"78c1f19e9249da8d0143a079f90aec9a","producerVersion":"2.0.26-libra-native.2","producerBinaryDigest":"28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c","producerPath":"/Applications/LibraRegistryFixture.app/Contents/MacOS/opencode","policyDescriptor":{"version":1,"buildIdentity":"78c1f19e9249da8d0143a079f90aec9a","protocols":["anthropic-messages","openai-responses"],"endpointDigests":["061f0c9e9a7b58c124b2282e620b1d01483350b246d38a4a2017002f13878531","253ec2c2c92c5e0b154de256a3cf1d1ac27f2f03c0119312229d2fe66b9b4cd3","4d87d389c2853ef2ee92e297146b3576d59c343900f70d099f6ed977573bcae0"],"builtinSetDigest":"137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82","libraTemplateDigest":"9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d","eventMapDigest":"3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe","exportDeadlineMillis":30000,"transportPolicy":"compiled-native-http-only-v1","libraCommandPath":"/opt/homebrew/bin/libra","libraRenderedDigest":"be742723a06b5b26357d5c4a935eca2ee9caf0e377596afcac7605b9cd9938ac"},"policyDigest":"3593923e7f7e5b86048bc192a66b3b75a7872fe97891846747b94f34dbd867d8","state":"active"}]}"#,
            "62b7798108e8f89839dc2a02663656926fbfe0a291e975c67297e996a665cdd8",
            "8f804c39f6ca01a752fcf04e6e531a3f9e6dbe7fc97b88444a1627bcf7bb6e57",
        ),
    ];

    fn legacy_registry_original_recipe_wire(index: usize, state: &str) -> Vec<u8> {
        let mut wire: serde_json::Value =
            serde_json::from_str(LEGACY_REGISTRY_ORIGINAL_RECIPE_GOLD[index].0).unwrap();
        wire["issuers"][0]["state"] = serde_json::json!(state);
        serde_json::to_vec(&wire).unwrap()
    }

    #[test]
    fn legacy_registry_original_recipe_rows_remain_readable() {
        // read_with_conn uses this exact closed decoder and validator after its
        // storage guards. No DB, current trust, challenge or state change occurs.
        for index in 0..LEGACY_REGISTRY_ORIGINAL_RECIPE_GOLD.len() {
            for (state_name, state) in [
                ("active", IssuerState::Active),
                ("verify-only", IssuerState::VerifyOnly),
                ("revoked", IssuerState::Revoked),
            ] {
                let wire = legacy_registry_original_recipe_wire(index, state_name);
                let registry: Registry = decode_owned(&wire).unwrap();
                let before = registry.clone();
                let record = &registry.issuers[0];
                assert_eq!(registry.version, 1);
                assert_eq!(registry.epoch, 7);
                assert_eq!(record.state, state);
                assert_eq!(
                    hex::encode(Sha256::digest(
                        codec::policy_bytes(&record.policy_descriptor).unwrap()
                    )),
                    record.policy_digest,
                    "independently frozen policy digest, case {index}"
                );
                registry.validate().unwrap_or_else(|error| {
                    panic!("original legacy policy case {index}/{state_name}: {error}")
                });
                assert_eq!(registry, before, "read validation must retain every state");
            }
        }
    }

    #[test]
    fn legacy_registry_original_recipe_binding_tamper_is_rejected() {
        for (index, gold) in LEGACY_REGISTRY_ORIGINAL_RECIPE_GOLD.iter().enumerate() {
            let wire = legacy_registry_original_recipe_wire(index, "revoked");
            let original: Registry = decode_owned(&wire).unwrap();
            original.validate().unwrap();

            let mut extra: serde_json::Value = serde_json::from_slice(&wire).unwrap();
            extra["issuers"][0]["unreviewedField"] = serde_json::json!(true);
            assert!(decode_owned::<Registry>(&serde_json::to_vec(&extra).unwrap()).is_err());

            let mut wrong_command = original.clone();
            wrong_command.issuers[0]
                .policy_descriptor
                .libra_command_path = "/Applications/OtherLibra.app/Contents/MacOS/libra".into();
            assert!(wrong_command.validate().is_err());

            let mut wrong_digest = original.clone();
            wrong_digest.issuers[0]
                .policy_digest
                .replace_range(0..1, "0");
            assert!(wrong_digest.validate().is_err());

            // Self-consistent policy bytes from the unreleased cd53 template
            // remain invalid for these immutable old producer identities.
            let mut current_template = original.clone();
            let record = &mut current_template.issuers[0];
            record.policy_descriptor.libra_template_digest =
                "cd53b2535c3dccd147ea851c8089c405ea84fcfaf2237e4a8284969520bdfdbd".into();
            record.policy_descriptor.libra_rendered_digest = gold.1.into();
            record.policy_digest = gold.2.into();
            assert_eq!(
                hex::encode(Sha256::digest(
                    codec::policy_bytes(&record.policy_descriptor).unwrap()
                )),
                record.policy_digest
            );
            assert!(current_template.validate().is_err());

            let other_wire = legacy_registry_original_recipe_wire((index + 2) % 4, "revoked");
            let other: Registry = decode_owned(&other_wire).unwrap();
            let mut crossed = original.clone();
            crossed.issuers[0].policy_descriptor = other.issuers[0].policy_descriptor.clone();
            crossed.issuers[0].policy_digest = other.issuers[0].policy_digest.clone();
            assert!(crossed.validate().is_err());
            assert_eq!(original.issuers[0].state, IssuerState::Revoked);
            assert_eq!(original.epoch, 7);
        }
    }
}
