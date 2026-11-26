//! Sealed receipt authentication. This module creates no archive capability.
// The family consumer is delivered separately; keep this preparatory API private.
#![allow(dead_code)]

use std::{fs, io::Read, path::Path};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::signature::{ED25519, UnparsedPublicKey};
use sea_orm::DatabaseTransaction;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{approved_build, codec, registry};
use crate::internal::ai::observed_agents::trust;

const EXECUTABLE_BYTE_CAP: u64 = 512 * 1024 * 1024;

/// Fixed diagnostics never include a payload, signature or untrusted label.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum VerificationError {
    #[error("Native-origin signature is invalid; discard the proof and keep capture partial")]
    Signature,
    #[error("Native-origin source coordinates differ; discard the proof and keep capture partial")]
    Source,
    #[error("Native-origin original fields differ; discard the proof and keep capture partial")]
    Field,
    #[error(
        "Native-origin runtime or observer policy changed; keep capture partial and prepare an explicitly registered approved installation"
    )]
    Runtime,
    #[error(
        "Native-origin current issuer permission is absent or changed; keep capture partial and check explicit exporter trust and issuer registration"
    )]
    Authority,
}

/// The consumer supplies actual source coordinates, never receipt-derived ones.
pub(crate) struct SourceCoordinates<'a> {
    pub(crate) session: &'a str,
    pub(crate) message: &'a str,
    pub(crate) part: &'a str,
}

/// Original decoded JSON string bytes. No base64 decoding or normalization.
pub(crate) struct OriginalField<'a> {
    pub(crate) name: codec::FieldName,
    pub(crate) value: &'a str,
}

/// Only this module can construct this value. No serialization, default,
/// mutable accessor or conversion from untrusted wire is available.
pub(crate) struct VerifiedProducerReceipt {
    receipt: codec::Receipt,
}

impl VerifiedProducerReceipt {
    pub(crate) fn receipt(&self) -> &codec::Receipt {
        &self.receipt
    }
}

