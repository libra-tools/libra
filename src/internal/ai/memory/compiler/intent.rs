use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use super::{
    EpisodeCompileConfig, EpisodeCompiler, EpisodeCompilerError, EpisodeCompilerErrorKind,
    EpisodeCompilerProposalV1,
    schema::{MAX_COMPILER_OUTPUT_BYTES, ProposalValidationErrorKind, validate_proposal},
};
use crate::internal::ai::{
    completion::{AssistantContent, CompletionModel, CompletionRequest, Message},
    memory::{domain::EpisodeRootKind, source::RedactedEpisodeSource},
    observed_agents::Redactor,
    providers::AnyCompletionModel,
};

pub(crate) const INTENT_ITERATION_PROMPT_VERSION: &str = "intent-iteration-v1";
pub(crate) const INTENT_ITERATION_RULES_VERSION: u32 = 1;
const DEFAULT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MODEL_ID_BYTES: usize = 80;
const INTENT_ITERATION_PROMPT: &str =
    include_str!("../../prompt/embedded/memory_intent_iteration_v1.md");

/// Provider-neutral Intent Iteration adapter. It receives only redacted
/// Intent facts and compact summaries of explicitly pinned Task revisions.
#[derive(Clone)]
pub(crate) struct IntentIterationCompiler<M> {
    model: M,
    model_id: String,
    timeout: Duration,
}

impl IntentIterationCompiler<AnyCompletionModel> {
    pub(crate) fn new(model: AnyCompletionModel) -> Result<Self, EpisodeCompilerError> {
        let model_id = model.model_id().to_string();
        Self::from_parts(model, model_id, DEFAULT_PROVIDER_TIMEOUT)
    }
}

impl<M> IntentIterationCompiler<M> {
    pub(crate) fn with_model_id(
        model: M,
        model_id: impl Into<String>,
    ) -> Result<Self, EpisodeCompilerError> {
        Self::from_parts(model, model_id.into(), DEFAULT_PROVIDER_TIMEOUT)
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        model: M,
        model_id: impl Into<String>,
    ) -> Result<Self, EpisodeCompilerError> {
        Self::with_model_id(model, model_id)
    }

    #[cfg(test)]
    fn with_timeout(
        model: M,
        model_id: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, EpisodeCompilerError> {
        Self::from_parts(model, model_id.into(), timeout)
    }

    fn from_parts(
        model: M,
        model_id: String,
        timeout: Duration,
    ) -> Result<Self, EpisodeCompilerError> {
        if model_id.trim().is_empty() || model_id.len() > MAX_MODEL_ID_BYTES || timeout.is_zero() {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::InvalidConfig,
            ));
        }
        Ok(Self {
            model,
            model_id,
            timeout,
        })
    }
}

#[async_trait]
impl<M> EpisodeCompiler for IntentIterationCompiler<M>
where
    M: CompletionModel,
{
    async fn compile(
        &self,
        source: &RedactedEpisodeSource,
        config: &EpisodeCompileConfig,
    ) -> Result<EpisodeCompilerProposalV1, EpisodeCompilerError> {
        if source.manifest().root_kind != EpisodeRootKind::Intent
            || config.rules_version() != INTENT_ITERATION_RULES_VERSION
            || config.prompt_version() != INTENT_ITERATION_PROMPT_VERSION
            || config.model_id() != self.model_id
        {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::InvalidConfig,
            ));
        }

        let intent_fragments = source
            .fragments()
            .iter()
            .filter(|fragment| matches!(fragment.object_type(), "intent" | "intent_event"))
            .map(PromptFragment::from)
            .collect::<Vec<_>>();
        let task_episodes = source
            .fragments()
            .iter()
            .filter(|fragment| fragment.object_type() == "task_episode")
            .map(PromptFragment::from)
            .collect::<Vec<_>>();
        if intent_fragments.is_empty() || task_episodes.is_empty() {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::InvalidConfig,
            ));
        }
        let input = IntentPromptInput {
            schema_version: 1,
            intent_fragments,
            task_episodes,
        };
        let input_json = serde_json::to_string(&input)
            .map_err(|_| EpisodeCompilerError::new(EpisodeCompilerErrorKind::MalformedOutput))?;
        if estimate_tokens(input_json.len()) > source.manifest().limits.max_token_estimate {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::OutputLimitExceeded,
            ));
        }

        let mut request = CompletionRequest::new(vec![Message::user(input_json)]);
        request.preamble = Some(INTENT_ITERATION_PROMPT.to_string());
        request.temperature = Some(0.0);
        request.stream = Some(false);
        let response = tokio::time::timeout(self.timeout, self.model.completion(request))
            .await
            .map_err(|_| EpisodeCompilerError::new(EpisodeCompilerErrorKind::ProviderTimedOut))?
            .map_err(|_| EpisodeCompilerError::new(EpisodeCompilerErrorKind::ProviderFailed))?;

        let mut content = response.content.into_iter();
        let Some(AssistantContent::Text(text)) = content.next() else {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::MalformedOutput,
            ));
        };
        if content.next().is_some() {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::MalformedOutput,
            ));
        }
        if text.text.len() > MAX_COMPILER_OUTPUT_BYTES {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::OutputLimitExceeded,
            ));
        }
        if !Redactor::new_default()
            .redact(text.text.as_bytes())
            .1
            .matches
            .is_empty()
        {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::SensitiveOutput,
            ));
        }

        let proposal: EpisodeCompilerProposalV1 = serde_json::from_str(&text.text)
            .map_err(|_| EpisodeCompilerError::new(EpisodeCompilerErrorKind::MalformedOutput))?;
        validate_proposal(&proposal).map_err(|kind| match kind {
            ProposalValidationErrorKind::Malformed => {
                EpisodeCompilerError::new(EpisodeCompilerErrorKind::MalformedOutput)
            }
            ProposalValidationErrorKind::OutputLimitExceeded => {
                EpisodeCompilerError::new(EpisodeCompilerErrorKind::OutputLimitExceeded)
            }
        })?;
        Ok(proposal)
    }
}

