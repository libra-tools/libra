use super::domain::{
    CompileOriginV1, CompileRecordV1, EpisodeClaimV1, EpisodeCodeContextV1, EpisodePayloadV1,
    EpisodeRoot, EpisodeRootKind, EpistemicStatus, EvidenceLocatorV1, EvidenceRefV1,
    IdempotencyScopeV1, MemoryContractError, MemoryEventAction, MemoryEventV1, MemoryKind,
    MemoryLifecycle, MemoryLinkKind, MemoryNoteV1, MemoryScopeV1, MemoryVisibility,
    TrustedEpisodeFieldsV1,
};

const MEMORY_SCHEMA_VERSION_V1: u32 = 1;
const MAX_COMPILE_RECORD_BYTES: usize = 32 * 1024;
const MAX_MEMORY_NOTE_BYTES: usize = 256 * 1024;
const MAX_MEMORY_EVENT_BYTES: usize = 128 * 1024;
const MAX_EVIDENCE_REF_BYTES: usize = 16 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 512;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_SESSION_FRAGMENT_ITEMS: u64 = 256;
const MAX_EPISODE_TEXT_BYTES: usize = 4 * 1024;
const MAX_EPISODE_COLLECTION_ITEMS: usize = 128;
const MAX_EPISODE_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_MEMORY_NOTE_BODY_BYTES: usize = 16 * 1024;
const MAX_MEMORY_NOTE_PARENTS: usize = 1;

pub(super) fn parse_compile_record_v1(
    bytes: &[u8],
) -> Result<CompileRecordV1, MemoryContractError> {
    validate_input_size(bytes, MAX_COMPILE_RECORD_BYTES, "CompileRecord.size")?;
    let record = serde_json::from_slice(bytes).map_err(|_| MemoryContractError::InvalidJson {
        object: "CompileRecord",
    })?;
    validate_compile_record(&record)?;
    Ok(record)
}

pub(super) fn parse_memory_note_v1(bytes: &[u8]) -> Result<MemoryNoteV1, MemoryContractError> {
    validate_input_size(bytes, MAX_MEMORY_NOTE_BYTES, "MemoryNote.size")?;
    let note = serde_json::from_slice(bytes).map_err(|_| MemoryContractError::InvalidJson {
        object: "MemoryNote",
    })?;
    validate_memory_note(&note)?;
    super::canonical::verify_memory_note_content_digest_v1(&note)?;
    Ok(note)
}

pub(super) fn parse_memory_event_v1(bytes: &[u8]) -> Result<MemoryEventV1, MemoryContractError> {
    validate_input_size(bytes, MAX_MEMORY_EVENT_BYTES, "MemoryEvent.size")?;
    let event = serde_json::from_slice(bytes).map_err(|_| MemoryContractError::InvalidJson {
        object: "MemoryEvent",
    })?;
    validate_memory_event(&event)?;
    Ok(event)
}

pub(super) fn parse_episode_payload_v1(
    bytes: &[u8],
    trusted: &TrustedEpisodeFieldsV1,
) -> Result<EpisodePayloadV1, MemoryContractError> {
    validate_input_size(bytes, MAX_EPISODE_PAYLOAD_BYTES, "EpisodePayload.size")?;
    let payload = serde_json::from_slice(bytes).map_err(|_| MemoryContractError::InvalidJson {
        object: "EpisodePayload",
    })?;
    validate_episode_payload(&payload, trusted)?;
    Ok(payload)
}

pub(super) fn parse_evidence_ref_v1(bytes: &[u8]) -> Result<EvidenceRefV1, MemoryContractError> {
    validate_input_size(bytes, MAX_EVIDENCE_REF_BYTES, "EvidenceRef.size")?;
    let evidence_ref =
        serde_json::from_slice(bytes).map_err(|_| MemoryContractError::InvalidJson {
            object: "EvidenceRef",
        })?;
    validate_evidence_ref(&evidence_ref)?;
    Ok(evidence_ref)
}

pub(super) fn validate_episode_payload(
    payload: &EpisodePayloadV1,
    trusted: &TrustedEpisodeFieldsV1,
) -> Result<(), MemoryContractError> {
    validate_episode_payload_shape(payload)?;
    if payload.root_kind != trusted.root.kind()
        || payload.root_id != trusted.root.id()
        || payload.related_intent_ids != trusted.related_intent_ids
        || payload.related_task_ids != trusted.related_task_ids
        || payload.related_run_ids != trusted.related_run_ids
        || payload.started_at != trusted.started_at
        || payload.ended_at != trusted.ended_at
        || payload.goal != trusted.goal
        || payload.completion_status != trusted.completion_status
        || payload.code_change_status != trusted.code_change_status
        || payload.code != trusted.code
        || payload.omissions != trusted.omissions
    {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.trusted_fields",
        });
    }

    Ok(())
}

fn validate_episode_payload_shape(payload: &EpisodePayloadV1) -> Result<(), MemoryContractError> {
    if payload.schema_version != MEMORY_SCHEMA_VERSION_V1 {
        return Err(MemoryContractError::UnsupportedSchemaVersion {
            object: "EpisodePayload",
            version: payload.schema_version,
        });
    }
    if payload
        .started_at
        .zip(payload.ended_at)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.time_range",
        });
    }
    validate_related_ids(payload)?;
    validate_code_context(&payload.code)?;

    validate_episode_claim(&payload.goal)?;
    if payload.goal.epistemic_status != EpistemicStatus::Observation {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.goal",
        });
    }
    validate_episode_claim(&payload.summary)?;
    if payload.summary.epistemic_status != EpistemicStatus::Inference {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.summary",
        });
    }
    validate_claim_collection(&payload.observations, Some(EpistemicStatus::Observation))?;
    validate_claim_collection(&payload.inferences, Some(EpistemicStatus::Inference))?;
    validate_claim_collection(&payload.decisions, None)?;
    validate_claim_collection(&payload.failed_attempts, None)?;
    validate_claim_collection(&payload.unresolved, None)?;

    let encoded = serde_json::to_vec(payload).map_err(|_| MemoryContractError::InvalidField {
        field: "EpisodePayload.encoding",
    })?;
    if encoded.len() > MAX_EPISODE_PAYLOAD_BYTES {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.size",
        });
    }

    Ok(())
}

fn validate_compile_record(record: &CompileRecordV1) -> Result<(), MemoryContractError> {
    if record.schema_version != MEMORY_SCHEMA_VERSION_V1 {
        return Err(MemoryContractError::UnsupportedSchemaVersion {
            object: "CompileRecord",
            version: record.schema_version,
        });
    }
    if !is_bounded_nonempty(&record.producer, MAX_IDENTIFIER_BYTES)
        || record.rules_version == 0
        || !is_bounded_nonempty(&record.policy_version, MAX_IDENTIFIER_BYTES)
        || record.input_hashes.is_empty()
        || record.input_hashes.len() > MAX_EPISODE_COLLECTION_ITEMS
        || record
            .input_hashes
            .iter()
            .any(|hash| !is_sha256_digest(hash) && !is_hmac_sha256_digest(hash))
        || record
            .input_hashes
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || !is_hmac_sha256_digest(&record.idempotency_key)
        || record
            .prompt_version
            .as_deref()
            .is_some_and(|value| !is_bounded_nonempty(value, MAX_IDENTIFIER_BYTES))
        || record
            .model_id
            .as_deref()
            .is_some_and(|value| !is_bounded_nonempty(value, MAX_IDENTIFIER_BYTES))
        || record.prompt_version.is_some() != record.model_id.is_some()
        || (record.origin == CompileOriginV1::EpisodeCompiler && record.model_id.is_none())
        || (record.idempotency_scope == IdempotencyScopeV1::Namespace
            && !matches!(
                record.origin,
                CompileOriginV1::Consolidation | CompileOriginV1::Onboard
            ))
    {
        return Err(MemoryContractError::InvalidField {
            field: "CompileRecord",
        });
    }

    Ok(())
}

