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

pub(crate) const TASK_EPISODE_PROMPT_VERSION: &str = "task-episode-v1";
pub(crate) const TASK_EPISODE_RULES_VERSION: u32 = 1;
const DEFAULT_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MODEL_ID_BYTES: usize = 80;
const TASK_EPISODE_PROMPT: &str = include_str!("../../prompt/embedded/memory_task_episode_v1.md");

/// Provider-neutral Task Episode adapter. The model receives only resolver-
/// issued redacted fragments and cannot write trusted Episode fields.
#[derive(Clone)]
pub(crate) struct TaskEpisodeCompiler<M> {
    model: M,
    model_id: String,
    timeout: Duration,
}

impl TaskEpisodeCompiler<AnyCompletionModel> {
    pub(crate) fn new(model: AnyCompletionModel) -> Result<Self, EpisodeCompilerError> {
        let model_id = model.model_id().to_string();
        Self::from_parts(model, model_id, DEFAULT_PROVIDER_TIMEOUT)
    }
}

impl<M> TaskEpisodeCompiler<M> {
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
impl<M> EpisodeCompiler for TaskEpisodeCompiler<M>
where
    M: CompletionModel,
{
    async fn compile(
        &self,
        source: &RedactedEpisodeSource,
        config: &EpisodeCompileConfig,
    ) -> Result<EpisodeCompilerProposalV1, EpisodeCompilerError> {
        if source.manifest().root_kind != EpisodeRootKind::Task
            || config.rules_version() != TASK_EPISODE_RULES_VERSION
            || config.prompt_version() != TASK_EPISODE_PROMPT_VERSION
            || config.model_id() != self.model_id
        {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::InvalidConfig,
            ));
        }

        let input = TaskPromptInput {
            schema_version: 1,
            fragments: source
                .fragments()
                .iter()
                .map(|fragment| TaskPromptFragment {
                    fragment_id: fragment.fragment_id(),
                    object_type: fragment.object_type(),
                    text: fragment.text(),
                })
                .collect(),
        };
        let input_json = serde_json::to_string(&input)
            .map_err(|_| EpisodeCompilerError::new(EpisodeCompilerErrorKind::MalformedOutput))?;
        if estimate_tokens(input_json.len()) > source.manifest().limits.max_token_estimate {
            return Err(EpisodeCompilerError::new(
                EpisodeCompilerErrorKind::OutputLimitExceeded,
            ));
        }

        let mut request = CompletionRequest::new(vec![Message::user(input_json)]);
        request.preamble = Some(TASK_EPISODE_PROMPT.to_string());
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
struct TaskPromptInput<'a> {
    schema_version: u32,
    fragments: Vec<TaskPromptFragment<'a>>,
}

