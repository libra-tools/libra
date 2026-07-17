//! Signed release-manifest verification (plan-20260714 §A.6).
//!
//! [`verify_envelope_bytes`] is a PURE function: bytes + trust table in,
//! validated payload out. It performs, in order:
//!
//! 1. envelope parse (`schema_version`, base64 `payload`, `signatures[]`,
//!    duplicate `key_id` rejection);
//! 2. pure Ed25519 verification of `b"libra-upgrade-manifest-v1\0" ||
//!    payload_bytes` against the compiled trust table;
//! 3. payload parse and full semantic validation (channel, release SemVer,
//!    artifact matrix coverage/uniqueness, structural URL validation with
//!    cross-field `tag == version` / URL-platform == artifact-platform
//!    binding, size bounds);
//! 4. key-policy checks that need payload fields: rotation-generation floor
//!    and key-validity windows around `published_at`/`expires_at`.
//!
//! Nothing here reads the clock or touches persistent state — time policy
//! and anti-rollback state live in the state module so that every
//! cross-field check provably happens before any state write (§A.6/§A.7).

use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    platform::Platform,
    trusted_keys::{MIN_TRUSTED_KEY_GENERATION, TrustedKey},
};

/// Domain-separation prefix for signature verification (§A.6 信封).
pub const SIGNATURE_DOMAIN_PREFIX: &[u8] = b"libra-upgrade-manifest-v1\0";

/// Manifest endpoint (§A.6). Only overridable in `test-upgrade` builds.
pub const MANIFEST_URL: &str =
    "https://download.libra.tools/libra/releases/stable/manifest-v1.json";

/// Manifest host every artifact URL must use.
pub const ARTIFACT_HOST: &str = "download.libra.tools";

/// Maximum accepted manifest size (§A.6 体积).
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Maximum accepted artifact size (§A.6 体积: `0 < size <= 128MiB`).
pub const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// A strictly-parsed release version: `X.Y.Z`, no prerelease/build metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseVersion(pub u64, pub u64, pub u64);

impl ReleaseVersion {
    /// Parse `X.Y.Z` where each part is `0` or a digit string without a
    /// leading zero. Anything else (prerelease, build metadata, `v` prefix,
    /// whitespace) is rejected (§A.6: 版本无 prerelease/build).
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split('.');
        let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        fn num(part: &str) -> Option<u64> {
            if part.is_empty() || (part.len() > 1 && part.starts_with('0')) {
                return None;
            }
            if !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        }
        Some(ReleaseVersion(num(a)?, num(b)?, num(c)?))
    }
}

impl std::fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// One artifact row of the manifest payload.
#[derive(Debug, Clone)]
pub struct VerifiedArtifact {
    pub platform: Platform,
    pub url: String,
    /// Lowercase 64-hex sha256.
    pub sha256: String,
    pub size: u64,
}

/// The fully validated payload.
#[derive(Debug, Clone)]
pub struct VerifiedManifest {
    /// sha256 of the raw payload bytes — the envelope digest used for
    /// `control_revision ==` idempotence checks in anti-rollback state (§A.7).
    pub payload_digest: [u8; 32],
    /// Key id of the accepted signature (recorded in the install marker).
    pub signer_key_id: String,
    pub version: ReleaseVersion,
    /// Raw version string exactly as signed (URL tags must match it).
    pub version_raw: String,
    pub control_revision: u64,
    /// Unix seconds.
    pub published_at: i64,
    /// Unix seconds.
    pub expires_at: i64,
    pub min_key_generation: u32,
    pub paused: bool,
    pub revoked_versions: Vec<ReleaseVersion>,
    /// Exactly one artifact per release-matrix platform.
    pub artifacts: Vec<VerifiedArtifact>,
}

impl VerifiedManifest {
    /// The artifact for `platform` (guaranteed present after validation).
    pub fn artifact_for(&self, platform: Platform) -> Option<&VerifiedArtifact> {
        self.artifacts.iter().find(|a| a.platform == platform)
    }

