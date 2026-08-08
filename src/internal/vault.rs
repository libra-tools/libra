//! Vault integration module wrapping libvault for PGP key management.
//!
//! Provides helpers to initialize a libvault instance backed by the repository's
//! `.libra/vault.db` SQLite database, generate PGP keys, sign data, and verify
//! signatures. The vault state (sealed/unsealed) is managed transparently.
//!
//! # Secret handling
//!
//! The unseal key is stored hex-encoded in the user's home directory
//! (`~/.libra/vault-keys/<repo-id>`) — outside the repository — so that
//! anyone with read access to the repo config alone cannot recover the root
//! token. The root token is encrypted (AES-256-GCM) with a key derived from
//! the unseal key before being persisted in the repo config
//! (`vault.roottoken_enc`). It is never stored in plaintext.
//!
//! # Threat model
//!
//! This design protects against casual repo-level read access (e.g. a
//! colleague cloning the repo, or a backup leak). It does NOT protect
//! against full compromise of the user's machine — an attacker with access
//! to both `~/.libra/` and the repo can recover the root token. For
//! stronger guarantees, integrate an OS keychain or hardware token.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use libvault::{
    RustyVault,
    core::SealConfig,
    errors::RvError,
    storage::{Backend, BackendEntry, sql::sqlite::SqliteBackend},
};
use sea_orm::sqlx::{AssertSqlSafe, SqlitePool, query_scalar, sqlite::SqliteConnectOptions};
use serde_json::Value;

use crate::utils::util::try_get_storage_path;

const VAULT_DB_NAME: &str = "vault.db";
const PGP_KEY_NAME: &str = "libra-signing";
const SSH_ROLE_NAME: &str = "libra-ssh";
const PKI_MOUNT_PATH: &str = "pki";

fn vault_home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("LIBRA_TEST_HOME") {
        return Some(PathBuf::from(path));
    }

    dirs::home_dir()
}

// ── Encryption helpers for root token ──

/// Derive a 256-bit AES key from the raw unseal key using HKDF-SHA256.
fn derive_token_key(unseal_key: &[u8]) -> Result<ring::aead::LessSafeKey> {
    use ring::{aead, hkdf};
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"libra-vault-token-enc");
    let prk = salt.extract(unseal_key);
    let okm = prk
        .expand(&[b"token-encryption"], &aead::AES_256_GCM)
        .map_err(|_| anyhow!("failed to derive vault token encryption key"))?;
    let key_bytes: aead::UnboundKey = okm.into();
    Ok(aead::LessSafeKey::new(key_bytes))
}

/// Encrypt `plaintext` with AES-256-GCM using a key derived from `unseal_key`.
/// Returns `nonce || ciphertext || tag` as a single byte vector.
pub fn encrypt_token(unseal_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    use ring::{
        aead,
        rand::{SecureRandom, SystemRandom},
    };

    let key = derive_token_key(unseal_key)?;
    let rng = SystemRandom::new();
    let mut nonce_bytes = [0u8; 12];
    rng.fill(&mut nonce_bytes)
        .map_err(|_| anyhow!("failed to generate nonce for vault token encryption"))?;
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);

    let mut in_out = plaintext.to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|_| anyhow!("failed to encrypt vault root token"))?;

    let mut result = nonce_bytes.to_vec();
    result.extend(in_out);
    Ok(result)
}

/// Decrypt `nonce || ciphertext || tag` with AES-256-GCM.
pub fn decrypt_token(unseal_key: &[u8], data: &[u8]) -> Result<String> {
    use ring::aead;

    if data.len() < 12 + aead::AES_256_GCM.tag_len() {
        return Err(anyhow!("encrypted token data too short"));
    }
    let (nonce_bytes, ciphertext_and_tag) = data.split_at(12);
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce_bytes)
        .map_err(|_| anyhow!("invalid nonce"))?;
    let key = derive_token_key(unseal_key)?;
    let mut buf = ciphertext_and_tag.to_vec();
    let plaintext = key
        .open_in_place(nonce, aead::Aad::empty(), &mut buf)
        .map_err(|_| anyhow!("failed to decrypt root token — unseal key may be wrong"))?;
    String::from_utf8(plaintext.to_vec()).context("root token is not valid UTF-8")
}