fn validate_memory_note(note: &MemoryNoteV1) -> Result<(), MemoryContractError> {
    if note.schema_version != MEMORY_SCHEMA_VERSION_V1 {
        return Err(MemoryContractError::UnsupportedSchemaVersion {
            object: "MemoryNote",
            version: note.schema_version,
        });
    }
    if !is_sha256_digest(&note.content_digest)
        || !is_valid_memory_scope(&note.scope)
        || !is_bounded_nonempty(&note.namespace, MAX_IDENTIFIER_BYTES)
        || !is_bounded_nonempty(&note.path, MAX_PATH_BYTES)
        || !is_bounded_nonempty(&note.acl_policy_id, MAX_IDENTIFIER_BYTES)
        || note.body.is_empty()
        || note.body.len() > MAX_MEMORY_NOTE_BODY_BYTES
        || note
            .rationale
            .as_deref()
            .is_some_and(|value| value.len() > MAX_EPISODE_TEXT_BYTES)
        || !is_bounded_nonempty(&note.author.principal_id, MAX_IDENTIFIER_BYTES)
        || note.evidence_refs.len() > MAX_EPISODE_COLLECTION_ITEMS
        || note.links.len() > MAX_EPISODE_COLLECTION_ITEMS
        || note.entities.len() > MAX_EPISODE_COLLECTION_ITEMS
        || note.parents.len() > MAX_MEMORY_NOTE_PARENTS
        || note.tags.len() > MAX_EPISODE_COLLECTION_ITEMS
        || note
            .valid_from
            .zip(note.valid_until)
            .is_some_and(|(from, until)| from > until)
        || note
            .effective_from_commit
            .as_deref()
            .is_some_and(|oid| !is_git_oid(oid))
        || note
            .effective_until_commit
            .as_deref()
            .is_some_and(|oid| !is_git_oid(oid))
        || note.parents.iter().any(|oid| !is_git_oid(oid))
        || note
            .tags
            .iter()
            .any(|tag| !is_bounded_nonempty(tag, MAX_IDENTIFIER_BYTES))
        || note.tags.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(MemoryContractError::InvalidField {
            field: "MemoryNote",
        });
    }
    validate_compile_record(&note.compile_record)?;
    for evidence_ref in &note.evidence_refs {
        validate_evidence_ref(evidence_ref)?;
    }
    for link in &note.links {
        if link.evidence_refs.len() > MAX_EPISODE_COLLECTION_ITEMS
            || link
                .target_revision_oid
                .as_deref()
                .is_some_and(|oid| !is_git_oid(oid))
            || link
                .valid_from
                .zip(link.valid_until)
                .is_some_and(|(from, until)| from > until)
        {
            return Err(MemoryContractError::InvalidField {
                field: "MemoryNote.links",
            });
        }
        for evidence_ref in &link.evidence_refs {
            validate_evidence_ref(evidence_ref)?;
        }
    }
    for entity in &note.entities {
        if entity.schema_version != MEMORY_SCHEMA_VERSION_V1
            || !is_bounded_nonempty(&entity.canonical_key, MAX_IDENTIFIER_BYTES)
            || !is_bounded_nonempty(&entity.display_name, MAX_EPISODE_TEXT_BYTES)
            || entity.aliases.len() > MAX_EPISODE_COLLECTION_ITEMS
            || entity
                .aliases
                .iter()
                .any(|alias| !is_bounded_nonempty(alias, MAX_EPISODE_TEXT_BYTES))
            || entity.aliases.windows(2).any(|pair| pair[0] >= pair[1])
            || entity.evidence_refs.is_empty()
            || entity.evidence_refs.len() > MAX_EPISODE_COLLECTION_ITEMS
        {
            return Err(MemoryContractError::InvalidField {
                field: "MemoryNote.entities",
            });
        }
        for evidence_ref in &entity.evidence_refs {
            validate_evidence_ref(evidence_ref)?;
        }
    }

    if note.episode.is_some() != (note.compile_record.origin == CompileOriginV1::EpisodeCompiler) {
        return Err(MemoryContractError::InvalidField {
            field: "MemoryNote.episode_origin",
        });
    }
    if let Some(payload) = &note.episode {
        validate_episode_payload_shape(payload)?;
        let root = match payload.root_kind {
            EpisodeRootKind::Task => EpisodeRoot::task(&payload.root_id),
            EpisodeRootKind::Intent => EpisodeRoot::intent(&payload.root_id),
        }?;
        let expected_effective_commit = payload
            .code
            .result_oid
            .as_ref()
            .or(payload.code.base_oid.as_ref());
        if note.kind != MemoryKind::Episodic
            || note.scope != MemoryScopeV1::Repo
            || note.visibility != MemoryVisibility::RepoLocal
            || note.lifecycle != MemoryLifecycle::Accretive
            || note.namespace != root.namespace()
            || note.path != root.path()
            || note.note_id != root.note_id()
            || note.valid_from.is_some()
            || note.valid_until.is_some()
            || note.expires_at.is_some()
            || note.effective_from_commit.as_ref() != expected_effective_commit
            || note.effective_until_commit.is_some()
            || note.compile_record.origin != CompileOriginV1::EpisodeCompiler
        {
            return Err(MemoryContractError::InvalidField {
                field: "MemoryNote.episode_envelope",
            });
        }
        validate_intent_task_links(note, payload)?;
    }

    Ok(())
}

fn validate_memory_event(event: &MemoryEventV1) -> Result<(), MemoryContractError> {
    if event.schema_version != MEMORY_SCHEMA_VERSION_V1 {
        return Err(MemoryContractError::UnsupportedSchemaVersion {
            object: "MemoryEvent",
            version: event.schema_version,
        });
    }
    if event.event_seq == 0
        || !is_bounded_nonempty(&event.actor.principal_id, MAX_IDENTIFIER_BYTES)
        || event
            .namespace
            .as_deref()
            .is_some_and(|value| !is_bounded_nonempty(value, MAX_IDENTIFIER_BYTES))
        || event
            .target_path
            .as_deref()
            .is_some_and(|value| !is_bounded_nonempty(value, MAX_PATH_BYTES))
        || event
            .reason_code
            .as_deref()
            .is_some_and(|value| !is_bounded_nonempty(value, MAX_IDENTIFIER_BYTES))
        || event.evidence_refs.len() > MAX_EPISODE_COLLECTION_ITEMS
        || event
            .revision_oid
            .as_deref()
            .is_some_and(|oid| !is_git_oid(oid))
    {
        return Err(MemoryContractError::InvalidField {
            field: "MemoryEvent",
        });
    }
    for evidence_ref in &event.evidence_refs {
        validate_evidence_ref(evidence_ref)?;
    }

    match event.action {
        MemoryEventAction::TaxonomyExpanded => {
            if event.namespace.as_deref().is_none_or(str::is_empty)
                || event.target_path.as_deref().is_none_or(str::is_empty)
                || event.note_id.is_some()
                || event.revision_oid.is_some()
                || event.next_note_id.is_some()
            {
                return Err(MemoryContractError::InvalidField {
                    field: "MemoryEvent.taxonomy_target",
                });
            }
        }
        _ if event.note_id.is_none()
            || event.revision_oid.is_none()
            || event.namespace.is_some()
            || event.target_path.is_some()
            || (event.next_note_id.is_some() && event.action != MemoryEventAction::Superseded) =>
        {
            return Err(MemoryContractError::InvalidField {
                field: "MemoryEvent.note_target",
            });
        }
        _ => {}
    }

    Ok(())
}