    /// Whether `version` is revoked by this manifest (§A.6: revoked versions
    /// must never install, even from cache/retry).
    pub fn is_revoked(&self, version: ReleaseVersion) -> bool {
        self.revoked_versions.contains(&version)
    }
}

/// Verification failures. Every variant is terminal for the current check
/// cycle and must never poison persistent state (§A.6 时间).
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit")]
    TooLarge,
    #[error("manifest envelope is not valid JSON: {0}")]
    EnvelopeParse(String),
    #[error("unsupported manifest envelope schema_version {0}")]
    EnvelopeSchema(u64),
    #[error("manifest payload is not valid base64: {0}")]
    PayloadBase64(String),
    #[error("duplicate signature key_id '{0}' in manifest envelope")]
    DuplicateKeyId(String),
    #[error("no manifest signature verifies against the compiled trust table")]
    NoTrustedSignature,
    #[error(
        "no verifying signature meets the key-generation floor {floor} \
         (manifest min_key_generation {manifest_min})"
    )]
    KeyGenerationBelowFloor { floor: u32, manifest_min: u32 },
    #[error("no verifying key's validity window covers the manifest lifetime")]
    KeyWindowMismatch,
    #[error("manifest payload is not valid JSON: {0}")]
    PayloadParse(String),
    #[error("manifest payload invalid: {0}")]
    PayloadInvalid(String),
}

// ── on-wire shapes ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EnvelopeDocument {
    schema_version: u64,
    payload: String,
    signatures: Vec<SignatureEntry>,
}

#[derive(Deserialize)]
struct SignatureEntry {
    key_id: String,
    signature: String,
}

#[derive(Deserialize)]
struct PayloadDocument {
    channel: String,
    version: String,
    control_revision: u64,
    published_at: String,
    expires_at: String,
    min_key_generation: u32,
    paused: bool,
    revoked_versions: Vec<String>,
    artifacts: Vec<ArtifactEntry>,
}

#[derive(Deserialize)]
struct ArtifactEntry {
    platform: String,
    url: String,
    sha256: String,
    size: u64,
}

// ── verification ─────────────────────────────────────────────────────────────

/// Verify a raw manifest envelope against `trust` (see module docs for the
/// exact order). Pure: no clock, no I/O, no state.
pub fn verify_envelope_bytes(
    envelope_bytes: &[u8],
    trust: &[TrustedKey],
) -> Result<VerifiedManifest, ManifestError> {
    if envelope_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    let envelope: EnvelopeDocument = serde_json::from_slice(envelope_bytes)
        .map_err(|e| ManifestError::EnvelopeParse(e.to_string()))?;
    if envelope.schema_version != 1 {
        return Err(ManifestError::EnvelopeSchema(envelope.schema_version));
    }
    // Reject duplicate key_ids before any crypto (§A.6 信任表).
    {
        let mut seen = std::collections::HashSet::new();
        for sig in &envelope.signatures {
            if !seen.insert(sig.key_id.as_str()) {
                return Err(ManifestError::DuplicateKeyId(sig.key_id.clone()));
            }
        }
    }
    let payload_bytes = base64::engine::general_purpose::STANDARD
        .decode(envelope.payload.as_bytes())
        .map_err(|e| ManifestError::PayloadBase64(e.to_string()))?;

    // 1) Pure cryptographic verification (no policy yet).
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN_PREFIX.len() + payload_bytes.len());
    message.extend_from_slice(SIGNATURE_DOMAIN_PREFIX);
    message.extend_from_slice(&payload_bytes);
    let mut crypto_valid: Vec<&TrustedKey> = Vec::new();
    for sig in &envelope.signatures {
        let Some(key) = trust.iter().find(|k| k.key_id == sig.key_id) else {
            continue;
        };
        let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(&sig.signature) else {
            continue;
        };
        let verifier =
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.ed25519_pubkey);
        if verifier.verify(&message, &sig_bytes).is_ok() {
            crypto_valid.push(key);
        }
    }
    if crypto_valid.is_empty() {
        return Err(ManifestError::NoTrustedSignature);
    }

    // 2) Payload parse + full semantic validation.
    let payload: PayloadDocument = serde_json::from_slice(&payload_bytes)
        .map_err(|e| ManifestError::PayloadParse(e.to_string()))?;
    let manifest = validate_payload(&payload, &payload_bytes)?;

    // 3) Key policy that needs payload fields (§A.6): generation floor, then
    //    validity windows around the signed lifetime.
    let floor = manifest.min_key_generation.max(MIN_TRUSTED_KEY_GENERATION);
    let meets_generation: Vec<&&TrustedKey> = crypto_valid
        .iter()
        .filter(|k| k.generation >= floor)
        .collect();
    if meets_generation.is_empty() {
        return Err(ManifestError::KeyGenerationBelowFloor {
            floor,
            manifest_min: manifest.min_key_generation,
        });
    }
    let accepted = meets_generation
        .iter()
        .find(|k| {
            k.not_before <= manifest.published_at
                && manifest.published_at <= k.not_after
                && manifest.expires_at <= k.not_after
        })
        .ok_or(ManifestError::KeyWindowMismatch)?;

    Ok(VerifiedManifest {
        signer_key_id: accepted.key_id.to_string(),
        ..manifest
    })
}

