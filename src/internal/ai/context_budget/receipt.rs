//! Shared context selection receipt domain contract.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::internal::ai::keyed_digest::{PrincipalDigest, QueryDigest};

const RECEIPT_SCHEMA_VERSION: u32 = 1;
const MAX_REPOSITORY_ID_BYTES: usize = 512;
const MAX_BRANCH_REF_BYTES: usize = 4 * 1024;
const MAX_SNAPSHOT_ENTRIES: usize = 64;
const MAX_SNAPSHOT_KEY_BYTES: usize = 128;
const MAX_SELECTOR_VERSION_BYTES: usize = 128;
const MAX_SELECTED: usize = 1_024;
const MAX_OMISSIONS: usize = 64;
const MAX_OBJECT_ID_BYTES: usize = 512;
const MAX_SUMMARY_KEY_BYTES: usize = 4 * 1024;
const MAX_REASON_CODES: usize = 16;
const MAX_REASON_CODE_BYTES: usize = 64;
const MAX_SCORE_COMPONENTS: usize = 32;
const MAX_SNAPSHOT_JSON_BYTES: usize = 64 * 1024;
const MAX_SELECTED_JSON_BYTES: usize = 1024 * 1024;
const MAX_OMISSIONS_JSON_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiptSourceKind {
    Memory,
    Intent,
    Hook,
}

impl ReceiptSourceKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Intent => "intent",
            Self::Hook => "hook",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "memory" => Some(Self::Memory),
            "intent" => Some(Self::Intent),
            "hook" => Some(Self::Hook),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiptReproducibilityState {
    Reproducible,
    Stale,
    Expired,
    NonReproducible,
}

