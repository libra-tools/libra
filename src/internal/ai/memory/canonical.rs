use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    super::context_budget::MemoryAnchorConfidence,
    domain::{
        ActorKind, ActorRefV1, CodeChangeStatus, CompileOriginV1, CompileRecordV1,
        CompletionStatus, EpisodeClaimV1, EpisodeCodeContextV1, EpisodeOmissionsV1,
        EpisodePayloadV1, EpisodeRootKind, EpistemicStatus, EvidenceKind, EvidenceLocatorV1,
        EvidenceRefV1, EvidenceSourcePlane, EvidenceVisibility, IdempotencyScopeV1,
        MemoryContractError, MemoryEntityMentionV1, MemoryEntityRole, MemoryKind, MemoryLifecycle,
        MemoryLinkKind, MemoryLinkV1, MemoryNoteV1, MemoryScopeV1, MemorySensitivity, MemoryTrust,
        MemoryVisibility, ToolCallPart,
    },
};

#[derive(Serialize)]
struct CanonicalMemoryNoteV1<'a> {
    schema_version: u32,
    note_id: &'a Uuid,
    namespace: &'a str,
    path: &'a str,
    kind: MemoryKind,
    scope: &'a MemoryScopeV1,
    visibility: MemoryVisibility,
    acl_policy_id: &'a str,
    lifecycle: MemoryLifecycle,
    body: &'a str,
    rationale: &'a Option<String>,
    episode: Option<CanonicalEpisodePayloadV1<'a>>,
    evidence_refs: Vec<CanonicalEvidenceRefV1<'a>>,
    links: Vec<CanonicalMemoryLinkV1<'a>>,
    entities: Vec<CanonicalMemoryEntityMentionV1<'a>>,
    tags: &'a [String],
    confidence: MemoryAnchorConfidence,
    trust: MemoryTrust,
    sensitivity: MemorySensitivity,
    valid_from: &'a Option<DateTime<Utc>>,
    valid_until: &'a Option<DateTime<Utc>>,
    effective_from_commit: &'a Option<String>,
    effective_until_commit: &'a Option<String>,
    expires_at: &'a Option<DateTime<Utc>>,
    author: CanonicalActorRefV1<'a>,
    created_at: &'a DateTime<Utc>,
    compile_record: CanonicalCompileRecordV1<'a>,
}

impl<'a> From<&'a MemoryNoteV1> for CanonicalMemoryNoteV1<'a> {
    fn from(note: &'a MemoryNoteV1) -> Self {
        Self {
            schema_version: note.schema_version,
            note_id: &note.note_id,
            namespace: &note.namespace,
            path: &note.path,
            kind: note.kind,
            scope: &note.scope,
            visibility: note.visibility,
            acl_policy_id: &note.acl_policy_id,
            lifecycle: note.lifecycle,
            body: &note.body,
            rationale: &note.rationale,
            episode: note.episode.as_ref().map(Into::into),
            evidence_refs: note.evidence_refs.iter().map(Into::into).collect(),
            links: note.links.iter().map(Into::into).collect(),
            entities: note.entities.iter().map(Into::into).collect(),
            tags: &note.tags,
            confidence: note.confidence,
            trust: note.trust,
            sensitivity: note.sensitivity,
            valid_from: &note.valid_from,
            valid_until: &note.valid_until,
            effective_from_commit: &note.effective_from_commit,
            effective_until_commit: &note.effective_until_commit,
            expires_at: &note.expires_at,
            author: (&note.author).into(),
            created_at: &note.created_at,
            compile_record: (&note.compile_record).into(),
        }
    }
}

