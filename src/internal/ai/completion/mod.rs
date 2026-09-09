//! Provider-neutral completion abstractions used by AI clients and runtime phases.
//!
//! Boundary: this module defines request/response/retry/throttle contracts only;
//! provider-specific authentication and HTTP details live under `providers`.

pub mod json_repair;
pub mod message;
pub(crate) mod progress;
pub mod request;
pub mod retry;
pub mod throttle;

use std::future::Future;

pub use json_repair::{
    JsonRepairError, JsonRepairErrorKind, JsonRepairFix, JsonRepairFixKind, JsonRepairOutcome,
    parse_json_repaired, parse_tool_call_arguments_with_repair,
};
pub use message::{
    AssistantContent, Function, Message, MessageError, OneOrMany, Text, ToolCall, ToolResult,
    UserContent,
};
pub use request::{
    CompletionReasoningEffort, CompletionRequest, CompletionResponse, CompletionStreamEvent,
    CompletionThinking,
};
pub use retry::{
    CompletionRetryEvent, CompletionRetryObserver, CompletionRetryPolicy, RetryingCompletionModel,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use throttle::ThrottledCompletionModel;

#[derive(Debug, Error)]
pub enum CompletionError {
    #[error("HttpError: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JsonError: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("RequestError: {0}")]
    RequestError(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),

    #[error("ProviderError: {0}")]
    ProviderError(String),

    #[error("ResponseError: {0}")]
    ResponseError(String),

    #[error("Feature not implemented: {0}")]
    NotImplemented(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompletionUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl CompletionUsageSummary {
    pub fn merge(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cached_tokens = merge_optional_u64(self.cached_tokens, other.cached_tokens);
        self.reasoning_tokens = merge_optional_u64(self.reasoning_tokens, other.reasoning_tokens);
        self.total_tokens = merge_optional_u64(self.total_tokens, other.total_tokens);
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (Some(left), Some(right)) => Some(left + right),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
    }

    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && optional_u64_is_zero(self.cached_tokens)
            && optional_u64_is_zero(self.reasoning_tokens)
            && optional_u64_is_zero(self.total_tokens)
            && self.cost_usd.is_none()
    }
}

fn merge_optional_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn optional_u64_is_zero(value: Option<u64>) -> bool {
    value.is_none_or(|value| value == 0)
}

pub trait CompletionUsage: Send + Sync {
    fn usage_summary(&self) -> Option<CompletionUsageSummary>;
}

impl CompletionUsage for () {
    fn usage_summary(&self) -> Option<CompletionUsageSummary> {
        None
    }
}

impl CompletionUsage for serde_json::Value {
    fn usage_summary(&self) -> Option<CompletionUsageSummary> {
        None
    }
}

pub trait CompletionModel: Clone + Send + Sync {
    type Response: Send + Sync;

    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse<Self::Response>, CompletionError>> + Send;

    /// Optional method to set run ID for linking to workflow objects.
    /// Default implementation does nothing.
    fn set_run_id(&self, _run_id: String) {}
}

pub trait Prompt: Send + Sync {
    fn prompt(
        &self,
        prompt: impl Into<Message> + Send,
    ) -> impl Future<Output = Result<String, CompletionError>> + Send;
}

pub trait Chat: Send + Sync {
    fn chat(
        &self,
        prompt: impl Into<Message> + Send,
        chat_history: Vec<Message>,
    ) -> impl Future<Output = Result<String, CompletionError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CompletionUsageSummary::default()` must produce an all-zero
    /// envelope: `input_tokens` and `output_tokens` are 0; every
    /// optional field is `None`. Pin so a future Default tweak doesn't
    /// inject surprising non-zero state into the usage-aggregation
    /// path.
    #[test]
    fn usage_summary_default_is_all_zero_and_none() {
        let summary = CompletionUsageSummary::default();
        assert_eq!(summary.input_tokens, 0);
        assert_eq!(summary.output_tokens, 0);
        assert!(summary.cached_tokens.is_none());
        assert!(summary.reasoning_tokens.is_none());
        assert!(summary.total_tokens.is_none());
        assert!(summary.cost_usd.is_none());
    }

    /// `is_zero()` must return `true` for the default envelope and
    /// `false` once any field carries non-zero / Some(_) state.
    #[test]
    fn usage_summary_is_zero_only_when_fully_default() {
        let zero = CompletionUsageSummary::default();
        assert!(zero.is_zero());

        let with_input = CompletionUsageSummary {
            input_tokens: 1,
            ..Default::default()
        };
        assert!(!with_input.is_zero());

        let with_cost = CompletionUsageSummary {
            cost_usd: Some(0.0),
            ..Default::default()
        };
        assert!(
            !with_cost.is_zero(),
            "Some(0.0) cost is NOT zero — None vs Some distinguishes 'no info' from 'free'",
        );

        // Optional fields carrying Some(0) are zero too.
        let with_some_zero = CompletionUsageSummary {
            cached_tokens: Some(0),
            reasoning_tokens: Some(0),
            total_tokens: Some(0),
            ..Default::default()
        };
        assert!(with_some_zero.is_zero());
    }

    /// `merge` must sum required u64 fields with saturating arithmetic
    /// so a peak-value pair doesn't wrap to zero. Pin the canonical
    /// addition semantics.
    #[test]
    fn usage_summary_merge_uses_saturating_add_on_u64_fields() {
        let mut left = CompletionUsageSummary {
            input_tokens: u64::MAX - 5,
            output_tokens: 10,
            cached_tokens: Some(u64::MAX),
            reasoning_tokens: Some(5),
            total_tokens: None,
            cost_usd: None,
        };
        let right = CompletionUsageSummary {
            input_tokens: 100,
            output_tokens: 10,
            cached_tokens: Some(7),
            reasoning_tokens: Some(5),
            total_tokens: Some(42),
            cost_usd: Some(1.5),
        };
        left.merge(&right);

        // input_tokens saturates at u64::MAX.
        assert_eq!(left.input_tokens, u64::MAX);
        assert_eq!(left.output_tokens, 20);
        assert_eq!(left.cached_tokens, Some(u64::MAX)); // also saturated
        assert_eq!(left.reasoning_tokens, Some(10));
        // None ⊕ Some(42) → Some(42)
        assert_eq!(left.total_tokens, Some(42));
        // None ⊕ Some(1.5) → Some(1.5)
        assert_eq!(left.cost_usd, Some(1.5));
    }

    /// `merge` cost_usd combination matrix: (None, None) → None,
    /// (Some, None) preserves left, (None, Some) takes right, (Some,
    /// Some) sums. Pin all four cases so a future "drop unknown side"
    /// refactor can't silently discard cost data.
    #[test]
    fn usage_summary_merge_cost_usd_handles_optional_combinations() {
        let cases = [
            ((None, None), None),
            ((Some(1.5), None), Some(1.5)),
            ((None, Some(2.25)), Some(2.25)),
            ((Some(1.5), Some(2.25)), Some(3.75)),
        ];
        for ((lhs, rhs), expected) in cases {
            let mut left = CompletionUsageSummary {
                cost_usd: lhs,
                ..Default::default()
            };
            let right = CompletionUsageSummary {
                cost_usd: rhs,
                ..Default::default()
            };
            left.merge(&right);
            assert_eq!(left.cost_usd, expected, "lhs={lhs:?}, rhs={rhs:?}");
        }
    }

    /// `merge` optional u64 fields: combine via saturating add when
    /// both are Some, else pass through the present side.
    #[test]
    fn usage_summary_merge_optional_u64_handles_all_four_combinations() {
        // (None, None) → None.
        let mut left = CompletionUsageSummary::default();
        let right = CompletionUsageSummary::default();
        left.merge(&right);
        assert_eq!(left.total_tokens, None);

        // (None, Some) → Some.
        let mut left = CompletionUsageSummary::default();
        let right = CompletionUsageSummary {
            total_tokens: Some(42),
            ..Default::default()
        };
        left.merge(&right);
        assert_eq!(left.total_tokens, Some(42));

        // (Some, None) → preserves left.
        let mut left = CompletionUsageSummary {
            total_tokens: Some(7),
            ..Default::default()
        };
        let right = CompletionUsageSummary::default();
        left.merge(&right);
        assert_eq!(left.total_tokens, Some(7));

        // (Some, Some) → saturating add.
        let mut left = CompletionUsageSummary {
            total_tokens: Some(10),
            ..Default::default()
        };
        let right = CompletionUsageSummary {
            total_tokens: Some(5),
            ..Default::default()
        };
        left.merge(&right);
        assert_eq!(left.total_tokens, Some(15));
    }

    /// `CompletionUsageSummary` must serde round-trip — `cached_tokens`,
    /// `reasoning_tokens`, `total_tokens`, `cost_usd` all use
    /// `skip_serializing_if = "Option::is_none"`, so the serialized
    /// form of a fully-default summary contains only the required
    /// `input_tokens` and `output_tokens` fields.
    #[test]
    fn usage_summary_serde_omits_none_optional_fields() {
        let summary = CompletionUsageSummary::default();
        let json = serde_json::to_string(&summary).unwrap();
        // Required fields must always serialise.
        assert!(json.contains("\"input_tokens\":0"));
        assert!(json.contains("\"output_tokens\":0"));
        // Optional None fields must be skipped.
        assert!(!json.contains("cached_tokens"));
        assert!(!json.contains("reasoning_tokens"));
        assert!(!json.contains("total_tokens"));
        assert!(!json.contains("cost_usd"));

        // Round-trip preserves equality.
        let back: CompletionUsageSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, summary);
    }

    /// `CompletionUsageSummary` with all optional fields populated
    /// round-trips through serde without losing any field.
    #[test]
    fn usage_summary_serde_round_trips_with_all_fields_set() {
        let summary = CompletionUsageSummary {
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: Some(25),
            reasoning_tokens: Some(10),
            total_tokens: Some(150),
            cost_usd: Some(0.0123),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: CompletionUsageSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, summary);
    }

    /// `CompletionUsage for ()` returns `None` — the unit-type implementation
    /// is the default for providers that don't surface usage info.
    #[test]
    fn completion_usage_for_unit_returns_none() {
        let unit_usage: () = ();
        assert!(unit_usage.usage_summary().is_none());
    }

    /// `CompletionUsage for serde_json::Value` returns `None` —
    /// raw Value responses don't carry structured usage info; providers
    /// must wrap with a typed Response if they want usage exposed.
    #[test]
    fn completion_usage_for_serde_value_returns_none() {
        let v = serde_json::json!({"some": "response"});
        assert!(v.usage_summary().is_none());
    }

    /// `CompletionError` Display formatting must match the `#[error]`
    /// templates for every variant. Pin so a template rewrite is
    /// caught here.
    #[test]
    fn completion_error_display_pins_each_variant_template() {
        assert_eq!(
            CompletionError::ProviderError("rate limit".to_string()).to_string(),
            "ProviderError: rate limit",
        );
        assert_eq!(
            CompletionError::ResponseError("malformed".to_string()).to_string(),
            "ResponseError: malformed",
        );
        assert_eq!(
            CompletionError::NotImplemented("streaming".to_string()).to_string(),
            "Feature not implemented: streaming",
        );

        // JsonError + RequestError templates: just verify the prefix
        // matches (the inner error text is the std-library-provided
        // formatter, which we don't pin here).
        let json_err = CompletionError::JsonError(
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
        );
        assert!(
            json_err.to_string().starts_with("JsonError: "),
            "got: {json_err}",
        );

        let req_err: Box<dyn std::error::Error + Send + Sync + 'static> = "boxed".into();
        let req_err = CompletionError::RequestError(req_err);
        assert!(
            req_err.to_string().starts_with("RequestError: "),
            "got: {req_err}",
        );
    }
}
