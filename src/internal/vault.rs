//! Vault integration module wrapping libvault for PGP key management.
//!
//! Provides helpers to initialize a libvault instance backed by the repository's
//! `.libra/vault.db` SQLite database, generate PGP keys, sign data, and verify
//! signatures. The vault state (sealed/unsealed) is managed transparently.
//!
//! # Secret handling
//!
//! The per-repository unseal key is stored hex-encoded in the user's home
//! directory (`~/.libra/vault-keys/<repo-id>`) — outside the repository — so
//! that anyone with read access to the repo config alone cannot recover the
//! root token. The global unseal key lives beside the global configuration
//! database it protects, at `<config dir>/libra/vault-unseal-key`
//! (plan-20260919 ADR-GCX-04); a pre-XDG `~/.libra/vault-unseal-key` is copied
//! there once and then kept, unmodified, as a downgrade backup. The root token is encrypted (AES-256-GCM) with a key derived from
//! the unseal key before being persisted in the repo config
//! (`vault.roottoken_enc`). It is never stored in plaintext.
//!
//! # Threat model
//!
//! This design protects against casual repo-level read access (e.g. a
//! colleague cloning the repo, or a backup leak). It does NOT protect
//! against full compromise of the user's machine — an attacker with access to
//! the repo plus the user's per-user state (`~/.libra/` for repository keys,
//! `<config dir>/libra/` for the global key) can recover the root token. For
//! stronger guarantees, integrate an OS keychain or hardware token.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use libvault::{RustyVault, core::SealConfig, storage::sql::sqlite::SqliteBackend};
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
/// Generate a PGP key pair in the vault (pure helper — no config writes,
/// plan-20260921 VG-14). Returns the armored public key.
pub async fn vault_generate_pgp_key(
    root_dir: &Path,
    unseal_key: &[u8],
    key_name: &str,
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
        "key_name": key_name,
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

    vault
        .seal()
        .await
        .map_err(|e| anyhow!("vault seal failed: {e}"))?;

    Ok(public_key)
}

/// Test-only override for the sequence of versioned key names.
///
/// Retry exhaustion cannot be induced from outside: the probe only retries
/// when the vault reports an existing name, and the real name is derived from
/// the current nanosecond, so no caller can pre-create a colliding key.
/// Tests therefore inject names at this seam. It is compiled out of non-test
/// builds — ADR-VG-07 §2 forbids *env* hooks, not `#[cfg(test)]` seams.
#[cfg(test)]
static KEY_NAME_OVERRIDE: std::sync::Mutex<Option<std::collections::VecDeque<String>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_key_name_override(names: Vec<String>) {
    if let Ok(mut guard) = KEY_NAME_OVERRIDE.lock() {
        *guard = Some(names.into_iter().collect());
    }
}

/// How many injected names are still queued — used as proof that the retry
/// loop really consumed all eight attempts.
#[cfg(test)]
pub(crate) fn key_name_override_remaining() -> usize {
    KEY_NAME_OVERRIDE
        .lock()
        .map(|guard| guard.as_ref().map_or(0, |queue| queue.len()))
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn clear_key_name_override() {
    if let Ok(mut guard) = KEY_NAME_OVERRIDE.lock() {
        *guard = None;
    }
}

fn next_versioned_key_name() -> String {
    #[cfg(test)]
    if let Ok(mut guard) = KEY_NAME_OVERRIDE.lock()
        && let Some(queue) = guard.as_mut()
        && let Some(name) = queue.pop_front()
    {
        return name;
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("libra-signing-{nanos}")
}

/// Read a config value into `Option<String>` (empty ⇒ `None`).
async fn read_cfg_value(key: &str) -> Option<String> {
    use crate::internal::config::ConfigKv;
    ConfigKv::get(key)
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .filter(|v| !v.is_empty())
}

/// Restore a config key to its before-image: set when it existed, otherwise
/// remove it. Used by the ADR-VG-06 §6 staged-step compensation.
async fn restore_cfg_value(key: &str, before: Option<String>) -> Result<()> {
    use crate::internal::config::ConfigKv;
    match before {
        Some(value) => ConfigKv::set(key, &value, false)
            .await
            .with_context(|| format!("failed to restore '{key}'")),
        None => ConfigKv::unset(key)
            .await
            .map(|_| ())
            .with_context(|| format!("failed to clear '{key}'")),
    }
}

/// Generate a signing PGP key with a versioned key name and persist the
/// generated-key metadata (`vault.gpg.generated_key_name`). Collisions on the
/// versioned name probe-retry up to 8 times; config write errors propagate.
/// Returns `(public_key, key_name)`.
pub async fn generate_pgp_key(
    root_dir: &Path,
    unseal_key: &[u8],
    user_name: &str,
    user_email: &str,
) -> Result<(String, String)> {
    use crate::internal::config::ConfigKv;

    // VG-14 §(e) resume: a previous run that already established the generated
    // key ((c) generated_pubkey, (d1) generated_key_name, (d2) pubkey) but
    // failed before committing (e) left the new generated key active while
    // `source` still names the old provenance. Completing that sequence must
    // not mint a second key — just commit (e) and report the existing pair.
    {
        let read = async |key: &str| {
            ConfigKv::get(key)
                .await
                .ok()
                .flatten()
                .map(|e| e.value)
                .unwrap_or_default()
        };
        let key_name = read("vault.gpg.generated_key_name").await;
        let generated_pubkey = read("vault.gpg.generated_pubkey").await;
        let source = read("vault.gpg.source").await;
        let active_pubkey = read("vault.gpg.pubkey").await;
        // VG-14 G5: the recorded pair is only resumable when the minted key
        // already holds the active slot (a failure after (d2) but before (e)) —
        // there `source` still names the previous provenance, and completing
        // (e) must not mint a second key. Other residues (e.g. after (c)/(d1))
        // are not resumable: the next generation overwrites the stale pair.
        if !key_name.is_empty()
            && !generated_pubkey.is_empty()
            && source != "generated"
            && active_pubkey == generated_pubkey
        {
            ConfigKv::set("vault.gpg.source", "generated", false)
                .await
                .context("failed to persist generated GPG key source")?;
            return Ok((generated_pubkey, key_name));
        }
    }

    // (a) Preserve the currently-active key (imported or generated) in history
    // before overwrite, so historical signatures stay verifiable through the
    // fixed allowlist (ADR-VG-04 / ADR-VG-06 §6). VG-14 relaxes the VG-03
    // empty-window guard by migrating rather than failing closed.
    snapshot_active_key_to_history().await?;
    generate_failpoint(1)?;

    let mut last_err: Option<anyhow::Error> = None;
    for _ in 0..8 {
        let key_name = next_versioned_key_name();
        match vault_generate_pgp_key(root_dir, unseal_key, &key_name, user_name, user_email).await {
            Ok(public_key) => {
                generate_failpoint(2)?;
                // ADR-VG-06 §6 staged writes: (c) generated_pubkey, (d1)
                // generated_key_name, (d2) vault.gpg.pubkey, (e) source. Each
                // step captures a before-image and compensates on the next
                // step's failure; the (e)-failure residue is intentionally
                // kept (history already holds the old key) and converges on
                // re-run via the resume branch above.
                let before_generated_pubkey = read_cfg_value("vault.gpg.generated_pubkey").await;
                let before_generated_key_name =
                    read_cfg_value("vault.gpg.generated_key_name").await;
                let before_pubkey = read_cfg_value("vault.gpg.pubkey").await;

                // (c) generated_pubkey
                if let Err(e) =
                    ConfigKv::set("vault.gpg.generated_pubkey", &public_key, false).await
                {
                    return Err(e).context("failed to persist generated GPG public key snapshot");
                }
                generate_failpoint(3)?;

                // (d1) generated_key_name
                if let Err(e) =
                    ConfigKv::set("vault.gpg.generated_key_name", &key_name, false).await
                {
                    let _ =
                        restore_cfg_value("vault.gpg.generated_pubkey", before_generated_pubkey)
                            .await;
                    return Err(e).context("failed to persist generated GPG key name");
                }
                generate_failpoint(4)?;

                // (d2) vault.gpg.pubkey
                if let Err(e) = ConfigKv::set("vault.gpg.pubkey", &public_key, false).await {
                    let _ =
                        restore_cfg_value("vault.gpg.generated_pubkey", before_generated_pubkey)
                            .await;
                    let _ = restore_cfg_value(
                        "vault.gpg.generated_key_name",
                        before_generated_key_name,
                    )
                    .await;
                    // ADR-VG-06 §6: the active pubkey may have partially applied.
                    let _ = restore_cfg_value("vault.gpg.pubkey", before_pubkey).await;
                    return Err(e).context("failed to persist generated GPG public key");
                }
                generate_failpoint(5)?;

                // (e) source=generated
                if let Err(e) = ConfigKv::set("vault.gpg.source", "generated", false).await {
                    return Err(e).context("failed to persist generated GPG key source");
                }
                generate_failpoint(6)?;
                return Ok((public_key, key_name));
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("exist") || msg.contains("Exist") {
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("failed to generate GPG key after 8 collision retries")))
}

/// Sign data using the vault's PGP key.
///
/// `data` is the raw bytes to sign. Returns the hex-encoded detached signature.
pub async fn pgp_sign(root_dir: &Path, unseal_key: &[u8], data: &[u8]) -> Result<String> {
    use crate::internal::config::ConfigKv;

    let source = ConfigKv::get("vault.gpg.source")
        .await
        .ok()
        .flatten()
        .map(|e| e.value);
    if source.as_deref() == Some("imported") {
        return sign_with_imported_key(unseal_key, data).await;
    }

    // plan-20260921 VG-08 G6: a signature the repository cannot verify is
    // worse than no signature. The generated path signs through the vault by
    // key name, so it would happily emit a signature whose public half is no
    // longer published (`vault.gpg.pubkey` and `vault.gpg.generated_pubkey`
    // both gone) — verification would then fail later, far from the cause.
    // Fail closed here, with the recovery hint.
    let published = |value: Option<crate::internal::config::ConfigKvEntry>| {
        value.is_some_and(|entry| !entry.value.is_empty())
    };
    let has_public_half = published(ConfigKv::get("vault.gpg.pubkey").await.ok().flatten())
        || published(
            ConfigKv::get("vault.gpg.generated_pubkey")
                .await
                .ok()
                .flatten(),
        );
    if !has_public_half {
        return Err(anyhow!(
            "no active GPG public key to verify with; signing would be unverifiable — \
             run `libra config generate-gpg-key` or import one with \
             `libra config import-gpg-key --file <armored-secret> --replace`"
        ));
    }

    let vault = create_vault(root_dir).await?;

    vault
        .unseal(&[unseal_key])
        .await
        .map_err(|e| anyhow!("vault unseal failed: {e}"))?;

    let root_token = recover_root_token(unseal_key).await?;
    vault.set_token(&root_token);

    let key_name = generated_key_name().await;
    let data_hex = hex::encode(data);
    let req_data = serde_json::json!({
        "key_name": key_name,
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

/// The versioned generated-key name (`vault.gpg.generated_key_name`), falling
/// back to the legacy constant `libra-signing` for repositories created before
/// plan-20260921 VG-14 (reader-side legacy fallback).
async fn generated_key_name() -> String {
    use crate::internal::config::ConfigKv;
    ConfigKv::get("vault.gpg.generated_key_name")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| PGP_KEY_NAME.to_string())
}

/// Decrypt the imported secret key stored in `vault.gpg.seckey_enc` and sign
/// the data in-process (ADR-VG-03 §1).
async fn sign_with_imported_key(unseal_key: &[u8], data: &[u8]) -> Result<String> {
    use crate::internal::config::ConfigKv;

    let seckey = ConfigKv::get("vault.gpg.seckey_enc")
        .await
        .context("failed to read imported secret key")?
        .ok_or_else(|| anyhow!("no imported GPG secret key (vault.gpg.seckey_enc missing)"))?;
    let signing_key_id = ConfigKv::get("vault.gpg.signing_key_id")
        .await
        .context("failed to read signing key id")?
        .ok_or_else(|| anyhow!("no signing key id (vault.gpg.signing_key_id missing)"))?
        .value;

    let ciphertext = hex::decode(&seckey.value).context("failed to decode imported secret key")?;
    let armored_secret =
        decrypt_token(unseal_key, &ciphertext).context("failed to decrypt imported secret key")?;

    sign_with_armored_secret_key(&armored_secret, &signing_key_id, data)
}

/// Public keys eligible for verification, in ADR-VG-03 §2 order:
/// active `vault.gpg.pubkey` → `vault.gpg.generated_pubkey` → `history.*`
/// (fingerprint lexicographic). Deduplicated by value.
async fn verify_pubkey_allowlist() -> Result<Vec<String>> {
    use crate::internal::config::ConfigKv;

    let mut armors: Vec<String> = Vec::new();
    let mut push = |value: Option<String>| {
        if let Some(v) = value
            && !v.is_empty()
            && !armors.contains(&v)
        {
            armors.push(v);
        }
    };

    push(
        ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value),
    );
    push(
        ConfigKv::get("vault.gpg.generated_pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value),
    );

    let mut history = ConfigKv::get_by_prefix("vault.gpg.history.")
        .await
        .map_err(|e| anyhow!("failed to read GPG key history: {e}"))?;
    history.sort_by(|l, r| l.key.cmp(&r.key));
    for entry in history {
        push(Some(entry.value));
    }

    Ok(armors)
}

/// Verify a hex-encoded PGP `signature` over `data` against the fixed allowlist
/// (active → generated → history), independent of `vault.gpg.source`.
pub async fn pgp_verify(
    _root_dir: &Path,
    _unseal_key: &[u8],
    data: &[u8],
    signature_hex: &str,
) -> Result<bool> {
    let pubkeys = verify_pubkey_allowlist().await?;
    Ok(verify_signature_hex(signature_hex, data, &pubkeys))
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

/// Load the unseal key for a specific configuration scope.
/// - Local scope: reads from `~/.libra/vault-keys/<repo-id>`
/// - Global scope: reads from `<config dir>/libra/vault-unseal-key`
///
/// A global key that exists but cannot be trusted (unreadable, malformed, or
/// present in two locations with different contents) yields `None` together
/// with an actionable warning. It must never be papered over by generating a
/// replacement: see [`lazy_init_vault_for_scope`], which fails instead.
pub async fn load_unseal_key_for_scope(scope: &str) -> Option<Vec<u8>> {
    match scope {
        "global" => match load_global_unseal_key().await {
            Ok(key) => key,
            Err(error) => {
                warn_about_the_global_key_once(format!("{error:#}"));
                None
            }
        },
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

/// File name of the global unseal key in both the new and the legacy layout.
pub(crate) const GLOBAL_UNSEAL_KEY_FILE: &str = "vault-unseal-key";
/// AES-256-GCM key material length. A file of any other length is a corrupt
/// key, never an invitation to generate a new one.
const UNSEAL_KEY_LEN: usize = 32;

/// The directory holding the global unseal key: the same user configuration
/// directory the global config database lives in (ADR-GCX-04), so the key and
/// the values it protects share one domain.
pub(crate) fn global_unseal_key_path() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("LIBRA_TEST_HOME") {
        return Some(
            PathBuf::from(path)
                .join(".config")
                .join("libra")
                .join(GLOBAL_UNSEAL_KEY_FILE),
        );
    }
    crate::internal::config::global_config_dir().map(|dir| dir.join(GLOBAL_UNSEAL_KEY_FILE))
}

/// The key file Libra actually uses right now: the configuration-directory one
/// when it exists, otherwise the legacy file while it is still the active key.
///
/// Callers that harden permissions use this so a migrated legacy file — a
/// backup Libra no longer reads — is left exactly as the user left it.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn active_global_unseal_key_path() -> Option<PathBuf> {
    let new_path = global_unseal_key_path();
    if new_path.as_deref().is_some_and(Path::exists) {
        return new_path;
    }
    legacy_global_unseal_key_path()
        .filter(|path| path.exists())
        .or(new_path)
}

/// The pre-XDG location. Still read (and copied forward) so no existing key —
/// and therefore no existing ciphertext — is ever lost.
pub(crate) fn legacy_global_unseal_key_path() -> Option<PathBuf> {
    vault_home_dir().map(|home| home.join(".libra").join(GLOBAL_UNSEAL_KEY_FILE))
}

/// Read one hex-encoded key file.
///
/// `Ok(None)` means "absent", which is the ONLY state that permits generating
/// a key. Every other problem is an error: silently treating an unreadable or
/// malformed key as absent would rotate the key and make every value already
/// encrypted with it permanently undecryptable (GC-GCX-03).
async fn read_unseal_key_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(anyhow!(
                "cannot read the global vault key '{}': {error}; fix the file's permissions \
                 (it must be readable by you and mode 0600) — it must not be deleted or \
                 regenerated, or every encrypted global value becomes unreadable",
                path.display()
            ));
        }
    };
    let key = hex::decode(raw.trim()).map_err(|error| {
        anyhow!(
            "the global vault key '{}' is not valid hex ({error}); restore it from a backup — \
             replacing it makes every encrypted global value unreadable",
            path.display()
        )
    })?;
    if key.len() != UNSEAL_KEY_LEN {
        return Err(anyhow!(
            "the global vault key '{}' is {} bytes instead of {UNSEAL_KEY_LEN}; restore it from \
             a backup — replacing it makes every encrypted global value unreadable",
            path.display(),
            key.len()
        ));
    }
    Ok(Some(key))
}

/// Load the global unseal key, moving it out of the legacy Libra home once.
///
/// Boundary conditions (ADR-GCX-04):
/// - the configuration-directory file wins; a legacy-only key is copied there
///   and the legacy file is then kept, unmodified, as a downgrade backup;
/// - two files with DIFFERENT contents fail closed, because picking either one
///   would make the values encrypted with the other unreadable;
/// - a failed copy is not fatal: the identical legacy key stays in use and the
///   caller is warned;
/// - `Ok(None)` means no key exists anywhere yet.
async fn load_global_unseal_key() -> Result<Option<Vec<u8>>> {
    let new_path = global_unseal_key_path();
    let legacy_path = legacy_global_unseal_key_path();

    // The configuration-directory file is authoritative, so a problem reading
    // it is fatal.
    let new_key = match new_path.as_deref() {
        Some(path) => read_unseal_key_file(path).await?,
        None => None,
    };
    let legacy_key = match legacy_path.as_deref() {
        Some(path) => match read_unseal_key_file(path).await {
            Ok(key) => key,
            // Once the key has moved, the legacy file is only a backup. An
            // unreadable backup means the conflict check cannot run — it does
            // not mean the command should fail.
            Err(error) if new_key.is_some() => {
                tracing::debug!(
                    path = %path.display(),
                    error = %format!("{error:#}"),
                    "ignoring an unreadable legacy global vault key backup"
                );
                None
            }
            Err(error) => return Err(error),
        },
        None => None,
    };

    match (new_key, legacy_key) {
        (Some(new), Some(legacy)) if new != legacy => Err(anyhow!(
            "two different global vault keys exist: '{}' and '{}'. Libra refuses to guess which \
             one protects your encrypted global values — keep the one that decrypts them and \
             move the other aside, then re-run",
            new_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            legacy_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        )),
        (Some(new), _) => Ok(Some(new)),
        (None, Some(legacy)) => {
            if let Some(path) = new_path.as_deref()
                && let Err(error) = write_unseal_key_file(path, &legacy).await
            {
                // The key itself is unchanged, so the command can proceed on
                // the legacy file; only the relocation is postponed.
                warn_about_the_global_key_once(format!(
                    "could not move the global vault key to '{}': {error:#}; still using '{}'",
                    path.display(),
                    legacy_path
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ));
            }
            Ok(Some(legacy))
        }
        (None, None) => Ok(None),
    }
}

/// One command can resolve the global key many times — `config list --global`
/// decrypts every encrypted row — so a standing problem with it is reported
/// once, not once per value.
fn warn_about_the_global_key_once(message: String) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    crate::utils::error::emit_warning(message);
}

/// Write a key file atomically with owner-only permissions.
///
/// The temporary file carries the final mode BEFORE the rename, so the key is
/// never observable through a world-readable file, not even briefly.
async fn write_unseal_key_file(path: &Path, unseal_key: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("'{}' has no parent directory", path.display()))?;
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create '{}'", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best effort: an inherited directory keeps the mode the user chose.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }

    let staging = dir.join(format!(
        ".{}.{}.tmp",
        GLOBAL_UNSEAL_KEY_FILE,
        std::process::id()
    ));
    tokio::fs::write(&staging, hex::encode(unseal_key))
        .await
        .with_context(|| format!("failed to write '{}'", staging.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))
        {
            let _ = tokio::fs::remove_file(&staging).await;
            return Err(anyhow::Error::new(error).context(format!(
                "failed to restrict '{}' to the owner",
                staging.display()
            )));
        }
    }
    if let Err(error) = tokio::fs::rename(&staging, path).await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(
            anyhow::Error::new(error).context(format!("failed to publish '{}'", path.display()))
        );
    }
    Ok(())
}

