//! Transcript intelligence for first-batch observed agents (AG-21).
//!
//! Pure, allocation-bounded parsers that project token usage, prompts,
//! models, modified files, subagent aggregates and skill events (E6/E7)
//! out of the raw transcript bytes each adapter reads. Everything here is
//! **fail-open**: a malformed transcript yields partial results plus
//! warnings — never an error that would block checkpoint persistence.
//! (Redaction, path validation and the write path stay fail-closed
//! elsewhere; this module only derives metadata.)
//!
//! Format provenance (see `tests/fixtures/agent_transcripts/MANIFEST.md`):
//! - Claude Code: session JSONL, entries `{type: user|assistant, message:
//!   {role, content, model?, usage?}, timestamp?}`; tool calls are
//!   `tool_use` blocks in assistant content; subagent work is the `Task`
//!   tool; slash commands appear in user text (optionally wrapped in
//!   `<command-name>` tags).
//! - Codex: rollout JSONL with heterogeneous records; user prompts carry
//!   `role:"user"` (string or block content), model ids appear under a
//!   `model` key, token counts under `usage`/`token_usage`-style objects.
//! - OpenCode: 2.0.26 session exports with typed `messages`, plus classic
//!   `info`/`parts` envelopes and best-effort flat JSONL compatibility.

use std::{borrow::Cow, collections::HashSet};

use serde_json::Value;

use super::capability::{SkillEvent, SkillEventSignal, SkillEventSource, SkillEventType, SkillRef};
use crate::internal::ai::completion::CompletionUsageSummary;

/// E7 curated skill registries (agent.md). OpenCode's upstream has no
/// verified slash-command surface distinct from its `/review`-style input
/// commands, so it shares the single-entry registry until upstream
/// evidence says otherwise.
pub const CLAUDE_CODE_SKILL_REGISTRY: &[&str] = &["/review", "/security-review", "/simplify"];

/// OpenCode-only injection grammar; other providers keep their own transcript semantics.
pub const OPENCODE_INJECTION_PREFIXES: &[&str] = &["<system-reminder>"];
const OPENCODE_INJECTION_END: &str = "</system-reminder>";

pub(crate) struct FilteredPrompt<'a> {
    pub text: Cow<'a, str>,
    pub removed: bool,
    pub malformed: bool,
}

/// Strip exact reminder blocks in one linear scan. Nested blocks stay suppressed;
/// an unfinished block suppresses its tail and is reported, never silently complete.
pub(crate) fn strip_injection_prefixes(text: &str) -> FilteredPrompt<'_> {
    if !OPENCODE_INJECTION_PREFIXES
        .iter()
        .any(|prefix| text.contains(prefix))
        && !text.contains(OPENCODE_INJECTION_END)
    {
        return FilteredPrompt {
            text: Cow::Borrowed(text),
            removed: false,
            malformed: false,
        };
    }
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut depth = 0usize;
    let mut removed = false;
    let mut malformed = false;
    while let Some(offset) = text[cursor..].find('<') {
        let start = cursor + offset;
        if depth == 0 {
            output.push_str(&text[cursor..start]);
        }
        let remaining = &text[start..];
        if let Some(prefix) = OPENCODE_INJECTION_PREFIXES
            .iter()
            .find(|prefix| remaining.starts_with(**prefix))
        {
            // Each increment consumes a non-empty tag within this bounded input;
            // nesting depth therefore cannot exceed the input's byte length.
            depth += 1;
            removed = true;
            cursor = start + prefix.len();
        } else if remaining.starts_with(OPENCODE_INJECTION_END) {
            if depth == 0 {
                malformed = true;
                output.push_str(OPENCODE_INJECTION_END);
            } else {
                depth -= 1;
            }
            cursor = start + OPENCODE_INJECTION_END.len();
        } else {
            if depth == 0 {
                output.push('<');
            }
            cursor = start + 1;
        }
    }
    if depth == 0 {
        output.push_str(&text[cursor..]);
    } else {
        malformed = true;
    }
    FilteredPrompt {
        text: Cow::Owned(output),
        removed,
        malformed,
    }
}
pub const CODEX_SKILL_REGISTRY: &[&str] = &["/review"];
pub const OPENCODE_SKILL_REGISTRY: &[&str] = &["/review"];

/// A0-07: exhaustive `AgentKind` → curated skill registry lookup. The single
/// fact source both transcript extraction and `libra agent skill` discovery
/// read through: a new `AgentKind` fails to compile here until it registers.
/// Non-first-batch agents expose no discoverable skills (`&[]`).
pub fn skill_registry_for(kind: super::adapter::AgentKind) -> &'static [&'static str] {
    use super::adapter::AgentKind;
    match kind {
        AgentKind::ClaudeCode => CLAUDE_CODE_SKILL_REGISTRY,
        AgentKind::Codex => CODEX_SKILL_REGISTRY,
        AgentKind::OpenCode => OPENCODE_SKILL_REGISTRY,
        AgentKind::Gemini | AgentKind::Cursor | AgentKind::Copilot | AgentKind::FactoryAi => &[],
    }
}

/// The full E6 token-usage projection: all SIX frozen wire keys, none
/// dropped. `summary` folds the token counts into the shared
/// [`CompletionUsageSummary`]; `api_call_count` and `subagent_tokens`
/// have no summary field, so they are surfaced explicitly here (and
/// recorded in checkpoint metadata) rather than silently discarded.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct E6TokenUsage {
    pub summary: CompletionUsageSummary,
    pub api_call_count: u64,
    pub subagent_tokens: u64,
}

/// Explicit E6 → [`CompletionUsageSummary`] mapping (frozen wire keys:
/// `input_tokens`, `cache_creation_tokens`, `cache_read_tokens`,
/// `output_tokens`, `api_call_count`, `subagent_tokens`).
///
/// Mapping decisions (documented so the E6 test can pin them):
/// - `input_tokens` → `summary.input_tokens`
/// - `output_tokens` → `summary.output_tokens`
/// - `cache_creation_tokens` + `cache_read_tokens` → `summary.cached_tokens`
///   (their sum; `None` when both keys are absent)
/// - `total_tokens` ← `input_tokens + output_tokens` (computed, since E6
///   has no explicit total)
/// - `api_call_count` and `subagent_tokens` are carried on
///   [`E6TokenUsage`] (no `CompletionUsageSummary` field exists for them).
pub fn map_e6_token_usage_full(value: &Value) -> E6TokenUsage {
    let get = |key: &str| value.get(key).and_then(Value::as_u64);
    let input = get("input_tokens").unwrap_or(0);
    let output = get("output_tokens").unwrap_or(0);
    let cache_creation = get("cache_creation_tokens");
    let cache_read = get("cache_read_tokens");
    let cached = match (cache_creation, cache_read) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    E6TokenUsage {
        summary: CompletionUsageSummary {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            reasoning_tokens: None,
            total_tokens: Some(input + output),
            cost_usd: None,
        },
        api_call_count: get("api_call_count").unwrap_or(0),
        subagent_tokens: get("subagent_tokens").unwrap_or(0),
    }
}

