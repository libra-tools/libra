//! Host-scoped HTTP token auth (lore.md §1.6) — the SINGLE owner API for the
//! `auth.token.*` namespace in the GLOBAL config store.
//!
//! Tokens are AES-256-GCM-encrypted with the global vault unseal key
//! (`~/.libra/vault-unseal-key`, created 0600) and stored as hex ciphertext
//! in `~/.libra/config.db` — the row's sanctioned "文件 fallback 加密"; a
//! real OS keyring is the 2.7 follow-up and swaps in behind this module
//! boundary. The plaintext token NEVER appears in logs, errors, JSON, or
//! status output; errors name only host/port.
//!
//! TRUST BOUNDARY (the lore row's client-side contract, STORED tokens only —
//! the interactive 401 prompt remains a process-global fallback): a stored
//! token is attached ONLY to requests whose normalized host:port scope
//! matches, over https (or http to a loopback host, for local dev remotes —
//! note a token stored without an explicit port normalizes to 443 and will
//! NOT match `http://localhost:80`; log in with the explicit port for
//! non-443 loopback remotes). Cross-host requests never see it.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    command::config::{ConfigScope, ScopedConfig},
    internal::{config::ConfigKv, vault},
};

/// Namespace prefix in the global config store (locked away from `libra
/// config get/set/list/unset` — this module is the only surface).
pub const AUTH_TOKEN_PREFIX: &str = "auth.token.";

/// A normalized host scope: lowercase host + effective port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostScope {
    pub host: String,
    pub port: u16,
}

impl HostScope {
    /// Parse a user-supplied host argument: bare `host`, `host:port`, or a
    /// full `https://host[:port]` URL. A bare form gets `https://` prepended
    /// BEFORE parsing (Url::parse("host:8443") would read `host` as a
    /// scheme). Requires https (or http to a loopback host); refuses
    /// userinfo, paths, queries, and fragments.
    pub fn parse(input: &str) -> Result<HostScope> {
        let text = input.trim();
        if text.is_empty() {
            bail!("host must not be empty");
        }
        let with_scheme = if text.contains("://") {
            text.to_string()
        } else {
            format!("https://{text}")
        };
        let url = url::Url::parse(&with_scheme)
            .map_err(|error| anyhow!("cannot parse host '{text}': {error}"))?;
        Self::from_url(&url)
    }

    /// Scope of a request URL (returns None for non-token-eligible schemes).
    ///
    /// A REQUEST url always carries a path, and usually a query — a smart-HTTP
    /// discovery is `https://host/owner/repo.git/info/refs?service=…`. Sharing
    /// [`Self::from_url`] with [`Self::parse`] therefore rejected every real
    /// request: the stored token was never attached, `libra push` over HTTPS
    /// answered `LBR-AUTH-001` no matter what `libra auth login` had stored,
    /// and the 401 hint that names the host printed the literal `<host>`. The
    /// path/query/fragment refusals belong to the user-supplied HOST argument
    /// only; the scope of a request is its host and port.
    pub fn from_request_url(url: &url::Url) -> Option<HostScope> {
        Self::scope_of(url).ok()
    }

    fn from_url(url: &url::Url) -> Result<HostScope> {
        if url.path() != "/" && !url.path().is_empty() {
            bail!("host must not carry a path");
        }
        if url.query().is_some() || url.fragment().is_some() {
            bail!("host must not carry a query or fragment");
        }
        Self::scope_of(url)
    }

    /// The host and port a token is scoped to, with the checks that hold for
    /// BOTH a host argument and a live request URL: https (or http to a
    /// loopback host), and no credentials embedded in the URL — a URL that
    /// carries its own userinfo is answering for itself, and must not have a
    /// stored token attached on top.
    fn scope_of(url: &url::Url) -> Result<HostScope> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("host is missing"))?
            .to_ascii_lowercase();
        let loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            other => bail!("scheme '{other}' is not supported (https only; http for loopback)"),
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("host must not carry credentials");
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("cannot determine the port"))?;
        Ok(HostScope { host, port })
    }

    pub fn display(&self) -> String {
        if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn storage_key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"libra-auth-v1\0https\0");
        hasher.update(self.host.as_bytes());
        hasher.update(b"\0");
        hasher.update(self.port.to_string().as_bytes());
        format!("{AUTH_TOKEN_PREFIX}{}", hex::encode(hasher.finalize()))
    }
}