pub(super) fn validate_episode_claim(claim: &EpisodeClaimV1) -> Result<(), MemoryContractError> {
    if claim.claim.is_empty() || claim.claim.len() > MAX_EPISODE_TEXT_BYTES {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodeClaim.claim",
        });
    }
    if claim.evidence_refs.is_empty() || claim.evidence_refs.len() > MAX_EPISODE_COLLECTION_ITEMS {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodeClaim.evidence_refs",
        });
    }
    match (claim.epistemic_status, claim.confidence) {
        (EpistemicStatus::Observation, None) | (EpistemicStatus::Inference, Some(_)) => {}
        _ => {
            return Err(MemoryContractError::InvalidField {
                field: "EpisodeClaim.confidence",
            });
        }
    }
    for evidence_ref in &claim.evidence_refs {
        validate_evidence_ref(evidence_ref)?;
    }

    Ok(())
}

pub(super) fn validate_evidence_ref(
    evidence_ref: &EvidenceRefV1,
) -> Result<(), MemoryContractError> {
    if evidence_ref.schema_version != MEMORY_SCHEMA_VERSION_V1 {
        return Err(MemoryContractError::UnsupportedSchemaVersion {
            object: "EvidenceRef",
            version: evidence_ref.schema_version,
        });
    }
    if !is_bounded_nonempty(&evidence_ref.object_id, MAX_IDENTIFIER_BYTES) {
        return Err(MemoryContractError::InvalidField {
            field: "EvidenceRef.object_id",
        });
    }
    if !is_git_oid(&evidence_ref.source_ref_oid) {
        return Err(MemoryContractError::InvalidField {
            field: "EvidenceRef.source_ref_oid",
        });
    }
    if !is_sha256_digest(&evidence_ref.fragment_digest) {
        return Err(MemoryContractError::InvalidField {
            field: "EvidenceRef.fragment_digest",
        });
    }
    if let Some(code_commit) = &evidence_ref.code_commit
        && !is_git_oid(code_commit)
    {
        return Err(MemoryContractError::InvalidField {
            field: "EvidenceRef.code_commit",
        });
    }

    match &evidence_ref.locator {
        EvidenceLocatorV1::Object => {}
        EvidenceLocatorV1::EventSeq { event_seq } if *event_seq > 0 => {}
        EvidenceLocatorV1::JsonPointer { pointer } if is_valid_json_pointer(pointer) => {}
        EvidenceLocatorV1::SessionFragment { start_seq, end_seq }
            if start_seq > &0
                && start_seq <= end_seq
                && end_seq - start_seq < MAX_SESSION_FRAGMENT_ITEMS => {}
        EvidenceLocatorV1::ToolCall { invocation_id, .. }
            if is_bounded_nonempty(invocation_id, MAX_IDENTIFIER_BYTES) => {}
        EvidenceLocatorV1::CodeRange {
            commit_oid,
            path,
            start_line,
            end_line,
        } if is_git_oid(commit_oid)
            && is_repo_relative_path(path)
            && start_line > &0
            && start_line <= end_line
            && evidence_ref
                .code_commit
                .as_ref()
                .is_none_or(|code_commit| code_commit == commit_oid) => {}
        _ => {
            return Err(MemoryContractError::InvalidField {
                field: "EvidenceRef.locator",
            });
        }
    }
    if !locator_matches_source(evidence_ref) {
        return Err(MemoryContractError::InvalidField {
            field: "EvidenceRef.source_locator",
        });
    }

    Ok(())
}

fn is_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(is_lower_hex_digit)
}

fn is_sha256_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(is_lower_hex_digit))
}

fn is_hmac_sha256_digest(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("hmac-sha256:") else {
        return false;
    };
    let Some((key_id, digest)) = rest.split_once(':') else {
        return false;
    };
    is_bounded_nonempty(key_id, 128) && digest.len() == 64 && digest.bytes().all(is_lower_hex_digit)
}

fn is_repo_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

fn is_lower_hex_digit(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn is_bounded_nonempty(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn is_valid_memory_scope(scope: &MemoryScopeV1) -> bool {
    match scope {
        MemoryScopeV1::Repo | MemoryScopeV1::Global => true,
        MemoryScopeV1::Branch(branch_ref) => is_valid_branch_ref(branch_ref),
        MemoryScopeV1::Worktree(id) | MemoryScopeV1::Actor(id) => {
            is_bounded_nonempty(id, MAX_IDENTIFIER_BYTES)
        }
    }
}

fn is_valid_json_pointer(pointer: &str) -> bool {
    if !pointer.starts_with('/')
        || pointer.len() > MAX_PATH_BYTES
        || pointer.chars().any(char::is_control)
    {
        return false;
    }

    let mut chars = pointer.chars();
    while let Some(character) = chars.next() {
        if character == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

fn validate_input_size(
    bytes: &[u8],
    max_bytes: usize,
    field: &'static str,
) -> Result<(), MemoryContractError> {
    if bytes.len() > max_bytes {
        return Err(MemoryContractError::InvalidField { field });
    }
    Ok(())
}

fn validate_related_ids(payload: &EpisodePayloadV1) -> Result<(), MemoryContractError> {
    for (field, ids) in [
        (
            "EpisodePayload.related_intent_ids",
            &payload.related_intent_ids,
        ),
        ("EpisodePayload.related_task_ids", &payload.related_task_ids),
        ("EpisodePayload.related_run_ids", &payload.related_run_ids),
    ] {
        if ids.len() > MAX_EPISODE_COLLECTION_ITEMS
            || ids
                .iter()
                .any(|id| !is_bounded_nonempty(id, MAX_IDENTIFIER_BYTES))
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(MemoryContractError::InvalidField { field });
        }
    }

    let root_is_present = match payload.root_kind {
        EpisodeRootKind::Task => payload
            .related_task_ids
            .iter()
            .any(|id| id == &payload.root_id),
        EpisodeRootKind::Intent => payload
            .related_intent_ids
            .iter()
            .any(|id| id == &payload.root_id),
    };
    if !root_is_present {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.root_relation",
        });
    }
    if payload.root_kind == EpisodeRootKind::Intent && payload.related_task_ids.is_empty() {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.related_task_ids",
        });
    }

    Ok(())
}

