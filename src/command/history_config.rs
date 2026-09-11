//! Typed readers for Git config defaults that can change commit history.

use serde::{Deserialize, Serialize};

use crate::internal::config::{
    LocalIdentityTarget, parse_git_config_bool, read_cascaded_config_value_strict,
};

const DEFAULT_MERGE_LOG_LIMIT: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum HistoryConfigError {
    #[error("failed to read config '{key}': {detail}")]
    Read { key: String, detail: String },
    #[error("bad config value '{value}' for '{key}' (expected {expected})")]
    Invalid {
        key: String,
        value: String,
        expected: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeFastForward {
    Allow,
    CreateMergeCommit,
    Only,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitSigningPolicy {
    #[default]
    InheritVault,
    Force,
    Disable,
}

pub(crate) async fn merge_fast_forward() -> Result<Option<MergeFastForward>, HistoryConfigError> {
    let Some(value) = read_value("merge.ff").await? else {
        return Ok(None);
    };
    if value.trim().eq_ignore_ascii_case("only") {
        return Ok(Some(MergeFastForward::Only));
    }
    let parsed = match parse_git_config_bool(&value) {
        Some(true) => MergeFastForward::Allow,
        Some(false) => MergeFastForward::CreateMergeCommit,
        None => return Err(invalid("merge.ff", value, "true, false, or only")),
    };
    Ok(Some(parsed))
}

pub(crate) async fn merge_log_limit() -> Result<usize, HistoryConfigError> {
    let Some(value) = read_value("merge.log").await? else {
        return Ok(0);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Ok(DEFAULT_MERGE_LOG_LIMIT),
        "false" | "no" | "off" => Ok(0),
        raw => raw
            .parse::<usize>()
            .map_err(|_| invalid("merge.log", value, "a boolean or non-negative integer")),
    }
}

pub(crate) async fn merge_verify_signatures() -> Result<Option<bool>, HistoryConfigError> {
    read_bool("merge.verifySignatures").await
}

pub(crate) async fn commit_signing_policy(
    no_gpg_sign: bool,
) -> Result<CommitSigningPolicy, HistoryConfigError> {
    if no_gpg_sign {
        return Ok(CommitSigningPolicy::Disable);
    }
    Ok(match read_bool("commit.gpgSign").await? {
        Some(true) => CommitSigningPolicy::Force,
        Some(false) => CommitSigningPolicy::Disable,
        None => CommitSigningPolicy::InheritVault,
    })
}

async fn read_bool(key: &str) -> Result<Option<bool>, HistoryConfigError> {
    let Some(value) = read_value(key).await? else {
        return Ok(None);
    };
    match parse_git_config_bool(&value) {
        Some(enabled) => Ok(Some(enabled)),
        None => Err(invalid(key, value, "a Git boolean")),
    }
}

async fn read_value(key: &str) -> Result<Option<String>, HistoryConfigError> {
    read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, key)
        .await
        .map_err(|error| HistoryConfigError::Read {
            key: key.to_string(),
            detail: format!("{error:#}"),
        })
}

fn invalid(key: &str, value: String, expected: &'static str) -> HistoryConfigError {
    HistoryConfigError::Invalid {
        key: key.to_string(),
        value,
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_config_error_display_is_actionable() {
        assert_eq!(
            invalid("merge.ff", "sometimes".to_string(), "true, false, or only").to_string(),
            "bad config value 'sometimes' for 'merge.ff' (expected true, false, or only)"
        );
    }
}
