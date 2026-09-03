use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;

use super::domain::{
    CodeChangeStatus, CompletionStatus, EpisodePayloadV1, EpisodeRoot, EpisodeRootKind,
};

pub(crate) const DEFAULT_RESULT_LIMIT: usize = 10;
pub(crate) const MAX_RESULT_LIMIT: usize = 50;
pub(crate) const MAX_CANDIDATES: usize = 200;
const MAX_PATH_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", content = "path", rename_all = "snake_case")]
pub(crate) enum EpisodePathFilter {
    Exact(String),
    Prefix(String),
}

impl EpisodePathFilter {
    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Exact(value) | Self::Prefix(value) => value,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EpisodeQueryV1 {
    pub(crate) text: Option<String>,
    pub(crate) root_kind: Option<EpisodeRootKind>,
    pub(crate) root_id: Option<String>,
    pub(crate) related_intent_id: Option<String>,
    pub(crate) related_task_id: Option<String>,
    pub(crate) ended_from: Option<DateTime<Utc>>,
    pub(crate) ended_until: Option<DateTime<Utc>>,
    /// Frozen effective time used only for validity/expiry filtering. Callers
    /// that inject context must supply this once and persist it in the shared
    /// selection receipt; the reader never consults the wall clock itself.
    pub(crate) effective_at: Option<DateTime<Utc>>,
    pub(crate) completion_status: Option<CompletionStatus>,
    pub(crate) code_change_status: Option<CodeChangeStatus>,
    pub(crate) path: Option<EpisodePathFilter>,
    pub(crate) include_diagnostics: bool,
    pub(crate) expand_evidence: bool,
    pub(crate) limit: usize,
}

impl Default for EpisodeQueryV1 {
    fn default() -> Self {
        Self {
            text: None,
            root_kind: None,
            root_id: None,
            related_intent_id: None,
            related_task_id: None,
            ended_from: None,
            ended_until: None,
            effective_at: None,
            completion_status: None,
            code_change_status: None,
            path: None,
            include_diagnostics: false,
            expand_evidence: false,
            limit: DEFAULT_RESULT_LIMIT,
        }
    }
}

impl EpisodeQueryV1 {
    pub(crate) fn validate(&self) -> Result<(), EpisodeQueryError> {
        if self.limit == 0 || self.limit > MAX_RESULT_LIMIT {
            return Err(EpisodeQueryError::new(EpisodeQueryErrorKind::InvalidLimit));
        }
        match (&self.root_kind, &self.root_id) {
            (Some(kind), Some(id)) => validate_root(*kind, id)?,
            (None, None) => {}
            _ => {
                return Err(EpisodeQueryError::new(
                    EpisodeQueryErrorKind::IncompleteRoot,
                ));
            }
        }
        if let Some(intent_id) = &self.related_intent_id {
            EpisodeRoot::intent(intent_id.clone())
                .map_err(|_| EpisodeQueryError::new(EpisodeQueryErrorKind::InvalidIdentifier))?;
        }
        if let Some(task_id) = &self.related_task_id {
            EpisodeRoot::task(task_id.clone())
                .map_err(|_| EpisodeQueryError::new(EpisodeQueryErrorKind::InvalidIdentifier))?;
        }
        if self
            .ended_from
            .zip(self.ended_until)
            .is_some_and(|(from, until)| from > until)
        {
            return Err(EpisodeQueryError::new(
                EpisodeQueryErrorKind::InvalidTimeRange,
            ));
        }
        if let Some(path) = &self.path {
            validate_path(path.value())?;
        }
        if self.text.is_none()
            && self.root_kind.is_none()
            && self.related_intent_id.is_none()
            && self.related_task_id.is_none()
            && self.ended_from.is_none()
            && self.ended_until.is_none()
            && self.completion_status.is_none()
            && self.code_change_status.is_none()
            && self.path.is_none()
        {
            return Err(EpisodeQueryError::new(EpisodeQueryErrorKind::EmptyQuery));
        }
        Ok(())
    }

    pub(crate) fn matches_episode(&self, episode: &EpisodePayloadV1) -> bool {
        self.related_intent_id.as_ref().is_none_or(|intent_id| {
            episode
                .related_intent_ids
                .iter()
                .any(|candidate| candidate == intent_id)
        }) && self.related_task_id.as_ref().is_none_or(|task_id| {
            episode
                .related_task_ids
                .iter()
                .any(|candidate| candidate == task_id)
        })
    }
}

fn validate_root(kind: EpisodeRootKind, id: &str) -> Result<(), EpisodeQueryError> {
    let result = match kind {
        EpisodeRootKind::Task => EpisodeRoot::task(id.to_string()),
        EpisodeRootKind::Intent => EpisodeRoot::intent(id.to_string()),
    };
    result
        .map(|_| ())
        .map_err(|_| EpisodeQueryError::new(EpisodeQueryErrorKind::InvalidIdentifier))
}

fn validate_path(path: &str) -> Result<(), EpisodeQueryError> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('.')
        || path.ends_with('.')
        || path.contains("..")
        || path.chars().any(|character| {
            !(character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-'))
        })
    {
        return Err(EpisodeQueryError::new(EpisodeQueryErrorKind::InvalidPath));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeQueryErrorKind {
    EmptyQuery,
    InvalidLimit,
    IncompleteRoot,
    InvalidIdentifier,
    InvalidTimeRange,
    InvalidPath,
}

#[derive(Debug, Error)]
#[error("invalid Episode query ({kind:?})")]
pub(crate) struct EpisodeQueryError {
    kind: EpisodeQueryErrorKind,
}

impl EpisodeQueryError {
    const fn new(kind: EpisodeQueryErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> EpisodeQueryErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_contract_rejects_unbounded_or_ambiguous_inputs() {
        assert_eq!(
            EpisodeQueryV1::default().validate().unwrap_err().kind(),
            EpisodeQueryErrorKind::EmptyQuery
        );
        let root_without_kind = EpisodeQueryV1 {
            root_id: Some("task-1".to_string()),
            ..EpisodeQueryV1::default()
        };
        assert_eq!(
            root_without_kind.validate().unwrap_err().kind(),
            EpisodeQueryErrorKind::IncompleteRoot
        );
        let invalid_path = EpisodeQueryV1 {
            path: Some(EpisodePathFilter::Prefix("episodic..tasks".to_string())),
            ..EpisodeQueryV1::default()
        };
        assert_eq!(
            invalid_path.validate().unwrap_err().kind(),
            EpisodeQueryErrorKind::InvalidPath
        );
        let too_many = EpisodeQueryV1 {
            text: Some("bounded".to_string()),
            limit: MAX_RESULT_LIMIT + 1,
            ..EpisodeQueryV1::default()
        };
        assert_eq!(
            too_many.validate().unwrap_err().kind(),
            EpisodeQueryErrorKind::InvalidLimit
        );
    }

    #[test]
    fn query_contract_accepts_text_and_structured_filters() {
        let query = EpisodeQueryV1 {
            text: Some("why did authentication fail".to_string()),
            root_kind: Some(EpisodeRootKind::Task),
            root_id: Some("task-1".to_string()),
            path: Some(EpisodePathFilter::Prefix("episodic.tasks".to_string())),
            completion_status: Some(CompletionStatus::Failed),
            code_change_status: Some(CodeChangeStatus::Changed),
            limit: 20,
            ..EpisodeQueryV1::default()
        };
        query.validate().expect("bounded query is valid");
    }
}
