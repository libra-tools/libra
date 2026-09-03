#![allow(
    dead_code,
    reason = "M2-01K lands the keyed-digest owner before later Memory writers consume it"
)]

//! Repository-scoped keyed digests for Agent Memory.
//!
//! Callers choose one closed [`DigestPurpose`] and pass bytes. This module
//! owns repository pinning, encrypted seed persistence, domain separation,
//! first-writer-wins initialization, and the process cache. It never exposes
//! the repository seed, derived HMAC keys, or custom HKDF labels.

use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use once_cell::sync::Lazy;
use ring::{digest, hkdf, hmac};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    internal::{
        config::{ConfigKv, MEMORY_KEYED_DIGEST_CONFIG_KEY},
        db, vault,
        workspace::RepoIdentity,
    },
    utils::error::StableErrorCode,
};

const DIGEST_VERSION: u8 = 1;
const DERIVED_KEY_BYTES: usize = 32;
const HKDF_SALT: &[u8] = b"libra/memory/keyed-digest/salt/v1";
const MEMORY_REF_PREFIX: &str = "libra/memory/";
const RECEIPT_TABLE: &str = "context_selection_receipt";
const PERSISTED_SCHEMA_VERSION: u8 = 1;
const PERSISTED_GENERATION: u32 = 1;
const PROCESS_CACHE_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RepositoryCacheKey {
    canonical_db_path: PathBuf,
    repository_id: String,
}

static REPOSITORY_KEYED_DIGEST_CACHE: Lazy<
    Mutex<HashMap<RepositoryCacheKey, Arc<RepositoryKeyedDigest>>>,
> = Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
enum DigestLoadMode {
    LoadOrInitialize,
    ExistingOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DigestPurpose {
    Idempotency,
    Principal,
    Query,
    SourceInput,
}

impl DigestPurpose {
    const fn info(self) -> &'static [u8] {
        match self {
            Self::Idempotency => b"libra/memory/idempotency/v1",
            Self::Principal => b"libra/memory/principal/v1",
            Self::Query => b"libra/memory/query/v1",
            Self::SourceInput => b"libra/memory/source-input/v1",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Idempotency => 0,
            Self::Principal => 1,
            Self::Query => 2,
            Self::SourceInput => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KeyedDigestEnvelope {
    version: u8,
    key_id: Uuid,
    purpose: DigestPurpose,
    digest: String,
}

impl KeyedDigestEnvelope {
    pub(crate) const fn key_id(&self) -> Uuid {
        self.key_id
    }

    pub(crate) const fn purpose(&self) -> DigestPurpose {
        self.purpose
    }

    pub(crate) const fn version(&self) -> u8 {
        self.version
    }

    pub(crate) fn digest_hex(&self) -> &str {
        &self.digest
    }
}

/// Purpose-locked digest for an authenticated principal written to a context
/// selection receipt.
///
/// This wrapper deliberately does not implement `Debug` or serialization so a
/// caller cannot accidentally log it or persist a generic digest envelope.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PrincipalDigest(KeyedDigestEnvelope);

impl PrincipalDigest {
    pub(crate) const fn version(&self) -> u8 {
        self.0.version()
    }

    pub(crate) const fn key_id(&self) -> Uuid {
        self.0.key_id()
    }

    pub(crate) fn digest_hex(&self) -> &str {
        self.0.digest_hex()
    }

    pub(crate) fn encoded(&self) -> String {
        encode_receipt_digest(&self.0)
    }
}

/// Purpose-locked digest for normalized retrieval inputs written to a context
/// selection receipt.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct QueryDigest(KeyedDigestEnvelope);

impl QueryDigest {
    pub(crate) const fn version(&self) -> u8 {
        self.0.version()
    }

    pub(crate) const fn key_id(&self) -> Uuid {
        self.0.key_id()
    }

    pub(crate) fn digest_hex(&self) -> &str {
        self.0.digest_hex()
    }

    pub(crate) fn encoded(&self) -> String {
        encode_receipt_digest(&self.0)
    }
}

fn encode_receipt_digest(envelope: &KeyedDigestEnvelope) -> String {
    format!(
        "hmac-sha256:{}:{}",
        envelope.key_id(),
        envelope.digest_hex()
    )
}

/// Purpose-locked fingerprint for a compiler root's canonical source inputs.
///
/// The wrapper deliberately has no `Debug` or serialization implementation:
/// job persistence writes its three validated parts explicitly, and routine
/// diagnostics must not print the digest.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceInputFingerprint(KeyedDigestEnvelope);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SourceInputFingerprintErrorKind {
    UnsupportedVersion,
    InvalidKeyId,
    InvalidDigest,
}

#[derive(Debug, Error)]
#[error("persisted source-input fingerprint is invalid ({kind:?})")]
pub(crate) struct SourceInputFingerprintError {
    kind: SourceInputFingerprintErrorKind,
}

impl SourceInputFingerprintError {
    const fn new(kind: SourceInputFingerprintErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> SourceInputFingerprintErrorKind {
        self.kind
    }
}

impl SourceInputFingerprint {
    pub(crate) fn from_parts(
        version: u8,
        key_id: Uuid,
        digest: String,
    ) -> Result<Self, SourceInputFingerprintError> {
        if version != DIGEST_VERSION {
            return Err(SourceInputFingerprintError::new(
                SourceInputFingerprintErrorKind::UnsupportedVersion,
            ));
        }
        if key_id.get_version_num() != 4 {
            return Err(SourceInputFingerprintError::new(
                SourceInputFingerprintErrorKind::InvalidKeyId,
            ));
        }
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SourceInputFingerprintError::new(
                SourceInputFingerprintErrorKind::InvalidDigest,
            ));
        }

        Ok(Self(KeyedDigestEnvelope {
            version,
            key_id,
            purpose: DigestPurpose::SourceInput,
            digest,
        }))
    }

    pub(crate) const fn version(&self) -> u8 {
        self.0.version()
    }

    pub(crate) const fn key_id(&self) -> Uuid {
        self.0.key_id()
    }

    pub(crate) fn digest_hex(&self) -> &str {
        self.0.digest_hex()
    }

    pub(crate) fn encoded(&self) -> String {
        encode_receipt_digest(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyedDigestErrorKind {
    RepositoryUnavailable,
    RepositoryIdentityInvalid,
    StateQueryFailed,
    MissingAfterDurableUse,
    PlaintextConfig,
    DuplicateConfig,
    VaultKeyUnavailable,
    CiphertextInvalid,
    PayloadInvalid,
    UnsupportedSchema,
    UnsupportedGeneration,
    RandomUnavailable,
    PersistFailed,
    PersistedStateChanged,
    CacheCapacity,
    Derivation,
}

impl fmt::Display for KeyedDigestErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::RepositoryUnavailable => "repository database unavailable",
            Self::RepositoryIdentityInvalid => "repository identity missing or ambiguous",
            Self::StateQueryFailed => "repository Memory state could not be inspected",
            Self::MissingAfterDurableUse => "key is missing after durable Memory state exists",
            Self::PlaintextConfig => "stored key payload is not marked encrypted",
            Self::DuplicateConfig => "multiple stored key payloads exist",
            Self::VaultKeyUnavailable => "repository vault key unavailable",
            Self::CiphertextInvalid => "stored key payload cannot be decrypted",
            Self::PayloadInvalid => "stored key payload is malformed",
            Self::UnsupportedSchema => "stored key schema version is unsupported",
            Self::UnsupportedGeneration => "stored key generation is unsupported",
            Self::RandomUnavailable => "secure randomness unavailable",
            Self::PersistFailed => "encrypted key payload could not be persisted",
            Self::PersistedStateChanged => "persisted key changed after it was cached",
            Self::CacheCapacity => "process cache capacity reached",
            Self::Derivation => "HKDF key derivation failed",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Error)]
#[error(
    "repository Memory digest key is unavailable ({kind}); restore 'memory.keyed_digest.v1' from repository-local encrypted configuration or repair the repository vault{rollback_context}"
)]
pub(crate) struct KeyedDigestError {
    kind: KeyedDigestErrorKind,
    rollback_context: &'static str,
}

