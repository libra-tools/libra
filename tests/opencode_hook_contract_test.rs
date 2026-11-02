//! OG-00/01 pinned source, fixture, registry and parser conformance.

use std::collections::BTreeSet;

use libra::internal::ai::hooks::{
    SessionHookEnvelope,
    providers::{
        opencode::events::{
            OPENCODE_BUILD_ID, OPENCODE_COMMIT, OPENCODE_DEPRECATED_ALIASES,
            OPENCODE_FILTERED_STATUSES, OPENCODE_HOOK_EVENT_SPECS, OPENCODE_IDLE_STATUS,
            OPENCODE_LEGACY_EVENT_SPECS, OPENCODE_OFFICIAL_BASE_PIN, OPENCODE_PIN,
            OPENCODE_TERMINAL_INTERRUPTION_REASONS, OpenCodeForwarding,
        },
        opencode_provider,
    },
};
use serde_json::Value;

#[cfg(unix)]
mod harness;

const FIXTURE: &str = include_str!("fixtures/agent_transcripts/opencode/hook_events.json");
const MANIFEST: &str = include_str!("fixtures/agent_transcripts/opencode/MANIFEST.md");
const PLAN: &str = include_str!("../docs/development/plan/plan-20260902.md");

#[test]
fn opencode_fixture_additions_preserve_rg_owned_cases() {
    use sha2::{Digest, Sha256};

    let frozen: [(&str, &[u8], &str); 5] = [
        (
            "provider_visible",
            include_bytes!("fixtures/agent_transcripts/reasoning/provider_visible.json"),
            "fb45309563ed1427b11dcc277470de42e7ee1c7a9a2d844c6aacd1104b1a80f9",
        ),
        (
            "not_present",
            include_bytes!("fixtures/agent_transcripts/reasoning/not_present.json"),
            "42ca3d234ca56f3d8389ff6bf0bbdba41efc698e68fd3273fb465e2e88057a62",
        ),
        (
            "opencode",
            include_bytes!("fixtures/agent_transcripts/reasoning/opencode.json"),
            "4750cff90cead1a31f79bc484bc0df180640c31946aa413b049250d2b410af8e",
        ),
        (
            "generic_encrypted",
            include_bytes!("fixtures/agent_transcripts/reasoning/generic_encrypted.json"),
            "4f7a60ed6186417e3e460c03b6505c53a77030e87ccd245aae1e0ddba2183e57",
        ),
        (
            "contract",
            include_bytes!("fixtures/agent_transcripts/reasoning/contract.snap"),
            "1e83e39d582c76d3b297ab8252c109a11eadaed40f2e6d299d221ae750b1b665",
        ),
    ];
    for (name, bytes, expected) in frozen {
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            expected,
            "RG fixture {name}"
        );
    }
    let og25 = include_bytes!("fixtures/agent_transcripts/reasoning/opencode_og25.json");
    assert_eq!(
        hex::encode(Sha256::digest(og25)),
        "1d1aed5a6e1f9df6c1d2e0e7fea3039148af4cf77a744fe4966f16a24e4d0004"
    );
    let parsed: Value = serde_json::from_slice(og25).expect("synthetic fixture JSON");
    assert_eq!(parsed["info"]["id"], "ses_og25_synthetic");
    assert_eq!(parsed["messages"][1]["content"][0]["type"], "reasoning");
}

fn source_record() -> &'static str {
    let start = "\n<!-- OG-00-PROBE-START -->\n";
    let end = "\n<!-- OG-00-PROBE-END -->\n";
    let starts: Vec<_> = PLAN.match_indices(start).collect();
    let ends: Vec<_> = PLAN.match_indices(end).collect();
    assert_eq!(
        starts.len(),
        1,
        "exactly one standalone source start marker"
    );
    assert_eq!(ends.len(), 1, "exactly one standalone source end marker");
    let begin = starts[0].0 + start.len();
    let finish = ends[0].0;
    assert!(
        finish > begin,
        "source markers must be ordered and non-empty"
    );
    &PLAN[begin..finish]
}

fn declared<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .collect()
}