#[derive(Serialize)]
struct CanonicalEpisodePayloadV1<'a> {
    schema_version: u32,
    root_kind: EpisodeRootKind,
    root_id: &'a str,
    related_intent_ids: &'a [String],
    related_task_ids: &'a [String],
    related_run_ids: &'a [String],
    started_at: &'a Option<DateTime<Utc>>,
    ended_at: &'a Option<DateTime<Utc>>,
    goal: CanonicalEpisodeClaimV1<'a>,
    completion_status: CompletionStatus,
    code_change_status: CodeChangeStatus,
    summary: CanonicalEpisodeClaimV1<'a>,
    observations: Vec<CanonicalEpisodeClaimV1<'a>>,
    inferences: Vec<CanonicalEpisodeClaimV1<'a>>,
    decisions: Vec<CanonicalEpisodeClaimV1<'a>>,
    failed_attempts: Vec<CanonicalEpisodeClaimV1<'a>>,
    unresolved: Vec<CanonicalEpisodeClaimV1<'a>>,
    code: CanonicalEpisodeCodeContextV1<'a>,
    omissions: CanonicalEpisodeOmissionsV1,
}

impl<'a> From<&'a EpisodePayloadV1> for CanonicalEpisodePayloadV1<'a> {
    fn from(payload: &'a EpisodePayloadV1) -> Self {
        Self {
            schema_version: payload.schema_version,
            root_kind: payload.root_kind,
            root_id: &payload.root_id,
            related_intent_ids: &payload.related_intent_ids,
            related_task_ids: &payload.related_task_ids,
            related_run_ids: &payload.related_run_ids,
            started_at: &payload.started_at,
            ended_at: &payload.ended_at,
            goal: (&payload.goal).into(),
            completion_status: payload.completion_status,
            code_change_status: payload.code_change_status,
            summary: (&payload.summary).into(),
            observations: payload.observations.iter().map(Into::into).collect(),
            inferences: payload.inferences.iter().map(Into::into).collect(),
            decisions: payload.decisions.iter().map(Into::into).collect(),
            failed_attempts: payload.failed_attempts.iter().map(Into::into).collect(),
            unresolved: payload.unresolved.iter().map(Into::into).collect(),
            code: (&payload.code).into(),
            omissions: (&payload.omissions).into(),
        }
    }
}

#[derive(Serialize)]
struct CanonicalEpisodeClaimV1<'a> {
    epistemic_status: EpistemicStatus,
    claim: &'a str,
    confidence: &'a Option<MemoryAnchorConfidence>,
    evidence_refs: Vec<CanonicalEvidenceRefV1<'a>>,
}

impl<'a> From<&'a EpisodeClaimV1> for CanonicalEpisodeClaimV1<'a> {
    fn from(claim: &'a EpisodeClaimV1) -> Self {
        Self {
            epistemic_status: claim.epistemic_status,
            claim: &claim.claim,
            confidence: &claim.confidence,
            evidence_refs: claim.evidence_refs.iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
struct CanonicalEpisodeCodeContextV1<'a> {
    base_oid: &'a Option<String>,
    result_oid: &'a Option<String>,
    branch_ref: &'a Option<String>,
    paths: &'a [String],
}

impl<'a> From<&'a EpisodeCodeContextV1> for CanonicalEpisodeCodeContextV1<'a> {
    fn from(code: &'a EpisodeCodeContextV1) -> Self {
        Self {
            base_oid: &code.base_oid,
            result_oid: &code.result_oid,
            branch_ref: &code.branch_ref,
            paths: &code.paths,
        }
    }
}

#[derive(Serialize)]
struct CanonicalEpisodeOmissionsV1 {
    related_run_ids: u32,
    observations: u32,
    inferences: u32,
    decisions: u32,
    failed_attempts: u32,
    unresolved: u32,
}

impl From<&EpisodeOmissionsV1> for CanonicalEpisodeOmissionsV1 {
    fn from(omissions: &EpisodeOmissionsV1) -> Self {
        Self {
            related_run_ids: omissions.related_run_ids,
            observations: omissions.observations,
            inferences: omissions.inferences,
            decisions: omissions.decisions,
            failed_attempts: omissions.failed_attempts,
            unresolved: omissions.unresolved,
        }
    }
}

#[derive(Serialize)]
struct CanonicalEvidenceRefV1<'a> {
    schema_version: u32,
    source_plane: EvidenceSourcePlane,
    kind: EvidenceKind,
    object_id: &'a str,
    locator: CanonicalEvidenceLocatorV1<'a>,
    visibility: EvidenceVisibility,
    captured_at: &'a Option<DateTime<Utc>>,
    code_commit: &'a Option<String>,
}

