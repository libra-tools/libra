//! Internal layer for the Libra runtime.
//!
//! Core database, VCS state, and utility modules live in `crates/libra-core`
//! and are re-exported here with unchanged module paths.

pub use libra_core::internal::{
    account, alternates, auth, branch, change, config, config_file, config_ownership, db, deps,
    dirty, head, layer, log, maintenance_lock, merge_base, metadata, mutable_state_ownership,
    notes, object_format, obliteration, operation, pack_writer, patch_mode, process_terminate,
    pseudo_ref, reflog, revision_ordinal, sequencer, shallow, sparse, tag, tree_plumbing, upgrade,
    vault, workspace, worktree_io, worktree_scope,
};
pub use libra_model as model;

pub mod ai;
pub mod protocol;
pub mod repo_hooks;
#[cfg(test)]
pub use libra_core::internal::source_scan;

#[cfg(test)]
mod metadata_ai_tests {
    use crate::internal::metadata::*;

    #[cfg(unix)]
    #[test]
    fn cov2_v1_identity_bytes_and_mac_frozen() {
        use std::{fs, os::unix::fs::PermissionsExt};

        use sha2::{Digest, Sha256};

        use crate::internal::ai::{
            capture::{
                checkpoint::{
                    reasoning_turn_descriptor_body_for_schema_version,
                    reasoning_turn_slot_body_for_schema_version,
                },
                key::{
                    ReasoningIdentityMacPurpose,
                    authenticate_reasoning_identity_existing_key_for_test,
                },
            },
            capture_scope::CaptureScope,
        };

        // These whole-byte SHA-256 and MAC answers were calculated from the
        // frozen identity-v1 serializer and tag/domain strings before adding
        // v2. The fixture key is synthetic, never a repository credential.
        let repo = "cov2-repo";
        let session = "cov2-session";
        let turn = "turn:alpha";
        let scope = CaptureScope {
            repo_id: repo.to_owned(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let root = tempfile::tempdir().expect("create isolated v1 golden fixture");
        let storage = root.path().join("storage");
        let private = storage.join("private");
        fs::create_dir(&storage).expect("create fixture storage");
        fs::create_dir(&private).expect("create private key directory");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700))
            .expect("secure private directory");
        let key_path = private.join("agent-capture-dedup-v1.key");
        fs::write(&key_path, [b'B'; 32]).expect("write synthetic fixed key");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .expect("secure fixture key");

        let descriptor_purpose =
            ReasoningIdentityMacPurpose::descriptor_for_schema_version(1).unwrap();
        let record_purpose = ReasoningIdentityMacPurpose::record_for_schema_version(1).unwrap();
        let map_purpose = ReasoningIdentityMacPurpose::map_for_schema_version(1).unwrap();
        assert_eq!(map_purpose, record_purpose, "frozen v1 map uses Record MAC");
        let sign = |body: &[u8], purpose| {
            authenticate_reasoning_identity_existing_key_for_test(&storage, body, None, purpose)
                .expect("sign with the existing synthetic fixture key")
        };

        let slot = reasoning_turn_slot_body_for_schema_version(&scope, session, turn, 1)
            .expect("canonical v1 turn slot");
        let slot_mac = sign(&slot, descriptor_purpose);
        let entries = vec![(
            "native_proven",
            "issuer:test",
            "11".repeat(32),
            "22".repeat(32),
            "locator:test",
            "opencode",
            "signature",
            "33".repeat(32),
        )];
        let descriptor =
            reasoning_turn_descriptor_body_for_schema_version(&scope, session, turn, &entries, 1)
                .expect("canonical v1 descriptor");
        let descriptor_mac = sign(&descriptor, descriptor_purpose);
        let record_body = ReasoningIdentityRecordBody {
            context: ReasoningIdentityRecordContext {
                repository_id: repo.to_owned(),
                session_id: session.to_owned(),
                logical_turn_key: slot_mac.clone(),
                coverage_schema_version: 1,
                claim_revision: 1,
                claim_fence_token: 2,
                checkpoint_id: "checkpoint-1".to_owned(),
            },
            kind: ReasoningIdentityKind::Nonempty,
            descriptor_mac: descriptor_mac.clone(),
        };
        let record_bytes = ReasoningIdentityRecord::authentication_body(&record_body)
            .expect("canonical v1 record body");
        let mut unsupported = record_body.clone();
        unsupported.context.coverage_schema_version = 3;
        assert!(ReasoningIdentityRecord::authentication_body(&unsupported).is_err());
        let record_mac = sign(&record_bytes, record_purpose);
        let record = ReasoningIdentityRecord::from_body_and_mac(record_body, record_mac.clone())
            .expect("closed v1 record");
        let record_envelope = record.encode().expect("canonical v1 record envelope");
        let records = std::collections::BTreeMap::from([(slot_mac.clone(), record)]);
        let map_bytes = ReasoningIdentitySessionMap::authentication_body(repo, session, &records)
            .expect("canonical v1 whole-map body");
        let map_mac = sign(&map_bytes, map_purpose);
        let map = ReasoningIdentitySessionMap::from_records_and_mac(
            repo.to_owned(),
            session.to_owned(),
            records,
            map_mac.clone(),
        )
        .expect("closed v1 whole map");
        let map_envelope = map.encode().expect("canonical v1 map envelope");

        for (name, actual, expected) in [
            (
                "slot",
                slot.as_slice(),
                "e8778866139fbe1cedc22eb902ab11280e266a15d3eba04cd46e08148c66004d",
            ),
            (
                "descriptor",
                descriptor.as_slice(),
                "ba5af77054719ae0545db8c74c2abc90bcbf964e2e4f666744dc01434c37a6eb",
            ),
            (
                "record body",
                record_bytes.as_slice(),
                "dcd8fd0483e1d236de7318703277aae7b357ac1e2268718d26e2bcc69103de37",
            ),
            (
                "record envelope",
                record_envelope.as_bytes(),
                "80b9a8b709149ee2dc36048061cf8ebac1f688267ee5da757fede55c38a90dc2",
            ),
            (
                "map body",
                map_bytes.as_slice(),
                "595b44ca9f1ba5c6ded70368808c3b501db08f0695f8354acf5dc835c0ca7eb7",
            ),
            (
                "map envelope",
                map_envelope.as_bytes(),
                "05c413b0b1d1a3eba544fc7f0e5dcb71f10b3a67d4401510452e323cf0263d0d",
            ),
        ] {
            assert_eq!(
                hex::encode(Sha256::digest(actual)),
                expected,
                "{name} drifted"
            );
        }
        assert_eq!(
            slot_mac,
            "reasoning-identity/hmac-v1/06589d8f32484845f01b8a2b5bf879c60f84b3564087694550976057ede30227"
        );
        assert_eq!(
            descriptor_mac,
            "reasoning-identity/hmac-v1/d6abec939340a2efa90b8ba74c3327f42cd1aa7b60ab59ffdd623a5fee252aad"
        );
        assert_eq!(
            record_mac,
            "reasoning-record/hmac-v1/a3c522269ed8c5f22e37057d900a477bc53b6028bfaf64df00350ef20dcc7445"
        );
        assert_eq!(
            map_mac,
            "reasoning-record/hmac-v1/05462402ee2a44a026bee5d740cf9e7e23b19de9c0ae696946f02593eb0082ee"
        );
        assert_eq!(
            fs::read(key_path).expect("read unchanged fixture key"),
            [b'B'; 32]
        );
    }
}
