//! Deadline-safe, redacted-only extraction projection for Agent Capture.
//!
//! The hook runtime hands this module only [`RedactedBytes`]. For an absolute
//! SessionEnd deadline the expensive parser/extractor chain runs in the
//! registered private helper and returns a bounded metadata object; the hook
//! parent never falls back to parsing a transcript after that helper fails.

use std::time::Instant;

use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::internal::ai::{
    authorized_read::{
        RegisteredHelperOutput, registered_helper_command, run_registered_bounded_helper_until,
    },
    observed_agents::{AgentKind, RedactedBytes, Redactor, agent_for},
};

/// Fixed private argv accepted by the main binary before normal CLI startup.
pub const CAPTURE_EXTRACTION_HELPER_ARG: &str = "--libra-internal-capture-extraction-helper";
/// A version/code/flags/length header followed by at most one redacted parent
/// transcript. Child native sources are intentionally omitted on the deadline
/// path rather than synchronously redacted in the hook parent.
pub const CAPTURE_EXTRACTION_HELPER_INPUT_CAP: u64 =
    CAPTURE_EXTRACTION_HELPER_PARENT_CAP + EXTRACTION_REQUEST_HEADER_BYTES as u64;
/// Extraction is metadata, not a second transcript channel. Keep its private
/// frame much smaller than the source cap and fail closed if a parser attempts
/// to emit an unbounded path/event list.
pub const CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP: u64 = 1024 * 1024;

const EXTRACTION_REQUEST_VERSION: u8 = 1;
const EXTRACTION_REQUEST_HEADER_BYTES: usize = 1 + 1 + 1 + 8;
const EXTRACTION_REQUEST_PARENT_PRESENT: u8 = 1;
const EXTRACTION_REQUEST_CHILDREN_OMITTED: u8 = 1 << 1;
// The live source helper bounds redacted output at 1.5x the 16 MiB raw
// transcript cap. Preserve that complete redacted source for extraction;
// the slice-parts transport below avoids a second parent-side copy.
const CAPTURE_EXTRACTION_HELPER_PARENT_CAP: u64 = 24 * 1024 * 1024;
const EXTRACTION_RESPONSE_COMPLETE: u8 = 0;
const EXTRACTION_RESPONSE_FAILED: u8 = 1;
const EXTRACTION_RESPONSE_HEADER_BYTES: usize = 1 + 4 + 32;
// `CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP` caps the *whole* helper frame.
// Reserve its fixed framing bytes so a maximal valid JSON payload can never
// make the child write a frame the parent deliberately truncates.
const CAPTURE_EXTRACTION_HELPER_PAYLOAD_CAP: u64 =
    CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP - EXTRACTION_RESPONSE_HEADER_BYTES as u64;
const EXTRACTION_JSON_MAX_DEPTH: usize = 16;
const EXTRACTION_JSON_MAX_CONTAINER_ITEMS: usize = 4_096;
const EXTRACTION_JSON_MAX_STRING_BYTES: usize = 64 * 1024;
// The helper's metadata is intentionally much smaller than its 24 MiB
// redacted input. Bound every parser-side collection before it is converted
// into JSON: this leaves ample room for JSON escaping and redaction expansion
// inside the 1 MiB response frame while preserving normal small transcripts.
const DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION: usize = 128;
const DEADLINE_EXTRACTION_MAX_STRING_BYTES: usize = 4 * 1024;
const DEADLINE_EXTRACTION_MAX_RETAINED_STRING_BYTES: usize = 32 * 1024;

const CHILDREN_OMITTED_WARNING: &str = "deadline-bound extraction omitted child source aggregation";
const EXTRACTION_DEADLINE_WARNING: &str =
    "deadline-bound extraction did not complete before the capture window";
const EXTRACTION_FAILED_WARNING: &str =
    "deadline-bound extraction helper returned no safe metadata";

/// Typed outcome of the redacted-only extraction boundary.
pub(crate) enum DeadlineExtractionResult {
    Complete(Value),
    DeadlineExceeded,
    Failed,
}