/// Convenience: just the [`CompletionUsageSummary`] slice of the E6
/// mapping (callers that only need token counts).
pub fn map_e6_token_usage(value: &Value) -> CompletionUsageSummary {
    map_e6_token_usage_full(value).summary
}

/// Best-effort extraction outcome. `partial` is set whenever any single
/// projection failed or the transcript contained undecodable lines; the
/// warnings are short, content-free descriptions (never raw transcript
/// text — they are additionally redacted before persistence).
#[derive(Debug, Clone, Default)]
pub struct ExtractionSummary {
    pub partial: bool,
    pub warnings: Vec<String>,
    pub prompts: Vec<String>,
    pub model: Option<String>,
    pub usage: Option<CompletionUsageSummary>,
    pub api_call_count: u64,
    pub modified_files: Vec<String>,
    pub subagent_usage: Option<CompletionUsageSummary>,
    pub skill_events: Vec<SkillEvent>,
}

/// Collection limits for the deadline-bound extraction helper.
///
/// Ordinary analyzer callers retain their historical complete projections.
/// The hook helper, by contrast, returns a small metadata frame and must
/// reject a transcript that would otherwise turn prompts, file paths, or
/// skill-event fields into a second large payload.  Reaching a limit makes
/// the summary partial with a content-free warning.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExtractionCollectionLimits {
    max_items_per_collection: usize,
    max_string_bytes: usize,
    max_retained_string_bytes: usize,
}

impl ExtractionCollectionLimits {
    pub(crate) const fn new(
        max_items_per_collection: usize,
        max_string_bytes: usize,
        max_retained_string_bytes: usize,
    ) -> Self {
        Self {
            max_items_per_collection,
            max_string_bytes,
            max_retained_string_bytes,
        }
    }
}

struct ExtractionCollectionLimiter {
    limits: ExtractionCollectionLimits,
    retained_string_bytes: usize,
    truncated: bool,
}

impl ExtractionCollectionLimiter {
    fn new(limits: ExtractionCollectionLimits) -> Self {
        Self {
            limits,
            retained_string_bytes: 0,
            truncated: false,
        }
    }

    fn max_string_bytes(&self) -> usize {
        self.limits.max_string_bytes
    }

    fn admit(&mut self, current_items: usize, string_bytes: usize) -> bool {
        current_items < self.limits.max_items_per_collection
            && string_bytes <= self.limits.max_string_bytes
            && self
                .retained_string_bytes
                .checked_add(string_bytes)
                .is_some_and(|next| next <= self.limits.max_retained_string_bytes)
    }

    fn retain(&mut self, string_bytes: usize) {
        self.retained_string_bytes = self.retained_string_bytes.saturating_add(string_bytes);
    }
}

const COLLECTION_LIMIT_WARNING: &str =
    "transcript extraction metadata exceeded its bounded collection budget";

enum ContentText {
    Empty,
    Text(String),
    TooLarge,
}

fn mark_collection_limit(
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
) {
    let Some(limiter) = limiter else {
        return;
    };
    if !limiter.truncated {
        limiter.truncated = true;
        out.partial = true;
        out.warnings.push(COLLECTION_LIMIT_WARNING.to_string());
    }
}

fn collection_exhausted(limiter: &Option<ExtractionCollectionLimiter>) -> bool {
    limiter.as_ref().is_some_and(|limiter| limiter.truncated)
}

fn admit_collection(
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
    current_items: usize,
    string_bytes: usize,
) -> bool {
    match limiter {
        None => true,
        Some(state) => {
            if state.admit(current_items, string_bytes) {
                state.retain(string_bytes);
                true
            } else {
                if !state.truncated {
                    state.truncated = true;
                    out.partial = true;
                    out.warnings.push(COLLECTION_LIMIT_WARNING.to_string());
                }
                false
            }
        }
    }
}

fn merge_usage(target: &mut Option<CompletionUsageSummary>, add: &CompletionUsageSummary) {
    match target {
        Some(existing) => existing.merge(add),
        None => *target = Some(add.clone()),
    }
}

/// Extract plain text out of a message `content` value (string or the
/// block-array form with `{"type":"text","text":...}` entries).  The
/// limited path measures before cloning so one malicious prompt cannot create
/// a second transcript-sized allocation in the extraction helper.
fn content_text(content: &Value, max_bytes: Option<usize>) -> ContentText {
    match content {
        Value::String(text) if text.is_empty() => ContentText::Empty,
        Value::String(text) if max_bytes.is_some_and(|max| text.len() > max) => {
            ContentText::TooLarge
        }
        Value::String(text) => ContentText::Text(text.clone()),
        Value::Array(blocks) => {
            let mut total = 0usize;
            let mut text_blocks = 0usize;
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(text) = block.get("text").and_then(Value::as_str) else {
                    continue;
                };
                total = match total
                    .checked_add(text.len())
                    .and_then(|total| total.checked_add(usize::from(text_blocks != 0)))
                {
                    Some(total) => total,
                    None => return ContentText::TooLarge,
                };
                text_blocks += 1;
                if max_bytes.is_some_and(|max| total > max) {
                    return ContentText::TooLarge;
                }
            }
            if text_blocks == 0 {
                return ContentText::Empty;
            }
            let mut joined = String::with_capacity(total);
            let mut appended_blocks = 0usize;
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(text) = block.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if appended_blocks != 0 {
                    joined.push('\n');
                }
                joined.push_str(text);
                appended_blocks += 1;
            }
            ContentText::Text(joined)
        }
        _ => ContentText::Empty,
    }
}

/// Match a curated slash command at the start of the prompt text (or
/// inside a `<command-name>` tag), returning the skill name plus signal.
fn match_skill(text: &str, registry: &[&str]) -> Option<(String, SkillEventSignal)> {
    let trimmed = text.trim_start();
    for skill in registry {
        if trimmed.starts_with(skill) {
            return Some(((*skill).to_string(), SkillEventSignal::InputSlashCommand));
        }
        let tag = format!("<command-name>{skill}</command-name>");
        if text.contains(&tag) {
            return Some(((*skill).to_string(), SkillEventSignal::PromptSlashCommand));
        }
    }
    None
}

fn skill_event(
    agent_slug: &str,
    skill: String,
    signal: SkillEventSignal,
    turn_id: String,
    timestamp: String,
    anchor: Option<String>,
    native: bool,
) -> SkillEvent {
    SkillEvent {
        id: format!("{turn_id}:{skill}"),
        event_type: match signal {
            SkillEventSignal::SkillToolUse => SkillEventType::ToolInvocation,
            _ => SkillEventType::PromptInvocation,
        },
        skill: SkillRef { name: skill },
        source: SkillEventSource {
            agent: agent_slug.to_string(),
            signal,
            confidence: 1.0,
        },
        turn_id,
        timestamp,
        transcript_anchor: anchor,
        native,
        collapse: false,
    }
}

fn push_prompt(
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
    text: String,
) {
    let prompt_count = out.prompts.len();
    if admit_collection(out, limiter, prompt_count, text.len()) {
        out.prompts.push(text);
    }
}