fn validate_intent_task_links(
    note: &MemoryNoteV1,
    payload: &EpisodePayloadV1,
) -> Result<(), MemoryContractError> {
    if payload.root_kind != EpisodeRootKind::Intent {
        return Ok(());
    }
    if note
        .links
        .iter()
        .filter(|link| link.kind == MemoryLinkKind::Supports)
        .count()
        != payload.related_task_ids.len()
    {
        return Err(MemoryContractError::InvalidField {
            field: "MemoryNote.intent_task_links",
        });
    }

    for task_id in &payload.related_task_ids {
        let task_note_id = EpisodeRoot::task(task_id)?.note_id();
        let mut matching_links = note.links.iter().filter(|link| {
            link.target_note_id == task_note_id && link.kind == MemoryLinkKind::Supports
        });
        let Some(task_link) = matching_links.next() else {
            return Err(MemoryContractError::InvalidField {
                field: "MemoryNote.intent_task_links",
            });
        };
        if matching_links.next().is_some() || task_link.target_revision_oid.is_none() {
            return Err(MemoryContractError::InvalidField {
                field: "MemoryNote.intent_task_links",
            });
        }
    }

    Ok(())
}

fn validate_code_context(code: &EpisodeCodeContextV1) -> Result<(), MemoryContractError> {
    if code.base_oid.as_deref().is_some_and(|oid| !is_git_oid(oid))
        || code
            .result_oid
            .as_deref()
            .is_some_and(|oid| !is_git_oid(oid))
        || code
            .branch_ref
            .as_deref()
            .is_some_and(|branch_ref| !is_valid_branch_ref(branch_ref))
        || code.paths.len() > MAX_EPISODE_COLLECTION_ITEMS
        || code.paths.iter().any(|path| !is_repo_relative_path(path))
        || code.paths.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.code",
        });
    }

    Ok(())
}

fn is_valid_branch_ref(branch_ref: &str) -> bool {
    let Some(name) = branch_ref.strip_prefix("refs/heads/") else {
        return false;
    };
    is_bounded_nonempty(name, MAX_PATH_BYTES) && crate::utils::util::is_valid_refname(branch_ref)
}

fn validate_claim_collection(
    claims: &[EpisodeClaimV1],
    required_status: Option<EpistemicStatus>,
) -> Result<(), MemoryContractError> {
    if claims.len() > MAX_EPISODE_COLLECTION_ITEMS {
        return Err(MemoryContractError::InvalidField {
            field: "EpisodePayload.claims",
        });
    }
    for claim in claims {
        validate_episode_claim(claim)?;
        if required_status.is_some_and(|status| claim.epistemic_status != status) {
            return Err(MemoryContractError::InvalidField {
                field: "EpisodePayload.claim_status",
            });
        }
    }

    Ok(())
}