/// RFC3339 → unix seconds.
fn parse_rfc3339(raw: &str, field: &str) -> Result<i64, ManifestError> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp())
        .map_err(|e| ManifestError::PayloadInvalid(format!("{field} is not RFC3339: {e}")))
}

/// Full semantic validation of the payload (§A.6 payload/URL 解析/体积).
/// `signer_key_id` is filled by the caller after key-policy checks.
fn validate_payload(
    payload: &PayloadDocument,
    payload_bytes: &[u8],
) -> Result<VerifiedManifest, ManifestError> {
    let invalid = |msg: String| ManifestError::PayloadInvalid(msg);

    if payload.channel != "stable" {
        return Err(invalid(format!(
            "channel '{}' is not 'stable'",
            payload.channel
        )));
    }
    let version = ReleaseVersion::parse(&payload.version).ok_or_else(|| {
        invalid(format!(
            "version '{}' is not a release SemVer (X.Y.Z, no prerelease/build)",
            payload.version
        ))
    })?;
    let published_at = parse_rfc3339(&payload.published_at, "published_at")?;
    let expires_at = parse_rfc3339(&payload.expires_at, "expires_at")?;
    if published_at >= expires_at {
        return Err(invalid("published_at must be before expires_at".into()));
    }
    let mut revoked_versions = Vec::with_capacity(payload.revoked_versions.len());
    for raw in &payload.revoked_versions {
        revoked_versions.push(
            ReleaseVersion::parse(raw).ok_or_else(|| {
                invalid(format!("revoked version '{raw}' is not a release SemVer"))
            })?,
        );
    }

    // Artifacts: unique platforms, exact release-matrix coverage, per-row
    // structural URL validation with cross-field binding.
    let mut artifacts = Vec::with_capacity(payload.artifacts.len());
    let mut seen = std::collections::HashSet::new();
    for row in &payload.artifacts {
        let platform = Platform::parse(&row.platform)
            .ok_or_else(|| invalid(format!("unknown artifact platform '{}'", row.platform)))?;
        if !seen.insert(platform) {
            return Err(invalid(format!("duplicate artifact platform '{platform}'")));
        }
        if row.size == 0 || row.size > MAX_ARTIFACT_BYTES {
            return Err(invalid(format!(
                "artifact '{platform}' size {} outside (0, {MAX_ARTIFACT_BYTES}]",
                row.size
            )));
        }
        if row.sha256.len() != 64 || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid(format!(
                "artifact '{platform}' sha256 is not 64 hex characters"
            )));
        }
        validate_artifact_url(&row.url, &payload.version, platform)?;
        artifacts.push(VerifiedArtifact {
            platform,
            url: row.url.clone(),
            sha256: row.sha256.to_ascii_lowercase(),
            size: row.size,
        });
    }
    for required in Platform::RELEASE_MATRIX {
        if !seen.contains(required) {
            return Err(invalid(format!(
                "artifact matrix is missing platform '{required}'"
            )));
        }
    }

    Ok(VerifiedManifest {
        payload_digest: Sha256::digest(payload_bytes).into(),
        signer_key_id: String::new(),
        version,
        version_raw: payload.version.clone(),
        control_revision: payload.control_revision,
        published_at,
        expires_at,
        min_key_generation: payload.min_key_generation,
        paused: payload.paused,
        revoked_versions,
        artifacts,
    })
}

