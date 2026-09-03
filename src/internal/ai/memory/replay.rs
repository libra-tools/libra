//! I/O-free reduction of validated Memory events into projection state.
//!
//! Object traversal and SQLite materialization deliberately live outside this
//! module. Rebuild, incremental replay, and the Writer companion all cross the
//! same reducer interface so lifecycle semantics cannot drift between paths.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use git_internal::hash::ObjectHash;
use uuid::Uuid;

use super::{
    domain::{MemoryEventAction, MemoryEventV1, MemoryNoteV1},
    error::{MemoryWriterError, MemoryWriterErrorKind},
    tree::parse_oid,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProjectedReviewState {
    Draft,
    Confirmed,
    Quarantined,
    Revoked,
    Superseded,
    Forgotten,
}

#[derive(Clone)]
pub(super) struct ProjectedNote {
    pub(super) latest_revision_oid: ObjectHash,
    pub(super) live_revision_oid: Option<ObjectHash>,
    pub(super) latest_action: MemoryEventAction,
    pub(super) review_state: ProjectedReviewState,
    pub(super) last_event_seq: u64,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) revisions: BTreeSet<String>,
}

#[derive(Clone, Default)]
pub(super) struct ReducedProjection {
    pub(super) last_event_seq: u64,
    pub(super) event_ids: HashSet<Uuid>,
    pub(super) notes: BTreeMap<Uuid, ProjectedNote>,
    pub(super) new_revisions: BTreeMap<String, MemoryNoteV1>,
    /// Revision OIDs in authoritative event order. The map above provides
    /// lookup; this sequence prevents object-hash ordering from changing which
    /// revision supplies the latest note-level projection values on rebuild.
    pub(super) new_revision_order: Vec<String>,
    pub(super) created_notes: BTreeSet<Uuid>,
    pub(super) changed_notes: BTreeSet<Uuid>,
}

pub(super) struct ReplayRecord {
    pub(super) event: MemoryEventV1,
    /// The validated revision addressed by the event. Taxonomy events have no
    /// revision; all note lifecycle events must carry one.
    pub(super) revision_oid: Option<ObjectHash>,
    pub(super) note: Option<MemoryNoteV1>,
}