fn set_model(
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
    model: &str,
) {
    if out.model.is_none() && admit_collection(out, limiter, 0, model.len()) {
        out.model = Some(model.to_string());
    }
}

fn push_modified_file(
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
    seen_files: &mut Option<HashSet<String>>,
    path: &str,
) {
    let duplicate = match seen_files {
        Some(seen_files) => seen_files.contains(path),
        None => out.modified_files.iter().any(|existing| existing == path),
    };
    let file_count = out.modified_files.len();
    if duplicate || !admit_collection(out, limiter, file_count, path.len()) {
        return;
    }
    let path = path.to_string();
    if let Some(seen_files) = seen_files {
        // This bounded set removes the previous O(n²) de-dup scan for the
        // deadline helper. It is populated only after the admission check,
        // so it can never outgrow the retained output collection.
        seen_files.insert(path.clone());
    }
    out.modified_files.push(path);
}

fn skill_event_string_bytes(
    agent_slug: &str,
    skill: &str,
    turn_id: &str,
    timestamp: &str,
    anchor: Option<&str>,
) -> Option<usize> {
    let id_len = turn_id.len().checked_add(1)?.checked_add(skill.len())?;
    [
        id_len,
        skill.len(),
        agent_slug.len(),
        turn_id.len(),
        timestamp.len(),
    ]
    .into_iter()
    .chain(anchor.map(str::len))
    .try_fold(0usize, usize::checked_add)
}

struct SkillEventInput<'a> {
    agent_slug: &'a str,
    skill: String,
    signal: SkillEventSignal,
    turn_id: &'a str,
    timestamp: &'a str,
    anchor: Option<String>,
    native: bool,
}

fn push_skill_event(
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
    input: SkillEventInput<'_>,
) {
    let Some(string_bytes) = skill_event_string_bytes(
        input.agent_slug,
        &input.skill,
        input.turn_id,
        input.timestamp,
        input.anchor.as_deref(),
    ) else {
        mark_collection_limit(out, limiter);
        return;
    };
    let skill_event_count = out.skill_events.len();
    if admit_collection(out, limiter, skill_event_count, string_bytes) {
        out.skill_events.push(skill_event(
            input.agent_slug,
            input.skill,
            input.signal,
            input.turn_id.to_string(),
            input.timestamp.to_string(),
            input.anchor,
            input.native,
        ));
    }
}

/// Claude Code session JSONL → full extraction (analyzer + prompts +
/// tokens + model + subagent + skills). Tool names that modify the
/// worktree contribute their `input.file_path` to `modified_files`;
/// `Task` tool calls mark subagent activity (their usage is not broken
/// out per-call in the transcript, so `subagent_usage` stays the summed
/// usage of assistant turns that immediately answer a Task result —
/// approximation flagged via a warning when Task calls are present).
pub fn extract_claude_code(data: &[u8]) -> ExtractionSummary {
    extract_claude_code_with_limits(data, None)
}

/// Deadline-helper variant of [`extract_claude_code`].  It keeps the parser's
/// semantic behavior for in-budget transcripts while bounding retained
/// collection state before it becomes JSON metadata.
pub(crate) fn extract_claude_code_bounded(
    data: &[u8],
    limits: ExtractionCollectionLimits,
) -> ExtractionSummary {
    extract_claude_code_with_limits(data, Some(limits))
}

fn extract_claude_code_with_limits(
    data: &[u8],
    limits: Option<ExtractionCollectionLimits>,
) -> ExtractionSummary {
    const MODIFYING_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];
    let mut out = ExtractionSummary::default();
    let mut limiter = limits.map(ExtractionCollectionLimiter::new);
    let mut seen_files = limits.map(|_| HashSet::new());
    let mut undecodable = 0usize;
    let mut saw_task_tool = false;
    for (line_no, line) in data.split(|b| *b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            undecodable += 1;
            continue;
        };
        let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        let message = entry.get("message");
        match entry_type {
            "user" => {
                let Some(content) = message.and_then(|m| m.get("content")) else {
                    continue;
                };
                let max_string_bytes = limiter
                    .as_ref()
                    .map(ExtractionCollectionLimiter::max_string_bytes);
                let text = match content_text(content, max_string_bytes) {
                    ContentText::Empty => continue,
                    ContentText::Text(text) => text,
                    ContentText::TooLarge => {
                        mark_collection_limit(&mut out, &mut limiter);
                        break;
                    }
                };
                if let Some((skill, signal)) = match_skill(&text, CLAUDE_CODE_SKILL_REGISTRY) {
                    let fallback_turn_id;
                    let turn_id = match entry.get("uuid").and_then(Value::as_str) {
                        Some(turn_id) => turn_id,
                        None => {
                            fallback_turn_id = format!("line-{line_no}");
                            &fallback_turn_id
                        }
                    };
                    let timestamp = entry.get("timestamp").and_then(Value::as_str).unwrap_or("");
                    push_skill_event(
                        &mut out,
                        &mut limiter,
                        SkillEventInput {
                            agent_slug: "claude-code",
                            skill,
                            signal,
                            turn_id,
                            timestamp,
                            anchor: Some(format!("line:{line_no}")),
                            native: false,
                        },
                    );
                }
                push_prompt(&mut out, &mut limiter, text);
            }
            "assistant" => {
                let Some(message) = message else { continue };
                if out.model.is_none()
                    && let Some(model) = message.get("model").and_then(Value::as_str)
                {
                    set_model(&mut out, &mut limiter, model);
                }
                if let Some(usage) = message.get("usage") {
                    // Claude-native usage keys (cache_*_input_tokens) —
                    // distinct from the E6 wire form.
                    let get = |key: &str| usage.get(key).and_then(Value::as_u64);
                    let input = get("input_tokens").unwrap_or(0);
                    let output = get("output_tokens").unwrap_or(0);
                    let cached = match (
                        get("cache_creation_input_tokens"),
                        get("cache_read_input_tokens"),
                    ) {
                        (None, None) => None,
                        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
                    };
                    let summary = CompletionUsageSummary {
                        input_tokens: input,
                        output_tokens: output,
                        cached_tokens: cached,
                        reasoning_tokens: None,
                        total_tokens: Some(input + output),
                        cost_usd: None,
                    };
                    merge_usage(&mut out.usage, &summary);
                    out.api_call_count += 1;
                }
                if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                            continue;
                        }
                        let tool = block.get("name").and_then(Value::as_str).unwrap_or("");
                        if tool == "Task" {
                            saw_task_tool = true;
                        }
                        if MODIFYING_TOOLS.contains(&tool)
                            && let Some(path) = block
                                .get("input")
                                .and_then(|i| i.get("file_path"))
                                .and_then(Value::as_str)
                        {
                            push_modified_file(&mut out, &mut limiter, &mut seen_files, path);
                        }
                    }
                }
            }
            _ => {}
        }
        if collection_exhausted(&limiter) {
            break;
        }
    }
    if undecodable > 0 {
        out.partial = true;
        out.warnings.push(format!(
            "{undecodable} transcript line(s) were not valid JSON"
        ));
    }
    if saw_task_tool {
        // The parent transcript does not attribute usage per child. DR-06's
        // multi-source extractor adds usage only from independently opened
        // `<session>/subagents/*.jsonl` files; never relabel the parent total.
        out.warnings.push(
            "Task (subagent) calls present; the parent transcript alone does not attribute \
             subagent usage"
                .to_string(),
        );
        out.partial = true;
    }
    out
}

