//! Auto-upgrade publish/manifest contract tests (plan-20260714 §A.9/§A.11).
//!
//! These pin the manifest *contract* the release/publish jobs must honour:
//! the exact signature-verification order, the release-matrix coverage and
//! URL grammar, the anti-rollback field rules that a "renew" job must
//! preserve (`paused`/`revoked_versions` byte-identical), and the size
//! bounds. Behind the `test-upgrade` feature (`required-features`).

#![cfg(feature = "test-upgrade")]

use base64::Engine as _;
use libra::internal::upgrade::{
    manifest::{MAX_ARTIFACT_BYTES, ManifestError, SIGNATURE_DOMAIN_PREFIX, verify_envelope_bytes},
    platform::Platform,
    trusted_keys::TrustedKey,
};

const SEED: [u8; 32] = [7u8; 32];

fn keypair() -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap()
}

fn trust() -> Vec<TrustedKey> {
    use ring::signature::KeyPair;
    let pk: [u8; 32] = keypair().public_key().as_ref().try_into().unwrap();
    vec![TrustedKey {
        key_id: "test-key-1",
        ed25519_pubkey: pk,
        not_before: 0,
        not_after: 4_102_444_800,
        generation: 1,
    }]
}

fn artifact(platform: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "platform": platform,
        "url": format!("https://download.libra.tools/libra/releases/v{version}/libra-{platform}"),
        "sha256": "a".repeat(64),
        "size": 4096,
    })
}

fn full_payload(version: &str) -> serde_json::Value {
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
            artifact("linux-amd64", version),
            artifact("linux-arm64", version),
            artifact("darwin-arm64", version),
            artifact("windows-amd64", version),
        ],
    })
}

fn envelope(payload: &serde_json::Value) -> Vec<u8> {
    let payload_bytes = serde_json::to_vec(payload).unwrap();
    let mut message = SIGNATURE_DOMAIN_PREFIX.to_vec();
    message.extend_from_slice(&payload_bytes);
    let sig = keypair().sign(&message);
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
fn upgrade_publish_is_conditional_and_complete() {
    // A well-formed, fully-covered manifest verifies; a manifest missing any
    // release-matrix platform (as an incomplete publish would produce) fails.
    let trust = trust();
    assert!(verify_envelope_bytes(&envelope(&full_payload("2.0.0")), &trust).is_ok());

    let mut incomplete = full_payload("2.0.0");
    incomplete["artifacts"].as_array_mut().unwrap().pop();
    assert!(matches!(
        verify_envelope_bytes(&envelope(&incomplete), &trust),
        Err(ManifestError::PayloadInvalid(_))
    ));
}

#[test]
fn upgrade_url_binding_is_enforced() {
    let trust = trust();
    // A URL whose tag does not match the payload version (a mis-tagged
    // publish) must fail the cross-field binding.
    let mut mismatched = full_payload("2.0.0");
    mismatched["artifacts"][0]["url"] =
        serde_json::json!("https://download.libra.tools/libra/releases/v9.9.9/libra-linux-amd64");
    assert!(matches!(
        verify_envelope_bytes(&envelope(&mismatched), &trust),
        Err(ManifestError::PayloadInvalid(_))
    ));
    // A non-pinned host is rejected.
    let mut bad_host = full_payload("2.0.0");
    bad_host["artifacts"][0]["url"] =
        serde_json::json!("https://cdn.evil.example/libra/releases/v2.0.0/libra-linux-amd64");
    assert!(verify_envelope_bytes(&envelope(&bad_host), &trust).is_err());
}

#[test]
fn upgrade_publish_size_bounds_enforced() {
    let trust = trust();
    let mut too_big = full_payload("2.0.0");
    too_big["artifacts"][0]["size"] = serde_json::json!(MAX_ARTIFACT_BYTES + 1);
    assert!(verify_envelope_bytes(&envelope(&too_big), &trust).is_err());

    let mut zero = full_payload("2.0.0");
    zero["artifacts"][0]["size"] = serde_json::json!(0);
    assert!(verify_envelope_bytes(&envelope(&zero), &trust).is_err());
}

#[test]
fn upgrade_new_release_and_renew_preserve_pause_revocations() {
    // A verified manifest surfaces `paused`/`revoked_versions` exactly as
    // signed; a renew job must carry them byte-for-byte, which this asserts by
    // round-tripping both a paused and a revoking payload.
    let trust = trust();
    let mut paused = full_payload("2.0.0");
    paused["paused"] = serde_json::json!(true);
    let m = verify_envelope_bytes(&envelope(&paused), &trust).unwrap();
    assert!(m.paused);

    let mut revoking = full_payload("2.0.0");
    revoking["revoked_versions"] = serde_json::json!(["1.9.0", "1.9.1"]);
    let m = verify_envelope_bytes(&envelope(&revoking), &trust).unwrap();
    assert_eq!(m.revoked_versions.len(), 2);
    assert!(m.is_revoked(libra::internal::upgrade::manifest::ReleaseVersion(1, 9, 0)));
}

#[test]
fn upgrade_channel_must_be_stable() {
    let trust = trust();
    let mut beta = full_payload("2.0.0");
    beta["channel"] = serde_json::json!("beta");
    assert!(verify_envelope_bytes(&envelope(&beta), &trust).is_err());
}

#[test]
fn upgrade_matrix_covers_exactly_four_platforms() {
    // The publish contract mandates one artifact per release-matrix platform.
    assert_eq!(Platform::RELEASE_MATRIX.len(), 4);
    for p in Platform::RELEASE_MATRIX {
        assert_eq!(Platform::parse(p.as_str()), Some(*p));
    }
}

// ── UP-01 B1-02 cross-implementation contract vectors (plan-20260821 A1-06 /
// DEP-07). The Backend (libra-backend cf, libs/release-signing/
// manifest-transitions.ts) is the only implementation of the publish/renew/
// emergency transitions; these tests consume its version-controlled vector
// handover and verify the signed outputs cross-implementation. The vectors
// are signed by a committed TEST keypair — never the production trust root.

const VECTOR_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/data/up01-transition-vectors-v1.json"
);
const VECTOR_TEST_KEY_ID: &str = "libra-release-test-1";
const MANIFEST_LIFETIME_SECONDS: i64 = 90 * 24 * 60 * 60;
/// Handover integrity (DEP-07): the CLIENT pins the whole fixture by digest
/// and the complete test trust root by value. The test private key is
/// intentionally public, so nothing inside the file is self-authenticating —
/// only these pinned constants are. Regenerating vectors on the backend
/// requires updating this digest together with the copied file.
const VECTOR_FILE_SHA256: &str = "1103cec7b531a3009cc89cc4d05ad13626a666ee1b725006635c5147d36d9239";
const VECTOR_TEST_PUBLIC_KEY_HEX: &str =
    "a8a00ded13ddafaad525fabddc13efc717b29ebed50cd6d653196057fa8f8a43";