/// Caller-owned permission snapshot. Any consumer that also persists data must
/// acquire db::begin_write_transaction BEFORE its first read and retain that
/// same transaction for admission. This function neither writes nor acquires a
/// second connection. Revocation after this snapshot does not cancel already
/// admitted in-flight work. Pending/import must obtain a fresh snapshot.
pub(crate) async fn verify_with_conn(
    conn: &DatabaseTransaction,
    worktree: &Path,
    raw_receipt: &[u8],
    source: &SourceCoordinates<'_>,
    fields: &[OriginalField<'_>],
) -> Result<VerifiedProducerReceipt> {
    let receipt = codec::decode_receipt(raw_receipt)?;
    let current = registry::read_with_conn(conn)
        .await?
        .ok_or(VerificationError::Authority)?;
    if !current.issuers.iter().any(|record| {
        record.issuer == receipt.issuer && record.state == registry::IssuerState::Active
    }) {
        return Err(VerificationError::Authority.into());
    }
    let trusted = trust::read_trust_with_conn(conn, trust::OPENCODE_EXPORTER_SLUG)
        .await?
        .ok_or(VerificationError::Authority)?;
    let producer = trusted.path.clone();
    let repo = worktree.to_owned();
    // Hashing a large producer and reading local files must not block Tokio.
    let measured = tokio::task::spawn_blocking(move || measure_runtime(&producer, &repo))
        .await
        .context(
            "measure native-origin runtime; keep capture partial if measurement cannot finish",
        )??;
    let record = registry::active_issuer_with_conn(conn, &receipt.issuer, &measured.producer)
        .await?
        .ok_or(VerificationError::Authority)?;
    let dirs = trust::read_trusted_dirs_with_conn(conn).await?;
    let dir = dirs
        .iter()
        .find(|dir| measured.producer.canonical_path.starts_with(dir))
        .ok_or(VerificationError::Authority)?;
    trust::ensure_dir_not_world_writable(dir).map_err(|_| VerificationError::Authority)?;
    verify_bound(receipt, &record, &measured, source, fields)
}

/// Backend-agnostic counterpart of [`verify_with_conn`] using [`crate::storage::ConfigStore`].
pub(crate) async fn verify_with_store(
    store: &dyn crate::storage::ConfigStore,
    worktree: &Path,
    raw_receipt: &[u8],
    source: &SourceCoordinates<'_>,
    fields: &[OriginalField<'_>],
) -> Result<VerifiedProducerReceipt> {
    let receipt = codec::decode_receipt(raw_receipt)?;
    let current = registry::read_with_store(store)
        .await?
        .ok_or(VerificationError::Authority)?;
    if !current.issuers.iter().any(|record| {
        record.issuer == receipt.issuer && record.state == registry::IssuerState::Active
    }) {
        return Err(VerificationError::Authority.into());
    }
    let trusted = trust::read_trust_with_store(store, trust::OPENCODE_EXPORTER_SLUG)
        .await?
        .ok_or(VerificationError::Authority)?;
    let producer = trusted.path.clone();
    let repo = worktree.to_owned();
    let measured = tokio::task::spawn_blocking(move || measure_runtime(&producer, &repo))
        .await
        .context(
            "measure native-origin runtime; keep capture partial if measurement cannot finish",
        )??;
    let record = registry::active_issuer_with_store(store, &receipt.issuer, &measured.producer)
        .await?
        .ok_or(VerificationError::Authority)?;
    let dirs = trust::read_trusted_dirs_with_store(store).await?;
    let dir = dirs
        .iter()
        .find(|dir| measured.producer.canonical_path.starts_with(dir))
        .ok_or(VerificationError::Authority)?;
    trust::ensure_dir_not_world_writable(dir).map_err(|_| VerificationError::Authority)?;
    verify_bound(receipt, &record, &measured, source, fields)
}

struct MeasuredRuntime {
    producer: trust::Provenance,
    observer_command: String,
    observer_bytes: Vec<u8>,
}

fn checked_executable(path: &Path) -> Result<std::path::PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|_| VerificationError::Runtime)?;
    let meta = fs::symlink_metadata(&canonical).map_err(|_| VerificationError::Runtime)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > EXECUTABLE_BYTE_CAP {
        return Err(VerificationError::Runtime.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid reads the process uid and has no pointer arguments.
        let uid = unsafe { libc::geteuid() };
        if (meta.uid() != uid && meta.uid() != 0)
            || meta.permissions().mode() & 0o022 != 0
            || meta.permissions().mode() & 0o111 == 0
        {
            return Err(VerificationError::Runtime.into());
        }
    }
    trust::ensure_parent_not_world_writable(&canonical).map_err(|_| VerificationError::Runtime)?;
    Ok(canonical)
}

fn read_observer(path: &Path) -> Result<Vec<u8>> {
    if fs::symlink_metadata(path)
        .map_err(|_| VerificationError::Runtime)?
        .file_type()
        .is_symlink()
    {
        return Err(VerificationError::Runtime.into());
    }
    trust::ensure_parent_not_world_writable(path).map_err(|_| VerificationError::Runtime)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| VerificationError::Runtime)?;
    let meta = file.metadata().map_err(|_| VerificationError::Runtime)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > codec::DESCRIPTOR_BYTE_CAP as u64 {
        return Err(VerificationError::Runtime.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid reads the process uid and has no pointer arguments.
        let uid = unsafe { libc::geteuid() };
        if (meta.uid() != uid && meta.uid() != 0) || meta.permissions().mode() & 0o022 != 0 {
            return Err(VerificationError::Runtime.into());
        }
    }
    let mut bytes = Vec::new();
    file.take(codec::DESCRIPTOR_BYTE_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| VerificationError::Runtime)?;
    if bytes.len() > codec::DESCRIPTOR_BYTE_CAP || bytes.len() as u64 != meta.len() {
        return Err(VerificationError::Runtime.into());
    }
    Ok(bytes)
}

fn measure_runtime(producer: &Path, worktree: &Path) -> Result<MeasuredRuntime> {
    let producer = checked_executable(producer)?;
    let provenance =
        trust::compute_provenance(&producer).map_err(|_| VerificationError::Runtime)?;
    let observer =
        checked_executable(&std::env::current_exe().map_err(|_| VerificationError::Runtime)?)?;
    let observer_command = observer
        .to_str()
        .ok_or(VerificationError::Runtime)?
        .to_owned();
    let observer_bytes = read_observer(&worktree.join(".opencode/plugin/libra-hooks.js"))?;
    Ok(MeasuredRuntime {
        producer: provenance,
        observer_command,
        observer_bytes,
    })
}

fn verify_signature(receipt: &codec::Receipt, public_key: &str) -> Result<()> {
    let preimage = codec::signing_bytes(receipt)?;
    let key = registry::lower_hex(public_key, 32).map_err(|_| VerificationError::Signature)?;
    if hex::encode(Sha256::digest(&key)) != receipt.issuer {
        return Err(VerificationError::Signature.into());
    }
    let signature = STANDARD
        .decode(&receipt.signature)
        .map_err(|_| VerificationError::Signature)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&preimage, &signature)
        .map_err(|_| VerificationError::Signature.into())
}