/// Store the global unseal key in the user configuration directory (0600).
async fn store_global_unseal_key(unseal_key: &[u8]) -> Result<()> {
    let path = global_unseal_key_path()
        .ok_or_else(|| anyhow!("cannot determine the user configuration directory"))?;
    write_unseal_key_file(&path, unseal_key).await
}

/// Lazy-initialize vault for a given scope and return the unseal key.
/// For local scope, initializes the repo vault (.libra/vault.db).
/// For global scope, creates a standalone AES key at
/// `<config dir>/libra/vault-unseal-key`.
pub async fn lazy_init_vault_for_scope(scope: &str) -> Result<Vec<u8>> {
    match scope {
        "global" => {
            // ACTUALLY lazy: reuse the persisted key when one exists —
            // regenerating on every call would rotate the key and make every
            // previously-encrypted global value (auth tokens, encrypted
            // config) permanently undecryptable.
            // A key that exists but cannot be trusted is an ERROR, not a
            // reason to make a new one: `load_global_unseal_key` already
            // rejects an unreadable, malformed or conflicting key, and that
            // error must propagate rather than fall through to generation.
            if let Some(existing) = load_global_unseal_key().await? {
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

async fn create_vault(root_dir: &Path) -> Result<RustyVault> {
    let db_path = root_dir.join(VAULT_DB_NAME);
    let table_name = "vault".to_string();
    let mut conf = HashMap::new();
    conf.insert(
        "filename".to_string(),
        Value::String(db_path.to_string_lossy().to_string()),
    );
    conf.insert("create_if_missing".to_string(), Value::Bool(true));
    conf.insert("timeout".to_string(), Value::String("5s".to_string()));
    conf.insert("table".to_string(), Value::String(table_name.clone()));

    let backend = Arc::new(
        SqliteBackend::new(&conf)
            .await
            .map_err(|e| anyhow!("vault sqlite backend creation failed: {e}"))?,
    );

    let vault =
        RustyVault::new(backend, None).map_err(|e| anyhow!("vault creation failed: {e}"))?;

    Ok(vault)
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

// ─────────────────────────────────────────────────────────────────────────────
// plan-20260921: imported GPG key discovery, deprotection, sign and verify
// ─────────────────────────────────────────────────────────────────────────────

/// A signing candidate discovered from `gpg --with-colons --list-secret-keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpgCandidate {
    pub fingerprint: String,
    pub key_id: String,
    pub uid: String,
    pub algorithm_bits: String,
    pub capabilities: String,
}

/// Parse `gpg --with-colons --list-secret-keys` output into candidates.
///
/// Colon record kinds consumed: `sec` (primary secret key), `ssb` (secret
/// subkey), `fpr` (fingerprint), `uid` (user id). Only keys with a secret part
/// (`sec`) are enumerated; a subkey carries capability letters in field 12
/// (`e` encrypt, `s` sign, `c` certify, `a` authenticate). A candidate is
/// reported once per primary key.
///
/// The unit test feeds this the fixed sample produced by `tests/data/fake-gpg`.
pub fn parse_gpg_colon_list(input: &str) -> Vec<GpgCandidate> {
    // field indices (0-based) for the colon-delimited record:
    // 0 type, 1 validity, 2 key length, 3 public key algo, 4 key id,
    // 9 user id, 11 capabilities, 12 capabilities (computed trust/caps)
    let mut candidates: Vec<GpgCandidate> = Vec::new();

    for line in input.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 2 || fields[0].is_empty() {
            continue;
        }
        match fields[0] {
            "sec" => {
                let key_id = fields.get(4).copied().unwrap_or_default().to_string();
                candidates.push(GpgCandidate {
                    fingerprint: String::new(),
                    key_id,
                    uid: String::new(),
                    algorithm_bits: format_gpg_algo_bits(
                        fields.get(2).copied().unwrap_or_default(),
                        fields.get(3).copied().unwrap_or_default(),
                    ),
                    capabilities: String::new(),
                });
            }
            "fpr" => {
                if let Some(last) = candidates.last_mut()
                    && last.fingerprint.is_empty()
                {
                    last.fingerprint = fields.get(9).copied().unwrap_or_default().to_string();
                }
            }
            "uid" => {
                if let Some(last) = candidates.last_mut()
                    && last.uid.is_empty()
                {
                    last.uid = unescape_gpg_colon(fields.get(9).copied().unwrap_or_default());
                }
            }
            "ssb" | "sub" => {
                if let Some(last) = candidates.last_mut() {
                    let caps = fields
                        .get(11)
                        .filter(|v| !v.is_empty())
                        .or_else(|| fields.get(12).filter(|v| !v.is_empty()))
                        .copied()
                        .unwrap_or_default()
                        .to_string();
                    last.capabilities = caps;
                }
            }
            _ => {}
        }
    }

    candidates
}

fn format_gpg_algo_bits(length: &str, algo: &str) -> String {
    let algo = match algo {
        "1" => "RSA",
        "17" => "DSA",
        "18" => "ECC",
        "19" => "ECDSA",
        "22" => "Ed25519",
        "20" | "16" => "ELG",
        other => other,
    };
    if length.is_empty() || length == "0" {
        algo.to_string()
    } else {
        format!("{algo} {length}")
    }
}

fn unescape_gpg_colon(value: &str) -> String {
    value.replace("\\x3a", ":")
}

/// Parse the version out of a `gpg --version` first line.
///
/// The first line looks like `gpg (GnuPG) 2.4.9` or `gpg (GnuPG) 2.2.27`.
/// Returns `None` when the version cannot be parsed.
pub fn parse_gpg_version(first_line: &str) -> Option<(u32, u32)> {
    // find the first version-like token `X.Y.Z`
    for token in first_line.split_whitespace() {
        let token = token.trim_end_matches('.');
        if let Some((major, rest)) = token.split_once('.')
            && major.chars().all(|c| c.is_ascii_digit())
            && !major.is_empty()
        {
            let minor = rest.split('.').next().unwrap_or("0");
            if let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) {
                return Some((major, minor));
            }
        }
    }
    None
}

/// True when the parsed version is at least `min_major.min_minor`.
pub fn gpg_version_at_least(version: Option<(u32, u32)>, min_major: u32, min_minor: u32) -> bool {
    match version {
        Some((major, minor)) => major > min_major || (major == min_major && minor >= min_minor),
        None => false,
    }
}

/// Run `gpg --with-colons --list-secret-keys` and return the raw colon output
/// plus the first line of `gpg --version`. Reads the user's GnuPG home only
/// (read-only); never modifies it.
pub async fn discover_gpg_secret_keys(
    gpg_program: &str,
    gnupghome: Option<&std::path::Path>,
) -> Result<(Option<(u32, u32)>, Vec<GpgCandidate>)> {
    use std::process::Stdio;

    let version_output = tokio::process::Command::new(gpg_program)
        .arg("--version")
        .output()
        .await
        .map_err(|e| anyhow!("failed to run '{gpg_program} --version': {e}"))?;
    let version_first_line = String::from_utf8_lossy(&version_output.stdout)
        .lines()
        .next()
        .map(str::to_string);
    let version = version_first_line.as_deref().and_then(parse_gpg_version);

    let mut cmd = tokio::process::Command::new(gpg_program);
    cmd.arg("--with-colons")
        .arg("--list-secret-keys")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = gnupghome {
        cmd.env("GNUPGHOME", home);
    }
    let output = cmd.output().await.map_err(|e| {
        anyhow!("failed to run '{gpg_program} --with-colons --list-secret-keys': {e}")
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("gpg --list-secret-keys failed: {}", stderr.trim()));
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok((version, parse_gpg_colon_list(&raw)))
}

/// Export the armored secret key for `fingerprint` from the GnuPG home via
/// `gpg --batch --armor --export-secret-keys <fingerprint>`.
pub async fn export_gpg_secret_key(
    gpg_program: &str,
    gnupghome: Option<&std::path::Path>,
    fingerprint: &str,
) -> Result<String> {
    use std::process::Stdio;

    let mut cmd = tokio::process::Command::new(gpg_program);
    cmd.arg("--batch")
        .arg("--armor")
        .arg("--export-secret-keys")
        .arg(fingerprint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = gnupghome {
        cmd.env("GNUPGHOME", home);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| anyhow!("failed to run '{gpg_program} --export-secret-keys': {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "gpg --export-secret-keys failed for '{fingerprint}': {}",
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Select a single signing candidate using ADR-VG-01's protocol:
/// 1. More than one candidate requires an explicit `--key` (fingerprint or
///    long/short key id).
/// 2. Exactly one candidate may omit `--key`.
/// 3. Zero candidates, or an unrecognised/multi-matching selector, is an error.
///
/// Returns the index of the selected candidate. On error the returned message
/// lists candidate fingerprints for the caller to surface.
pub fn select_signing_candidate(
    candidates: &[GpgCandidate],
    key: Option<&str>,
) -> Result<usize, String> {
    if candidates.is_empty() {
        return Err("no secret keys with signing capability found in GnuPG home".to_string());
    }

    let list = |_| {
        candidates
            .iter()
            .map(|c| c.fingerprint.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };

    match key {
        None => {
            if candidates.len() == 1 {
                Ok(0)
            } else {
                Err(format!(
                    "multiple secret keys found; specify one with --key:\n{}",
                    list(())
                ))
            }
        }
        Some(selector) => {
            let sel = selector.trim();
            let matches: Vec<usize> = candidates
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.fingerprint.eq_ignore_ascii_case(sel)
                        || c.key_id.eq_ignore_ascii_case(sel)
                        || (!c.uid.is_empty()
                            && c.uid
                                .to_ascii_lowercase()
                                .contains(&sel.to_ascii_lowercase()))
                })
                .map(|(i, _)| i)
                .collect();
            match matches.len() {
                0 => Err(format!(
                    "no secret key matching '{selector}' found; candidates:\n{}",
                    list(())
                )),
                1 => Ok(matches[0]),
                _ => Err(format!(
                    "selector '{selector}' is ambiguous (matches {} keys); use --key <fingerprint>:\n{}",
                    matches.len(),
                    list(())
                )),
            }
        }
    }
}

/// Rebuild a password-protected ASCII-armored transferable secret key into an
/// unprotected one (ADR-VG-10). On wrong passphrase this returns `Err` before
/// any output is produced, so the caller can guarantee zero-write.
pub fn rebuild_unprotected_certificate(armor: &str, passphrase: &str) -> Result<String> {
    use pgp::{
        composed::{ArmorOptions, Deserializable, SignedSecretKey},
        types::Password,
    };

    let (mut skey, _headers) = SignedSecretKey::from_armor_single(armor.as_bytes())
        .context("failed to parse armored secret key")?;

    let pw = Password::from(passphrase);
    skey.primary_key
        .remove_password(&pw)
        .context("failed to unlock secret key — wrong passphrase or corrupt key")?;
    for sub in skey.secret_subkeys.iter_mut() {
        sub.key
            .remove_password(&pw)
            .context("failed to unlock signing subkey — wrong passphrase or corrupt key")?;
    }

    skey.to_armored_string(ArmorOptions::default())
        .context("failed to serialize unprotected secret key")
}

/// Parse an armored secret or public key block and return its primary-key
/// fingerprint as an uppercase hex string (with subkey material ignored).
pub fn key_fingerprint_from_armor(armor: &str) -> Result<String> {
    use pgp::{
        composed::{Deserializable, SignedPublicKey},
        types::KeyDetails,
    };

    if let Ok((key, _)) = SignedPublicKey::from_armor_single(armor.as_bytes()) {
        return Ok(format!("{:X}", key.primary_key.fingerprint()));
    }

    use pgp::composed::SignedSecretKey;
    let (key, _) = SignedSecretKey::from_armor_single(armor.as_bytes())
        .context("failed to parse armored key")?;
    Ok(format!("{:X}", key.primary_key.fingerprint()))
}

/// Extract the primary user id from an armored secret key, if one exists.
pub fn primary_uid_from_armor(armor: &str) -> String {
    use pgp::composed::{Deserializable, SignedSecretKey};

    let Ok((key, _)) = SignedSecretKey::from_armor_single(armor.as_bytes()) else {
        return String::new();
    };
    key.details
        .users
        .first()
        .map(|u| String::from_utf8_lossy(u.id.id()).into_owned())
        .unwrap_or_default()
}

/// Describe the real algorithm/bits of an armored public key (VG-06), so the
/// list surface is not stuck on a hard-coded `PGP 2048`.
pub fn describe_public_key(armor: &str) -> Result<String> {
    use pgp::{
        composed::{Deserializable, SignedPublicKey},
        crypto::public_key::PublicKeyAlgorithm,
        types::{KeyDetails, PublicParams},
    };
    use rsa::traits::PublicKeyParts;

    let (key, _) = SignedPublicKey::from_armor_single(armor.as_bytes())
        .context("failed to parse public key")?;
    let alg = key.primary_key.algorithm();
    let name = match alg {
        PublicKeyAlgorithm::RSA | PublicKeyAlgorithm::RSAEncrypt | PublicKeyAlgorithm::RSASign => {
            "RSA"
        }
        PublicKeyAlgorithm::EdDSALegacy | PublicKeyAlgorithm::Ed25519 => "Ed25519",
        PublicKeyAlgorithm::ECDSA => "ECDSA",
        PublicKeyAlgorithm::ECDH => "ECDH",
        PublicKeyAlgorithm::DSA => "DSA",
        PublicKeyAlgorithm::Ed448 => "Ed448",
        PublicKeyAlgorithm::X25519 => "X25519",
        PublicKeyAlgorithm::X448 => "X448",
        other => return Ok(format!("{other:?}")),
    };
    let bits = match key.primary_key.public_params() {
        PublicParams::RSA(rsa) => Some(rsa.key.n().bits()),
        _ => None,
    };
    Ok(match bits {
        Some(bits) => format!("{name} {bits}"),
        None => name.to_string(),
    })
}

/// Material extracted from an imported secret key during `import-gpg-key`.
#[derive(Debug, Clone)]
pub struct ImportedKeyMaterial {
    pub fingerprint: String,
    pub signing_key_id: String,
    pub uid: String,
    /// Self-wiping: the unprotected certificate is plaintext key material
    /// (plan GC-VG-01), so the buffer is zeroed when it is dropped.
    pub rebuilt_armor: zeroize::Zeroizing<String>,
    pub pubkey_armor: String,
}

fn algorithm_can_sign(alg: pgp::crypto::public_key::PublicKeyAlgorithm) -> bool {
    use pgp::crypto::public_key::PublicKeyAlgorithm::*;
    matches!(
        alg,
        RSA | RSASign | DSA | ECDSA | EdDSALegacy | Ed25519 | Ed448
    )
}

/// ADR-VG-09: a subkey only qualifies as a signer when its hashed binding
/// self-signature grants the `sign` key flag. An explicit flag set without
/// `sign` (encryption-only subkeys) is rejected, while a binding signature that
/// carries no key flags at all stays eligible on algorithm capability alone
/// (older implementations do not always emit flags).
/// ADR-VG-09: a subkey whose binding self-signature declares an expiry that has
/// already passed must not be selected as the signing key. `None` means the
/// binding declares no expiry at all. When several bindings exist (GnuPG
/// supersedes expiry by adding a newer binding signature) the latest declared
/// expiry wins.
fn subkey_declared_expiry(signatures: &[pgp::packet::Signature]) -> Option<u64> {
    use pgp::packet::{SignatureType, SubpacketData};

    let mut latest: Option<u64> = None;
    for sig in signatures {
        if sig.typ() != Some(SignatureType::SubkeyBinding) {
            continue;
        }
        let Some(config) = sig.config() else {
            continue;
        };
        let mut created: Option<u64> = None;
        let mut duration: Option<u64> = None;
        for sub in config.hashed_subpackets() {
            match &sub.data {
                SubpacketData::SignatureCreationTime(ts) => created = Some(u64::from(ts.as_secs())),
                SubpacketData::KeyExpirationTime(key_expiration) => {
                    duration = Some(u64::from(key_expiration.as_secs()));
                }
                _ => {}
            }
        }
        if let (Some(created), Some(duration)) = (created, duration) {
            let expiry = created.saturating_add(duration);
            latest = Some(latest.map_or(expiry, |current| current.max(expiry)));
        }
    }
    latest
}

/// Seconds since the Unix epoch (0 if the clock is somehow before it).
fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn binding_signature_allows_signing(signatures: &[pgp::packet::Signature]) -> bool {
    use pgp::packet::{SignatureType, SubpacketData};

    let mut explicit: Option<bool> = None;
    for sig in signatures {
        if sig.typ() != Some(SignatureType::SubkeyBinding) {
            continue;
        }
        let Some(config) = sig.config() else {
            continue;
        };
        for sub in config.hashed_subpackets() {
            if let SubpacketData::KeyFlags(flags) = &sub.data {
                explicit = Some(explicit.unwrap_or(false) || flags.sign());
            }
        }
    }
    explicit.unwrap_or(true)
}

/// Parse a (possibly protected) armored secret key, validate the passphrase,
/// rebuild an unprotected transferable certificate, and select the signing
/// (sub)key (ADR-VG-09 / ADR-VG-10).
///
/// Selection: prefer the newest secret signing subkey with a SubkeyBinding and
/// no SubkeyRevocation signature; deterministic by legacy key id on ties. When
/// no usable signing subkey exists, fall back to the primary key.
pub fn prepare_imported_key(armor: &str, passphrase: &str) -> Result<ImportedKeyMaterial> {
    use pgp::{
        composed::{ArmorOptions, Deserializable, SignedSecretKey},
        packet::SignatureType,
        types::{KeyDetails, Password},
    };

    let (skey, _headers) = SignedSecretKey::from_armor_single(armor.as_bytes())
        .context("failed to parse armored secret key")?;

    // ADR-VG-09: a revoked certificate must never become the signing key. The
    // key revocation signature lives on the primary key's details.
    if !skey.details.revocation_signatures.is_empty() {
        return Err(anyhow!(
            "the certificate is revoked and cannot be imported as a signing key"
        ));
    }

    // ADR-VG-09 / VG-05 G7: a subkey is only eligible when its SubkeyBinding
    // signature is verifiably issued by *this* certificate's primary key, so a
    // forged certificate cannot smuggle in a foreign-bound signing subkey.
    let public = skey.to_public_key();
    let foreign_bound_subkeys: Vec<String> = public
        .public_subkeys
        .iter()
        .filter(|sub| sub.verify_bindings(&public.primary_key).is_err())
        .map(|sub| format!("{}", sub.key.legacy_key_id()))
        .collect();

    let fingerprint = format!("{:X}", skey.primary_key.fingerprint());
    let uid = skey
        .details
        .users
        .first()
        .map(|u| String::from_utf8_lossy(u.id.id()).into_owned())
        .unwrap_or_default();

    let pw = Password::from(passphrase);

    // Select the newest valid signing subkey (deterministic by key id on tie).
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for sub in &skey.secret_subkeys {
        let has_binding = sub
            .signatures
            .iter()
            .any(|s| s.typ() == Some(SignatureType::SubkeyBinding));
        let has_revocation = sub
            .signatures
            .iter()
            .any(|s| s.typ() == Some(SignatureType::SubkeyRevocation));
        let key_id = format!("{}", sub.key.legacy_key_id());
        if has_binding
            && !has_revocation
            && !foreign_bound_subkeys.contains(&key_id)
            && algorithm_can_sign(sub.key.algorithm())
            && binding_signature_allows_signing(&sub.signatures)
            && subkey_declared_expiry(&sub.signatures)
                .is_none_or(|expiry| expiry > now_epoch_secs())
        {
            candidates.push((key_id, sub.key.created_at().as_secs()));
        }
    }
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let signing_key_id = candidates
        .first()
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| format!("{}", skey.primary_key.legacy_key_id()));

    // Rebuild unprotected certificate (also validates the passphrase).
    let mut skey = skey;
    skey.primary_key
        .remove_password(&pw)
        .context("failed to unlock secret key — wrong passphrase or corrupt key")?;
    for sub in skey.secret_subkeys.iter_mut() {
        sub.key
            .remove_password(&pw)
            .context("failed to unlock signing subkey — wrong passphrase or corrupt key")?;
    }
    let rebuilt_armor = zeroize::Zeroizing::new(
        skey.to_armored_string(ArmorOptions::default())
            .context("failed to serialize unprotected secret key")?,
    );
    let pubkey_armor = skey
        .to_public_key()
        .to_armored_string(ArmorOptions::default())
        .context("failed to serialize public key")?;

    Ok(ImportedKeyMaterial {
        fingerprint,
        signing_key_id,
        uid,
        rebuilt_armor,
        pubkey_armor,
    })
}

/// Sign raw data in-process with an unprotected armored secret key, selecting
/// the key identified by `signing_key_id` (primary or subkey legacy key id).
/// Returns the hex-encoded detached signature (existing wire encoding).
pub fn sign_with_armored_secret_key(
    armored_secret: &str,
    signing_key_id: &str,
    data: &[u8],
) -> Result<String> {
    use pgp::{
        composed::{Deserializable, SignedSecretKey},
        types::{KeyDetails, Password},
    };

    let (skey, _) = SignedSecretKey::from_armor_single(armored_secret.as_bytes())
        .context("failed to parse secret key for signing")?;
    let pw = Password::empty();

    // Match the selected signing key by legacy key id; default to primary.
    let sign_result = if skey
        .primary_key
        .legacy_key_id()
        .to_string()
        .eq_ignore_ascii_case(signing_key_id)
    {
        sign_detached(&skey.primary_key, &pw, data)
    } else {
        let sub = skey
            .secret_subkeys
            .iter()
            .find(|s| {
                s.key
                    .legacy_key_id()
                    .to_string()
                    .eq_ignore_ascii_case(signing_key_id)
            })
            .ok_or_else(|| {
                anyhow!("signing key id '{signing_key_id}' not found in imported key")
            })?;
        sign_detached(&sub.key, &pw, data)
    }?;

    Ok(sign_result)
}

fn sign_detached<K: pgp::types::SigningKey>(
    key: &K,
    pw: &pgp::types::Password,
    data: &[u8],
) -> Result<String> {
    use pgp::{
        composed::{ArmorOptions, DetachedSignature},
        crypto::hash::HashAlgorithm,
    };

    let rng = rand::rngs::OsRng;
    let sig = DetachedSignature::sign_binary_data(rng, key, pw, HashAlgorithm::Sha256, data)
        .context("failed to produce detached signature")?;
    // No armor checksum: `armored_to_signature_hex` expects a bare base64 body
    // matching the legacy wire encoding produced by `signature_to_armored`.
    let opts = ArmorOptions {
        headers: None,
        include_checksum: false,
    };
    let armored_sig = sig
        .to_armored_string(opts)
        .context("failed to armor signature")?;
    armored_to_signature_hex(&armored_sig)
}

/// Upper bound for the issuer-absent fallback (ADR-VG-06 ④): verification must
/// not scan an arbitrarily large candidate set when the signature names no
/// issuer. The plan fixes the bound at 16 candidate (sub)keys.
const ISSUER_ABSENT_FALLBACK_CAP: usize = 16;

/// Whether an issuer-absent signature may still be checked after
/// `candidate_count` candidate (sub)keys. Attempts beyond the cap are refused
/// outright so that the fallback stays bounded and fails closed.
fn issuer_absent_fallback_allowed(candidate_count: usize) -> bool {
    candidate_count <= ISSUER_ABSENT_FALLBACK_CAP
}

/// Records one more issuer-absent candidate attempt and reports whether the
/// attempt is still inside the cap (ADR-VG-06 ④). The counter only advances
/// while the attempt is allowed, so the caller refuses from candidate
/// `ISSUER_ABSENT_FALLBACK_CAP + 1` on.
fn issuer_absent_candidate_allowed(tried: &mut usize) -> bool {
    if !issuer_absent_fallback_allowed(tried.saturating_add(1)) {
        return false;
    }
    *tried += 1;
    true
}

/// plan-20260921 VG-05 G13/G14: verification is evaluated at the **signature's
/// own creation time** (the hashed `SignatureCreationTime` subpacket) rather
/// than at "now" — a signature made while the key was still valid stays
/// acceptable even if the key was revoked or expired later, while a signature
/// made after revocation/expiry is refused.
fn signature_creation_time_secs(sig: &pgp::packet::Signature) -> Option<u64> {
    use pgp::packet::SubpacketData;

    sig.config()?
        .hashed_subpackets()
        .find_map(|sub| match &sub.data {
            SubpacketData::SignatureCreationTime(stamp) => Some(u64::from(stamp.as_secs())),
            _ => None,
        })
}

/// Latest revocation time (epoch seconds) of the certificate itself, if any.
fn key_revoked_at(key: &pgp::composed::SignedPublicKey) -> Option<u64> {
    use pgp::packet::SignatureType;

    let mut latest: Option<u64> = None;
    for sig in &key.details.revocation_signatures {
        if sig.typ() != Some(SignatureType::KeyRevocation) {
            continue;
        }
        if let Some(at) = sig.config().and_then(|config| {
            config.hashed_subpackets().find_map(|sub| match &sub.data {
                pgp::packet::SubpacketData::SignatureCreationTime(stamp) => {
                    Some(u64::from(stamp.as_secs()))
                }
                _ => None,
            })
        }) {
            latest = Some(latest.map_or(at, |current: u64| current.max(at)));
        }
    }
    latest
}

/// Latest revocation time (epoch seconds) of one subkey, if any.
fn subkey_revoked_at(sub: &pgp::composed::SignedPublicSubKey) -> Option<u64> {
    use pgp::packet::SignatureType;

    let mut latest: Option<u64> = None;
    for sig in &sub.signatures {
        if sig.typ() != Some(SignatureType::SubkeyRevocation) {
            continue;
        }
        if let Some(at) = sig.config().and_then(|config| {
            config.hashed_subpackets().find_map(|sub| match &sub.data {
                pgp::packet::SubpacketData::SignatureCreationTime(stamp) => {
                    Some(u64::from(stamp.as_secs()))
                }
                _ => None,
            })
        }) {
            latest = Some(latest.map_or(at, |current: u64| current.max(at)));
        }
    }
    latest
}

/// Whether a revocation that happened at `revoked_at` must refuse a signature
/// made at `signature_at`. Pure so both time orders are unit-tested.
fn revocation_applies(revoked_at: Option<u64>, signature_at: u64) -> bool {
    revoked_at.is_some_and(|revoked| revoked <= signature_at)
}

/// Whether a declared expiry must refuse a signature made at `signature_at`.
fn expiry_applies(expiry: Option<u64>, signature_at: u64) -> bool {
    expiry.is_some_and(|expires| expires <= signature_at)
}

fn issuer_matches(issuers: &[&pgp::types::KeyId], key_id: &pgp::types::KeyId) -> bool {
    issuers.is_empty() || issuers.contains(&key_id)
}

/// Verify a hex-encoded detached `signature` over `data` against a list of
/// armored public keys (active → generated → history, in caller order).
/// Returns `true` when any listed public key verifies the signature.
/// Parse failures on individual keys are skipped (debug) and never fatal.
pub fn verify_signature_hex(signature_hex: &str, data: &[u8], pubkey_armors: &[String]) -> bool {
    use pgp::{
        composed::{Deserializable, DetachedSignature, SignedPublicKey},
        types::KeyDetails,
    };

    let armored = match signature_to_armored(signature_hex) {
        Ok(a) => a,
        Err(_) => return false,
    };
    let parsed = match DetachedSignature::from_armor_single(armored.as_bytes()) {
        Ok((sig, _)) => sig,
        Err(_) => return false,
    };
    let issuers = parsed.signature.issuer_key_id();

    // ADR-VG-06 ④: a signature without an issuer may fall back to trying the
    // candidate (sub)keys of the listed certificates, but only within a bounded
    // number of candidates — an unbounded scan would let a long key list
    // amplify verification work. The (cap + 1)-th candidate is refused outright
    // so the fallback fails closed.
    let issuer_absent = issuers.is_empty();
    let mut issuer_absent_candidates: usize = 0;
    // ADR-VG-06 ⑤ / VG-05 G13–G14: evaluate revocation and expiry at the
    // signature's creation time, falling back to "now" when the signature
    // carries none.
    let signature_at =
        signature_creation_time_secs(&parsed.signature).unwrap_or_else(now_epoch_secs);

    for armor in pubkey_armors {
        let Ok((key, _)) = SignedPublicKey::from_armor_single(armor.as_bytes()) else {
            continue;
        };
        let primary_id = key.primary_key.legacy_key_id();
        if issuer_matches(&issuers, &primary_id) {
            if issuer_absent && !issuer_absent_candidate_allowed(&mut issuer_absent_candidates) {
                return false;
            }
            if !revocation_applies(key_revoked_at(&key), signature_at)
                && parsed.verify(&key, data).is_ok()
            {
                return true;
            }
        }
        for sub in &key.public_subkeys {
            if issuer_matches(&issuers, &sub.key.legacy_key_id()) {
                if issuer_absent && !issuer_absent_candidate_allowed(&mut issuer_absent_candidates)
                {
                    return false;
                }
                if !revocation_applies(subkey_revoked_at(sub), signature_at)
                    && !expiry_applies(subkey_declared_expiry(&sub.signatures), signature_at)
                    && parsed.verify(sub, data).is_ok()
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Persist an imported (rebuilt unprotected) secret key and its metadata into
/// the local config (ADR-VG-02/03). `source` is written last; any failure
/// before that propagates and leaves no `source=imported` marker.
///
/// History invariant (ADR-VG-04): when an active public key is about to be
/// overwritten, its fingerprint is recorded in `vault.gpg.history.<FPR>.pubkey`
/// first. This function is only called for a *different* fingerprint (duplicate
/// imports short-circuit in the CLI) so the caller preserves idempotency.
pub async fn persist_imported_gpg_key(
    unseal_key: &[u8],
    material: &ImportedKeyMaterial,
) -> Result<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::config::ConfigKv;

    // ADR-VG-02/03: every write below is part of one transaction, so a failure
    // at any step — including the sensitive secret-key write — leaves the
    // repository exactly as it was instead of half-switched to the imported key.
    let db = crate::internal::db::get_db_conn_instance().await;
    let txn = db
        .begin()
        .await
        .context("failed to start the imported GPG key transaction")?;

    // VG-13 G1/G2: record the current active key (and a generated snapshot when
    // needed) before any overwrite.
    snapshot_active_key_to_history_with_conn(&txn).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    let seckey_hex = hex::encode(encrypt_token(
        unseal_key,
        material.rebuilt_armor.as_bytes(),
    )?);

    ConfigKv::set_with_conn(&txn, "vault.gpg.pubkey", &material.pubkey_armor, false).await?;
    ConfigKv::set_with_conn(&txn, "vault.gpg.fingerprint", &material.fingerprint, false).await?;
    ConfigKv::set_with_conn(
        &txn,
        "vault.gpg.signing_key_id",
        &material.signing_key_id,
        false,
    )
    .await?;
    ConfigKv::set_with_conn(&txn, "vault.gpg.uid", &material.uid, false).await?;
    ConfigKv::set_with_conn(&txn, "vault.gpg.imported_at", &now, false).await?;
    ConfigKv::set_with_conn(&txn, "vault.gpg.seckey_enc", &seckey_hex, true).await?;
    // source last: presence of `imported` is the commit point.
    ConfigKv::set_with_conn(&txn, "vault.gpg.source", "imported", false).await?;

    // ADR-VG-12: first import with `vault.signing` unset enables signing; an
    // explicit `false` stays off (the caller prints the hint).
    let signing = ConfigKv::get_with_conn(&txn, "vault.signing")
        .await
        .ok()
        .flatten()
        .map(|e| e.value);
    if signing.is_none() {
        ConfigKv::set_with_conn(&txn, "vault.signing", "true", false).await?;
    }

    txn.commit()
        .await
        .context("failed to commit the imported GPG key")?;
    Ok(())
}

/// plan-20260921 VG-14 G1–G6: `#[cfg(test)]`-only injection point so each step
/// of the staged generated-key migration ((a) history snapshot, (b) vault key
/// generation, (c) `generated_pubkey`, (d1) `generated_key_name`,
/// (d2) `vault.gpg.pubkey`, (e) `source`) can be failed individually.
fn generate_failpoint(step: u8) -> Result<()> {
    #[cfg(test)]
    if GENERATE_FAIL_AT.load(std::sync::atomic::Ordering::SeqCst) == step {
        GENERATE_FAIL_AT.store(0, std::sync::atomic::Ordering::SeqCst);
        return Err(anyhow!("injected generation failure after step {step}"));
    }
    let _ = step;
    Ok(())
}

#[cfg(test)]
static GENERATE_FAIL_AT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
fn set_generate_fail_at(step: u8) {
    GENERATE_FAIL_AT.store(step, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn clear_generate_fail_at() {
    GENERATE_FAIL_AT.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Record the currently-active public key into `history.<FPR>.pubkey` (idempotent
/// by fingerprint). When `source == generated` and no `vault.gpg.generated_pubkey`
/// snapshot exists yet, snapshot it first (ADR-VG-04 §4 / VG-13 G2).
async fn snapshot_active_key_to_history() -> Result<()> {
    let db = crate::internal::db::get_db_conn_instance().await;
    snapshot_active_key_to_history_with_conn(&db).await
}

/// plan-20260921 VG-08 G9–G12: `#[cfg(test)]`-only injection point so each of
/// the four removal steps can be failed individually. ADR-VG-07 §2 forbids env
/// hooks, so the seam exists only in test builds — the same approach the two
/// generator injection gates use.
#[cfg(test)]
static REMOVE_FAIL_AT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
fn set_remove_fail_at(step: u8) {
    REMOVE_FAIL_AT.store(step, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn clear_remove_fail_at() {
    REMOVE_FAIL_AT.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Fail the step named by the injected value (once), or do nothing.
fn remove_failpoint(step: u8) -> Result<()> {
    #[cfg(test)]
    if REMOVE_FAIL_AT.load(std::sync::atomic::Ordering::SeqCst) == step {
        REMOVE_FAIL_AT.store(0, std::sync::atomic::Ordering::SeqCst);
        return Err(anyhow!("injected removal failure at step {step}"));
    }
    let _ = step;
    Ok(())
}

/// [`snapshot_active_key_to_history`] against a caller-provided connection, so
/// the whole import sequence can run inside one transaction.
async fn snapshot_active_key_to_history_with_conn<C>(db: &C) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    use crate::internal::config::ConfigKv;

    let active = match ConfigKv::get_with_conn(db, "vault.gpg.pubkey")
        .await
        .ok()
        .flatten()
    {
        Some(e) if !e.value.is_empty() => e.value,
        _ => return Ok(()),
    };

    let source = ConfigKv::get_with_conn(db, "vault.gpg.source")
        .await
        .ok()
        .flatten()
        .map(|e| e.value);
    if source.as_deref() == Some("generated") {
        let snapshot = ConfigKv::get_with_conn(db, "vault.gpg.generated_pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        if snapshot.is_none() {
            ConfigKv::set_with_conn(db, "vault.gpg.generated_pubkey", &active, false).await?;
        }
    }

    let fp = key_fingerprint_from_armor(&active).unwrap_or_else(|_| String::new());
    if fp.is_empty() {
        return Ok(());
    }
    let key = format!("vault.gpg.history.{fp}.pubkey");
    if ConfigKv::get_with_conn(db, &key).await?.is_none() {
        ConfigKv::set_with_conn(db, &key, &active, false).await?;
    }
    Ok(())
}

/// The `source` value for the active GPG key (`generated`/`imported`/empty).
pub async fn gpg_source() -> Option<String> {
    use crate::internal::config::ConfigKv;
    ConfigKv::get("vault.gpg.source")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
}

/// Remove an active imported GPG key and fall back to the generated key
/// (ADR-VG-06 §3/4). Deletes only the allowlist keys; `history.*`,
/// `generated_pubkey` and `generated_key_name` are never touched.
pub async fn remove_imported_gpg_key() -> Result<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::config::ConfigKv;

    // plan-20260921 VG-08 G9–G12: the removal is ONE transaction, so a failure
    // at any of the four steps (archive the active key, drop the imported
    // metadata, restore the generated public half, switch `source`) rolls back
    // and leaves the imported key exactly as it was. Without the transaction a
    // mid-way failure would leave, say, `source=imported` with the secret key
    // already deleted — signing would then fail far from the cause.
    let db = crate::internal::db::get_db_conn_instance().await;
    let txn = db
        .begin()
        .await
        .context("failed to start the GPG key removal transaction")?;

    snapshot_active_key_to_history_with_conn(&txn).await?;
    remove_failpoint(1)?;

    for key in [
        "vault.gpg.seckey_enc",
        "vault.gpg.fingerprint",
        "vault.gpg.signing_key_id",
        "vault.gpg.uid",
        "vault.gpg.imported_at",
    ] {
        ConfigKv::unset_with_conn(&txn, key).await?;
    }
    remove_failpoint(2)?;

    let generated = ConfigKv::get_with_conn(&txn, "vault.gpg.generated_pubkey")
        .await
        .ok()
        .flatten()
        .map(|e| e.value);
    match generated.filter(|g| !g.is_empty()) {
        Some(g) => ConfigKv::set_with_conn(&txn, "vault.gpg.pubkey", &g, false).await?,
        None => {
            let _ = ConfigKv::unset_with_conn(&txn, "vault.gpg.pubkey").await;
        }
    }
    remove_failpoint(3)?;

    ConfigKv::set_with_conn(&txn, "vault.gpg.source", "generated", false).await?;
    remove_failpoint(4)?;

    txn.commit()
        .await
        .context("failed to commit the GPG key removal")?;
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

    /// plan-20260921: imported GPG key discovery, selection, desprotection and
    /// sign/verify round-trips. Fixtures in `tests/data/fake-gpg/` are fixed
    /// (protected secret key passphrase `libra-test-fixture-passphrase`).
    mod gpg_import {
        use super::super::*;

        const SECRET: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/fake-gpg/protected-secret.asc"
        );
        const PUBLIC: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/fake-gpg/pubkey.asc"
        );
        const PASSPHRASE: &str = "libra-test-fixture-passphrase";

        #[test]
        fn gpg_colon_list_parses_fingerprint_uid_and_capabilities() {
            let sample = concat!(
                "sec:u:25519:22:4FB6368B886D5973:1790096777::u:::cEsSC::::::22::",
                ":::::::",
                "\n",
                "fpr:::::::::6362FF0BA5456A8E9C7DD8C04FB6368B886D5973:\n",
                "uid:u::::1790096777::1234::libra-test-fixture <fixture@libra.invalid>:::::::::my::",
                "\n",
                "ssb:u:25519:22:237B8B4AF7B4A7EF:1790096777::::::s::::::22::::::",
                "\n",
                "fpr:::::::::237B8B4AF7B4A7EF:1790096777:::::::\n",
            );
            let candidates = parse_gpg_colon_list(sample);
            assert_eq!(candidates.len(), 1);
            let c = &candidates[0];
            assert_eq!(c.fingerprint, "6362FF0BA5456A8E9C7DD8C04FB6368B886D5973");
            assert_eq!(c.key_id, "4FB6368B886D5973");
            assert!(c.uid.contains("libra-test-fixture"));
            assert_eq!(c.algorithm_bits, "Ed25519 25519");
            assert_eq!(c.capabilities, "s");
        }

        #[test]
        fn gpg_version_gate_rejects_old_or_unparsable_version() {
            assert!(gpg_version_at_least(
                parse_gpg_version("gpg (GnuPG) 2.4.9"),
                2,
                2
            ));
            assert!(gpg_version_at_least(
                parse_gpg_version("gpg (GnuPG) 2.2.0"),
                2,
                2
            ));
            assert!(!gpg_version_at_least(
                parse_gpg_version("gpg (GnuPG) 2.1.22"),
                2,
                2
            ));
            assert!(!gpg_version_at_least(
                parse_gpg_version("gpg (GnuPG) 1.4.20"),
                2,
                2
            ));
            assert!(!gpg_version_at_least(
                parse_gpg_version("gpg (GnuPG)"),
                2,
                2
            ));
        }

        #[test]
        fn gpg_selector_requires_explicit_key_when_multiple_candidates() {
            let candidates = vec![
                GpgCandidate {
                    fingerprint: "AA".into(),
                    key_id: "1".into(),
                    uid: "a".into(),
                    algorithm_bits: "Ed25519".into(),
                    capabilities: "s".into(),
                },
                GpgCandidate {
                    fingerprint: "BB".into(),
                    key_id: "2".into(),
                    uid: "b".into(),
                    algorithm_bits: "Ed25519".into(),
                    capabilities: "s".into(),
                },
            ];
            assert!(select_signing_candidate(&candidates, None).is_err());
            assert!(select_signing_candidate(&candidates, Some("AA")).is_ok());
        }

        #[test]
        fn gpg_selector_auto_selects_single_signing_candidate() {
            let candidates = vec![GpgCandidate {
                fingerprint: "AA".into(),
                key_id: "1".into(),
                uid: "a".into(),
                algorithm_bits: "Ed25519".into(),
                capabilities: "s".into(),
            }];
            assert_eq!(select_signing_candidate(&candidates, None), Ok(0));
        }

        #[test]
        fn gpg_selector_rejects_no_candidate_and_ambiguous_email() {
            assert!(select_signing_candidate(&[], None).is_err());
            let dup = vec![
                GpgCandidate {
                    fingerprint: "AA".into(),
                    key_id: "1".into(),
                    uid: "shared".into(),
                    algorithm_bits: "Ed25519".into(),
                    capabilities: "s".into(),
                },
                GpgCandidate {
                    fingerprint: "BB".into(),
                    key_id: "2".into(),
                    uid: "shared x".into(),
                    algorithm_bits: "Ed25519".into(),
                    capabilities: "s".into(),
                },
            ];
            assert!(select_signing_candidate(&dup, Some("shared")).is_err());
        }

        #[test]
        fn rebuilt_certificate_is_unprotected_and_fingerprint_stable() {
            let secret = std::fs::read_to_string(SECRET).unwrap();
            let before = key_fingerprint_from_armor(&secret).unwrap();
            let material = prepare_imported_key(&secret, PASSPHRASE).unwrap();
            let after = key_fingerprint_from_armor(&material.rebuilt_armor).unwrap();
            assert_eq!(before, after);
            // Rebuilt certificate signs without any passphrase using the selected key.
            let sig = sign_with_armored_secret_key(
                &material.rebuilt_armor,
                &material.signing_key_id,
                b"payload",
            )
            .unwrap();
            let public = std::fs::read_to_string(PUBLIC).unwrap();
            assert!(verify_signature_hex(&sig, b"payload", &[public]));
        }

        #[test]
        fn rebuilt_certificate_can_sign_roundtrip() {
            let secret = std::fs::read_to_string(SECRET).unwrap();
            let material = prepare_imported_key(&secret, PASSPHRASE).unwrap();
            let sig = sign_with_armored_secret_key(
                &material.rebuilt_armor,
                &material.signing_key_id,
                b"roundtrip data",
            )
            .unwrap();
            assert!(verify_signature_hex(
                &sig,
                b"roundtrip data",
                std::slice::from_ref(&material.pubkey_armor)
            ));
            assert!(!verify_signature_hex(
                &sig,
                b"different data",
                &[material.pubkey_armor]
            ));
        }

        #[test]
        fn wrong_passphrase_fails_before_rebuild() {
            let secret = std::fs::read_to_string(SECRET).unwrap();
            assert!(rebuild_unprotected_certificate(&secret, "wrong-passphrase").is_err());
        }
    }
    mod global_unseal_key {
        use std::path::Path;

        use crate::{
            internal::vault::{
                UNSEAL_KEY_LEN, decrypt_token, encrypt_token, global_unseal_key_path,
                lazy_init_vault_for_scope, legacy_global_unseal_key_path,
                load_unseal_key_for_scope,
            },
            utils::test::ScopedEnvVar,
        };

        fn key(seed: u8) -> Vec<u8> {
            vec![seed; UNSEAL_KEY_LEN]
        }

        fn write_key(path: &Path, value: &[u8]) {
            std::fs::create_dir_all(path.parent().expect("key parent")).expect("create key dir");
            std::fs::write(path, hex::encode(value)).expect("write key");
        }

        struct Home {
            _env: ScopedEnvVar,
            _root: tempfile::TempDir,
            new_path: std::path::PathBuf,
            legacy_path: std::path::PathBuf,
        }

        fn isolated_home() -> Home {
            let root = tempfile::tempdir().expect("tempdir");
            let env = ScopedEnvVar::set("LIBRA_TEST_HOME", root.path());
            let new_path = global_unseal_key_path().expect("new key path");
            let legacy_path = legacy_global_unseal_key_path().expect("legacy key path");
            assert!(new_path.starts_with(root.path()), "{new_path:?}");
            assert!(legacy_path.starts_with(root.path()), "{legacy_path:?}");
            Home {
                _env: env,
                _root: root,
                new_path,
                legacy_path,
            }
        }

        /// A legacy-only key is adopted, copied into the configuration
        /// directory, and the legacy file is left exactly as it was.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn legacy_key_is_copied_forward_and_left_untouched() {
            let home = isolated_home();
            write_key(&home.legacy_path, &key(0xAB));
            let legacy_bytes = std::fs::read(&home.legacy_path).expect("read legacy");

            let loaded = load_unseal_key_for_scope("global").await;

            assert_eq!(loaded, Some(key(0xAB)));
            assert!(home.new_path.exists(), "the key must be copied forward");
            assert_eq!(
                std::fs::read(&home.new_path).expect("read new"),
                legacy_bytes
            );
            assert_eq!(
                std::fs::read(&home.legacy_path).expect("read legacy"),
                legacy_bytes,
                "the legacy key file must not be rewritten"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&home.new_path)
                    .expect("stat new key")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "the key must stay owner-only");
                let dir_mode = std::fs::metadata(home.new_path.parent().expect("parent"))
                    .expect("stat key dir")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(dir_mode, 0o700);
            }
        }

        /// The configuration-directory key wins and the legacy file is ignored.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn new_key_wins_over_an_identical_legacy_copy() {
            let home = isolated_home();
            write_key(&home.new_path, &key(0x11));
            write_key(&home.legacy_path, &key(0x11));

            assert_eq!(load_unseal_key_for_scope("global").await, Some(key(0x11)));
            assert_eq!(
                std::fs::read_to_string(&home.legacy_path).expect("read legacy"),
                hex::encode(key(0x11))
            );
        }

        /// Two DIFFERENT keys fail closed. Choosing either one would make the
        /// values encrypted with the other permanently unreadable, and
        /// generating a third is worse still (GC-GCX-03).
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn conflicting_keys_fail_closed_without_rotating() {
            let home = isolated_home();
            write_key(&home.new_path, &key(0x11));
            write_key(&home.legacy_path, &key(0x22));

            assert_eq!(load_unseal_key_for_scope("global").await, None);
            let error = lazy_init_vault_for_scope("global")
                .await
                .expect_err("a key conflict must fail closed");
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("two different global vault keys exist"),
                "unexpected error: {rendered}"
            );
            assert_eq!(
                std::fs::read_to_string(&home.new_path).expect("read new"),
                hex::encode(key(0x11)),
                "no key may be rewritten"
            );
            assert_eq!(
                std::fs::read_to_string(&home.legacy_path).expect("read legacy"),
                hex::encode(key(0x22))
            );
        }

        /// Once the key has moved, an unreadable legacy backup is just a
        /// backup: it cannot be compared, but it must not break the command.
        /// (A directory at the key path is a deterministic "unreadable" that
        /// does not depend on the test user's privileges.)
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn an_unreadable_legacy_backup_is_ignored_once_the_key_has_moved() {
            let home = isolated_home();
            write_key(&home.new_path, &key(0x33));
            std::fs::create_dir_all(&home.legacy_path).expect("occupy the legacy path");

            assert_eq!(load_unseal_key_for_scope("global").await, Some(key(0x33)));
        }

        /// The same unreadable file WITHOUT a new key is fatal: it is the only
        /// candidate, and generating a replacement would rotate the key.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn an_unreadable_legacy_key_fails_closed_when_it_is_the_only_one() {
            let home = isolated_home();
            std::fs::create_dir_all(&home.legacy_path).expect("occupy the legacy path");

            assert_eq!(load_unseal_key_for_scope("global").await, None);
            let error = lazy_init_vault_for_scope("global")
                .await
                .expect_err("an unreadable sole key must fail closed");
            assert!(
                format!("{error:#}").contains("cannot read the global vault key"),
                "{error:#}"
            );
            assert!(
                !home.new_path.exists(),
                "no replacement key may be generated"
            );
        }

        /// A malformed key is an error, never an invitation to generate one.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn a_short_key_fails_closed_instead_of_rotating() {
            let home = isolated_home();
            write_key(&home.new_path, &[0x01, 0x02, 0x03]);

            let error = lazy_init_vault_for_scope("global")
                .await
                .expect_err("a malformed key must fail closed");
            let rendered = format!("{error:#}");
            assert!(rendered.contains("3 bytes instead of 32"), "{rendered}");
            assert_eq!(
                std::fs::read_to_string(&home.new_path).expect("read new"),
                hex::encode([0x01, 0x02, 0x03])
            );
        }

        /// With no key anywhere, one is generated — into the configuration
        /// directory, not the legacy Libra home.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn a_generated_key_lands_in_the_configuration_directory() {
            let home = isolated_home();

            let generated = lazy_init_vault_for_scope("global")
                .await
                .expect("generate a global key");

            assert_eq!(generated.len(), UNSEAL_KEY_LEN);
            assert!(home.new_path.exists());
            assert!(!home.legacy_path.exists(), "the legacy path stays absent");
            assert_eq!(
                lazy_init_vault_for_scope("global").await.expect("reload"),
                generated,
                "a second call must not rotate the key"
            );
        }

        /// The point of the whole card: a value encrypted before the move is
        /// still decryptable after it.
        #[tokio::test]
        #[serial_test::serial(env)]
        async fn values_encrypted_under_the_legacy_key_still_decrypt_after_the_move() {
            let home = isolated_home();
            write_key(&home.legacy_path, &key(0x5A));
            let ciphertext = encrypt_token(&key(0x5A), b"global-secret").expect("encrypt");

            let migrated = load_unseal_key_for_scope("global")
                .await
                .expect("load the migrated key");

            assert!(home.new_path.exists());
            assert_eq!(
                decrypt_token(&migrated, &ciphertext).expect("decrypt"),
                "global-secret"
            );
        }
    }

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

#[cfg(test)]
mod gpg_passphrase_tests {
    /// plan-20260921 ADR-VG-10: a protected certificate unlocks with the
    /// passphrase read from `--passphrase-file`, and a wrong passphrase is
    /// rejected before any rebuild happens.
    #[test]
    fn protected_certificate_unlocks_with_passphrase_file() {
        let armor = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/fake-gpg/protected-secret.asc"
        ))
        .expect("fixture secret key");

        let rebuilt =
            super::rebuild_unprotected_certificate(&armor, "libra-test-fixture-passphrase")
                .expect("the fixture passphrase must unlock the certificate");
        assert!(
            rebuilt.contains("PRIVATE KEY"),
            "a rebuilt certificate must stay a secret key"
        );
        assert!(
            super::rebuild_unprotected_certificate(&armor, "wrong-passphrase").is_err(),
            "a wrong passphrase must fail before rebuild"
        );
    }
}

#[cfg(test)]
mod gpg_subkey_qualification_tests {
    use super::prepare_imported_key;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 ADR-VG-09: a subkey without the signing flag must never be
    /// selected as the signing key. The encrypt-only fixture must therefore
    /// fall back to the primary key instead of the encryption subkey.
    #[test]
    fn signing_subkey_requires_sign_flag_and_valid_binding() {
        let material = prepare_imported_key(&fixture("secret-encrypt-only-subkey.asc"), "")
            .expect("the certificate itself is importable");
        assert!(
            !material
                .signing_key_id
                .eq_ignore_ascii_case("96D684119DA1BF1E"),
            "the encryption-only subkey must not be selected, got {}",
            material.signing_key_id
        );
    }

    /// A disabled (non-signing) subkey is rejected as a candidate rather than
    /// silently used; the import still succeeds through the primary key.
    #[test]
    fn disabled_subkey_is_rejected() {
        let material = prepare_imported_key(&fixture("secret-encrypt-only-subkey.asc"), "")
            .expect("primary fallback keeps the certificate importable");
        assert!(
            material
                .signing_key_id
                .eq_ignore_ascii_case("1B06C190D0A3B048"),
            "the primary key must sign when no subkey qualifies, got {}",
            material.signing_key_id
        );
    }

    /// With no usable signing subkey the primary key signs (ADR-VG-09), so a
    /// certify-only certificate is not silently rejected either.
    #[test]
    fn primary_key_signs_when_no_usable_signing_subkey() {
        let material = prepare_imported_key(&fixture("secret-primary-only.asc"), "")
            .expect("a certificate without subkeys stays importable");
        assert!(
            material
                .signing_key_id
                .eq_ignore_ascii_case("CDBB1C773B79D67E"),
            "the primary key id must be used, got {}",
            material.signing_key_id
        );
    }

    /// With two valid signing subkeys the newest one is selected, and the
    /// choice is deterministic across calls (ADR-VG-09).
    #[test]
    fn signing_subkey_selected_by_newest_valid_self_signature() {
        let armor = fixture("secret-two-signing-subkeys.asc");
        let first = prepare_imported_key(&armor, "").expect("two signing subkeys are usable");
        let second = prepare_imported_key(&armor, "").expect("deterministic re-selection");
        assert_eq!(
            first.signing_key_id, second.signing_key_id,
            "subkey choice must be deterministic"
        );
        assert!(
            first
                .signing_key_id
                .eq_ignore_ascii_case("2522AF449D04EEF9"),
            "the newest signing subkey must win, got {}",
            first.signing_key_id
        );
    }

    /// The selected signing material is a subkey, not the primary key.
    #[test]
    fn signing_subkey_material_is_used_when_present() {
        let material = prepare_imported_key(&fixture("secret-two-signing-subkeys.asc"), "")
            .expect("signing subkey fixture");
        assert!(
            material
                .signing_key_id
                .eq_ignore_ascii_case("2522AF449D04EEF9")
                || material
                    .signing_key_id
                    .eq_ignore_ascii_case("EC118D168417D1B9"),
            "a subkey key id must be selected, got {}",
            material.signing_key_id
        );
        assert!(
            !material.fingerprint.is_empty() && material.fingerprint != material.signing_key_id,
            "the primary fingerprint and the signing subkey id are distinct"
        );
    }
}

#[cfg(test)]
mod gpg_detached_signing_tests {
    use super::{prepare_imported_key, sign_with_armored_secret_key, verify_signature_hex};

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 VG-04: with `source=imported` the signing material is the
    /// imported certificate, and its detached signature verifies against the
    /// imported public key through the verification allowlist.
    #[test]
    fn imported_source_signs_detached_payload() {
        let secret = fixture("protected-secret.asc");
        let material =
            prepare_imported_key(&secret, "libra-test-fixture-passphrase").expect("imported key");
        let rebuilt =
            super::rebuild_unprotected_certificate(&secret, "libra-test-fixture-passphrase")
                .expect("rebuilt certificate");

        let payload = b"plan-20260921 detached payload";
        let signature = sign_with_armored_secret_key(&rebuilt, &material.signing_key_id, payload)
            .expect("detached signature");
        assert!(
            verify_signature_hex(&signature, payload, &[fixture("pubkey.asc")]),
            "the imported key's detached signature must verify"
        );
        assert!(
            !verify_signature_hex(&signature, b"other payload", &[fixture("pubkey.asc")]),
            "a tampered payload must not verify"
        );
    }

    /// Two calls must pick the same subkey for the same certificate (ADR-VG-09
    /// determinism by key id), independent of iteration order.
    #[test]
    fn subkey_choice_is_deterministic_by_key_id() {
        let armor = fixture("secret-two-signing-subkeys.asc");
        let first = prepare_imported_key(&armor, "").expect("first selection");
        let second = prepare_imported_key(&armor, "").expect("second selection");
        assert_eq!(first.signing_key_id, second.signing_key_id);
        assert!(
            first
                .signing_key_id
                .eq_ignore_ascii_case("2522AF449D04EEF9"),
            "the newest signing subkey must win deterministically, got {}",
            first.signing_key_id
        );
    }
}

#[cfg(test)]
mod gpg_revocation_tests {
    use super::prepare_imported_key;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 ADR-VG-09: a revoked certificate must not become the
    /// signing key. This asserts the fail-closed behaviour for the whole-key
    /// revocation case (`gpg 2.4.9` cannot revoke a lone subkey, so the subkey
    /// variant stays unverified).
    #[test]
    fn revoked_key_is_rejected_at_selection_time() {
        let result = prepare_imported_key(&fixture("secret-revoked-key.asc"), "");
        assert!(
            result.is_err(),
            "a revoked certificate must not be importable as a signer; got {:?}",
            result.map(|m| m.signing_key_id)
        );
    }
}

#[cfg(test)]
mod gpg_expiry_tests {
    use super::prepare_imported_key;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 ADR-VG-09: an expired signing subkey must be skipped, so
    /// selection falls back to the primary key instead of signing with a key
    /// every verifier treats as expired.
    #[test]
    fn expired_subkey_is_rejected_at_evaluation_time() {
        let material = prepare_imported_key(&fixture("secret-expired-subkey.asc"), "")
            .expect("the certificate itself stays importable");
        assert!(
            !material
                .signing_key_id
                .eq_ignore_ascii_case("A6688F6A48A7839B"),
            "the expired signing subkey must not be selected, got {}",
            material.signing_key_id
        );
        assert!(
            material
                .signing_key_id
                .eq_ignore_ascii_case("631383892167E13C"),
            "the primary key must sign instead, got {}",
            material.signing_key_id
        );
    }
}

#[cfg(test)]
mod gpg_revoked_subkey_tests {
    use super::prepare_imported_key;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 ADR-VG-09: a revoked signing subkey must be skipped, so
    /// selection falls back to the primary key instead of signing with a
    /// revoked (and therefore rejected) key.
    #[test]
    fn revoked_subkey_is_rejected() {
        let material = prepare_imported_key(&fixture("secret-revoked-subkey.asc"), "")
            .expect("the certificate itself stays importable");
        assert!(
            !material
                .signing_key_id
                .eq_ignore_ascii_case("A7C42C5A0F208C7A"),
            "the revoked signing subkey must not be selected, got {}",
            material.signing_key_id
        );
        assert!(
            material
                .signing_key_id
                .eq_ignore_ascii_case("8D2D0A59FBDBC481"),
            "the primary key must sign instead, got {}",
            material.signing_key_id
        );
    }
}

#[cfg(test)]
mod gpg_binding_issuer_tests {
    use super::prepare_imported_key;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 VG-05 G7 (acceptance: "binding 由本证书 primary 签发"): a
    /// subkey whose binding signature was issued by a *different* primary key
    /// must never be eligible for signing, even though its key flags say Sign.
    #[test]
    fn issuer_conflict_subkey_is_rejected() {
        use pgp::{
            composed::{ArmorOptions, Deserializable, SignedSecretKey},
            types::KeyDetails,
        };

        let (mut host, _) =
            SignedSecretKey::from_armor_single(fixture("secret-primary-only.asc").as_bytes())
                .expect("host fixture parses");
        let (donor, _) = SignedSecretKey::from_armor_single(
            fixture("secret-two-signing-subkeys.asc").as_bytes(),
        )
        .expect("donor fixture parses");
        assert!(
            host.secret_subkeys.is_empty(),
            "host fixture must start without subkeys"
        );
        assert!(
            !donor.secret_subkeys.is_empty(),
            "donor fixture must carry a signing subkey"
        );

        // Forge a certificate that carries the donor's signing subkey, whose
        // SubkeyBinding signature still names the donor's primary key.
        host.secret_subkeys.push(donor.secret_subkeys[0].clone());
        let forged = host
            .to_armored_string(ArmorOptions::default())
            .expect("forged certificate re-armors");

        let donor_subkey_id = format!("{}", donor.secret_subkeys[0].key.legacy_key_id());
        let material = prepare_imported_key(&forged, "")
            .expect("the certificate's own primary key stays importable");
        assert_ne!(
            material.signing_key_id, donor_subkey_id,
            "the foreign-bound subkey must never be selected as the signing key"
        );
    }

    /// Guard against over-strictness: certificates whose bindings *are* issued
    /// by their own primary must keep importing, whatever their key flags.
    #[test]
    fn legitimate_subkey_bindings_still_pass() {
        for (name, passphrase) in [
            ("secret-primary-only.asc", ""),
            ("secret-two-signing-subkeys.asc", ""),
            ("secret-encrypt-only-subkey.asc", ""),
            ("protected-secret.asc", "libra-test-fixture-passphrase"),
        ] {
            let material = prepare_imported_key(&fixture(name), passphrase)
                .unwrap_or_else(|e| panic!("{name} must stay importable: {e}"));
            assert!(!material.signing_key_id.is_empty(), "{name}: no key chosen");
        }
    }
}

#[cfg(test)]
mod gpg_retry_exhaustion_tests {
    use serial_test::serial;

    use super::*;
    use crate::internal::config::ConfigKv;

    /// plan-20260921 ADR-VG-06 §6 / VG-14 (`generate_pgp_key_retry_exhaustion_fails_closed`):
    /// when every name probe collides the generator must fail closed instead of
    /// overwriting the active key.
    #[tokio::test]
    #[serial(env)]
    #[serial(cwd)]
    async fn generate_pgp_key_retry_exhaustion_fails_closed() {
        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let root = crate::utils::util::storage_path();
        let unseal = lazy_init_vault_for_scope("local")
            .await
            .expect("local vault");

        // Seed one key so a colliding name really exists in the vault.
        let (_pubkey, seeded_name) =
            generate_pgp_key(&root, &unseal, "Seed", "seed@example.invalid")
                .await
                .expect("the first generation must succeed");
        let active_before = ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);

        set_key_name_override(vec![seeded_name; 8]);
        let outcome = generate_pgp_key(&root, &unseal, "Seed", "seed@example.invalid").await;
        clear_key_name_override();

        let err = outcome.expect_err("exhausted retries must fail closed");
        // Exhaustion surfaces the last collision error (the "after 8 collision
        // retries" fallback is unreachable once a collision was seen at all).
        assert!(
            err.to_string().to_lowercase().contains("exist"),
            "unexpected error: {err}"
        );
        // All eight probes were consumed: the loop really retried.
        assert_eq!(
            key_name_override_remaining(),
            0,
            "the generator must retry through every injected name"
        );
        let active_after = ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        assert_eq!(
            active_after, active_before,
            "the active key must be unchanged after exhaustion"
        );
    }

    /// plan-20260921 (`config_generate_gpg_key_after_import_same_nanosecond_collision_retries`):
    /// after an import, a generation whose first name probe collides must retry
    /// with a fresh name instead of failing or overwriting.
    ///
    /// Imported keys live in `vault.gpg.seckey_enc` (ADR-VG-12) rather than
    /// under a versioned vault name, so the collision is injected at the same
    /// seam the nanosecond-derived probe uses — the state a same-nanosecond
    /// collision would produce.
    #[tokio::test]
    #[serial(env)]
    #[serial(cwd)]
    async fn config_generate_gpg_key_after_import_same_nanosecond_collision_retries() {
        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let root = crate::utils::util::storage_path();
        let unseal = lazy_init_vault_for_scope("local")
            .await
            .expect("local vault");

        let (_pubkey, colliding_name) =
            generate_pgp_key(&root, &unseal, "Seed", "seed@example.invalid")
                .await
                .expect("seed generation");

        let armor = std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/protected-secret.asc",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture armor");
        let material = prepare_imported_key(&armor, "libra-test-fixture-passphrase")
            .expect("fixture key must unlock");
        persist_imported_gpg_key(&unseal, &material)
            .await
            .expect("persist imported key");
        crate::internal::config::ConfigKv::set("vault.gpg.source", "imported", false)
            .await
            .expect("record imported source");

        set_key_name_override(vec![colliding_name.clone()]);
        let outcome = generate_pgp_key(&root, &unseal, "Seed", "seed@example.invalid").await;
        clear_key_name_override();

        let (_pubkey, new_name) = outcome.expect("a colliding probe must be retried, not fatal");
        assert_ne!(new_name, colliding_name, "the retry must mint a fresh name");
        assert_eq!(key_name_override_remaining(), 0);
    }
}