impl ReceiptReproducibilityState {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Reproducible => "reproducible",
            Self::Stale => "stale",
            Self::Expired => "expired",
            Self::NonReproducible => "non_reproducible",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "reproducible" => Some(Self::Reproducible),
            "stale" => Some(Self::Stale),
            "expired" => Some(Self::Expired),
            "non_reproducible" => Some(Self::NonReproducible),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptSensitivity {
    Allowed,
    SecretLike,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptDependencyAvailability {
    Exact,
    Stale,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReceiptReplayAvailabilityV1 {
    pub(crate) digest_key: ReceiptDependencyAvailability,
    pub(crate) source_snapshot: ReceiptDependencyAvailability,
    pub(crate) policy: ReceiptDependencyAvailability,
    pub(crate) index_snapshot: ReceiptDependencyAvailability,
}

impl ReceiptReplayAvailabilityV1 {
    pub(crate) const fn all_exact() -> Self {
        Self {
            digest_key: ReceiptDependencyAvailability::Exact,
            source_snapshot: ReceiptDependencyAvailability::Exact,
            policy: ReceiptDependencyAvailability::Exact,
            index_snapshot: ReceiptDependencyAvailability::Exact,
        }
    }
}

pub(crate) struct ReceiptSelectionInputV1 {
    pub(crate) object_id: String,
    pub(crate) revision_oid: String,
    pub(crate) summary_key: String,
    pub(crate) order: u32,
    pub(crate) reason_codes: Vec<String>,
    pub(crate) score_components: BTreeMap<String, i64>,
    pub(crate) sensitivity: ReceiptSensitivity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiptSelectionV1 {
    pub(crate) object_id: String,
    pub(crate) revision_oid: String,
    pub(crate) summary_key: String,
    pub(crate) order: u32,
    pub(crate) reason_codes: Vec<String>,
    pub(crate) score_components: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiptOmissionV1 {
    pub(crate) reason_code: String,
    pub(crate) count: u32,
}

pub(crate) struct ReceiptDraftFieldsV1 {
    pub(crate) source_kind: ReceiptSourceKind,
    pub(crate) repository_id: String,
    pub(crate) principal_digest: PrincipalDigest,
    pub(crate) query_digest: QueryDigest,
    pub(crate) effective_at: DateTime<Utc>,
    pub(crate) code_commit: Option<String>,
    pub(crate) full_branch_ref: Option<String>,
    pub(crate) source_heads: BTreeMap<String, String>,
    pub(crate) projection_watermarks: BTreeMap<String, String>,
    pub(crate) policy_hash: String,
    pub(crate) selector_version: String,
    pub(crate) selected: Vec<ReceiptSelectionInputV1>,
    pub(crate) omissions: Vec<ReceiptOmissionV1>,
    pub(crate) token_budget: u64,
    pub(crate) bundle_hash: String,
    pub(crate) reproducibility_state: ReceiptReproducibilityState,
    pub(crate) frame_id: Option<Uuid>,
}

pub(crate) struct ContextSelectionReceiptDraftV1 {
    source_kind: ReceiptSourceKind,
    repository_id: String,
    digest_key_id: Uuid,
    principal_hmac: String,
    query_hmac: String,
    effective_at: DateTime<Utc>,
    code_commit: Option<String>,
    full_branch_ref: Option<String>,
    source_heads: BTreeMap<String, String>,
    projection_watermarks: BTreeMap<String, String>,
    policy_hash: String,
    selector_version: String,
    selected: Vec<ReceiptSelectionV1>,
    omissions: Vec<ReceiptOmissionV1>,
    token_budget: u64,
    bundle_hash: String,
    reproducibility_state: ReceiptReproducibilityState,
    frame_id: Option<Uuid>,
}

impl ContextSelectionReceiptDraftV1 {
    pub(crate) fn new(fields: ReceiptDraftFieldsV1) -> Result<Self, ReceiptValidationError> {
        if fields.principal_digest.version() != RECEIPT_SCHEMA_VERSION as u8
            || fields.query_digest.version() != RECEIPT_SCHEMA_VERSION as u8
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::DigestVersion,
            ));
        }
        if fields.principal_digest.key_id() != fields.query_digest.key_id() {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::DigestKeyMismatch,
            ));
        }
        if !valid_bounded(&fields.repository_id, MAX_REPOSITORY_ID_BYTES) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::RepositoryId,
            ));
        }
        if fields
            .code_commit
            .as_deref()
            .is_some_and(|value| !valid_object_id(value))
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::CodeCommit,
            ));
        }
        if fields.full_branch_ref.as_deref().is_some_and(|value| {
            value.len() < 12
                || !value.starts_with("refs/heads/")
                || !valid_bounded(value, MAX_BRANCH_REF_BYTES)
        }) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::BranchRef,
            ));
        }
        validate_snapshot(&fields.source_heads)?;
        validate_snapshot(&fields.projection_watermarks)?;
        if !valid_sha256(&fields.policy_hash) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::PolicyHash,
            ));
        }
        if !valid_bounded(&fields.selector_version, MAX_SELECTOR_VERSION_BYTES) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::SelectorVersion,
            ));
        }
        if fields.token_budget > i64::MAX as u64 {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::TokenBudget,
            ));
        }
        if !valid_sha256(&fields.bundle_hash) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::BundleHash,
            ));
        }

        let selected = validate_selected(fields.selected)?;
        validate_omissions(&fields.omissions)?;
        validate_json_envelope(
            &fields.source_heads,
            MAX_SNAPSHOT_JSON_BYTES,
            ReceiptValidationErrorKind::Snapshot,
        )?;
        validate_json_envelope(
            &fields.projection_watermarks,
            MAX_SNAPSHOT_JSON_BYTES,
            ReceiptValidationErrorKind::Snapshot,
        )?;
        validate_json_envelope(
            &selected,
            MAX_SELECTED_JSON_BYTES,
            ReceiptValidationErrorKind::Selected,
        )?;
        validate_json_envelope(
            &fields.omissions,
            MAX_OMISSIONS_JSON_BYTES,
            ReceiptValidationErrorKind::Omissions,
        )?;
        let digest_key_id = fields.principal_digest.key_id();
        Ok(Self {
            source_kind: fields.source_kind,
            repository_id: fields.repository_id,
            digest_key_id,
            principal_hmac: fields.principal_digest.encoded(),
            query_hmac: fields.query_digest.encoded(),
            effective_at: fields.effective_at,
            code_commit: fields.code_commit,
            full_branch_ref: fields.full_branch_ref,
            source_heads: fields.source_heads,
            projection_watermarks: fields.projection_watermarks,
            policy_hash: fields.policy_hash,
            selector_version: fields.selector_version,
            selected,
            omissions: fields.omissions,
            token_budget: fields.token_budget,
            bundle_hash: fields.bundle_hash,
            reproducibility_state: fields.reproducibility_state,
            frame_id: fields.frame_id,
        })
    }

    pub(crate) const fn schema_version(&self) -> u32 {
        RECEIPT_SCHEMA_VERSION
    }

    pub(crate) const fn digest_key_id(&self) -> Uuid {
        self.digest_key_id
    }

    pub(crate) fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub(crate) const fn source_kind(&self) -> ReceiptSourceKind {
        self.source_kind
    }

    pub(crate) fn selected(&self) -> &[ReceiptSelectionV1] {
        &self.selected
    }

    pub(crate) fn omissions(&self) -> &[ReceiptOmissionV1] {
        &self.omissions
    }
}