/// The encrypted-at-rest record (entirely inside the ciphertext).
#[derive(Debug, Serialize, Deserialize)]
struct StoredAuthToken {
    version: u32,
    host: String,
    port: u16,
    username: String,
    token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    created_at: u64,
}

/// A non-secret status row (`auth status` / list — no token field exists).
#[derive(Debug, Clone, Serialize)]
pub struct TokenStatus {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// `valid` / `expired` / `undecryptable` (vault key rotated) /
    /// `unreadable` (OS-keyring entry missing, locked, or unavailable).
    pub state: String,
    /// Which backend holds the secret (`file` / `keyring`) — additive field.
    pub backend: String,
}

impl TokenStatus {
    pub fn host_display(&self) -> String {
        if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Outcome of a read on the network hot path.
#[derive(Debug)]
pub enum Lookup {
    Miss,
    /// Ciphertext exists but the unseal key cannot open it (key rotated).
    Undecryptable,
    Expired {
        expires_at: u64,
    },
    Valid {
        username: String,
        token: String,
    },
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// chmod-0600 repair for the secret-bearing global files (Unix; Windows
/// relies on per-user profile ACLs — the service-token precedent).
fn repair_global_modes() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [
            ConfigScope::Global.get_config_path(),
            dirs::home_dir().map(|home| home.join(".libra").join("vault-unseal-key")),
        ]
        .into_iter()
        .flatten()
        {
            if path.exists() {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

/// Marker value stored in a scope's config row when the SECRET lives in the
/// OS keyring (non-secret; enables enumeration — revocation keys the OS
/// entry directly by hash and never depends on it). Deliberately non-hex so
/// a pre-2.7 binary classifies it as undecryptable rather than a token.
const KEYRING_MARKER: &str = "keyring";

/// Which storage backend holds a scope's secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    File,
    Keyring,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::File => "file",
            BackendKind::Keyring => "keyring",
        }
    }
}

/// Resolve `auth.backend` (global config; `file` when unset). `Err` carries
/// the actionable message for the AUTH COMMAND surface; the network hot path
/// must instead degrade (see [`lookup`]).
pub async fn resolve_backend() -> Result<BackendKind> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let value = ConfigKv::get_with_conn(&conn, "auth.backend")
        .await
        .map_err(|error| anyhow!("failed to read auth.backend: {error}"))?
        .map(|entry| entry.value.trim().to_ascii_lowercase());
    match value.as_deref() {
        None | Some("") | Some("file") => Ok(BackendKind::File),
        Some("keyring") => {
            #[cfg(feature = "keyring")]
            {
                Ok(BackendKind::Keyring)
            }
            #[cfg(not(feature = "keyring"))]
            {
                bail!(
                    "auth.backend=keyring but this build has no keyring support; rebuild with \
                     --features keyring or run: libra config --global auth.backend file"
                )
            }
        }
        Some(other) => bail!("unsupported auth.backend '{other}' (expected 'file' or 'keyring')"),
    }
}

#[cfg(feature = "keyring")]
mod keyring_backend {
    //! OS-keyring storage (lore.md 2.7): service = "libra", account = the
    //! scope's storage-key hash (the hostname-hiding-at-rest property of 1.6
    //! carries over to keyring entry labels); the secret is the same
    //! StoredAuthToken JSON — the OS store IS the cipher. Calls run under
    //! spawn_blocking with a timeout: a hung D-Bus session degrades, never
    //! stalls a clone. Unavailability is cached per process (the hot path
    //! must not re-pay a 5s probe per request).

    use super::*;

    const SERVICE: &str = "libra";
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    static UNAVAILABLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    /// In-process mock store for headless tests: honored ONLY in debug
    /// builds (a stray env var must never silently swap a release user's
    /// credential store to volatile memory).
    fn mock_enabled() -> bool {
        cfg!(debug_assertions) && std::env::var_os("LIBRA_AUTH_KEYRING_MOCK").is_some()
    }