/// Initialize a new vault instance backed by the given `.libra` directory.
///
/// Creates `vault.db` inside `root_dir`, initializes the vault with a single
/// unseal key (threshold=1, shares=1), mounts the PKI engine, and returns
/// `(unseal_key, encrypted_root_token)`.
#[allow(dead_code)]
pub async fn init_vault(root_dir: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let vault = create_vault(root_dir).await?;

    let seal_config = SealConfig {
        secret_shares: 1,
        secret_threshold: 1,
    };
    let init_result = vault
        .init(&seal_config)
        .await
        .map_err(|e| anyhow!("vault init failed: {e}"))?;

    let unseal_key = init_result
        .secret_shares
        .first()
        .ok_or_else(|| anyhow!("no unseal key generated"))?
        .clone();

    let root_token = init_result.root_token.clone();

    vault
        .unseal(&[unseal_key.as_slice()])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    vault.set_token(&root_token);

    let pki = PKI_MOUNT_PATH.to_string();
    vault
        .mount(Some(root_token.clone()), pki.clone(), pki)
        .await
        .map_err(|e| anyhow!("vault mount pki failed: {e}"))?;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    let enc_token = encrypt_token(&unseal_key, root_token.as_bytes())?;
    Ok((unseal_key, enc_token))
}

/// Generate a PGP key pair in the vault for commit signing.
#[allow(dead_code)]
pub async fn generate_pgp_key(
    root_dir: &Path,
    unseal_key: &[u8],
    user_name: &str,
    user_email: &str,
) -> Result<String> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let data = serde_json::json!({
        "key_name": PGP_KEY_NAME,
        "key_type": "pgp",
        "name": user_name,
        "email": user_email,
        "key_bits": 2048,
        "ttl": "3650d",
    });

    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/keys/generate/internal"),
            data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault pgp key generation failed: {e}"))?;

    let public_key = resp
        .and_then(|r| r.data)
        .and_then(|d| d.get("public_key").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .ok_or_else(|| anyhow!("no public key in vault response"))?;

    // Store in config so it can be exported without requiring backend-specific
    // read-path support.
    upsert_config_value("vault.gpg.pubkey", &public_key).await;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(public_key)
}

/// Sign data using the vault's PGP key.
///
/// `data` is the raw bytes to sign. Returns the hex-encoded detached signature.
pub async fn pgp_sign(root_dir: &Path, unseal_key: &[u8], data: &[u8]) -> Result<String> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let data_hex = hex::encode(data);
    let req_data = serde_json::json!({
        "key_name": PGP_KEY_NAME,
        "data": data_hex,
    });

    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/keys/sign"),
            req_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault pgp sign failed: {e}"))?;

    let signature_hex = resp
        .and_then(|r| r.data)
        .and_then(|d| d.get("signature").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .ok_or_else(|| anyhow!("no signature in vault response"))?;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(signature_hex)
}

/// Verify a hex-encoded PGP `signature` over `data` using the vault PGP key.
/// Mirrors [`pgp_sign`] but calls the `keys/verify` endpoint and returns whether
/// the signature is valid.
pub async fn pgp_verify(
    root_dir: &Path,
    unseal_key: &[u8],
    data: &[u8],
    signature_hex: &str,
) -> Result<bool> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let req_data = serde_json::json!({
        "key_name": PGP_KEY_NAME,
        "data": hex::encode(data),
        "signature": signature_hex,
    });

    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/keys/verify"),
            req_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault pgp verify failed: {e}"))?;

    // The PGP verify path returns `{valid: bool}`; the generic key path returns
    // `{result: bool}`. Accept either.
    let valid = resp
        .and_then(|r| r.data)
        .and_then(|d| {
            d.get("valid")
                .or_else(|| d.get("result"))
                .and_then(|v| v.as_bool())
        })
        .ok_or_else(|| anyhow!("no verification result in vault verify response"))?;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(valid)
}