pub(super) struct PersistedReceiptFieldsV1 {
    pub(super) receipt_id: Uuid,
    pub(super) schema_version: u32,
    pub(super) source_kind: ReceiptSourceKind,
    pub(super) repository_id: String,
    pub(super) digest_key_id: Uuid,
    pub(super) principal_hmac: String,
    pub(super) query_hmac: String,
    pub(super) effective_at: DateTime<Utc>,
    pub(super) code_commit: Option<String>,
    pub(super) full_branch_ref: Option<String>,
    pub(super) source_heads: BTreeMap<String, String>,
    pub(super) projection_watermarks: BTreeMap<String, String>,
    pub(super) policy_hash: String,
    pub(super) selector_version: String,
    pub(super) token_budget: u64,
    pub(super) selected: Vec<ReceiptSelectionV1>,
    pub(super) omissions: Vec<ReceiptOmissionV1>,
    pub(super) bundle_hash: String,
    pub(super) reproducibility_state: ReceiptReproducibilityState,
    pub(super) frame_id: Option<Uuid>,
    pub(super) recorded_at: DateTime<Utc>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ContextSelectionReceiptV1 {
    pub(super) receipt_id: Uuid,
    pub(super) schema_version: u32,
    pub(super) source_kind: ReceiptSourceKind,
    pub(super) repository_id: String,
    pub(super) digest_key_id: Uuid,
    pub(super) principal_hmac: String,
    pub(super) query_hmac: String,
    pub(super) effective_at: DateTime<Utc>,
    pub(super) code_commit: Option<String>,
    pub(super) full_branch_ref: Option<String>,
    pub(super) source_heads: BTreeMap<String, String>,
    pub(super) projection_watermarks: BTreeMap<String, String>,
    pub(super) policy_hash: String,
    pub(super) selector_version: String,
    pub(super) token_budget: u64,
    pub(super) selected: Vec<ReceiptSelectionV1>,
    pub(super) omissions: Vec<ReceiptOmissionV1>,
    pub(super) bundle_hash: String,
    pub(super) reproducibility_state: ReceiptReproducibilityState,
    pub(super) frame_id: Option<Uuid>,
    pub(super) recorded_at: DateTime<Utc>,
}

impl ContextSelectionReceiptV1 {
    pub(super) fn from_draft(
        receipt_id: Uuid,
        recorded_at: DateTime<Utc>,
        draft: ContextSelectionReceiptDraftV1,
    ) -> Self {
        Self {
            receipt_id,
            schema_version: RECEIPT_SCHEMA_VERSION,
            source_kind: draft.source_kind,
            repository_id: draft.repository_id,
            digest_key_id: draft.digest_key_id,
            principal_hmac: draft.principal_hmac,
            query_hmac: draft.query_hmac,
            effective_at: draft.effective_at,
            code_commit: draft.code_commit,
            full_branch_ref: draft.full_branch_ref,
            source_heads: draft.source_heads,
            projection_watermarks: draft.projection_watermarks,
            policy_hash: draft.policy_hash,
            selector_version: draft.selector_version,
            token_budget: draft.token_budget,
            selected: draft.selected,
            omissions: draft.omissions,
            bundle_hash: draft.bundle_hash,
            reproducibility_state: draft.reproducibility_state,
            frame_id: draft.frame_id,
            recorded_at,
        }
    }

