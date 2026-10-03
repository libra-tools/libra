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
//! - OpenCode: JSON session exports with `parts`/`messages` arrays; the
//!   same generic heuristics apply.

use std::collections::HashSet;

use serde_json::Value;

use super::capability::{SkillEvent, SkillEventSignal, SkillEventSource, SkillEventType, SkillRef};
use crate::internal::ai::completion::CompletionUsageSummary;

/// E7 curated skill registries (agent.md). OpenCode's upstream has no
/// verified slash-command surface distinct from its `/review`-style input
/// commands, so it shares the single-entry registry until upstream
/// evidence says otherwise.
pub const CLAUDE_CODE_SKILL_REGISTRY: &[&str] = &["/review", "/security-review", "/simplify"];
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

/// OpenCode session export → prompts / model / skills. Accepts either
/// JSONL or a single JSON document with a `messages`/`parts` array.
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

fn extract_opencode_with_limits(
    data: &[u8],
    limits: Option<ExtractionCollectionLimits>,
) -> ExtractionSummary {
    // Whole-document form first.
    if let Ok(doc) = serde_json::from_slice::<Value>(data)
        && let Some(messages) = doc
            .get("messages")
            .or_else(|| doc.get("parts"))
            .and_then(Value::as_array)
    {
        let mut out = ExtractionSummary::default();
        let mut limiter = limits.map(ExtractionCollectionLimiter::new);
        for (idx, message) in messages.iter().enumerate() {
            ingest_generic_record(
                message,
                idx,
                "opencode",
                OPENCODE_SKILL_REGISTRY,
                &mut out,
                &mut limiter,
            );
            if collection_exhausted(&limiter) {
                break;
            }
        }
        return out;
    }
    extract_generic_jsonl(data, "opencode", OPENCODE_SKILL_REGISTRY, limits)
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
        let e6 = map_e6_token_usage_full(usage);
        if e6.summary.input_tokens > 0 || e6.summary.output_tokens > 0 {
            merge_usage(&mut out.usage, &e6.summary);
        }
        // `api_call_count` is taken from the wire when present, else one
        // per usage object (each usage object is one API call).
        out.api_call_count += if e6.api_call_count > 0 {
            e6.api_call_count
        } else {
            1
        };
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