    fn mock_store() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
        static STORE: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<String, String>>,
        > = std::sync::OnceLock::new();
        STORE.get_or_init(Default::default)
    }

    async fn run_blocking<T: Send + 'static>(
        operation: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        if let Some(reason) = UNAVAILABLE.get() {
            bail!("OS keyring unavailable: {reason}");
        }
        let outcome = tokio::time::timeout(TIMEOUT, tokio::task::spawn_blocking(operation)).await;
        match outcome {
            Ok(Ok(result)) => result,
            Ok(Err(join)) => bail!("keyring worker failed: {join}"),
            Err(_) => {
                let _ = UNAVAILABLE.set("operation timed out (hung session bus?)".to_string());
                bail!("OS keyring unavailable: operation timed out")
            }
        }
    }

    pub async fn set(account: String, secret: String) -> Result<()> {
        if mock_enabled() {
            mock_store()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(account, secret);
            return Ok(());
        }
        run_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &account)
                .map_err(|error| anyhow!("keyring entry: {error}"))?;
            entry
                .set_password(&secret)
                .map_err(|error| anyhow!("keyring write failed: {error}"))
        })
        .await
    }

    pub async fn get(account: String) -> Result<Option<String>> {
        if mock_enabled() {
            return Ok(mock_store()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&account)
                .cloned());
        }
        run_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &account)
                .map_err(|error| anyhow!("keyring entry: {error}"))?;
            match entry.get_password() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(anyhow!("keyring read failed: {error}")),
            }
        })
        .await
    }

    pub async fn delete(account: String) -> Result<bool> {
        if mock_enabled() {
            return Ok(mock_store()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&account)
                .is_some());
        }
        run_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &account)
                .map_err(|error| anyhow!("keyring entry: {error}"))?;
            match entry.delete_credential() {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(error) => Err(anyhow!("keyring delete failed: {error}")),
            }
        })
        .await
    }
}

/// Store (upsert) a token for a scope. `expires_at` is unix seconds.
pub async fn store_token(
    scope: &HostScope,
    username: &str,
    token: &str,
    expires_at: Option<u64>,
) -> Result<()> {
    let record = StoredAuthToken {
        version: 1,
        host: scope.host.clone(),
        port: scope.port,
        username: username.to_string(),
        token: token.to_string(),
        expires_at,
        created_at: now_unix(),
    };
    match resolve_backend().await? {
        BackendKind::File => {
            // If this scope previously lived in the OS keyring, the old
            // secret must not survive the overwrite (an orphaned entry is
            // rediscoverable by a later keyring-enabled build).
            let conn = ScopedConfig::get_connection(ConfigScope::Global)
                .await
                .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
            let prior = ConfigKv::get_with_conn(&conn, &scope.storage_key())
                .await
                .map_err(|error| anyhow!("failed to read the token record: {error}"))?;
            if prior.is_some_and(|entry| entry.value == KEYRING_MARKER) {
                #[cfg(feature = "keyring")]
                {
                    keyring_backend::delete(scope.storage_key()).await?;
                }
                #[cfg(not(feature = "keyring"))]
                {
                    bail!(
                        "this scope's token is stored in the OS keyring, which this build \
                         cannot open; rebuild with --features keyring (or remove the entry \
                         via the OS keychain UI) before overwriting it"
                    );
                }
            }
            store_record_file(scope, &record).await
        }
        #[cfg(feature = "keyring")]
        BackendKind::Keyring => {
            let secret =
                serde_json::to_string(&record).context("failed to serialize the auth record")?;
            keyring_backend::set(scope.storage_key(), secret).await?;
            // Non-secret marker row: enables enumeration; a pre-2.7 binary
            // classifies it as undecryptable (non-hex — pinned by test).
            let conn = ScopedConfig::get_connection(ConfigScope::Global)
                .await
                .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
            ConfigKv::set_with_conn(&conn, &scope.storage_key(), KEYRING_MARKER, false)
                .await
                .map_err(|error| anyhow!("failed to persist the keyring marker: {error}"))?;
            repair_global_modes();
            Ok(())
        }
        #[cfg(not(feature = "keyring"))]
        BackendKind::Keyring => unreachable!("resolve_backend refuses keyring without the feature"),
    }
}

