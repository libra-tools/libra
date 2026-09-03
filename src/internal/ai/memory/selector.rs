use chrono::{DateTime, Utc};
use git_internal::hash::ObjectHash;
use uuid::Uuid;

use super::applicability::CodeApplicability;

pub(crate) const EPISODE_SELECTOR_VERSION: &str = "episode-fts-bm25-v1";

pub(crate) struct SelectableEpisode {
    pub(crate) note_id: Uuid,
    pub(crate) revision_oid: ObjectHash,
    pub(crate) bm25_score: f64,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) applicability: CodeApplicability,
}

pub(crate) fn select_episode_indexes(
    candidates: &[SelectableEpisode],
    include_diagnostics: bool,
    limit: usize,
) -> Vec<usize> {
    let mut indexes = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| include_diagnostics || candidate.applicability.injectable())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    indexes.sort_by(|left, right| {
        let left = &candidates[*left];
        let right = &candidates[*right];
        left.applicability
            .tier()
            .cmp(&right.applicability.tier())
            .then_with(|| left.bm25_score.total_cmp(&right.bm25_score))
            .then_with(|| right.ended_at.cmp(&left.ended_at))
            .then_with(|| left.note_id.cmp(&right.note_id))
            .then_with(|| {
                left.revision_oid
                    .to_string()
                    .cmp(&right.revision_oid.to_string())
            })
    });
    indexes.truncate(limit);
    indexes
}

#[cfg(test)]
mod tests {
    use git_internal::internal::object::blob::Blob;

    use super::*;

    fn candidate(
        seed: &str,
        score: f64,
        ended_at: &str,
        applicability: CodeApplicability,
    ) -> SelectableEpisode {
        SelectableEpisode {
            note_id: Uuid::new_v5(&Uuid::NAMESPACE_OID, seed.as_bytes()),
            revision_oid: Blob::from_content(seed).id,
            bm25_score: score,
            ended_at: Some(ended_at.parse().expect("timestamp")),
            applicability,
        }
    }

    #[test]
    fn selector_uses_applicability_bm25_recency_and_stable_ids() {
        let candidates = vec![
            candidate(
                "changed",
                -100.0,
                "2026-08-25T00:00:00Z",
                CodeApplicability::DescendantPathChanged,
            ),
            candidate(
                "exact-weaker",
                -1.0,
                "2026-08-25T00:00:00Z",
                CodeApplicability::Exact,
            ),
            candidate(
                "exact-stronger",
                -2.0,
                "2026-08-24T00:00:00Z",
                CodeApplicability::Exact,
            ),
        ];
        assert_eq!(select_episode_indexes(&candidates, true, 3), [2, 1, 0]);
        assert_eq!(select_episode_indexes(&candidates, false, 3), [2, 1]);
    }

    #[test]
    fn selector_breaks_equal_scores_by_newest_time_then_identity() {
        let older = candidate(
            "older",
            -1.0,
            "2026-08-24T00:00:00Z",
            CodeApplicability::DescendantUnchanged,
        );
        let newer = candidate(
            "newer",
            -1.0,
            "2026-08-25T00:00:00Z",
            CodeApplicability::DescendantUnchanged,
        );
        let candidates = vec![older, newer];
        assert_eq!(select_episode_indexes(&candidates, false, 2), [1, 0]);
    }
}
