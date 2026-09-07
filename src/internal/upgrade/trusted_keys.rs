//! Manifest signing trust table (plan-20260714 §A.6).
//!
//! The table is compiled into the client. Each key carries a validity window
//! and a monotonically increasing `generation`; the compile-time floor
//! [`MIN_TRUSTED_KEY_GENERATION`] implements anti-rollback for key rotation:
//! old clients keep a lower floor, new releases raise it — neither the
//! manifest nor wall-clock time can lower the accepted generation.
//!
//! The production table contains only the public half of the release key. Its
//! matching private key must be provisioned only in the protected signing
//! environment (§A.6 signing-job isolation), never in this repository.
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

/// Production trust table provisioned by the 2026-08-31 release-key ceremony.
pub const PRODUCTION_TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    key_id: "libra-release-1",
    ed25519_pubkey: [
        0x68, 0xaa, 0x00, 0xea, 0x93, 0x58, 0xd4, 0x55, 0x64, 0x50, 0x10, 0xd8, 0x11, 0xd4, 0x07,
        0x02, 0xb3, 0xf6, 0x7c, 0xec, 0x4b, 0xdf, 0xf5, 0x2d, 0x3d, 0x4f, 0xb8, 0x10, 0x7a, 0xfa,
        0xee, 0xd3,
    ],
    not_before: 1_788_174_595,
    not_after: 1_819_670_400,
    generation: 1,
}];

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
    use base64::Engine as _;

    use super::*;
    use crate::internal::upgrade::manifest::{SIGNATURE_DOMAIN_PREFIX, verify_envelope_bytes};

    const RELEASE_KEY_SMOKE_DOMAIN_PREFIX: &[u8] = b"libra-release-key-smoke-v1\0";

    #[derive(serde::Deserialize)]
    struct PublicKeySmokeProof {
        schema_version: u8,
        key_id: String,
        payload: String,
        signature: String,
    }

    #[test]
    fn production_table_matches_configured_public_key_installer_constants_and_ceremony_record() {
        let key = PRODUCTION_TRUSTED_KEYS
            .first()
            .copied()
            .expect("production ceremony key must be present");
        assert_eq!(PRODUCTION_TRUSTED_KEYS.len(), 1);
        assert_eq!(key.key_id, "libra-release-1");
        assert_eq!(
            key.ed25519_pubkey,
            [
                0x68, 0xaa, 0x00, 0xea, 0x93, 0x58, 0xd4, 0x55, 0x64, 0x50, 0x10, 0xd8, 0x11, 0xd4,
                0x07, 0x02, 0xb3, 0xf6, 0x7c, 0xec, 0x4b, 0xdf, 0xf5, 0x2d, 0x3d, 0x4f, 0xb8, 0x10,
                0x7a, 0xfa, 0xee, 0xd3,
            ]
        );
        assert_eq!(key.not_before, 1_788_174_595);
        assert_eq!(key.not_after, 1_819_670_400);
        assert_eq!(key.generation, 1);
        const {
            assert!(MIN_TRUSTED_KEY_GENERATION >= 1);
        }

        let install_sh = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"));
        assert!(install_sh.contains("LIBRA_RELEASE_MANIFEST_KEY_ID=\"libra-release-1\""));
        assert!(install_sh.contains(
            "LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_HEX=\"68aa00ea9358d455645010d811d40702b3f67cec4bdff52d3d4fb8107afaeed3\""
        ));
        // The PEM constant install.sh feeds to `openssl pkeyutl` must be the
        // SAME key: SubjectPublicKeyInfo DER = fixed Ed25519 prefix + raw key.
        {
            use base64::Engine as _;
            let mut spki = vec![
                0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
            ];
            spki.extend_from_slice(&key.ed25519_pubkey);
            let expected_pem_body = base64::engine::general_purpose::STANDARD.encode(&spki);
            assert!(
                install_sh.contains(&format!(
                    "-----BEGIN PUBLIC KEY-----\n{expected_pem_body}\n-----END PUBLIC KEY-----"
                )),
                "install.sh PEM constant must encode the production trust root"
            );
        }

        // The installers' key-policy constants (generation + validity window)
        // must track the same trust-table entry: a rotation that updates the
        // native table but leaves an installer's window stale fails here.
        assert!(install_sh.contains("LIBRA_RELEASE_MANIFEST_KEY_GENERATION=1"));
        assert!(
            install_sh.contains("LIBRA_RELEASE_MANIFEST_KEY_NOT_BEFORE=\"2026-08-31T11:09:55Z\"")
        );
        assert!(
            install_sh.contains("LIBRA_RELEASE_MANIFEST_KEY_NOT_AFTER=\"2027-08-31T00:00:00Z\"")
        );

        let install_ps1 = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/install.ps1"));
        assert!(install_ps1.contains("$ReleaseManifestKeyId = \"libra-release-1\""));
        assert!(install_ps1.contains(
            "$ReleaseManifestPublicKeyHex = \"68aa00ea9358d455645010d811d40702b3f67cec4bdff52d3d4fb8107afaeed3\""
        ));
        assert!(install_ps1.contains("$ReleaseManifestKeyGeneration = 1"));
        assert!(install_ps1.contains("$ReleaseManifestKeyNotBefore = \"2026-08-31T11:09:55Z\""));
        assert!(install_ps1.contains("$ReleaseManifestKeyNotAfter = \"2027-08-31T00:00:00Z\""));

        // The canonical-UTC window strings must equal the numeric windows in
        // this table (chrono is the authority, not a hand-derived comment).
        for (raw, expected) in [
            ("2026-08-31T11:09:55Z", key.not_before),
            ("2027-08-31T00:00:00Z", key.not_after),
        ] {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw)
                .expect("window constant must be RFC3339")
                .timestamp();
            assert_eq!(parsed, expected, "installer window constant {raw} drifted");
        }

        let ceremony_record = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/development/internal/release-signing-auto-upgrade.md"
        ));
        assert!(ceremony_record.contains(
            "`key_id=libra-release-1`、`generation=1`、raw Ed25519 public key hex=`68aa00ea9358d455645010d811d40702b3f67cec4bdff52d3d4fb8107afaeed3`、`not_before=2026-08-31T11:09:55Z`（`1788174595`）、`not_after=2027-08-31T00:00:00Z`（`1819670400`）"
        ));
    }

    #[test]
    fn production_key_verifies_public_smoke_proof_only_in_smoke_domain() {
        let smoke_fixture = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/up01-production-key-smoke.json"
        ));
        let proof: PublicKeySmokeProof =
            serde_json::from_slice(smoke_fixture).expect("public smoke fixture must be valid JSON");
        assert_eq!(proof.schema_version, 1);
        assert_eq!(proof.key_id, "libra-release-1");

        let key = PRODUCTION_TRUSTED_KEYS
            .iter()
            .find(|key| key.key_id == proof.key_id)
            .expect("public smoke proof must name the configured production key");
        let payload = base64::engine::general_purpose::STANDARD
            .decode(proof.payload)
            .expect("public smoke payload must be base64");
        let signature = base64::engine::general_purpose::STANDARD
            .decode(proof.signature)
            .expect("public smoke signature must be base64");

        let mut smoke_message = RELEASE_KEY_SMOKE_DOMAIN_PREFIX.to_vec();
        smoke_message.extend_from_slice(&payload);
        let verifier =
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.ed25519_pubkey);
        assert!(verifier.verify(&smoke_message, &signature).is_ok());

        let mut manifest_message = SIGNATURE_DOMAIN_PREFIX.to_vec();
        manifest_message.extend_from_slice(&payload);
        let manifest_verifier =
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.ed25519_pubkey);
        assert!(
            manifest_verifier
                .verify(&manifest_message, &signature)
                .is_err()
        );
        assert!(verify_envelope_bytes(smoke_fixture, PRODUCTION_TRUSTED_KEYS).is_err());

        let payload: serde_json::Value =
            serde_json::from_slice(&payload).expect("public smoke payload must be valid JSON");
        assert_eq!(payload["purpose"], "libra-release-key-smoke-v1");
        assert_eq!(payload["key_id"], proof.key_id);
        assert_eq!(payload["generation"], 1);
        assert!(payload.get("artifacts").is_none());
        assert!(payload.get("version").is_none());
    }
}