impl<'a> From<&'a EvidenceRefV1> for CanonicalEvidenceRefV1<'a> {
    fn from(evidence: &'a EvidenceRefV1) -> Self {
        Self {
            schema_version: evidence.schema_version,
            source_plane: evidence.source_plane,
            kind: evidence.kind,
            object_id: &evidence.object_id,
            locator: (&evidence.locator).into(),
            visibility: evidence.visibility,
            captured_at: &evidence.captured_at,
            code_commit: &evidence.code_commit,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CanonicalEvidenceLocatorV1<'a> {
    Object,
    EventSeq {
        event_seq: u64,
    },
    JsonPointer {
        pointer: &'a str,
    },
    SessionFragment {
        start_seq: u64,
        end_seq: u64,
    },
    ToolCall {
        invocation_id: &'a str,
        part: ToolCallPart,
    },
    CodeRange {
        commit_oid: &'a str,
        path: &'a str,
        start_line: u32,
        end_line: u32,
    },
}

impl<'a> From<&'a EvidenceLocatorV1> for CanonicalEvidenceLocatorV1<'a> {
    fn from(locator: &'a EvidenceLocatorV1) -> Self {
        match locator {
            EvidenceLocatorV1::Object => Self::Object,
            EvidenceLocatorV1::EventSeq { event_seq } => Self::EventSeq {
                event_seq: *event_seq,
            },
            EvidenceLocatorV1::JsonPointer { pointer } => Self::JsonPointer { pointer },
            EvidenceLocatorV1::SessionFragment { start_seq, end_seq } => Self::SessionFragment {
                start_seq: *start_seq,
                end_seq: *end_seq,
            },
            EvidenceLocatorV1::ToolCall {
                invocation_id,
                part,
            } => Self::ToolCall {
                invocation_id,
                part: *part,
            },
            EvidenceLocatorV1::CodeRange {
                commit_oid,
                path,
                start_line,
                end_line,
            } => Self::CodeRange {
                commit_oid,
                path,
                start_line: *start_line,
                end_line: *end_line,
            },
        }
    }
}

#[derive(Serialize)]
struct CanonicalMemoryLinkV1<'a> {
    kind: MemoryLinkKind,
    target_note_id: &'a Uuid,
    evidence_refs: Vec<CanonicalEvidenceRefV1<'a>>,
    valid_from: &'a Option<DateTime<Utc>>,
    valid_until: &'a Option<DateTime<Utc>>,
}

impl<'a> From<&'a MemoryLinkV1> for CanonicalMemoryLinkV1<'a> {
    fn from(link: &'a MemoryLinkV1) -> Self {
        Self {
            kind: link.kind,
            target_note_id: &link.target_note_id,
            evidence_refs: link.evidence_refs.iter().map(Into::into).collect(),
            valid_from: &link.valid_from,
            valid_until: &link.valid_until,
        }
    }
}

#[derive(Serialize)]
struct CanonicalMemoryEntityMentionV1<'a> {
    schema_version: u32,
    canonical_key: &'a str,
    display_name: &'a str,
    aliases: &'a [String],
    role: MemoryEntityRole,
    resolution_confidence: MemoryAnchorConfidence,
    evidence_refs: Vec<CanonicalEvidenceRefV1<'a>>,
}

impl<'a> From<&'a MemoryEntityMentionV1> for CanonicalMemoryEntityMentionV1<'a> {
    fn from(entity: &'a MemoryEntityMentionV1) -> Self {
        Self {
            schema_version: entity.schema_version,
            canonical_key: &entity.canonical_key,
            display_name: &entity.display_name,
            aliases: &entity.aliases,
            role: entity.role,
            resolution_confidence: entity.resolution_confidence,
            evidence_refs: entity.evidence_refs.iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
struct CanonicalActorRefV1<'a> {
    kind: ActorKind,
    principal_id: &'a str,
}

impl<'a> From<&'a ActorRefV1> for CanonicalActorRefV1<'a> {
    fn from(actor: &'a ActorRefV1) -> Self {
        Self {
            kind: actor.kind,
            principal_id: &actor.principal_id,
        }
    }
}

