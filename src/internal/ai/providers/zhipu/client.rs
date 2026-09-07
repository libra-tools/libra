//! Zhipu API client for libra.
//!
//! Zhipu AI (also known as GLM / ChatGLM) is a Chinese AI research lab that provides
//! large language models through a cloud API. Authentication is performed via a Bearer
//! token included in the `Authorization` header of each HTTP request. The default base
//! URL points to `https://open.bigmodel.cn/api/paas/v4`.

use std::fmt;

use crate::internal::ai::client::{Client as GenericClient, Provider};

/// Zhipu API provider.
#[derive(Clone)]
pub struct ZhipuProvider {
    api_key: String,
}

impl fmt::Debug for ZhipuProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZhipuProvider")
            .field("api_key", &"***")
            .finish()
    }
}

impl ZhipuProvider {
    /// Creates a new Zhipu provider with the given API key.
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// Returns the API key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

/// Implements the [`Provider`] trait so that every outgoing HTTP request is
/// annotated with a `Bearer <api_key>` authorization header, which is the
/// authentication scheme required by the Zhipu API.
impl Provider for ZhipuProvider {
    fn on_request(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        // Zhipu uses Bearer token authentication
        request = request.header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.api_key),
        );
        request
    }
}

/// Zhipu client type.
pub type Client = GenericClient<ZhipuProvider>;

impl Client {
    /// Creates a Zhipu client from environment variables.
    ///
    /// Resolves `ZHIPU_API_KEY` and optional `ZHIPU_BASE_URL` through the
    /// same vault-aware sync lookup used by the other providers, while keeping
    /// the legacy `std::env::VarError` return type for backward compatibility.
    /// New async call sites should prefer [`Client::from_resolved_env`] so they
    /// can pass an explicit [`crate::internal::config::LocalIdentityTarget`].
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let api_key = resolve_zhipu_env_required("ZHIPU_API_KEY")?;
        let base_url = resolve_zhipu_env_optional("ZHIPU_BASE_URL")?
            .unwrap_or_else(|| "https://open.bigmodel.cn/api/paas/v4".to_string());

        let provider = ZhipuProvider::new(api_key);
        Ok(Self::new(&base_url, provider))
    }

    /// Vault-aware async constructor: resolves `ZHIPU_API_KEY` (required)
    /// and `ZHIPU_BASE_URL` (optional override) through the libra-aware
    /// lookup chain. See deepseek / gemini / openai / anthropic
    /// `from_resolved_env` for the shared contract; `ZHIPU_BASE_URL`
    /// defaults to `https://open.bigmodel.cn/api/paas/v4`.
    pub async fn from_resolved_env(
        local_target: crate::internal::config::LocalIdentityTarget<'_>,
    ) -> anyhow::Result<Self> {
        use anyhow::anyhow;

        use crate::internal::config::resolve_env_for_target;

        let api_key = resolve_env_for_target("ZHIPU_API_KEY", local_target)
            .await?
            .ok_or_else(|| {
                // plan-20260825 PS-01 (ADR-PS-03 a-mode): second user-facing
                // instance of the missing-credential message; the checked
                // chain here is the resolve_env_for_target order (no
                // --env-file at this layer).
                anyhow!(
                    // Line-joined: `\` continuations strip the next line's
                    // whitespace, which would eat the four-space command
                    // indentation the a-mode structure mandates.
                    [
                        "ZHIPU_API_KEY is not configured",
                        "",
                        "Checked in order: process environment, repo-local vault, global vault",
                        "",
                        "Configure the key (recommended):",
                        "    libra config set --global vault.env.ZHIPU_API_KEY <value>",
                        "    export ZHIPU_API_KEY=<value>",
                        "",
                        "Run without credentials:",
                        "    libra code --provider codex",
                        "    libra code --provider ollama --model <name>",
                    ]
                    .join("\n")
                )
            })?;
        let base_url = resolve_env_for_target("ZHIPU_BASE_URL", local_target)
            .await?
            .unwrap_or_else(|| "https://open.bigmodel.cn/api/paas/v4".to_string());

        let provider = ZhipuProvider::new(api_key);
        Ok(Self::new(&base_url, provider))
    }

    /// Creates a Zhipu client with the given API key.
    pub fn with_api_key(api_key: String) -> Self {
        let provider = ZhipuProvider::new(api_key);
        Self::new("https://open.bigmodel.cn/api/paas/v4", provider)
    }

    /// Creates a Zhipu client with a custom base URL and API key.
    pub fn with_base_url(base_url: &str, api_key: String) -> Self {
        let provider = ZhipuProvider::new(api_key);
        Self::new(base_url, provider)
    }
}

fn resolve_zhipu_env_required(name: &'static str) -> Result<String, std::env::VarError> {
    match crate::internal::config::resolve_env_sync(name) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(std::env::VarError::NotPresent),
        Err(error) => {
            tracing::warn!(
                env_var = name,
                error = %error,
                "vault-aware Zhipu env resolution failed; falling back to process env"
            );
            std::env::var(name)
        }
    }
}

