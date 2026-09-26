//! `libra credential fill | store | erase` — a vault-backed Git credential
//! helper. Speaks the Git credential key/value protocol on stdin/stdout and
//! stores secrets AES-256-GCM-encrypted (via the repository vault), keyed by a
//! digest of `protocol/host/path` so the stored config never reveals the host
//! or username in clear text.
//!
//! Security invariants (see GGT-08):
//! - Passwords/tokens are NEVER logged, traced, or echoed in error messages.
//!   Errors only ever mention non-secret routing fields (protocol/host).
//! - `fill` is side-channel free: a hit and a miss both exit 0; a miss prints
//!   nothing, so the exit code / output shape never reveal whether an entry
//!   exists.
//! - Stored credentials carry an expiry; expired entries are treated as a miss
//!   on `fill`, and `store` refuses an already-expired `password_expiry_utc`.

use std::{
    collections::BTreeMap,
    io::{self, BufRead},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    command::config::{ConfigScope, ScopedConfig},
    internal::{config::ConfigKv, vault},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        util,
    },
};

/// Default credential lifetime when the protocol does not supply
/// `password_expiry_utc` (30 days), in seconds.
const DEFAULT_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

pub const CREDENTIAL_EXAMPLES: &str = "\
EXAMPLES:
    printf 'protocol=https\\nhost=example.com\\n' | libra credential get
    printf 'protocol=https\\nhost=example.com\\nusername=u\\npassword=p\\n' | libra credential store
    printf 'protocol=https\\nhost=example.com\\n' | libra credential erase

    Configure as a Git helper:  credential.helper = '!libra credential'";

/// Vault-backed credential helper (Git credential protocol on stdin/stdout).
#[derive(Parser, Debug)]
#[command(after_help = CREDENTIAL_EXAMPLES)]
pub struct CredentialArgs {
    /// The helper operation. Git calls `get`/`store`/`erase`; `fill` is the
    /// legacy Libra name for `get`. An unknown or absent operation is silently
    /// ignored (Git helper convention), exiting 0 with no output.
    #[arg(value_name = "OPERATION")]
    pub operation: Option<String>,
}

/// Parsed Git credential attributes (only the fields this helper uses).
#[derive(Debug, Default)]
struct CredentialAttrs {
    protocol: String,
    host: String,
    path: String,
    username: Option<String>,
    password: Option<String>,
    /// `password_expiry_utc` — a unix timestamp after which the password is
    /// invalid (Git 2.30+).
    password_expiry_utc: Option<u64>,
}

/// The encrypted-at-rest credential record.
#[derive(Debug, Serialize, Deserialize)]
struct StoredCredential {
    username: String,
    password: String,
    /// Unix timestamp after which this entry is ignored.
    expires_at: u64,
}