#[cfg(test)]
mod gpg_issuer_gate_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 (`issuer_selects_trusted_subkey`): the issuer named by the
    /// signature decides which key proves it — a trusted key verifies, and the
    /// same signature must not verify against an unrelated certificate.
    #[test]
    fn issuer_selects_trusted_subkey() {
        let secret = fixture("secret-two-signing-subkeys.asc");
        let material = prepare_imported_key(&secret, "").expect("fixture imports");
        let data = b"issuer gate payload";
        let sig_hex = sign_with_armored_secret_key(&secret, &material.signing_key_id, data)
            .expect("detached signature");

        assert!(
            verify_signature_hex(&sig_hex, data, std::slice::from_ref(&material.pubkey_armor)),
            "the trusted certificate must verify its own subkey signature"
        );
        assert!(
            !verify_signature_hex(&sig_hex, data, &[fixture("pubkey.asc")]),
            "an unrelated certificate must not verify the signature"
        );
    }

    /// plan-20260921 (`issuer_selected_subkey_requires_eligibility`): carrying
    /// an issuer is not enough — the issuer must be one of the keys offered for
    /// verification, so the signature made by key B fails against key A alone.
    #[test]
    fn issuer_selected_subkey_requires_eligibility() {
        let donor = fixture("secret-two-signing-subkeys.asc");
        let donor_material = prepare_imported_key(&donor, "").expect("donor imports");
        let data = b"eligibility payload";
        let sig_hex = sign_with_armored_secret_key(&donor, &donor_material.signing_key_id, data)
            .expect("detached signature");

        let host = prepare_imported_key(
            &fixture("protected-secret.asc"),
            "libra-test-fixture-passphrase",
        )
        .expect("host imports");
        assert!(
            !verify_signature_hex(&sig_hex, data, std::slice::from_ref(&host.pubkey_armor)),
            "an issuer that is not in the candidate list must not be accepted"
        );
    }

    /// plan-20260921 (`issuer_absent_fallback_is_bounded`): the issuer-absent
    /// fallback accepts lists up to the cap and the normal path is unaffected.
    #[test]
    fn issuer_absent_fallback_is_bounded() {
        assert!(issuer_absent_fallback_allowed(1));
        assert!(issuer_absent_fallback_allowed(ISSUER_ABSENT_FALLBACK_CAP));

        let secret = fixture("secret-primary-only.asc");
        let material = prepare_imported_key(&secret, "").expect("fixture imports");
        let data = b"bounded payload";
        let sig_hex = sign_with_armored_secret_key(&secret, &material.signing_key_id, data)
            .expect("detached signature");
        assert!(
            verify_signature_hex(&sig_hex, data, std::slice::from_ref(&material.pubkey_armor)),
            "an issuer-carrying signature still verifies within the bound"
        );
    }

    /// plan-20260921 (`issuer_absent_cap_exceeded_fails_closed`): one candidate
    /// beyond the cap is refused instead of scanned.
    #[test]
    fn issuer_absent_cap_exceeded_fails_closed() {
        assert!(
            !issuer_absent_fallback_allowed(ISSUER_ABSENT_FALLBACK_CAP + 1),
            "a list longer than the cap must be refused, not scanned"
        );
        let oversized = vec!["not-a-key".to_string(); ISSUER_ABSENT_FALLBACK_CAP + 1];
        assert!(
            !verify_signature_hex("00", b"data", &oversized),
            "an oversized candidate list must fail closed"
        );

        // The counter refuses exactly at the boundary, so the (cap + 1)-th
        // candidate is never attempted.
        let mut tried = 0usize;
        for _ in 0..ISSUER_ABSENT_FALLBACK_CAP {
            assert!(issuer_absent_candidate_allowed(&mut tried));
        }
        assert_eq!(tried, ISSUER_ABSENT_FALLBACK_CAP);
        assert!(
            !issuer_absent_candidate_allowed(&mut tried),
            "candidates beyond the cap must be refused"
        );
        assert_eq!(
            tried, ISSUER_ABSENT_FALLBACK_CAP,
            "a refused attempt must not advance the counter"
        );
    }

    /// plan-20260921 VG-13 G5 (`replace_never_deletes_history_rows`): replacing
    /// the active key records history but never removes existing history rows.
    #[tokio::test]
    #[serial_test::serial(env)]
    #[serial_test::serial(cwd)]
    async fn replace_never_deletes_history_rows() {
        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let unseal = lazy_init_vault_for_scope("local")
            .await
            .expect("local vault");

        let first = prepare_imported_key(
            &fixture("protected-secret.asc"),
            "libra-test-fixture-passphrase",
        )
        .expect("first key imports");
        persist_imported_gpg_key(&unseal, &first)
            .await
            .expect("persist first");
        let history_after_first: Vec<String> = crate::internal::config::ConfigKv::list_all()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.key)
            .filter(|k| k.starts_with("vault.gpg.history."))
            .collect();

        let second = prepare_imported_key(&fixture("secret-two-signing-subkeys.asc"), "")
            .expect("second key imports");
        persist_imported_gpg_key(&unseal, &second)
            .await
            .expect("persist second");
        let history_after_second: Vec<String> = crate::internal::config::ConfigKv::list_all()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.key)
            .filter(|k| k.starts_with("vault.gpg.history."))
            .collect();

        for key in &history_after_first {
            assert!(
                history_after_second.contains(key),
                "replacing the active key must not delete {key}"
            );
        }
        assert!(
            history_after_second.len() >= history_after_first.len(),
            "history rows must never shrink on replacement"
        );
    }
}

