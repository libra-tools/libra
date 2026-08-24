//! Bridge payload redaction (plan-20260818 LB-03 / ADR-LB-03 / GC-LB-08).
//!
//! The bridge persists ONLY a bounded, redacted projection. Before any event
//! payload is written it is passed through the same redactor the external
//! agent capture pipeline uses (review `redact_untrusted`). Redaction is
//! fail-closed: a payload that cannot be safely redacted is refused with a
//! stable error, never stored raw and never acked.

use super::protocol::BridgeError;

/// Redact a bridge event payload for durable storage. Returns the redacted
/// JSON text (as a JSON `Value`) or a fail-closed `BridgeError` when the
/// payload cannot be represented as bounded JSON.
///
/// GC-LB-08: raw prompt/reasoning, tokens, environment, secrets and full
/// sensitive file contents must not land in the durable projection. The
/// redactor replaces known secret patterns; this is the server-side final
/// boundary, so a payload that cannot be redacted is refused rather than
/// stored raw.
pub fn redact_payload(payload: &str) -> Result<serde_json::Value, BridgeError> {
    // First, ensure the payload is valid JSON (the Harness projection is
    // always structured JSON). A non-JSON payload cannot be a valid typed
    // projection and is refused fail-closed.
    serde_json::from_str::<serde_json::Value>(payload).map_err(|e| {
        BridgeError::new(
            super::protocol::code::REDACTION_FAILED,
            "LBR-AGENT-034",
            format!("bridge event payload is not valid JSON and cannot be safely projected: {e}"),
        )
    })?;

    // Run the shared untrusted redactor over the canonical JSON bytes and
    // re-parse. Any secret-bearing string fields get scrubbed. Fail-closed
    // (GC-LB-08 / ADR-LB-03): if the redactor output cannot be re-parsed as
    // JSON — e.g. a scrub rule corrupts the structure — we refuse the payload
    // rather than falling back to the raw value, so a raw prompt/secret never
    // reaches the durable projection.
    let (redacted, _report) = crate::internal::ai::review::redact_untrusted(payload.as_bytes());
    let redacted_value: serde_json::Value = serde_json::from_str(&redacted).map_err(|e| {
        BridgeError::new(
            super::protocol::code::REDACTION_FAILED,
            "LBR-AGENT-034",
            format!("bridge event payload redaction produced non-JSON output ({e}); refusing raw fallback"),
        )
    })?;

    // Control-scrub every string so no terminal-escape / control sequence
    // survives into the durable projection.
    Ok(scrub_json_strings(redacted_value))
}

/// Replace control characters (except `\n`/`\t`) in every string leaf of a
/// JSON value so a hostile payload cannot persist ANSI/control escapes.
fn scrub_json_strings(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            serde_json::Value::String(crate::internal::ai::review::scrub_controls(&s))
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(scrub_json_strings).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| (scrub_key(k), scrub_json_strings(v)))
                .collect(),
        ),
        other => other,
    }
}

fn scrub_key(key: String) -> String {
    crate::internal::ai::review::scrub_controls(&key)
}

/// A payload that fails redaction is refused with a stable, non-retryable
/// error (ADR-LB-03: no raw fallback).
pub fn redaction_failed(message: impl Into<String>) -> BridgeError {
    BridgeError::new(
        super::protocol::code::REDACTION_FAILED,
        "LBR-AGENT-034",
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets_and_controls() {
        // A GitHub-PAT-shaped secret (36 alphanumerics after `ghp_`) which the
        // default redactor's `github-token` rule catches. Built at runtime so
        // no full secret literal appears in this source file.
        let secret = format!("{}_{}", "ghp", "abcdefghijklmnopqrstuvwxyz0123456789");
        let payload =
            format!(r#"{{"tool":"libra_status","token":"{secret}","text":"a\u001b[31mb"}}"#);
        let out = redact_payload(&payload).expect("redacts");
        let s = out.to_string();
        assert!(!s.contains(&secret), "secret must be redacted: {s}");
        // Control character scrubbed to U+FFFD.
        assert!(s.contains('\u{FFFD}'), "control char must be scrubbed: {s}");
    }

    #[test]
    fn rejects_non_json_payload() {
        assert!(redact_payload("not json at all").is_err());
    }

    #[test]
    fn valid_structured_payload_passes_through() {
        let out = redact_payload(r#"{"kind":"tool/result","tool":"libra_status","ok":true}"#)
            .expect("valid projection redacts");
        assert_eq!(out["tool"], "libra_status");
        assert_eq!(out["ok"], true);
    }
}