impl KeyedDigestError {
    const fn new(kind: KeyedDigestErrorKind) -> Self {
        Self {
            kind,
            rollback_context: "",
        }
    }

    const fn with_rollback_failure(mut self) -> Self {
        self.rollback_context = "; repository transaction rollback also failed, so inspect repository state before retrying";
        self
    }

    pub(crate) const fn kind(&self) -> KeyedDigestErrorKind {
        self.kind
    }

    pub(crate) const fn stable_code(&self) -> StableErrorCode {
        StableErrorCode::MemoryDigestKeyUnavailable
    }
}

#[derive(Serialize, Deserialize)]
struct PersistedDigestKeyV1 {
    schema_version: u8,
    generation: u32,
    key_id: Uuid,
    seed_hex: String,
}

struct HmacSha256KeyLength;

impl hkdf::KeyType for HmacSha256KeyLength {
    fn len(&self) -> usize {
        DERIVED_KEY_BYTES
    }
}

pub(crate) struct RepositoryKeyedDigest {
    repository_id: String,
    key_id: Uuid,
    keys: [hmac::Key; 4],
    persisted_config_fingerprint: [u8; 32],
    valid: AtomicBool,
}

impl fmt::Debug for RepositoryKeyedDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryKeyedDigest")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl RepositoryKeyedDigest {
    #[cfg(test)]
    pub(crate) fn for_receipt_tests(
        repository_id: &str,
        key_id: Uuid,
        seed: [u8; 32],
        persisted_ciphertext: &str,
    ) -> Self {
        Self::from_seed(
            repository_id.to_string(),
            key_id,
            seed,
            config_fingerprint(persisted_ciphertext),
        )
        .expect("fixed test seed must construct a keyed-digest provider")
    }

    pub(crate) async fn load_or_initialize(
        repository_db_path: &Path,
    ) -> Result<Arc<Self>, KeyedDigestError> {
        let canonical_db_path = tokio::fs::canonicalize(repository_db_path)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryUnavailable))?;
        let database = db::get_db_conn_instance_for_path(&canonical_db_path)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryUnavailable))?;
        Self::load_cached(
            &database,
            canonical_db_path,
            DigestLoadMode::LoadOrInitialize,
        )
        .await
    }

    /// Load an existing repository digest key without creating one.
    ///
    /// Read-only command adapters use this entry so `search`, `show`, and
    /// `status` cannot mutate repository configuration as a side effect.
    pub(crate) async fn load_existing(
        repository_db_path: &Path,
    ) -> Result<Arc<Self>, KeyedDigestError> {
        let canonical_db_path = tokio::fs::canonicalize(repository_db_path)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryUnavailable))?;
        let database = db::open_database_without_migrations(&canonical_db_path)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryUnavailable))?;
        Self::load_cached(&database, canonical_db_path, DigestLoadMode::ExistingOnly).await
    }

    /// Load an existing key through a caller-owned connection.
    ///
    /// Read-only adapters use this form so the same no-migration connection
    /// serves Memory diagnostics and keyed-digest validation.
    pub(crate) async fn load_existing_with_connection(
        repository_db_path: &Path,
        database: &DatabaseConnection,
    ) -> Result<Arc<Self>, KeyedDigestError> {
        let canonical_db_path = tokio::fs::canonicalize(repository_db_path)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryUnavailable))?;
        Self::load_cached(database, canonical_db_path, DigestLoadMode::ExistingOnly).await
    }

    async fn load_cached(
        database: &DatabaseConnection,
        canonical_db_path: PathBuf,
        mode: DigestLoadMode,
    ) -> Result<Arc<Self>, KeyedDigestError> {
        let repository_id = repository_id(database).await?;
        let cache_key = RepositoryCacheKey {
            canonical_db_path: canonical_db_path.clone(),
            repository_id,
        };

        // Holding this async mutex across the cold load is intentional: it is
        // the single-flight guard that prevents duplicate decrypts for one
        // repository. Loads are local-only and bounded; normal processes own
        // one repository identity.
        let mut cache = REPOSITORY_KEYED_DIGEST_CACHE.lock().await;
        if let Some(provider) = cache.get(&cache_key) {
            validate_cached_provider(database, provider).await?;
            return Ok(Arc::clone(provider));
        }
        if cache.len() >= PROCESS_CACHE_CAPACITY {
            return Err(KeyedDigestError::new(KeyedDigestErrorKind::CacheCapacity));
        }
        let provider = Arc::new(match mode {
            DigestLoadMode::LoadOrInitialize => {
                Self::load_or_initialize_uncached(
                    database,
                    &canonical_db_path,
                    &cache_key.repository_id,
                )
                .await?
            }
            DigestLoadMode::ExistingOnly => {
                Self::load_existing_uncached(database, &canonical_db_path, &cache_key.repository_id)
                    .await?
            }
        });
        cache.insert(cache_key, Arc::clone(&provider));
        Ok(provider)
    }

    async fn load_existing_uncached(
        database: &DatabaseConnection,
        repository_db_path: &Path,
        repository_id: &str,
    ) -> Result<Self, KeyedDigestError> {
        let rows = ConfigKv::get_all_with_conn(database, MEMORY_KEYED_DIGEST_CONFIG_KEY)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::StateQueryFailed))?;
        match rows.as_slice() {
            [row] => {
                load_persisted_provider(database, repository_db_path, repository_id, row).await
            }
            [] => Err(KeyedDigestError::new(
                KeyedDigestErrorKind::MissingAfterDurableUse,
            )),
            _ => Err(KeyedDigestError::new(KeyedDigestErrorKind::DuplicateConfig)),
        }
    }

    async fn load_or_initialize_uncached(
        database: &DatabaseConnection,
        repository_db_path: &Path,
        repository_id: &str,
    ) -> Result<Self, KeyedDigestError> {
        let transaction = db::begin_write_transaction(database)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::StateQueryFailed))?;

        let result = Self::load_or_initialize_in_transaction(
            &transaction,
            repository_db_path,
            repository_id,
        )
        .await;
        match result {
            Ok(provider) => {
                transaction
                    .commit()
                    .await
                    .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::PersistFailed))?;
                Ok(provider)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(_) => Err(error.with_rollback_failure()),
            },
        }
    }

    async fn load_or_initialize_in_transaction<C: ConnectionTrait>(
        database: &C,
        repository_db_path: &Path,
        repository_id: &str,
    ) -> Result<Self, KeyedDigestError> {
        let rows = ConfigKv::get_all_with_conn(database, MEMORY_KEYED_DIGEST_CONFIG_KEY)
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::StateQueryFailed))?;
        match rows.as_slice() {
            [row] => {
                load_persisted_provider(database, repository_db_path, repository_id, row).await
            }
            [] => {
                ensure_initialization_is_eligible(database).await?;
                initialize_provider(database, repository_db_path, repository_id).await
            }
            _ => Err(KeyedDigestError::new(KeyedDigestErrorKind::DuplicateConfig)),
        }
    }

    fn from_seed(
        repository_id: String,
        key_id: Uuid,
        seed: [u8; 32],
        persisted_config_fingerprint: [u8; 32],
    ) -> Result<Self, KeyedDigestError> {
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, HKDF_SALT);
        let pseudo_random_key = salt.extract(&seed);
        let mut derived_keys = Vec::with_capacity(4);

        for purpose in [
            DigestPurpose::Idempotency,
            DigestPurpose::Principal,
            DigestPurpose::Query,
            DigestPurpose::SourceInput,
        ] {
            let info = [purpose.info()];
            let output = pseudo_random_key
                .expand(&info, HmacSha256KeyLength)
                .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::Derivation))?;
            let mut key_bytes = [0_u8; DERIVED_KEY_BYTES];
            output
                .fill(&mut key_bytes)
                .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::Derivation))?;
            derived_keys.push(hmac::Key::new(hmac::HMAC_SHA256, &key_bytes));
        }

        let keys: [hmac::Key; 4] = derived_keys
            .try_into()
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::Derivation))?;
        Ok(Self {
            repository_id,
            key_id,
            keys,
            persisted_config_fingerprint,
            valid: AtomicBool::new(true),
        })
    }

    pub(crate) const fn key_id(&self) -> Uuid {
        self.key_id
    }

    pub(crate) fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub(crate) async fn validate_for_connection<C: ConnectionTrait>(
        &self,
        database: &C,
    ) -> Result<(), KeyedDigestError> {
        validate_cached_provider(database, self).await
    }

    fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }

    fn ensure_valid(&self) -> Result<(), KeyedDigestError> {
        if self.valid.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(KeyedDigestError::new(
                KeyedDigestErrorKind::PersistedStateChanged,
            ))
        }
    }

    fn digest(
        &self,
        purpose: DigestPurpose,
        input: &[u8],
    ) -> Result<KeyedDigestEnvelope, KeyedDigestError> {
        self.ensure_valid()?;
        let tag = hmac::sign(&self.keys[purpose.index()], input);
        let envelope = KeyedDigestEnvelope {
            version: DIGEST_VERSION,
            key_id: self.key_id,
            purpose,
            digest: hex::encode(tag.as_ref()),
        };
        // A concurrent mismatch observation can invalidate the shared handle
        // while HMAC is running. Check again before releasing the envelope.
        self.ensure_valid()?;
        Ok(envelope)
    }

    pub(crate) fn source_input_fingerprint(
        &self,
        input: &[u8],
    ) -> Result<SourceInputFingerprint, KeyedDigestError> {
        self.digest(DigestPurpose::SourceInput, input)
            .map(SourceInputFingerprint)
    }

    pub(crate) fn principal_digest(
        &self,
        input: &[u8],
    ) -> Result<PrincipalDigest, KeyedDigestError> {
        self.digest(DigestPurpose::Principal, input)
            .map(PrincipalDigest)
    }

    pub(crate) fn query_digest(&self, input: &[u8]) -> Result<QueryDigest, KeyedDigestError> {
        self.digest(DigestPurpose::Query, input).map(QueryDigest)
    }
}