    pub(super) fn from_persisted(
        fields: PersistedReceiptFieldsV1,
    ) -> Result<Self, ReceiptValidationError> {
        if fields.receipt_id.get_version_num() != 7 {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::ReceiptId,
            ));
        }
        if fields.schema_version != RECEIPT_SCHEMA_VERSION {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::SchemaVersion,
            ));
        }
        if fields.digest_key_id.get_version_num() != 4
            || !valid_receipt_hmac(&fields.principal_hmac, fields.digest_key_id)
            || !valid_receipt_hmac(&fields.query_hmac, fields.digest_key_id)
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::DigestEncoding,
            ));
        }
        if !valid_bounded(&fields.repository_id, MAX_REPOSITORY_ID_BYTES) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::RepositoryId,
            ));
        }
        if fields
            .code_commit
            .as_deref()
            .is_some_and(|value| !valid_object_id(value))
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::CodeCommit,
            ));
        }
        if fields.full_branch_ref.as_deref().is_some_and(|value| {
            value.len() < 12
                || !value.starts_with("refs/heads/")
                || !valid_bounded(value, MAX_BRANCH_REF_BYTES)
        }) {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::BranchRef,
            ));
        }
        validate_snapshot(&fields.source_heads)?;
        validate_snapshot(&fields.projection_watermarks)?;
        if !valid_sha256(&fields.policy_hash)
            || !valid_sha256(&fields.bundle_hash)
            || !valid_bounded(&fields.selector_version, MAX_SELECTOR_VERSION_BYTES)
            || fields.token_budget > i64::MAX as u64
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::PersistedEnvelope,
            ));
        }
        validate_stored_selected(&fields.selected)?;
        validate_omissions(&fields.omissions)?;
        validate_json_envelope(
            &fields.source_heads,
            MAX_SNAPSHOT_JSON_BYTES,
            ReceiptValidationErrorKind::PersistedEnvelope,
        )?;
        validate_json_envelope(
            &fields.projection_watermarks,
            MAX_SNAPSHOT_JSON_BYTES,
            ReceiptValidationErrorKind::PersistedEnvelope,
        )?;
        validate_json_envelope(
            &fields.selected,
            MAX_SELECTED_JSON_BYTES,
            ReceiptValidationErrorKind::PersistedEnvelope,
        )?;
        validate_json_envelope(
            &fields.omissions,
            MAX_OMISSIONS_JSON_BYTES,
            ReceiptValidationErrorKind::PersistedEnvelope,
        )?;

        Ok(Self {
            receipt_id: fields.receipt_id,
            schema_version: fields.schema_version,
            source_kind: fields.source_kind,
            repository_id: fields.repository_id,
            digest_key_id: fields.digest_key_id,
            principal_hmac: fields.principal_hmac,
            query_hmac: fields.query_hmac,
            effective_at: fields.effective_at,
            code_commit: fields.code_commit,
            full_branch_ref: fields.full_branch_ref,
            source_heads: fields.source_heads,
            projection_watermarks: fields.projection_watermarks,
            policy_hash: fields.policy_hash,
            selector_version: fields.selector_version,
            token_budget: fields.token_budget,
            selected: fields.selected,
            omissions: fields.omissions,
            bundle_hash: fields.bundle_hash,
            reproducibility_state: fields.reproducibility_state,
            frame_id: fields.frame_id,
            recorded_at: fields.recorded_at,
        })
    }

    pub(crate) const fn receipt_id(&self) -> Uuid {
        self.receipt_id
    }

    pub(crate) fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub(crate) fn query_hmac(&self) -> &str {
        &self.query_hmac
    }

    pub(crate) const fn effective_at(&self) -> DateTime<Utc> {
        self.effective_at
    }

    pub(crate) fn code_commit(&self) -> Option<&str> {
        self.code_commit.as_deref()
    }

    pub(crate) fn full_branch_ref(&self) -> Option<&str> {
        self.full_branch_ref.as_deref()
    }

    pub(crate) fn source_heads(&self) -> &BTreeMap<String, String> {
        &self.source_heads
    }

    pub(crate) fn projection_watermarks(&self) -> &BTreeMap<String, String> {
        &self.projection_watermarks
    }

    pub(crate) fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    pub(crate) fn selector_version(&self) -> &str {
        &self.selector_version
    }

    pub(crate) const fn token_budget(&self) -> u64 {
        self.token_budget
    }

    pub(crate) fn selected(&self) -> &[ReceiptSelectionV1] {
        &self.selected
    }

    pub(crate) fn omissions(&self) -> &[ReceiptOmissionV1] {
        &self.omissions
    }

    pub(crate) fn bundle_hash(&self) -> &str {
        &self.bundle_hash
    }

    pub(crate) fn replay_state(
        &self,
        availability: ReceiptReplayAvailabilityV1,
    ) -> ReceiptReproducibilityState {
        let dependencies = [
            availability.digest_key,
            availability.source_snapshot,
            availability.policy,
            availability.index_snapshot,
        ];
        let dependency_state = if dependencies.contains(&ReceiptDependencyAvailability::Missing) {
            ReceiptReproducibilityState::NonReproducible
        } else if dependencies.contains(&ReceiptDependencyAvailability::Stale) {
            ReceiptReproducibilityState::Stale
        } else {
            ReceiptReproducibilityState::Reproducible
        };
        more_severe_replay_state(self.reproducibility_state, dependency_state)
    }
}

