//! Manifest signing trust table (plan-20260714 §A.6).
//!
//! The table is compiled into the client. Each key carries a validity window
//! and a monotonically increasing `generation`; the compile-time floor
//! [`MIN_TRUSTED_KEY_GENERATION`] implements anti-rollback for key rotation:
//! old clients keep a lower floor, new releases raise it — neither the
//! manifest nor wall-clock time can lower the accepted generation.
//!
//! The production table ships EMPTY until the official release-key ceremony
//! provisions a key pair inside the protected signing environment (§A.6
//! signing-job isolation). With no trusted keys, every envelope fails
//! verification and auto-upgrade stays inert — fail-closed by construction.
//!
//! Test injection: only the `test-upgrade` feature compiles the override
//! hook, and it additionally requires `LIBRA_TEST=1` at runtime. Production
//! builds contain no override code path at all (§A.11).

/// One trusted manifest-signing key.
#[derive(Debug, Clone, Copy)]
pub struct TrustedKey {
    /// Stable identifier referenced by envelope signatures.
    pub key_id: &'static str,
    /// Raw Ed25519 public key bytes.
    pub ed25519_pubkey: [u8; 32],
    /// Validity window start (unix seconds, inclusive).
    pub not_before: i64,
    /// Validity window end (unix seconds, inclusive).
    pub not_after: i64,
    /// Rotation generation; must be `>= max(manifest.min_key_generation,`
    /// [`MIN_TRUSTED_KEY_GENERATION`]`)` to accept a signature.
    pub generation: u32,
}

/// Compile-time anti-rollback floor for key generations (§A.6).
pub const MIN_TRUSTED_KEY_GENERATION: u32 = 1;

/// Production trust table. Empty until the release-key ceremony; see module
/// docs — an empty table means every verification fails closed.
pub const PRODUCTION_TRUSTED_KEYS: &[TrustedKey] = &[];

/// The active trust table.
///
/// In production builds this is always [`PRODUCTION_TRUSTED_KEYS`]. Under the
/// `test-upgrade` feature, tests may install an override (guarded again at
/// runtime by `LIBRA_TEST=1`).
pub fn active_trust_table() -> &'static [TrustedKey] {
    #[cfg(feature = "test-upgrade")]
    {
        if std::env::var_os("LIBRA_TEST").is_some_and(|v| v == "1")
            && let Some(injected) = test_injection::injected_keys()
        {
            return injected;
        }
    }
    PRODUCTION_TRUSTED_KEYS
}

/// Test-only trust-root injection, compiled only with `--features
/// test-upgrade` (§A.11: release builds cannot alter the trust root even with
/// `LIBRA_TEST=1` set, because this module does not exist there).
#[cfg(feature = "test-upgrade")]
pub mod test_injection {
    use std::sync::OnceLock;

    use super::TrustedKey;

    static INJECTED: OnceLock<&'static [TrustedKey]> = OnceLock::new();

    /// Install a leaked, process-lifetime trust table for tests. First call
    /// wins; later calls are ignored (tests must be serialized around this).
    pub fn inject_keys(keys: &'static [TrustedKey]) {
        let _ = INJECTED.set(keys);
    }

    pub(super) fn injected_keys() -> Option<&'static [TrustedKey]> {
        INJECTED.get().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_table_is_empty_until_key_ceremony() {
        // Fail-closed guarantee: with no trusted keys, no envelope verifies.
        assert!(PRODUCTION_TRUSTED_KEYS.is_empty());
        const {
            assert!(MIN_TRUSTED_KEY_GENERATION >= 1);
        }
    }
}