/// The 1.6 file backend: vault-encrypted record in the global config store.
async fn store_record_file(scope: &HostScope, record: &StoredAuthToken) -> Result<()> {
    let unseal_key = vault::lazy_init_vault_for_scope("global")
        .await
        .map_err(|_| anyhow!("failed to initialize the global vault key"))?;
    let plaintext = serde_json::to_vec(record).context("failed to serialize the auth record")?;
    let encrypted = vault::encrypt_token(&unseal_key, &plaintext)
        // The vault error never contains the secret.
        .map_err(|_| anyhow!("failed to encrypt the token"))?;
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    // Pre-encrypted hex with secret=false — the credential.rs precedent (the
    // config `encrypted` flag drives the vault door, which this bypasses).
    ConfigKv::set_with_conn(&conn, &scope.storage_key(), &hex::encode(encrypted), false)
        .await
        .map_err(|error| anyhow!("failed to persist the token record: {error}"))?;
    repair_global_modes();
    Ok(())
}

async fn read_record(scope: &HostScope) -> Result<Option<Result<StoredAuthToken, ()>>> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let entry = ConfigKv::get_with_conn(&conn, &scope.storage_key())
        .await
        .map_err(|error| anyhow!("failed to read the token record: {error}"))?;
    let Some(entry) = entry else {
        return Ok(None);
    };
    if entry.value == KEYRING_MARKER {
        // The secret lives in the OS keyring.
        #[cfg(feature = "keyring")]
        {
            return match keyring_backend::get(scope.storage_key()).await {
                Ok(Some(secret)) => Ok(Some(serde_json::from_str(&secret).map_err(|_| ()))),
                // Missing/locked/down: 'unreadable', reported as a decrypt
                // failure at this layer (list distinguishes the states).
                Ok(None) | Err(_) => Ok(Some(Err(()))),
            };
        }
        #[cfg(not(feature = "keyring"))]
        {
            // A featureless binary cannot open the OS store.
            return Ok(Some(Err(())));
        }
    }
    Ok(Some(decrypt_record(&entry.value).await))
}

async fn decrypt_record(cipher_hex: &str) -> Result<StoredAuthToken, ()> {
    let Ok(cipher) = hex::decode(cipher_hex) else {
        return Err(());
    };
    let Ok(unseal_key) = vault::lazy_init_vault_for_scope("global").await else {
        return Err(());
    };
    let Ok(plaintext) = vault::decrypt_token(&unseal_key, &cipher) else {
        return Err(());
    };
    serde_json::from_str(&plaintext).map_err(|_| ())
}

/// The network-hot-path read: never errors loudly (a broken store must not
/// take down an unauthenticated clone) — degrades to `Miss`.
pub async fn lookup(scope: &HostScope) -> Lookup {
    match read_record(scope).await {
        Ok(None) => Lookup::Miss,
        Ok(Some(Err(()))) => Lookup::Undecryptable,
        Ok(Some(Ok(record))) => match record.expires_at {
            Some(expires_at) if expires_at <= now_unix() => Lookup::Expired { expires_at },
            _ => Lookup::Valid {
                username: record.username,
                token: record.token,
            },
        },
        Err(_) => Lookup::Miss,
    }
}