#[cfg(test)]
mod gpg_allowlist_gate_tests {
    use std::time::Duration;

    use super::*;
    use crate::internal::config::ConfigKv;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 VG-05 G1 (`imported_key_verifies_own_signature`): a
    /// certificate that went through the import pipeline must verify the
    /// signature its signing subkey produced.
    #[test]
    fn imported_key_verifies_own_signature() {
        let secret = fixture("secret-two-signing-subkeys.asc");
        let material = prepare_imported_key(&secret, "").expect("fixture imports");
        let data = b"imported-key verification payload";
        let sig_hex = sign_with_armored_secret_key(&secret, &material.signing_key_id, data)
            .expect("detached signature");

        assert!(
            verify_signature_hex(&sig_hex, data, std::slice::from_ref(&material.pubkey_armor)),
            "an imported key must verify its own signature"
        );
    }

    /// plan-20260921 VG-05 G2 (`verification_ignores_source_with_internal_fixture`):
    /// the allowlist is built from `vault.gpg.pubkey` → `generated_pubkey` →
    /// `history.*` only. An internal state fixture whose `vault.gpg.source`
    /// metadata does not match the installed certificate must not change the
    /// verdict, while the allowlist content still decides the outcome.
    #[tokio::test]
    #[serial_test::serial(env, cwd)]
    async fn verification_ignores_source_with_internal_fixture() {
        use crate::internal::config::ConfigKv;

        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let secret = fixture("secret-two-signing-subkeys.asc");
        let material = prepare_imported_key(&secret, "").expect("unprotected fixture imports");
        ConfigKv::set("vault.gpg.pubkey", &material.pubkey_armor, false)
            .await
            .expect("install allowlist key");
        // Internal state fixture: the recorded source contradicts the key that
        // is actually installed, so reading it would flip the verdict.
        ConfigKv::set("vault.gpg.source", "generated", false)
            .await
            .expect("record mismatching source");

        let data = b"source independence payload";
        let sig_hex = sign_with_armored_secret_key(&secret, &material.signing_key_id, data)
            .expect("detached signature");
        let verified = pgp_verify(std::path::Path::new("."), &[], data, &sig_hex)
            .await
            .expect("allowlist verification");
        assert!(verified, "verification must not depend on vault.gpg.source");

        // Control: the allowlist content still decides — a foreign certificate
        // cannot verify the same signature.
        let foreign = fixture("pubkey.asc");
        ConfigKv::set("vault.gpg.pubkey", &foreign, false)
            .await
            .expect("swap allowlist key");
        let rejected = pgp_verify(std::path::Path::new("."), &[], data, &sig_hex)
            .await
            .expect("allowlist verification");
        assert!(
            !rejected,
            "a certificate that is not in the allowlist must not verify"
        );
    }