#[test]
fn source_record_matches_fixture_manifest() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("valid synthetic fixture");
    for document in [source_record(), MANIFEST] {
        assert_eq!(
            declared(document, "opencode_pin: "),
            [OPENCODE_OFFICIAL_BASE_PIN]
        );
        assert_eq!(declared(document, "opencode_commit: "), [OPENCODE_COMMIT]);
        for stale in ["1.17.13", "1.18.29"] {
            assert!(!document.contains(stale), "mixed pin {stale}");
        }
    }
    assert_eq!(fixture["opencode_pin"], OPENCODE_OFFICIAL_BASE_PIN);
    assert_eq!(fixture["opencode_commit"], OPENCODE_COMMIT);
    assert_eq!(fixture["payload_member"], "data");
    for (prefix, specs, forwarding) in [
        (
            "standalone_events: ",
            OPENCODE_HOOK_EVENT_SPECS,
            OpenCodeForwarding::Standalone,
        ),
        (
            "plugin_hook_events: ",
            OPENCODE_HOOK_EVENT_SPECS,
            OpenCodeForwarding::PluginHook,
        ),
        (
            "plugin_merged_events: ",
            OPENCODE_HOOK_EVENT_SPECS,
            OpenCodeForwarding::PluginMerged,
        ),
        (
            "compatibility_events: ",
            OPENCODE_LEGACY_EVENT_SPECS,
            OpenCodeForwarding::LegacyCompatibility,
        ),
        (
            "deprecated_aliases: ",
            OPENCODE_DEPRECATED_ALIASES,
            OpenCodeForwarding::DeprecatedAlias,
        ),
    ] {
        let names = specs
            .iter()
            .filter(|spec| spec.forwarding == forwarding)
            .map(|spec| spec.name)
            .collect::<Vec<_>>()
            .join(", ");
        for document in [source_record(), MANIFEST] {
            assert_eq!(declared(document, prefix), [names.as_str()]);
        }
    }
    for (prefix, names) in [
        (
            "terminal_interruption_reasons: ",
            OPENCODE_TERMINAL_INTERRUPTION_REASONS,
        ),
        ("status_filtered: ", OPENCODE_FILTERED_STATUSES),
    ] {
        let expected = names.join(", ");
        for document in [source_record(), MANIFEST] {
            assert_eq!(declared(document, prefix), [expected.as_str()]);
        }
    }
    assert_eq!(
        fixture["terminal_interruption_reasons"],
        serde_json::json!(OPENCODE_TERMINAL_INTERRUPTION_REASONS)
    );
    assert_eq!(
        fixture["status_filtered"],
        serde_json::json!(OPENCODE_FILTERED_STATUSES)
    );
    assert_eq!(fixture["idle_status"], OPENCODE_IDLE_STATUS);
}

#[test]
fn fixture_contains_no_real_content() {
    let forbidden =
        regex::Regex::new(r"sk-[a-zA-Z0-9]|ANTHROPIC_API_KEY|OPENAI_API_KEY|/Users/|/home/")
            .expect("fixed privacy guard");
    for document in [source_record(), MANIFEST, FIXTURE] {
        assert!(!forbidden.is_match(document));
    }
}

#[test]
fn registry_matches_fixture_manifest() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture");
    let expected = fixture["events"].as_array().expect("event list");
    let advertised: Vec<_> = OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .filter(|spec| spec.kind.is_some())
        .collect();
    assert_eq!(advertised.len(), expected.len());
    let mut unique = BTreeSet::new();
    for spec in OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .chain(OPENCODE_LEGACY_EVENT_SPECS)
        .chain(OPENCODE_DEPRECATED_ALIASES)
    {
        assert!(
            unique.insert(spec.name),
            "duplicate registry name across current/compatibility/alias tables"
        );
    }
    for (spec, fixture) in advertised.iter().zip(expected) {
        assert_eq!(spec.name, fixture["name"].as_str().expect("name"));
        let forwarding = match spec.forwarding {
            OpenCodeForwarding::Standalone => "standalone",
            OpenCodeForwarding::PluginHook => "plugin-hook",
            other => panic!("unexpected advertised forwarding: {other:?}"),
        };
        assert_eq!(fixture["forwarding"], forwarding);
        assert_eq!(
            format!("{:?}", spec.kind.expect("kind")),
            fixture["lifecycle"]
        );
        assert_eq!(spec.command.expect("verb").to_string(), fixture["verb"]);
        assert_eq!(
            spec.command.expect("verb").lifecycle_event_kind(),
            spec.kind.expect("kind")
        );
        let envelope: SessionHookEnvelope =
            serde_json::from_value(fixture["envelope"].clone()).expect("envelope");
        assert_eq!(
            opencode_provider()
                .parse_hook_event(spec.name, &envelope)
                .expect("parse current event")
                .kind,
            spec.kind.expect("kind")
        );
    }
    let merged: Vec<_> = OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .filter(|spec| spec.forwarding == OpenCodeForwarding::PluginMerged)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(fixture["plugin_merged_events"], serde_json::json!(merged));
    assert!(
        !OPENCODE_HOOK_EVENT_SPECS
            .iter()
            .any(|spec| spec.name == "session.idle")
    );
    for name in ["session.error", "message.part.updated", "unknown.synthetic"] {
        assert!(!opencode_provider().recognizes_event(name));
    }
    for spec in OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .filter(|spec| spec.kind.is_none())
    {
        assert!(!opencode_provider().recognizes_event(spec.name));
    }
}