/// Remove one scope's token (idempotent; works without decryption so revoke
/// survives key rotation). Returns whether something was removed.
pub async fn remove(scope: &HostScope) -> Result<bool> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    // Revocation must reach BOTH backends (a revoked token that still
    // attaches is a defect). A featureless binary REFUSES to remove a
    // keyring-marked scope — deleting only the marker would leave the OS
    // secret alive and rediscoverable (never report success on a
    // half-revoke). The marker row is deleted AFTER the OS entry.
    let entry = ConfigKv::get_with_conn(&conn, &scope.storage_key())
        .await
        .map_err(|error| anyhow!("failed to read the token record: {error}"))?;
    if entry
        .as_ref()
        .is_some_and(|entry| entry.value == KEYRING_MARKER)
    {
        #[cfg(feature = "keyring")]
        {
            keyring_backend::delete(scope.storage_key()).await?;
        }
        #[cfg(not(feature = "keyring"))]
        {
            bail!(
                "this token is stored in the OS keyring, which this build cannot open; \
                 rebuild with --features keyring (or delete the 'libra' entry via the OS \
                 keychain UI, then retry)"
            );
        }
    }
    let rows = ConfigKv::unset_all_with_conn(&conn, &scope.storage_key())
        .await
        .map_err(|error| anyhow!("failed to remove the token record: {error}"))?;
    Ok(rows > 0)
}

/// Remove every stored token. Returns the count.
pub async fn remove_all() -> Result<usize> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let entries = ConfigKv::list_all_with_conn(&conn)
        .await
        .map_err(|error| anyhow!("failed to list the global config store: {error}"))?;
    // PRE-SCAN (featureless builds): refuse BEFORE deleting anything if any
    // scope is keyring-marked — a partial clear that reports progress while
    // OS secrets stay alive is a half-revoke.
    #[cfg(not(feature = "keyring"))]
    if entries
        .iter()
        .any(|entry| entry.key.starts_with(AUTH_TOKEN_PREFIX) && entry.value == KEYRING_MARKER)
    {
        bail!(
            "at least one token is stored in the OS keyring, which this build cannot open; \
             rebuild with --features keyring to revoke it (nothing was removed)"
        );
    }
    let mut removed = 0usize;
    for entry in entries {
        if !entry.key.starts_with(AUTH_TOKEN_PREFIX) {
            continue;
        }
        #[cfg(feature = "keyring")]
        if entry.value == KEYRING_MARKER {
            keyring_backend::delete(entry.key.clone()).await?;
        }
        removed += ConfigKv::unset_all_with_conn(&conn, &entry.key)
            .await
            .map_err(|error| anyhow!("failed to remove a token record: {error}"))?
            as usize;
    }
    Ok(removed)
}