/// Decode an ASCII-armored PGP signature block back into the hex-encoded
/// signature bytes (the inverse of [`signature_to_armored`]). Used to verify a
/// signature that was embedded in an annotated tag.
pub fn armored_to_signature_hex(armored: &str) -> Result<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let b64: String = armored
        .lines()
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("-----BEGIN PGP SIGNATURE-----")
                && !line.starts_with("-----END PGP SIGNATURE-----")
        })
        .collect();
    let sig_bytes = STANDARD
        .decode(b64.as_bytes())
        .context("failed to base64-decode armored signature")?;
    Ok(hex::encode(sig_bytes))
}

/// Generate an SSH key pair in the vault for Git transport authentication.
///
/// Configures the SSH CA, creates a role, and issues a certificate with a
/// generated user keypair. The private key is stored at
/// `~/.libra/ssh-keys/<repo-id>/id_ed25519` and the public key is returned.
#[allow(dead_code)]
pub async fn generate_ssh_key(
    root_dir: &Path,
    unseal_key: &[u8],
    user_name: &str,
) -> Result<String> {
    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    // Step 1: Configure SSH CA (generates CA keypair if not already configured)
    let ca_data = serde_json::json!({
        "key_type": "ed25519",
    });
    vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/config/ca/ssh"),
            ca_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH CA configuration failed: {e}"))?;

    // Step 2: Create SSH role for user certificates
    // NOTE:
    // OpenSSH in many environments does not accept PKCS8-encoded Ed25519 private keys.
    // Vault's SSH issue API returns PKCS8 for generated keys, which can lead to
    // `Load key ...: invalid format` when invoking `ssh -i`.
    // Use RSA here so the returned private key is consumable by OpenSSH directly.
    let role_data = serde_json::json!({
        "key_type": "rsa",
        "key_bits": 3072,
        "cert_type_ssh": "user",
        "default_user": "git",
        "allowed_users": "git",
        "ttl": "3650d",
        "max_ttl": "3650d",
    });
    vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/roles/ssh/{SSH_ROLE_NAME}"),
            role_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH role creation failed: {e}"))?;

    // Step 3: Issue SSH certificate with generated keypair
    let issue_data = serde_json::json!({
        "key_type": "rsa",
        "key_bits": 3072,
        "valid_principals": ["git"],
        "ttl": "3650d",
        "key_id": format!("libra-{user_name}"),
    });
    let resp = vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/issue/ssh/{SSH_ROLE_NAME}"),
            issue_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH key issuance failed: {e}"))?;

    let data = resp
        .and_then(|r| r.data)
        .ok_or_else(|| anyhow!("no data in vault SSH issue response"))?;

    let private_key = data
        .get("private_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no private_key in vault SSH response"))?;

    let public_key = data
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no public_key in vault SSH response"))?;

    // Store private key to filesystem for backward compat (will be removed in future)
    store_ssh_private_key(private_key).await?;

    // Store public key in config for easy retrieval.
    upsert_config_value("vault.ssh_pubkey", public_key).await;

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    // Return both public and private keys so callers can store them per-remote
    Ok(public_key.to_string())
}

/// Generate an SSH key pair and return (public_key, private_key) without
/// storing them. The caller is responsible for per-remote storage.
#[allow(dead_code)]
pub async fn generate_ssh_key_pair(
    root_dir: &Path,
    unseal_key: &[u8],
    user_name: &str,
) -> Result<(String, String)> {
    let vault = create_vault(root_dir).await?;
    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;
    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    // Configure SSH CA
    let ca_data = serde_json::json!({ "key_type": "ed25519" });
    vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/config/ca/ssh"),
            ca_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH CA configuration failed: {e}"))?;

    // Create SSH role
    let role_data = serde_json::json!({
        "key_type": "rsa",
        "key_bits": 3072,
        "cert_type_ssh": "user",
        "default_user": "git",
        "allowed_users": "git",
        "ttl": "3650d",
        "max_ttl": "3650d",
    });
    vault
        .write(
            Some(root_token.clone()),
            format!("{PKI_MOUNT_PATH}/roles/ssh/{SSH_ROLE_NAME}"),
            role_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH role creation failed: {e}"))?;

    // Issue SSH certificate
    let issue_data = serde_json::json!({
        "key_type": "rsa",
        "key_bits": 3072,
        "valid_principals": ["git"],
        "ttl": "3650d",
        "key_id": format!("libra-{user_name}"),
    });
    let resp = vault
        .write(
            Some(root_token),
            format!("{PKI_MOUNT_PATH}/issue/ssh/{SSH_ROLE_NAME}"),
            issue_data.as_object().cloned(),
        )
        .await
        .map_err(|e| anyhow!("vault SSH key issuance failed: {e}"))?;

    let data = resp
        .and_then(|r| r.data)
        .ok_or_else(|| anyhow!("no data in vault SSH issue response"))?;
    let private_key = data
        .get("private_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no private_key in vault SSH response"))?
        .to_string();
    let public_key = data
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no public_key in vault SSH response"))?
        .to_string();

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok((public_key, private_key))
}