    /// plan-20260921 VG-05 G3 (`history_public_keys_verify_replaced_key_signatures`):
    /// once a key is replaced it moves into `vault.gpg.history.*`, and the
    /// signatures it produced must still verify through that archive.
    #[tokio::test]
    #[serial_test::serial(env, cwd)]
    async fn history_public_keys_verify_replaced_key_signatures() {
        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        // The replaced (older) certificate, with a signature it made.
        let old_secret = fixture("secret-two-signing-subkeys.asc");
        let old = prepare_imported_key(&old_secret, "").expect("old fixture imports");
        let data = b"replaced-key payload";
        let sig_hex = sign_with_armored_secret_key(&old_secret, &old.signing_key_id, data)
            .expect("old key signs");

        // Replacement: the new key takes the active slot, the old one is archived.
        let new = prepare_imported_key(
            &fixture("protected-secret.asc"),
            "libra-test-fixture-passphrase",
        )
        .expect("new fixture imports");
        ConfigKv::set("vault.gpg.pubkey", &new.pubkey_armor, false)
            .await
            .expect("install the replacement key");
        ConfigKv::set(
            &format!("vault.gpg.history.{}.pubkey", old.fingerprint),
            &old.pubkey_armor,
            false,
        )
        .await
        .expect("archive the replaced key");

        let allowlist = verify_pubkey_allowlist().await.expect("allowlist");
        assert!(
            allowlist.iter().any(|a| a == &old.pubkey_armor),
            "the replaced key must be in the allowlist"
        );
        assert!(
            verify_signature_hex(&sig_hex, data, &allowlist),
            "a signature made before the replacement must still verify"
        );
        // Control: without the archive the same signature is rejected.
        assert!(
            !verify_signature_hex(&sig_hex, data, std::slice::from_ref(&new.pubkey_armor)),
            "the replacement key alone must not verify the replaced key's signature"
        );
    }