/// Build the legacy in-process projection for callers without a capture
/// deadline. This remains intentionally synchronous so historical import and
/// ordinary tests preserve their established behavior.
pub(crate) fn build_extraction_projection(
    agent_kind: &str,
    redacted_parent: Option<&RedactedBytes>,
    redacted_subagents: &[RedactedBytes],
    subagent_snapshot_warnings: &[String],
) -> Value {
    let mut partial = false;
    let mut warnings: Vec<String> = Vec::new();
    for warning in subagent_snapshot_warnings {
        partial = true;
        warnings.push(warning.to_string());
    }
    let mut value = json!({
        "schema_version": 1,
        "present": false,
        "partial": false,
        "warnings": [],
    });

    let adapter = AgentKind::from_db_str(agent_kind).map(agent_for);
    let Some(adapter) = adapter else {
        partial = true;
        // Do not reflect an arbitrary provider label through a durable helper
        // frame or metadata warning.
        warnings.push("unknown agent kind; extraction skipped".to_string());
        finalize_extraction(&mut value, partial, warnings);
        return value;
    };
    let subagent_bytes = redacted_subagents
        .iter()
        .map(RedactedBytes::bytes)
        .collect::<Vec<_>>();
    let has_subagent_sources = !subagent_bytes.is_empty();
    let transcript = match redacted_parent {
        Some(redacted_parent) => redacted_parent.bytes(),
        None if !subagent_bytes.is_empty() && adapter.as_subagent_aware_extractor().is_some() => {
            partial = true;
            warnings.push(
                "no redacted parent transcript available; extraction derived from child sources only"
                    .to_string(),
            );
            &[]
        }
        None => {
            partial = true;
            warnings.push("no redacted transcript available; extraction skipped".to_string());
            finalize_extraction(&mut value, partial, warnings);
            return value;
        }
    };

    // INVARIANT: `value` is constructed by the object-form `json!` literal
    // above and is never reassigned before this point.
    let Some(object) = value.as_object_mut() else {
        return json!({
            "schema_version": 1,
            "present": false,
            "partial": true,
            "warnings": ["internal extraction metadata shape was not an object"],
        });
    };
    object.insert("present".into(), Value::Bool(true));

    if let Some(calculator) = adapter.as_token_calculator() {
        match calculator.calculate_token_usage(transcript, 0) {
            Ok(usage) => {
                insert_extraction_value(object, "token_usage", &usage, &mut partial, &mut warnings);
            }
            Err(_) => {
                partial = true;
                warnings.push("token usage extraction failed".to_string());
            }
        }
    }
    if let Some(extractor) = adapter.as_model_extractor() {
        match extractor.extract_model(transcript) {
            Ok(Some(model)) => {
                object.insert(
                    "model".into(),
                    Value::String(redact_extracted_string(&model)),
                );
            }
            Ok(None) => {}
            Err(_) => {
                partial = true;
                warnings.push("model extraction failed".to_string());
            }
        }
    }
    if let Some(extractor) = adapter.as_prompt_extractor() {
        match extractor.extract_prompts(transcript, 0) {
            Ok(prompts) => {
                object.insert("prompt_count".into(), json!(prompts.len()));
            }
            Err(_) => {
                partial = true;
                warnings.push("prompt extraction failed".to_string());
            }
        }
    }
    if let Some(extractor) = adapter.as_subagent_aware_extractor() {
        match extractor.extract_parent_and_subagents(transcript, &subagent_bytes) {
            Ok(aggregate) => {
                insert_extraction_value(
                    object,
                    "aggregate_token_usage",
                    &aggregate.aggregate_usage,
                    &mut partial,
                    &mut warnings,
                );
                object.insert("subagent_source_count".into(), json!(subagent_bytes.len()));
                if let Some(usage) = aggregate.subagent_usage {
                    insert_extraction_value(
                        object,
                        "subagent_token_usage",
                        &usage,
                        &mut partial,
                        &mut warnings,
                    );
                }
                let list: Vec<String> = aggregate
                    .modified_files
                    .iter()
                    .map(|path| redact_extracted_string(&path.display().to_string()))
                    .collect();
                insert_extraction_value(
                    object,
                    "modified_files",
                    &list,
                    &mut partial,
                    &mut warnings,
                );
                partial |= aggregate.partial;
                // Built-in aggregate extractors include parent-parser warnings
                // even when there are no child sources. Those are reported by
                // the ordinary provider summary below; labeling them as a
                // subagent aggregate warning would be misleading.
                if has_subagent_sources && !aggregate.warnings.is_empty() {
                    warnings.push("subagent aggregate extraction reported warnings".to_string());
                }
            }
            Err(_) => {
                partial = true;
                warnings.push("subagent aggregate extraction failed".to_string());
            }
        }
    }
    if !object.contains_key("modified_files")
        && let Some(analyzer) = adapter.as_transcript_analyzer()
    {
        match analyzer.extract_modified_files_from_offset(transcript, 0) {
            Ok(files) => {
                let list: Vec<String> = files
                    .iter()
                    .map(|path| redact_extracted_string(&path.display().to_string()))
                    .collect();
                insert_extraction_value(
                    object,
                    "modified_files",
                    &list,
                    &mut partial,
                    &mut warnings,
                );
            }
            Err(_) => {
                partial = true;
                warnings.push("modified-files extraction failed".to_string());
            }
        }
    }
    if let Some(extractor) = adapter.as_skill_event_extractor() {
        match extractor.extract_skill_events(transcript, 0) {
            Ok(events) => {
                if let Some(value) =
                    serialize_extraction_value(&events, &mut partial, &mut warnings)
                {
                    object.insert("skill_events".into(), redact_extracted_json(value));
                }
            }
            Err(_) => {
                partial = true;
                warnings.push("skill event extraction failed".to_string());
            }
        }
    }

    let format_summary = match AgentKind::from_db_str(agent_kind) {
        Some(AgentKind::ClaudeCode) => {
            Some(crate::internal::ai::observed_agents::extract::extract_claude_code(transcript))
        }
        Some(AgentKind::Codex) => {
            Some(crate::internal::ai::observed_agents::extract::extract_codex(transcript))
        }
        Some(AgentKind::OpenCode) => {
            Some(crate::internal::ai::observed_agents::extract::extract_opencode(transcript))
        }
        _ => None,
    };
    if let Some(summary) = format_summary {
        if !object.contains_key("modified_files") && !summary.modified_files.is_empty() {
            let files: Vec<String> = summary
                .modified_files
                .iter()
                .map(|path| redact_extracted_string(path))
                .collect();
            insert_extraction_value(
                object,
                "modified_files",
                &files,
                &mut partial,
                &mut warnings,
            );
        }
        if summary.partial {
            partial = true;
        }
        object.insert("api_call_count".into(), json!(summary.api_call_count));
        if !object.contains_key("subagent_token_usage")
            && let Some(subagent) = &summary.subagent_usage
        {
            insert_extraction_value(
                object,
                "subagent_token_usage",
                subagent,
                &mut partial,
                &mut warnings,
            );
        }
        if !summary.warnings.is_empty() {
            warnings.push("transcript parser reported partial output".to_string());
        }
    }

    finalize_extraction(&mut value, partial, warnings);
    value
}