/// Retrieve the SSH public key from config.
#[allow(dead_code)]
pub async fn get_ssh_public_key() -> Option<String> {
    use crate::internal::config::ConfigKv;
    ConfigKv::get("vault.ssh_pubkey")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
}

/// Retrieve the GPG (PGP) public key from the vault.
#[allow(dead_code)]
pub async fn get_gpg_public_key(root_dir: &Path, unseal_key: &[u8]) -> Result<String> {
    use crate::internal::config::ConfigKv;

    // Prefer cached value from config, populated during key generation.
    if let Some(entry) = ConfigKv::get("vault.gpg.pubkey").await.ok().flatten() {
        return Ok(entry.value);
    }
    if let Some(entry) = ConfigKv::get("vault.gpg_pubkey").await.ok().flatten() {
        return Ok(entry.value);
    }

    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let read_result = async {
        let pgp_key_path = format!("{PKI_MOUNT_PATH}/keys/{PGP_KEY_NAME}");
        let resp = vault
            .read(Some(root_token), &pgp_key_path)
            .await
            .map_err(|e| anyhow!("vault read PGP key failed: {e}"))?;

        resp.and_then(|r| r.data)
            .and_then(|d| d.get("public_key").cloned())
            .and_then(|v| v.as_str().map(String::from))
            .ok_or_else(|| anyhow!("no PGP public key found in vault"))
    }
    .await;

    let seal_result = vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"));

    match (read_result, seal_result) {
        (Ok(public_key), Ok(())) => Ok(public_key),
        (Ok(_), Err(seal_err)) => Err(seal_err),
        (Err(read_err), Ok(())) => Err(read_err),
        (Err(read_err), Err(seal_err)) => {
            Err(read_err.context(format!("additionally failed to reseal vault: {seal_err}")))
        }
    }
}

/// Get the path to the SSH private key file for the current repo.
pub async fn ssh_key_path() -> Result<std::path::PathBuf> {
    use crate::internal::config::ConfigKv;
    let home = vault_home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let repo_id = ConfigKv::get("libra.repoid")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("libra.repoid not set — was the repo initialized?"))?;
    Ok(home
        .join(".libra")
        .join("ssh-keys")
        .join(repo_id)
        .join("id_ed25519"))
}

/// Check if an SSH key has been generated for this repo.
#[allow(dead_code)]
pub async fn ssh_key_exists() -> bool {
    ssh_key_path().await.map(|p| p.exists()).unwrap_or(false)
}