#[derive(Serialize)]
struct IntentPromptInput<'a> {
    schema_version: u32,
    intent_fragments: Vec<PromptFragment<'a>>,
    task_episodes: Vec<PromptFragment<'a>>,
}

#[derive(Serialize)]
struct PromptFragment<'a> {
    fragment_id: &'a str,
    object_type: &'a str,
    text: &'a str,
}

impl<'a> From<&'a super::super::source::RedactedEpisodeFragment> for PromptFragment<'a> {
    fn from(fragment: &'a super::super::source::RedactedEpisodeFragment) -> Self {
        Self {
            fragment_id: fragment.fragment_id(),
            object_type: fragment.object_type(),
            text: fragment.text(),
        }
    }
}

const fn estimate_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    use super::*;
    use crate::internal::ai::{
        completion::{CompletionError, CompletionResponse, OneOrMany, Text, UserContent},
        memory::{
            domain::{CodeChangeStatus, CompletionStatus},
            policy::REPO_EPISODE_PRODUCER,
        },
    };

    const MODEL_ID: &str = "memory-intent-test-model";
    const INTENT_FRAGMENT: &str = "intent:test";
    const TASK_FRAGMENT: &str = "task_episode:test-task";

    #[derive(Clone)]
    struct ScriptedModel {
        reply: Arc<String>,
        captured: Arc<Mutex<Option<CompletionRequest>>>,
        delay: Duration,
    }

    impl ScriptedModel {
        fn new(reply: impl Into<String>) -> Self {
            Self {
                reply: Arc::new(reply.into()),
                captured: Arc::new(Mutex::new(None)),
                delay: Duration::ZERO,
            }
        }
    }

    impl CompletionModel for ScriptedModel {
        type Response = ();

        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
            *self.captured.lock().await = Some(request);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(CompletionResponse {
                content: vec![AssistantContent::Text(Text {
                    text: self.reply.as_ref().clone(),
                })],
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    fn source() -> RedactedEpisodeSource {
        RedactedEpisodeSource::for_compiler_test(
            EpisodeRootKind::Intent,
            &[
                (INTENT_FRAGMENT, "intent", "the requirement evolved once"),
                (
                    TASK_FRAGMENT,
                    "task_episode",
                    "the pinned task summary records one failed attempt",
                ),
                (
                    "run:forbidden",
                    "run",
                    "raw run content must stay outside the prompt",
                ),
            ],
            CompletionStatus::Completed,
            CodeChangeStatus::Changed,
        )
    }

    fn config() -> EpisodeCompileConfig {
        EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            INTENT_ITERATION_RULES_VERSION,
            INTENT_ITERATION_PROMPT_VERSION,
            MODEL_ID,
        )
        .expect("construct Intent Iteration config")
    }

    fn valid_json() -> String {
        json!({
            "summary": {
                "epistemic_status": "inference",
                "claim": "the iteration converged after correcting a repeated failure",
                "confidence": "medium",
                "evidence_fragment_ids": [INTENT_FRAGMENT, TASK_FRAGMENT]
            },
            "observations": [{
                "epistemic_status": "observation",
                "claim": "one pinned task records a failed attempt",
                "confidence": null,
                "evidence_fragment_ids": [TASK_FRAGMENT]
            }],
            "inferences": [],
            "decisions": [],
            "failed_attempts": [],
            "unresolved": []
        })
        .to_string()
    }

    #[tokio::test]
    async fn request_contains_only_intent_facts_and_compact_task_episodes() {
        let model = ScriptedModel::new(valid_json());
        let compiler = IntentIterationCompiler::for_tests(model.clone(), MODEL_ID)
            .expect("construct Intent compiler");
        compiler
            .compile(&source(), &config())
            .await
            .expect("compile Intent Iteration");
        let request = model
            .captured
            .lock()
            .await
            .take()
            .expect("capture Intent request");
        assert_eq!(request.preamble.as_deref(), Some(INTENT_ITERATION_PROMPT));
        assert_eq!(request.temperature, Some(0.0));
        assert_eq!(request.stream, Some(false));
        assert!(request.tools.is_empty());
        let Message::User { content } = &request.chat_history[0] else {
            panic!("Intent compiler must emit one user message");
        };
        let OneOrMany::One(UserContent::Text(text)) = content else {
            panic!("Intent compiler must emit one text part");
        };
        let input: Value = serde_json::from_str(&text.text).expect("parse Intent input");
        assert_eq!(input["intent_fragments"][0]["fragment_id"], INTENT_FRAGMENT);
        assert_eq!(input["task_episodes"][0]["fragment_id"], TASK_FRAGMENT);
        assert!(!text.text.contains("raw run content"));
    }

    #[tokio::test]
    async fn rejects_non_intent_sources_and_missing_task_episode() {
        let compiler =
            IntentIterationCompiler::for_tests(ScriptedModel::new(valid_json()), MODEL_ID)
                .expect("construct Intent compiler");
        let task_source = RedactedEpisodeSource::for_compiler_test(
            EpisodeRootKind::Task,
            &[("task:test", "task", "terminal task")],
            CompletionStatus::Completed,
            CodeChangeStatus::Changed,
        );
        assert_eq!(
            compiler
                .compile(&task_source, &config())
                .await
                .expect_err("Task source must not cross Intent adapter")
                .kind(),
            EpisodeCompilerErrorKind::InvalidConfig
        );
        let no_task = RedactedEpisodeSource::for_compiler_test(
            EpisodeRootKind::Intent,
            &[(INTENT_FRAGMENT, "intent", "terminal intent")],
            CompletionStatus::Completed,
            CodeChangeStatus::Unknown,
        );
        assert_eq!(
            compiler
                .compile(&no_task, &config())
                .await
                .expect_err("Intent source must pin at least one Task Episode")
                .kind(),
            EpisodeCompilerErrorKind::InvalidConfig
        );
    }

    #[tokio::test]
    async fn malformed_sensitive_and_timeout_fail_with_stable_kinds() {
        let malformed = IntentIterationCompiler::for_tests(ScriptedModel::new("{"), MODEL_ID)
            .expect("construct malformed compiler");
        assert_eq!(
            malformed
                .compile(&source(), &config())
                .await
                .expect_err("malformed output must fail")
                .kind(),
            EpisodeCompilerErrorKind::MalformedOutput
        );

        let secret_reply = valid_json().replace(
            "the iteration converged after correcting a repeated failure",
            &format!("the iteration exposed github_pat_{}", "x".repeat(60)),
        );
        let sensitive =
            IntentIterationCompiler::for_tests(ScriptedModel::new(secret_reply), MODEL_ID)
                .expect("construct sensitive compiler");
        assert_eq!(
            sensitive
                .compile(&source(), &config())
                .await
                .expect_err("secret echo must fail")
                .kind(),
            EpisodeCompilerErrorKind::SensitiveOutput
        );

        let mut delayed = ScriptedModel::new(valid_json());
        delayed.delay = Duration::from_millis(50);
        let timeout =
            IntentIterationCompiler::with_timeout(delayed, MODEL_ID, Duration::from_millis(1))
                .expect("construct timeout compiler");
        assert_eq!(
            timeout
                .compile(&source(), &config())
                .await
                .expect_err("provider timeout must fail")
                .kind(),
            EpisodeCompilerErrorKind::ProviderTimedOut
        );
    }

    #[cfg(feature = "test-provider")]
    #[tokio::test]
    async fn existing_test_provider_implements_intent_compiler_contract() {
        use std::path::Path;

        use crate::internal::ai::providers::fake::Client;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/memory/intent/valid_intent_iteration.json");
        let client = Client::from_fixture_path(&fixture).expect("load fake provider fixture");
        let model = AnyCompletionModel::Fake(client.completion_model(MODEL_ID));
        let compiler = IntentIterationCompiler::new(model).expect("construct fake Intent compiler");
        let proposal = compiler
            .compile(&source(), &config())
            .await
            .expect("compile through existing fake provider");
        assert_eq!(
            proposal.summary.evidence_fragment_ids,
            [INTENT_FRAGMENT, TASK_FRAGMENT]
        );
    }
}