/// Codex rollout JSONL → prompts / model / token usage / skills
/// (best-effort generic shapes; see module docs).
pub fn extract_codex(data: &[u8]) -> ExtractionSummary {
    extract_generic_jsonl(data, "codex", CODEX_SKILL_REGISTRY, None)
}

/// Deadline-helper variant of [`extract_codex`].
pub(crate) fn extract_codex_bounded(
    data: &[u8],
    limits: ExtractionCollectionLimits,
) -> ExtractionSummary {
    extract_generic_jsonl(data, "codex", CODEX_SKILL_REGISTRY, Some(limits))
}

/// OpenCode session export → prompts / model / tokens / modified files / skills.
/// Native message usage totals all five counters; session-level totals are omitted.
/// Classic envelopes and flat JSONL remain best-effort compatibility inputs.
pub fn extract_opencode(data: &[u8]) -> ExtractionSummary {
    extract_opencode_with_limits(data, None)
}

/// Deadline-helper variant of [`extract_opencode`].
pub(crate) fn extract_opencode_bounded(
    data: &[u8],
    limits: ExtractionCollectionLimits,
) -> ExtractionSummary {
    extract_opencode_with_limits(data, Some(limits))
}

const OPENCODE_SHAPE_WARNING: &str =
    "OpenCode export contained an unsupported or incomplete message shape";
const OPENCODE_TOKEN_WARNING: &str = "OpenCode token counts could not be represented exactly";
const OPENCODE_FILE_WARNING: &str = "OpenCode modified-file provenance was incomplete";
const OPENCODE_STATE_WARNING: &str = "OpenCode extraction exceeded its internal state budget";
const OPENCODE_MAX_KEY_BYTES: usize = 4096;
const OPENCODE_MAX_ROOTS: usize = 16;

fn opencode_partial(out: &mut ExtractionSummary, warning: &'static str) {
    out.partial = true;
    if !out.warnings.iter().any(|existing| existing == warning) {
        out.warnings.push(warning.to_string());
    }
}

fn opencode_seen<'a>(
    seen: &mut HashSet<&'a str>,
    value: Option<&'a str>,
    cap: usize,
    out: &mut ExtractionSummary,
) -> bool {
    let Some(id) = value.filter(|id| !id.is_empty() && id.len() <= OPENCODE_MAX_KEY_BYTES) else {
        opencode_partial(out, OPENCODE_SHAPE_WARNING);
        return false;
    };
    if seen.contains(id) {
        return true;
    }
    if seen.len() >= cap {
        opencode_partial(out, OPENCODE_STATE_WARNING);
        return true;
    }
    seen.insert(id);
    false
}

fn opencode_counter(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(integer) = value.as_u64() {
        return Some(integer);
    }
    let number = value.as_f64()?;
    // IEEE-754 decimal encodings are admitted only within the exact integer range.
    (number.is_finite()
        && (0.0..=9_007_199_254_740_992.0).contains(&number)
        && number.fract() == 0.0)
        .then_some(number as u64)
}

fn opencode_usage(tokens: &Value) -> Option<CompletionUsageSummary> {
    let input = opencode_counter(tokens.get("input"))?;
    let output = opencode_counter(tokens.get("output"))?;
    let reasoning = opencode_counter(tokens.get("reasoning"))?;
    let read = opencode_counter(tokens.get("cache")?.get("read"))?;
    let write = opencode_counter(tokens.get("cache")?.get("write"))?;
    let cached = read.checked_add(write)?;
    let total = input
        .checked_add(output)?
        .checked_add(reasoning)?
        .checked_add(cached)?;
    Some(CompletionUsageSummary {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: Some(cached),
        reasoning_tokens: Some(reasoning),
        total_tokens: Some(total),
        cost_usd: None,
    })
}

fn opencode_add_usage(out: &mut ExtractionSummary, tokens: &Value) {
    let Some(usage) = opencode_usage(tokens) else {
        opencode_partial(out, OPENCODE_TOKEN_WARNING);
        return;
    };
    let merged = match out.usage.as_ref() {
        None => Some(usage),
        Some(old) => (|| {
            Some(CompletionUsageSummary {
                input_tokens: old.input_tokens.checked_add(usage.input_tokens)?,
                output_tokens: old.output_tokens.checked_add(usage.output_tokens)?,
                cached_tokens: Some(old.cached_tokens?.checked_add(usage.cached_tokens?)?),
                reasoning_tokens: Some(old.reasoning_tokens?.checked_add(usage.reasoning_tokens?)?),
                total_tokens: Some(old.total_tokens?.checked_add(usage.total_tokens?)?),
                cost_usd: None,
            })
        })(),
    };
    let Some(merged) = merged else {
        opencode_partial(out, OPENCODE_TOKEN_WARNING);
        return;
    };
    let Some(count) = out.api_call_count.checked_add(1) else {
        opencode_partial(out, OPENCODE_TOKEN_WARNING);
        return;
    };
    out.usage = Some(merged);
    out.api_call_count = count;
}

// Pure lexical projection: it never reads provider files or guesses a host home.
fn opencode_path(value: &str, absolute: bool) -> Option<String> {
    if value.is_empty()
        || value.len() > OPENCODE_MAX_KEY_BYTES
        || value.contains(['\0', '\\'])
        || value.contains("[redacted:")
        || value.starts_with('~')
        || value.contains("://")
        || value.starts_with('/') != absolute
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return absolute.then(|| "/".to_string());
    }
    Some(format!(
        "{}{}",
        if absolute { "/" } else { "" },
        parts.join("/")
    ))
}

#[derive(Clone)]
struct OpenCodeLocation {
    directory: String,
    root: String,
}

impl OpenCodeLocation {
    fn from_record(record: &Value) -> Option<Self> {
        let directory = record
            .get("location")
            .and_then(|v| v.get("directory"))
            .or_else(|| record.get("directory"))?
            .as_str()?;
        let directory = opencode_path(directory, true)?;
        let subpath = match record.get("subpath") {
            None => String::new(),
            Some(value) if value.as_str() == Some("") => String::new(),
            Some(value) => opencode_path(value.as_str()?, false)?,
        };
        let root = if subpath.is_empty() {
            directory.clone()
        } else {
            directory.strip_suffix(&format!("/{subpath}"))?.to_string()
        };
        Some(Self {
            directory,
            root: if root.is_empty() { "/".into() } else { root },
        })
    }
}

struct OpenCodeFiles {
    current: Option<OpenCodeLocation>,
    roots: Vec<String>,
    seen_files: Option<HashSet<String>>,
}