/// Build the deadline-worker projection from exactly one provider parser
/// pass. The ordinary no-deadline path intentionally retains its historical
/// adapter-by-capability behavior above; this path trades exhaustive child
/// aggregation for a bounded, cancelable parent summary.
fn build_deadline_extraction_projection(
    agent_kind: AgentKind,
    redacted_parent: Option<&RedactedBytes>,
    children_omitted: bool,
) -> Value {
    let mut partial = children_omitted;
    let mut warnings = Vec::new();
    if children_omitted {
        warnings.push(CHILDREN_OMITTED_WARNING.to_string());
    }
    let mut value = json!({
        "schema_version": 1,
        "present": false,
        "partial": false,
        "warnings": [],
    });
    let Some(redacted_parent) = redacted_parent else {
        partial = true;
        warnings.push("no redacted transcript available; extraction skipped".to_string());
        finalize_extraction(&mut value, partial, warnings);
        return value;
    };

    // INVARIANT: the object-form literal above is never reassigned.
    let Some(object) = value.as_object_mut() else {
        return deadline_extraction_partial(false);
    };
    object.insert("present".into(), Value::Bool(true));

    // This is deliberately the only provider parser invocation for a
    // deadline request. It consumes the complete, bounded redacted source
    // passed through the private helper frame.
    let summary = extract_deadline_provider_once(agent_kind, redacted_parent.bytes());
    if summary.partial {
        partial = true;
    }
    if !summary.warnings.is_empty() {
        warnings.push("transcript parser reported partial output".to_string());
    }
    if let Some(usage) = &summary.usage {
        insert_extraction_value(object, "token_usage", usage, &mut partial, &mut warnings);
    }
    if let Some(model) = &summary.model {
        object.insert(
            "model".into(),
            Value::String(redact_extracted_string(model)),
        );
    }
    object.insert("prompt_count".into(), json!(summary.prompts.len()));
    object.insert("api_call_count".into(), json!(summary.api_call_count));
    if let Some(usage) = &summary.subagent_usage {
        insert_extraction_value(
            object,
            "subagent_token_usage",
            usage,
            &mut partial,
            &mut warnings,
        );
    }
    let files: Vec<String> = summary
        .modified_files
        .iter()
        .map(|path| redact_extracted_string(path))
        .collect();
    if !files.is_empty() {
        insert_extraction_value(
            object,
            "modified_files",
            &files,
            &mut partial,
            &mut warnings,
        );
    }
    if !summary.skill_events.is_empty()
        && let Some(events) =
            serialize_extraction_value(&summary.skill_events, &mut partial, &mut warnings)
    {
        object.insert("skill_events".into(), redact_extracted_json(events));
    }

    finalize_extraction(&mut value, partial, warnings);
    value
}