/// Store the SSH private key to `~/.libra/ssh-keys/<repo-id>/id_ed25519`.
async fn store_ssh_private_key(private_key: &str) -> Result<()> {
    let path = ssh_key_path().await?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create ~/.libra/ssh-keys/")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).with_context(
                || format!("failed to set permissions to 700 on '{}'", parent.display()),
            )?;
        }
    }
    tokio::fs::write(&path, private_key)
        .await
        .context("failed to write SSH private key")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).with_context(
            || {
                format!(
                    "failed to set permissions to 600 on SSH private key '{}'",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

/// Convert a hex-encoded PGP detached signature into an armored PGP signature
/// string suitable for embedding in a Git/Libra commit object.
/// Build the ASCII-armored PGP signature block from a hex-encoded signature.
/// Shared by commit `gpgsig` headers and annotated-tag signatures (which append
/// this block verbatim to the tag message rather than indenting it as a header).
pub fn signature_to_armored(signature_hex: &str) -> Result<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let sig_bytes = hex::decode(signature_hex).context("failed to decode signature hex")?;

    let b64 = STANDARD.encode(&sig_bytes);
    let mut armored = String::from("-----BEGIN PGP SIGNATURE-----\n\n");
    for chunk in b64.as_bytes().chunks(76) {
        let line = std::str::from_utf8(chunk).context("base64 signature chunk is not UTF-8")?;
        armored.push_str(line);
        armored.push('\n');
    }
    armored.push_str("-----END PGP SIGNATURE-----");
    Ok(armored)
}

pub fn signature_to_gpgsig(signature_hex: &str) -> Result<String> {
    let armored = signature_to_armored(signature_hex)?;

    let mut gpgsig = String::from("gpgsig ");
    for (i, line) in armored.lines().enumerate() {
        if i > 0 {
            gpgsig.push_str("\n ");
        }
        gpgsig.push_str(line);
    }

    Ok(gpgsig)
}

/// Check whether the vault has been initialized in this repository.
#[allow(dead_code)]
pub fn vault_exists(root_dir: &Path) -> bool {
    if let Ok(path) = std::env::var("VAULT_SQLITE_FILENAME") {
        let configured = Path::new(&path);
        return if configured.is_absolute() {
            configured.exists()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(configured).exists())
                .unwrap_or(false)
        };
    }
    root_dir.join(VAULT_DB_NAME).exists()
}

/// Load the unseal key for a specific configuration scope.
/// - Local scope: reads from `~/.libra/vault-keys/<repo-id>`
/// - Global scope: reads from `~/.libra/vault-unseal-key`
pub async fn load_unseal_key_for_scope(scope: &str) -> Option<Vec<u8>> {
    match scope {
        "global" => load_global_unseal_key().await,
        _ => load_unseal_key().await, // "local" or default
    }
}

/// Load the local unseal key for the repository backed by `db_path`.
///
/// This is used when callers need to resolve local secrets for an explicit
/// repository target instead of the current working directory repository.
pub async fn load_unseal_key_for_db_path(db_path: &Path) -> Option<Vec<u8>> {
    if let Ok(repo_id) = repo_id_for_db_path(db_path).await
        && let Some(hex_key) = load_unseal_key_from_home_for_repo_id(&repo_id).await
    {
        return hex::decode(hex_key).ok();
    }

    use crate::internal::{config::ConfigKv, db::get_db_conn_instance_for_path};
    let conn = get_db_conn_instance_for_path(db_path).await.ok()?;
    let entry = ConfigKv::get_with_conn(&conn, "vault.unsealkey")
        .await
        .ok()??;
    hex::decode(entry.value).ok()
}

/// Load global unseal key from `~/.libra/vault-unseal-key`.
async fn load_global_unseal_key() -> Option<Vec<u8>> {
    let home = vault_home_dir()?;
    let path = home.join(".libra").join("vault-unseal-key");
    let hex_key = tokio::fs::read_to_string(&path).await.ok()?;
    hex::decode(hex_key.trim()).ok()
}

/// Store global unseal key to `~/.libra/vault-unseal-key` with 0o600 permissions.
async fn store_global_unseal_key(unseal_key: &[u8]) -> Result<()> {
    let home = vault_home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let dir = home.join(".libra");
    tokio::fs::create_dir_all(&dir)
        .await
        .context("failed to create ~/.libra/")?;
    let path = dir.join("vault-unseal-key");
    tokio::fs::write(&path, hex::encode(unseal_key))
        .await
        .context("failed to write global unseal key")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("failed to set permissions on global unseal key")?;
    }
    Ok(())
}