impl OpenCodeFiles {
    fn new(info: Option<&Value>, messages: &[Value], out: &mut ExtractionSummary) -> Self {
        let mut files = Self {
            current: info.and_then(OpenCodeLocation::from_record),
            roots: Vec::new(),
            seen_files: Some(HashSet::new()),
        };
        if let Some(current) = files.current.clone() {
            files.register(&current, out);
        } else if info.is_some_and(Value::is_object) {
            opencode_partial(out, OPENCODE_FILE_WARNING);
        }
        // Export info is the final location. The first switch's previous location
        // is the authoritative context for messages preceding that switch.
        if let Some(first) = messages.iter().find(|message| {
            message.get("type").and_then(Value::as_str) == Some("location-switched")
        }) {
            files.current = first
                .get("previous")
                .and_then(OpenCodeLocation::from_record);
            if files.current.is_none() {
                opencode_partial(out, OPENCODE_FILE_WARNING);
            }
            if let Some(current) = files.current.clone() {
                files.register(&current, out);
            }
        }
        files
    }

    fn register(&mut self, location: &OpenCodeLocation, out: &mut ExtractionSummary) {
        if self.roots.contains(&location.root) {
            return;
        }
        if self.roots.len() >= OPENCODE_MAX_ROOTS {
            opencode_partial(out, OPENCODE_STATE_WARNING);
            return;
        }
        self.roots.push(location.root.clone());
    }

    fn switch(&mut self, record: &Value, out: &mut ExtractionSummary) {
        self.current = OpenCodeLocation::from_record(record);
        if let Some(current) = self.current.clone() {
            self.register(&current, out);
        } else {
            opencode_partial(out, OPENCODE_FILE_WARNING);
        }
    }

    fn relative(&self, raw: &str, snapshot: bool) -> Option<String> {
        if snapshot {
            return opencode_path(raw, false);
        }
        let absolute = if raw.starts_with('/') {
            opencode_path(raw, true)?
        } else {
            let current = self.current.as_ref()?;
            if raw.len() > OPENCODE_MAX_KEY_BYTES
                || raw.contains(['\0', '\\'])
                || raw.starts_with('~')
                || raw.contains("[redacted:")
            {
                return None;
            }
            opencode_path(
                &format!("{}/{}", current.directory.trim_end_matches('/'), raw),
                true,
            )?
        };
        self.roots
            .iter()
            .filter_map(|root| {
                let prefix = format!("{}/", root.trim_end_matches('/'));
                absolute
                    .strip_prefix(&prefix)
                    .filter(|path| !path.is_empty())
                    .map(|path| (root.len(), path))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, path)| path.to_string())
    }

    fn add(
        &mut self,
        value: &Value,
        snapshot: bool,
        out: &mut ExtractionSummary,
        limiter: &mut Option<ExtractionCollectionLimiter>,
    ) -> bool {
        let Some(path) = value.as_str().and_then(|raw| self.relative(raw, snapshot)) else {
            opencode_partial(out, OPENCODE_FILE_WARNING);
            return false;
        };
        if out.modified_files.len() >= 4096 {
            opencode_partial(out, OPENCODE_STATE_WARNING);
            return false;
        }
        push_modified_file(out, limiter, &mut self.seen_files, &path);
        true
    }

    fn list(
        &mut self,
        value: &Value,
        snapshot: bool,
        out: &mut ExtractionSummary,
        limiter: &mut Option<ExtractionCollectionLimiter>,
    ) {
        let Some(paths) = value.as_array() else {
            opencode_partial(out, OPENCODE_FILE_WARNING);
            return;
        };
        for path in paths {
            self.add(path, snapshot, out, limiter);
            if collection_exhausted(limiter) {
                break;
            }
        }
    }
}

/// Validate the file-header/hunk grammar before deriving patch destinations.
/// Unsupported grammar is partial, even when a tool metadata destination exists.
fn opencode_patch_paths(text: &str) -> Option<Vec<&str>> {
    if text.len() > 1024 * 1024 {
        return None;
    }
    let mut lines: Vec<&str> = text
        .trim()
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    let first = *lines.first()?;
    let prefix = first
        .strip_prefix("cat")
        .filter(|suffix| suffix.starts_with(char::is_whitespace))
        .map(str::trim_start)
        .unwrap_or(first);
    if let Some(marker) = prefix.strip_prefix("<<") {
        let marker = marker.trim().trim_matches(['\'', '"']);
        if marker.is_empty()
            || !marker
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
            || lines.last()?.trim() != marker
        {
            return None;
        }
        lines.remove(0);
        lines.pop();
    }
    if lines.first()?.trim() != "*** Begin Patch" || lines.last()?.trim() != "*** End Patch" {
        return None;
    }
    let mut paths = Vec::new();
    let mut kind = "";
    let mut has_chunk = false;
    let mut has_body = false;
    let mut after_eof = false;
    let mut moved = false;
    for (index, line) in lines[1..lines.len() - 1].iter().enumerate() {
        let header = line.trim();
        if index == 0
            && header
                .strip_prefix("*** Environment ID:")
                .is_some_and(|id| !id.trim().is_empty())
        {
            continue;
        }
        let file = [
            ("*** Add File: ", "add"),
            ("*** Delete File: ", "delete"),
            ("*** Update File: ", "update"),
        ]
        .into_iter()
        .find_map(|(prefix, next)| header.strip_prefix(prefix).map(|path| (next, path.trim())));
        if let Some((next, path)) = file {
            if kind == "update" && !has_body {
                return None;
            }
            if path.is_empty() || path.len() > OPENCODE_MAX_KEY_BYTES || paths.len() >= 256 {
                return None;
            }
            paths.push(path);
            kind = next;
            has_chunk = false;
            has_body = false;
            after_eof = false;
            moved = false;
            continue;
        }
        if kind == "update" && !has_chunk && !moved && header.starts_with("*** Move to: ") {
            let path = header.strip_prefix("*** Move to: ")?.trim();
            if path.is_empty() || path.len() > OPENCODE_MAX_KEY_BYTES || paths.len() >= 256 {
                return None;
            }
            paths.push(path);
            moved = true;
            continue;
        }
        match kind {
            "add" if line.starts_with('+') => (),
            "update" if line.trim_end() == "*** End of File" => {
                if has_chunk && !has_body {
                    return None;
                }
                after_eof = has_chunk;
            }
            "update" if line.trim_end() == "@@" || line.trim_end().starts_with("@@ ") => {
                if has_chunk && !has_body {
                    return None;
                }
                has_chunk = true;
                has_body = false;
                after_eof = false;
            }
            "update" if !after_eof && (line.is_empty() || line.starts_with([' ', '+', '-'])) => {
                has_chunk = true;
                has_body = true;
            }
            "update" if after_eof && line.trim().is_empty() => (),
            _ => return None,
        }
    }
    if kind == "update" && !has_body {
        return None;
    }
    Some(paths)
}