pub async fn execute(args: CredentialArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Note: this helper deliberately ignores `--json` — it speaks
/// the fixed Git credential key/value protocol so its stdout stays parseable by
/// Git.
pub async fn execute_safe(args: CredentialArgs, _output: &OutputConfig) -> CliResult<()> {
    let attrs = read_attrs(&read_request()?)?;
    match args.operation.as_deref() {
        // Git's helper protocol operation for a fill is `get`; `fill` is the
        // legacy Libra spelling and is accepted as an alias.
        Some("get") | Some("fill") => fill(&attrs).await,
        Some("store") => store(&attrs).await,
        Some("erase") => erase(&attrs).await,
        // An unknown or absent operation is silently ignored (Git helper
        // convention) rather than treated as a usage error.
        _ => Ok(()),
    }
}

/// `fill`: print the stored credential, or nothing. Always exits 0 — a miss is
/// indistinguishable from "no vault" or "expired" from the caller's side.
async fn fill(attrs: &CredentialAttrs) -> CliResult<()> {
    match repo_scoped_credential(attrs).await {
        Some(stored) => emit_fill(
            attrs,
            &stored.username,
            &stored.password,
            Some(stored.expires_at),
        ),
        // User-level store: available outside a repository (HP-14), so the
        // helper is usable as a global `credential.helper`.
        None => match user_scoped_credential(attrs).await {
            Some(stored) => emit_fill(
                attrs,
                &stored.username,
                &stored.password,
                Some(stored.expires_at),
            ),
            // Global-token fallback (lore.md 1.6, gh-style): https only, silent
            // on every miss — store/erase never manage auth tokens.
            None => {
                if attrs.protocol == "https"
                    && !attrs.host.is_empty()
                    && let Ok(scope) = crate::internal::auth::HostScope::parse(&attrs.host)
                    && let crate::internal::auth::Lookup::Valid { username, token } =
                        crate::internal::auth::lookup(&scope).await
                {
                    // Username pinning applies to the fallback too.
                    if let Some(asked) = &attrs.username
                        && asked != &username
                    {
                        return Ok(());
                    }
                    return emit_fill(attrs, &username, &token, None);
                }
                Ok(())
            }
        },
    }
}

/// The pre-1.6 repo-scoped lookup, unchanged in semantics.
async fn repo_scoped_credential(attrs: &CredentialAttrs) -> Option<StoredCredential> {
    // Outside a repository there is no vault to consult — a clean miss, not an
    // error (the vault loader panics if called outside a repo).
    util::try_get_storage_path(None).ok()?;
    let unseal_key = vault::load_unseal_key().await?;
    let entry = ConfigKv::get(&credential_key(attrs)).await.ok().flatten()?;
    let raw = hex::decode(entry.value).ok()?;
    // A decryption failure (e.g. the vault unseal key was rotated) is a miss,
    // not an error: the caller is asked to re-authenticate.
    let plaintext = vault::decrypt_token(&unseal_key, &raw).ok()?;
    let stored = serde_json::from_str::<StoredCredential>(&plaintext).ok()?;
    if stored.expires_at <= now_unix() {
        return None;
    }
    // If the caller pinned a username, it must match the stored one.
    if let Some(asked) = &attrs.username
        && asked != &stored.username
    {
        return None;
    }
    Some(stored)
}

/// Read a credential from the user-level store (HP-14). Available outside a
/// repository so the helper can act as a global `credential.helper`. The
/// record is vault-encrypted (global unseal key) and stored in the global
/// config store; a miss/decrypt-failure returns `None` (not an error).
async fn user_scoped_credential(attrs: &CredentialAttrs) -> Option<StoredCredential> {
    let db = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .ok()?;
    let entry = ConfigKv::get_with_conn(&db, &credential_key(attrs))
        .await
        .ok()
        .flatten()?;
    let raw = hex::decode(entry.value).ok()?;
    let unseal_key = vault::load_unseal_key_for_scope("global").await?;
    let plaintext = vault::decrypt_token(&unseal_key, &raw).ok()?;
    let stored = serde_json::from_str::<StoredCredential>(&plaintext).ok()?;
    if stored.expires_at <= now_unix() {
        return None;
    }
    if let Some(asked) = &attrs.username
        && asked != &stored.username
    {
        return None;
    }
    Some(stored)
}

/// Encrypt and persist a credential to the user-level (global) store (HP-14).
async fn user_store_encrypted(attrs: &CredentialAttrs, plaintext: &[u8]) -> CliResult<()> {
    let db = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|e| {
            CliError::fatal(format!("failed to open the global config store: {e}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
    let unseal_key = vault::lazy_init_vault_for_scope("global")
        .await
        .map_err(|e| {
            CliError::fatal(format!("failed to initialize the global vault: {e}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
    let encrypted = vault::encrypt_token(&unseal_key, plaintext).map_err(|_| {
        // The vault error never contains the secret.
        CliError::fatal("failed to encrypt the credential")
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::InternalInvariant)
    })?;
    ConfigKv::set_with_conn(&db, &credential_key(attrs), &hex::encode(encrypted), false)
        .await
        .map_err(|_| {
            CliError::fatal("failed to persist the user-level credential")
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    Ok(())
}

/// Erase a credential from the user-level (global) store, idempotently (HP-14).
async fn user_erase(attrs: &CredentialAttrs) -> CliResult<()> {
    let db = match ScopedConfig::get_connection(ConfigScope::Global).await {
        Ok(db) => db,
        // No global config: nothing to erase.
        Err(_) => return Ok(()),
    };
    ConfigKv::unset_with_conn(&db, &credential_key(attrs))
        .await
        .map_err(|_| {
            CliError::fatal("failed to erase the user-level credential")
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    Ok(())
}

fn emit_fill(
    attrs: &CredentialAttrs,
    username: &str,
    password: &str,
    expires_at: Option<u64>,
) -> CliResult<()> {
    let mut response = String::new();
    if !attrs.protocol.is_empty() {
        response.push_str(&format!("protocol={}\n", attrs.protocol));
    }
    if !attrs.host.is_empty() {
        response.push_str(&format!("host={}\n", attrs.host));
    }
    if !attrs.path.is_empty() {
        response.push_str(&format!("path={}\n", attrs.path));
    }
    response.push_str(&format!("username={username}\n"));
    response.push_str(&format!("password={password}\n"));
    if let Some(expires_at) = expires_at {
        response.push_str(&format!("password_expiry_utc={expires_at}\n"));
    }
    print!("{response}");
    Ok(())
}

/// `store`: encrypt and persist the supplied credential.
async fn store(attrs: &CredentialAttrs) -> CliResult<()> {
    let (Some(username), Some(password)) = (&attrs.username, &attrs.password) else {
        // No secret echoed — only the routing context.
        return Err(routing_error(
            attrs,
            "store requires both username and password on stdin",
        ));
    };

    let now = now_unix();
    let expires_at = match attrs.password_expiry_utc {
        Some(expiry) if expiry <= now => {
            return Err(routing_error(
                attrs,
                "refusing to store an already-expired credential (password_expiry_utc is in the past)",
            ));
        }
        Some(expiry) => expiry,
        None => now.saturating_add(DEFAULT_TTL_SECONDS),
    };

    let record = StoredCredential {
        username: username.clone(),
        password: password.clone(),
        expires_at,
    };
    let plaintext = serde_json::to_vec(&record).map_err(|_| {
        CliError::fatal("failed to serialize the credential record")
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::InternalInvariant)
    })?;

    if util::try_get_storage_path(None).is_ok() {
        let unseal_key = vault::load_unseal_key().await.ok_or_else(|| {
            CliError::fatal("the repository vault is not initialized; cannot store credentials")
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
        let encrypted = vault::encrypt_token(&unseal_key, &plaintext).map_err(|_| {
            // The vault error never contains the secret.
            CliError::fatal("failed to encrypt the credential")
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
        })?;
        // Store the pre-encrypted hex as-is (secret = false, matching how the
        // vault root token is stored).
        ConfigKv::set(&credential_key(attrs), &hex::encode(encrypted), false)
            .await
            .map_err(|_| {
                CliError::fatal("failed to persist the credential")
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
    } else {
        // Outside a repository: user-level store (HP-14), so the helper is
        // usable as a global `credential.helper` (Git parity).
        user_store_encrypted(attrs, &plaintext).await?;
    }
    Ok(())
}

/// `erase`: remove the credential for the requested context (idempotent).
async fn erase(attrs: &CredentialAttrs) -> CliResult<()> {
    // Erase from the repository store if present (idempotent).
    if util::try_get_storage_path(None).is_ok() {
        // `unset_all` returns Ok(0) when there is nothing to delete (idempotent);
        // a real storage error must surface rather than be silently swallowed.
        ConfigKv::unset_all(&credential_key(attrs))
            .await
            .map_err(|_| {
                CliError::fatal("failed to erase the credential")
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
    }
    // Also erase from the user-level store (HP-14).
    user_erase(attrs).await
}

/// Build a non-reversible config key from the routing fields so the stored
/// config never contains the host/username in clear text.
fn credential_key(attrs: &CredentialAttrs) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"libra-credential-v1\0");
    hasher.update(attrs.protocol.as_bytes());
    hasher.update(b"\0");
    hasher.update(attrs.host.as_bytes());
    hasher.update(b"\0");
    hasher.update(attrs.path.as_bytes());
    format!("credential.{}", hex::encode(hasher.finalize()))
}

/// Build an error that names only the non-secret routing context.
fn routing_error(attrs: &CredentialAttrs, message: &str) -> CliError {
    let context = if attrs.host.is_empty() {
        message.to_string()
    } else {
        format!("{message} (for {}://{})", attrs.protocol, attrs.host)
    };
    CliError::command_usage(context)
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

/// Read the credential request line by line, stopping at the first blank line
/// (the protocol terminator) or EOF. A `read_to_string` would deadlock, because
/// Git keeps the helper's stdin open after the blank line.
fn read_request() -> CliResult<String> {
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut request = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = handle.read_line(&mut line).map_err(|error| {
            CliError::fatal(format!("failed to read credential request: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        if read == 0 {
            break; // EOF
        }
        if line.trim_end_matches(['\n', '\r']).is_empty() {
            break; // blank line terminates the request
        }
        request.push_str(&line);
    }
    Ok(request)
}

/// Parse the Git credential key/value protocol. Reading stops at the first
/// blank line (the protocol terminator). `url=` is expanded into
/// protocol/host/path; explicit fields take precedence.
fn read_attrs(input: &str) -> CliResult<CredentialAttrs> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for line in input.lines() {
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        fields.insert(key.to_string(), value.to_string());
    }

    let mut attrs = CredentialAttrs::default();
    if let Some(url) = fields.get("url") {
        apply_url(&mut attrs, url);
    }
    if let Some(protocol) = fields.get("protocol") {
        attrs.protocol = protocol.clone();
    }
    if let Some(host) = fields.get("host") {
        attrs.host = host.clone();
    }
    if let Some(path) = fields.get("path") {
        attrs.path = path.clone();
    }
    attrs.username = fields.get("username").filter(|v| !v.is_empty()).cloned();
    attrs.password = fields.get("password").filter(|v| !v.is_empty()).cloned();
    attrs.password_expiry_utc = fields
        .get("password_expiry_utc")
        .and_then(|value| value.parse::<u64>().ok());
    Ok(attrs)
}

/// Best-effort split of a `url=` value into protocol/host/path.
fn apply_url(attrs: &mut CredentialAttrs, url: &str) {
    if let Some((protocol, rest)) = url.split_once("://") {
        attrs.protocol = protocol.to_string();
        let rest = rest.split_once('@').map(|(_, after)| after).unwrap_or(rest);
        if let Some((host, path)) = rest.split_once('/') {
            attrs.host = host.to_string();
            attrs.path = path.to_string();
        } else {
            attrs.host = rest.to_string();
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_is_split_into_routing_fields() {
        let attrs = read_attrs("url=https://user@example.com/org/repo.git\n").unwrap();
        assert_eq!(attrs.protocol, "https");
        assert_eq!(attrs.host, "example.com");
        assert_eq!(attrs.path, "org/repo.git");
    }

    #[test]
    fn explicit_fields_override_url_and_parsing_stops_at_blank_line() {
        let attrs =
            read_attrs("url=https://example.com\nhost=override.example\n\nhost=ignored\n").unwrap();
        assert_eq!(attrs.host, "override.example");
    }

    #[test]
    fn credential_key_is_stable_and_hashed() {
        let a = CredentialAttrs {
            protocol: "https".into(),
            host: "example.com".into(),
            ..Default::default()
        };
        let key = credential_key(&a);
        assert!(key.starts_with("credential."));
        // The host never appears in clear text in the key.
        assert!(!key.contains("example.com"));
        // Deterministic.
        assert_eq!(key, credential_key(&a));
    }

    #[test]
    fn empty_password_is_treated_as_absent() {
        let attrs = read_attrs("protocol=https\nhost=h\nusername=u\npassword=\n").unwrap();
        assert_eq!(attrs.username.as_deref(), Some("u"));
        assert_eq!(attrs.password, None);
    }
}