#[test]
fn fallback_events_are_a_subset_of_advertised_names() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture");
    for (specs, field) in [
        (OPENCODE_LEGACY_EVENT_SPECS, "compatibility_events"),
        (OPENCODE_DEPRECATED_ALIASES, "deprecated_aliases"),
    ] {
        let expected: Vec<_> = fixture[field]
            .as_array()
            .expect("compatibility names")
            .iter()
            .map(|value| value.as_str().expect("name"))
            .collect();
        assert_eq!(
            specs.iter().map(|spec| spec.name).collect::<Vec<_>>(),
            expected
        );
        for spec in specs {
            assert!(opencode_provider().recognizes_event(spec.name));
            assert!(
                opencode_provider()
                    .supported_commands()
                    .contains(&spec.command.expect("command"))
            );
            let envelope: SessionHookEnvelope = serde_json::from_value(serde_json::json!({
                "hook_event_name": spec.name, "session_id": "ses_synthetic_001",
                "cwd": "/tmp/synthetic-project", "prompt": "", "role": "user",
                "status": {"type":"idle"}
            }))
            .expect("legacy envelope");
            assert_eq!(
                opencode_provider()
                    .parse_hook_event(spec.name, &envelope)
                    .expect("legacy parse")
                    .kind,
                spec.kind.expect("kind")
            );
        }
    }
}

fn native_build_descriptor() -> Value {
    serde_json::from_str(include_str!(
        "fixtures/agent_transcripts/opencode/native-origin-build.json"
    ))
    .expect("public current artifact descriptor")
}

#[test]
fn npr_18_current_pin() {
    use libra::internal::ai::observed_agents::native_origin::codec;
    assert_eq!(OPENCODE_PIN, "2.0.26-libra-native.1");
    assert_eq!(OPENCODE_PIN, codec::PRODUCER_VERSION);
    let current = native_build_descriptor();
    assert_eq!(current["producer_version"], OPENCODE_PIN);
    assert_eq!(current["build_identity"], OPENCODE_BUILD_ID);
    assert!(
        current["source_ready_audit"]
            .as_str()
            .unwrap()
            .contains("single Claude PASS")
    );
}

#[test]
fn npr_18_base_truth() {
    assert_eq!(OPENCODE_OFFICIAL_BASE_PIN, "2.0.26");
    assert_eq!(OPENCODE_COMMIT, "9b4ec5714d481559990db0a816d5dec19541a814");
    let current = native_build_descriptor();
    assert_eq!(current["official_base_version"], OPENCODE_OFFICIAL_BASE_PIN);
    assert_eq!(current["official_base_commit"], OPENCODE_COMMIT);
    assert_eq!(current["official_base_is_patched_source_commit"], false);
    assert_ne!(current["source_archive_sha256"], OPENCODE_COMMIT);
    assert_eq!(
        declared(source_record(), "opencode_pin: "),
        [OPENCODE_OFFICIAL_BASE_PIN]
    );
    assert_eq!(
        declared(source_record(), "opencode_commit: "),
        [OPENCODE_COMMIT]
    );
}