fn config_fingerprint(ciphertext_hex: &str) -> [u8; 32] {
    digest::digest(&digest::SHA256, ciphertext_hex.as_bytes())
        .as_ref()
        .try_into()
        // INVARIANT: ring's SHA-256 algorithm always emits exactly 32 bytes.
        .expect("SHA-256 output length is fixed at 32 bytes")
}

async fn validate_cached_provider<C: ConnectionTrait>(
    database: &C,
    provider: &RepositoryKeyedDigest,
) -> Result<(), KeyedDigestError> {
    provider.ensure_valid()?;
    let validation = match ConfigKv::get_all_with_conn(database, MEMORY_KEYED_DIGEST_CONFIG_KEY)
        .await
    {
        Err(_) => Err(KeyedDigestError::new(
            KeyedDigestErrorKind::StateQueryFailed,
        )),
        Ok(rows) => match rows.as_slice() {
            [] => Err(KeyedDigestError::new(
                KeyedDigestErrorKind::MissingAfterDurableUse,
            )),
            [row] if !row.encrypted => {
                Err(KeyedDigestError::new(KeyedDigestErrorKind::PlaintextConfig))
            }
            [row] if config_fingerprint(&row.value) != provider.persisted_config_fingerprint => {
                Err(KeyedDigestError::new(
                    KeyedDigestErrorKind::PersistedStateChanged,
                ))
            }
            [_] => Ok(()),
            _ => Err(KeyedDigestError::new(KeyedDigestErrorKind::DuplicateConfig)),
        },
    };
    if validation.is_err() {
        provider.invalidate();
    }
    validation
}

async fn repository_id(database: &DatabaseConnection) -> Result<String, KeyedDigestError> {
    let identity = RepoIdentity::resolve(database)
        .await
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryIdentityInvalid))?;
    let rows = ConfigKv::get_all_with_conn(database, "libra.repoid")
        .await
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RepositoryIdentityInvalid))?;
    let [row] = rows.as_slice() else {
        return Err(KeyedDigestError::new(
            KeyedDigestErrorKind::RepositoryIdentityInvalid,
        ));
    };
    if row.encrypted {
        return Err(KeyedDigestError::new(
            KeyedDigestErrorKind::RepositoryIdentityInvalid,
        ));
    }
    Ok(identity.as_str().to_owned())
}

async fn ensure_initialization_is_eligible<C: ConnectionTrait>(
    database: &C,
) -> Result<(), KeyedDigestError> {
    let backend = database.get_database_backend();
    let memory_ref = database
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM reference
             WHERE kind = 'Branch' AND remote IS NULL
               AND (name = ? OR name LIKE ?)
             LIMIT 1",
            [
                "libra/memory/repo".into(),
                format!("{MEMORY_REF_PREFIX}%").into(),
            ],
        ))
        .await
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::StateQueryFailed))?;
    if memory_ref.is_some() {
        return Err(KeyedDigestError::new(
            KeyedDigestErrorKind::MissingAfterDurableUse,
        ));
    }

    let receipt_table = database
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            [RECEIPT_TABLE.into()],
        ))
        .await
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::StateQueryFailed))?;
    if receipt_table.is_some() {
        let receipt = database
            .query_one_raw(Statement::from_string(
                backend,
                format!("SELECT 1 FROM {RECEIPT_TABLE} LIMIT 1"),
            ))
            .await
            .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::StateQueryFailed))?;
        if receipt.is_some() {
            return Err(KeyedDigestError::new(
                KeyedDigestErrorKind::MissingAfterDurableUse,
            ));
        }
    }
    Ok(())
}