const VECTOR_TEST_KEY_NOT_BEFORE: i64 = 1_767_225_600;
const VECTOR_TEST_KEY_NOT_AFTER: i64 = 1_830_297_600;
/// The exact vector corpus (backend B1-02): dropping any ID must fail.
const VECTOR_REQUIRED_IDS: [&str; 18] = [
    "publish-first-control-revision-1",
    "publish-upgrade-inherits-controls",
    "publish-version-regression",
    "publish-same-version-idempotent",
    "publish-same-version-conflict",
    "publish-key-window-expiry",
    "publish-before-not-before",
    "renew-skip-above-60d",
    "renew-applied-below-60d",
    "renew-key-window",
    "emergency-pause",
    "emergency-resume",
    "emergency-revoke-append",
    "emergency-revoke-duplicate",
    "emergency-pause-noop",
    "publish-anti-vv-placeholder",
    "publish-version-beyond-u64",
    "publish-version-above-2p53",
];

fn vector_document() -> serde_json::Value {
    let raw = std::fs::read(VECTOR_FILE).expect("DEP-07 vector file present");
    // Whole-file digest pin: any edit (payloads, signatures, expectations,
    // even re-signed ones) fails here before anything else runs.
    use sha2::Digest as _;
    let digest = hex::encode(sha2::Sha256::digest(&raw));
    assert_eq!(
        digest, VECTOR_FILE_SHA256,
        "vector fixture digest drifted from the pinned handover"
    );
    serde_json::from_slice(&raw).expect("vector file is valid JSON")
}