const fn replay_state_severity(state: ReceiptReproducibilityState) -> u8 {
    match state {
        ReceiptReproducibilityState::Reproducible => 0,
        ReceiptReproducibilityState::Stale => 1,
        ReceiptReproducibilityState::Expired => 2,
        ReceiptReproducibilityState::NonReproducible => 3,
    }
}

const fn more_severe_replay_state(
    stored: ReceiptReproducibilityState,
    current: ReceiptReproducibilityState,
) -> ReceiptReproducibilityState {
    if replay_state_severity(stored) >= replay_state_severity(current) {
        stored
    } else {
        current
    }
}

fn validate_stored_selected(selected: &[ReceiptSelectionV1]) -> Result<(), ReceiptValidationError> {
    if selected.len() > MAX_SELECTED {
        return Err(ReceiptValidationError::new(
            ReceiptValidationErrorKind::Selected,
        ));
    }
    let mut identities = HashSet::with_capacity(selected.len());
    for (index, item) in selected.iter().enumerate() {
        if usize::try_from(item.order).ok() != Some(index)
            || !valid_bounded(&item.object_id, MAX_OBJECT_ID_BYTES)
            || !valid_object_id(&item.revision_oid)
            || !valid_bounded(&item.summary_key, MAX_SUMMARY_KEY_BYTES)
            || item.reason_codes.is_empty()
            || item.reason_codes.len() > MAX_REASON_CODES
            || item
                .reason_codes
                .iter()
                .any(|reason| !valid_reason_code(reason, MAX_REASON_CODE_BYTES))
            || item.score_components.len() > MAX_SCORE_COMPONENTS
            || item
                .score_components
                .keys()
                .any(|key| !valid_reason_code(key, MAX_REASON_CODE_BYTES))
            || !identities.insert((item.object_id.as_str(), item.revision_oid.as_str()))
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::Selected,
            ));
        }
    }
    Ok(())
}