/// Structural artifact-URL validation (§A.6 URL 解析): https, pinned host,
/// default/443 port, exact 4-segment path `/libra/releases/v{tag}/libra-
/// {platform}`, empty query/fragment, `tag == payload.version`, URL platform
/// == artifact platform.
fn validate_artifact_url(
    raw: &str,
    version_raw: &str,
    platform: Platform,
) -> Result<(), ManifestError> {
    let invalid = |msg: String| ManifestError::PayloadInvalid(msg);
    let url = url::Url::parse(raw).map_err(|e| invalid(format!("artifact url invalid: {e}")))?;
    if url.scheme() != "https" {
        return Err(invalid(format!("artifact url scheme '{}'", url.scheme())));
    }
    if url.host_str() != Some(ARTIFACT_HOST) {
        return Err(invalid(format!("artifact url host {:?}", url.host_str())));
    }
    if !matches!(url.port(), None | Some(443)) {
        return Err(invalid(format!("artifact url port {:?}", url.port())));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("artifact url must not carry query/fragment".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("artifact url must not carry credentials".into()));
    }
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    let [seg_libra, seg_releases, seg_tag, seg_artifact] = segments.as_slice() else {
        return Err(invalid(format!(
            "artifact url path must have exactly 4 segments, got {}",
            segments.len()
        )));
    };
    if *seg_libra != "libra" || *seg_releases != "releases" {
        return Err(invalid(
            "artifact url path must start /libra/releases".into(),
        ));
    }
    let Some(tag) = seg_tag.strip_prefix('v') else {
        return Err(invalid(format!(
            "release tag segment '{seg_tag}' lacks 'v'"
        )));
    };
    if tag != version_raw {
        return Err(invalid(format!(
            "url tag 'v{tag}' does not match payload version '{version_raw}'"
        )));
    }
    let Some(url_platform) = seg_artifact.strip_prefix("libra-") else {
        return Err(invalid(format!(
            "artifact segment '{seg_artifact}' lacks the 'libra-' prefix"
        )));
    };
    // §A.1/§A.6: Windows `.exe` suffix is an R0 follow-up; auto-upgrade never
    // enters the Windows download path, so the strict grammar (no suffix)
    // stands for every platform in R0.
    if url_platform != platform.as_str() {
        return Err(invalid(format!(
            "url platform '{url_platform}' does not match artifact platform '{platform}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic test keypair (generated once; private half embedded ONLY
    // in tests to sign fixtures).
    fn test_keypair() -> ring::signature::Ed25519KeyPair {
        // INVARIANT: fixed seed is valid 32 bytes, generation cannot fail.
        ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7u8; 32]).unwrap()
    }

    fn test_trust(generation: u32) -> Vec<TrustedKey> {
        use ring::signature::KeyPair;
        let pk: [u8; 32] = test_keypair().public_key().as_ref().try_into().unwrap();
        vec![TrustedKey {
            key_id: "test-key-1",
            ed25519_pubkey: pk,
            not_before: 0,
            not_after: 4102444800, // 2100-01-01
            generation,
        }]
    }

    fn artifact_json(platform: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "platform": platform,
            "url": format!("https://download.libra.tools/libra/releases/v{version}/libra-{platform}"),
            "sha256": "a".repeat(64),
            "size": 1024,
        })
    }

    fn payload_json(version: &str) -> serde_json::Value {
        serde_json::json!({
            "channel": "stable",
            "version": version,
            "control_revision": 5,
            "published_at": "2026-07-01T00:00:00Z",
            "expires_at": "2026-09-29T00:00:00Z",
            "min_key_generation": 1,
            "paused": false,
            "revoked_versions": [],
            "artifacts": [
                artifact_json("linux-amd64", version),
                artifact_json("linux-arm64", version),
                artifact_json("darwin-arm64", version),
                artifact_json("windows-amd64", version),
            ],
        })
    }

    fn envelope_for(payload: &serde_json::Value) -> Vec<u8> {
        let payload_bytes = serde_json::to_vec(payload).unwrap();
        let mut message = SIGNATURE_DOMAIN_PREFIX.to_vec();
        message.extend_from_slice(&payload_bytes);
        let sig = test_keypair().sign(&message);
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "payload": base64::engine::general_purpose::STANDARD.encode(&payload_bytes),
            "signatures": [{
                "key_id": "test-key-1",
                "signature": base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
            }],
        }))
        .unwrap()
    }

    #[test]
    fn valid_envelope_verifies_end_to_end() {
        let envelope = envelope_for(&payload_json("1.2.3"));
        let manifest = verify_envelope_bytes(&envelope, &test_trust(1)).unwrap();
        assert_eq!(manifest.version, ReleaseVersion(1, 2, 3));
        assert_eq!(manifest.signer_key_id, "test-key-1");
        assert_eq!(manifest.control_revision, 5);
        assert_eq!(manifest.artifacts.len(), 4);
        assert!(!manifest.paused);
    }

    #[test]
    fn empty_trust_table_fails_closed() {
        let envelope = envelope_for(&payload_json("1.2.3"));
        assert!(matches!(
            verify_envelope_bytes(&envelope, &[]),
            Err(ManifestError::NoTrustedSignature)
        ));
    }

    #[test]
    fn tampered_payload_fails_signature() {
        let payload_bytes = serde_json::to_vec(&payload_json("1.2.3")).unwrap();
        let mut message = SIGNATURE_DOMAIN_PREFIX.to_vec();
        message.extend_from_slice(&payload_bytes);
        let sig = test_keypair().sign(&message);
        // Sign 1.2.3 but ship 9.9.9 in the payload.
        let tampered = serde_json::to_vec(&payload_json("9.9.9")).unwrap();
        let envelope = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "payload": base64::engine::general_purpose::STANDARD.encode(&tampered),
            "signatures": [{
                "key_id": "test-key-1",
                "signature": base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
            }],
        }))
        .unwrap();
        assert!(matches!(
            verify_envelope_bytes(&envelope, &test_trust(1)),
            Err(ManifestError::NoTrustedSignature)
        ));
    }

    #[test]
    fn duplicate_key_id_rejected_before_crypto() {
        let payload = payload_json("1.2.3");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let envelope = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "payload": base64::engine::general_purpose::STANDARD.encode(&payload_bytes),
            "signatures": [
                {"key_id": "test-key-1", "signature": "AAAA"},
                {"key_id": "test-key-1", "signature": "BBBB"},
            ],
        }))
        .unwrap();
        assert!(matches!(
            verify_envelope_bytes(&envelope, &test_trust(1)),
            Err(ManifestError::DuplicateKeyId(_))
        ));
    }

    #[test]
    fn generation_floor_from_manifest_rejects_old_key() {
        let mut payload = payload_json("1.2.3");
        payload["min_key_generation"] = serde_json::json!(2);
        let envelope = envelope_for(&payload);
        // Trust table key has generation 1 < manifest floor 2.
        assert!(matches!(
            verify_envelope_bytes(&envelope, &test_trust(1)),
            Err(ManifestError::KeyGenerationBelowFloor { floor: 2, .. })
        ));
        // A generation-2 key passes.
        assert!(verify_envelope_bytes(&envelope, &test_trust(2)).is_ok());
    }

    #[test]
    fn key_window_must_cover_signed_lifetime() {
        let envelope = envelope_for(&payload_json("1.2.3"));
        let mut trust = test_trust(1);
        trust[0].not_after = 0; // window ends before published_at
        assert!(matches!(
            verify_envelope_bytes(&envelope, &trust),
            Err(ManifestError::KeyWindowMismatch)
        ));
    }

    type UrlMutation = fn(&mut serde_json::Value);

    #[test]
    fn url_grammar_is_strict() {
        let cases: [(UrlMutation, &str); 7] = [
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "http://download.libra.tools/libra/releases/v1.2.3/libra-linux-amd64"
                    )
                },
                "http scheme",
            ),
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "https://evil.example.com/libra/releases/v1.2.3/libra-linux-amd64"
                    )
                },
                "wrong host",
            ),
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "https://download.libra.tools:8443/libra/releases/v1.2.3/libra-linux-amd64"
                    )
                },
                "non-443 port",
            ),
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "https://download.libra.tools/libra/releases/v1.2.3/libra-linux-amd64?x=1"
                    )
                },
                "query",
            ),
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "https://download.libra.tools/libra/releases/v9.9.9/libra-linux-amd64"
                    )
                },
                "tag != version",
            ),
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "https://download.libra.tools/libra/releases/v1.2.3/libra-linux-arm64"
                    )
                },
                "url platform != artifact platform",
            ),
            (
                |p| {
                    p["artifacts"][0]["url"] = serde_json::json!(
                        "https://download.libra.tools/extra/libra/releases/v1.2.3/libra-linux-amd64"
                    )
                },
                "5 path segments",
            ),
        ];
        for (mutate, expect) in cases {
            let mut payload = payload_json("1.2.3");
            mutate(&mut payload);
            let envelope = envelope_for(&payload);
            assert!(
                matches!(
                    verify_envelope_bytes(&envelope, &test_trust(1)),
                    Err(ManifestError::PayloadInvalid(_))
                ),
                "expected rejection for: {expect}"
            );
        }
    }

    #[test]
    fn payload_semantics_are_strict() {
        // channel
        let mut p = payload_json("1.2.3");
        p["channel"] = serde_json::json!("beta");
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // prerelease version
        let p = payload_json("1.2.3-rc1");
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // missing platform
        let mut p = payload_json("1.2.3");
        p["artifacts"].as_array_mut().unwrap().pop();
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // duplicate platform
        let mut p = payload_json("1.2.3");
        let dup = p["artifacts"][0].clone();
        p["artifacts"].as_array_mut().unwrap().push(dup);
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // zero size
        let mut p = payload_json("1.2.3");
        p["artifacts"][0]["size"] = serde_json::json!(0);
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // oversize
        let mut p = payload_json("1.2.3");
        p["artifacts"][0]["size"] = serde_json::json!(MAX_ARTIFACT_BYTES + 1);
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // expires before published
        let mut p = payload_json("1.2.3");
        p["expires_at"] = serde_json::json!("2026-06-01T00:00:00Z");
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
        // revoked list must parse
        let mut p = payload_json("1.2.3");
        p["revoked_versions"] = serde_json::json!(["not-a-version"]);
        assert!(verify_envelope_bytes(&envelope_for(&p), &test_trust(1)).is_err());
    }

    #[test]
    fn release_version_parser_is_strict() {
        assert_eq!(
            ReleaseVersion::parse("1.2.3"),
            Some(ReleaseVersion(1, 2, 3))
        );
        assert_eq!(
            ReleaseVersion::parse("0.18.94"),
            Some(ReleaseVersion(0, 18, 94))
        );
        for bad in [
            "v1.2.3",
            "1.2",
            "1.2.3.4",
            "1.2.03",
            "1.2.3-rc1",
            "1.2.3+b",
            " 1.2.3",
            "",
        ] {
            assert_eq!(ReleaseVersion::parse(bad), None, "{bad}");
        }
        assert!(ReleaseVersion(1, 2, 10) > ReleaseVersion(1, 2, 9));
        assert!(ReleaseVersion(2, 0, 0) > ReleaseVersion(1, 99, 99));
    }

    #[test]
    fn oversized_manifest_rejected() {
        let big = vec![b'x'; MAX_MANIFEST_BYTES + 1];
        assert!(matches!(
            verify_envelope_bytes(&big, &test_trust(1)),
            Err(ManifestError::TooLarge)
        ));
    }
}