#[test]
fn npr_18_fixture_history() {
    use sha2::{Digest, Sha256};
    let frozen: &[(&str, &str)] = &[
        (
            include_str!("fixtures/agent_transcripts/opencode/hook_events.json"),
            "20751ac63ef1c5e98acd262f4a7411d14d72e1bf5482d404b0ddbbbfa4edb564",
        ),
        (
            include_str!("fixtures/agent_transcripts/opencode.json"),
            "4a8003af087c5c485f611275290b34c116f314a13bb6c4a545b8c0c51cd5ce05",
        ),
        (
            include_str!("fixtures/agent_transcripts/reasoning/opencode.json"),
            "4750cff90cead1a31f79bc484bc0df180640c31946aa413b049250d2b410af8e",
        ),
        (
            include_str!("fixtures/agent_transcripts/native_origin_receipt_v1.json"),
            "9bea0dd73736dfa3ce7544921ef7c7286c1adf30463b0d2d913b54f5e8e1386b",
        ),
        (
            include_str!("fixtures/agent_transcripts/native_origin_signature_v1.json"),
            "d3589b58c490a0937cc69d84f8b124b1674151f1b142c200a95af40779dbdff1",
        ),
    ];
    for (bytes, expected) in frozen {
        assert_eq!(hex::encode(Sha256::digest(bytes.as_bytes())), *expected);
    }
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture["opencode_pin"], OPENCODE_OFFICIAL_BASE_PIN);
    assert_ne!(fixture["opencode_pin"], OPENCODE_PIN);
    assert!(
        MANIFEST.contains("historical official") && MANIFEST.contains("native-origin-build.json")
    );
}

#[test]
fn npr_18_descriptor() {
    use libra::internal::ai::observed_agents::native_origin::approved_build;
    let current = native_build_descriptor();
    let build = approved_build::lookup(
        OPENCODE_PIN,
        OPENCODE_BUILD_ID,
        current["binary_sha256"].as_str().unwrap(),
    )
    .unwrap();
    let identity = build.identity();
    for (key, value) in [
        ("producer_version", identity.producer_version),
        ("build_identity", identity.build_identity),
        ("target", identity.target),
        ("official_base_commit", identity.official_base_commit),
        ("source_archive_sha256", identity.source_archive_sha256),
        ("source_manifest_sha256", identity.source_manifest_sha256),
        ("patch_sha256", identity.patch_sha256),
        ("patch_manifest_sha256", identity.patch_manifest_sha256),
        ("lock_sha256", identity.lock_sha256),
        ("build_inputs_sha256", identity.build_inputs_sha256),
        ("bun_version", identity.bun_version),
        ("bun_binary_sha256", identity.bun_binary_sha256),
        ("review_raw_sha256", identity.review_raw_sha256),
    ] {
        assert_eq!(current[key], value, "artifact identity {key}");
    }
    assert_eq!(current["binary_bytes"], identity.binary_bytes);
    assert_eq!(current["native_GO"], false);
    for key in [
        "issuer",
        "public_key",
        "private_key",
        "policy_digest",
        "installation_path",
        "libra_command_path",
    ] {
        assert!(
            current.get(key).is_none(),
            "no per-install authority or secrets in public descriptor"
        );
    }
}

#[test]
fn npr_18_drift() {
    let current = native_build_descriptor();
    let good = current["binary_sha256"].as_str().unwrap();
    #[cfg(target_os = "macos")]
    {
        use harness::agent_local_capture::opencode_acceptance_identity;
        let version = format!("opencode v{OPENCODE_PIN}");
        let size = current["binary_bytes"].as_u64().unwrap();
        opencode_acceptance_identity(&version, good, size).unwrap();
        assert!(opencode_acceptance_identity("opencode v2.0.26", good, size).is_err());
        assert!(
            opencode_acceptance_identity("opencode v2.0.27-libra-native.1", good, size).is_err()
        );
        assert!(opencode_acceptance_identity(&version, &"ff".repeat(32), size).is_err());
        assert!(opencode_acceptance_identity(&version, good, size + 1).is_err());
    }
    #[cfg(not(target_os = "macos"))]
    assert!(
        !good.is_empty(),
        "public descriptor only; private runtime acceptance macOS, Linux user-later"
    );
}