/// Lazy-initialize vault for a given scope and return the unseal key.
/// For local scope, initializes the repo vault (.libra/vault.db).
/// For global scope, creates a standalone AES key at ~/.libra/vault-unseal-key.
pub async fn lazy_init_vault_for_scope(scope: &str) -> Result<Vec<u8>> {
    match scope {
        "global" => {
            // ACTUALLY lazy: reuse the persisted key when one exists —
            // regenerating on every call would rotate the key and make every
            // previously-encrypted global value (auth tokens, encrypted
            // config) permanently undecryptable.
            if let Some(existing) = load_global_unseal_key().await
                && existing.len() == 32
            {
                return Ok(existing);
            }
            use ring::rand::{SecureRandom, SystemRandom};
            let rng = SystemRandom::new();
            let mut key = vec![0u8; 32];
            rng.fill(&mut key)
                .map_err(|_| anyhow!("failed to generate random key"))?;
            store_global_unseal_key(&key).await?;
            Ok(key)
        }
        _ => {
            // Local scope: use the full vault init
            let storage =
                try_get_storage_path(None).map_err(|_| anyhow!("not a libra repository"))?;
            let (unseal_key, enc_token) = init_vault(&storage).await?;
            store_credentials(&unseal_key, &enc_token).await?;
            Ok(unseal_key)
        }
    }
}

/// Read the stored unseal key from the user's home directory.
///
/// The key is stored at `~/.libra/vault-keys/<repo-id>` to keep it
/// separate from the repository config (where the encrypted root token
/// lives). Falls back to the legacy repo-config location
/// (`vault.unsealkey`) for backwards compatibility.
pub async fn load_unseal_key() -> Option<Vec<u8>> {
    // Try the new location first: ~/.libra/vault-keys/<repo-id>
    if let Some(hex_key) = load_unseal_key_from_home().await {
        return hex::decode(hex_key).ok();
    }
    // Fallback: legacy repo-config location
    use crate::internal::config::ConfigKv;
    let entry = ConfigKv::get("vault.unsealkey").await.ok()??;
    hex::decode(entry.value).ok()
}

/// Store the unseal key in `~/.libra/vault-keys/<repo-id>` and the
/// encrypted root token in the repo config.
#[allow(dead_code)]
pub async fn store_credentials(unseal_key: &[u8], encrypted_token: &[u8]) -> Result<()> {
    use crate::internal::config::ConfigKv;
    // Store unseal key outside the repo; do not silently downgrade to repo config.
    store_unseal_key_to_home(unseal_key)
        .await
        .context("failed to store vault unseal key in ~/.libra/")?;

    // Clean up any legacy insecure storage if present.
    let _ = ConfigKv::unset_all("vault.unsealkey").await;

    // Encrypted token always goes in repo config
    ConfigKv::set("vault.roottoken_enc", &hex::encode(encrypted_token), false)
        .await
        .context("failed to store encrypted root token")?;
    Ok(())
}

/// Remove previously stored vault credentials.
///
/// Used to rollback when vault initialization partially succeeds (e.g. credentials
/// are stored but PGP key generation fails).
#[allow(dead_code)]
pub async fn remove_credentials() {
    use crate::internal::config::ConfigKv;
    // Remove from home dir
    let _ = remove_unseal_key_from_home().await;
    // Remove legacy repo-config entries
    let _ = ConfigKv::unset_all("vault.unsealkey").await;
    let _ = ConfigKv::unset_all("vault.roottoken_enc").await;
}

// ── Internal helpers ──

/// Compatibility wrapper around libvault's SQLite backend.
///
/// libvault 0.2.2 currently emits `ESCAPE '\\\\'` in `list()`, which SQLite
/// rejects with `ESCAPE expression must be a single character`. We delegate all
/// operations to the upstream backend except `list()`, which is implemented
/// with an SQL expression accepted by SQLite.
struct CompatSqliteBackend {
    inner: SqliteBackend,
    pool: SqlitePool,
    table: String,
}