fn locator_matches_source(evidence_ref: &EvidenceRefV1) -> bool {
    use super::domain::{EvidenceKind, EvidenceSourcePlane};

    matches!(
        (
            evidence_ref.source_plane,
            evidence_ref.kind,
            &evidence_ref.locator,
        ),
        (
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Intent
                | EvidenceKind::Task
                | EvidenceKind::Run
                | EvidenceKind::Evidence
                | EvidenceKind::Decision
                | EvidenceKind::PatchSet,
            EvidenceLocatorV1::Object
                | EvidenceLocatorV1::EventSeq { .. }
                | EvidenceLocatorV1::JsonPointer { .. },
        ) | (
            EvidenceSourcePlane::Session,
            EvidenceKind::Session,
            EvidenceLocatorV1::SessionFragment { .. },
        ) | (
            EvidenceSourcePlane::AgentRuntime | EvidenceSourcePlane::Session,
            EvidenceKind::ToolCall,
            EvidenceLocatorV1::ToolCall { .. },
        ) | (
            EvidenceSourcePlane::Git,
            EvidenceKind::Code,
            EvidenceLocatorV1::CodeRange { .. }
        )
    )
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{
        super::{
            super::context_budget::MemoryAnchorConfidence,
            domain::{
                CodeChangeStatus, CompletionStatus, EpisodeClaimV1, EpisodeCodeContextV1,
                EpisodeOmissionsV1, EpisodePayloadV1, EpisodeRoot, EpistemicStatus, EvidenceKind,
                EvidenceLocatorV1, EvidenceRefV1, EvidenceSourcePlane, EvidenceVisibility,
                MemoryContractError, MemoryNoteV1, MemoryTrust, ToolCallPart,
                TrustedEpisodeFieldsV1,
            },
        },
        validate_episode_claim, validate_episode_payload, validate_evidence_ref,
    };

    const SOURCE_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CODE_OID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const FRAGMENT_DIGEST: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn evidence(
        source_plane: EvidenceSourcePlane,
        kind: EvidenceKind,
        locator: EvidenceLocatorV1,
    ) -> EvidenceRefV1 {
        EvidenceRefV1 {
            schema_version: 1,
            source_plane,
            kind,
            object_id: "object-1".to_string(),
            source_ref_oid: SOURCE_OID.to_string(),
            locator,
            fragment_digest: FRAGMENT_DIGEST.to_string(),
            visibility: EvidenceVisibility::RepoLocal,
            captured_at: None,
            code_commit: None,
        }
    }

    fn task_payload_fixture() -> (EpisodePayloadV1, TrustedEpisodeFieldsV1) {
        let root = EpisodeRoot::task("task-42").expect("synthetic task id is valid");
        let evidence_ref = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Evidence,
            EvidenceLocatorV1::EventSeq { event_seq: 7 },
        );
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
        let code = EpisodeCodeContextV1 {
            base_oid: Some(SOURCE_OID.to_string()),
            result_oid: Some(CODE_OID.to_string()),
            branch_ref: Some("refs/heads/feature/retry".to_string()),
            paths: vec!["src/retry.rs".to_string()],
        };
        let started_at = Utc
            .with_ymd_and_hms(2026, 8, 20, 8, 0, 0)
            .single()
            .expect("synthetic timestamp is valid");
        let ended_at = Utc
            .with_ymd_and_hms(2026, 8, 20, 9, 0, 0)
            .single()
            .expect("synthetic timestamp is valid");
        let trusted = TrustedEpisodeFieldsV1 {
            root: root.clone(),
            related_intent_ids: vec!["intent-9".to_string()],
            related_task_ids: vec!["task-42".to_string()],
            related_run_ids: vec!["run-1".to_string(), "run-2".to_string()],
            started_at: Some(started_at),
            ended_at: Some(ended_at),
            goal: observation.clone(),
            completion_status: CompletionStatus::Completed,
            code_change_status: CodeChangeStatus::Changed,
            code: code.clone(),
            omissions: EpisodeOmissionsV1::default(),
        };
        let payload = EpisodePayloadV1 {
            schema_version: 1,
            root_kind: root.kind(),
            root_id: root.id().to_string(),
            related_intent_ids: trusted.related_intent_ids.clone(),
            related_task_ids: trusted.related_task_ids.clone(),
            related_run_ids: trusted.related_run_ids.clone(),
            started_at: trusted.started_at,
            ended_at: trusted.ended_at,
            goal: trusted.goal.clone(),
            completion_status: trusted.completion_status,
            code_change_status: trusted.code_change_status,
            summary: inference.clone(),
            observations: vec![observation],
            inferences: vec![inference],
            decisions: Vec::new(),
            failed_attempts: Vec::new(),
            unresolved: Vec::new(),
            code,
            omissions: trusted.omissions.clone(),
        };

        (payload, trusted)
    }

    fn compile_record_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "origin": "explicit",
            "producer": "libra-memory/1",
            "rules_version": 1,
            "prompt_version": null,
            "model_id": null,
            "policy_version": "repo-policy-v1",
            "input_hashes": [FRAGMENT_DIGEST],
            "idempotency_key": "hmac-sha256:key-1:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "idempotency_scope": "cell"
        })
    }

    fn mark_episode_compiler(note: &mut serde_json::Value) {
        note["compile_record"]["origin"] = serde_json::json!("episode_compiler");
        note["compile_record"]["prompt_version"] = serde_json::json!("episode-v1");
        note["compile_record"]["model_id"] = serde_json::json!("synthetic-model");
    }

    fn memory_note_json() -> serde_json::Value {
        let mut note = serde_json::json!({
            "schema_version": 1,
            "note_id": "98809d1c-f0cd-5e98-84b8-c1dddf5aeb19",
            "content_digest": FRAGMENT_DIGEST,
            "namespace": "default",
            "path": "episodic.tasks.r-7461736b2d3432",
            "kind": "episodic",
            "scope": { "type": "repo" },
            "visibility": "repo_local",
            "acl_policy_id": "repo-policy-v1",
            "lifecycle": "accretive",
            "body": "The retry clock caused two failed attempts.",
            "rationale": null,
            "episode": null,
            "evidence_refs": [],
            "links": [],
            "entities": [],
            "parents": [],
            "tags": ["retry"],
            "confidence": "high",
            "trust": "repo_evidence",
            "sensitivity": "internal",
            "valid_from": null,
            "valid_until": null,
            "effective_from_commit": CODE_OID,
            "effective_until_commit": null,
            "expires_at": null,
            "author": { "kind": "agent", "principal_id": "agent:test" },
            "created_at": "2026-08-20T09:00:00Z",
            "compile_record": compile_record_json()
        });
        refresh_note_digest(&mut note);
        note
    }

    fn refresh_note_digest(note: &mut serde_json::Value) {
        let typed: MemoryNoteV1 =
            serde_json::from_value(note.clone()).expect("note fixture has a valid wire shape");
        let digest = super::super::canonical::memory_note_content_digest_v1(&typed)
            .expect("note fixture canonicalizes");
        note["content_digest"] = serde_json::json!(digest);
    }

    fn memory_event_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "event_id": "018fe3c4-4c00-7000-8000-000000000001",
            "event_seq": 1,
            "note_id": "98809d1c-f0cd-5e98-84b8-c1dddf5aeb19",
            "revision_oid": SOURCE_OID,
            "namespace": null,
            "target_path": null,
            "action": "created",
            "reason_code": null,
            "actor": { "kind": "agent", "principal_id": "agent:test" },
            "at": "2026-08-20T09:00:00Z",
            "evidence_refs": [],
            "next_note_id": null
        })
    }

    fn intent_episode_note_json() -> serde_json::Value {
        let (mut payload, _) = task_payload_fixture();
        let intent_root = EpisodeRoot::intent("intent-9").expect("synthetic intent id is valid");
        let task_root = EpisodeRoot::task("task-42").expect("synthetic task id is valid");
        payload.root_kind = intent_root.kind();
        payload.root_id = intent_root.id().to_string();
        payload.related_intent_ids = vec![intent_root.id().to_string()];

        let mut note = memory_note_json();
        note["note_id"] = serde_json::json!(intent_root.note_id());
        note["path"] = serde_json::json!(intent_root.path());
        note["episode"] = serde_json::to_value(payload).expect("Episode serializes");
        note["links"] = serde_json::json!([{
            "kind": "supports",
            "target_note_id": task_root.note_id(),
            "target_revision_oid": SOURCE_OID,
            "evidence_refs": [],
            "valid_from": null,
            "valid_until": null
        }]);
        mark_episode_compiler(&mut note);
        refresh_note_digest(&mut note);
        note
    }

    #[test]
    fn evidence_ref_accepts_all_six_bounded_locators() {
        let cases = [
            evidence(
                EvidenceSourcePlane::AgentRuntime,
                EvidenceKind::Task,
                EvidenceLocatorV1::Object,
            ),
            evidence(
                EvidenceSourcePlane::AgentRuntime,
                EvidenceKind::Evidence,
                EvidenceLocatorV1::EventSeq { event_seq: 7 },
            ),
            evidence(
                EvidenceSourcePlane::AgentRuntime,
                EvidenceKind::Task,
                EvidenceLocatorV1::JsonPointer {
                    pointer: "/goal".to_string(),
                },
            ),
            evidence(
                EvidenceSourcePlane::Session,
                EvidenceKind::Session,
                EvidenceLocatorV1::SessionFragment {
                    start_seq: 3,
                    end_seq: 8,
                },
            ),
            evidence(
                EvidenceSourcePlane::Session,
                EvidenceKind::ToolCall,
                EvidenceLocatorV1::ToolCall {
                    invocation_id: "call-2".to_string(),
                    part: ToolCallPart::Output,
                },
            ),
            evidence(
                EvidenceSourcePlane::Git,
                EvidenceKind::Code,
                EvidenceLocatorV1::CodeRange {
                    commit_oid: CODE_OID.to_string(),
                    path: "src/lib.rs".to_string(),
                    start_line: 10,
                    end_line: 14,
                },
            ),
        ];

        for evidence_ref in cases {
            validate_evidence_ref(&evidence_ref).expect("bounded synthetic locator is valid");
        }
    }

    #[test]
    fn evidence_ref_rejects_unbounded_or_ambiguous_locators() {
        let mut cases = Vec::new();

        let mut unknown_version = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Task,
            EvidenceLocatorV1::Object,
        );
        unknown_version.schema_version = 2;
        cases.push(unknown_version);

        let mut uppercase_oid = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Task,
            EvidenceLocatorV1::Object,
        );
        uppercase_oid.source_ref_oid = "A".repeat(40);
        cases.push(uppercase_oid);

        cases.push(evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Evidence,
            EvidenceLocatorV1::EventSeq { event_seq: 0 },
        ));
        cases.push(evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Task,
            EvidenceLocatorV1::JsonPointer {
                pointer: "goal".to_string(),
            },
        ));
        cases.push(evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Task,
            EvidenceLocatorV1::JsonPointer {
                pointer: "/bad~2escape".to_string(),
            },
        ));
        cases.push(evidence(
            EvidenceSourcePlane::Session,
            EvidenceKind::Session,
            EvidenceLocatorV1::SessionFragment {
                start_seq: 1,
                end_seq: 258,
            },
        ));
        cases.push(evidence(
            EvidenceSourcePlane::Session,
            EvidenceKind::ToolCall,
            EvidenceLocatorV1::ToolCall {
                invocation_id: String::new(),
                part: ToolCallPart::Invocation,
            },
        ));
        cases.push(evidence(
            EvidenceSourcePlane::Git,
            EvidenceKind::Code,
            EvidenceLocatorV1::CodeRange {
                commit_oid: CODE_OID.to_string(),
                path: "../src/lib.rs".to_string(),
                start_line: 10,
                end_line: 14,
            },
        ));
        cases.push(evidence(
            EvidenceSourcePlane::Git,
            EvidenceKind::Code,
            EvidenceLocatorV1::CodeRange {
                commit_oid: CODE_OID.to_string(),
                path: "src/lib.rs".to_string(),
                start_line: 14,
                end_line: 10,
            },
        ));
        let mut mismatched_commit = evidence(
            EvidenceSourcePlane::Git,
            EvidenceKind::Code,
            EvidenceLocatorV1::CodeRange {
                commit_oid: CODE_OID.to_string(),
                path: "src/lib.rs".to_string(),
                start_line: 10,
                end_line: 14,
            },
        );
        mismatched_commit.code_commit = Some(SOURCE_OID.to_string());
        cases.push(mismatched_commit);
        cases.push(evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Session,
            EvidenceLocatorV1::SessionFragment {
                start_seq: 1,
                end_seq: 2,
            },
        ));

        for evidence_ref in cases {
            assert!(
                validate_evidence_ref(&evidence_ref).is_err(),
                "invalid locator was accepted: {evidence_ref:?}",
            );
        }
    }

    #[test]
    fn claims_separate_observations_from_inferences() {
        let evidence_ref = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Evidence,
            EvidenceLocatorV1::EventSeq { event_seq: 7 },
        );
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

        validate_episode_claim(&observation).expect("direct observation is valid");
        validate_episode_claim(&inference).expect("evidence-backed inference is valid");

        let mut invalid_observation = observation.clone();
        invalid_observation.confidence = Some(MemoryAnchorConfidence::Low);
        assert!(validate_episode_claim(&invalid_observation).is_err());

        let mut invalid_inference = inference.clone();
        invalid_inference.confidence = None;
        assert!(validate_episode_claim(&invalid_inference).is_err());

        let mut unsupported = observation;
        unsupported.evidence_refs.clear();
        assert!(validate_episode_claim(&unsupported).is_err());
    }

    #[test]
    fn payload_rejects_compiler_overrides_of_trusted_task_fields() {
        let (payload, trusted) = task_payload_fixture();

        validate_episode_payload(&payload, &trusted).expect("trusted Task payload is valid");

        let mut overridden = payload;
        overridden.completion_status = CompletionStatus::Failed;
        assert!(validate_episode_payload(&overridden, &trusted).is_err());

        let (mut overridden, trusted) = task_payload_fixture();
        overridden.related_run_ids.pop();
        assert!(validate_episode_payload(&overridden, &trusted).is_err());

        let (mut overridden, trusted) = task_payload_fixture();
        overridden.started_at = None;
        assert!(validate_episode_payload(&overridden, &trusted).is_err());

        let (mut overridden, trusted) = task_payload_fixture();
        overridden.code.result_oid = Some(SOURCE_OID.to_string());
        assert!(validate_episode_payload(&overridden, &trusted).is_err());
    }

    #[test]
    fn payload_rejects_git_invalid_full_branch_refs() {
        for branch_ref in [
            "refs/heads/foo/.bar",
            "refs/heads/foo/bar.lock/baz",
            r"refs/heads/foo\bar",
        ] {
            let (mut payload, mut trusted) = task_payload_fixture();
            payload.code.branch_ref = Some(branch_ref.to_string());
            trusted.code = payload.code.clone();

            assert!(
                validate_episode_payload(&payload, &trusted).is_err(),
                "Git-invalid full branch ref was accepted: {branch_ref}",
            );
        }
    }

    #[test]
    fn payload_rejects_compiler_override_of_trusted_goal() {
        let (mut payload, trusted) = task_payload_fixture();
        payload.goal.claim = "compiler replaced the trusted goal".to_string();

        assert!(validate_episode_payload(&payload, &trusted).is_err());
    }

    #[test]
    fn payload_rejects_compiler_override_of_trusted_omissions() {
        let (mut payload, trusted) = task_payload_fixture();
        payload.omissions.failed_attempts = 1;

        assert!(validate_episode_payload(&payload, &trusted).is_err());
    }

    #[test]
    fn intent_payload_requires_at_least_one_contributing_task() {
        let (mut payload, mut trusted) = task_payload_fixture();
        let root = EpisodeRoot::intent("intent-9").expect("synthetic intent id is valid");
        trusted.root = root.clone();
        payload.root_kind = root.kind();
        payload.root_id = root.id().to_string();

        validate_episode_payload(&payload, &trusted).expect("Intent payload has a Task input");

        payload.related_task_ids.clear();
        trusted.related_task_ids.clear();
        assert!(validate_episode_payload(&payload, &trusted).is_err());
    }

    #[test]
    fn intent_episode_note_pins_each_contributing_task_revision_once() {
        use super::parse_memory_note_v1;

        let note = intent_episode_note_json();
        parse_memory_note_v1(&serde_json::to_vec(&note).expect("note serializes"))
            .expect("Intent Episode pins its contributing Task revision");

        let mut with_sibling = note.clone();
        let mut sibling = with_sibling["links"][0].clone();
        sibling["kind"] = serde_json::json!("sibling");
        with_sibling["links"]
            .as_array_mut()
            .expect("links fixture is an array")
            .push(sibling);
        refresh_note_digest(&mut with_sibling);
        parse_memory_note_v1(
            &serde_json::to_vec(&with_sibling).expect("note with sibling serializes"),
        )
        .expect("a non-Supports relation to the same Task remains valid");

        let mut missing = note.clone();
        missing["links"] = serde_json::json!([]);
        refresh_note_digest(&mut missing);
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&missing).expect("note serializes")).is_err(),
        );

        let mut duplicate = note.clone();
        duplicate["links"] = serde_json::json!([note["links"][0], note["links"][0]]);
        refresh_note_digest(&mut duplicate);
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&duplicate).expect("note serializes"))
                .is_err(),
        );

        let mut extra = note.clone();
        let mut extra_link = extra["links"][0].clone();
        extra_link["target_note_id"] = serde_json::json!(
            EpisodeRoot::task("unrelated-task")
                .expect("construct unrelated Task root")
                .note_id()
        );
        extra["links"]
            .as_array_mut()
            .expect("links fixture is an array")
            .push(extra_link);
        refresh_note_digest(&mut extra);
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&extra).expect("note serializes")).is_err(),
        );

        let mut floating = note;
        floating["links"][0]["target_revision_oid"] = serde_json::Value::Null;
        refresh_note_digest(&mut floating);
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&floating).expect("note serializes")).is_err(),
        );
    }

    #[test]
    fn payload_rejects_noncanonical_trusted_ids() {
        let (mut payload, mut trusted) = task_payload_fixture();
        payload.related_run_ids = vec!["run-\n".to_string()];
        trusted.related_run_ids = payload.related_run_ids.clone();

        assert!(validate_episode_payload(&payload, &trusted).is_err());

        let (mut payload, mut trusted) = task_payload_fixture();
        payload.related_run_ids = vec!["r".repeat(513)];
        trusted.related_run_ids = payload.related_run_ids.clone();
        assert!(validate_episode_payload(&payload, &trusted).is_err());
    }

    #[test]
    fn episode_and_evidence_v1_gate_versions_enums_and_additive_fields() {
        use super::{parse_episode_payload_v1, parse_evidence_ref_v1};

        let (payload, trusted) = task_payload_fixture();
        let mut payload_json = serde_json::to_value(&payload).expect("payload serializes");
        payload_json
            .as_object_mut()
            .expect("payload is an object")
            .insert("future_hint".to_string(), serde_json::json!(true));
        parse_episode_payload_v1(
            &serde_json::to_vec(&payload_json).expect("payload JSON serializes"),
            &trusted,
        )
        .expect("additive field is ignored by a v1 reader");

        payload_json["schema_version"] = serde_json::json!(2);
        assert!(
            parse_episode_payload_v1(
                &serde_json::to_vec(&payload_json).expect("payload JSON serializes"),
                &trusted,
            )
            .is_err(),
        );
        payload_json["schema_version"] = serde_json::json!(1);
        payload_json["root_kind"] = serde_json::json!("future_root");
        assert!(
            parse_episode_payload_v1(
                &serde_json::to_vec(&payload_json).expect("payload JSON serializes"),
                &trusted,
            )
            .is_err(),
        );

        let evidence_ref = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Task,
            EvidenceLocatorV1::Object,
        );
        let mut evidence_json = serde_json::to_value(evidence_ref).expect("evidence serializes");
        evidence_json
            .as_object_mut()
            .expect("evidence is an object")
            .insert("future_hint".to_string(), serde_json::json!(true));
        parse_evidence_ref_v1(
            &serde_json::to_vec(&evidence_json).expect("evidence JSON serializes"),
        )
        .expect("additive field is ignored by a v1 reader");
        evidence_json["schema_version"] = serde_json::json!(2);
        assert!(
            parse_evidence_ref_v1(
                &serde_json::to_vec(&evidence_json).expect("evidence JSON serializes"),
            )
            .is_err(),
        );
    }

    #[test]
    fn note_event_and_compile_record_gate_versions_enums_and_additive_fields() {
        use super::{parse_compile_record_v1, parse_memory_event_v1, parse_memory_note_v1};

        let mut compile = compile_record_json();
        compile["future_hint"] = serde_json::json!(true);
        parse_compile_record_v1(&serde_json::to_vec(&compile).expect("compile JSON serializes"))
            .expect("additive CompileRecord field is ignored");
        compile["schema_version"] = serde_json::json!(2);
        assert!(
            parse_compile_record_v1(
                &serde_json::to_vec(&compile).expect("compile JSON serializes"),
            )
            .is_err(),
        );
        compile["schema_version"] = serde_json::json!(1);
        compile["origin"] = serde_json::json!("future_origin");
        assert!(
            parse_compile_record_v1(
                &serde_json::to_vec(&compile).expect("compile JSON serializes"),
            )
            .is_err(),
        );

        let mut note = memory_note_json();
        note["future_hint"] = serde_json::json!(true);
        super::parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
            .expect("additive MemoryNote field is ignored");
        note["schema_version"] = serde_json::json!(2);
        assert!(
            super::parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
                .is_err(),
        );
        note["schema_version"] = serde_json::json!(1);
        note["kind"] = serde_json::json!("future_kind");
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
                .is_err(),
        );

        let mut event = memory_event_json();
        event["future_hint"] = serde_json::json!(true);
        parse_memory_event_v1(&serde_json::to_vec(&event).expect("event JSON serializes"))
            .expect("additive MemoryEvent field is ignored");
        event["schema_version"] = serde_json::json!(2);
        assert!(
            parse_memory_event_v1(&serde_json::to_vec(&event).expect("event JSON serializes"))
                .is_err(),
        );
        event["schema_version"] = serde_json::json!(1);
        event["action"] = serde_json::json!("updated");
        assert!(
            parse_memory_event_v1(&serde_json::to_vec(&event).expect("event JSON serializes"))
                .is_err(),
        );
    }

    #[test]
    fn memory_event_actions_require_unambiguous_target_shapes() {
        use super::parse_memory_event_v1;

        let lifecycle = memory_event_json();
        parse_memory_event_v1(&serde_json::to_vec(&lifecycle).expect("event serializes"))
            .expect("lifecycle event has a note target");

        let mut taxonomy = memory_event_json();
        taxonomy["action"] = serde_json::json!("taxonomy_expanded");
        taxonomy["namespace"] = serde_json::json!("default");
        taxonomy["target_path"] = serde_json::json!("episodic.tasks");
        assert!(
            parse_memory_event_v1(&serde_json::to_vec(&taxonomy).expect("event serializes"))
                .is_err(),
        );
        taxonomy["note_id"] = serde_json::Value::Null;
        taxonomy["revision_oid"] = serde_json::Value::Null;
        parse_memory_event_v1(&serde_json::to_vec(&taxonomy).expect("event serializes"))
            .expect("taxonomy event has only a taxonomy target");

        let mut mixed_lifecycle = memory_event_json();
        mixed_lifecycle["namespace"] = serde_json::json!("default");
        assert!(
            parse_memory_event_v1(
                &serde_json::to_vec(&mixed_lifecycle).expect("event serializes"),
            )
            .is_err(),
        );

        let mut invalid_successor = memory_event_json();
        invalid_successor["next_note_id"] =
            serde_json::json!("760369f7-ba78-541a-9aae-4e899154530b");
        assert!(
            parse_memory_event_v1(
                &serde_json::to_vec(&invalid_successor).expect("event serializes"),
            )
            .is_err(),
        );
        invalid_successor["action"] = serde_json::json!("superseded");
        parse_memory_event_v1(&serde_json::to_vec(&invalid_successor).expect("event serializes"))
            .expect("Superseded may identify its successor");
    }

    #[test]
    fn episode_note_envelope_rejects_wall_clock_validity() {
        use super::parse_memory_note_v1;

        let (payload, _) = task_payload_fixture();
        let mut note = memory_note_json();
        note["episode"] = serde_json::to_value(payload).expect("Episode serializes");
        mark_episode_compiler(&mut note);
        refresh_note_digest(&mut note);
        parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
            .expect("fixed Episode envelope is valid");

        note["expires_at"] = serde_json::json!("2026-09-01T00:00:00Z");
        refresh_note_digest(&mut note);
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
                .is_err(),
        );
    }

    #[test]
    fn episode_compiler_origin_requires_an_episode_payload() {
        use super::parse_memory_note_v1;

        let mut note = memory_note_json();
        mark_episode_compiler(&mut note);
        refresh_note_digest(&mut note);
        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
                .is_err(),
        );
    }

    #[test]
    fn memory_note_parser_rejects_content_digest_mismatch() {
        use super::parse_memory_note_v1;

        let mut note = memory_note_json();
        note["body"] = serde_json::json!("tampered after the digest was computed");

        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
                .is_err(),
        );
    }

    #[test]
    fn top_level_parsers_reject_oversized_input_before_deserialize() {
        use super::{
            MAX_COMPILE_RECORD_BYTES, MAX_EPISODE_PAYLOAD_BYTES, MAX_EVIDENCE_REF_BYTES,
            MAX_MEMORY_EVENT_BYTES, MAX_MEMORY_NOTE_BYTES, parse_compile_record_v1,
            parse_episode_payload_v1, parse_evidence_ref_v1, parse_memory_event_v1,
            parse_memory_note_v1,
        };

        let (_, trusted) = task_payload_fixture();
        let cases = [
            (
                parse_compile_record_v1(&vec![b' '; MAX_COMPILE_RECORD_BYTES + 1]).map(|_| ()),
                "CompileRecord.size",
            ),
            (
                parse_memory_note_v1(&vec![b' '; MAX_MEMORY_NOTE_BYTES + 1]).map(|_| ()),
                "MemoryNote.size",
            ),
            (
                parse_memory_event_v1(&vec![b' '; MAX_MEMORY_EVENT_BYTES + 1]).map(|_| ()),
                "MemoryEvent.size",
            ),
            (
                parse_episode_payload_v1(&vec![b' '; MAX_EPISODE_PAYLOAD_BYTES + 1], &trusted)
                    .map(|_| ()),
                "EpisodePayload.size",
            ),
            (
                parse_evidence_ref_v1(&vec![b' '; MAX_EVIDENCE_REF_BYTES + 1]).map(|_| ()),
                "EvidenceRef.size",
            ),
        ];

        for (result, field) in cases {
            assert_eq!(result, Err(MemoryContractError::InvalidField { field }));
        }
    }

    #[test]
    fn memory_note_rejects_oversized_metadata() {
        use super::parse_memory_note_v1;

        let mut too_many_tags = memory_note_json();
        let tags = (0..129)
            .map(|index| format!("tag-{index:03}"))
            .collect::<Vec<_>>();
        too_many_tags["tags"] = serde_json::to_value(tags).expect("tags serialize");
        refresh_note_digest(&mut too_many_tags);
        assert!(
            parse_memory_note_v1(
                &serde_json::to_vec(&too_many_tags).expect("note JSON serializes"),
            )
            .is_err(),
        );

        let mut long_namespace = memory_note_json();
        long_namespace["namespace"] = serde_json::json!("n".repeat(4 * 1024 + 1));
        refresh_note_digest(&mut long_namespace);
        assert!(
            parse_memory_note_v1(
                &serde_json::to_vec(&long_namespace).expect("note JSON serializes"),
            )
            .is_err(),
        );
    }

    #[test]
    fn memory_note_rejects_oversized_scope_identity() {
        use super::parse_memory_note_v1;

        let mut note = memory_note_json();
        note["scope"] = serde_json::json!({
            "type": "actor",
            "value": "a".repeat(513),
        });
        refresh_note_digest(&mut note);

        assert!(
            parse_memory_note_v1(&serde_json::to_vec(&note).expect("note JSON serializes"))
                .is_err(),
        );
    }

    #[test]
    fn memory_event_rejects_oversized_evidence_collection() {
        use super::parse_memory_event_v1;

        let evidence_ref = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Evidence,
            EvidenceLocatorV1::EventSeq { event_seq: 7 },
        );
        let mut event = memory_event_json();
        event["evidence_refs"] =
            serde_json::to_value(vec![evidence_ref; 129]).expect("evidence serializes");

        assert!(
            parse_memory_event_v1(&serde_json::to_vec(&event).expect("event JSON serializes"))
                .is_err(),
        );
    }

    #[test]
    fn evidence_ref_rejects_oversized_locator_metadata() {
        let evidence_ref = evidence(
            EvidenceSourcePlane::AgentRuntime,
            EvidenceKind::Task,
            EvidenceLocatorV1::JsonPointer {
                pointer: format!("/{}", "p".repeat(4 * 1024)),
            },
        );

        assert!(validate_evidence_ref(&evidence_ref).is_err());
    }

    #[test]
    fn compile_record_rejects_oversized_metadata() {
        use super::parse_compile_record_v1;

        let mut record = compile_record_json();
        record["producer"] = serde_json::json!("p".repeat(4 * 1024 + 1));

        assert!(
            parse_compile_record_v1(
                &serde_json::to_vec(&record).expect("compile record serializes"),
            )
            .is_err(),
        );

        let mut unsafe_key_id = compile_record_json();
        unsafe_key_id["idempotency_key"] =
            serde_json::json!(format!("hmac-sha256:key\n1:{}", "f".repeat(64)));
        assert!(
            parse_compile_record_v1(
                &serde_json::to_vec(&unsafe_key_id).expect("compile record serializes"),
            )
            .is_err(),
        );
    }

    #[test]
    fn namespace_idempotency_is_reserved_for_aggregate_compilers() {
        use super::parse_compile_record_v1;

        let mut episode = compile_record_json();
        episode["origin"] = serde_json::json!("episode_compiler");
        episode["prompt_version"] = serde_json::json!("episode-v1");
        episode["model_id"] = serde_json::json!("synthetic-model");
        episode["idempotency_scope"] = serde_json::json!("namespace");
        assert!(
            parse_compile_record_v1(
                &serde_json::to_vec(&episode).expect("compile record serializes"),
            )
            .is_err(),
            "Episode compilation must not deduplicate across Cells",
        );

        let mut consolidation = compile_record_json();
        consolidation["origin"] = serde_json::json!("consolidation");
        consolidation["idempotency_scope"] = serde_json::json!("namespace");
        parse_compile_record_v1(
            &serde_json::to_vec(&consolidation).expect("compile record serializes"),
        )
        .expect("consolidation may explicitly deduplicate within a namespace");
    }

    #[test]
    fn episode_contract_does_not_filter_development_outcomes_or_provenance() {
        use super::parse_memory_note_v1;

        for completion_status in [
            CompletionStatus::Completed,
            CompletionStatus::Failed,
            CompletionStatus::Cancelled,
        ] {
            for code_change_status in [
                CodeChangeStatus::Changed,
                CodeChangeStatus::Unchanged,
                CodeChangeStatus::Unknown,
            ] {
                for trust in [
                    MemoryTrust::Verified,
                    MemoryTrust::RepoEvidence,
                    MemoryTrust::UserAsserted,
                    MemoryTrust::ExternalUntrusted,
                    MemoryTrust::Inferred,
                ] {
                    for confidence in [
                        MemoryAnchorConfidence::Low,
                        MemoryAnchorConfidence::Medium,
                        MemoryAnchorConfidence::High,
                    ] {
                        let (mut payload, mut trusted) = task_payload_fixture();
                        payload.completion_status = completion_status;
                        trusted.completion_status = completion_status;
                        payload.code_change_status = code_change_status;
                        trusted.code_change_status = code_change_status;
                        payload.summary.confidence = Some(confidence);
                        payload.inferences[0].confidence = Some(confidence);
                        validate_episode_payload(&payload, &trusted).expect(
                            "completion, code-change, trust and confidence are not intake filters",
                        );

                        let mut note = memory_note_json();
                        note["episode"] =
                            serde_json::to_value(payload).expect("Episode serializes");
                        note["trust"] = serde_json::to_value(trust).expect("trust serializes");
                        note["confidence"] =
                            serde_json::to_value(confidence).expect("confidence serializes");
                        mark_episode_compiler(&mut note);
                        refresh_note_digest(&mut note);

                        parse_memory_note_v1(
                            &serde_json::to_vec(&note).expect("MemoryNote serializes"),
                        )
                        .expect("provenance labels do not block a valid M2 Episode contract");
                    }
                }
            }
        }
    }
}