fn resolve_zhipu_env_optional(name: &'static str) -> Result<Option<String>, std::env::VarError> {
    match crate::internal::config::resolve_env_sync(name) {
        Ok(value) => Ok(value),
        Err(error) => {
            tracing::warn!(
                env_var = name,
                error = %error,
                "vault-aware Zhipu env resolution failed; falling back to process env"
            );
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zhipu_provider_debug() {
        let provider = ZhipuProvider::new("test-key".to_string());
        let debug_str = format!("{:?}", provider);
        assert!(!debug_str.contains("test-key"));
        assert!(debug_str.contains("***"));
    }

    #[test]
    fn test_zhipu_provider_api_key() {
        let provider = ZhipuProvider::new("test-key".to_string());
        assert_eq!(provider.api_key(), "test-key");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn from_resolved_env_reads_zhipu_api_key_from_process_env() {
        let key_guard = TestEnvGuard::set("ZHIPU_API_KEY", Some("zh-test-resolved"));
        let base_guard = TestEnvGuard::set("ZHIPU_BASE_URL", None);
        let global_guard = TestEnvGuard::set(
            "LIBRA_CONFIG_GLOBAL_DB",
            Some("/nonexistent/zhipu-from-resolved-env-test.db"),
        );

        let client = Client::from_resolved_env(crate::internal::config::LocalIdentityTarget::None)
            .await
            .expect("from_resolved_env should succeed when ZHIPU_API_KEY is set");
        assert_eq!(client.provider.api_key(), "zh-test-resolved");

        drop(key_guard);
        drop(base_guard);
        drop(global_guard);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn from_resolved_env_errors_when_no_layer_supplies_api_key() {
        let key_guard = TestEnvGuard::set("ZHIPU_API_KEY", None);
        let base_guard = TestEnvGuard::set("ZHIPU_BASE_URL", None);
        let global_guard = TestEnvGuard::set(
            "LIBRA_CONFIG_GLOBAL_DB",
            Some("/nonexistent/zhipu-from-resolved-env-test.db"),
        );

        let err = Client::from_resolved_env(crate::internal::config::LocalIdentityTarget::None)
            .await
            .expect_err("from_resolved_env must fail without an API key");
        // plan-20260825 PS-01 (terra R1): pin the ADR-PS-03 a-mode
        // structure so a regression to the old unexecutable hint, a wrong
        // lookup chain, lost indentation, or missing credential-free
        // alternatives turns this test red.
        let msg = err.to_string();
        assert!(
            msg.starts_with("ZHIPU_API_KEY is not configured"),
            "conclusion line drifted: {msg}"
        );
        assert!(
            msg.contains("Checked in order: process environment, repo-local vault, global vault")
                && !msg.contains("--env-file"),
            "chain must reflect resolve_env_for_target (no --env-file layer): {msg}"
        );
        assert!(
            msg.contains("    libra config set --global vault.env.ZHIPU_API_KEY <value>"),
            "recommended command must be the executable config set form: {msg}"
        );
        let extinct = format!("config --global {}", "add");
        assert!(
            !msg.contains(&extinct),
            "the removed subcommand shape must never reappear: {msg}"
        );
        assert!(
            msg.contains("    libra code --provider codex")
                && msg.contains("    libra code --provider ollama --model <name>"),
            "credential-free alternatives missing: {msg}"
        );

        drop(key_guard);
        drop(base_guard);
        drop(global_guard);
    }

    #[test]
    #[serial_test::serial]
    fn from_env_process_env_overrides_global_vault_and_vault_is_fallback() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key_guard = TestEnvGuard::set("ZHIPU_API_KEY", Some("zh-env-key"));
        let base_guard = TestEnvGuard::set("ZHIPU_BASE_URL", None);
        let global_dir = tempfile::tempdir().unwrap();
        let global_db_path = global_dir.path().join("zhipu-global-config.db");
        let global_db_string = global_db_path.to_string_lossy().into_owned();
        let global_guard = TestEnvGuard::set("LIBRA_CONFIG_GLOBAL_DB", Some(&global_db_string));

        let global_conn = runtime
            .block_on(crate::internal::db::create_database(&global_db_string))
            .unwrap();
        runtime
            .block_on(crate::internal::config::ConfigKv::set_with_conn(
                &global_conn,
                "vault.env.ZHIPU_API_KEY",
                "zh-vault-key",
                false,
            ))
            .unwrap();
        let _cwd_guard = crate::utils::test::ChangeDirGuard::new(global_dir.path());

        let client = Client::from_env().expect("from_env should prefer process ZHIPU_API_KEY");
        assert_eq!(client.provider.api_key(), "zh-env-key");

        drop(key_guard);
        let key_unset = TestEnvGuard::set("ZHIPU_API_KEY", None);
        let client = Client::from_env().expect("from_env should fall back to vault");
        assert_eq!(client.provider.api_key(), "zh-vault-key");

        drop(key_unset);
        drop(base_guard);
        drop(global_guard);
    }

    struct TestEnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl TestEnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: tests are serialized via `#[serial_test::serial]`.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self { key, original }
        }
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.original {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}
