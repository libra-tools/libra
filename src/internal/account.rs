//! Website account session storage for `libra login`.
//!
//! This is intentionally separate from `internal::auth`: `auth.token.*` stores
//! host-scoped Git HTTP credentials, while `account.host.*` stores website CLI
//! login sessions returned by `/api/cli/exchange`.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    command::config::{ConfigScope, ScopedConfig},
    internal::{config::ConfigKv, vault},
};

const ACCOUNT_SESSION_PREFIX: &str = "account.host.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSession {
    pub host: String,
    pub username: String,
    pub user_id: String,
    pub github_id: String,
    pub session_token: String,
    pub issued_at: String,
    pub expires_at: String,
}

pub fn storage_key(host: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"libra-account-v1\0");
    hasher.update(host.as_bytes());
    format!("{ACCOUNT_SESSION_PREFIX}{}", hex::encode(hasher.finalize()))
}

pub async fn store_session(session: &AccountSession) -> Result<()> {
    let unseal_key = vault::lazy_init_vault_for_scope("global")
        .await
        .map_err(|_| anyhow!("failed to initialize the global vault key"))?;
    let plaintext =
        serde_json::to_vec(session).context("failed to serialize the account session")?;
    let encrypted = vault::encrypt_token(&unseal_key, &plaintext)
        .map_err(|_| anyhow!("failed to encrypt the account session"))?;
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    ConfigKv::set_with_conn(
        &conn,
        &storage_key(&session.host),
        &hex::encode(encrypted),
        false,
    )
    .await
    .map_err(|error| anyhow!("failed to persist the account session: {error}"))?;
    repair_global_modes();
    Ok(())
}

pub async fn load_session(host: &str) -> Result<Option<AccountSession>> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let Some(entry) = ConfigKv::get_with_conn(&conn, &storage_key(host))
        .await
        .map_err(|error| anyhow!("failed to read the account session: {error}"))?
    else {
        return Ok(None);
    };
    decrypt_session(&entry.value).await.map(Some)
}

pub async fn remove_session(host: &str) -> Result<bool> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let removed = ConfigKv::unset_all_with_conn(&conn, &storage_key(host))
        .await
        .map_err(|error| anyhow!("failed to remove the account session: {error}"))?;
    Ok(removed > 0)
}

pub async fn remove_all_sessions() -> Result<usize> {
    let conn = ScopedConfig::get_connection(ConfigScope::Global)
        .await
        .map_err(|error| anyhow!("failed to open the global config store: {error}"))?;
    let entries = ConfigKv::get_by_prefix_with_conn(&conn, ACCOUNT_SESSION_PREFIX)
        .await
        .map_err(|error| anyhow!("failed to list account sessions: {error}"))?;
    let mut removed = 0;
    for entry in entries {
        removed += ConfigKv::unset_all_with_conn(&conn, &entry.key)
            .await
            .map_err(|error| anyhow!("failed to remove account session: {error}"))?;
    }
    Ok(removed)
}

async fn decrypt_session(cipher_hex: &str) -> Result<AccountSession> {
    let cipher = hex::decode(cipher_hex).context("account session is not valid hex")?;
    let unseal_key = vault::lazy_init_vault_for_scope("global").await?;
    let plaintext = vault::decrypt_token(&unseal_key, &cipher)
        .context("failed to decrypt the account session")?;
    serde_json::from_str(&plaintext).context("failed to parse the account session")
}

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

#[cfg(test)]
mod tests {
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{internal::config::ConfigKv, utils::test};

    #[tokio::test]
    #[serial]
    async fn remove_session_deletes_global_session_not_local_key() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = test::ChangeDirGuard::new(repo.path());

        let global = tempdir().unwrap();
        let global_db = global.path().join("global.db");
        let _global_config = test::ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &global_db);

        let host = "http://localhost:7001";
        let key = storage_key(host);
        ScopedConfig::set(ConfigScope::Global, &key, "global-session", false)
            .await
            .unwrap();
        ConfigKv::set(&key, "local-session", false).await.unwrap();

        assert!(remove_session(host).await.unwrap());

        assert!(
            ScopedConfig::get(ConfigScope::Global, &key)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            ConfigKv::get(&key).await.unwrap().map(|entry| entry.value),
            Some("local-session".to_string())
        );
    }

    #[tokio::test]
    #[serial]
    async fn remove_all_sessions_deletes_global_sessions_not_local_keys() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = test::ChangeDirGuard::new(repo.path());

        let global = tempdir().unwrap();
        let global_db = global.path().join("global.db");
        let _global_config = test::ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &global_db);

        let first_key = storage_key("http://localhost:7001");
        let second_key = storage_key("https://libra.tools");
        ScopedConfig::set(ConfigScope::Global, &first_key, "first", false)
            .await
            .unwrap();
        ScopedConfig::set(ConfigScope::Global, &second_key, "second", false)
            .await
            .unwrap();
        ConfigKv::set(&first_key, "local-first", false)
            .await
            .unwrap();

        assert_eq!(remove_all_sessions().await.unwrap(), 2);

        assert!(
            ScopedConfig::get(ConfigScope::Global, &first_key)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            ScopedConfig::get(ConfigScope::Global, &second_key)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            ConfigKv::get(&first_key)
                .await
                .unwrap()
                .map(|entry| entry.value),
            Some("local-first".to_string())
        );
    }
}