#[derive(Serialize)]
struct CanonicalCompileRecordV1<'a> {
    schema_version: u32,
    origin: CompileOriginV1,
    producer: &'a str,
    rules_version: u32,
    prompt_version: &'a Option<String>,
    model_id: &'a Option<String>,
    policy_version: &'a str,
    input_hashes: &'a [String],
    idempotency_key: &'a str,
    idempotency_scope: IdempotencyScopeV1,
}

impl<'a> From<&'a CompileRecordV1> for CanonicalCompileRecordV1<'a> {
    fn from(record: &'a CompileRecordV1) -> Self {
        Self {
            schema_version: record.schema_version,
            origin: record.origin,
            producer: &record.producer,
            rules_version: record.rules_version,
            prompt_version: &record.prompt_version,
            model_id: &record.model_id,
            policy_version: &record.policy_version,
            input_hashes: &record.input_hashes,
            idempotency_key: &record.idempotency_key,
            idempotency_scope: record.idempotency_scope,
        }
    }
}

pub(super) fn memory_note_content_digest_v1(
    note: &MemoryNoteV1,
) -> Result<String, MemoryContractError> {
    let payload = memory_note_canonical_payload_v1(note)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(payload))))
}

pub(super) fn verify_memory_note_content_digest_v1(
    note: &MemoryNoteV1,
) -> Result<(), MemoryContractError> {
    if note.content_digest != memory_note_content_digest_v1(note)? {
        return Err(MemoryContractError::InvalidField {
            field: "MemoryNote.content_digest",
        });
    }
    Ok(())
}