    /// plan-20260921 VG-05 G5 (`unknown_key_signature_is_rejected`): a signature
    /// from a key the repository never listed is rejected, never accepted.
    #[test]
    fn unknown_key_signature_is_rejected() {
        let donor_secret = fixture("secret-two-signing-subkeys.asc");
        let donor = prepare_imported_key(&donor_secret, "").expect("donor fixture imports");
        let data = b"unknown-key payload";
        let sig_hex = sign_with_armored_secret_key(&donor_secret, &donor.signing_key_id, data)
            .expect("donor signs");

        // An allowlist that only holds an unrelated certificate rejects it.
        let unrelated = fixture("pubkey.asc");
        assert!(
            !verify_signature_hex(&sig_hex, data, std::slice::from_ref(&unrelated)),
            "a key outside the allowlist must not verify"
        );
        // And an empty allowlist rejects everything.
        assert!(
            !verify_signature_hex(&sig_hex, data, &[]),
            "an empty allowlist must reject every signature"
        );
    }

    /// plan-20260921 VG-05 G8 (`malformed_history_key_is_skipped_not_fatal`):
    /// history entries that do not parse must be skipped, never fatal — a
    /// malformed row cannot take verification down with it.
    #[test]
    fn malformed_history_key_is_skipped_not_fatal() {
        let secret = fixture("secret-two-signing-subkeys.asc");
        let material = prepare_imported_key(&secret, "").expect("fixture imports");
        let data = b"malformed history payload";
        let sig_hex = sign_with_armored_secret_key(&secret, &material.signing_key_id, data)
            .expect("detached signature");

        // Malformed rows interleaved with the certificate that really signed.
        let mut allowlist = vec![
            "not-a-pgp-block".to_string(),
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\ntruncated".to_string(),
            String::new(),
        ];
        assert!(
            !verify_signature_hex(&sig_hex, data, &allowlist),
            "a list of only malformed rows is simply not verified"
        );
        allowlist.push(material.pubkey_armor.clone());
        assert!(
            verify_signature_hex(&sig_hex, data, &allowlist),
            "malformed history rows must be skipped so the valid certificate still verifies"
        );
    }