#[test]
fn npr_18_opt_in() {
    let current = native_build_descriptor();
    assert_eq!(current["opt_in_env"], "LIBRA_OPENCODE_NATIVE_BINARY");
    #[cfg(target_os = "macos")]
    {
        use std::{os::unix::fs::PermissionsExt, path::Path};

        use harness::agent_local_capture::{
            OPENCODE_NATIVE_BINARY_ENV, opencode_acceptance_binary,
        };
        assert_eq!(OPENCODE_NATIVE_BINARY_ENV, current["opt_in_env"]);
        assert!(
            opencode_acceptance_binary(None)
                .unwrap_err()
                .contains(OPENCODE_NATIVE_BINARY_ENV)
        );
        assert!(opencode_acceptance_binary(Some(Path::new("relative/opencode"))).is_err());
        let temp = tempfile::tempdir().unwrap();
        let binary = temp.path().join("opencode");
        std::fs::write(&binary, b"synthetic binary; must not be executed").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(opencode_acceptance_binary(Some(&binary)).is_err());
        if let Ok(actual) = std::env::var("LIBRA_NATIVE_TEST_PRODUCER") {
            let selected = opencode_acceptance_binary(Some(Path::new(&actual))).unwrap();
            assert_eq!(selected, std::fs::canonicalize(actual).unwrap());
            let raw = tempfile::tempdir().unwrap();
            let version = harness::agent_local_capture::opencode_version_probe(
                &selected,
                raw.path(),
                std::time::Duration::from_secs(30),
            )
            .unwrap();
            assert_eq!(version, format!("opencode v{OPENCODE_PIN}"));
            eprintln!(
                "NPR18 actual separate approved private binary selected/read-only measured; no model call or issuer admission"
            );
        }
    }
}

#[test]
fn current_official_identity_does_not_rewrite_frozen_native_baseline() {
    use libra::internal::ai::hooks::providers::opencode::events::{
        CURRENT_OFFICIAL_COMMIT, CURRENT_OFFICIAL_PIN,
    };
    use sha2::{Digest, Sha256};
    assert_eq!(CURRENT_OFFICIAL_PIN, "2.0.29");
    assert_eq!(
        CURRENT_OFFICIAL_COMMIT,
        "042d0832f5f3a73eaa039a58a7d565776f6d4290"
    );
    assert_eq!(OPENCODE_OFFICIAL_BASE_PIN, "2.0.26");
    assert_eq!(OPENCODE_COMMIT, "9b4ec5714d481559990db0a816d5dec19541a814");
    assert_eq!(OPENCODE_PIN, "2.0.26-libra-native.1");
    let fixture: Value = serde_json::from_str(FIXTURE).expect("historical synthetic fixture");
    assert_eq!(fixture["opencode_pin"], OPENCODE_OFFICIAL_BASE_PIN);
    assert_ne!(CURRENT_OFFICIAL_COMMIT, OPENCODE_COMMIT);
    // Frozen dc1 prefixes are measured from committed bytes, never generated
    // from the new namespace or a rewritten fixture postimage.
    for (bytes, length, expected_sha) in [
        (
            include_bytes!("fixtures/agent_transcripts/MANIFEST.md").as_slice(),
            8725usize,
            "b5684598710ddbcc444a53cac1a288b715f8b31234a17511c050f03144a32bf0",
        ),
        (
            include_bytes!("fixtures/agent_transcripts/opencode/MANIFEST.md").as_slice(),
            4466usize,
            "ed31d690f373f742eabc0ec25e04a266a202324ba6522d01b7810e1b949d573d",
        ),
    ] {
        let frozen = bytes
            .get(..length)
            .expect("original manifest prefix must remain");
        assert_eq!(
            hex::encode(Sha256::digest(frozen)),
            expected_sha,
            "historical MANIFEST prefix was modified"
        );
    }
    for (label, manifest) in [
        ("opencode/MANIFEST.md", MANIFEST),
        (
            "MANIFEST.md",
            include_str!("fixtures/agent_transcripts/MANIFEST.md"),
        ),
    ] {
        assert!(
            manifest.contains(&format!(
                "current_official_opencode_pin: {CURRENT_OFFICIAL_PIN}"
            )),
            "{label} must declare the current ordinary version"
        );
        assert!(
            manifest.contains(&format!(
                "current_official_opencode_commit: {CURRENT_OFFICIAL_COMMIT}"
            )),
            "{label} must declare the current ordinary source commit"
        );
    }
}