impl ReducedProjection {
    pub(super) fn apply(&mut self, record: ReplayRecord) -> Result<(), MemoryWriterError> {
        let event_at = record.event.at;
        let expected_seq = self
            .last_event_seq
            .checked_add(1)
            .ok_or_else(|| corrupt("Memory projection event sequence overflowed"))?;
        if record.event.event_seq != expected_seq {
            return Err(corrupt(
                "Memory projection event sequence is not contiguous",
            ));
        }
        if !self.event_ids.insert(record.event.event_id) {
            return Err(corrupt("Memory projection event ID is duplicated"));
        }

        if record.event.action == MemoryEventAction::TaxonomyExpanded {
            if record.revision_oid.is_some() || record.note.is_some() {
                return Err(corrupt("taxonomy event unexpectedly addresses a revision"));
            }
            self.last_event_seq = expected_seq;
            return Ok(());
        }

        let note_id = record
            .event
            .note_id
            .ok_or_else(|| corrupt("Memory lifecycle event has no note ID"))?;
        let revision_oid = record
            .revision_oid
            .ok_or_else(|| corrupt("Memory lifecycle event has no reachable revision"))?;
        let note = record
            .note
            .ok_or_else(|| corrupt("Memory lifecycle event has no reachable note"))?;
        let event_revision = record
            .event
            .revision_oid
            .as_deref()
            .ok_or_else(|| corrupt("Memory lifecycle event has no revision OID"))
            .and_then(parse_oid)?;
        if note_id != note.note_id || event_revision != revision_oid {
            return Err(corrupt(
                "Memory lifecycle event target disagrees with its revision",
            ));
        }

        match record.event.action {
            MemoryEventAction::Created => {
                if self.notes.contains_key(&note_id) || !note.parents.is_empty() {
                    return Err(corrupt(
                        "Memory Created transition targets an existing note",
                    ));
                }
                let mut revisions = BTreeSet::new();
                revisions.insert(revision_oid.to_string());
                self.notes.insert(
                    note_id,
                    ProjectedNote {
                        latest_revision_oid: revision_oid,
                        live_revision_oid: None,
                        latest_action: MemoryEventAction::Created,
                        review_state: ProjectedReviewState::Draft,
                        last_event_seq: expected_seq,
                        updated_at: event_at,
                        revisions,
                    },
                );
                let revision_oid = revision_oid.to_string();
                self.new_revisions.insert(revision_oid.clone(), note);
                self.new_revision_order.push(revision_oid);
                self.created_notes.insert(note_id);
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Revised => {
                let projected = self
                    .notes
                    .get_mut(&note_id)
                    .ok_or_else(|| corrupt("Memory Revised transition targets an unknown note"))?;
                if projected.revisions.contains(&revision_oid.to_string())
                    || !note
                        .parents
                        .iter()
                        .any(|parent| parent == &projected.latest_revision_oid.to_string())
                {
                    return Err(corrupt(
                        "Memory Revised transition has invalid revision ancestry",
                    ));
                }
                projected.revisions.insert(revision_oid.to_string());
                projected.latest_revision_oid = revision_oid;
                projected.latest_action = MemoryEventAction::Revised;
                projected.review_state = ProjectedReviewState::Draft;
                projected.last_event_seq = expected_seq;
                projected.updated_at = event_at;
                let revision_oid = revision_oid.to_string();
                self.new_revisions.insert(revision_oid.clone(), note);
                self.new_revision_order.push(revision_oid);
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Confirmed => {
                let projected = known_revision_mut(&mut self.notes, note_id, revision_oid)?;
                if projected.latest_revision_oid != revision_oid
                    || !matches!(
                        projected.review_state,
                        ProjectedReviewState::Draft | ProjectedReviewState::Quarantined
                    )
                {
                    return Err(corrupt(
                        "Memory Confirmed transition targets a stale revision",
                    ));
                }
                projected.live_revision_oid = Some(revision_oid);
                projected.latest_action = MemoryEventAction::Confirmed;
                projected.review_state = ProjectedReviewState::Confirmed;
                projected.last_event_seq = expected_seq;
                projected.updated_at = event_at;
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Quarantined => {
                let projected = known_revision_mut(&mut self.notes, note_id, revision_oid)?;
                if projected.latest_revision_oid != revision_oid
                    || !matches!(
                        projected.review_state,
                        ProjectedReviewState::Draft | ProjectedReviewState::Confirmed
                    )
                {
                    return Err(corrupt("Memory Quarantined transition is invalid"));
                }
                if projected.live_revision_oid == Some(revision_oid) {
                    projected.live_revision_oid = None;
                }
                projected.latest_action = MemoryEventAction::Quarantined;
                projected.review_state = ProjectedReviewState::Quarantined;
                projected.last_event_seq = expected_seq;
                projected.updated_at = event_at;
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Superseded => {
                terminal_transition(
                    &mut self.notes,
                    note_id,
                    revision_oid,
                    TerminalRule::SUPERSEDED,
                    expected_seq,
                    event_at,
                )?;
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Revoked => {
                terminal_transition(
                    &mut self.notes,
                    note_id,
                    revision_oid,
                    TerminalRule::REVOKED,
                    expected_seq,
                    event_at,
                )?;
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Forgotten => {
                terminal_transition(
                    &mut self.notes,
                    note_id,
                    revision_oid,
                    TerminalRule::FORGOTTEN,
                    expected_seq,
                    event_at,
                )?;
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::Consolidated => {
                let projected = known_revision_mut(&mut self.notes, note_id, revision_oid)?;
                if projected.latest_revision_oid != revision_oid {
                    return Err(corrupt(
                        "Memory Consolidated event targets a stale revision",
                    ));
                }
                projected.latest_action = MemoryEventAction::Consolidated;
                projected.last_event_seq = expected_seq;
                projected.updated_at = event_at;
                self.changed_notes.insert(note_id);
            }
            MemoryEventAction::TaxonomyExpanded => {
                return Err(corrupt("taxonomy event reached note lifecycle reduction"));
            }
        }
        self.last_event_seq = expected_seq;
        Ok(())
    }
}

fn known_revision_mut(
    notes: &mut BTreeMap<Uuid, ProjectedNote>,
    note_id: Uuid,
    revision_oid: ObjectHash,
) -> Result<&mut ProjectedNote, MemoryWriterError> {
    let projected = notes
        .get_mut(&note_id)
        .ok_or_else(|| corrupt("Memory transition targets an unknown note"))?;
    if !projected.revisions.contains(&revision_oid.to_string()) {
        return Err(corrupt("Memory transition targets an unknown revision"));
    }
    Ok(projected)
}

#[derive(Clone, Copy)]
struct TerminalRule {
    action: MemoryEventAction,
    state: ProjectedReviewState,
    allowed_from: &'static [ProjectedReviewState],
}

impl TerminalRule {
    const SUPERSEDED: Self = Self {
        action: MemoryEventAction::Superseded,
        state: ProjectedReviewState::Superseded,
        allowed_from: &[
            ProjectedReviewState::Confirmed,
            ProjectedReviewState::Quarantined,
        ],
    };
    const REVOKED: Self = Self {
        action: MemoryEventAction::Revoked,
        state: ProjectedReviewState::Revoked,
        allowed_from: &[
            ProjectedReviewState::Draft,
            ProjectedReviewState::Confirmed,
            ProjectedReviewState::Quarantined,
        ],
    };
    const FORGOTTEN: Self = Self {
        action: MemoryEventAction::Forgotten,
        state: ProjectedReviewState::Forgotten,
        allowed_from: &[
            ProjectedReviewState::Draft,
            ProjectedReviewState::Confirmed,
            ProjectedReviewState::Quarantined,
        ],
    };
}

fn terminal_transition(
    notes: &mut BTreeMap<Uuid, ProjectedNote>,
    note_id: Uuid,
    revision_oid: ObjectHash,
    rule: TerminalRule,
    event_seq: u64,
    event_at: DateTime<Utc>,
) -> Result<(), MemoryWriterError> {
    let projected = known_revision_mut(notes, note_id, revision_oid)?;
    if projected.latest_revision_oid != revision_oid
        || !rule.allowed_from.contains(&projected.review_state)
    {
        return Err(corrupt("Memory terminal transition is invalid"));
    }
    if projected.live_revision_oid == Some(revision_oid) {
        projected.live_revision_oid = None;
    }
    projected.latest_action = rule.action;
    projected.review_state = rule.state;
    projected.last_event_seq = event_seq;
    projected.updated_at = event_at;
    Ok(())
}

fn corrupt(summary: &'static str) -> MemoryWriterError {
    MemoryWriterError::new(MemoryWriterErrorKind::CorruptHistory, summary)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use git_internal::internal::object::types::ObjectType;

    use super::*;
    use crate::internal::ai::{
        context_budget::MemoryAnchorConfidence,
        memory::domain::{
            ActorKind, ActorRefV1, CompileOriginV1, CompileRecordV1, IdempotencyScopeV1,
            MemoryKind, MemoryLifecycle, MemoryScopeV1, MemorySensitivity, MemoryTrust,
            MemoryVisibility,
        },
    };

    fn oid(seed: &[u8]) -> ObjectHash {
        ObjectHash::from_type_and_data(ObjectType::Blob, seed)
    }

    fn note(note_id: Uuid, parents: Vec<String>, label: &str) -> MemoryNoteV1 {
        MemoryNoteV1 {
            schema_version: 1,
            note_id,
            content_digest: format!("sha256:{}", "0".repeat(64)),
            namespace: "default".to_string(),
            path: "episodic.tasks.r-test".to_string(),
            kind: MemoryKind::Episodic,
            scope: MemoryScopeV1::Repo,
            visibility: MemoryVisibility::RepoLocal,
            acl_policy_id: "repo-default-v1".to_string(),
            lifecycle: MemoryLifecycle::Accretive,
            body: label.to_string(),
            rationale: None,
            episode: None,
            evidence_refs: Vec::new(),
            links: Vec::new(),
            entities: Vec::new(),
            parents,
            tags: Vec::new(),
            confidence: MemoryAnchorConfidence::High,
            trust: MemoryTrust::RepoEvidence,
            sensitivity: MemorySensitivity::Internal,
            valid_from: None,
            valid_until: None,
            effective_from_commit: None,
            effective_until_commit: None,
            expires_at: None,
            author: ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:test".to_string(),
            },
            created_at: Utc
                .with_ymd_and_hms(2026, 8, 25, 0, 0, 0)
                .single()
                .expect("valid fixture time"),
            compile_record: CompileRecordV1 {
                schema_version: 1,
                origin: CompileOriginV1::EpisodeCompiler,
                producer: "test".to_string(),
                rules_version: 1,
                prompt_version: None,
                model_id: None,
                policy_version: "repo-policy-v1".to_string(),
                input_hashes: Vec::new(),
                idempotency_key: label.to_string(),
                idempotency_scope: IdempotencyScopeV1::Cell,
            },
        }
    }

    fn record(
        event_id: Uuid,
        event_seq: u64,
        action: MemoryEventAction,
        note: MemoryNoteV1,
        revision_oid: ObjectHash,
    ) -> ReplayRecord {
        ReplayRecord {
            event: MemoryEventV1 {
                schema_version: 1,
                event_id,
                event_seq,
                note_id: Some(note.note_id),
                revision_oid: Some(revision_oid.to_string()),
                namespace: None,
                target_path: None,
                action,
                reason_code: Some("test".to_string()),
                actor: note.author.clone(),
                at: note.created_at,
                evidence_refs: Vec::new(),
                next_note_id: None,
            },
            revision_oid: Some(revision_oid),
            note: Some(note),
        }
    }

    fn event_id(seq: u128) -> Uuid {
        Uuid::from_u128(seq)
    }

    fn confirmed_projection() -> (ReducedProjection, Uuid, ObjectHash, MemoryNoteV1) {
        let note_id = Uuid::from_u128(100);
        let revision_oid = oid(b"revision-1");
        let note = note(note_id, Vec::new(), "revision-1");
        let mut projection = ReducedProjection::default();
        projection
            .apply(record(
                event_id(1),
                1,
                MemoryEventAction::Created,
                note.clone(),
                revision_oid,
            ))
            .expect("create note");
        projection
            .apply(record(
                event_id(2),
                2,
                MemoryEventAction::Confirmed,
                note.clone(),
                revision_oid,
            ))
            .expect("confirm note");
        (projection, note_id, revision_oid, note)
    }

    #[test]
    fn reducer_covers_revision_review_and_terminal_transitions() {
        let (mut projection, note_id, first_oid, first_note) = confirmed_projection();
        let second_oid = oid(b"revision-2");
        let second_note = note(note_id, vec![first_oid.to_string()], "revision-2");
        projection
            .apply(record(
                event_id(3),
                3,
                MemoryEventAction::Revised,
                second_note.clone(),
                second_oid,
            ))
            .expect("revise note");
        let revised = projection.notes.get(&note_id).expect("projected note");
        assert_eq!(revised.review_state, ProjectedReviewState::Draft);
        assert_eq!(revised.live_revision_oid, Some(first_oid));

        projection
            .apply(record(
                event_id(4),
                4,
                MemoryEventAction::Quarantined,
                second_note.clone(),
                second_oid,
            ))
            .expect("quarantine draft");
        assert_eq!(
            projection
                .notes
                .get(&note_id)
                .expect("projected note")
                .live_revision_oid,
            Some(first_oid),
        );

        projection
            .apply(record(
                event_id(5),
                5,
                MemoryEventAction::Confirmed,
                second_note.clone(),
                second_oid,
            ))
            .expect("confirm second revision");
        projection
            .apply(record(
                event_id(6),
                6,
                MemoryEventAction::Consolidated,
                second_note.clone(),
                second_oid,
            ))
            .expect("annotate consolidation");
        assert_eq!(
            projection
                .notes
                .get(&note_id)
                .expect("projected note")
                .review_state,
            ProjectedReviewState::Confirmed,
        );

        projection
            .apply(record(
                event_id(7),
                7,
                MemoryEventAction::Revoked,
                second_note,
                second_oid,
            ))
            .expect("revoke live revision");
        let revoked = projection.notes.get(&note_id).expect("projected note");
        assert_eq!(revoked.review_state, ProjectedReviewState::Revoked);
        assert_eq!(revoked.live_revision_oid, None);

        projection
            .apply(ReplayRecord {
                event: MemoryEventV1 {
                    schema_version: 1,
                    event_id: event_id(8),
                    event_seq: 8,
                    note_id: None,
                    revision_oid: None,
                    namespace: Some("default".to_string()),
                    target_path: Some("episodic".to_string()),
                    action: MemoryEventAction::TaxonomyExpanded,
                    reason_code: Some("test".to_string()),
                    actor: first_note.author,
                    at: first_note.created_at,
                    evidence_refs: Vec::new(),
                    next_note_id: None,
                },
                revision_oid: None,
                note: None,
            })
            .expect("taxonomy annotation");
        assert_eq!(projection.last_event_seq, 8);
    }

    #[test]
    fn reducer_covers_superseded_and_forgotten_terminal_states() {
        for (action, expected) in [
            (
                MemoryEventAction::Superseded,
                ProjectedReviewState::Superseded,
            ),
            (
                MemoryEventAction::Forgotten,
                ProjectedReviewState::Forgotten,
            ),
        ] {
            let (mut projection, note_id, revision_oid, note) = confirmed_projection();
            projection
                .apply(record(event_id(3), 3, action, note, revision_oid))
                .expect("apply terminal transition");
            let projected = projection.notes.get(&note_id).expect("projected note");
            assert_eq!(projected.review_state, expected);
            assert_eq!(projected.live_revision_oid, None);
        }
    }

    #[test]
    fn reducer_rejects_gap_duplicate_and_unknown_revision() {
        let note_id = Uuid::from_u128(200);
        let revision_oid = oid(b"invalid-revision");
        let note = note(note_id, Vec::new(), "invalid");
        let mut projection = ReducedProjection::default();
        assert!(
            projection
                .apply(record(
                    event_id(1),
                    2,
                    MemoryEventAction::Created,
                    note.clone(),
                    revision_oid,
                ))
                .is_err(),
        );
        assert!(
            projection
                .apply(record(
                    event_id(2),
                    1,
                    MemoryEventAction::Confirmed,
                    note.clone(),
                    revision_oid,
                ))
                .is_err(),
        );
        projection
            .apply(record(
                event_id(3),
                1,
                MemoryEventAction::Created,
                note.clone(),
                revision_oid,
            ))
            .expect("valid create");
        assert!(
            projection
                .apply(record(
                    event_id(3),
                    2,
                    MemoryEventAction::Confirmed,
                    note,
                    revision_oid,
                ))
                .is_err(),
        );
    }

    #[test]
    fn reducer_rejects_terminal_resurrection_and_invalid_supersede() {
        let (mut revoked, _note_id, revision_oid, confirmed_note) = confirmed_projection();
        revoked
            .apply(record(
                event_id(3),
                3,
                MemoryEventAction::Revoked,
                confirmed_note.clone(),
                revision_oid,
            ))
            .expect("revoke confirmed revision");
        assert!(
            revoked
                .apply(record(
                    event_id(4),
                    4,
                    MemoryEventAction::Confirmed,
                    confirmed_note,
                    revision_oid,
                ))
                .is_err(),
        );

        let note_id = Uuid::from_u128(300);
        let draft_oid = oid(b"draft-supersede");
        let draft = note(note_id, Vec::new(), "draft-supersede");
        let mut projection = ReducedProjection::default();
        projection
            .apply(record(
                event_id(10),
                1,
                MemoryEventAction::Created,
                draft.clone(),
                draft_oid,
            ))
            .expect("create draft");
        assert!(
            projection
                .apply(record(
                    event_id(11),
                    2,
                    MemoryEventAction::Superseded,
                    draft,
                    draft_oid,
                ))
                .is_err(),
        );
    }
}
