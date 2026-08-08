//! Handler for the `request_user_input` tool.

use std::collections::HashSet;

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use super::parse_arguments;
use crate::internal::ai::tools::{
    ToolResult,
    context::{
        RequestUserInputArgs, ToolInvocation, ToolKind, ToolOutput, ToolPayload, UserInputRequest,
        UserInputResponse,
    },
    error::ToolError,
    registry::ToolHandler,
    spec::ToolSpec,
};

/// Blocking handler that presents questions to the user and awaits their response.
///
/// Communication with the TUI happens through an unbounded channel:
///
/// 1. The handler creates a `oneshot` channel for the response.
/// 2. It sends a `UserInputRequest` (questions + response sender) to the TUI
///    via the `request_tx` channel.
/// 3. It awaits the response on the `oneshot` receiver.
/// 4. The user's answers are returned as JSON to the model.
///
/// AI user story: let the agent pause for a small number of explicit human
/// decisions when local context is insufficient or assumptions would be risky,
/// then resume with structured answers instead of guessing.
pub struct RequestUserInputHandler {
    request_tx: UnboundedSender<UserInputRequest>,
}

impl RequestUserInputHandler {
    /// Create a new handler wired to the given request channel.
    pub fn new(request_tx: UnboundedSender<UserInputRequest>) -> Self {
        Self { request_tx }
    }
}

