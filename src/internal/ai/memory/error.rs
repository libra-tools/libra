use std::fmt;

use git_internal::hash::ObjectHash;
use thiserror::Error;
use uuid::Uuid;

use super::domain::MemoryContractError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryWriterErrorKind {
    DigestKeyUnavailable,
    InvalidProposal,
    PolicyRejected,
    SourceRejected,
    SourceLimitExceeded,
    EvidenceMismatch,
    UnknownDigestKey,
    CorruptHistory,
    CorruptProjection,
    ProjectionStale,
    StorageFailure,
    ConflictExhausted,
}

impl MemoryWriterErrorKind {
    pub(crate) const fn stable_code(self) -> &'static str {
        match self {
            Self::DigestKeyUnavailable => "LBR-MEMORY-001",
            Self::InvalidProposal | Self::SourceLimitExceeded => "LBR-MEMORY-002",
            Self::PolicyRejected | Self::SourceRejected | Self::UnknownDigestKey => {
                "LBR-MEMORY-003"
            }
            Self::CorruptHistory | Self::CorruptProjection | Self::EvidenceMismatch => {
                "LBR-MEMORY-004"
            }
            Self::ProjectionStale => "LBR-MEMORY-PROJECTION-STALE",
            Self::StorageFailure | Self::ConflictExhausted => "LBR-MEMORY-005",
        }
    }
}

/// A bounded, non-content location for Memory corruption diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryDamagePoint {
    MemoryHead {
        oid: ObjectHash,
    },
    Commit {
        oid: ObjectHash,
    },
    EventObject {
        commit_oid: ObjectHash,
        event_seq: u64,
        event_oid: ObjectHash,
    },
    EventIdentity {
        event_seq: u64,
        event_id: Uuid,
    },
    EventSequence {
        event_seq: u64,
    },
}

impl fmt::Display for MemoryDamagePoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryHead { oid } => write!(formatter, "memory_head_oid={oid}"),
            Self::Commit { oid } => write!(formatter, "commit_oid={oid}"),
            Self::EventObject {
                commit_oid,
                event_seq,
                event_oid,
            } => write!(
                formatter,
                "commit_oid={commit_oid},event_seq={event_seq},event_oid={event_oid}"
            ),
            Self::EventIdentity {
                event_seq,
                event_id,
            } => write!(formatter, "event_seq={event_seq},event_id={event_id}"),
            Self::EventSequence { event_seq } => write!(formatter, "event_seq={event_seq}"),
        }
    }
}

#[derive(Clone, Debug, Error)]
#[error("{code}: {summary}", code = .kind.stable_code())]
pub(crate) struct MemoryWriterError {
    kind: MemoryWriterErrorKind,
    summary: String,
    damage_point: Option<MemoryDamagePoint>,
}

impl MemoryWriterError {
    pub(crate) fn new(kind: MemoryWriterErrorKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
            damage_point: None,
        }
    }

    /// Attach a bounded, non-content location for corruption diagnostics.
    ///
    /// The first owner that can identify a precise event/object wins. Outer
    /// layers may therefore add a coarser head OID without replacing an event
    /// sequence discovered while validating the history tree.
    pub(crate) fn with_damage_point(mut self, damage_point: MemoryDamagePoint) -> Self {
        if self.damage_point.is_none() {
            self.damage_point = Some(damage_point);
        }
        self
    }

    pub(crate) const fn kind(&self) -> MemoryWriterErrorKind {
        self.kind
    }

    pub(crate) const fn stable_code(&self) -> &'static str {
        self.kind.stable_code()
    }

    pub(crate) const fn damage_point(&self) -> Option<&MemoryDamagePoint> {
        self.damage_point.as_ref()
    }
}

impl From<MemoryContractError> for MemoryWriterError {
    fn from(error: MemoryContractError) -> Self {
        Self::new(MemoryWriterErrorKind::InvalidProposal, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_error_categories_have_stable_redacted_messages() {
        for (kind, code) in [
            (MemoryWriterErrorKind::SourceLimitExceeded, "LBR-MEMORY-002"),
            (MemoryWriterErrorKind::SourceRejected, "LBR-MEMORY-003"),
            (MemoryWriterErrorKind::EvidenceMismatch, "LBR-MEMORY-004"),
        ] {
            let error = MemoryWriterError::new(kind, "source validation failed");
            assert_eq!(error.stable_code(), code);
            assert_eq!(
                error.to_string(),
                format!("{code}: source validation failed")
            );
        }
    }

    #[test]
    fn damage_point_keeps_the_first_precise_location() {
        let error = MemoryWriterError::new(
            MemoryWriterErrorKind::CorruptHistory,
            "redacted corruption summary",
        )
        .with_damage_point(MemoryDamagePoint::EventSequence { event_seq: 7 })
        .with_damage_point(MemoryDamagePoint::MemoryHead {
            oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .expect("valid test OID"),
        });
        assert_eq!(
            error.damage_point(),
            Some(&MemoryDamagePoint::EventSequence { event_seq: 7 })
        );
        assert!(!error.to_string().contains("event_seq=7"));
    }
}