fn opencode_tool(
    part: &Value,
    files: &mut OpenCodeFiles,
    legacy: bool,
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
) {
    let name = part
        .get(if legacy { "tool" } else { "name" })
        .and_then(Value::as_str)
        .unwrap_or("");
    if !matches!(name, "edit" | "write" | "patch" | "apply_patch") {
        return;
    }
    let Some(state) = part
        .get("state")
        .filter(|state| state.get("status").and_then(Value::as_str) == Some("completed"))
    else {
        opencode_partial(out, OPENCODE_FILE_WARNING);
        return;
    };
    let mut found = false;
    if let Some(metadata) = state
        .get("metadata")
        .and_then(|metadata| metadata.get("files"))
    {
        if let Some(entries) = metadata.as_array() {
            for entry in entries {
                found |= files.add(
                    entry.get("file").unwrap_or(&Value::Null),
                    false,
                    out,
                    limiter,
                );
                if collection_exhausted(limiter) {
                    break;
                }
            }
        } else {
            opencode_partial(out, OPENCODE_FILE_WARNING);
        }
    }
    if let Some(input) = state.get("input") {
        if let Some(path) = input.get("path").or_else(|| input.get("filePath")) {
            found |= files.add(path, false, out, limiter);
        }
        if matches!(name, "patch" | "apply_patch") {
            let paths = input
                .get("patchText")
                .or_else(|| input.get("patch"))
                .and_then(Value::as_str)
                .and_then(opencode_patch_paths);
            if let Some(paths) = paths {
                for path in paths {
                    if out.modified_files.len() >= 4096 {
                        opencode_partial(out, OPENCODE_STATE_WARNING);
                        break;
                    }
                    if let Some(relative) = files.relative(path, false) {
                        push_modified_file(out, limiter, &mut files.seen_files, &relative);
                        found = true;
                    } else {
                        opencode_partial(out, OPENCODE_FILE_WARNING);
                    }
                    if collection_exhausted(limiter) {
                        break;
                    }
                }
            } else {
                opencode_partial(out, OPENCODE_FILE_WARNING);
            }
        }
    } else {
        opencode_partial(out, OPENCODE_FILE_WARNING);
    }
    if !found {
        opencode_partial(out, OPENCODE_FILE_WARNING);
    }
}

fn opencode_prompt(
    text: &str,
    ordinal: usize,
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
) {
    if limiter
        .as_ref()
        .is_some_and(|limiter| text.len() > limiter.max_string_bytes())
    {
        mark_collection_limit(out, limiter);
        return;
    }
    let filtered = strip_injection_prefixes(text);
    if filtered.malformed {
        opencode_partial(out, OPENCODE_SHAPE_WARNING);
    }
    if filtered.removed && filtered.text.trim().is_empty() {
        return;
    }
    let text = filtered.text.as_ref();
    if text.contains("[redacted:") {
        opencode_partial(out, OPENCODE_SHAPE_WARNING);
        return;
    }
    if let Some((skill, signal)) = match_skill(text, OPENCODE_SKILL_REGISTRY) {
        push_skill_event(
            out,
            limiter,
            SkillEventInput {
                agent_slug: "opencode",
                skill,
                signal,
                turn_id: &format!("record-{ordinal}"),
                timestamp: "",
                anchor: Some(format!("record:{ordinal}")),
                native: false,
            },
        );
    }
    push_prompt(out, limiter, text.to_string());
}

fn extract_opencode_with_limits(
    data: &[u8],
    limits: Option<ExtractionCollectionLimits>,
) -> ExtractionSummary {
    let Ok(doc) = serde_json::from_slice::<Value>(data) else {
        return extract_generic_jsonl(data, "opencode", OPENCODE_SKILL_REGISTRY, limits);
    };
    let Some(messages) = doc
        .get("messages")
        .or_else(|| doc.get("parts"))
        .and_then(Value::as_array)
    else {
        // A native envelope cannot silently fall through to a generic zero result.
        if doc.get("info").is_some() || doc.get("messages").is_some() || doc.get("type").is_some() {
            let mut out = ExtractionSummary::default();
            opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
            return out;
        }
        return extract_generic_jsonl(data, "opencode", OPENCODE_SKILL_REGISTRY, limits);
    };
    let mut out = ExtractionSummary::default();
    let mut limiter = limits.map(ExtractionCollectionLimiter::new);
    let mut seen = HashSet::new();
    let id_cap = if limits.is_some() { 256 } else { 65_536 };
    let mut files = OpenCodeFiles::new(doc.get("info"), messages, &mut out);
    let native = messages.iter().any(|message| message.get("type").is_some());
    let classic = messages.iter().any(|message| message.get("info").is_some());
    if (native || classic) && !doc.get("info").is_some_and(Value::is_object) {
        opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
    }
    if native && classic {
        opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
    }
    for (ordinal, message) in messages.iter().enumerate() {
        if collection_exhausted(&limiter) {
            break;
        }
        if let Some(kind) = message.get("type").and_then(Value::as_str) {
            if opencode_seen(
                &mut seen,
                message.get("id").and_then(Value::as_str),
                id_cap,
                &mut out,
            ) {
                continue;
            }
            match kind {
                "user" => {
                    if let Some(text) = message.get("text").and_then(Value::as_str) {
                        opencode_prompt(text, ordinal, &mut out, &mut limiter);
                    } else {
                        opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
                    }
                }
                "assistant" => {
                    if out.model.is_none() {
                        if let Some(model) = message
                            .get("model")
                            .and_then(|model| model.get("id"))
                            .and_then(Value::as_str)
                        {
                            set_model(&mut out, &mut limiter, model);
                        } else {
                            opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
                        }
                    }
                    if let Some(tokens) = message.get("tokens") {
                        opencode_add_usage(&mut out, tokens);
                    }
                    if let Some(content) = message.get("content").and_then(Value::as_array) {
                        for part in content {
                            if part.get("type").and_then(Value::as_str) == Some("tool") {
                                opencode_tool(part, &mut files, false, &mut out, &mut limiter);
                            }
                            if collection_exhausted(&limiter) {
                                break;
                            }
                        }
                    } else {
                        opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
                    }
                    if let Some(snapshot) = message
                        .get("snapshot")
                        .and_then(|snapshot| snapshot.get("files"))
                    {
                        files.list(snapshot, true, &mut out, &mut limiter);
                    }
                }
                "location-switched" => files.switch(message, &mut out),
                // Coverage/status semantics are owned by OG-09, not this projection.
                "agent-switched" | "model-switched" | "synthetic" | "system" | "skill"
                | "shell" | "compaction" | "idle" => (),
                _ => opencode_partial(&mut out, OPENCODE_SHAPE_WARNING),
            }
        } else if let Some(info) = message.get("info").filter(|info| info.is_object()) {
            if opencode_seen(
                &mut seen,
                info.get("id").and_then(Value::as_str),
                id_cap,
                &mut out,
            ) {
                continue;
            }
            let Some(parts) = message.get("parts").and_then(Value::as_array) else {
                opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
                continue;
            };
            match info.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let mut text = String::new();
                    for part in parts {
                        if part.get("type").and_then(Value::as_str) != Some("text")
                            || part.get("synthetic").and_then(Value::as_bool) == Some(true)
                            || part.get("ignored").and_then(Value::as_bool) == Some(true)
                        {
                            continue;
                        }
                        let Some(next) = part.get("text").and_then(Value::as_str) else {
                            opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
                            continue;
                        };
                        let next_len = text
                            .len()
                            .checked_add(next.len())
                            .and_then(|len| len.checked_add(usize::from(!text.is_empty())));
                        if next_len.is_none_or(|len| {
                            limiter
                                .as_ref()
                                .is_some_and(|limit| len > limit.max_string_bytes())
                        }) {
                            mark_collection_limit(&mut out, &mut limiter);
                            break;
                        }
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(next);
                    }
                    if !text.is_empty() {
                        opencode_prompt(&text, ordinal, &mut out, &mut limiter);
                    }
                }
                Some("assistant") => {
                    if out.model.is_none()
                        && let Some(model) = info
                            .get("modelID")
                            .or_else(|| {
                                info.get("model").and_then(|model| {
                                    model.get("modelID").or_else(|| model.get("id"))
                                })
                            })
                            .and_then(Value::as_str)
                    {
                        set_model(&mut out, &mut limiter, model);
                    }
                    let aggregate = info.get("tokens");
                    if let Some(tokens) = aggregate {
                        opencode_add_usage(&mut out, tokens);
                    }
                    let mut parts_seen = HashSet::new();
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("step-finish") if aggregate.is_none() => {
                                if opencode_seen(
                                    &mut parts_seen,
                                    part.get("id").and_then(Value::as_str),
                                    id_cap,
                                    &mut out,
                                ) {
                                    continue;
                                }
                                if let Some(tokens) = part.get("tokens") {
                                    opencode_add_usage(&mut out, tokens);
                                } else {
                                    opencode_partial(&mut out, OPENCODE_TOKEN_WARNING);
                                }
                            }
                            Some("tool") => {
                                opencode_tool(part, &mut files, true, &mut out, &mut limiter)
                            }
                            Some("patch") => {
                                if let Some(paths) = part.get("files") {
                                    files.list(paths, true, &mut out, &mut limiter);
                                } else {
                                    opencode_partial(&mut out, OPENCODE_FILE_WARNING);
                                }
                            }
                            _ => (),
                        }
                        if collection_exhausted(&limiter) {
                            break;
                        }
                    }
                }
                _ => opencode_partial(&mut out, OPENCODE_SHAPE_WARNING),
            }
        } else if !native && !classic {
            ingest_generic_record(
                message,
                ordinal,
                "opencode",
                OPENCODE_SKILL_REGISTRY,
                &mut out,
                &mut limiter,
            );
        } else {
            opencode_partial(&mut out, OPENCODE_SHAPE_WARNING);
        }
    }
    out
}