async fn initialize_provider<C: ConnectionTrait>(
    database: &C,
    repository_db_path: &Path,
    repository_id: &str,
) -> Result<RepositoryKeyedDigest, KeyedDigestError> {
    use ring::rand::{SecureRandom, SystemRandom};

    let unseal_key = vault::load_unseal_key_for_db_path_with_conn(repository_db_path, database)
        .await
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::VaultKeyUnavailable))?;
    let mut seed = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut seed)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::RandomUnavailable))?;
    let key_id = Uuid::new_v4();
    let payload = PersistedDigestKeyV1 {
        schema_version: PERSISTED_SCHEMA_VERSION,
        generation: PERSISTED_GENERATION,
        key_id,
        seed_hex: hex::encode(seed),
    };
    let plaintext = serde_json::to_vec(&payload)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::PayloadInvalid))?;
    let ciphertext = vault::encrypt_token(&unseal_key, &plaintext)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::CiphertextInvalid))?;
    let ciphertext_hex = hex::encode(ciphertext);
    let inserted = ConfigKv::insert_vault_internal_if_absent_with_conn(
        database,
        MEMORY_KEYED_DIGEST_CONFIG_KEY,
        &ciphertext_hex,
    )
    .await
    .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::PersistFailed))?;
    if !inserted {
        return Err(KeyedDigestError::new(KeyedDigestErrorKind::PersistFailed));
    }
    RepositoryKeyedDigest::from_seed(
        repository_id.to_string(),
        key_id,
        seed,
        config_fingerprint(&ciphertext_hex),
    )
}

async fn load_persisted_provider<C: ConnectionTrait>(
    database: &C,
    repository_db_path: &Path,
    repository_id: &str,
    row: &crate::internal::config::ConfigKvEntry,
) -> Result<RepositoryKeyedDigest, KeyedDigestError> {
    if !row.encrypted {
        return Err(KeyedDigestError::new(KeyedDigestErrorKind::PlaintextConfig));
    }
    let unseal_key = vault::load_unseal_key_for_db_path_with_conn(repository_db_path, database)
        .await
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::VaultKeyUnavailable))?;
    let ciphertext = hex::decode(&row.value)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::CiphertextInvalid))?;
    let plaintext = vault::decrypt_token(&unseal_key, &ciphertext)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::CiphertextInvalid))?;
    let payload: PersistedDigestKeyV1 = serde_json::from_str(&plaintext)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::PayloadInvalid))?;
    if payload.schema_version != PERSISTED_SCHEMA_VERSION {
        return Err(KeyedDigestError::new(
            KeyedDigestErrorKind::UnsupportedSchema,
        ));
    }
    if payload.generation != PERSISTED_GENERATION {
        return Err(KeyedDigestError::new(
            KeyedDigestErrorKind::UnsupportedGeneration,
        ));
    }
    if payload.key_id.get_version_num() != 4
        || payload.seed_hex.len() != 64
        || !payload
            .seed_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(KeyedDigestError::new(KeyedDigestErrorKind::PayloadInvalid));
    }
    let seed_bytes = hex::decode(payload.seed_hex)
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::PayloadInvalid))?;
    let seed: [u8; 32] = seed_bytes
        .try_into()
        .map_err(|_| KeyedDigestError::new(KeyedDigestErrorKind::PayloadInvalid))?;
    RepositoryKeyedDigest::from_seed(
        repository_id.to_string(),
        payload.key_id,
        seed,
        config_fingerprint(&row.value),
    )
}