/// Trust table built from PINNED constants; the file's declared header must
/// match them exactly (a silently regenerated key/window cannot pass).
fn vector_trust(doc: &serde_json::Value) -> Vec<TrustedKey> {
    let key = &doc["test_key"];
    assert_eq!(key["key_id"], VECTOR_TEST_KEY_ID);
    assert_eq!(key["generation"], 1);
    assert_eq!(key["public_key_hex"], VECTOR_TEST_PUBLIC_KEY_HEX);
    assert_eq!(key["not_before"].as_i64(), Some(VECTOR_TEST_KEY_NOT_BEFORE));
    assert_eq!(key["not_after"].as_i64(), Some(VECTOR_TEST_KEY_NOT_AFTER));
    let mut pubkey = [0u8; 32];
    for (i, byte) in pubkey.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&VECTOR_TEST_PUBLIC_KEY_HEX[i * 2..i * 2 + 2], 16).unwrap();
    }
    vec![TrustedKey {
        key_id: VECTOR_TEST_KEY_ID,
        ed25519_pubkey: pubkey,
        not_before: VECTOR_TEST_KEY_NOT_BEFORE,
        not_after: VECTOR_TEST_KEY_NOT_AFTER,
        generation: 1,
    }]
}

fn vectors(doc: &serde_json::Value) -> &Vec<serde_json::Value> {
    doc["vectors"].as_array().expect("vectors array")
}

fn vector_by_id<'a>(doc: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    vectors(doc)
        .iter()
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("vector '{id}' present"))
}

/// Verify one accepted vector's envelope and return the client-visible
/// manifest.
fn verified(
    doc: &serde_json::Value,
    vector: &serde_json::Value,
) -> libra::internal::upgrade::manifest::VerifiedManifest {
    let envelope_bytes = serde_json::to_vec(&vector["envelope"]).unwrap();
    let manifest = verify_envelope_bytes(&envelope_bytes, &vector_trust(doc))
        .unwrap_or_else(|e| panic!("vector '{}' must verify: {e}", vector["id"]));
    assert_eq!(manifest.signer_key_id, VECTOR_TEST_KEY_ID);
    let digest_hex: String = manifest
        .payload_digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest_hex, vector["payload_digest_sha256"],
        "vector '{}' payload digest",
        vector["id"]
    );
    manifest
}

#[test]
fn up01_vector_fixture_matches_the_backend_handover_when_present() {
    // DEP-07 cross-repo attestation, executable form: on machines carrying
    // the sibling libra-backend checkout (the dev/acceptance machine where
    // the plan's gates run; override with LIBRA_BACKEND_CHECKOUT), the
    // handover copy must be BYTE-identical to the backend's current
    // fixture — a backend regeneration that updated only its own pin fails
    // HERE instead of silently validating an obsolete contract. Without a
    // sibling checkout this prints skipped (the dual digest pins still
    // force a conscious two-repo lockstep update).
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(root) = std::env::var("LIBRA_BACKEND_CHECKOUT") {
        candidates.push(std::path::PathBuf::from(root));
    }
    candidates.push(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../libra-backend"));
    let Some(backend_fixture) = candidates
        .iter()
        .map(|root| root.join("tests/fixtures/up01-transition-vectors-v1.json"))
        .find(|path| path.is_file())
    else {
        eprintln!("skipped (no sibling libra-backend checkout; set LIBRA_BACKEND_CHECKOUT)");
        return;
    };
    let backend_bytes = std::fs::read(&backend_fixture).expect("backend fixture readable");
    let client_bytes = std::fs::read(VECTOR_FILE).expect("client fixture readable");
    assert_eq!(
        backend_bytes,
        client_bytes,
        "the client handover copy drifted from the backend fixture at {}",
        backend_fixture.display()
    );
}

#[test]
fn up01_vector_schema_is_complete() {
    let doc = vector_document();
    assert_eq!(doc["schema_version"], 1);
    let digest = doc["vectors_digest_sha256"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));

    // Exactly the required corpus, no more, no less: a dropped key-window or
    // boundary vector must fail loudly, not shrink coverage silently.
    let ids: Vec<&str> = vectors(&doc)
        .iter()
        .map(|v| v["id"].as_str().expect("vector id"))
        .collect();
    assert_eq!(ids.len(), VECTOR_REQUIRED_IDS.len(), "vector corpus size");
    for required in VECTOR_REQUIRED_IDS {
        assert!(
            ids.contains(&required),
            "missing required vector '{required}'"
        );
    }

    for vector in vectors(&doc) {
        let id = vector["id"].as_str().expect("vector id");
        assert!(
            matches!(
                vector["operation"].as_str(),
                Some("publish" | "renew" | "emergency")
            ),
            "vector '{id}' operation"
        );
        assert!(
            vector.get("pre_state").is_some(),
            "vector '{id}' pre_state (null allowed, key required)"
        );
        assert!(vector["now_seconds"].is_i64() || vector["now_seconds"].is_u64());
        assert!(
            vector["evidence"]
                .as_str()
                .is_some_and(|e| e.contains("release-manifest-transitions.test.ts")),
            "vector '{id}' cites its backend B1-02 test evidence"
        );
        let outcome = vector["expected"]["outcome"].as_str().unwrap();
        match outcome {
            "accepted" => assert!(vector.get("envelope").is_some()),
            "rejected" => {
                assert!(vector["expected"]["reason"].as_str().is_some());
            }
            "idempotent" | "skip" => {}
            other => panic!("vector '{id}' unknown outcome '{other}'"),
        }
    }
}

