use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use super::source::RedactedEpisodeSource;
use crate::internal::ai::completion::{
    CompletionModel, CompletionRequest, CompletionResponse, CompletionStreamEvent, progress,
};

const PROVIDER_IDLE_TIMEOUT: std::time::Duration = progress::EPISODE_IDLE_TIMEOUT;

/// Both reasoning and final text renew the watchdog; closing the event channel
/// does not disable it while the completion remains pending.
async fn complete_with_idle_timeout<M: CompletionModel>(
    model: &M,
    mut request: CompletionRequest,
    idle_timeout: std::time::Duration,
) -> Result<CompletionResponse<M::Response>, EpisodeCompilerError> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    request.stream_events = Some(sender);
    let completion = model.completion(request);
    tokio::pin!(completion);
    let deadline = tokio::time::sleep(idle_timeout);
    tokio::pin!(deadline);
    let mut channel_open = true;
    loop {
        tokio::select! {
            result = &mut completion => return result.map_err(|_| {
                EpisodeCompilerError::new(EpisodeCompilerErrorKind::ProviderFailed)
            }),
            event = receiver.recv(), if channel_open => {
                match event {
                    Some(CompletionStreamEvent::TextDelta { delta, .. }
                        | CompletionStreamEvent::ThinkingDelta { delta, .. }) if !delta.is_empty() => {
                        deadline.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                        progress::record();
                    }
                    None => channel_open = false,
                    _ => {}
                }
            }
            _ = &mut deadline => return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::ProviderTimedOut,
            )),
        }
    }
}

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

#[cfg(test)]
mod idle_tests {
    use std::time::Duration;

    use super::*;
    use crate::internal::ai::completion::CompletionError;

    #[derive(Clone)]
    struct StreamingModel {
        thinking: bool,
        empty: bool,
        stall: bool,
        close: bool,
    }

    impl CompletionModel for StreamingModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<()>, CompletionError> {
            if self.close {
                drop(request);
                std::future::pending::<()>().await;
            } else {
                let sender = request.stream_events.expect("progress sink");
                for _ in 0..6 {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    let delta = if self.empty { "" } else { "chunk" }.to_string();
                    let event = if self.thinking {
                        CompletionStreamEvent::ThinkingDelta {
                            request_id: None,
                            delta,
                        }
                    } else {
                        CompletionStreamEvent::TextDelta {
                            request_id: None,
                            delta,
                        }
                    };
                    let _ = sender.send(event);
                }
                if self.stall {
                    std::future::pending::<()>().await;
                }
            }
            Ok(CompletionResponse {
                content: vec![],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    #[tokio::test]
    async fn thinking_and_text_extend_compiler_and_outer_idle_deadlines() {
        for thinking in [true, false] {
            let model = StreamingModel {
                thinking,
                empty: false,
                stall: false,
                close: false,
            };
            let result = progress::with_idle_timeout(
                Duration::from_millis(120),
                complete_with_idle_timeout(
                    &model,
                    CompletionRequest::default(),
                    Duration::from_millis(100),
                ),
            )
            .await;
            assert!(matches!(result, Ok(Ok(_))));
        }
    }

    #[tokio::test]
    async fn stalled_empty_and_closed_streams_still_time_out() {
        for (empty, stall, close) in [
            (false, true, false),
            (true, false, false),
            (false, false, true),
        ] {
            let model = StreamingModel {
                thinking: true,
                empty,
                stall,
                close,
            };
            let error = complete_with_idle_timeout(
                &model,
                CompletionRequest::default(),
                Duration::from_millis(100),
            )
            .await
            .expect_err("inactivity must fail");
            assert_eq!(error.kind(), EpisodeCompilerErrorKind::ProviderTimedOut);
        }
    }
}