#[cfg(test)]
async fn reset_digest_cache_for_tests() {
    REPOSITORY_KEYED_DIGEST_CACHE.lock().await.clear();
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashSet},
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };

    use ring::hmac;
    use sea_orm::{ConnectionTrait, Statement};
    use serial_test::serial;
    use tempfile::TempDir;
    use uuid::Uuid;
    use walkdir::WalkDir;

    use super::{
        DigestPurpose, KeyedDigestError, KeyedDigestErrorKind, PERSISTED_GENERATION,
        PERSISTED_SCHEMA_VERSION, PersistedDigestKeyV1, RepositoryKeyedDigest,
        SourceInputFingerprint, SourceInputFingerprintErrorKind, config_fingerprint, repository_id,
        reset_digest_cache_for_tests,
    };
    use crate::{
        internal::{
            config::{self, ConfigKv, MEMORY_KEYED_DIGEST_CONFIG_KEY as CONFIG_KEY},
            db, vault,
        },
        utils::error::StableErrorCode,
    };

    const TEST_VAULT_KEY: [u8; 32] = [0x42; 32];

    struct TestRepository {
        _repo: TempDir,
        _home: TempDir,
        previous_test_home: Option<OsString>,
        repo_root: PathBuf,
        home_root: PathBuf,
        repo_id: String,
        vault_key: Vec<u8>,
        db_path: PathBuf,
    }

    impl TestRepository {
        async fn new() -> Self {
            Self::new_inner(false).await
        }

        async fn new_with_real_vault() -> Self {
            Self::new_inner(true).await
        }

        async fn new_inner(real_vault: bool) -> Self {
            let repo = tempfile::tempdir().expect("temporary repository must be created");
            let home = tempfile::tempdir().expect("temporary home must be created");
            let previous_test_home = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: every test using this fixture is marked `#[serial]`, and
            // the previous value is restored when the fixture is dropped.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };

            let storage = repo.path().join(".libra");
            tokio::fs::create_dir_all(&storage)
                .await
                .expect("repository storage must be created");
            let db_path = storage.join("libra.db");
            let conn = db::create_database(&db_path.to_string_lossy())
                .await
                .expect("repository database must be created");
            let repo_id = Uuid::new_v4().to_string();
            ConfigKv::set_with_conn(&conn, "libra.repoid", &repo_id, false)
                .await
                .expect("repository identity must be stored");

            let vault_key = if real_vault {
                vault::init_vault(&storage)
                    .await
                    .expect("repository vault must initialize")
                    .0
            } else {
                TEST_VAULT_KEY.to_vec()
            };

            let key_path = home.path().join(".libra/vault-keys").join(&repo_id);
            tokio::fs::create_dir_all(
                key_path
                    .parent()
                    .expect("vault key path must have a parent"),
            )
            .await
            .expect("vault key directory must be created");
            tokio::fs::write(&key_path, hex::encode(&vault_key))
                .await
                .expect("repository vault key must be stored");

            Self {
                repo_root: repo.path().to_path_buf(),
                home_root: home.path().to_path_buf(),
                repo_id,
                vault_key,
                _repo: repo,
                _home: home,
                previous_test_home,
                db_path,
            }
        }

        async fn connection(&self) -> sea_orm::DatabaseConnection {
            db::get_db_conn_instance_for_path(&self.db_path)
                .await
                .expect("repository database must open")
        }

        async fn set_digest_config(&self, value: &str, encrypted: bool) {
            let conn = self.connection().await;
            ConfigKv::set_with_conn(&conn, CONFIG_KEY, value, encrypted)
                .await
                .expect("digest config must be stored");
        }

        async fn add_digest_config(&self, value: &str, encrypted: bool) {
            let conn = self.connection().await;
            ConfigKv::add_with_conn(&conn, CONFIG_KEY, value, encrypted)
                .await
                .expect("additional digest config must be stored");
        }

        async fn store_payload(&self, payload: &PersistedDigestKeyV1) {
            let plaintext = serde_json::to_vec(payload).expect("payload must serialize");
            let ciphertext =
                vault::encrypt_token(&self.vault_key, &plaintext).expect("payload must encrypt");
            self.set_digest_config(&hex::encode(ciphertext), true).await;
        }

        async fn insert_memory_ref(&self) {
            let conn = self.connection().await;
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "INSERT INTO reference (name, kind, `commit`, remote, worktree_id) \
                 VALUES ('libra/memory/repo', 'Branch', NULL, NULL, NULL)",
            ))
            .await
            .expect("Memory ref must be inserted");
        }

        async fn insert_receipt(&self) {
            let conn = self.connection().await;
            conn.execute_unprepared(
                "INSERT INTO context_selection_receipt (
                    receipt_id, schema_version, source_kind, repository_id,
                    digest_key_id, principal_hmac, query_hmac, effective_at,
                    source_heads_json, projection_watermarks_json, policy_hash,
                    selector_version, token_budget, selected_json, omissions_json,
                    bundle_hash, reproducibility_state, recorded_at
                 ) VALUES (
                    '0198a7e0-7c00-7000-8000-000000000001', 1, 'memory', 'repo-test',
                    '123e4567-e89b-42d3-a456-426614174000',
                    'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                    'hmac-sha256:123e4567-e89b-42d3-a456-426614174000:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                    '2026-08-24T00:00:00.000000000Z', '{}', '{}',
                    'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                    'memory-v1', 1, '[]', '[]',
                    'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                    'reproducible', '2026-08-24T00:00:00.000000000Z'
                 )",
            )
            .await
            .expect("receipt must be inserted");
        }

        async fn remove_home_vault_key(&self) {
            tokio::fs::remove_file(self.home_root.join(".libra/vault-keys").join(&self.repo_id))
                .await
                .expect("home vault key must be removed");
        }

        async fn cleanup(&self) {
            reset_digest_cache_for_tests().await;
            db::reset_db_conn_instance_for_path(&self.db_path).await;
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            // SAFETY: the fixture's tests are serialized and this restores the
            // process environment to the value observed during construction.
            unsafe {
                match self.previous_test_home.take() {
                    Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

    fn inventory_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut inventory = BTreeMap::new();
        for entry in WalkDir::new(root) {
            let entry = entry.expect("persistence surface must be walkable");
            if entry.file_type().is_file() {
                let relative_path = entry
                    .path()
                    .strip_prefix(root)
                    .expect("inventory entry must remain below its root")
                    .to_path_buf();
                inventory.insert(
                    relative_path,
                    fs::read(entry.path()).expect("persistence surface must be readable"),
                );
            }
        }
        inventory
    }

    struct WorkingDirectoryGuard(PathBuf);

    impl WorkingDirectoryGuard {
        fn enter(path: &std::path::Path) -> Self {
            let previous = std::env::current_dir().expect("working directory must resolve");
            std::env::set_current_dir(path).expect("working directory must change");
            Self(previous)
        }
    }

    impl Drop for WorkingDirectoryGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("working directory must be restored");
        }
    }

    #[test]
    fn keyed_digest_domains_are_distinct_and_match_frozen_vectors() {
        let seed: [u8; 32] = std::array::from_fn(|index| index as u8);
        let provider = RepositoryKeyedDigest::from_seed(
            "test-repository".to_string(),
            Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000")
                .expect("fixed UUIDv4 must parse"),
            seed,
            config_fingerprint("frozen-test-vector"),
        )
        .expect("fixed seed must construct a provider");

        let input = b"libra memory vector";
        let cases = [
            (
                DigestPurpose::Idempotency,
                "c113c6657398689378eda6ca44806a480c1bd02be1861edaf16fb4206a5cba4e",
            ),
            (
                DigestPurpose::Principal,
                "a2cdadc40035b5ea669a39256e3011f4e5d06110f72cb1dcef15b909b02a3428",
            ),
            (
                DigestPurpose::Query,
                "4635c232c8e0d3fe583958bd98270695ef87e0548a7da2b8e3acb90c34126243",
            ),
            (
                DigestPurpose::SourceInput,
                "829132f11015ee8e47dba36fc775f76faa1f70feb8f393be8a0af54e8f4c4b49",
            ),
        ];

        for &(purpose, expected) in &cases {
            let digest = provider
                .digest(purpose, input)
                .expect("a fresh fixed-vector provider must remain valid");
            assert_eq!(digest.digest_hex(), expected);
            assert_eq!(digest.purpose(), purpose);
            assert_eq!(digest.key_id(), provider.key_id());
            assert_eq!(digest.version(), 1);
        }

        let distinct: HashSet<_> = cases
            .into_iter()
            .map(|(purpose, _)| {
                provider
                    .digest(purpose, input)
                    .expect("a fresh fixed-vector provider must remain valid")
                    .digest_hex()
                    .to_string()
            })
            .collect();
        assert_eq!(distinct.len(), 4);

        let envelope = provider
            .digest(DigestPurpose::Query, input)
            .expect("a fresh fixed-vector provider must remain valid");
        let json = serde_json::to_value(&envelope).expect("envelope must serialize");
        assert_eq!(json["version"], 1);
        assert_eq!(json["key_id"], provider.key_id().to_string());
        assert_eq!(json["purpose"], "query");
        assert_eq!(json["digest"], envelope.digest_hex());
        assert!(json.get("seed").is_none());
    }

    #[test]
    fn source_input_fingerprint_is_purpose_locked_and_round_trips_parts() {
        let key_id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000")
            .expect("fixed UUIDv4 must parse");
        let provider = RepositoryKeyedDigest::from_seed(
            "test-repository".to_string(),
            key_id,
            [0x24; 32],
            config_fingerprint("source-input-fingerprint"),
        )
        .expect("fixed seed must construct a provider");

        let fingerprint = provider
            .source_input_fingerprint(b"task-42 terminal inputs")
            .expect("valid provider produces a source-input fingerprint");
        assert_eq!(fingerprint.version(), 1);
        assert_eq!(fingerprint.key_id(), key_id);
        assert_eq!(fingerprint.digest_hex().len(), 64);

        let restored = SourceInputFingerprint::from_parts(
            fingerprint.version(),
            fingerprint.key_id(),
            fingerprint.digest_hex().to_owned(),
        )
        .expect("persisted source-input parts must round-trip");
        assert!(restored == fingerprint);

        let Err(unsupported_version) =
            SourceInputFingerprint::from_parts(2, key_id, "a".repeat(64))
        else {
            panic!("unsupported versions must fail closed");
        };
        assert_eq!(
            unsupported_version.kind(),
            SourceInputFingerprintErrorKind::UnsupportedVersion
        );

        let Err(invalid_digest) = SourceInputFingerprint::from_parts(1, key_id, "A".repeat(64))
        else {
            panic!("non-canonical digest text must fail closed");
        };
        assert_eq!(
            invalid_digest.kind(),
            SourceInputFingerprintErrorKind::InvalidDigest
        );
    }

    #[test]
    fn receipt_digests_are_purpose_locked_and_share_the_repository_key() {
        let key_id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000")
            .expect("fixed UUIDv4 must parse");
        let provider = RepositoryKeyedDigest::from_seed(
            "test-repository".to_string(),
            key_id,
            [0x25; 32],
            config_fingerprint("receipt-digests"),
        )
        .expect("fixed seed must construct a provider");

        let principal = provider
            .principal_digest(b"agent:alice")
            .expect("valid provider produces a principal digest");
        let query = provider
            .query_digest(b"normalized retrieval inputs")
            .expect("valid provider produces a query digest");

        assert_eq!(principal.key_id(), key_id);
        assert_eq!(query.key_id(), key_id);
        assert_eq!(principal.version(), 1);
        assert_eq!(query.version(), 1);
        assert_ne!(principal.digest_hex(), query.digest_hex());
        assert_eq!(
            principal.encoded(),
            format!("hmac-sha256:{key_id}:{}", principal.digest_hex())
        );
        assert_eq!(
            query.encoded(),
            format!("hmac-sha256:{key_id}:{}", query.digest_hex())
        );
    }

    #[test]
    fn keyed_digest_error_contract_is_stable_and_actionable() {
        let error = KeyedDigestError::new(KeyedDigestErrorKind::UnsupportedGeneration);
        assert_eq!(
            error.to_string(),
            "repository Memory digest key is unavailable (stored key generation is unsupported); \
             restore 'memory.keyed_digest.v1' from repository-local encrypted configuration or \
             repair the repository vault"
        );
        assert_eq!(
            error.stable_code(),
            StableErrorCode::MemoryDigestKeyUnavailable
        );
        assert_eq!(error.stable_code().as_str(), "LBR-MEMORY-001");

        let rollback_error =
            KeyedDigestError::new(KeyedDigestErrorKind::PersistFailed).with_rollback_failure();
        assert_eq!(rollback_error.kind(), KeyedDigestErrorKind::PersistFailed);
        assert_eq!(
            rollback_error.stable_code(),
            StableErrorCode::MemoryDigestKeyUnavailable
        );
        assert!(
            rollback_error
                .to_string()
                .contains("repository transaction rollback also failed")
        );
        assert!(
            rollback_error
                .to_string()
                .contains("inspect repository state before retrying")
        );
    }

    #[tokio::test]
    #[serial]
    async fn encrypted_key_survives_provider_reload() {
        reset_digest_cache_for_tests().await;
        let repository = TestRepository::new_with_real_vault().await;

        let first = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
            .await
            .expect("first load must initialize the repository digest key");
        let first_digest = first
            .digest(DigestPurpose::Query, b"same query")
            .expect("fresh provider must digest");

        reset_digest_cache_for_tests().await;
        let second = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
            .await
            .expect("second load must decrypt the persisted repository digest key");
        let second_digest = second
            .digest(DigestPurpose::Query, b"same query")
            .expect("reloaded provider must digest");

        assert_eq!(second.key_id(), first.key_id());
        assert_eq!(second_digest, first_digest);

        let rows = ConfigKv::get_all_with_conn(&repository.connection().await, CONFIG_KEY)
            .await
            .expect("digest key rows must be readable");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].encrypted);
        assert!(config::is_sensitive_key(CONFIG_KEY));
        assert!(config::is_vault_internal_key(CONFIG_KEY));

        repository.cleanup().await;
    }

    #[tokio::test]
    #[serial]
    async fn cached_provider_rejects_deleted_or_replaced_persisted_key() {
        reset_digest_cache_for_tests().await;
        {
            let repository = TestRepository::new().await;
            let cached = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect("first load must populate the process cache");

            ConfigKv::unset_all_with_conn(&repository.connection().await, CONFIG_KEY)
                .await
                .expect("fixture must delete the persisted owner row");
            let deleted_error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("a cache hit must reject a deleted persisted key");
            assert_eq!(
                deleted_error.kind(),
                KeyedDigestErrorKind::MissingAfterDurableUse
            );
            let stale_handle_error = cached
                .digest(DigestPurpose::Query, b"must not sign after deletion")
                .expect_err("a previously issued handle must be poisoned after deletion");
            assert_eq!(
                stale_handle_error.kind(),
                KeyedDigestErrorKind::PersistedStateChanged
            );

            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            let cached = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect("first load must populate the process cache");
            let replacement_id = Uuid::new_v4();
            assert_ne!(replacement_id, cached.key_id());
            repository
                .store_payload(&PersistedDigestKeyV1 {
                    schema_version: PERSISTED_SCHEMA_VERSION,
                    generation: PERSISTED_GENERATION,
                    key_id: replacement_id,
                    seed_hex: hex::encode([0x7c_u8; 32]),
                })
                .await;

            let replaced_error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("a cache hit must reject a replaced persisted key");
            assert_eq!(
                replaced_error.kind(),
                KeyedDigestErrorKind::PersistedStateChanged
            );
            let stale_handle_error = cached
                .digest(DigestPurpose::Query, b"must not sign after replacement")
                .expect_err("a previously issued handle must be poisoned after replacement");
            assert_eq!(
                stale_handle_error.kind(),
                KeyedDigestErrorKind::PersistedStateChanged
            );

            repository.cleanup().await;
        }
    }

    #[tokio::test]
    #[serial]
    async fn concurrent_initialization_keeps_one_persisted_winner() {
        reset_digest_cache_for_tests().await;
        let repository = TestRepository::new().await;
        let mut tasks = Vec::new();

        for _ in 0..32 {
            let db_path = repository.db_path.clone();
            tasks.push(tokio::spawn(async move {
                let conn = db::open_connection_without_schema_management(
                    db_path.to_string_lossy().as_ref(),
                    Duration::from_secs(30),
                )
                .await
                .expect("independent repository connection must open");
                let repository_id = repository_id(&conn).await?;
                RepositoryKeyedDigest::load_or_initialize_uncached(&conn, &db_path, &repository_id)
                    .await
                    .map(|provider| provider.key_id())
            }));
        }

        let mut key_ids = Vec::new();
        for task in tasks {
            key_ids.push(
                task.await
                    .expect("initializer task must not panic")
                    .expect("initializer must converge on the winner"),
            );
        }
        assert!(key_ids.iter().all(|key_id| *key_id == key_ids[0]));

        let rows = ConfigKv::get_all_with_conn(&repository.connection().await, CONFIG_KEY)
            .await
            .expect("digest key rows must be readable");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].encrypted);

        repository.cleanup().await;
    }

    #[tokio::test]
    #[serial]
    async fn keyed_digest_missing_existing_repo_fails_closed() {
        reset_digest_cache_for_tests().await;
        {
            let repository = TestRepository::new().await;
            repository.insert_memory_ref().await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("a Memory ref must prevent silent key replacement");
            assert_eq!(error.kind(), KeyedDigestErrorKind::MissingAfterDurableUse);
            assert_eq!(
                error.stable_code(),
                StableErrorCode::MemoryDigestKeyUnavailable
            );
            let rows = ConfigKv::get_all_with_conn(&repository.connection().await, CONFIG_KEY)
                .await
                .expect("digest rows must be inspectable");
            assert!(rows.is_empty());
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            repository.insert_receipt().await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("a selection receipt must prevent silent key replacement");
            assert_eq!(error.kind(), KeyedDigestErrorKind::MissingAfterDurableUse);
            let rows = ConfigKv::get_all_with_conn(&repository.connection().await, CONFIG_KEY)
                .await
                .expect("digest rows must be inspectable");
            assert!(rows.is_empty());
            repository.cleanup().await;
        }
    }

    #[tokio::test]
    #[serial]
    async fn invalid_persisted_states_fail_closed_without_replacement() {
        reset_digest_cache_for_tests().await;
        {
            let repository = TestRepository::new().await;
            repository.set_digest_config("plaintext-seed", false).await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("plaintext digest config must be rejected");
            assert_eq!(error.kind(), KeyedDigestErrorKind::PlaintextConfig);
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            repository.set_digest_config("00", true).await;
            repository.add_digest_config("11", true).await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("duplicate digest config must be rejected");
            assert_eq!(error.kind(), KeyedDigestErrorKind::DuplicateConfig);
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            repository.set_digest_config("not-hex", true).await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("invalid ciphertext must be rejected");
            assert_eq!(error.kind(), KeyedDigestErrorKind::CiphertextInvalid);
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            let valid_payload = serde_json::to_vec(&PersistedDigestKeyV1 {
                schema_version: PERSISTED_SCHEMA_VERSION,
                generation: PERSISTED_GENERATION,
                key_id: Uuid::new_v4(),
                seed_hex: hex::encode([0x19_u8; 32]),
            })
            .expect("payload must serialize");
            let wrong_ciphertext = vault::encrypt_token(&[0x99_u8; 32], &valid_payload)
                .expect("payload must encrypt with the wrong key");
            repository
                .set_digest_config(&hex::encode(wrong_ciphertext), true)
                .await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("AES-GCM authentication failure must be rejected");
            assert_eq!(error.kind(), KeyedDigestErrorKind::CiphertextInvalid);
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            let malformed_ciphertext = vault::encrypt_token(&repository.vault_key, b"{}")
                .expect("malformed payload must still encrypt");
            repository
                .set_digest_config(&hex::encode(malformed_ciphertext), true)
                .await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("malformed decrypted JSON must be rejected");
            assert_eq!(error.kind(), KeyedDigestErrorKind::PayloadInvalid);
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            repository
                .store_payload(&PersistedDigestKeyV1 {
                    schema_version: PERSISTED_SCHEMA_VERSION,
                    generation: PERSISTED_GENERATION,
                    key_id: Uuid::parse_str("018f6f77-20c3-7d61-9d9b-94b63ce9a243")
                        .expect("fixed UUIDv7 must parse"),
                    seed_hex: hex::encode([0x28_u8; 32]),
                })
                .await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("non-v4 key identity must be rejected");
            assert_eq!(error.kind(), KeyedDigestErrorKind::PayloadInvalid);
            repository.cleanup().await;
        }

        for (schema_version, generation, expected) in [
            (
                2,
                PERSISTED_GENERATION,
                KeyedDigestErrorKind::UnsupportedSchema,
            ),
            (
                PERSISTED_SCHEMA_VERSION,
                2,
                KeyedDigestErrorKind::UnsupportedGeneration,
            ),
        ] {
            let repository = TestRepository::new().await;
            repository
                .store_payload(&PersistedDigestKeyV1 {
                    schema_version,
                    generation,
                    key_id: Uuid::new_v4(),
                    seed_hex: hex::encode([0x24_u8; 32]),
                })
                .await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("unsupported persisted metadata must be rejected");
            assert_eq!(error.kind(), expected);
            let rows = ConfigKv::get_all_with_conn(&repository.connection().await, CONFIG_KEY)
                .await
                .expect("digest rows must be inspectable");
            assert_eq!(rows.len(), 1, "invalid state must not be replaced");
            repository.cleanup().await;
        }
    }

    #[tokio::test]
    #[serial]
    async fn repository_identity_and_state_queries_fail_closed() {
        reset_digest_cache_for_tests().await;
        let absent_root = tempfile::tempdir().expect("temporary root must exist");
        let absent_db = absent_root.path().join("missing.db");
        let error = RepositoryKeyedDigest::load_or_initialize(&absent_db)
            .await
            .expect_err("an absent repository database must be rejected");
        assert_eq!(error.kind(), KeyedDigestErrorKind::RepositoryUnavailable);

        {
            let repository = TestRepository::new().await;
            ConfigKv::unset_all_with_conn(&repository.connection().await, "libra.repoid")
                .await
                .expect("repository identity must be removable for the corruption probe");
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("missing repository identity must be rejected");
            assert_eq!(
                error.kind(),
                KeyedDigestErrorKind::RepositoryIdentityInvalid
            );
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            let conn = repository.connection().await;
            conn.execute_raw(Statement::from_string(
                conn.get_database_backend(),
                "DROP TABLE reference",
            ))
            .await
            .expect("reference table must be removable for the corruption probe");
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("unreadable Memory state must fail closed");
            assert_eq!(error.kind(), KeyedDigestErrorKind::StateQueryFailed);
            repository.cleanup().await;
        }
    }

    #[tokio::test]
    #[serial]
    async fn missing_vault_key_and_wrong_working_directory_fail_closed() {
        reset_digest_cache_for_tests().await;
        {
            let repository = TestRepository::new().await;
            repository
                .store_payload(&PersistedDigestKeyV1 {
                    schema_version: PERSISTED_SCHEMA_VERSION,
                    generation: PERSISTED_GENERATION,
                    key_id: Uuid::new_v4(),
                    seed_hex: hex::encode([0x35_u8; 32]),
                })
                .await;
            repository.remove_home_vault_key().await;
            let error = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect_err("missing vault key must fail closed");
            assert_eq!(error.kind(), KeyedDigestErrorKind::VaultKeyUnavailable);
            repository.cleanup().await;
        }

        {
            let repository = TestRepository::new().await;
            let unrelated = tempfile::tempdir().expect("unrelated working directory must exist");
            let _cwd = WorkingDirectoryGuard::enter(unrelated.path());
            let provider = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
                .await
                .expect("explicit repository path must not depend on cwd");
            assert_eq!(provider.key_id().get_version_num(), 4);
            repository.cleanup().await;
        }
    }

    #[tokio::test]
    #[serial]
    async fn keyed_digest_secret_probe_zero_leak() {
        reset_digest_cache_for_tests().await;
        let repository = TestRepository::new_with_real_vault().await;
        let object_root = repository.repo_root.join(".libra/objects");
        fs::create_dir_all(&object_root).expect("object surface must be created");
        fs::write(
            object_root.join("secret-probe-control"),
            b"non-secret object control",
        )
        .expect("object control must be populated");
        let object_inventory_before = inventory_files(&object_root);
        assert_eq!(
            object_inventory_before
                .get(Path::new("secret-probe-control"))
                .map(Vec::as_slice),
            Some(b"non-secret object control".as_slice()),
            "Git-object surface control must be present before provider use"
        );

        let conn = repository.connection().await;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO operation
             (op_id, repo_id, view_id, command_name, description, actor,
              args_digest, start_ts, end_ts, status)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                "secret-probe-operation-control".into(),
                repository.repo_id.clone().into(),
                "secret-probe-view".into(),
                "memory-keyed-digest-probe".into(),
                "non-secret operation-log control".into(),
                "test-actor".into(),
                "non-secret-args-digest".into(),
                1_i64.into(),
                2_i64.into(),
                "succeeded".into(),
            ],
        ))
        .await
        .expect("operation-log control must be populated");

        let known_seed = [0xa5_u8; 32];
        let known_seed_hex = hex::encode(known_seed);
        let known_principal_derived_key =
            hex::decode("877eaf4c522e3f5080b2914c17462c794bc465349936550910c49d1cefda5cf2")
                .expect("independently calculated principal HKDF vector must decode");
        let known_query_derived_key =
            hex::decode("f5bb7c87932fc33ec7cb98ce063edf8011fc991f77a794a69d554790fc4c2cd6")
                .expect("independently calculated query HKDF vector must decode");
        let known_principal_derived_key_hex = hex::encode(&known_principal_derived_key);
        let known_query_derived_key_hex = hex::encode(&known_query_derived_key);
        repository
            .store_payload(&PersistedDigestKeyV1 {
                schema_version: PERSISTED_SCHEMA_VERSION,
                generation: PERSISTED_GENERATION,
                key_id: Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000")
                    .expect("fixed UUIDv4 must parse"),
                seed_hex: known_seed_hex.clone(),
            })
            .await;

        let provider = RepositoryKeyedDigest::load_or_initialize(&repository.db_path)
            .await
            .expect("encrypted test seed must load");
        let reversible_principal = "alice@example.com";
        let reversible_query = "why did checkout retry after the lock error?";
        let principal_envelope = provider
            .digest(DigestPurpose::Principal, reversible_principal.as_bytes())
            .expect("valid provider must digest the principal probe");
        let query_envelope = provider
            .digest(DigestPurpose::Query, reversible_query.as_bytes())
            .expect("valid provider must digest the query probe");
        let expected_principal = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, &known_principal_derived_key),
            reversible_principal.as_bytes(),
        );
        let expected_query = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, &known_query_derived_key),
            reversible_query.as_bytes(),
        );
        assert_eq!(
            principal_envelope.digest_hex(),
            hex::encode(expected_principal.as_ref()),
            "principal derived-key marker must be the key actually used"
        );
        assert_eq!(
            query_envelope.digest_hex(),
            hex::encode(expected_query.as_ref()),
            "query derived-key marker must be the key actually used"
        );
        let rendered_surfaces = [
            format!("{provider:?}"),
            format!("{principal_envelope:?}"),
            format!("{query_envelope:?}"),
            serde_json::to_string(&principal_envelope).expect("principal envelope must serialize"),
            serde_json::to_string(&query_envelope).expect("query envelope must serialize"),
            KeyedDigestError::new(KeyedDigestErrorKind::CiphertextInvalid).to_string(),
        ];
        assert!(!rendered_surfaces.is_empty());
        for rendered in &rendered_surfaces {
            assert!(
                !rendered.is_empty(),
                "every declared render surface must exist"
            );
            for forbidden in [
                known_seed_hex.as_str(),
                known_principal_derived_key_hex.as_str(),
                known_query_derived_key_hex.as_str(),
                reversible_principal,
                reversible_query,
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "secret marker leaked into a rendered surface"
                );
            }
            for forbidden in [
                known_seed.as_slice(),
                known_principal_derived_key.as_slice(),
                known_query_derived_key.as_slice(),
            ] {
                assert!(
                    !rendered
                        .as_bytes()
                        .windows(forbidden.len())
                        .any(|window| window == forbidden),
                    "raw key bytes leaked into a rendered surface"
                );
            }
        }

        let object_inventory_after = inventory_files(&object_root);
        assert_eq!(
            object_inventory_after, object_inventory_before,
            "keyed-digest use must not add or alter Git-object bytes"
        );

        let operation_rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT op_id || char(31) || repo_id || char(31) || view_id || char(31) ||
                        command_name || char(31) || description || char(31) || actor || char(31) ||
                        COALESCE(args_digest, '')
                 FROM operation ORDER BY op_id",
            ))
            .await
            .expect("operation-log surface must be readable");
        assert_eq!(
            operation_rows.len(),
            1,
            "operation-log probe must inspect its populated control row"
        );
        let operation_surface = operation_rows[0]
            .try_get_by_index::<String>(0)
            .expect("operation-log control must remain textual");
        assert!(
            operation_surface.contains("secret-probe-operation-control"),
            "operation-log control marker must be present"
        );
        for forbidden in [
            known_seed_hex.as_str(),
            known_principal_derived_key_hex.as_str(),
            known_query_derived_key_hex.as_str(),
            reversible_principal,
            reversible_query,
        ] {
            assert!(
                !operation_surface.contains(forbidden),
                "secret marker leaked into the populated operation log"
            );
        }

        // This owner has no remote-tier dependency or adapter. Keep that
        // architectural boundary executable until a later task deliberately
        // adds remote publication with its own secret contract.
        let production_source = include_str!("keyed_digest.rs")
            .split_once("\n#[cfg(test)]\n")
            .expect("module must keep production and test sections distinct")
            .0;
        assert!(
            production_source.contains("impl RepositoryKeyedDigest"),
            "remote-tier guard must inspect the populated production module"
        );
        for remote_adapter in [
            "RemoteStorage",
            "ClientStorage",
            "D1Client",
            "reqwest::",
            "utils::storage::remote",
        ] {
            assert!(
                !production_source.contains(remote_adapter),
                "keyed-digest owner unexpectedly gained remote adapter {remote_adapter}"
            );
        }

        let mut repository_files = Vec::new();
        for entry in WalkDir::new(&repository.repo_root) {
            let entry = entry.expect("repository surface must be walkable");
            if entry.file_type().is_file() {
                repository_files.push((
                    entry.path().to_path_buf(),
                    fs::read(entry.path()).expect("repository surface must be readable"),
                ));
            }
        }
        assert!(
            repository_files.iter().any(|(_, bytes)| bytes
                .windows(CONFIG_KEY.len())
                .any(|window| window == CONFIG_KEY.as_bytes())),
            "probe must inspect a populated config database"
        );
        assert!(
            repository_files.len() >= 2,
            "probe must inspect both repository and vault persistence surfaces"
        );
        for (path, bytes) in &repository_files {
            for forbidden in [
                known_seed.as_slice(),
                known_principal_derived_key.as_slice(),
                known_query_derived_key.as_slice(),
            ] {
                assert!(
                    !bytes
                        .windows(forbidden.len())
                        .any(|window| window == forbidden),
                    "raw key material leaked into {}",
                    path.display()
                );
            }
            let rendered = String::from_utf8_lossy(bytes);
            for forbidden in [
                known_seed_hex.as_str(),
                known_principal_derived_key_hex.as_str(),
                known_query_derived_key_hex.as_str(),
                reversible_principal,
                reversible_query,
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "secret marker leaked into {}",
                    path.display()
                );
            }
        }

        let home_key = fs::read_to_string(
            repository
                .home_root
                .join(".libra/vault-keys")
                .join(&repository.repo_id),
        )
        .expect("control vault key must be readable");
        assert_eq!(home_key, hex::encode(&repository.vault_key));
        for forbidden in [
            known_seed_hex.as_str(),
            known_principal_derived_key_hex.as_str(),
            known_query_derived_key_hex.as_str(),
            reversible_principal,
            reversible_query,
        ] {
            assert!(!home_key.contains(forbidden));
        }
        let vault_db = repository.repo_root.join(".libra/vault.db");
        assert!(
            vault_db.is_file(),
            "probe must include a real repository vault"
        );
        assert!(
            fs::metadata(vault_db)
                .expect("vault metadata must be readable")
                .len()
                > 0
        );

        let rows = ConfigKv::get_all_with_conn(&repository.connection().await, CONFIG_KEY)
            .await
            .expect("digest row must be readable");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].encrypted);
        for forbidden in [
            known_seed_hex.as_str(),
            known_principal_derived_key_hex.as_str(),
            known_query_derived_key_hex.as_str(),
            reversible_principal,
            reversible_query,
        ] {
            assert!(!rows[0].value.contains(forbidden));
        }

        repository.cleanup().await;
    }
}