pub(super) fn memory_note_canonical_payload_v1(
    note: &MemoryNoteV1,
) -> Result<Vec<u8>, MemoryContractError> {
    let value = serde_json::to_value(CanonicalMemoryNoteV1::from(note)).map_err(|_| {
        MemoryContractError::InvalidJson {
            object: "MemoryNote canonical payload",
        }
    })?;

    let mut bytes = Vec::new();
    write_canonical_json(&value, &mut bytes)?;
    Ok(bytes)
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<(), MemoryContractError> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(number) => {
            if !number.is_i64() && !number.is_u64() {
                return Err(MemoryContractError::InvalidField {
                    field: "MemoryNote canonical number",
                });
            }
            output.extend_from_slice(number.to_string().as_bytes());
        }
        Value::String(value) => {
            let encoded =
                serde_json::to_vec(value).map_err(|_| MemoryContractError::InvalidJson {
                    object: "MemoryNote canonical string",
                })?;
            output.extend_from_slice(&encoded);
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(object) => {
            output.push(b'{');
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                let encoded_key =
                    serde_json::to_vec(key).map_err(|_| MemoryContractError::InvalidJson {
                        object: "MemoryNote canonical key",
                    })?;
                output.extend_from_slice(&encoded_key);
                output.push(b':');
                write_canonical_json(value, output)?;
            }
            output.push(b'}');
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        super::{
            super::context_budget::MemoryAnchorConfidence,
            domain::{
                CodeChangeStatus, CompileOriginV1, CompletionStatus, EpisodeClaimV1,
                EpisodeCodeContextV1, EpisodeOmissionsV1, EpisodePayloadV1, EpisodeRootKind,
                EpistemicStatus,
            },
            validation::parse_memory_note_v1,
        },
        memory_note_content_digest_v1, verify_memory_note_content_digest_v1,
    };

    const SHA1_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA256_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn note_fixture() -> super::super::domain::MemoryNoteV1 {
        let value = serde_json::json!({
            "schema_version": 1,
            "note_id": "98809d1c-f0cd-5e98-84b8-c1dddf5aeb19",
            "content_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "namespace": "default",
            "path": "episodic.tasks.r-7461736b2d3432",
            "kind": "episodic",
            "scope": { "type": "repo" },
            "visibility": "repo_local",
            "acl_policy_id": "repo-policy-v1",
            "lifecycle": "accretive",
            "body": "The retry clock caused two failed attempts.",
            "rationale": "Keep the failure chain available to later agents.",
            "episode": null,
            "evidence_refs": [{
                "schema_version": 1,
                "source_plane": "agent_runtime",
                "kind": "task",
                "object_id": "task-42",
                "source_ref_oid": SHA1_OID,
                "locator": { "type": "object" },
                "fragment_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                "visibility": "repo_local",
                "captured_at": null,
                "code_commit": null
            }],
            "links": [{
                "kind": "supports",
                "target_note_id": "760369f7-ba78-541a-9aae-4e899154530b",
                "target_revision_oid": SHA1_OID,
                "evidence_refs": [],
                "valid_from": null,
                "valid_until": null
            }],
            "entities": [],
            "parents": [SHA1_OID],
            "tags": ["retry"],
            "confidence": "high",
            "trust": "repo_evidence",
            "sensitivity": "internal",
            "valid_from": null,
            "valid_until": null,
            "effective_from_commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "effective_until_commit": null,
            "expires_at": null,
            "author": { "kind": "agent", "principal_id": "agent:test" },
            "created_at": "2026-08-20T09:00:00Z",
            "compile_record": {
                "schema_version": 1,
                "origin": "explicit",
                "producer": "libra-memory/1",
                "rules_version": 1,
                "prompt_version": null,
                "model_id": null,
                "policy_version": "repo-policy-v1",
                "input_hashes": ["sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
                "idempotency_key": "hmac-sha256:key-1:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "idempotency_scope": "cell"
            }
        });

        let mut note: super::super::domain::MemoryNoteV1 =
            serde_json::from_value(value).expect("fixture has a valid MemoryNote shape");
        note.content_digest = memory_note_content_digest_v1(&note).expect("fixture canonicalizes");

        parse_memory_note_v1(&serde_json::to_vec(&note).expect("fixture serializes"))
            .expect("fixture is a valid MemoryNote")
    }

    fn episode_note_fixture() -> super::super::domain::MemoryNoteV1 {
        let mut note = note_fixture();
        let evidence_ref = note.evidence_refs[0].clone();
        let observation = EpisodeClaimV1 {
            epistemic_status: EpistemicStatus::Observation,
            claim: "the retry test failed twice".to_string(),
            confidence: None,
            evidence_refs: vec![evidence_ref.clone()],
        };
        let inference = EpisodeClaimV1 {
            epistemic_status: EpistemicStatus::Inference,
            claim: "the retry clock is probably nondeterministic".to_string(),
            confidence: Some(MemoryAnchorConfidence::High),
            evidence_refs: vec![evidence_ref],
        };
        note.episode = Some(EpisodePayloadV1 {
            schema_version: 1,
            root_kind: EpisodeRootKind::Task,
            root_id: "task-42".to_string(),
            related_intent_ids: vec!["intent-9".to_string()],
            related_task_ids: vec!["task-42".to_string()],
            related_run_ids: vec!["run-1".to_string(), "run-2".to_string()],
            started_at: Utc.with_ymd_and_hms(2026, 8, 20, 8, 0, 0).single(),
            ended_at: Utc.with_ymd_and_hms(2026, 8, 20, 9, 0, 0).single(),
            goal: observation.clone(),
            completion_status: CompletionStatus::Completed,
            code_change_status: CodeChangeStatus::Changed,
            summary: inference.clone(),
            observations: vec![observation],
            inferences: vec![inference],
            decisions: Vec::new(),
            failed_attempts: Vec::new(),
            unresolved: Vec::new(),
            code: EpisodeCodeContextV1 {
                base_oid: Some(SHA1_OID.to_string()),
                result_oid: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
                branch_ref: Some("refs/heads/feature/retry".to_string()),
                paths: vec!["src/retry.rs".to_string()],
            },
            omissions: EpisodeOmissionsV1::default(),
        });
        note.compile_record.origin = CompileOriginV1::EpisodeCompiler;
        note.compile_record.prompt_version = Some("episode-v1".to_string());
        note.compile_record.model_id = Some("synthetic-model".to_string());
        note
    }

    #[test]
    fn content_digest_has_a_storage_oid_independent_golden_vector() {
        let note = note_fixture();
        let digest = memory_note_content_digest_v1(&note).expect("note canonicalizes");
        assert_eq!(
            digest,
            "sha256:74c9998dbdda299afe9b90d80ad3c5493534b4d9f3a32fd3e33ccf6d733e004b",
        );

        let mut different_storage = note.clone();
        different_storage.parents[0] = SHA256_OID.to_string();
        different_storage.evidence_refs[0].source_ref_oid = SHA256_OID.to_string();
        different_storage.evidence_refs[0].fragment_digest = format!("sha256:{}", "1".repeat(64));
        different_storage.links[0].target_revision_oid = Some(SHA256_OID.to_string());
        assert_eq!(
            digest,
            memory_note_content_digest_v1(&different_storage).expect("note canonicalizes"),
        );

        let mut different_body = note.clone();
        different_body.body.push_str(" A third attempt succeeded.");
        assert_ne!(
            digest,
            memory_note_content_digest_v1(&different_body).expect("note canonicalizes"),
        );

        let mut different_code_anchor = note;
        different_code_anchor.effective_from_commit = Some("2".repeat(40));
        assert_ne!(
            digest,
            memory_note_content_digest_v1(&different_code_anchor).expect("note canonicalizes"),
        );
    }

    #[test]
    fn semantic_code_anchor_has_sha1_and_sha256_golden_vectors() {
        let sha1_note = note_fixture();
        assert_eq!(
            memory_note_content_digest_v1(&sha1_note).expect("SHA-1 note canonicalizes"),
            "sha256:74c9998dbdda299afe9b90d80ad3c5493534b4d9f3a32fd3e33ccf6d733e004b",
        );

        let mut sha256_note = sha1_note;
        sha256_note.effective_from_commit = Some(SHA256_OID.to_string());
        assert_eq!(
            memory_note_content_digest_v1(&sha256_note).expect("SHA-256 note canonicalizes"),
            "sha256:a4be4edf791a7773d7316ece36caac545f868224d8d13d7fb49063d00b1f653b",
        );
    }

    #[test]
    fn content_digest_verification_detects_semantic_tampering() {
        let mut note = note_fixture();
        note.content_digest = memory_note_content_digest_v1(&note).expect("note canonicalizes");
        verify_memory_note_content_digest_v1(&note).expect("matching digest is valid");

        note.body.push_str(" Tampered after hashing.");
        assert!(verify_memory_note_content_digest_v1(&note).is_err());
    }

    #[test]
    fn canonical_episode_payload_golden() {
        let note = episode_note_fixture();
        let digest = memory_note_content_digest_v1(&note).expect("Episode note canonicalizes");
        assert_eq!(
            digest,
            "sha256:a6112fef9230e538eff93eca6c9449009728145054571c9a2c0bb8b14f52b4e9",
        );

        let mut different_evidence_storage = note.clone();
        different_evidence_storage.parents[0] = SHA256_OID.to_string();
        different_evidence_storage.evidence_refs[0].source_ref_oid = SHA256_OID.to_string();
        different_evidence_storage.evidence_refs[0].fragment_digest =
            format!("sha256:{}", "2".repeat(64));
        different_evidence_storage.links[0].target_revision_oid = Some(SHA256_OID.to_string());
        let claim = &mut different_evidence_storage
            .episode
            .as_mut()
            .expect("fixture carries Episode")
            .summary;
        claim.evidence_refs[0].source_ref_oid = SHA256_OID.to_string();
        claim.evidence_refs[0].fragment_digest = format!("sha256:{}", "3".repeat(64));
        assert_eq!(
            digest,
            memory_note_content_digest_v1(&different_evidence_storage)
                .expect("Episode note canonicalizes"),
        );

        let mut different_summary = note;
        different_summary
            .episode
            .as_mut()
            .expect("fixture carries Episode")
            .summary
            .claim
            .push_str(" A controllable clock should fix it.");
        assert_ne!(
            digest,
            memory_note_content_digest_v1(&different_summary).expect("Episode note canonicalizes"),
        );
    }
}