impl CompatSqliteBackend {
    async fn new(
        conf: &HashMap<String, Value>,
        db_path: &Path,
        table: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let inner = SqliteBackend::new(conf)
            .await
            .map_err(|e| anyhow!("vault sqlite backend creation failed: {e}"))?;

        // Keep this side-channel connection aligned with libvault's effective
        // filename resolution, especially when `VAULT_SQLITE_FILENAME` is set.
        let create_if_missing = conf
            .get("create_if_missing")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let configured_path = std::env::var("VAULT_SQLITE_FILENAME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                conf.get("filename")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| db_path.to_path_buf());

        let resolved_path = if configured_path.is_absolute() {
            configured_path
        } else {
            std::env::current_dir()
                .context("failed to resolve current directory for vault sqlite path")?
                .join(configured_path)
        };
        let resolved_path = match resolved_path.canonicalize() {
            Ok(canonical) => canonical,
            Err(_) if create_if_missing => resolved_path,
            Err(err) => {
                return Err(anyhow!(
                    "failed to resolve vault sqlite path '{}': {err}",
                    resolved_path.display()
                ));
            }
        };

        let options = SqliteConnectOptions::new()
            .filename(resolved_path)
            .busy_timeout(timeout)
            .create_if_missing(true)
            .read_only(false);
        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| anyhow!("vault sqlite pool creation failed: {e}"))?;

        Ok(Self {
            inner,
            pool,
            table: table.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl Backend for CompatSqliteBackend {
    async fn list(&self, prefix: &str) -> Result<Vec<String>, RvError> {
        if prefix.starts_with('/') {
            return Err(RvError::ErrSqliteBackendNotSupportAbsolute);
        }

        // NOTE: `ESCAPE '\'` uses a single-character escape literal accepted by SQLite.
        let sql = format!(
            "SELECT vault_key FROM `{}` WHERE vault_key LIKE ? ESCAPE '\\'",
            self.table
        );
        let escaped_prefix = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        // sqlx 0.9 requires an explicit assertion for interpolated SQL: the
        // table name comes from our own vault configuration (never user input)
        // and the LIKE prefix is bound as a parameter.
        let keys: Vec<Vec<u8>> = query_scalar(AssertSqlSafe(sql))
            .bind(format!("{escaped_prefix}%").as_bytes())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| RvError::from(anyhow!(e)))?;

        let mut result = HashSet::new();
        for key_bytes in keys {
            let key = String::from_utf8(key_bytes)?;
            let key = key.strip_prefix(prefix).unwrap_or(&key);
            match key.find('/') {
                Some(idx) => {
                    result.insert(key[..idx + 1].to_string());
                }
                None => {
                    result.insert(key.to_string());
                }
            }
        }

        Ok(result.into_iter().collect())
    }

    async fn get(&self, key: &str) -> Result<Option<BackendEntry>, RvError> {
        self.inner.get(key).await
    }

    async fn put(&self, entry: &BackendEntry) -> Result<(), RvError> {
        self.inner.put(entry).await
    }

    async fn delete(&self, key: &str) -> Result<(), RvError> {
        self.inner.delete(key).await
    }
}

async fn create_vault(root_dir: &Path) -> Result<RustyVault> {
    let db_path = root_dir.join(VAULT_DB_NAME);
    let table_name = "vault".to_string();
    let timeout = Duration::from_secs(5);
    let mut conf = HashMap::new();
    conf.insert(
        "filename".to_string(),
        Value::String(db_path.to_string_lossy().to_string()),
    );
    conf.insert("create_if_missing".to_string(), Value::Bool(true));
    conf.insert("timeout".to_string(), Value::String("5s".to_string()));
    conf.insert("table".to_string(), Value::String(table_name.clone()));

    let backend: Arc<CompatSqliteBackend> =
        Arc::new(CompatSqliteBackend::new(&conf, &db_path, &table_name, timeout).await?);

    let vault =
        RustyVault::new(backend, None).map_err(|e| anyhow!("vault creation failed: {e}"))?;

    Ok(vault)
}

async fn upsert_config_value(dotted_key: &str, value: &str) {
    use crate::internal::config::ConfigKv;
    // set does upsert for single-value keys; ignore errors for vault internals
    let _ = ConfigKv::set(dotted_key, value, false).await;
}

/// Recover the root token by decrypting the stored encrypted token with the unseal key.
async fn recover_root_token(unseal_key: &[u8]) -> Result<String> {
    use crate::internal::config::ConfigKv;
    let enc_hex = ConfigKv::get("vault.roottoken_enc")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("vault encrypted root token not found in config"))?;
    let enc_bytes = hex::decode(&enc_hex).context("failed to decode encrypted root token hex")?;
    decrypt_token(unseal_key, &enc_bytes)
}

// ── Home-directory unseal key storage ──

/// Resolve the path `~/.libra/vault-keys/<repo-id>` for the current repo.
async fn unseal_key_path() -> Result<std::path::PathBuf> {
    let repo_id = current_repo_id().await?;
    unseal_key_path_for_repo_id(&repo_id)
}

async fn current_repo_id() -> Result<String> {
    use crate::internal::config::ConfigKv;
    ConfigKv::get("libra.repoid")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("libra.repoid not set — was the repo initialized?"))
}

async fn repo_id_for_db_path(db_path: &Path) -> Result<String> {
    use crate::internal::{config::ConfigKv, db::get_db_conn_instance_for_path};
    let conn = get_db_conn_instance_for_path(db_path)
        .await
        .context("failed to open repository config database")?;
    ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await?
        .map(|e| e.value)
        .ok_or_else(|| anyhow!("libra.repoid not set — was the repo initialized?"))
}

fn unseal_key_path_for_repo_id(repo_id: &str) -> Result<std::path::PathBuf> {
    let home = vault_home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    Ok(home.join(".libra").join("vault-keys").join(repo_id))
}

/// Read the hex-encoded unseal key from `~/.libra/vault-keys/<repo-id>`.
async fn load_unseal_key_from_home() -> Option<String> {
    let path = unseal_key_path().await.ok()?;
    load_unseal_key_from_home_for_repo_id_path(&path).await
}

async fn load_unseal_key_from_home_for_repo_id(repo_id: &str) -> Option<String> {
    let path = unseal_key_path_for_repo_id(repo_id).ok()?;
    load_unseal_key_from_home_for_repo_id_path(&path).await
}

async fn load_unseal_key_from_home_for_repo_id_path(path: &Path) -> Option<String> {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|s| s.trim().to_string())
}