fn extract_generic_jsonl(
    data: &[u8],
    slug: &str,
    registry: &[&str],
    limits: Option<ExtractionCollectionLimits>,
) -> ExtractionSummary {
    let mut out = ExtractionSummary::default();
    let mut limiter = limits.map(ExtractionCollectionLimiter::new);
    let mut undecodable = 0usize;
    for (line_no, line) in data.split(|b| *b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            undecodable += 1;
            continue;
        };
        ingest_generic_record(&entry, line_no, slug, registry, &mut out, &mut limiter);
        if collection_exhausted(&limiter) {
            break;
        }
    }
    if undecodable > 0 {
        out.partial = true;
        out.warnings.push(format!(
            "{undecodable} transcript line(s) were not valid JSON"
        ));
    }
    out
}

/// Shared heuristics for codex/opencode records: user prompts, model ids,
/// usage objects (native or E6-shaped), curated skill commands.
fn ingest_generic_record(
    entry: &Value,
    ordinal: usize,
    slug: &str,
    registry: &[&str],
    out: &mut ExtractionSummary,
    limiter: &mut Option<ExtractionCollectionLimiter>,
) {
    if slug == "opencode"
        && (entry.get("type").is_some()
            || entry.get("info").is_some()
            || entry.get("messages").is_some())
    {
        opencode_partial(out, OPENCODE_SHAPE_WARNING);
        return;
    }
    let record = entry.get("message").unwrap_or(entry);
    let role = record
        .get("role")
        .or_else(|| entry.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if role == "user"
        && let Some(content) = record
            .get("content")
            .or_else(|| record.get("text"))
            .or_else(|| entry.get("content"))
    {
        let max_string_bytes = limiter
            .as_ref()
            .map(ExtractionCollectionLimiter::max_string_bytes);
        let text = match content_text(content, max_string_bytes) {
            ContentText::Empty => None,
            ContentText::Text(text) => Some(text),
            ContentText::TooLarge => {
                mark_collection_limit(out, limiter);
                None
            }
        };
        let text = text.and_then(|text| {
            if slug != "opencode" {
                return Some(text);
            }
            let filtered = strip_injection_prefixes(&text);
            if filtered.malformed {
                opencode_partial(out, OPENCODE_SHAPE_WARNING);
            }
            if filtered.removed && filtered.text.trim().is_empty() {
                None
            } else {
                Some(filtered.text.into_owned())
            }
        });
        if let Some(text) = text {
            if let Some((skill, signal)) = match_skill(&text, registry) {
                let timestamp = entry.get("timestamp").and_then(Value::as_str).unwrap_or("");
                push_skill_event(
                    out,
                    limiter,
                    SkillEventInput {
                        agent_slug: slug,
                        skill,
                        signal,
                        turn_id: &format!("record-{ordinal}"),
                        timestamp,
                        anchor: Some(format!("record:{ordinal}")),
                        native: false,
                    },
                );
            }
            push_prompt(out, limiter, text);
        }
    }
    if out.model.is_none()
        && !(slug == "opencode" && role == "user")
        && let Some(model) = record
            .get("model")
            .or_else(|| entry.get("model"))
            .and_then(Value::as_str)
    {
        set_model(out, limiter, model);
    }
    if let Some(usage) = record
        .get("usage")
        .or_else(|| entry.get("usage"))
        .or_else(|| entry.get("token_usage"))
        && usage.is_object()
    {
        // Consume ALL six E6 wire keys — the count/subagent fields are
        // additive rather than dropped (agent.md E6).
        if slug == "opencode"
            && (usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .checked_add(
                    usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                )
                .is_none()
                || usage
                    .get("cache_creation_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .checked_add(
                        usage
                            .get("cache_read_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                    )
                    .is_none())
        {
            opencode_partial(out, OPENCODE_TOKEN_WARNING);
            return;
        }
        let e6 = map_e6_token_usage_full(usage);
        if e6.summary.input_tokens > 0 || e6.summary.output_tokens > 0 {
            merge_usage(&mut out.usage, &e6.summary);
        }
        // `api_call_count` is taken from the wire, else one per usage object.
        let calls = if e6.api_call_count > 0 {
            e6.api_call_count
        } else {
            1
        };
        if slug == "opencode" {
            if let Some(count) = out.api_call_count.checked_add(calls) {
                out.api_call_count = count;
            } else {
                opencode_partial(out, OPENCODE_TOKEN_WARNING);
            }
        } else {
            out.api_call_count += calls;
        }
        if e6.subagent_tokens > 0 {
            let subagent = CompletionUsageSummary {
                input_tokens: e6.subagent_tokens,
                total_tokens: Some(e6.subagent_tokens),
                ..CompletionUsageSummary::default()
            };
            merge_usage(&mut out.subagent_usage, &subagent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e6_mapping_uses_frozen_wire_keys() {
        let value = serde_json::json!({
            "input_tokens": 100,
            "cache_creation_tokens": 10,
            "cache_read_tokens": 5,
            "output_tokens": 50,
            "api_call_count": 3,
            "subagent_tokens": 20,
        });
        let full = map_e6_token_usage_full(&value);
        assert_eq!(full.summary.input_tokens, 100);
        assert_eq!(full.summary.output_tokens, 50);
        assert_eq!(full.summary.cached_tokens, Some(15));
        assert_eq!(full.summary.total_tokens, Some(150));
        assert_eq!(full.summary.reasoning_tokens, None);
        assert_eq!(full.summary.cost_usd, None);
        // All SIX frozen keys consumed — count + subagent are not dropped.
        assert_eq!(full.api_call_count, 3);
        assert_eq!(full.subagent_tokens, 20);
        // The summary-only convenience wrapper agrees.
        assert_eq!(map_e6_token_usage(&value), full.summary);
    }

    #[test]
    fn e6_mapping_absent_cache_keys_yield_none() {
        let summary = map_e6_token_usage(&serde_json::json!({
            "input_tokens": 1, "output_tokens": 2,
        }));
        assert_eq!(summary.cached_tokens, None);
    }

    #[test]
    fn claude_extraction_projects_all_dimensions() {
        let jsonl = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-07-05T00:00:00Z","message":{"role":"user","content":"/review please check this"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","message":{"role":"assistant","model":"claude-sonnet-5","content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Write","input":{"file_path":"src/main.rs"}},{"type":"tool_use","name":"Task","input":{"prompt":"sub"}}],"usage":{"input_tokens":10,"output_tokens":4,"cache_read_input_tokens":6}}}"#,
            "\n",
            r#"not-json"#,
            "\n",
        );
        let out = extract_claude_code(jsonl.as_bytes());
        assert_eq!(out.prompts.len(), 1);
        assert_eq!(out.model.as_deref(), Some("claude-sonnet-5"));
        let usage = out.usage.expect("usage summed");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cached_tokens, Some(6));
        assert_eq!(out.api_call_count, 1);
        assert_eq!(out.modified_files, ["src/main.rs"]);
        assert_eq!(
            out.subagent_usage, None,
            "a Task marker alone must not relabel the parent session total"
        );
        assert_eq!(out.skill_events.len(), 1);
        assert_eq!(out.skill_events[0].skill.name, "/review");
        assert!(out.partial, "undecodable line + Task approximation");
        assert!(!out.warnings.is_empty());
    }

    #[test]
    fn generic_extraction_handles_codex_and_opencode_shapes() {
        let codex = concat!(
            r#"{"role":"user","content":"/review the diff"}"#,
            "\n",
            r#"{"model":"gpt-5-codex","usage":{"input_tokens":7,"output_tokens":3}}"#,
            "\n",
        );
        let out = extract_codex(codex.as_bytes());
        assert_eq!(out.prompts, ["/review the diff"]);
        assert_eq!(out.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(out.usage.as_ref().unwrap().total_tokens, Some(10));
        assert_eq!(out.skill_events.len(), 1);

        let opencode = serde_json::json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "model": "claude-sonnet-5", "content": "hi"},
            ]
        })
        .to_string();
        let out2 = extract_opencode(opencode.as_bytes());
        assert_eq!(out2.prompts, ["hello"]);
        assert_eq!(out2.model.as_deref(), Some("claude-sonnet-5"));
        assert!(!out2.partial);
    }

    #[test]
    fn opencode_bounded_native_collections_and_internal_state_are_partial() {
        let messages: Vec<Value> = (0..400)
            .map(|index| {
                serde_json::json!({
                    "id": format!("m-{index}"), "type":"user", "text": "synthetic"
                })
            })
            .collect();
        let doc = serde_json::json!({"info":{"id":"s","location":{"directory":"/project"}},"messages": messages});
        let bytes = serde_json::to_vec(&doc).unwrap();
        let result =
            extract_opencode_bounded(&bytes, ExtractionCollectionLimits::new(128, 4096, 32768));
        assert!(result.partial);
        assert_eq!(result.prompts.len(), 128);
        assert_eq!(result.warnings, [COLLECTION_LIMIT_WARNING]);
        let messages: Vec<Value> = (0..400).map(|index| serde_json::json!({
            "id":format!("m-{index}"),"type":"assistant","model":{"id":"synthetic"},"content":[]
        })).collect();
        let bytes = serde_json::to_vec(&serde_json::json!({"info":{"id":"s","location":{"directory":"/project"}},"messages":messages}))
            .unwrap();
        let result =
            extract_opencode_bounded(&bytes, ExtractionCollectionLimits::new(128, 4096, 32768));
        assert!(result.partial);
        assert_eq!(result.warnings, [OPENCODE_STATE_WARNING]);
        let bytes = serde_json::to_vec(
            &serde_json::json!({"info":{"id":"s","location":{"directory":"/project"}},"messages":[
                {"id":"u","type":"user","text":"x".repeat(4097)}
            ]}),
        )
        .unwrap();
        let result =
            extract_opencode_bounded(&bytes, ExtractionCollectionLimits::new(128, 4096, 32768));
        assert!(result.prompts.is_empty());
        assert_eq!(result.warnings, [COLLECTION_LIMIT_WARNING]);
    }

    #[test]
    fn opencode_patch_grammar_rejects_empty_or_malformed_updates() {
        for patch in [
            "*** Begin Patch\n*** Update File: a\n*** End Patch",
            "*** Begin Patch\n*** Update File: a\n@@\n*** End Patch",
            "*** Begin Patch\n*** Delete File: a\n-unexpected\n*** End Patch",
        ] {
            assert!(opencode_patch_paths(patch).is_none(), "{patch}");
        }
        let patch = "<<'PATCH'\r\n*** Begin Patch\r\n*** Add File: a\r\n+new\r\n*** Delete File: b\r\n*** End Patch\r\nPATCH";
        assert_eq!(opencode_patch_paths(patch).unwrap(), ["a", "b"]);
        let cat = patch.replacen("<<'PATCH'", "cat <<'PATCH'", 1);
        assert_eq!(opencode_patch_paths(&cat).unwrap(), ["a", "b"]);
    }

    #[test]
    fn empty_or_garbage_input_is_partial_not_panic() {
        let out = extract_claude_code(b"");
        assert!(!out.partial && out.prompts.is_empty());
        let out2 = extract_claude_code(b"\x00\xff garbage\nmore garbage\n");
        assert!(out2.partial);
        assert!(out2.prompts.is_empty());
        let out3 = extract_opencode(b"{\"unexpected\": true}");
        assert!(out3.prompts.is_empty(), "non-array document yields empty");
    }
}