fn extract_deadline_provider_once(
    agent_kind: AgentKind,
    transcript: &[u8],
) -> crate::internal::ai::observed_agents::extract::ExtractionSummary {
    let limits = crate::internal::ai::observed_agents::extract::ExtractionCollectionLimits::new(
        DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION,
        DEADLINE_EXTRACTION_MAX_STRING_BYTES,
        DEADLINE_EXTRACTION_MAX_RETAINED_STRING_BYTES,
    );
    match agent_kind {
        AgentKind::ClaudeCode => {
            crate::internal::ai::observed_agents::extract::extract_claude_code_bounded(
                transcript, limits,
            )
        }
        AgentKind::Codex => {
            crate::internal::ai::observed_agents::extract::extract_codex_bounded(transcript, limits)
        }
        AgentKind::OpenCode => {
            crate::internal::ai::observed_agents::extract::extract_opencode_bounded(
                transcript, limits,
            )
        }
        // `decode_extraction_request` accepts only the first-batch provider
        // codes, so this arm is defensive and cannot carry an error string.
        AgentKind::Gemini | AgentKind::Cursor | AgentKind::Copilot | AgentKind::FactoryAi => {
            crate::internal::ai::observed_agents::extract::ExtractionSummary {
                partial: true,
                ..Default::default()
            }
        }
    }
}

/// Execute parent extraction in the registered private helper. The request
/// has only redacted parent bytes and fixed enum/flag fields; child raw bytes,
/// prompts, paths, and errors never cross this frame.
pub(crate) async fn build_extraction_projection_until(
    agent_kind: &str,
    redacted_parent: Option<&RedactedBytes>,
    children_omitted: bool,
    deadline: Instant,
) -> DeadlineExtractionResult {
    if Instant::now() >= deadline {
        return DeadlineExtractionResult::DeadlineExceeded;
    }
    let parent_bytes = redacted_parent.map_or(&[][..], RedactedBytes::bytes);
    let Some(request_header) = encode_extraction_request_header(
        agent_kind,
        redacted_parent.is_some(),
        parent_bytes.len(),
        children_omitted,
    ) else {
        return DeadlineExtractionResult::Failed;
    };
    let Some(command) = registered_helper_command(CAPTURE_EXTRACTION_HELPER_ARG) else {
        return DeadlineExtractionResult::Failed;
    };
    match run_registered_bounded_helper_until(
        command,
        &[request_header.as_slice(), parent_bytes],
        CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP,
        deadline,
    )
    .await
    {
        RegisteredHelperOutput::Output(frame) => decode_extraction_response(frame),
        RegisteredHelperOutput::DeadlineExceeded => DeadlineExtractionResult::DeadlineExceeded,
        RegisteredHelperOutput::Failed => DeadlineExtractionResult::Failed,
    }
}

/// A content-free partial used when a deadline worker cannot complete. The
/// parent must use this rather than parsing/redacting transcript bytes itself.
pub(crate) fn deadline_extraction_partial(deadline_exceeded: bool) -> Value {
    json!({
        "schema_version": 1,
        "present": false,
        "partial": true,
        "warnings": [if deadline_exceeded {
            EXTRACTION_DEADLINE_WARNING
        } else {
            EXTRACTION_FAILED_WARNING
        }],
    })
}

/// Entry used by the main binary before normal CLI initialization. It returns
/// only a fixed status frame; malformed input or any extraction failure never
/// serializes an error string back to the deadline-owning parent.
#[doc(hidden)]
pub fn run_capture_extraction_helper(input: Vec<u8>) -> Vec<u8> {
    let Some((agent_kind, parent, children_omitted)) = decode_extraction_request(input) else {
        return vec![EXTRACTION_RESPONSE_FAILED];
    };
    let metadata =
        build_deadline_extraction_projection(agent_kind, parent.as_ref(), children_omitted);
    let Ok(payload) = serde_json::to_vec(&metadata) else {
        return vec![EXTRACTION_RESPONSE_FAILED];
    };
    if u64::try_from(payload.len())
        .ok()
        .is_none_or(|length| length > CAPTURE_EXTRACTION_HELPER_PAYLOAD_CAP)
    {
        return vec![EXTRACTION_RESPONSE_FAILED];
    }
    let digest: [u8; 32] = Sha256::digest(&payload).into();
    let Ok(payload_len) = u32::try_from(payload.len()) else {
        return vec![EXTRACTION_RESPONSE_FAILED];
    };
    let mut frame = Vec::with_capacity(EXTRACTION_RESPONSE_HEADER_BYTES + payload.len());
    frame.push(EXTRACTION_RESPONSE_COMPLETE);
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(&digest);
    frame.extend_from_slice(&payload);
    frame
}