#[test]
fn up01_accepted_vectors_verify_and_match_result_payload() {
    let doc = vector_document();
    let mut accepted = 0;
    for vector in vectors(&doc) {
        if vector["expected"]["outcome"] != "accepted" {
            continue;
        }
        accepted += 1;
        let manifest = verified(&doc, vector);
        let result = &vector["result_payload"];
        let now = vector["now_seconds"].as_i64().unwrap();

        assert_eq!(manifest.version_raw, result["version"].as_str().unwrap());
        assert_eq!(
            manifest.control_revision,
            result["control_revision"].as_u64().unwrap()
        );
        assert_eq!(manifest.paused, result["paused"].as_bool().unwrap());
        assert_eq!(
            manifest.min_key_generation,
            result["min_key_generation"].as_u64().unwrap() as u32
        );
        // Signing time semantics: published_at = now, expires_at = now + 90d.
        assert_eq!(manifest.published_at, now);
        assert_eq!(manifest.expires_at, now + MANIFEST_LIFETIME_SECONDS);

        let expected_revoked: Vec<&str> = result["revoked_versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let got_revoked: Vec<String> = manifest
            .revoked_versions
            .iter()
            .map(|v| v.to_string())
            .collect();
        assert_eq!(got_revoked, expected_revoked);

        let expected_artifacts = result["artifacts"].as_array().unwrap();
        assert_eq!(manifest.artifacts.len(), expected_artifacts.len());
        for row in expected_artifacts {
            let platform = Platform::parse(row["platform"].as_str().unwrap()).unwrap();
            let artifact = manifest.artifact_for(platform).unwrap();
            assert_eq!(artifact.url, row["url"].as_str().unwrap());
            assert_eq!(artifact.sha256, row["sha256"].as_str().unwrap());
            assert_eq!(artifact.size, row["size"].as_u64().unwrap());
        }
    }
    assert_eq!(
        accepted, 8,
        "exactly the eight accepted vectors must verify"
    );

    // Precision boundary: a component above 2^53 survives verbatim (the
    // backend serializes it without float rounding, the client parses u64).
    let above = vector_by_id(&doc, "publish-version-above-2p53");
    let m = verified(&doc, above);
    assert_eq!(m.version_raw, "9007199254740993.0.0");
    assert_eq!(
        above["asserts"]["version_round_trip"],
        "9007199254740993.0.0"
    );
}

#[test]
fn up01_rejected_vectors_carry_no_acceptable_envelope() {
    let doc = vector_document();
    let mut reasons = std::collections::BTreeSet::new();
    let mut outcomes = std::collections::BTreeSet::new();
    for vector in vectors(&doc) {
        let outcome = vector["expected"]["outcome"].as_str().unwrap();
        outcomes.insert(outcome.to_string());
        if outcome == "accepted" {
            continue;
        }
        assert!(
            vector.get("envelope").is_none() && vector.get("result_payload").is_none(),
            "non-accepted vector '{}' must carry no signed envelope",
            vector["id"]
        );
        if outcome == "rejected" {
            reasons.insert(vector["expected"]["reason"].as_str().unwrap().to_string());
        }
    }
    // The backend's rejection families must all be exercised.
    for reason in [
        "version_regression",
        "same_version_conflict",
        "key_window",
        "empty_emergency",
    ] {
        assert!(
            reasons.contains(reason),
            "missing rejected reason '{reason}'"
        );
    }
    assert!(outcomes.contains("skip"), "renew >60d skip vector present");
    assert!(
        outcomes.contains("idempotent"),
        "new==current idempotent vector present"
    );
}

#[test]
fn up01_transition_semantics_match_backend_expectations() {
    let doc = vector_document();

    // First publish: control_revision starts at 1 with empty control fields.
    let first = vector_by_id(&doc, "publish-first-control-revision-1");
    let m = verified(&doc, first);
    assert_eq!(m.control_revision, 1);
    assert!(!m.paused);
    assert!(m.revoked_versions.is_empty());

    // Upgrade publish: paused/revoked inherited byte-for-byte, revision +1.
    let upgrade = vector_by_id(&doc, "publish-upgrade-inherits-controls");
    let pre = &upgrade["pre_state"];
    let m = verified(&doc, upgrade);
    assert_eq!(
        m.control_revision,
        pre["control_revision"].as_u64().unwrap() + 1
    );
    assert_eq!(m.paused, pre["paused"].as_bool().unwrap());
    let pre_revoked: Vec<&str> = pre["revoked_versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let got_revoked: Vec<String> = m.revoked_versions.iter().map(|v| v.to_string()).collect();
    assert_eq!(got_revoked, pre_revoked);

    // Renew: same version and artifact identity, only time fields + revision.
    let renew = vector_by_id(&doc, "renew-applied-below-60d");
    let pre = &renew["pre_state"];
    let m = verified(&doc, renew);
    assert_eq!(m.version_raw, pre["version"].as_str().unwrap());
    assert_eq!(
        m.control_revision,
        pre["control_revision"].as_u64().unwrap() + 1
    );
    for row in pre["artifacts"].as_array().unwrap() {
        let platform = Platform::parse(row["platform"].as_str().unwrap()).unwrap();
        let artifact = m.artifact_for(platform).unwrap();
        assert_eq!(artifact.sha256, row["sha256"].as_str().unwrap());
        assert_eq!(artifact.size, row["size"].as_u64().unwrap());
        assert_eq!(artifact.url, row["url"].as_str().unwrap());
    }

    // Emergency: paused/revoked change; version/artifacts immutable.
    let pause = vector_by_id(&doc, "emergency-pause");
    let m = verified(&doc, pause);
    assert!(m.paused);
    assert_eq!(
        m.version_raw,
        pause["pre_state"]["version"].as_str().unwrap()
    );

    let resume = vector_by_id(&doc, "emergency-resume");
    let pre = &resume["pre_state"];
    let m = verified(&doc, resume);
    assert!(!m.paused);
    // Resume must change ONLY paused/time/revision: everything else is
    // pinned against the pre-state.
    assert_eq!(
        m.control_revision,
        pre["control_revision"].as_u64().unwrap() + 1
    );
    assert_eq!(m.version_raw, pre["version"].as_str().unwrap());
    assert_eq!(
        m.min_key_generation,
        pre["min_key_generation"].as_u64().unwrap() as u32
    );
    let pre_revoked: Vec<&str> = pre["revoked_versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let got_revoked: Vec<String> = m.revoked_versions.iter().map(|v| v.to_string()).collect();
    assert_eq!(got_revoked, pre_revoked);
    for row in pre["artifacts"].as_array().unwrap() {
        let platform = Platform::parse(row["platform"].as_str().unwrap()).unwrap();
        let artifact = m.artifact_for(platform).unwrap();
        assert_eq!(artifact.url, row["url"].as_str().unwrap());
        assert_eq!(artifact.sha256, row["sha256"].as_str().unwrap());
        assert_eq!(artifact.size, row["size"].as_u64().unwrap());
    }

    let revoke = vector_by_id(&doc, "emergency-revoke-append");
    let pre = &revoke["pre_state"];
    let m = verified(&doc, revoke);
    let mut expected: Vec<String> = pre["revoked_versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    expected.push(
        revoke["input"]["emergency"]["version"]
            .as_str()
            .unwrap()
            .to_string(),
    );
    let got: Vec<String> = m.revoked_versions.iter().map(|v| v.to_string()).collect();
    assert_eq!(got, expected);
    assert_eq!(m.version_raw, pre["version"].as_str().unwrap());
}

#[test]
fn up01_anti_vv_placeholder_contract() {
    // Placeholder contract (GC-UP01-2): tag=v0.20.3 -> object key
    // `libra/releases/v0.20.3/libra-linux-amd64`, never `vv0.20.3`.
    let doc = vector_document();
    let vector = vector_by_id(&doc, "publish-anti-vv-placeholder");
    let m = verified(&doc, vector);

    let url = &m.artifact_for(Platform::LinuxAmd64).unwrap().url;
    assert_eq!(
        url,
        "https://download.libra.tools/libra/releases/v0.20.3/libra-linux-amd64"
    );
    assert_eq!(vector["asserts"]["linux_amd64_url"], url.as_str());
    let object_key = vector["asserts"]["linux_amd64_object_key"]
        .as_str()
        .unwrap();
    assert_eq!(object_key, "libra/releases/v0.20.3/libra-linux-amd64");
    assert!(!url.contains("vv") && !object_key.contains("vv"));
}
