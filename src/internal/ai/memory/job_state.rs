use git_internal::hash::ObjectHash;
use thiserror::Error;

use super::domain::{EpisodeRoot, EpisodeRootKind};
use crate::internal::ai::{
    keyed_digest::SourceInputFingerprint, observed_agents::redaction::Redactor,
};

pub(crate) const COMPILE_JOB_LEASE_MS: i64 = 30_000;
pub(crate) const COMPILE_JOB_MAX_RETRIES: u32 = 5;
const COMPILE_JOB_RETRY_BASE_MS: i64 = 500;
const COMPILE_JOB_RETRY_MAX_MS: i64 = 30_000;
const MAX_LEASE_OWNER_BYTES: usize = 128;
const MAX_ERROR_SUMMARY_BYTES: usize = 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompileJobKey {
    scope_key: String,
    root: EpisodeRoot,
}

impl CompileJobKey {
    pub(crate) fn new(
        scope_key: impl Into<String>,
        root: EpisodeRoot,
    ) -> Result<Self, CompileJobStateError> {
        let scope_key = scope_key.into();
        if scope_key.is_empty()
            || scope_key.len() > 512
            || scope_key.trim() != scope_key
            || scope_key.chars().any(char::is_control)
        {
            return Err(CompileJobStateError::new(
                CompileJobStateErrorKind::InvalidInput,
            ));
        }
        Ok(Self { scope_key, root })
    }

    pub(crate) fn scope_key(&self) -> &str {
        &self.scope_key
    }

    pub(crate) const fn root(&self) -> &EpisodeRoot {
        &self.root
    }

    pub(crate) fn root_kind_label(&self) -> &'static str {
        match self.root.kind() {
            EpisodeRootKind::Task => "task",
            EpisodeRootKind::Intent => "intent",
        }
    }
}

#[derive(Clone)]
pub(crate) struct CompileJobLease {
    key: CompileJobKey,
    owner: String,
    fence: i64,
    target_generation: i64,
    terminal_source_oid: ObjectHash,
    input_fingerprint: SourceInputFingerprint,
}

impl CompileJobLease {
    pub(super) fn from_persisted(
        key: CompileJobKey,
        owner: String,
        fence: i64,
        target_generation: i64,
        terminal_source_oid: ObjectHash,
        input_fingerprint: SourceInputFingerprint,
    ) -> Result<Self, CompileJobStateError> {
        if !valid_owner(&owner) || fence <= 0 || target_generation <= 0 {
            return Err(CompileJobStateError::new(
                CompileJobStateErrorKind::CorruptState,
            ));
        }
        Ok(Self {
            key,
            owner,
            fence,
            target_generation,
            terminal_source_oid,
            input_fingerprint,
        })
    }

    pub(crate) const fn key(&self) -> &CompileJobKey {
        &self.key
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) const fn fence(&self) -> i64 {
        self.fence
    }

    pub(crate) const fn target_generation(&self) -> i64 {
        self.target_generation
    }

    pub(crate) const fn terminal_source_oid(&self) -> ObjectHash {
        self.terminal_source_oid
    }

    pub(crate) const fn input_fingerprint(&self) -> &SourceInputFingerprint {
        &self.input_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompileJobMutationOutcome {
    Applied,
    FencedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompileJobCompletionOutcome {
    Clean,
    NewGenerationPending,
    FencedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompileFailureClass {
    Transient,
    Stable,
}

pub(crate) struct StableJobFailure {
    class: CompileFailureClass,
    code: String,
    summary: String,
}

impl StableJobFailure {
    pub(crate) fn new(
        class: CompileFailureClass,
        code: impl Into<String>,
        summary: impl AsRef<str>,
    ) -> Result<Self, CompileJobStateError> {
        let code = code.into();
        if !valid_error_code(&code) {
            return Err(CompileJobStateError::new(
                CompileJobStateErrorKind::InvalidInput,
            ));
        }
        let redactor = Redactor::new_default();
        let (redacted, _) = redactor.redact(summary.as_ref().as_bytes());
        let summary = truncate_utf8(
            String::from_utf8_lossy(redacted.bytes()).as_ref(),
            MAX_ERROR_SUMMARY_BYTES,
        );
        Ok(Self {
            class,
            code,
            summary,
        })
    }

    pub(crate) const fn class(&self) -> CompileFailureClass {
        self.class
    }

    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    pub(crate) fn summary(&self) -> &str {
        &self.summary
    }
}

pub(crate) fn validate_lease_owner(owner: &str) -> Result<(), CompileJobStateError> {
    if valid_owner(owner) {
        Ok(())
    } else {
        Err(CompileJobStateError::new(
            CompileJobStateErrorKind::InvalidInput,
        ))
    }
}

pub(crate) fn retry_delay_ms(retry_count: u32) -> i64 {
    let exponent = retry_count.saturating_sub(1).min(16);
    COMPILE_JOB_RETRY_BASE_MS
        .saturating_mul(1_i64 << exponent)
        .min(COMPILE_JOB_RETRY_MAX_MS)
}

fn valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= MAX_LEASE_OWNER_BYTES
        && owner.trim() == owner
        && !owner.chars().any(char::is_control)
}

fn valid_error_code(code: &str) -> bool {
    code.len() == 14
        && code.starts_with("LBR-MEMORY-")
        && code[11..].bytes().all(|byte| byte.is_ascii_digit())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompileJobStateErrorKind {
    InvalidInput,
    CorruptState,
    Storage,
}

#[derive(Debug, Error)]
#[error("Memory compiler job state failed ({kind:?})")]
pub(crate) struct CompileJobStateError {
    kind: CompileJobStateErrorKind,
}

impl CompileJobStateError {
    pub(super) const fn new(kind: CompileJobStateErrorKind) -> Self {
        Self { kind }
    }

    pub(super) const fn storage() -> Self {
        Self::new(CompileJobStateErrorKind::Storage)
    }

    pub(crate) const fn kind(&self) -> CompileJobStateErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_bounded_exponential() {
        assert_eq!(retry_delay_ms(1), 500);
        assert_eq!(retry_delay_ms(2), 1_000);
        assert_eq!(retry_delay_ms(5), 8_000);
        assert_eq!(retry_delay_ms(u32::MAX), 30_000);
    }

    #[test]
    fn job_failure_is_redacted_and_bounded() {
        let failure = StableJobFailure::new(
            CompileFailureClass::Transient,
            "LBR-MEMORY-101",
            format!("provider token ghp_{} {}", "a".repeat(40), "界".repeat(500)),
        )
        .expect("stable job failure validates");
        assert!(!failure.summary().contains("ghp_"));
        assert!(failure.summary().len() <= MAX_ERROR_SUMMARY_BYTES);
        assert!(std::str::from_utf8(failure.summary().as_bytes()).is_ok());
    }
}
