//! Handler for the `submit_plan_draft` tool.

use async_trait::async_trait;

use super::parse_arguments;
use crate::internal::ai::tools::{
    ToolResult,
    context::{
        MAX_SUBMIT_PLAN_DRAFT_ARGUMENT_BYTES, SubmitPlanDraftArgs, ToolInvocation, ToolKind,
        ToolOutput, ToolPayload, validate_submit_plan_draft_bounds,
    },
    error::ToolError,
    registry::ToolHandler,
    spec::ToolSpec,
};

/// Planning-only handler for Phase 1 provider drafts.
///
/// The runtime captures this tool as internal planning data. It is intentionally
/// not rendered as the generic checkbox `update_plan` transcript.
///
/// AI user story: let a provider propose ordered step titles before Libra's
/// local planner converts them into formal task records. This tool captures
/// planning intent only and must not imply that execution has started.
pub struct SubmitPlanDraftHandler;

#[async_trait]
impl ToolHandler for SubmitPlanDraftHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(ToolError::IncompatiblePayload(
                    "submit_plan_draft requires Function payload".into(),
                ));
            }
        };

        if arguments.len() > MAX_SUBMIT_PLAN_DRAFT_ARGUMENT_BYTES {
            return Err(ToolError::InvalidArguments(format!(
                "submit_plan_draft arguments exceed {MAX_SUBMIT_PLAN_DRAFT_ARGUMENT_BYTES} bytes"
            )));
        }
        let args: SubmitPlanDraftArgs = parse_arguments(&arguments)?;
        validate_plan_draft(&args)?;

        Ok(ToolOutput::success("Plan draft submitted"))
    }

    fn schema(&self) -> ToolSpec {
        ToolSpec::submit_plan_draft()
    }
}

fn validate_plan_draft(args: &SubmitPlanDraftArgs) -> ToolResult<()> {
    validate_submit_plan_draft_bounds(args).map_err(ToolError::InvalidArguments)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;

    fn make_invocation(json: &str) -> ToolInvocation {
        ToolInvocation::new(
            "call-plan-draft-1",
            "submit_plan_draft",
            ToolPayload::Function {
                arguments: json.to_string(),
            },
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn accepts_non_empty_ordered_titles() {
        let handler = SubmitPlanDraftHandler;
        let inv = make_invocation(
            r#"{
                "explanation": "split into implementation and verification",
                "steps": [
                    {"title": "Implement planning draft tool"},
                    {"title": "Verify plan review output"}
                ]
            }"#,
        );

        let output = handler.handle(inv).await.unwrap();

        assert!(output.is_success());
        assert_eq!(output.as_text(), Some("Plan draft submitted"));
    }

    #[tokio::test]
    async fn rejects_empty_drafts() {
        let handler = SubmitPlanDraftHandler;
        let inv = make_invocation(r#"{"steps":[]}"#);

        let result = handler.handle(inv).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rejects_empty_step_titles() {
        let handler = SubmitPlanDraftHandler;
        let inv = make_invocation(r#"{"steps":[{"title":"Inspect"},{"title":"  "}]}"#);

        let result = handler.handle(inv).await;

        assert!(result.is_err());
    }

    #[test]
    fn exposes_expected_schema() {
        let handler = SubmitPlanDraftHandler;
        let spec = handler.schema();

        assert_eq!(spec.function.name, "submit_plan_draft");
        let schema = serde_json::to_value(&spec.function.parameters).unwrap();
        let steps = schema
            .get("properties")
            .and_then(|properties| properties.get("steps"))
            .expect("steps schema");
        assert_eq!(steps.get("minItems"), Some(&Value::from(1)));
        assert!(
            steps
                .pointer("/items/properties/title")
                .is_some_and(|title| title.get("type") == Some(&Value::from("string")))
        );
    }
}