    /// plan-20260921 VG-05 G15 (`verification_cost_is_linear_with_history`):
    /// every candidate is attempted once, so the work grows with the length of
    /// the allowlist instead of its square. The bound is deliberately loose so
    /// the gate is not a timing flake: only a quadratic blow-up (≈4096× for a
    /// 64× longer list) can trip it.
    #[test]
    fn verification_cost_is_linear_with_history() {
        let secret = fixture("secret-two-signing-subkeys.asc");
        let material = prepare_imported_key(&secret, "").expect("fixture imports");
        let data = b"linearity payload";
        let sig_hex = sign_with_armored_secret_key(&secret, &material.signing_key_id, data)
            .expect("detached signature");
        let unrelated = fixture("pubkey.asc");

        let elapsed = |entries: usize| {
            let allowlist = vec![unrelated.clone(); entries];
            let start = std::time::Instant::now();
            let verified = verify_signature_hex(&sig_hex, data, &allowlist);
            (verified, start.elapsed())
        };

        let _ = elapsed(1); // warm-up
        let (verified_one, one) = elapsed(1);
        let (verified_many, many) = elapsed(64);
        assert!(
            !verified_one && !verified_many,
            "unrelated keys must not verify"
        );
        let one = one.max(Duration::from_micros(200));
        assert!(
            many <= one * 300,
            "64 candidates took {many:?} against {one:?} for one — verification must stay linear"
        );
    }
}

#[cfg(test)]
mod gpg_remove_gate_tests {
    use serial_test::serial;

    use super::*;
    use crate::internal::config::ConfigKv;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// Sandboxed repository whose active key is the shared fixture import, with
    /// a generated key behind it so the removal's "restore the generated public
    /// half" step is meaningful.
    struct Sandbox {
        _env: crate::utils::test::ConfigDbFixture,
        repo: tempfile::TempDir,
        _cwd: crate::utils::test::ChangeDirGuard,
        unseal: Vec<u8>,
        fingerprint: String,
    }

    async fn imported_key_repo() -> Sandbox {
        let env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let root = crate::utils::util::storage_path();
        let unseal = lazy_init_vault_for_scope("local")
            .await
            .expect("local vault");
        let (_pubkey, _name) = generate_pgp_key(&root, &unseal, "Seed", "seed@example.invalid")
            .await
            .expect("the generated key must exist for the fallback step");

        let material = prepare_imported_key(
            &fixture("protected-secret.asc"),
            "libra-test-fixture-passphrase",
        )
        .expect("fixture unlocks");
        let fingerprint = material.fingerprint.clone();
        persist_imported_gpg_key(&unseal, &material)
            .await
            .expect("persist the imported key");
        ConfigKv::set("vault.gpg.source", "imported", false)
            .await
            .expect("record imported source");

        Sandbox {
            _env: env,
            repo,
            _cwd: cwd,
            unseal,
            fingerprint,
        }
    }

    /// The four removal steps' invariant: after a failed removal the imported
    /// key must still be the active, usable one.
    async fn assert_imported_state_intact(web: &Sandbox, pubkey_before: Option<String>) {
        let value = |key: &'static str| async move {
            ConfigKv::get(key)
                .await
                .ok()
                .flatten()
                .map(|entry| entry.value)
        };
        assert_eq!(
            value("vault.gpg.source").await.as_deref(),
            Some("imported"),
            "a failed removal must not flip the source"
        );
        assert_eq!(
            value("vault.gpg.fingerprint").await.as_deref(),
            Some(web.fingerprint.as_str()),
            "a failed removal must keep the imported fingerprint"
        );
        assert!(
            value("vault.gpg.seckey_enc").await.is_some(),
            "a failed removal must keep the imported secret key"
        );
        assert_eq!(
            value("vault.gpg.pubkey").await,
            pubkey_before,
            "a failed removal must keep the active public key"
        );
        assert!(
            value("vault.gpg.generated_pubkey").await.is_some(),
            "a failed removal must keep the generated fallback snapshot"
        );
        let _ = &web.unseal;
        let _ = &web.repo;
    }

    /// plan-20260921 故障恢复矩陣「`remove --force` 误删风险」的**移除路径**：移除活动
    /// 导入键**不得**触及既有的 `vault.gpg.history.*` 归档行（替换路径已由
    /// `replace_never_deletes_history_rows` 覆盖，矩阵该行原先只对 `generated_pubkey`
    /// 有断言）。
    #[tokio::test]
    #[serial(env, cwd)]
    async fn removal_preserves_existing_history_rows() {
        let web = imported_key_repo().await;
        let history_rows = || async {
            ConfigKv::list_all()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|entry| entry.key)
                .filter(|key| key.starts_with("vault.gpg.history."))
                .collect::<Vec<String>>()
        };
        let before = history_rows().await;
        assert!(
            !before.is_empty(),
            "the fixture must already archive a replaced key, otherwise this check is vacuous"
        );

        remove_imported_gpg_key().await.expect("removal succeeds");

        let after = history_rows().await;
        for row in &before {
            assert!(
                after.contains(row),
                "removal must not delete an existing history row ({row}); before={before:?} after={after:?}"
            );
        }
        // The removed key's own public half is archived so signatures it already
        // made keep verifying: exactly one new row, naming the removed key.
        let archived_removal = format!(
            "vault.gpg.history.{}.pubkey",
            web.fingerprint.to_uppercase()
        );
        assert!(
            after.contains(&archived_removal),
            "removal must archive the removed key's public half ({archived_removal}); after={after:?}"
        );
        assert_eq!(
            after.len(),
            before.len() + 1,
            "removal must add exactly the removed key's archived row; before={before:?} after={after:?}"
        );
        let _ = &web.unseal;
    }

    /// plan-20260921 VG-08 G9 (`config_remove_gpg_key_inject_failure_history`):
    /// a failure while archiving the active key must leave the import untouched.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_remove_gpg_key_inject_failure_history() {
        let web = imported_key_repo().await;
        let pubkey_before = ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        set_remove_fail_at(1);
        let outcome = remove_imported_gpg_key().await;
        clear_remove_fail_at();
        let err = outcome.expect_err("an injected step-1 failure must abort the removal");
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_imported_state_intact(&web, pubkey_before).await;
    }

    /// plan-20260921 VG-08 G10
    /// (`config_remove_gpg_key_inject_failure_delete_imported`): a failure while
    /// dropping the imported metadata must roll the whole removal back.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_remove_gpg_key_inject_failure_delete_imported() {
        let web = imported_key_repo().await;
        let pubkey_before = ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        set_remove_fail_at(2);
        let outcome = remove_imported_gpg_key().await;
        clear_remove_fail_at();
        let err = outcome.expect_err("an injected step-2 failure must abort the removal");
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_imported_state_intact(&web, pubkey_before).await;
    }

    /// plan-20260921 VG-08 G11
    /// (`config_remove_gpg_key_inject_failure_restore_pubkey`): a failure while
    /// restoring the generated public half must roll back the deleted metadata.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_remove_gpg_key_inject_failure_restore_pubkey() {
        let web = imported_key_repo().await;
        let pubkey_before = ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        set_remove_fail_at(3);
        let outcome = remove_imported_gpg_key().await;
        clear_remove_fail_at();
        let err = outcome.expect_err("an injected step-3 failure must abort the removal");
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_imported_state_intact(&web, pubkey_before).await;
    }

    /// plan-20260921 VG-08 G12
    /// (`config_remove_gpg_key_inject_failure_source`): a failure while writing
    /// the `source` marker must roll back everything written before it.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_remove_gpg_key_inject_failure_source() {
        let web = imported_key_repo().await;
        let pubkey_before = ConfigKv::get("vault.gpg.pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        set_remove_fail_at(4);
        let outcome = remove_imported_gpg_key().await;
        clear_remove_fail_at();
        let err = outcome.expect_err("an injected step-4 failure must abort the removal");
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_imported_state_intact(&web, pubkey_before).await;
    }
}

