//! Fixture schema for the test-only fake provider.

use std::{fs, path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::internal::ai::completion::CompletionUsageSummary;

#[derive(Debug, Error)]
pub enum FakeFixtureError {
    #[error("failed to read fake provider fixture '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse fake provider fixture '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeFixture {
    #[serde(default)]
    pub responses: Vec<FakeResponseRule>,
    #[serde(default)]
    pub fallback: Option<FakeResponseAction>,
}

impl FakeFixture {
    pub fn from_path(path: &Path) -> Result<Self, FakeFixtureError> {
        let text = fs::read_to_string(path).map_err(|source| FakeFixtureError::Read {
            path: path.display().to_string(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| FakeFixtureError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    pub fn select<'a>(
        &'a self,
        latest_user_text: &str,
    ) -> Option<(Option<usize>, &'a FakeResponseAction)> {
        self.select_ctx(
            &FakeMatchContext {
                latest_user_text,
                after_tool_result: false,
                after_tool_name: None,
            },
            &std::collections::HashSet::new(),
        )
    }

    /// First matching rule, skipping `once` rules already consumed.
    pub fn select_ctx<'a>(
        &'a self,
        ctx: &FakeMatchContext<'_>,
        consumed: &std::collections::HashSet<usize>,
    ) -> Option<(Option<usize>, &'a FakeResponseAction)> {
        self.responses
            .iter()
            .enumerate()
            .find(|(index, rule)| {
                if rule.once && consumed.contains(index) {
                    return false;
                }
                rule.matcher.matches(ctx)
            })
            .map(|(index, rule)| (Some(index), &rule.action))
            .or_else(|| self.fallback.as_ref().map(|action| (None, action)))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeResponseRule {
    #[serde(default, rename = "match")]
    pub matcher: FakeMatcher,
    /// When true, this rule is consumed after the first match so a later
    /// tool-loop invocation with the same user text cannot replay the tool.
    #[serde(default)]
    pub once: bool,
    #[serde(flatten)]
    pub action: FakeResponseAction,
}

#[derive(Clone, Debug, Default)]
pub struct FakeMatchContext<'a> {
    pub latest_user_text: &'a str,
    /// True when the latest user message is a tool result (same-turn follow-up).
    pub after_tool_result: bool,
    /// Name of the latest tool-result, when the latest user message is one.
    pub after_tool_name: Option<&'a str>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeMatcher {
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub equals: Option<String>,
    /// When set, the rule only matches if the latest user message is (or is
    /// not) a tool-result follow-up. Distinguishes the first model call from
    /// the post-tool invocation that still carries the original prompt text.
    #[serde(default, rename = "afterToolResult")]
    pub after_tool_result: Option<bool>,
    /// When set, the rule only matches if the latest tool-result name equals
    /// this value. Distinguishes post-risk drafts from a later empty-prompt
    /// follow-up after `submit_intent_draft`.
    #[serde(default, rename = "afterToolName")]
    pub after_tool_name: Option<String>,
}

impl FakeMatcher {
    fn matches(&self, ctx: &FakeMatchContext<'_>) -> bool {
        let contains = self
            .contains
            .as_ref()
            .is_none_or(|needle| ctx.latest_user_text.contains(needle));
        let equals = self
            .equals
            .as_ref()
            .is_none_or(|expected| ctx.latest_user_text == expected);
        let after_tool_result = self
            .after_tool_result
            .is_none_or(|expected| expected == ctx.after_tool_result);
        let after_tool_name = self
            .after_tool_name
            .as_ref()
            .is_none_or(|expected| ctx.after_tool_name.is_some_and(|actual| actual == expected));
        contains && equals && after_tool_result && after_tool_name
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FakeResponseAction {
    Text {
        text: String,
        #[serde(default, rename = "delayMs", alias = "delay_ms")]
        delay_ms: u64,
        #[serde(default)]
        stream: Vec<FakeStreamDelta>,
        /// Optional deterministic provider usage emitted with this response.
        #[serde(default)]
        usage: Option<CompletionUsageSummary>,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        arguments: Value,
        #[serde(default, rename = "delayMs", alias = "delay_ms")]
        delay_ms: u64,
        #[serde(default)]
        stream: Vec<FakeStreamDelta>,
        /// Optional deterministic provider usage emitted with this response.
        #[serde(default)]
        usage: Option<CompletionUsageSummary>,
    },
    Error {
        message: String,
        #[serde(default, rename = "delayMs", alias = "delay_ms")]
        delay_ms: u64,
    },
}

impl Default for FakeResponseAction {
    fn default() -> Self {
        Self::Error {
            message: "no fake provider response matched".to_string(),
            delay_ms: 0,
        }
    }
}

impl FakeResponseAction {
    pub fn delay(&self) -> Duration {
        let millis = match self {
            Self::Text { delay_ms, .. }
            | Self::ToolCall { delay_ms, .. }
            | Self::Error { delay_ms, .. } => *delay_ms,
        };
        Duration::from_millis(millis)
    }

    pub fn stream(&self) -> &[FakeStreamDelta] {
        match self {
            Self::Text { stream, .. } | Self::ToolCall { stream, .. } => stream,
            Self::Error { .. } => &[],
        }
    }

    pub fn usage(&self) -> Option<CompletionUsageSummary> {
        match self {
            Self::Text { usage, .. } | Self::ToolCall { usage, .. } => usage.clone(),
            Self::Error { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FakeStreamDelta {
    Text { delta: String },
    Thinking { delta: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_selects_first_matching_response() {
        let fixture = FakeFixture {
            responses: vec![FakeResponseRule {
                matcher: FakeMatcher {
                    contains: Some("hello".to_string()),
                    equals: None,
                    after_tool_result: None,
                    after_tool_name: None,
                },
                once: false,
                action: FakeResponseAction::Text {
                    text: "hi".to_string(),
                    delay_ms: 0,
                    stream: vec![],
                    usage: None,
                },
            }],
            fallback: None,
        };

        let (index, action) = fixture.select("say hello").expect("match should exist");
        assert_eq!(index, Some(0));
        assert!(matches!(action, FakeResponseAction::Text { text, .. } if text == "hi"));
    }

    #[test]
    fn fixture_accepts_camel_case_delay_ms() {
        let fixture: FakeFixture = serde_json::from_value(serde_json::json!({
            "responses": [
                {
                    "match": { "contains": "slow" },
                    "type": "text",
                    "delayMs": 10000,
                    "text": "delayed"
                }
            ]
        }))
        .expect("fixture should parse");

        let (_, action) = fixture.select("slow request").expect("match should exist");

        assert_eq!(action.delay(), Duration::from_secs(10));
    }

    #[test]
    fn fixture_skips_consumed_once_rules_then_matches_after_tool_result() {
        let fixture: FakeFixture = serde_json::from_value(serde_json::json!({
            "responses": [
                {
                    "match": { "contains": "approval-shell-test", "afterToolResult": false },
                    "once": true,
                    "type": "tool_call",
                    "id": "shell-1",
                    "name": "shell",
                    "arguments": { "command": "true" }
                },
                {
                    "match": { "afterToolResult": true },
                    "type": "text",
                    "text": "turn complete"
                }
            ]
        }))
        .expect("fixture should parse");

        let first = fixture
            .select_ctx(
                &FakeMatchContext {
                    latest_user_text: "/run approval-shell-test",
                    after_tool_result: false,
                    after_tool_name: None,
                },
                &std::collections::HashSet::new(),
            )
            .expect("first match");
        assert_eq!(first.0, Some(0));
        assert!(matches!(first.1, FakeResponseAction::ToolCall { .. }));

        let mut consumed = std::collections::HashSet::new();
        consumed.insert(0);
        let follow_up = fixture
            .select_ctx(
                &FakeMatchContext {
                    latest_user_text: "/run approval-shell-test",
                    after_tool_result: true,
                    after_tool_name: Some("shell"),
                },
                &consumed,
            )
            .expect("follow-up match");
        assert_eq!(follow_up.0, Some(1));
        assert!(
            matches!(follow_up.1, FakeResponseAction::Text { text, .. } if text == "turn complete")
        );
    }

    #[test]
    fn fixture_after_tool_name_selects_post_risk_draft_not_post_draft_follow_up() {
        let fixture: FakeFixture = serde_json::from_value(serde_json::json!({
            "responses": [
                {
                    "match": {
                        "equals": "",
                        "afterToolResult": true,
                        "afterToolName": "request_user_input"
                    },
                    "type": "tool_call",
                    "id": "draft-1",
                    "name": "submit_intent_draft",
                    "arguments": {}
                }
            ],
            "fallback": { "type": "text", "text": "no second draft" }
        }))
        .expect("fixture should parse");

        let after_risk = fixture
            .select_ctx(
                &FakeMatchContext {
                    latest_user_text: "",
                    after_tool_result: true,
                    after_tool_name: Some("request_user_input"),
                },
                &std::collections::HashSet::new(),
            )
            .expect("post-risk empty prompt matches the draft");
        assert_eq!(after_risk.0, Some(0));
        assert!(matches!(
            after_risk.1,
            FakeResponseAction::ToolCall { name, .. } if name == "submit_intent_draft"
        ));

        let after_draft = fixture
            .select_ctx(
                &FakeMatchContext {
                    latest_user_text: "",
                    after_tool_result: true,
                    after_tool_name: Some("submit_intent_draft"),
                },
                &std::collections::HashSet::new(),
            )
            .expect("post-draft empty prompt falls back instead of a second draft");
        assert_eq!(after_draft.0, None);
        assert!(
            matches!(after_draft.1, FakeResponseAction::Text { text, .. } if text == "no second draft")
        );
    }

    #[test]
    fn fake_fixture_error_display_pins_each_template() {
        let read_err = FakeFixtureError::Read {
            path: "/tmp/fake.json".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        };
        assert_eq!(
            read_err.to_string(),
            "failed to read fake provider fixture '/tmp/fake.json': missing",
        );

        let parse_err = FakeFixtureError::Parse {
            path: "/tmp/fake.json".to_string(),
            source: serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
        };
        let parse_rendered = parse_err.to_string();
        assert!(
            parse_rendered.starts_with("failed to parse fake provider fixture '/tmp/fake.json': "),
            "got: {parse_rendered}",
        );
    }
}
