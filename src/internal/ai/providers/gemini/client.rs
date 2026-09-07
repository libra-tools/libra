//! Gemini API client construction and authentication.
//!
//! This module provides [`GeminiProvider`], which implements the generic
//! [`Provider`] trait by attaching the `x-goog-api-key` header to every
//! outgoing HTTP request. Unlike OpenAI-style providers that use
//! `Authorization: Bearer <token>`, the Gemini API authenticates via its
//! own header (it also accepts a `key=` query parameter, but the header
//! approach is used here for consistency with the generic `Provider` trait).
//!
//! The [`Client`] type alias combines the generic HTTP client with
//! `GeminiProvider` and exposes convenience constructors. The base URL
//! defaults to `https://generativelanguage.googleapis.com`.

use std::fmt;

use crate::internal::ai::client::{Client as HttpClient, Provider};

/// Gemini API provider that carries the API key and injects the
/// `x-goog-api-key` authentication header into every request.
///
/// The `Debug` impl masks the secret to keep it out of `tracing` output and
/// panic backtraces.
#[derive(Clone)]
pub struct GeminiProvider {
    api_key: String,
}

/// Manually implemented to redact the API key from debug output.
impl fmt::Debug for GeminiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiProvider")
            .field("api_key", &"***")
            .finish()
    }
}

impl GeminiProvider {
    /// Creates a new GeminiProvider with the given API key.
    ///
    /// Boundary conditions: the key is not validated; an invalid key surfaces
    /// at request time as an HTTP 403 from the Gemini API.
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    /// Returns the API key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

impl Provider for GeminiProvider {
    /// Attaches the `x-goog-api-key` header for Gemini authentication.
    fn on_request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("x-goog-api-key", &self.api_key)
    }
}

/// Concrete Gemini client type, combining the generic HTTP client with
/// [`GeminiProvider`] for authentication.
pub type Client = HttpClient<GeminiProvider>;