/// Write the unseal key (hex) to `~/.libra/vault-keys/<repo-id>` with
/// restrictive permissions (owner-only on Unix).
async fn store_unseal_key_to_home(unseal_key: &[u8]) -> Result<()> {
    let path = unseal_key_path().await?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create ~/.libra/vault-keys/")?;
        // Restrict directory permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(parent, perms).with_context(|| {
                format!("failed to set permissions to 700 on '{}'", parent.display())
            })?;
        }
    }
    tokio::fs::write(&path, hex::encode(unseal_key))
        .await
        .context("failed to write unseal key")?;
    // Restrict file permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms).with_context(|| {
            format!(
                "failed to set permissions to 600 on unseal key '{}'",
                path.display()
            )
        })?;
    }
    Ok(())
}

/// Remove the unseal key file from `~/.libra/vault-keys/<repo-id>`.
async fn remove_unseal_key_from_home() -> Result<()> {
    let path = unseal_key_path().await?;
    if path.exists() {
        tokio::fs::remove_file(&path)
            .await
            .context("failed to remove unseal key file")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::load_unseal_key_for_db_path;
    use crate::internal::{
        config::ConfigKv,
        db::{create_database, reset_db_conn_instance_for_path},
    };

    #[tokio::test]
    async fn load_unseal_key_for_db_path_falls_back_to_legacy_db_key_without_repo_id() {
        let temp = tempdir().expect("failed to create temp dir");
        let db_path = temp.path().join("libra.db");
        let expected = vec![0x12, 0x34, 0x56, 0x78];

        let conn = create_database(db_path.to_string_lossy().as_ref())
            .await
            .expect("failed to create test database");
        ConfigKv::set_with_conn(&conn, "vault.unsealkey", &hex::encode(&expected), false)
            .await
            .expect("failed to seed legacy vault.unsealkey");
        drop(conn);

        let actual = load_unseal_key_for_db_path(&db_path).await;
        assert_eq!(actual, Some(expected));

        reset_db_conn_instance_for_path(&db_path).await;
    }
}