#[cfg(test)]
mod gpg_generated_name_gate_tests {
    use super::*;
    use crate::internal::config::ConfigKv;

    /// plan-20260921 VG-04 (`generated_path_legacy_fallback_when_key_name_absent`):
    /// repositories created before VG-14 carry no versioned `generated_key_name`,
    /// so the reader must fall back to the legacy constant instead of failing.
    #[tokio::test]
    #[serial_test::serial(env, cwd)]
    async fn generated_path_legacy_fallback_when_key_name_absent() {
        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        // A recorded versioned name wins.
        ConfigKv::set("vault.gpg.generated_key_name", "libra-signing-42", false)
            .await
            .expect("seed the versioned name");
        assert_eq!(generated_key_name().await, "libra-signing-42");

        // Legacy repository: the row is gone, so the pre-VG-14 constant is used.
        ConfigKv::unset("vault.gpg.generated_key_name")
            .await
            .expect("drop the versioned name");
        assert_eq!(generated_key_name().await, PGP_KEY_NAME);
        assert_eq!(generated_key_name().await, "libra-signing");
    }
}

#[cfg(test)]
mod gpg_history_order_gate_tests {
    use super::*;
    use crate::internal::config::ConfigKv;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 VG-13/VG-05 (`历史顺序确定` + `历史顺序/去重幂等`): the
    /// verification allowlist is active → generated → history sorted by key,
    /// and deduplicated by value (ADR-VG-03 §2).
    #[tokio::test]
    #[serial_test::serial(env, cwd)]
    async fn history_public_keys_are_sorted_and_deduplicated() {
        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let active = fixture("pubkey.asc");
        let second = prepare_imported_key(&fixture("secret-two-signing-subkeys.asc"), "")
            .expect("donor fixture imports")
            .pubkey_armor;

        ConfigKv::set("vault.gpg.pubkey", &active, false)
            .await
            .expect("install the active key");
        // Deliberately seeded out of order, with the "ZZZ" row duplicating the
        // active certificate so deduplication is observable too.
        ConfigKv::set(
            "vault.gpg.history.ZZZ0000000000000000000000000000000000000.pubkey",
            &active,
            false,
        )
        .await
        .expect("seed duplicate history row");
        ConfigKv::set(
            "vault.gpg.history.AAA0000000000000000000000000000000000000.pubkey",
            &second,
            false,
        )
        .await
        .expect("seed second history row");

        let allowlist = verify_pubkey_allowlist().await.expect("allowlist");
        assert_eq!(
            allowlist.len(),
            2,
            "the duplicate of the active certificate must be deduplicated: {allowlist:?}"
        );
        assert_eq!(allowlist[0], active, "the active key comes first");
        assert_eq!(
            allowlist[1], second,
            "history rows follow in fingerprint lexicographic order"
        );
    }

    /// plan-20260921 VG-04/VG-13 (`source=generated` 且无 `generated_pubkey` 时先写入快照):
    /// generating over a generated key with no snapshot records the active
    /// certificate before anything else can overwrite it.
    #[tokio::test]
    #[serial_test::serial(env, cwd)]
    async fn generated_snapshot_is_written_before_overwrite() {
        use sea_orm::TransactionTrait;

        let _env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let _cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let active = fixture("pubkey.asc");
        ConfigKv::set("vault.gpg.pubkey", &active, false)
            .await
            .expect("install the active key");
        ConfigKv::set("vault.gpg.source", "generated", false)
            .await
            .expect("record generated source");
        ConfigKv::unset("vault.gpg.generated_pubkey")
            .await
            .expect("drop any snapshot");

        let db = crate::internal::db::get_db_conn_instance().await;
        let txn = db.begin().await.expect("transaction");
        snapshot_active_key_to_history_with_conn(&txn)
            .await
            .expect("snapshot the active key");
        txn.commit().await.expect("commit");

        let snapshot = ConfigKv::get("vault.gpg.generated_pubkey")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        assert_eq!(
            snapshot.as_deref(),
            Some(active.as_str()),
            "the missing generated snapshot must be written from the active key"
        );
        let fp = key_fingerprint_from_armor(&active).expect("fingerprint");
        let history = ConfigKv::get(&format!("vault.gpg.history.{fp}.pubkey"))
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        assert_eq!(
            history.as_deref(),
            Some(active.as_str()),
            "the active key must also be archived in history"
        );
    }
}

#[cfg(test)]
mod gpg_generate_failpoint_tests {
    use serial_test::serial;

    use super::*;
    use crate::internal::config::ConfigKv;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    struct Sandbox {
        _env: crate::utils::test::ConfigDbFixture,
        _repo: tempfile::TempDir,
        _cwd: crate::utils::test::ChangeDirGuard,
        root: std::path::PathBuf,
        unseal: Vec<u8>,
        imported_fingerprint: String,
    }

    /// Sandboxed repository whose active key is the fixture *import* — the
    /// scenario the declared gates name (`..._after_import_...`).
    async fn imported_key_repo() -> Sandbox {
        let env = crate::utils::test::ConfigDbFixture::new().expect("env sandbox");
        let repo = tempfile::tempdir().expect("temp repo");
        let cwd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let root = crate::utils::util::storage_path();
        let unseal = lazy_init_vault_for_scope("local")
            .await
            .expect("local vault");
        let material = prepare_imported_key(
            &fixture("protected-secret.asc"),
            "libra-test-fixture-passphrase",
        )
        .expect("fixture unlocks");
        let imported_fingerprint = material.fingerprint.clone();
        persist_imported_gpg_key(&unseal, &material)
            .await
            .expect("persist the imported key");
        ConfigKv::set("vault.gpg.source", "imported", false)
            .await
            .expect("record imported source");

        Sandbox {
            _env: env,
            _repo: repo,
            _cwd: cwd,
            root,
            unseal,
            imported_fingerprint,
        }
    }

    async fn cfg(key: &str) -> Option<String> {
        ConfigKv::get(key).await.ok().flatten().map(|e| e.value)
    }

    /// The imported key must still be the active, usable one.
    async fn assert_import_still_active(web: &Sandbox) {
        assert_eq!(
            cfg("vault.gpg.source").await.as_deref(),
            Some("imported"),
            "a failed migration must leave the imported key active"
        );
        assert_eq!(
            cfg("vault.gpg.fingerprint").await.as_deref(),
            Some(web.imported_fingerprint.as_str()),
            "the imported fingerprint must survive a failed migration"
        );
        assert!(
            cfg("vault.gpg.seckey_enc").await.is_some(),
            "the imported secret key must survive a failed migration"
        );
    }

    async fn run_with_failpoint(web: &Sandbox, step: u8) -> anyhow::Error {
        set_generate_fail_at(step);
        let outcome =
            generate_pgp_key(&web.root, &web.unseal, "Seed", "seed@example.invalid").await;
        clear_generate_fail_at();
        outcome.expect_err("the injected failure must fail the migration")
    }

    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_generate_gpg_key_after_import_inject_failure_after_a() {
        let web = imported_key_repo().await;
        let err = run_with_failpoint(&web, 1).await;
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_import_still_active(&web).await;
    }

    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_generate_gpg_key_after_import_inject_failure_after_b() {
        let web = imported_key_repo().await;
        let err = run_with_failpoint(&web, 2).await;
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_import_still_active(&web).await;
        assert!(
            cfg("vault.gpg.generated_pubkey").await.is_none(),
            "step (c) must not have run"
        );
    }

    /// VG-14 G3: a (c)-only residue has no key name, so it is not resumable —
    /// the next mint overwrites the stale snapshot; the replaced import is
    /// archived, so no signature is lost.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_generate_gpg_key_after_import_inject_failure_after_c() {
        let web = imported_key_repo().await;
        let err = run_with_failpoint(&web, 3).await;
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_import_still_active(&web).await;
        assert!(
            cfg("vault.gpg.generated_key_name").await.is_none(),
            "step (d1) must not have run right after (c)"
        );
        assert!(
            cfg("vault.gpg.generated_pubkey").await.is_some(),
            "the (c) write is the residue this gate pins"
        );

        let (fresh, _name) =
            generate_pgp_key(&web.root, &web.unseal, "Seed", "seed@example.invalid")
                .await
                .expect("a re-run must succeed");
        assert!(!fresh.is_empty(), "the re-run must mint a usable key");
        assert_eq!(cfg("vault.gpg.source").await.as_deref(), Some("generated"));
        assert!(
            cfg(&format!(
                "vault.gpg.history.{}.pubkey",
                web.imported_fingerprint
            ))
            .await
            .is_some(),
            "the replaced imported key must stay verifiable through history"
        );
    }

    /// VG-14 G4: a failure after `generated_key_name` leaves the import active;
    /// the re-run completes the recorded pair instead of minting a second key.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_generate_gpg_key_after_import_inject_failure_after_d1() {
        let web = imported_key_repo().await;
        let err = run_with_failpoint(&web, 4).await;
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        assert_import_still_active(&web).await;
        // A (d1) residue records a name+snapshot but the active slot still
        // holds the import, so it is not resumable: the next generation mints
        // (and overwrites the stale pair) instead of switching to it.
        let stale = cfg("vault.gpg.generated_pubkey")
            .await
            .expect("the minted key is recorded");
        let stale_name = cfg("vault.gpg.generated_key_name")
            .await
            .expect("the minted name is recorded");

        let (fresh, fresh_name) =
            generate_pgp_key(&web.root, &web.unseal, "Seed", "seed@example.invalid")
                .await
                .expect("a re-run must succeed");
        assert!(!fresh.is_empty());
        assert_ne!(
            fresh_name, stale_name,
            "a (d1) residue must be replaced by a fresh mint, not resumed"
        );
        assert_ne!(fresh, stale, "the stale snapshot must not be switched in");
        assert_eq!(cfg("vault.gpg.source").await.as_deref(), Some("generated"));
        assert!(
            cfg(&format!(
                "vault.gpg.history.{}.pubkey",
                web.imported_fingerprint
            ))
            .await
            .is_some(),
            "the replaced imported key must stay verifiable through history"
        );
    }
    /// VG-14 G5: a failure after `vault.gpg.pubkey` leaves the `source` switch
    /// unrun (the resumable pair); the re-run converges on the same key.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_generate_gpg_key_after_import_inject_failure_after_d2() {
        let web = imported_key_repo().await;
        let err = run_with_failpoint(&web, 5).await;
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );
        let active = cfg("vault.gpg.pubkey").await.expect("active key");
        assert_eq!(
            cfg("vault.gpg.generated_pubkey").await.as_deref(),
            Some(active.as_str()),
            "the minted key already holds the active slot"
        );
        assert_eq!(
            cfg("vault.gpg.source").await.as_deref(),
            Some("imported"),
            "the source switch is the step that did not run"
        );

        let (again, _name) =
            generate_pgp_key(&web.root, &web.unseal, "Seed", "seed@example.invalid")
                .await
                .expect("a re-run must converge");
        assert_eq!(again, active, "the re-run must not mint a second key");
        assert_eq!(cfg("vault.gpg.source").await.as_deref(), Some("generated"));
    }

    /// VG-14 G6: (e) had already applied, so the residue is a fully migrated
    /// state — active key named and switched, replaced key archived.
    #[tokio::test]
    #[serial(env, cwd)]
    async fn config_generate_gpg_key_after_import_inject_failure_after_e() {
        let web = imported_key_repo().await;
        let err = run_with_failpoint(&web, 6).await;
        assert!(
            err.to_string().contains("injected"),
            "unexpected error: {err}"
        );

        let active = cfg("vault.gpg.pubkey").await.expect("active public key");
        assert_eq!(
            cfg("vault.gpg.generated_pubkey").await.as_deref(),
            Some(active.as_str()),
            "the minted key is the active one"
        );
        assert!(
            cfg("vault.gpg.generated_key_name").await.is_some(),
            "the minted key is recorded by name"
        );
        assert_eq!(cfg("vault.gpg.source").await.as_deref(), Some("generated"));
        assert!(
            cfg(&format!(
                "vault.gpg.history.{}.pubkey",
                web.imported_fingerprint
            ))
            .await
            .is_some(),
            "the imported key must be archived so its signatures stay verifiable"
        );
    }
}

#[cfg(test)]
mod gpg_verification_time_gate_tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/data/fake-gpg/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    /// plan-20260921 VG-05 G13 (`revocation_is_evaluated_at_signature_creation_time`):
    /// revocation is judged at the signature's own creation time — a signature
    /// made *after* the revocation is refused, one made before stays acceptable.
    #[test]
    fn revocation_is_evaluated_at_signature_creation_time() {
        // Pure predicate: both time orders.
        assert!(
            revocation_applies(Some(100), 200),
            "revoked before signing must be refused"
        );
        assert!(
            !revocation_applies(Some(300), 200),
            "revoked after signing must stay acceptable"
        );
        assert!(
            !revocation_applies(None, 200),
            "never revoked stays acceptable"
        );

        // A real revoked certificate carries a revocation time; a signature made
        // now is after it, while one made the second before is not.
        use pgp::composed::Deserializable;
        let (revoked, _) = pgp::composed::SignedPublicKey::from_armor_single(
            fixture("revoked-key.asc").as_bytes(),
        )
        .expect("the revoked fixture parses");
        let revoked_at = key_revoked_at(&revoked).expect("the fixture's revocation is detected");
        assert!(
            revocation_applies(Some(revoked_at), now_epoch_secs()),
            "a signature made now is after the revocation"
        );
        assert!(
            !revocation_applies(Some(revoked_at), revoked_at.saturating_sub(1)),
            "a signature made the second before revocation stays acceptable"
        );
    }

    /// plan-20260921 VG-05 G14 (`expiration_is_evaluated_at_signature_creation_time`):
    /// expiry is judged at the signature's own creation time.
    #[test]
    fn expiration_is_evaluated_at_signature_creation_time() {
        assert!(
            expiry_applies(Some(100), 200),
            "expired before signing must be refused"
        );
        assert!(
            !expiry_applies(Some(300), 200),
            "expiring after signing must stay acceptable"
        );
        assert!(
            !expiry_applies(None, 200),
            "no declared expiry stays acceptable"
        );

        use pgp::composed::Deserializable;
        let (key, _) = pgp::composed::SignedPublicKey::from_armor_single(
            fixture("expired-subkey.asc").as_bytes(),
        )
        .expect("the expired fixture parses");
        let expiry = key
            .public_subkeys
            .iter()
            .find_map(|sub| subkey_declared_expiry(&sub.signatures))
            .expect("the fixture's expired subkey declares an expiry");
        assert!(
            expiry_applies(Some(expiry), now_epoch_secs()),
            "a signature made now is after the expiry"
        );
        assert!(
            !expiry_applies(Some(expiry), expiry.saturating_sub(1)),
            "a signature made before the expiry stays acceptable"
        );
    }
}