fn encode_extraction_request_header(
    agent_kind: &str,
    parent_present: bool,
    parent_len: usize,
    children_omitted: bool,
) -> Option<Vec<u8>> {
    let agent_code = extraction_agent_code(AgentKind::from_db_str(agent_kind)?)?;
    if u64::try_from(parent_len).ok()? > CAPTURE_EXTRACTION_HELPER_PARENT_CAP {
        return None;
    }
    let mut flags = 0_u8;
    if parent_present {
        flags |= EXTRACTION_REQUEST_PARENT_PRESENT;
    }
    if children_omitted {
        flags |= EXTRACTION_REQUEST_CHILDREN_OMITTED;
    }
    let parent_len = u64::try_from(parent_len).ok()?;
    let mut request = Vec::with_capacity(EXTRACTION_REQUEST_HEADER_BYTES);
    request.push(EXTRACTION_REQUEST_VERSION);
    request.push(agent_code);
    request.push(flags);
    request.extend_from_slice(&parent_len.to_le_bytes());
    Some(request)
}

#[cfg(test)]
fn encode_extraction_request(
    agent_kind: &str,
    parent: Option<&RedactedBytes>,
    children_omitted: bool,
) -> Option<Vec<u8>> {
    let parent_bytes = parent.map_or(&[][..], RedactedBytes::bytes);
    let mut request = encode_extraction_request_header(
        agent_kind,
        parent.is_some(),
        parent_bytes.len(),
        children_omitted,
    )?;
    request.extend_from_slice(parent_bytes);
    Some(request)
}

fn decode_extraction_request(
    mut input: Vec<u8>,
) -> Option<(AgentKind, Option<RedactedBytes>, bool)> {
    if input.len() < EXTRACTION_REQUEST_HEADER_BYTES
        || u64::try_from(input.len()).ok()? > CAPTURE_EXTRACTION_HELPER_INPUT_CAP
        || input[0] != EXTRACTION_REQUEST_VERSION
    {
        return None;
    }
    let agent_kind = extraction_agent_kind(input[1])?;
    let flags = input[2];
    if flags & !(EXTRACTION_REQUEST_PARENT_PRESENT | EXTRACTION_REQUEST_CHILDREN_OMITTED) != 0 {
        return None;
    }
    let parent_len = <[u8; 8]>::try_from(&input[3..EXTRACTION_REQUEST_HEADER_BYTES])
        .ok()
        .map(u64::from_le_bytes)
        .and_then(|length| usize::try_from(length).ok())?;
    let parent_len_from_frame = input.len().checked_sub(EXTRACTION_REQUEST_HEADER_BYTES)?;
    if parent_len_from_frame != parent_len
        || u64::try_from(parent_len).ok()? > CAPTURE_EXTRACTION_HELPER_PARENT_CAP
        || (flags & EXTRACTION_REQUEST_PARENT_PRESENT == 0 && parent_len != 0)
    {
        return None;
    }
    // The request's owning buffer already contains exactly the redacted
    // parent bytes after its tiny header. Compact it in place instead of
    // cloning a second full redacted transcript while the helper parses it.
    let parent = if flags & EXTRACTION_REQUEST_PARENT_PRESENT != 0 {
        input.copy_within(EXTRACTION_REQUEST_HEADER_BYTES.., 0);
        input.truncate(parent_len);
        Some(RedactedBytes::new_unchecked(input))
    } else {
        None
    };
    Some((
        agent_kind,
        parent,
        flags & EXTRACTION_REQUEST_CHILDREN_OMITTED != 0,
    ))
}

