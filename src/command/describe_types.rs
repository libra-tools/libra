use serde::Serialize;

use crate::utils::util::CommitBaseError;

#[derive(Debug, Clone, Serialize)]
pub(super) struct DescribeOutput {
    pub(super) input: String,
    pub(super) resolved_commit: String,
    pub(super) result: String,
    pub(super) tag: Option<String>,
    pub(super) distance: Option<usize>,
    pub(super) abbreviated_commit: Option<String>,
    pub(super) exact_match: bool,
    pub(super) used_always: bool,
    pub(super) long_format: bool,
    pub(super) dirty: bool,
    pub(super) dirty_mark: Option<String>,
}

/// Result of the worktree dirty check, feeding the `--dirty`/`--broken` suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorktreeCheck {
    Clean,
    Dirty,
    /// The dirty check itself failed (e.g. a corrupt index).
    Failed,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DescribeError {
    #[error("HEAD does not point to a commit")]
    HeadUnborn,
    #[error("{0}")]
    InvalidReference(String),
    #[error("{0}")]
    ReadFailure(String),
    #[error("{0}")]
    CorruptReference(String),
    #[error("failed to load commit '{commit_id}': {detail}")]
    LoadCommit { commit_id: String, detail: String },
    #[error("no names found, cannot describe anything")]
    NoNamesFound,
    #[error("cannot describe '{commit_id}': no tag contains it")]
    NoContainingTag { commit_id: String },
    #[error("no tag exactly matches '{commit_id}'")]
    NoExactMatch { commit_id: String },
    #[error("options '--long' and '--abbrev=0' cannot be used together")]
    LongWithAbbrevZero,
    #[error("{0}")]
    InvalidArgument(String),
    #[error("option '{option}' and commit-ishes cannot be used together")]
    CommitishIncompatible { option: String },
}

impl From<CommitBaseError> for DescribeError {
    fn from(error: CommitBaseError) -> Self {
        match error {
            CommitBaseError::HeadUnborn => Self::HeadUnborn,
            CommitBaseError::InvalidReference(message) => Self::InvalidReference(message),
            error @ CommitBaseError::Ambiguous { .. } => Self::InvalidReference(error.to_string()),
            CommitBaseError::ReadFailure(message) => Self::ReadFailure(message),
            CommitBaseError::CorruptReference(message) => Self::CorruptReference(message),
        }
    }
}