#[async_trait]
impl ToolHandler for RequestUserInputHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(ToolError::IncompatiblePayload(
                    "request_user_input requires Function payload".into(),
                ));
            }
        };

        let mut args: RequestUserInputArgs = parse_arguments(&arguments)?;

        if args.questions.is_empty() {
            return Err(ToolError::InvalidArguments(
                "At least one question is required".into(),
            ));
        }
        if args.questions.len() > 3 {
            return Err(ToolError::InvalidArguments(
                "At most three questions are allowed in one request".into(),
            ));
        }
        let mut question_ids = HashSet::with_capacity(args.questions.len());
        if args
            .questions
            .iter()
            .any(|question| !question_ids.insert(question.id.as_str()))
        {
            return Err(ToolError::InvalidArguments(
                "Question ids must be unique within one request".into(),
            ));
        }

        // Enable "None of the above" on all questions that have options,
        // matching the Codex reference behaviour.
        for q in &mut args.questions {
            if q.options.as_ref().is_some_and(|o| !o.is_empty()) {
                q.is_other = true;
            }
        }

        // Create a oneshot channel for the response.
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<UserInputResponse>();

        let request = UserInputRequest {
            call_id: invocation.call_id.clone(),
            questions: args.questions,
            response_tx,
        };

        // Send the request to the TUI.
        self.request_tx
            .send(request)
            .map_err(|_| ToolError::ExecutionFailed("TUI is not available".into()))?;

        // Block until the user responds.
        let response = response_rx
            .await
            .map_err(|_| ToolError::ExecutionFailed("User input was cancelled".into()))?;

        // Serialize the response as JSON for the model.
        let json = serde_json::to_string(&response).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to serialize response: {e}"))
        })?;

        Ok(ToolOutput::success(json))
    }

    fn schema(&self) -> ToolSpec {
        ToolSpec::request_user_input()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use tokio::sync::mpsc;

    use super::*;
    use crate::internal::ai::tools::context::UserInputAnswer;

    fn make_invocation(json: &str) -> ToolInvocation {
        ToolInvocation::new(
            "call-input-1",
            "request_user_input",
            ToolPayload::Function {
                arguments: json.to_string(),
            },
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn test_valid_request_with_options() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "q1",
                    "header": "Auth",
                    "question": "Which auth method?",
                    "options": [
                        {"label": "JWT (Recommended)", "description": "JSON Web Tokens"},
                        {"label": "OAuth", "description": "OAuth 2.0"}
                    ]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.expect("should receive request");
        assert_eq!(request.call_id, "call-input-1");
        assert_eq!(request.questions.len(), 1);
        assert_eq!(request.questions[0].id, "q1");
        // Handler should have set is_other = true
        assert!(request.questions[0].is_other);

        let mut answers = HashMap::new();
        answers.insert(
            "q1".to_string(),
            UserInputAnswer {
                answers: vec![
                    "JWT (Recommended)".to_string(),
                    "user_note: looks good".to_string(),
                ],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        let result = handle.await.unwrap();
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.is_success());
        let text = output.as_text().unwrap();
        assert!(text.contains("JWT (Recommended)"));
        assert!(text.contains("user_note"));
    }

    #[tokio::test]
    async fn duplicate_question_ids_fail_before_the_interaction_is_queued() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let result = handler
            .handle(make_invocation(
                r#"{
                    "questions": [
                        {"id": "choice", "question": "First?"},
                        {"id": "choice", "question": "Second?"}
                    ]
                }"#,
            ))
            .await
            .expect_err("duplicate ids cannot be presented as a safe multi-question interaction");

        assert!(
            result
                .to_string()
                .contains("Question ids must be unique within one request"),
            "error must tell the model how to correct the request, got {result}"
        );
        assert!(
            rx.try_recv().is_err(),
            "an invalid request must not leave a pending UI interaction"
        );
    }

    #[tokio::test]
    async fn test_request_accepts_string_options_from_lenient_models() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "risk",
                    "header": "Risk",
                    "question": "What risk level should be used?",
                    "options": ["Low", "Medium", "High"]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.expect("should receive request");
        let options = request.questions[0]
            .options
            .as_ref()
            .expect("options should be normalized");
        assert_eq!(options[0].label, "Low");
        assert_eq!(options[0].description, "");
        assert!(request.questions[0].is_other);

        let mut answers = HashMap::new();
        answers.insert(
            "risk".to_string(),
            UserInputAnswer {
                answers: vec!["Low".to_string()],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_request_accepts_option_without_description() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "mode",
                    "header": "Mode",
                    "question": "Which mode should be used?",
                    "options": [
                        {"label": "Fast"},
                        {"label": "Careful", "description": "Run extra checks"}
                    ]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.expect("should receive request");
        let options = request.questions[0]
            .options
            .as_ref()
            .expect("options should be normalized");
        assert_eq!(options[0].label, "Fast");
        assert_eq!(options[0].description, "");
        assert_eq!(options[1].description, "Run extra checks");

        let mut answers = HashMap::new();
        answers.insert(
            "mode".to_string(),
            UserInputAnswer {
                answers: vec!["Fast".to_string()],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_request_accepts_lenient_option_shapes() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "risk_profile",
                    "header": "Risk",
                    "question": "Which risk profile?",
                    "options": [
                        {"label": "Low", "description": null},
                        {"value": "Medium"},
                        {"text": "High", "description": 3}
                    ]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.expect("should receive request");
        let options = request.questions[0]
            .options
            .as_ref()
            .expect("options should be normalized");
        assert_eq!(options[0].label, "Low");
        assert_eq!(options[0].description, "");
        assert_eq!(options[1].label, "Medium");
        assert_eq!(options[2].label, "High");
        assert_eq!(options[2].description, "3");

        let mut answers = HashMap::new();
        answers.insert(
            "risk_profile".to_string(),
            UserInputAnswer {
                answers: vec!["Medium".to_string()],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_request_defaults_missing_header() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "risk_profile",
                    "question": "Which risk profile should be used?",
                    "options": ["Low", "Medium", "High"]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.expect("should receive request");
        assert_eq!(request.questions[0].header, "Risk profile");

        let mut answers = HashMap::new();
        answers.insert(
            "risk_profile".to_string(),
            UserInputAnswer {
                answers: vec!["Low".to_string()],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_request_defaults_missing_question() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "risk_profile",
                    "header": "Risk",
                    "options": ["Low", "Medium", "High"]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.expect("should receive request");
        assert_eq!(request.questions[0].question, "Please provide risk.");

        let mut answers = HashMap::new();
        answers.insert(
            "risk_profile".to_string(),
            UserInputAnswer {
                answers: vec!["Low".to_string()],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_freeform_question() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "q1",
                    "header": "Name",
                    "question": "What is the project name?"
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.unwrap();
        // No options => is_other should NOT be forced
        assert!(!request.questions[0].is_other);
        assert!(request.questions[0].options.is_none());

        let mut answers = HashMap::new();
        answers.insert(
            "q1".to_string(),
            UserInputAnswer {
                answers: vec!["my-project".to_string()],
            },
        );
        request
            .response_tx
            .send(UserInputResponse { answers })
            .unwrap();

        let result = handle.await.unwrap();
        assert!(result.is_ok());
        let text = result.unwrap().as_text().unwrap().to_string();
        assert!(text.contains("my-project"));
    }

    #[tokio::test]
    async fn test_empty_questions() {
        let (tx, _rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(r#"{"questions": []}"#);
        let result = handler.handle(inv).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cancelled_response() {
        let (tx, mut rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);

        let inv = make_invocation(
            r#"{
                "questions": [{
                    "id": "q1",
                    "header": "Test",
                    "question": "Pick one",
                    "options": [
                        {"label": "A", "description": "Option A"},
                        {"label": "B", "description": "Option B"}
                    ]
                }]
            }"#,
        );

        let handle = tokio::spawn(async move { handler.handle(inv).await });

        let request = rx.recv().await.unwrap();
        drop(request.response_tx);

        let result = handle.await.unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_schema() {
        let (tx, _rx) = mpsc::unbounded_channel::<UserInputRequest>();
        let handler = RequestUserInputHandler::new(tx);
        let spec = handler.schema();
        assert_eq!(spec.function.name, "request_user_input");
    }
}