fn decode_extraction_response(frame: Vec<u8>) -> DeadlineExtractionResult {
    if frame == [EXTRACTION_RESPONSE_FAILED] {
        return DeadlineExtractionResult::Failed;
    }
    if frame.len() < EXTRACTION_RESPONSE_HEADER_BYTES || frame[0] != EXTRACTION_RESPONSE_COMPLETE {
        return DeadlineExtractionResult::Failed;
    }
    let Some(payload_len) = <[u8; 4]>::try_from(&frame[1..5])
        .ok()
        .and_then(|length| usize::try_from(u32::from_le_bytes(length)).ok())
    else {
        return DeadlineExtractionResult::Failed;
    };
    let Some(expected_len) = EXTRACTION_RESPONSE_HEADER_BYTES.checked_add(payload_len) else {
        return DeadlineExtractionResult::Failed;
    };
    if expected_len != frame.len()
        || u64::try_from(payload_len)
            .ok()
            .is_none_or(|length| length > CAPTURE_EXTRACTION_HELPER_PAYLOAD_CAP)
    {
        return DeadlineExtractionResult::Failed;
    }
    let Ok(expected_digest) = <[u8; 32]>::try_from(&frame[5..EXTRACTION_RESPONSE_HEADER_BYTES])
    else {
        return DeadlineExtractionResult::Failed;
    };
    let payload = &frame[EXTRACTION_RESPONSE_HEADER_BYTES..];
    let actual_digest: [u8; 32] = Sha256::digest(payload).into();
    if actual_digest != expected_digest {
        return DeadlineExtractionResult::Failed;
    }
    let Ok(metadata) = serde_json::from_slice::<Value>(payload) else {
        return DeadlineExtractionResult::Failed;
    };
    if valid_extraction_metadata(&metadata) {
        DeadlineExtractionResult::Complete(metadata)
    } else {
        DeadlineExtractionResult::Failed
    }
}

fn valid_extraction_metadata(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    const TOP_LEVEL_KEYS: &[&str] = &[
        "schema_version",
        "present",
        "partial",
        "warnings",
        "token_usage",
        "model",
        "prompt_count",
        "aggregate_token_usage",
        "subagent_source_count",
        "subagent_token_usage",
        "modified_files",
        "skill_events",
        "api_call_count",
    ];
    if object
        .keys()
        .any(|key| !TOP_LEVEL_KEYS.contains(&key.as_str()))
    {
        return false;
    }
    let mut budget = usize::try_from(CAPTURE_EXTRACTION_HELPER_PAYLOAD_CAP).unwrap_or(usize::MAX);
    valid_extraction_value(value, 0, &mut budget)
}

fn valid_extraction_value(value: &Value, depth: usize, string_budget: &mut usize) -> bool {
    if depth > EXTRACTION_JSON_MAX_DEPTH {
        return false;
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(string) => {
            if string.len() > EXTRACTION_JSON_MAX_STRING_BYTES || string.len() > *string_budget {
                return false;
            }
            *string_budget -= string.len();
            true
        }
        Value::Array(values) => {
            values.len() <= EXTRACTION_JSON_MAX_CONTAINER_ITEMS
                && values
                    .iter()
                    .all(|value| valid_extraction_value(value, depth + 1, string_budget))
        }
        Value::Object(values) => {
            values.len() <= EXTRACTION_JSON_MAX_CONTAINER_ITEMS
                && values.iter().all(|(key, value)| {
                    key.len() <= 128 && key.len() <= *string_budget && {
                        *string_budget -= key.len();
                        valid_extraction_value(value, depth + 1, string_budget)
                    }
                })
        }
    }
}

fn extraction_agent_code(agent_kind: AgentKind) -> Option<u8> {
    match agent_kind {
        AgentKind::ClaudeCode => Some(1),
        AgentKind::Codex => Some(2),
        AgentKind::OpenCode => Some(3),
        _ => None,
    }
}

fn extraction_agent_kind(code: u8) -> Option<AgentKind> {
    match code {
        1 => Some(AgentKind::ClaudeCode),
        2 => Some(AgentKind::Codex),
        3 => Some(AgentKind::OpenCode),
        _ => None,
    }
}

fn redact_extracted_string(value: &str) -> String {
    let (bytes, _report) = Redactor::new_default().redact(value.as_bytes());
    String::from_utf8_lossy(bytes.as_ref()).into_owned()
}

fn redact_extracted_json(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_extracted_string(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_extracted_json).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (redact_extracted_string(&key), redact_extracted_json(value)))
                .collect(),
        ),
        other => other,
    }
}

fn insert_extraction_value<T: Serialize>(
    object: &mut Map<String, Value>,
    key: &str,
    value: &T,
    partial: &mut bool,
    warnings: &mut Vec<String>,
) {
    if let Some(value) = serialize_extraction_value(value, partial, warnings) {
        object.insert(key.to_string(), value);
    }
}

fn serialize_extraction_value<T: Serialize>(
    value: &T,
    partial: &mut bool,
    warnings: &mut Vec<String>,
) -> Option<Value> {
    match serde_json::to_value(value) {
        Ok(value) => Some(value),
        Err(_) => {
            *partial = true;
            warnings.push("extraction metadata serialization failed".to_string());
            None
        }
    }
}