#[derive(Serialize)]
struct TaskPromptFragment<'a> {
    fragment_id: &'a str,
    object_type: &'a str,
    text: &'a str,
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
        completion::{
            CompletionError, CompletionResponse, Function, OneOrMany, Text, ToolCall, UserContent,
        },
        memory::{
            domain::{CodeChangeStatus, CompletionStatus},
            policy::REPO_EPISODE_PRODUCER,
        },
    };

    const MODEL_ID: &str = "memory-test-model";
    const FRAGMENT_ID: &str = "task:test";

    #[derive(Clone)]
    struct ScriptedModel {
        action: Arc<ScriptedAction>,
        captured: Arc<Mutex<Option<CompletionRequest>>>,
    }

    #[derive(Clone)]
    enum ScriptedAction {
        Text(String),
        Multiple(String),
        ToolCall,
        ProviderError,
        Delayed(String, Duration),
    }

    impl ScriptedModel {
        fn text(text: impl Into<String>) -> Self {
            Self::new(ScriptedAction::Text(text.into()))
        }

        fn new(action: ScriptedAction) -> Self {
            Self {
                action: Arc::new(action),
                captured: Arc::new(Mutex::new(None)),
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
            let content = match self.action.as_ref() {
                ScriptedAction::Text(text) => {
                    vec![AssistantContent::Text(Text { text: text.clone() })]
                }
                ScriptedAction::Multiple(text) => vec![
                    AssistantContent::Text(Text { text: text.clone() }),
                    AssistantContent::Text(Text {
                        text: "unexpected second block".to_string(),
                    }),
                ],
                ScriptedAction::ToolCall => vec![AssistantContent::ToolCall(ToolCall {
                    id: "call-1".to_string(),
                    name: "write".to_string(),
                    function: Function {
                        name: "write".to_string(),
                        arguments: json!({}),
                    },
                })],
                ScriptedAction::ProviderError => {
                    return Err(CompletionError::ProviderError(
                        "sensitive provider diagnostic".to_string(),
                    ));
                }
                ScriptedAction::Delayed(text, delay) => {
                    tokio::time::sleep(*delay).await;
                    vec![AssistantContent::Text(Text { text: text.clone() })]
                }
            };
            Ok(CompletionResponse {
                content,
                reasoning_content: None,
                raw_response: (),
            })
        }
    }

    fn source(
        root_kind: EpisodeRootKind,
        completion_status: CompletionStatus,
        code_change_status: CodeChangeStatus,
    ) -> RedactedEpisodeSource {
        RedactedEpisodeSource::for_compiler_test(
            root_kind,
            &[(
                FRAGMENT_ID,
                "task",
                "the focused task reached its terminal state",
            )],
            completion_status,
            code_change_status,
        )
    }

    fn config() -> EpisodeCompileConfig {
        EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            TASK_EPISODE_RULES_VERSION,
            TASK_EPISODE_PROMPT_VERSION,
            MODEL_ID,
        )
        .expect("construct Task Episode compile config")
    }

    fn valid_json(fragment_id: &str) -> String {
        json!({
            "summary": {
                "epistemic_status": "inference",
                "claim": "the task completed after the focused change",
                "confidence": "medium",
                "evidence_fragment_ids": [fragment_id]
            },
            "observations": [{
                "epistemic_status": "observation",
                "claim": "the task reached a terminal state",
                "confidence": null,
                "evidence_fragment_ids": [fragment_id]
            }],
            "inferences": [],
            "decisions": [],
            "failed_attempts": [],
            "unresolved": []
        })
        .to_string()
    }

    #[tokio::test]
    async fn task_completion_code_change_matrix_is_orthogonal() {
        for completion_status in [
            CompletionStatus::Completed,
            CompletionStatus::Failed,
            CompletionStatus::Cancelled,
        ] {
            for code_change_status in [
                CodeChangeStatus::Changed,
                CodeChangeStatus::Unchanged,
                CodeChangeStatus::Unknown,
            ] {
                let compiler = TaskEpisodeCompiler::for_tests(
                    ScriptedModel::text(valid_json(FRAGMENT_ID)),
                    MODEL_ID,
                )
                .expect("construct Task Episode compiler");
                let first = compiler
                    .compile(
                        &source(EpisodeRootKind::Task, completion_status, code_change_status),
                        &config(),
                    )
                    .await
                    .expect("compile status combination");
                let repeated = compiler
                    .compile(
                        &source(EpisodeRootKind::Task, completion_status, code_change_status),
                        &config(),
                    )
                    .await
                    .expect("repeat status combination");
                assert_eq!(first, repeated);
            }
        }
    }

    #[tokio::test]
    async fn request_contains_only_redacted_fragments_and_frozen_prompt() {
        let model = ScriptedModel::text(valid_json(FRAGMENT_ID));
        let compiler = TaskEpisodeCompiler::for_tests(model.clone(), MODEL_ID)
            .expect("construct Task Episode compiler");
        compiler
            .compile(
                &source(
                    EpisodeRootKind::Task,
                    CompletionStatus::Completed,
                    CodeChangeStatus::Changed,
                ),
                &config(),
            )
            .await
            .expect("compile Task Episode");
        let request = model
            .captured
            .lock()
            .await
            .take()
            .expect("completion request was captured");
        assert_eq!(request.preamble.as_deref(), Some(TASK_EPISODE_PROMPT));
        assert_eq!(request.temperature, Some(0.0));
        assert_eq!(request.stream, Some(false));
        assert!(request.tools.is_empty());
        let Message::User { content } = &request.chat_history[0] else {
            panic!("Task Episode request must contain one user message");
        };
        let OneOrMany::One(UserContent::Text(text)) = content else {
            panic!("Task Episode request must contain one text part");
        };
        let input: Value = serde_json::from_str(&text.text).expect("parse captured input");
        assert_eq!(input["fragments"][0]["fragment_id"], FRAGMENT_ID);
        assert_eq!(input["fragments"][0]["object_type"], "task");
        for forbidden in [
            "root_id",
            "repository_id",
            "principal_digest",
            "completion_status",
            "code_change_status",
            "source_ref_oid",
        ] {
            assert!(
                input.get(forbidden).is_none(),
                "unexpected field {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_non_task_sources_and_unfrozen_config() {
        let compiler =
            TaskEpisodeCompiler::for_tests(ScriptedModel::text(valid_json(FRAGMENT_ID)), MODEL_ID)
                .expect("construct Task Episode compiler");
        let intent_error = compiler
            .compile(
                &source(
                    EpisodeRootKind::Intent,
                    CompletionStatus::Completed,
                    CodeChangeStatus::Unknown,
                ),
                &config(),
            )
            .await
            .expect_err("Intent source must not cross Task adapter");
        assert_eq!(intent_error.kind(), EpisodeCompilerErrorKind::InvalidConfig);

        let wrong_prompt = EpisodeCompileConfig::new(
            REPO_EPISODE_PRODUCER,
            TASK_EPISODE_RULES_VERSION,
            "task-episode-v2",
            MODEL_ID,
        )
        .expect("construct alternate config");
        let config_error = compiler
            .compile(
                &source(
                    EpisodeRootKind::Task,
                    CompletionStatus::Completed,
                    CodeChangeStatus::Unknown,
                ),
                &wrong_prompt,
            )
            .await
            .expect_err("unfrozen prompt must be rejected");
        assert_eq!(config_error.kind(), EpisodeCompilerErrorKind::InvalidConfig);
    }

    #[tokio::test]
    async fn malformed_output_matrix_has_stable_typed_failures() {
        let valid: Value =
            serde_json::from_str(&valid_json(FRAGMENT_ID)).expect("parse valid fixture");
        let mut oversized = valid.clone();
        oversized["observations"] = Value::Array(vec![valid["observations"][0].clone(); 65]);
        let mut unknown_enum = valid.clone();
        unknown_enum["summary"]["epistemic_status"] = json!("guess");
        let mut missing_evidence = valid.clone();
        missing_evidence["summary"]["evidence_fragment_ids"] = json!([]);
        let mut trusted_injection = valid.clone();
        trusted_injection["root_id"] = json!("model-controlled-root");
        let mut observation_confidence = valid.clone();
        observation_confidence["observations"][0]["confidence"] = json!("high");
        let mut missing_confidence = valid.clone();
        missing_confidence["observations"][0]
            .as_object_mut()
            .expect("observation fixture is an object")
            .remove("confidence");

        let cases = [
            ("{".to_string(), EpisodeCompilerErrorKind::MalformedOutput),
            (
                oversized.to_string(),
                EpisodeCompilerErrorKind::OutputLimitExceeded,
            ),
            (
                unknown_enum.to_string(),
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
            (
                missing_evidence.to_string(),
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
            (
                trusted_injection.to_string(),
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
            (
                observation_confidence.to_string(),
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
            (
                missing_confidence.to_string(),
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
        ];
        for (reply, expected) in cases {
            let compiler = TaskEpisodeCompiler::for_tests(ScriptedModel::text(reply), MODEL_ID)
                .expect("construct Task Episode compiler");
            let error = compiler
                .compile(
                    &source(
                        EpisodeRootKind::Task,
                        CompletionStatus::Completed,
                        CodeChangeStatus::Changed,
                    ),
                    &config(),
                )
                .await
                .expect_err("invalid output must fail");
            assert_eq!(error.kind(), expected);
        }
    }

    #[tokio::test]
    async fn rejects_secret_echo_oversize_multiple_blocks_and_tool_calls() {
        let secret = format!("github_pat_{}", "x".repeat(60));
        let secret_reply = valid_json(FRAGMENT_ID).replace(
            "the task completed after the focused change",
            &format!("the task echoed {secret}"),
        );
        let cases = [
            (
                ScriptedAction::Text(secret_reply),
                EpisodeCompilerErrorKind::SensitiveOutput,
            ),
            (
                ScriptedAction::Text("x".repeat(MAX_COMPILER_OUTPUT_BYTES + 1)),
                EpisodeCompilerErrorKind::OutputLimitExceeded,
            ),
            (
                ScriptedAction::Multiple(valid_json(FRAGMENT_ID)),
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
            (
                ScriptedAction::ToolCall,
                EpisodeCompilerErrorKind::MalformedOutput,
            ),
        ];
        for (action, expected) in cases {
            let compiler = TaskEpisodeCompiler::for_tests(ScriptedModel::new(action), MODEL_ID)
                .expect("construct Task Episode compiler");
            let error = compiler
                .compile(
                    &source(
                        EpisodeRootKind::Task,
                        CompletionStatus::Completed,
                        CodeChangeStatus::Changed,
                    ),
                    &config(),
                )
                .await
                .expect_err("unsafe response must fail");
            assert_eq!(error.kind(), expected);
        }
    }

    #[tokio::test]
    async fn provider_error_and_timeout_do_not_expose_provider_diagnostics() {
        let provider = TaskEpisodeCompiler::for_tests(
            ScriptedModel::new(ScriptedAction::ProviderError),
            MODEL_ID,
        )
        .expect("construct provider-error compiler");
        let provider_error = provider
            .compile(
                &source(
                    EpisodeRootKind::Task,
                    CompletionStatus::Failed,
                    CodeChangeStatus::Unknown,
                ),
                &config(),
            )
            .await
            .expect_err("provider failure must be typed");
        assert_eq!(
            provider_error.kind(),
            EpisodeCompilerErrorKind::ProviderFailed
        );
        assert!(!provider_error.to_string().contains("sensitive provider"));

        let timeout = TaskEpisodeCompiler::with_timeout(
            ScriptedModel::new(ScriptedAction::Delayed(
                valid_json(FRAGMENT_ID),
                Duration::from_millis(50),
            )),
            MODEL_ID,
            Duration::from_millis(1),
        )
        .expect("construct timeout compiler");
        let timeout_error = timeout
            .compile(
                &source(
                    EpisodeRootKind::Task,
                    CompletionStatus::Cancelled,
                    CodeChangeStatus::Unchanged,
                ),
                &config(),
            )
            .await
            .expect_err("provider timeout must be typed");
        assert_eq!(
            timeout_error.kind(),
            EpisodeCompilerErrorKind::ProviderTimedOut
        );
    }

    #[cfg(feature = "test-provider")]
    #[tokio::test]
    async fn existing_test_provider_implements_task_compiler_contract() {
        use std::path::Path;

        use crate::internal::ai::providers::fake::Client;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/memory/task/valid_task_episode.json");
        let client = Client::from_fixture_path(&fixture).expect("load fake provider fixture");
        let model = AnyCompletionModel::Fake(client.completion_model(MODEL_ID));
        let compiler = TaskEpisodeCompiler::new(model).expect("construct fake Task compiler");
        let proposal = compiler
            .compile(
                &source(
                    EpisodeRootKind::Task,
                    CompletionStatus::Completed,
                    CodeChangeStatus::Changed,
                ),
                &config(),
            )
            .await
            .expect("compile through existing fake provider");
        assert_eq!(proposal.summary.evidence_fragment_ids, [FRAGMENT_ID]);
    }
}