impl Client {
    /// Creates a Gemini Client from environment variables or Vault.
    ///
    /// Functional scope: priority chain (12-Factor, see
    /// `docs/development/commands/config.md`):
    /// 1. Process env `GEMINI_API_KEY`
    /// 2. Local repo config (`vault.env.GEMINI_API_KEY`)
    /// 3. Global config (`vault.env.GEMINI_API_KEY`)
    ///
    /// Points at the public `generativelanguage.googleapis.com` endpoint.
    ///
    /// Boundary conditions: returns an actionable error when `GEMINI_API_KEY`
    /// is unset across Vault and process env; the CLI deliberately does not
    /// expose a base-URL override — Gemini's public API has no stable proxy
    /// contract for end users. Test-only consumers that need to point at a
    /// localhost stub should use [`Client::with_base_url`].
    ///
    /// New call sites should prefer [`Client::from_resolved_env`], which
    /// performs the same lookup chain asynchronously and accepts an
    /// explicit `LocalIdentityTarget<'_>` so vault values from a specific
    /// repository are honored. `from_env` is retained for backward
    /// compatibility and currently delegates to the same vault-aware
    /// resolver.
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = crate::internal::config::resolve_required_env_sync("GEMINI_API_KEY")?;
        let provider = GeminiProvider::new(api_key);
        Ok(Self::new(
            "https://generativelanguage.googleapis.com",
            provider,
        ))
    }

    /// Constructs a Gemini client whose `GEMINI_API_KEY` is resolved through
    /// the libra-aware lookup chain: process env → local `.libra/libra.db`
    /// (`vault.env.GEMINI_API_KEY`, when `local_target` selects a repo) →
    /// global `~/.libra/config.db` (same key).
    ///
    /// Differs from [`Self::from_env`] in two ways:
    ///
    /// 1. The lookup honours the `libra config set --global
    ///    vault.env.GEMINI_API_KEY <…>` setting, so users who configured the
    ///    key once via the CLI no longer need to re-export it in every shell.
    /// 2. The error surface is `anyhow::Error`, so callers can attach context
    ///    (which key was missing, whether the config DB was unreachable, …)
    ///    and surface the underlying chain via `format!("{error:#}")`.
    ///
    /// The `local_target` argument mirrors the
    /// [`super::super::deepseek::client::Client::from_resolved_env`]
    /// contract; pass `LocalIdentityTarget::None` from non-repo entry points
    /// (the Gemini CLI / Code bootstrap), or `LocalIdentityTarget::CurrentRepo`
    /// when running inside a repo where `.libra/libra.db` may carry a
    /// repo-scoped override.
    ///
    /// Returns `Err` when the key is unset across all three layers OR the
    /// config DB read failed in an unrecoverable way (the global-config-DB
    /// schema-mismatch path is already downgraded to a `tracing::warn!` in
    /// `resolve_user_identity_sources`, but other I/O errors still bubble up
    /// here).
    pub async fn from_resolved_env(
        local_target: crate::internal::config::LocalIdentityTarget<'_>,
    ) -> anyhow::Result<Self> {
        use anyhow::anyhow;

        use crate::internal::config::resolve_env_for_target;

        let api_key = resolve_env_for_target("GEMINI_API_KEY", local_target)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "GEMINI_API_KEY is not configured; set vault.env.GEMINI_API_KEY with \
                     `libra config set vault.env.GEMINI_API_KEY <key>` or export GEMINI_API_KEY"
                )
            })?;
        let provider = GeminiProvider::new(api_key);
        Ok(Self::new(
            "https://generativelanguage.googleapis.com",
            provider,
        ))
    }

    /// Creates a Gemini Client with a custom base URL and API key.
    ///
    /// Intended for tests that need to point the client at a localhost stub
    /// (Wave 10 §5.2 boot smoke); the CLI never invokes this constructor —
    /// `from_env` is the production entry point. A separate constructor
    /// (rather than mutating `from_env`) keeps the production base URL
    /// explicit and ensures CLI-driven configuration cannot accidentally
    /// reroute requests to an arbitrary host.
    pub fn with_base_url(base_url: &str, api_key: String) -> Self {
        let provider = GeminiProvider::new(api_key);
        Self::new(base_url, provider)
    }

    /// Creates a [`CompletionModel`](super::completion::CompletionModel) bound
    /// to this client for the given Gemini model identifier (e.g.,
    /// `"gemini-2.5-flash"` or one of the constants from [`super`]).
    ///
    /// Boundary conditions: the model id is forwarded verbatim; unknown ids
    /// fail at request time with a 404.
    pub fn completion_model(&self, model: &str) -> super::completion::CompletionModel {
        super::completion::CompletionModel::new(self.clone(), model)
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;
    use crate::internal::config::LocalIdentityTarget;

    /// Process-env path: when `GEMINI_API_KEY` is exported, the async
    /// resolver path returns the key verbatim — no global-config DB
    /// involvement needed.
    #[tokio::test]
    #[serial]
    async fn from_resolved_env_reads_gemini_api_key_from_process_env() {
        let key_guard = TestEnvGuard::set("GEMINI_API_KEY", Some("gm-test-resolved"));
        // Point the global config DB at a nonexistent path so the resolver
        // can't accidentally pick up a host-side `vault.env.GEMINI_API_KEY`
        // and mask the env-path assertion.
        let global_guard = TestEnvGuard::set(
            "LIBRA_CONFIG_GLOBAL_DB",
            Some("/nonexistent/gemini-from-resolved-env-test.db"),
        );

        let client = Client::from_resolved_env(LocalIdentityTarget::None)
            .await
            .expect("from_resolved_env should succeed when GEMINI_API_KEY is set");
        assert_eq!(client.provider.api_key, "gm-test-resolved");

        drop(key_guard);
        drop(global_guard);
    }

    /// Absence path: when neither the env nor the (nonexistent) global
    /// config DB supplies a key, the error must mention `GEMINI_API_KEY`
    /// by name so users know which setting to populate.
    #[tokio::test]
    #[serial]
    async fn from_resolved_env_errors_when_no_layer_supplies_api_key() {
        let key_guard = TestEnvGuard::set("GEMINI_API_KEY", None);
        let global_guard = TestEnvGuard::set(
            "LIBRA_CONFIG_GLOBAL_DB",
            Some("/nonexistent/gemini-from-resolved-env-test.db"),
        );

        let err = Client::from_resolved_env(LocalIdentityTarget::None)
            .await
            .expect_err("from_resolved_env must fail without an API key");
        assert!(
            err.to_string().contains("GEMINI_API_KEY"),
            "error should name the missing key, got: {err}"
        );

        drop(key_guard);
        drop(global_guard);
    }

    /// Debug formatting must not leak the secret API key.
    #[test]
    fn gemini_provider_debug_masks_api_key() {
        let provider = GeminiProvider::new("gm-secret-key-1234".to_string());
        let debug_str = format!("{provider:?}");
        assert!(
            !debug_str.contains("gm-secret-key-1234"),
            "Debug must redact the API key; got {debug_str}"
        );
    }

    struct TestEnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl TestEnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            // SAFETY: tests are serialized via `#[serial]`, so concurrent
            // env mutation across tests cannot race; the guard restores the
            // previous value on drop.
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
            // SAFETY: see [`TestEnvGuard::set`].
            unsafe {
                match &self.original {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}