fn finalize_extraction(value: &mut Value, partial: bool, warnings: Vec<String>) {
    let redactor = Redactor::new_default();
    let redacted: Vec<String> = warnings
        .into_iter()
        .map(|warning| {
            let (bytes, _report) = redactor.redact(warning.as_bytes());
            String::from_utf8_lossy(bytes.as_ref()).into_owned()
        })
        .collect();
    if let Some(object) = value.as_object_mut() {
        object.insert("partial".into(), Value::Bool(partial));
        object.insert(
            "warnings".into(),
            serde_json::to_value(redacted).unwrap_or_else(|_| json!([])),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_projection_reaches_ordinary_and_deadline_capture() {
        let parent = RedactedBytes::new_unchecked(
            include_bytes!("../../../../tests/fixtures/agent_transcripts/opencode.json").to_vec(),
        );
        let ordinary = build_extraction_projection("opencode", Some(&parent), &[], &[]);
        let deadline =
            build_deadline_extraction_projection(AgentKind::OpenCode, Some(&parent), false);
        for projection in [&ordinary, &deadline] {
            assert_eq!(projection["prompt_count"], 2);
            assert_eq!(projection["model"], "claude-sonnet-5");
            assert_eq!(projection["token_usage"]["reasoning_tokens"], 4);
            assert!(
                projection["modified_files"]
                    .as_array()
                    .is_some_and(|files| files.iter().any(|file| file == "src/lib.rs"))
            );
            assert_eq!(projection["partial"], false);
        }
        let broken = RedactedBytes::new_unchecked(
            br#"{"messages":[{"id":"a","type":"assistant","content":[] }]}"#.to_vec(),
        );
        assert_eq!(
            build_extraction_projection("opencode", Some(&broken), &[], &[])["partial"],
            true
        );
        assert_eq!(
            build_deadline_extraction_projection(AgentKind::OpenCode, Some(&broken), false)["partial"],
            true
        );
    }

    struct FailingSerialization;

    impl Serialize for FailingSerialization {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("private fixture error"))
        }
    }

    #[test]
    fn failed_metadata_serialization_is_partial_without_error_details() {
        let mut object = Map::new();
        let mut partial = false;
        let mut warnings = Vec::new();
        insert_extraction_value(
            &mut object,
            "usage",
            &FailingSerialization,
            &mut partial,
            &mut warnings,
        );

        assert!(partial);
        assert!(!object.contains_key("usage"));
        assert_eq!(warnings, ["extraction metadata serialization failed"]);
        assert!(!warnings[0].contains("private fixture error"));
    }

    #[test]
    fn helper_frame_rejects_a_digest_mismatch() {
        let mut frame = run_capture_extraction_helper(
            encode_extraction_request(
                "claude_code",
                Some(&RedactedBytes::new_unchecked(b"{}\n".to_vec())),
                true,
            )
            .expect("encode fixed test request"),
        );
        frame[5..EXTRACTION_RESPONSE_HEADER_BYTES].fill(0);
        assert!(matches!(
            decode_extraction_response(frame),
            DeadlineExtractionResult::Failed
        ));
    }

    #[test]
    fn helper_omits_child_aggregation_with_a_safe_partial_warning() {
        let parent = RedactedBytes::new_unchecked(b"{}\n".to_vec());
        let request = encode_extraction_request("claude_code", Some(&parent), true)
            .expect("encode fixed test request");
        let frame = run_capture_extraction_helper(request);
        let DeadlineExtractionResult::Complete(metadata) = decode_extraction_response(frame) else {
            panic!("valid bounded extraction response must decode");
        };
        assert_eq!(metadata["partial"], Value::Bool(true));
        assert!(
            metadata["warnings"]
                .as_array()
                .is_some_and(|warnings| warnings.iter().any(|warning| warning
                    .as_str()
                    .is_some_and(|warning| warning == CHILDREN_OMITTED_WARNING)))
        );
    }

    #[test]
    fn deadline_helper_bounds_high_cardinality_modified_files_before_serialization() {
        let mut transcript = String::new();
        for index in 0..=DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION {
            transcript.push_str(&format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Write","input":{{"file_path":"src/generated-{index}.rs"}}}}]}}}}"#
            ));
            transcript.push('\n');
        }
        let parent = RedactedBytes::new_unchecked(transcript.into_bytes());
        let request = encode_extraction_request("claude_code", Some(&parent), false)
            .expect("encode bounded file-collection request");
        let frame = run_capture_extraction_helper(request);
        assert!(
            frame.len()
                <= usize::try_from(CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP)
                    .expect("output cap fits usize"),
            "the helper must reject collection growth before serializing a large metadata frame"
        );
        let DeadlineExtractionResult::Complete(metadata) = decode_extraction_response(frame) else {
            panic!("bounded file collection must still return a safe metadata frame");
        };
        assert_eq!(metadata["partial"], Value::Bool(true));
        assert_eq!(
            metadata["modified_files"].as_array().map(Vec::len),
            Some(DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION)
        );
        assert!(
            metadata["warnings"]
                .as_array()
                .is_some_and(|warnings| warnings
                    .iter()
                    .any(|warning| warning.as_str()
                        == Some("transcript parser reported partial output"))),
            "overflow must be an explicit, content-free partial diagnostic: {metadata}"
        );
    }

    #[test]
    fn deadline_helper_bounds_prompt_and_skill_event_collections_before_serialization() {
        let mut transcript = String::new();
        for index in 0..=DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION {
            transcript.push_str(&format!(
                r#"{{"type":"user","uuid":"turn-{index}","timestamp":"2026-09-29T00:00:00Z","message":{{"content":"/review item-{index}"}}}}"#
            ));
            transcript.push('\n');
        }
        let parent = RedactedBytes::new_unchecked(transcript.into_bytes());
        let request = encode_extraction_request("claude_code", Some(&parent), false)
            .expect("encode bounded prompt-collection request");
        let frame = run_capture_extraction_helper(request);
        assert!(
            frame.len()
                <= usize::try_from(CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP)
                    .expect("output cap fits usize")
        );
        let DeadlineExtractionResult::Complete(metadata) = decode_extraction_response(frame) else {
            panic!("bounded prompt collection must still return a safe metadata frame");
        };
        assert_eq!(metadata["partial"], Value::Bool(true));
        assert_eq!(
            metadata["prompt_count"],
            Value::from(DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION as u64)
        );
        assert_eq!(
            metadata["skill_events"].as_array().map(Vec::len),
            Some(DEADLINE_EXTRACTION_MAX_ITEMS_PER_COLLECTION)
        );
    }

    #[test]
    fn deadline_helper_keeps_in_budget_collections_complete() {
        let parent = RedactedBytes::new_unchecked(
            concat!(
                r#"{"type":"user","uuid":"turn-1","message":{"content":"/review this"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"claude-test","content":[{"type":"tool_use","name":"Write","input":{"file_path":"src/lib.rs"}},{"type":"tool_use","name":"Write","input":{"file_path":"src/lib.rs"}}]}}"#,
                "\n",
            )
            .as_bytes()
            .to_vec(),
        );
        let request = encode_extraction_request("claude_code", Some(&parent), false)
            .expect("encode in-budget collection request");
        let DeadlineExtractionResult::Complete(metadata) =
            decode_extraction_response(run_capture_extraction_helper(request))
        else {
            panic!("in-budget collection must produce complete metadata");
        };
        assert_eq!(metadata["partial"], Value::Bool(false));
        assert_eq!(metadata["prompt_count"], Value::from(1));
        assert_eq!(
            metadata["modified_files"],
            serde_json::json!(["src/lib.rs"])
        );
        assert_eq!(metadata["skill_events"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn request_decoder_moves_the_full_redacted_parent_without_a_clone() {
        let parent = RedactedBytes::new_unchecked(b"{\"type\":\"user\"}\n".to_vec());
        let request = encode_extraction_request("claude_code", Some(&parent), false)
            .expect("encode fixed test request");
        let allocation = request.as_ptr();
        let Some((_, Some(decoded_parent), _)) = decode_extraction_request(request) else {
            panic!("valid request must decode");
        };
        assert_eq!(
            decoded_parent.bytes().as_ptr(),
            allocation,
            "helper must compact the frame into its existing allocation instead of cloning parent bytes"
        );
    }

    #[test]
    fn request_header_preserves_the_complete_live_redacted_source_cap() {
        let full_live_redacted_len =
            usize::try_from(CAPTURE_EXTRACTION_HELPER_PARENT_CAP).expect("cap fits usize");
        assert!(
            encode_extraction_request_header("claude_code", true, full_live_redacted_len, false,)
                .is_some(),
            "a complete 1.5x live redacted source must not be converted into a prefix capture"
        );
        assert_eq!(
            CAPTURE_EXTRACTION_HELPER_INPUT_CAP,
            CAPTURE_EXTRACTION_HELPER_PARENT_CAP + EXTRACTION_REQUEST_HEADER_BYTES as u64
        );
    }

    #[test]
    fn derived_skill_json_redacts_dynamic_object_keys_as_well_as_values() {
        let secret = format!("ghp_{}", "d".repeat(36));
        let value = json!({ format!("key-{secret}"): secret.clone() });
        let redacted = redact_extracted_json(value);
        assert!(
            !serde_json::to_string(&redacted)
                .expect("serialize redacted derived skill JSON")
                .contains(&secret),
            "dynamic JSON keys must not bypass derived-field redaction"
        );
    }
}