/// Migrate every readable token to `target` (lore.md 2.7): probe target
/// availability first (throwaway probe entry with a FIXED account name that
/// is garbage-collected at start — crash residue converges), then per scope
/// read → write target → VERIFY readback → delete source. Idempotent; a
/// crash mid-scope leaves the token readable from at least one backend
/// (lookup consults the marker/format, not `auth.backend`). Sets
/// `auth.backend` on success. Returns the moved count; never token material.
pub async fn migrate_tokens(target: BackendKind) -> Result<usize> {
    #[cfg(not(feature = "keyring"))]
    if target == BackendKind::Keyring {
        bail!(
            "this build has no keyring support; rebuild with --features keyring or migrate \
             --to file"
        );
    }
    // Target availability probe.
    #[cfg(feature = "keyring")]
    if target == BackendKind::Keyring {
        const PROBE: &str = "auth.token.migrate-probe";
        let _ = keyring_backend::delete(PROBE.to_string()).await; // GC residue
        keyring_backend::set(PROBE.to_string(), "probe".to_string())
            .await
            .context("the OS keyring is not available (probe write failed)")?;
        let read = keyring_backend::get(PROBE.to_string())
            .await
            .context("the OS keyring is not available (probe read failed)")?;
        let _ = keyring_backend::delete(PROBE.to_string()).await;
        if read.as_deref() != Some("probe") {
            bail!("the OS keyring probe did not read back");
        }
    }
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let entries = ConfigKv::list_all_with_conn(&conn)
        .await
        .map_err(|error| anyhow!("failed to list the global config store: {error}"))?;
    let mut moved = 0usize;
    for entry in entries {
        if !entry.key.starts_with(AUTH_TOKEN_PREFIX) || entry.key.ends_with("migrate-probe") {
            continue;
        }
        let source_is_keyring = entry.value == KEYRING_MARKER;
        // Read the record from its CURRENT home.
        let record: Option<StoredAuthToken> = if source_is_keyring {
            #[cfg(feature = "keyring")]
            {
                keyring_backend::get(entry.key.clone())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|secret| serde_json::from_str(&secret).ok())
            }
            #[cfg(not(feature = "keyring"))]
            {
                bail!(
                    "a token is stored in the OS keyring, which this build cannot open; \
                     rebuild with --features keyring"
                );
            }
        } else {
            decrypt_record(&entry.value).await.ok()
        };
        let Some(record) = record else {
            continue; // unreadable rows stay put (reported by status)
        };
        match target {
            BackendKind::File if source_is_keyring => {
                let scope = HostScope {
                    host: record.host.clone(),
                    port: record.port,
                };
                store_record_file(&scope, &record).await?;
                // Verify readback via the file path before deleting the
                // keyring copy.
                if decrypt_record(
                    &ConfigKv::get_with_conn(&conn, &entry.key)
                        .await
                        .map_err(|error| anyhow!("verify read failed: {error}"))?
                        .map(|row| row.value)
                        .unwrap_or_default(),
                )
                .await
                .is_err()
                {
                    bail!("migration verify-readback failed; the keyring copy was kept");
                }
                #[cfg(feature = "keyring")]
                keyring_backend::delete(entry.key.clone()).await?;
                moved += 1;
            }
            #[cfg(feature = "keyring")]
            BackendKind::Keyring if !source_is_keyring => {
                let secret = serde_json::to_string(&record)
                    .context("failed to serialize the auth record")?;
                keyring_backend::set(entry.key.clone(), secret).await?;
                let read = keyring_backend::get(entry.key.clone()).await?;
                if read
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<StoredAuthToken>(text).ok())
                    .is_none()
                {
                    bail!("migration verify-readback failed; the file copy was kept");
                }
                ConfigKv::set_with_conn(&conn, &entry.key, KEYRING_MARKER, false)
                    .await
                    .map_err(|error| anyhow!("failed to write the keyring marker: {error}"))?;
                moved += 1;
            }
            _ => {} // already home
        }
    }
    ConfigKv::set_with_conn(&conn, "auth.backend", target.as_str(), false)
        .await
        .map_err(|error| anyhow!("failed to set auth.backend: {error}"))?;
    Ok(moved)
}

