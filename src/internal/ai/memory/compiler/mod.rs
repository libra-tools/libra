use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use super::source::RedactedEpisodeSource;

pub(crate) mod intent;
pub(crate) mod schema;
pub(crate) mod task;

pub(crate) use schema::{EpisodeClaimProposalV1, EpisodeCompilerProposalV1};

const MAX_PRODUCER_BYTES: usize = 120;
const MAX_VERSION_BYTES: usize = 80;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct EpisodeCompileConfig {
    producer: String,
    rules_version: u32,
    prompt_version: String,
    model_id: String,
}

impl EpisodeCompileConfig {
    pub(crate) fn new(
        producer: impl Into<String>,
        rules_version: u32,
        prompt_version: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Self, EpisodeCompilerError> {
        let config = Self {
            producer: producer.into(),
            rules_version,
            prompt_version: prompt_version.into(),
            model_id: model_id.into(),
        };
        if config.producer.is_empty()
            || config.producer.len() > MAX_PRODUCER_BYTES
            || config.rules_version == 0
            || config.prompt_version.is_empty()
            || config.prompt_version.len() > MAX_VERSION_BYTES
            || config.model_id.is_empty()
            || config.model_id.len() > MAX_VERSION_BYTES
        {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::InvalidConfig,
            ));
        }
        Ok(config)
    }

    pub(crate) fn producer(&self) -> &str {
        &self.producer
    }

    pub(crate) const fn rules_version(&self) -> u32 {
        self.rules_version
    }

    pub(crate) fn prompt_version(&self) -> &str {
        &self.prompt_version
    }

    pub(crate) fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeCompilerErrorKind {
    InvalidConfig,
    ProviderFailed,
    ProviderTimedOut,
    MalformedOutput,
    OutputLimitExceeded,
    SensitiveOutput,
}

#[derive(Debug, Error)]
#[error("Episode compiler failed ({kind:?})")]
pub(crate) struct EpisodeCompilerError {
    kind: EpisodeCompilerErrorKind,
}

impl EpisodeCompilerError {
    pub(crate) const fn new(kind: EpisodeCompilerErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> EpisodeCompilerErrorKind {
        self.kind
    }
}

/// Crate-private compiler seam. Adapters can inspect only redacted source and
/// return claim drafts keyed to resolver-issued fragment IDs.
#[async_trait]
pub(crate) trait EpisodeCompiler: Send + Sync {
    async fn compile(
        &self,
        source: &RedactedEpisodeSource,
        config: &EpisodeCompileConfig,
    ) -> Result<EpisodeCompilerProposalV1, EpisodeCompilerError>;
}

/// One repository worker consumes a mixed Task/Intent queue. This pair keeps
/// each adapter's frozen configuration beside it so a claimed root can never
/// be sent through the wrong prompt contract.
pub(crate) struct EpisodeCompilerSet<'a, T: ?Sized, I: ?Sized> {
    task_compiler: &'a T,
    task_config: &'a EpisodeCompileConfig,
    intent_compiler: &'a I,
    intent_config: &'a EpisodeCompileConfig,
}

impl<'a, T: ?Sized, I: ?Sized> EpisodeCompilerSet<'a, T, I> {
    pub(crate) const fn new(
        task_compiler: &'a T,
        task_config: &'a EpisodeCompileConfig,
        intent_compiler: &'a I,
        intent_config: &'a EpisodeCompileConfig,
    ) -> Self {
        Self {
            task_compiler,
            task_config,
            intent_compiler,
            intent_config,
        }
    }

    pub(crate) const fn task(&self) -> (&T, &EpisodeCompileConfig) {
        (self.task_compiler, self.task_config)
    }

    pub(crate) const fn intent(&self) -> (&I, &EpisodeCompileConfig) {
        (self.intent_compiler, self.intent_config)
    }
}