fn valid_receipt_hmac(value: &str, key_id: Uuid) -> bool {
    let prefix = format!("hmac-sha256:{key_id}:");
    value.len() == prefix.len() + 64
        && value.starts_with(&prefix)
        && value[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_snapshot(snapshot: &BTreeMap<String, String>) -> Result<(), ReceiptValidationError> {
    if snapshot.len() > MAX_SNAPSHOT_ENTRIES
        || snapshot.iter().any(|(key, value)| {
            !valid_reason_code(key, MAX_SNAPSHOT_KEY_BYTES) || !valid_object_id(value)
        })
    {
        return Err(ReceiptValidationError::new(
            ReceiptValidationErrorKind::Snapshot,
        ));
    }
    Ok(())
}

fn validate_selected(
    selected: Vec<ReceiptSelectionInputV1>,
) -> Result<Vec<ReceiptSelectionV1>, ReceiptValidationError> {
    if selected.len() > MAX_SELECTED {
        return Err(ReceiptValidationError::new(
            ReceiptValidationErrorKind::Selected,
        ));
    }
    let mut identities = HashSet::with_capacity(selected.len());
    let mut stored = Vec::with_capacity(selected.len());
    for (index, item) in selected.into_iter().enumerate() {
        if item.sensitivity == ReceiptSensitivity::SecretLike {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::SecretLikeSelection,
            ));
        }
        if usize::try_from(item.order).ok() != Some(index)
            || !valid_bounded(&item.object_id, MAX_OBJECT_ID_BYTES)
            || !valid_object_id(&item.revision_oid)
            || !valid_bounded(&item.summary_key, MAX_SUMMARY_KEY_BYTES)
            || item.reason_codes.is_empty()
            || item.reason_codes.len() > MAX_REASON_CODES
            || item
                .reason_codes
                .iter()
                .any(|reason| !valid_reason_code(reason, MAX_REASON_CODE_BYTES))
            || item.score_components.len() > MAX_SCORE_COMPONENTS
            || item
                .score_components
                .keys()
                .any(|key| !valid_reason_code(key, MAX_REASON_CODE_BYTES))
            || !identities.insert((item.object_id.clone(), item.revision_oid.clone()))
        {
            return Err(ReceiptValidationError::new(
                ReceiptValidationErrorKind::Selected,
            ));
        }
        stored.push(ReceiptSelectionV1 {
            object_id: item.object_id,
            revision_oid: item.revision_oid,
            summary_key: item.summary_key,
            order: item.order,
            reason_codes: item.reason_codes,
            score_components: item.score_components,
        });
    }
    Ok(stored)
}

fn validate_omissions(omissions: &[ReceiptOmissionV1]) -> Result<(), ReceiptValidationError> {
    let mut reasons = HashSet::with_capacity(omissions.len());
    if omissions.len() > MAX_OMISSIONS
        || omissions.iter().any(|omission| {
            !valid_reason_code(&omission.reason_code, MAX_REASON_CODE_BYTES)
                || !reasons.insert(omission.reason_code.as_str())
        })
    {
        return Err(ReceiptValidationError::new(
            ReceiptValidationErrorKind::Omissions,
        ));
    }
    Ok(())
}

fn validate_json_envelope<T: Serialize>(
    value: &T,
    max_bytes: usize,
    kind: ReceiptValidationErrorKind,
) -> Result<(), ReceiptValidationError> {
    let encoded = serde_json::to_vec(value).map_err(|_| ReceiptValidationError::new(kind))?;
    if !(2..=max_bytes).contains(&encoded.len()) {
        return Err(ReceiptValidationError::new(kind));
    }
    Ok(())
}