/// Status rows for every stored token (undecryptable entries reported, not
/// hidden). No token material is ever included.
pub async fn list() -> Result<Vec<TokenStatus>> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let entries = ConfigKv::list_all_with_conn(&conn)
        .await
        .map_err(|error| anyhow!("failed to list the global config store: {error}"))?;
    let now = now_unix();
    let mut rows = Vec::new();
    for entry in entries {
        if !entry.key.starts_with(AUTH_TOKEN_PREFIX) {
            continue;
        }
        if entry.value == KEYRING_MARKER {
            // Secret lives in the OS keyring.
            #[cfg(feature = "keyring")]
            let loaded: Option<StoredAuthToken> = keyring_backend::get(entry.key.clone())
                .await
                .ok()
                .flatten()
                .and_then(|secret| serde_json::from_str(&secret).ok());
            #[cfg(not(feature = "keyring"))]
            let loaded: Option<StoredAuthToken> = None;
            match loaded {
                Some(record) => {
                    let state = match record.expires_at {
                        Some(expires_at) if expires_at <= now => "expired",
                        _ => "valid",
                    };
                    rows.push(TokenStatus {
                        host: record.host,
                        port: record.port,
                        username: record.username,
                        created_at: record.created_at,
                        expires_at: record.expires_at,
                        state: state.to_string(),
                        backend: "keyring".to_string(),
                    });
                }
                None => rows.push(TokenStatus {
                    // The marker is a hash — no hostname is recoverable.
                    host: "<unreadable>".to_string(),
                    port: 0,
                    username: String::new(),
                    created_at: 0,
                    expires_at: None,
                    state: "unreadable".to_string(),
                    backend: "keyring".to_string(),
                }),
            }
            continue;
        }
        match decrypt_record(&entry.value).await {
            Ok(record) => {
                let state = match record.expires_at {
                    Some(expires_at) if expires_at <= now => "expired",
                    _ => "valid",
                };
                rows.push(TokenStatus {
                    host: record.host,
                    port: record.port,
                    username: record.username,
                    created_at: record.created_at,
                    expires_at: record.expires_at,
                    state: state.to_string(),
                    backend: "file".to_string(),
                });
            }
            Err(()) => rows.push(TokenStatus {
                host: "<undecryptable>".to_string(),
                port: 0,
                username: String::new(),
                created_at: 0,
                expires_at: None,
                state: "undecryptable".to_string(),
                backend: "file".to_string(),
            }),
        }
    }
    rows.sort_by(|a, b| a.host.cmp(&b.host).then(a.port.cmp(&b.port)));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_scope_parse_matrix() {
        // Bare host, host:port, and full URL all normalize.
        assert_eq!(
            HostScope::parse("Git.Example.COM").unwrap(),
            HostScope {
                host: "git.example.com".to_string(),
                port: 443
            }
        );
        assert_eq!(HostScope::parse("git.example.com:8443").unwrap().port, 8443);
        assert_eq!(
            HostScope::parse("https://git.example.com:8443/")
                .unwrap()
                .port,
            8443
        );
        // Loopback http is allowed; non-loopback http refused.
        assert!(HostScope::parse("http://localhost:8000").is_ok());
        assert!(HostScope::parse("http://127.0.0.1:8000").is_ok());
        assert!(HostScope::parse("http://git.example.com").is_err());
        // Junk refused.
        for bad in [
            "",
            "https://user:pw@host",
            "https://host/path/repo",
            "https://host?q=1",
            "ssh://host",
        ] {
            assert!(HostScope::parse(bad).is_err(), "{bad}");
        }
    }

    /// A stored token is worthless if the scope of a live REQUEST cannot be
    /// computed. Smart-HTTP discovery URLs carry a path and a query, which the
    /// host-argument parser refuses by design — sharing that parser meant
    /// `from_request_url` returned `None` for every real request, so
    /// `libra push` over HTTPS failed with `LBR-AUTH-001` however good the
    /// stored token was, and the 401 hint printed a literal `<host>`.
    #[test]
    fn request_url_scope_survives_paths_and_queries() {
        let discovery = url::Url::parse(
            "https://github.com/libra-tools/libra.git/info/refs?service=git-receive-pack",
        )
        .unwrap();
        assert_eq!(
            HostScope::from_request_url(&discovery),
            Some(HostScope {
                host: "github.com".to_string(),
                port: 443,
            })
        );

        // Non-default ports keep their identity: a token stored for
        // `host:8443` must not be attached to `host:443`.
        let alt = url::Url::parse("https://git.example.com:8443/o/r.git/git-upload-pack").unwrap();
        assert_eq!(HostScope::from_request_url(&alt).unwrap().port, 8443);

        // The checks that DO apply to a request survive: cleartext to a
        // non-loopback host, and a URL that carries its own credentials, get
        // no stored token attached.
        for refused in [
            "http://git.example.com/o/r.git/info/refs",
            "https://user:pw@git.example.com/o/r.git/info/refs",
            "ssh://git.example.com/o/r.git",
        ] {
            let url = url::Url::parse(refused).unwrap();
            assert!(
                HostScope::from_request_url(&url).is_none(),
                "{refused} must not resolve to a token scope"
            );
        }

        // Loopback http stays eligible, which is what local dev servers use.
        let local = url::Url::parse("http://127.0.0.1:8000/o/r.git/info/refs").unwrap();
        assert_eq!(HostScope::from_request_url(&local).unwrap().port, 8000);

        // And the host-argument parser still refuses paths and queries.
        assert!(HostScope::parse("https://host/path/repo").is_err());
    }

    #[test]
    fn display_elides_default_port() {
        assert_eq!(
            HostScope::parse("h.example").unwrap().display(),
            "h.example"
        );
        assert_eq!(
            HostScope::parse("h.example:8443").unwrap().display(),
            "h.example:8443"
        );
    }
}