fn verify_bound(
    receipt: codec::Receipt,
    record: &registry::IssuerRecord,
    measured: &MeasuredRuntime,
    source: &SourceCoordinates<'_>,
    fields: &[OriginalField<'_>],
) -> Result<VerifiedProducerReceipt> {
    if record.state != registry::IssuerState::Active
        || receipt.issuer != record.issuer
        || receipt.key_generation != record.key_generation
        || measured.producer.canonical_path != Path::new(&record.producer_path)
        || measured.producer.sha256 != record.producer_binary_digest
    {
        return Err(VerificationError::Authority.into());
    }
    verify_signature(&receipt, &record.public_key)?;
    if receipt.source_session != source.session
        || receipt.source_message != source.message
        || receipt.source_part != source.part
    {
        return Err(VerificationError::Source.into());
    }
    if receipt.fields.len() != fields.len()
        || receipt.fields.iter().zip(fields).any(|(signed, original)| {
            signed.name != original.name
                || usize::try_from(signed.length).ok() != Some(original.value.len())
                || signed.sha256 != hex::encode(Sha256::digest(original.value.as_bytes()))
        })
    {
        return Err(VerificationError::Field.into());
    }
    let build = approved_build::lookup(
        &record.producer_version,
        &record.build_identity,
        &measured.producer.sha256,
    )
    .map_err(|_| VerificationError::Runtime)?;
    let policy = build
        .installation_policy(&measured.observer_command, &measured.observer_bytes)
        .map_err(|_| VerificationError::Runtime)?;
    if receipt.build_identity != record.build_identity
        || receipt.producer_version != record.producer_version
        || policy != record.policy_descriptor
        || receipt.policy_digest != record.policy_digest
        || record.policy_digest != hex::encode(Sha256::digest(codec::policy_bytes(&policy)?))
        || !build.matches_endpoint_pair(receipt.protocol, &receipt.endpoint_digest)
    {
        return Err(VerificationError::Runtime.into());
    }
    Ok(VerifiedProducerReceipt { receipt })
}

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
    use serde_json::Value;

    use super::*;
    use crate::internal::{config::ConfigKv, db};

    const VECTOR: &str = include_str!(
        "../../../../../tests/fixtures/agent_transcripts/native_origin_signature_v1.json"
    );
    const BINARY_SHA: &str = "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196";
    const NONCE: &str = "bac21cc4027896b8d1836133c347d20f";
    const VALUE: &str = "SYNTHETIC native string 界 e\u{301}";

    fn fixture() -> (Value, codec::Receipt) {
        let v: Value = serde_json::from_str(VECTOR).unwrap();
        let r = codec::decode_receipt(&serde_json::to_vec(&v["receipt"]).unwrap()).unwrap();
        (v, r)
    }

    fn key() -> Ed25519KeyPair {
        // Public NPR03 test seed only; no installation key or private store.
        let seed: Vec<u8> = (0..32).collect();
        Ed25519KeyPair::from_seed_unchecked(&seed).unwrap()
    }

    fn sign(r: &mut codec::Receipt) {
        r.signature = STANDARD.encode(key().sign(&codec::signing_bytes(r).unwrap()).as_ref());
    }

    fn input() -> (codec::Receipt, registry::IssuerRecord, MeasuredRuntime) {
        // Pure SYNTHETIC predicate input, not enrolled provenance or native GO.
        let (_, mut receipt) = fixture();
        let build = approved_build::lookup(codec::PRODUCER_VERSION, NONCE, BINARY_SHA).unwrap();
        let path = std::env::current_exe().unwrap().canonicalize().unwrap();
        let command = path.to_str().unwrap();
        let rendered = build.render_observer(command).unwrap();
        let policy = build
            .installation_policy(command, rendered.as_bytes())
            .unwrap();
        let record = registry::IssuerRecord {
            issuer: receipt.issuer.clone(),
            public_key: hex::encode(key().public_key().as_ref()),
            key_generation: 1,
            build_identity: NONCE.into(),
            producer_version: codec::PRODUCER_VERSION.into(),
            producer_binary_digest: BINARY_SHA.into(),
            producer_path: "/synthetic/approved-producer/opencode".into(),
            policy_digest: hex::encode(Sha256::digest(codec::policy_bytes(&policy).unwrap())),
            policy_descriptor: policy,
            state: registry::IssuerState::Active,
        };
        receipt.build_identity = record.build_identity.clone();
        receipt.policy_digest = record.policy_digest.clone();
        receipt.endpoint_digest = record.policy_descriptor.endpoint_digests[0].clone();
        receipt.fields = vec![codec::FieldDigest {
            name: codec::FieldName::Signature,
            length: VALUE.len().try_into().unwrap(),
            sha256: hex::encode(Sha256::digest(VALUE.as_bytes())),
        }];
        sign(&mut receipt);
        let measured = MeasuredRuntime {
            producer: trust::Provenance {
                canonical_path: record.producer_path.clone().into(),
                sha256: BINARY_SHA.into(),
                device: 7,
                inode: 8,
                mtime: 9,
            },
            observer_command: command.into(),
            observer_bytes: rendered.into_bytes(),
        };
        (receipt, record, measured)
    }

    fn anthropic_input() -> (codec::Receipt, registry::IssuerRecord, MeasuredRuntime) {
        let (mut receipt, mut record, mut measured) = input();
        let build = approved_build::lookup(
            codec::ANTHROPIC_PRODUCER_VERSION,
            "78c1f19e9249da8d0143a079f90aec9a",
            "28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c",
        )
        .unwrap();
        let policy = build
            .installation_policy(&measured.observer_command, &measured.observer_bytes)
            .unwrap();
        record.build_identity = build.identity().build_identity.into();
        record.producer_version = build.identity().producer_version.into();
        record.producer_binary_digest = build.identity().binary_sha256.into();
        record.policy_digest = hex::encode(Sha256::digest(codec::policy_bytes(&policy).unwrap()));
        record.policy_descriptor = policy;
        measured.producer.sha256 = build.identity().binary_sha256.into();
        receipt.build_identity = record.build_identity.clone();
        receipt.producer_version = record.producer_version.clone();
        receipt.policy_digest = record.policy_digest.clone();
        receipt.protocol = codec::Protocol::AnthropicMessages;
        receipt.endpoint_digest = build
            .endpoint(
                codec::Protocol::AnthropicMessages,
                codec::ANTHROPIC_ENDPOINT,
            )
            .unwrap()
            .sha256
            .into();
        sign(&mut receipt);
        (receipt, record, measured)
    }

    fn coordinates(r: &codec::Receipt) -> SourceCoordinates<'_> {
        SourceCoordinates {
            session: &r.source_session,
            message: &r.source_message,
            part: &r.source_part,
        }
    }

    fn fields() -> [OriginalField<'static>; 1] {
        [OriginalField {
            name: codec::FieldName::Signature,
            value: VALUE,
        }]
    }

    fn error(result: Result<VerifiedProducerReceipt>) -> VerificationError {
        result.err().unwrap().downcast().unwrap()
    }

    #[test]
    fn npr_11_signature() {
        let (v, r) = fixture();
        let preimage = codec::signing_bytes(&r).unwrap();
        assert_eq!(hex::encode(&preimage), v["canonical_hex"].as_str().unwrap());
        assert_eq!(
            hex::encode(Sha256::digest(&preimage)),
            v["canonical_sha256"].as_str().unwrap()
        );
        assert_eq!(
            hex::encode(STANDARD.decode(&r.signature).unwrap()),
            v["signature_hex"].as_str().unwrap()
        );
        verify_signature(&r, v["public_key_hex"].as_str().unwrap()).unwrap();
        assert!(
            approved_build::lookup(&r.producer_version, &r.build_identity, BINARY_SHA).is_err()
        );
        let mut changed = r.clone();
        changed.source_message.push('x');
        assert!(verify_signature(&changed, v["public_key_hex"].as_str().unwrap()).is_err());
        changed = r.clone();
        let mut sig = STANDARD.decode(&changed.signature).unwrap();
        sig[0] ^= 1;
        changed.signature = STANDARD.encode(sig);
        assert!(verify_signature(&changed, v["public_key_hex"].as_str().unwrap()).is_err());
        assert!(verify_signature(&r, &hex::encode([1u8; 32])).is_err());
        let (r, mut record, m) = input();
        verify_bound(r.clone(), &record, &m, &coordinates(&r), &fields()).unwrap();
        record.key_generation += 1;
        assert_eq!(
            error(verify_bound(
                r.clone(),
                &record,
                &m,
                &coordinates(&r),
                &fields()
            )),
            VerificationError::Authority
        );
    }

    #[test]
    fn anthropic_receipt_rejects_crossed_protocol_endpoint_pair() {
        let (receipt, record, measured) = anthropic_input();
        assert!(
            verify_bound(
                receipt.clone(),
                &record,
                &measured,
                &coordinates(&receipt),
                &fields(),
            )
            .is_ok()
        );
        let mut wrong = receipt.clone();
        wrong.protocol = codec::Protocol::OpenaiResponses;
        sign(&mut wrong);
        assert_eq!(
            error(verify_bound(
                wrong,
                &record,
                &measured,
                &coordinates(&receipt),
                &fields(),
            )),
            VerificationError::Runtime
        );
        let mut wrong = receipt.clone();
        wrong.endpoint_digest = record.policy_descriptor.endpoint_digests[0].clone();
        sign(&mut wrong);
        assert_eq!(
            error(verify_bound(
                wrong,
                &record,
                &measured,
                &coordinates(&receipt),
                &fields(),
            )),
            VerificationError::Runtime
        );
        let (mut old_receipt, old_record, old_measured) = input();
        old_receipt.protocol = codec::Protocol::AnthropicMessages;
        old_receipt.endpoint_digest = receipt.endpoint_digest;
        sign(&mut old_receipt);
        assert_eq!(
            error(verify_bound(
                old_receipt.clone(),
                &old_record,
                &old_measured,
                &coordinates(&old_receipt),
                &fields(),
            )),
            VerificationError::Runtime
        );
    }

    #[test]
    fn npr_11_source() {
        let (r, record, m) = input();
        for c in [
            SourceCoordinates {
                session: "different",
                ..coordinates(&r)
            },
            SourceCoordinates {
                message: "different",
                ..coordinates(&r)
            },
            SourceCoordinates {
                part: "npart_70707070707070707070707070707070",
                ..coordinates(&r)
            },
        ] {
            assert_eq!(
                error(verify_bound(r.clone(), &record, &m, &c, &fields())),
                VerificationError::Source
            );
        }
        verify_bound(r.clone(), &record, &m, &coordinates(&r), &fields()).unwrap();
    }

    #[test]
    fn npr_11_field() {
        let (r, record, m) = input();
        for f in [
            OriginalField {
                name: codec::FieldName::RedactedData,
                value: VALUE,
            },
            OriginalField {
                name: codec::FieldName::Signature,
                value: "SYNTHETIC native string 界 é",
            },
            OriginalField {
                name: codec::FieldName::Signature,
                value: "",
            },
        ] {
            assert_eq!(
                error(verify_bound(r.clone(), &record, &m, &coordinates(&r), &[f])),
                VerificationError::Field
            );
        }
        assert_eq!(
            error(verify_bound(r.clone(), &record, &m, &coordinates(&r), &[])),
            VerificationError::Field
        );
        for mutate in [0, 1, 2] {
            let mut changed = r.clone();
            match mutate {
                0 => changed.fields[0].length += 1,
                1 => changed.fields[0].sha256 = "ab".repeat(32),
                _ => changed.fields[0].name = codec::FieldName::ReasoningEncryptedContent,
            }
            sign(&mut changed);
            assert_eq!(
                error(verify_bound(
                    changed,
                    &record,
                    &m,
                    &coordinates(&r),
                    &fields()
                )),
                VerificationError::Field
            );
        }
        verify_bound(r.clone(), &record, &m, &coordinates(&r), &fields()).unwrap();
    }

    #[test]
    fn npr_11_runtime() {
        let (r, record, mut m) = input();
        for mutate in 0..5 {
            let mut changed = r.clone();
            match mutate {
                0 => changed.build_identity = "22".repeat(16),
                1 => changed.producer_version = "2.0.27-libra-native.1".into(),
                2 => changed.protocol = codec::Protocol::AnthropicMessages,
                3 => changed.endpoint_digest = "ab".repeat(32),
                _ => changed.policy_digest = "ab".repeat(32),
            }
            if mutate == 1 {
                assert!(codec::signing_bytes(&changed).is_err());
            } else {
                sign(&mut changed);
                assert_eq!(
                    error(verify_bound(
                        changed,
                        &record,
                        &m,
                        &coordinates(&r),
                        &fields()
                    )),
                    VerificationError::Runtime
                );
            }
        }
        let original = m.observer_bytes.clone();
        m.observer_bytes.push(b' ');
        assert_eq!(
            error(verify_bound(
                r.clone(),
                &record,
                &m,
                &coordinates(&r),
                &fields()
            )),
            VerificationError::Runtime
        );
        m.observer_bytes = original;
        m.observer_command.push_str("-moved");
        assert_eq!(
            error(verify_bound(
                r.clone(),
                &record,
                &m,
                &coordinates(&r),
                &fields()
            )),
            VerificationError::Runtime
        );
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("libra");
        fs::write(&exe, b"synthetic observer executable v1").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let build = approved_build::lookup(codec::PRODUCER_VERSION, NONCE, BINARY_SHA).unwrap();
        let p = exe.to_str().unwrap();
        let bytes = build.render_observer(p).unwrap();
        let before = build.installation_policy(p, bytes.as_bytes()).unwrap();
        checked_executable(&exe).unwrap();
        fs::write(&exe, b"synthetic observer executable updated v2").unwrap();
        checked_executable(&exe).unwrap();
        assert_eq!(
            before,
            build.installation_policy(p, bytes.as_bytes()).unwrap()
        );
        let plugin = temp.path().join("libra-hooks.js");
        fs::write(&plugin, &bytes).unwrap();
        // umask 0002 creates this file group-writable (0664). read_observer
        // rejects mode bits 0o022, so the success case must clear them.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&plugin, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_observer(&plugin).unwrap(), bytes.as_bytes());
        fs::write(&plugin, vec![b'x'; codec::DESCRIPTOR_BYTE_CAP + 1]).unwrap();
        assert!(read_observer(&plugin).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            fs::remove_file(&plugin).unwrap();
            symlink(&exe, &plugin).unwrap();
            assert!(read_observer(&plugin).is_err());
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o777)).unwrap();
            assert!(checked_executable(&exe).is_err());
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    async fn storage() -> DatabaseConnection {
        let c = Database::connect("sqlite::memory:").await.unwrap();
        c.execute_unprepared("CREATE TABLE config_kv (id INTEGER PRIMARY KEY, key TEXT NOT NULL, value TEXT NOT NULL, encrypted INTEGER NOT NULL)").await.unwrap();
        c
    }

    #[tokio::test]
    async fn npr_11_authority() {
        let conn = storage().await;
        let (r, record, m) = input();
        let raw = serde_json::to_vec(&r).unwrap();
        let txn = db::begin_write_transaction(&conn).await.unwrap();
        assert_eq!(
            error(
                verify_with_conn(
                    &txn,
                    Path::new("/synthetic"),
                    &raw,
                    &coordinates(&r),
                    &fields()
                )
                .await
            ),
            VerificationError::Authority
        );
        for state in [
            registry::IssuerState::VerifyOnly,
            registry::IssuerState::Revoked,
        ] {
            let mut changed = record.clone();
            changed.state = state;
            let row = registry::Registry {
                version: 1,
                epoch: 1,
                issuers: vec![changed.clone()],
            };
            ConfigKv::set_with_conn(
                &txn,
                registry::REGISTRY_KEY,
                &serde_json::to_string(&row).unwrap(),
                false,
            )
            .await
            .unwrap();
            assert_eq!(
                error(
                    verify_with_conn(
                        &txn,
                        Path::new("/synthetic"),
                        &raw,
                        &coordinates(&r),
                        &fields()
                    )
                    .await
                ),
                VerificationError::Authority
            );
            assert_eq!(
                error(verify_bound(
                    r.clone(),
                    &changed,
                    &m,
                    &coordinates(&r),
                    &fields()
                )),
                VerificationError::Authority
            );
        }
        let row = registry::Registry {
            version: 1,
            epoch: 3,
            issuers: vec![record.clone()],
        };
        ConfigKv::set_with_conn(
            &txn,
            registry::REGISTRY_KEY,
            &serde_json::to_string(&row).unwrap(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            error(
                verify_with_conn(
                    &txn,
                    Path::new("/synthetic"),
                    &raw,
                    &coordinates(&r),
                    &fields()
                )
                .await
            ),
            VerificationError::Authority
        );
        // A manually forged data-only trust row cannot replace disk measurement.
        let trusted = trust::TrustRecord {
            path: record.producer_path.clone().into(),
            sha256: BINARY_SHA.into(),
            device: 7,
            inode: 8,
            mtime: 9,
        };
        ConfigKv::set_with_conn(
            &txn,
            "agent.trust.opencode",
            &serde_json::to_string(&trusted).unwrap(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            error(
                verify_with_conn(
                    &txn,
                    Path::new("/synthetic"),
                    &raw,
                    &coordinates(&r),
                    &fields()
                )
                .await
            ),
            VerificationError::Runtime
        );
        // Optional controlled local acceptance uses the actual immutable private
        // artifact. The key/receipt remain explicitly SYNTHETIC test data; no
        // live provider observation, registration challenge or native GO.
        if let Ok(producer) = std::env::var("LIBRA_NATIVE_TEST_PRODUCER") {
            let repo = tempfile::tempdir().unwrap();
            let plugin = repo.path().join(".opencode/plugin");
            fs::create_dir_all(&plugin).unwrap();
            fs::write(plugin.join("libra-hooks.js"), &m.observer_bytes).unwrap();
            let measured = measure_runtime(Path::new(&producer), repo.path()).unwrap();
            assert_eq!(measured.producer.sha256, BINARY_SHA);
            let mut active = record.clone();
            active.producer_path = measured.producer.canonical_path.to_str().unwrap().into();
            let trusted = trust::TrustRecord {
                path: measured.producer.canonical_path.clone(),
                sha256: measured.producer.sha256.clone(),
                device: measured.producer.device,
                inode: measured.producer.inode,
                mtime: measured.producer.mtime,
            };
            let dirs = serde_json::to_string(&[trusted.path.parent().unwrap()]).unwrap();
            let row = registry::Registry {
                version: 1,
                epoch: 4,
                issuers: vec![active],
            };
            ConfigKv::set_with_conn(
                &txn,
                registry::REGISTRY_KEY,
                &serde_json::to_string(&row).unwrap(),
                false,
            )
            .await
            .unwrap();
            ConfigKv::set_with_conn(
                &txn,
                "agent.trust.opencode",
                &serde_json::to_string(&trusted).unwrap(),
                false,
            )
            .await
            .unwrap();
            ConfigKv::set_with_conn(&txn, trust::TRUSTED_DIRS_KEY, &dirs, false)
                .await
                .unwrap();
            let verified = verify_with_conn(&txn, repo.path(), &raw, &coordinates(&r), &fields())
                .await
                .unwrap();
            assert_eq!(verified.receipt(), &r);
            ConfigKv::set_with_conn(&txn, trust::TRUSTED_DIRS_KEY, "[]", false)
                .await
                .unwrap();
            assert_eq!(
                error(verify_with_conn(&txn, repo.path(), &raw, &coordinates(&r), &fields()).await),
                VerificationError::Authority
            );
            ConfigKv::set_with_conn(&txn, trust::TRUSTED_DIRS_KEY, &dirs, false)
                .await
                .unwrap();
            let mut drifted = trusted.clone();
            drifted.inode = drifted.inode.wrapping_add(1);
            ConfigKv::set_with_conn(
                &txn,
                "agent.trust.opencode",
                &serde_json::to_string(&drifted).unwrap(),
                false,
            )
            .await
            .unwrap();
            assert_eq!(
                error(verify_with_conn(&txn, repo.path(), &raw, &coordinates(&r), &fields()).await),
                VerificationError::Authority
            );
            trust::revoke_trust_with_conn(&txn, trust::OPENCODE_EXPORTER_SLUG)
                .await
                .unwrap();
            assert_eq!(
                error(verify_with_conn(&txn, repo.path(), &raw, &coordinates(&r), &fields()).await),
                VerificationError::Authority
            );
            eprintln!(
                "NPR11 actual private artifact/current-transaction verification exercised; synthetic key/receipt only, native origin UNPROVEN"
            );
        }
        txn.rollback().await.unwrap();
        assert!(registry::read_with_conn(&conn).await.unwrap().is_none());
    }

    #[test]
    fn npr_11_closed_type() {
        let (r, record, m) = input();
        let value = verify_bound(r.clone(), &record, &m, &coordinates(&r), &fields()).unwrap();
        assert_eq!(value.receipt(), &r);
        let implementation = include_str!("verify.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let type_body = implementation
            .split("struct VerifiedProducerReceipt {")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        assert!(!type_body.contains("pub"));
        assert!(!implementation.contains("impl Default"));
        assert!(!implementation.contains("Deserialize"));
        assert!(!implementation.contains("Serialize"));
        assert!(!implementation.contains("&mut codec::Receipt"));
    }

    #[test]
    fn npr_11_no_factory() {
        let implementation = include_str!("verify.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "reasoning::",
            "NativeProven",
            "OpaqueEncryptedBytes",
            "impl From<",
            "impl TryFrom<",
        ] {
            assert!(
                !implementation.contains(forbidden),
                "unexpected alternate authority: {forbidden}"
            );
        }
        assert_eq!(
            implementation
                .matches("Ok(VerifiedProducerReceipt { receipt })")
                .count(),
            1
        );
        for (variant, expected) in [
            (
                VerificationError::Signature,
                "Native-origin signature is invalid; discard the proof and keep capture partial",
            ),
            (
                VerificationError::Source,
                "Native-origin source coordinates differ; discard the proof and keep capture partial",
            ),
            (
                VerificationError::Field,
                "Native-origin original fields differ; discard the proof and keep capture partial",
            ),
            (
                VerificationError::Runtime,
                "Native-origin runtime or observer policy changed; keep capture partial and prepare an explicitly registered approved installation",
            ),
            (
                VerificationError::Authority,
                "Native-origin current issuer permission is absent or changed; keep capture partial and check explicit exporter trust and issuer registration",
            ),
        ] {
            let message = variant.to_string();
            assert_eq!(message, expected);
            assert!(message.starts_with("Native-origin ") && message.contains("partial"));
            assert!(!message.contains(VALUE));
        }
    }
}