fn valid_bounded(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_reason_code(value: &str, max_bytes: usize) -> bool {
    valid_bounded(value, max_bytes)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

fn valid_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiptValidationErrorKind {
    ReceiptId,
    SchemaVersion,
    DigestVersion,
    DigestKeyMismatch,
    DigestEncoding,
    RepositoryId,
    CodeCommit,
    BranchRef,
    Snapshot,
    PolicyHash,
    SelectorVersion,
    TokenBudget,
    Selected,
    SecretLikeSelection,
    Omissions,
    BundleHash,
    PersistedEnvelope,
}

#[derive(Debug, Error)]
#[error("context selection receipt is invalid ({kind:?})")]
pub(crate) struct ReceiptValidationError {
    kind: ReceiptValidationErrorKind,
}

impl ReceiptValidationError {
    const fn new(kind: ReceiptValidationErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> ReceiptValidationErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::internal::ai::keyed_digest::RepositoryKeyedDigest;

    fn fields(
        provider: &RepositoryKeyedDigest,
        sensitivity: ReceiptSensitivity,
    ) -> ReceiptDraftFieldsV1 {
        let mut source_heads = BTreeMap::new();
        source_heads.insert("memory_repo".to_string(), "a".repeat(40));
        let mut projection_watermarks = BTreeMap::new();
        projection_watermarks.insert("memory_repo".to_string(), "a".repeat(40));
        let mut score_components = BTreeMap::new();
        score_components.insert("bm25".to_string(), -42);

        ReceiptDraftFieldsV1 {
            source_kind: ReceiptSourceKind::Memory,
            repository_id: "repo-42".to_string(),
            principal_digest: provider
                .principal_digest(b"agent:alice")
                .expect("principal digest"),
            query_digest: provider
                .query_digest(b"normalized query")
                .expect("query digest"),
            effective_at: "2026-08-24T00:00:00Z"
                .parse::<DateTime<Utc>>()
                .expect("effective timestamp"),
            code_commit: Some("b".repeat(40)),
            full_branch_ref: Some("refs/heads/feature/memory".to_string()),
            source_heads,
            projection_watermarks,
            policy_hash: format!("sha256:{}", "c".repeat(64)),
            selector_version: "memory-v1".to_string(),
            selected: vec![ReceiptSelectionInputV1 {
                object_id: "episode:task-42".to_string(),
                revision_oid: "d".repeat(40),
                summary_key: "episodic/tasks/task-42".to_string(),
                order: 0,
                reason_codes: vec!["bm25_match".to_string()],
                score_components,
                sensitivity,
            }],
            omissions: vec![ReceiptOmissionV1 {
                reason_code: "budget".to_string(),
                count: 2,
            }],
            token_budget: 1_600,
            bundle_hash: format!("sha256:{}", "e".repeat(64)),
            reproducibility_state: ReceiptReproducibilityState::Reproducible,
            frame_id: Some(Uuid::now_v7()),
        }
    }

    #[test]
    fn draft_accepts_only_bounded_structured_fields_and_purpose_locked_digests() {
        let key_id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("fixed UUIDv4");
        let provider = RepositoryKeyedDigest::for_receipt_tests(
            "repo-42",
            key_id,
            [0x31; 32],
            "receipt-domain-test-key",
        );
        let draft =
            ContextSelectionReceiptDraftV1::new(fields(&provider, ReceiptSensitivity::Allowed))
                .expect("valid structured receipt draft");

        assert_eq!(draft.schema_version(), 1);
        assert_eq!(draft.digest_key_id(), key_id);
        assert_eq!(draft.source_kind(), ReceiptSourceKind::Memory);
        assert_eq!(draft.selected().len(), 1);
        assert_eq!(draft.omissions().len(), 1);
    }

    #[test]
    fn draft_rejects_secret_like_selection_and_mixed_digest_keys() {
        let first_key =
            Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("first UUIDv4");
        let second_key =
            Uuid::parse_str("223e4567-e89b-42d3-a456-426614174000").expect("second UUIDv4");
        let first = RepositoryKeyedDigest::for_receipt_tests(
            "repo-42",
            first_key,
            [0x32; 32],
            "receipt-domain-first-key",
        );
        let second = RepositoryKeyedDigest::for_receipt_tests(
            "repo-42",
            second_key,
            [0x33; 32],
            "receipt-domain-second-key",
        );

        let secret_error =
            ContextSelectionReceiptDraftV1::new(fields(&first, ReceiptSensitivity::SecretLike))
                .err()
                .expect("SecretLike content cannot enter a receipt");
        assert_eq!(
            secret_error.kind(),
            ReceiptValidationErrorKind::SecretLikeSelection
        );

        let mut mixed = fields(&first, ReceiptSensitivity::Allowed);
        mixed.query_digest = second
            .query_digest(b"normalized query")
            .expect("second query digest");
        let mixed_error = ContextSelectionReceiptDraftV1::new(mixed)
            .err()
            .expect("receipt digests must use the same repository key");
        assert_eq!(
            mixed_error.kind(),
            ReceiptValidationErrorKind::DigestKeyMismatch
        );
    }

    #[test]
    fn draft_rejects_schema_incompatible_branch_and_json_envelope() {
        let key_id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("fixed UUIDv4");
        let provider = RepositoryKeyedDigest::for_receipt_tests(
            "repo-42",
            key_id,
            [0x35; 32],
            "receipt-domain-envelope-key",
        );

        let mut empty_branch = fields(&provider, ReceiptSensitivity::Allowed);
        empty_branch.full_branch_ref = Some("refs/heads/".to_string());
        assert_eq!(
            ContextSelectionReceiptDraftV1::new(empty_branch)
                .err()
                .expect("empty branch name must be rejected")
                .kind(),
            ReceiptValidationErrorKind::BranchRef
        );

        let mut oversized = fields(&provider, ReceiptSensitivity::Allowed);
        let template = oversized.selected.pop().expect("selection template");
        oversized.selected = (0..MAX_SELECTED)
            .map(|order| ReceiptSelectionInputV1 {
                object_id: format!("episode:{order:04}"),
                revision_oid: format!("{order:040x}"),
                summary_key: "s".repeat(MAX_SUMMARY_KEY_BYTES),
                order: u32::try_from(order).expect("bounded order"),
                reason_codes: template.reason_codes.clone(),
                score_components: template.score_components.clone(),
                sensitivity: ReceiptSensitivity::Allowed,
            })
            .collect();
        assert_eq!(
            ContextSelectionReceiptDraftV1::new(oversized)
                .err()
                .expect("serialized selection envelope must fit the SQLite CHECK")
                .kind(),
            ReceiptValidationErrorKind::Selected
        );

        assert!(
            validate_json_envelope(
                &Vec::<String>::new(),
                2,
                ReceiptValidationErrorKind::Selected
            )
            .is_ok()
        );
        assert!(
            validate_json_envelope(&vec!["x"], 4, ReceiptValidationErrorKind::Selected).is_err()
        );
        assert!(validate_json_envelope(&"xxxx", 6, ReceiptValidationErrorKind::Selected).is_ok());
        assert!(validate_json_envelope(&"xxxxx", 6, ReceiptValidationErrorKind::Selected).is_err());
    }

    #[test]
    fn replay_state_distinguishes_stale_from_missing_dependencies() {
        let key_id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("fixed UUIDv4");
        let provider = RepositoryKeyedDigest::for_receipt_tests(
            "repo-42",
            key_id,
            [0x34; 32],
            "receipt-domain-replay-key",
        );
        let receipt = ContextSelectionReceiptV1::from_draft(
            Uuid::now_v7(),
            Utc::now(),
            ContextSelectionReceiptDraftV1::new(fields(&provider, ReceiptSensitivity::Allowed))
                .expect("valid receipt draft"),
        );

        assert_eq!(
            receipt.replay_state(ReceiptReplayAvailabilityV1::all_exact()),
            ReceiptReproducibilityState::Reproducible
        );
        assert_eq!(
            receipt.replay_state(ReceiptReplayAvailabilityV1 {
                policy: ReceiptDependencyAvailability::Stale,
                ..ReceiptReplayAvailabilityV1::all_exact()
            }),
            ReceiptReproducibilityState::Stale
        );
        for availability in [
            ReceiptReplayAvailabilityV1 {
                digest_key: ReceiptDependencyAvailability::Missing,
                ..ReceiptReplayAvailabilityV1::all_exact()
            },
            ReceiptReplayAvailabilityV1 {
                source_snapshot: ReceiptDependencyAvailability::Missing,
                ..ReceiptReplayAvailabilityV1::all_exact()
            },
            ReceiptReplayAvailabilityV1 {
                policy: ReceiptDependencyAvailability::Missing,
                ..ReceiptReplayAvailabilityV1::all_exact()
            },
            ReceiptReplayAvailabilityV1 {
                index_snapshot: ReceiptDependencyAvailability::Missing,
                ..ReceiptReplayAvailabilityV1::all_exact()
            },
        ] {
            assert_eq!(
                receipt.replay_state(availability),
                ReceiptReproducibilityState::NonReproducible
            );
        }

        for stored in [
            ReceiptReproducibilityState::Expired,
            ReceiptReproducibilityState::NonReproducible,
        ] {
            let mut stored_fields = fields(&provider, ReceiptSensitivity::Allowed);
            stored_fields.reproducibility_state = stored;
            let stored_receipt = ContextSelectionReceiptV1::from_draft(
                Uuid::now_v7(),
                Utc::now(),
                ContextSelectionReceiptDraftV1::new(stored_fields).expect("valid stored state"),
            );
            assert_eq!(
                stored_receipt.replay_state(ReceiptReplayAvailabilityV1 {
                    policy: ReceiptDependencyAvailability::Stale,
                    ..ReceiptReplayAvailabilityV1::all_exact()
                }),
                stored,
                "replay classification cannot reduce an existing terminal severity"
            );
        }
    }
}
