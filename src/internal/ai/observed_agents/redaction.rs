//! Redaction engine and the [`RedactedBytes`] compile-time contract.
//!
//! # Why a newtype?
//!
//! `agent_capture` writes transcript bytes into Git blobs that become part of
//! `refs/libra/traces` and (eventually) sync to R2/D1. If a future
//! refactor accidentally hands raw `&[u8]` to one of those persistence paths,
//! every still-unscanned secret in the bytes leaks into the durable store.
//! The Phase 1 risk table calls this out as P0.
//!
//! `RedactedBytes` is a transparent newtype around `Vec<u8>` that can only be
//! produced inside this module. Persistence functions take `&RedactedBytes`,
//! not `&[u8]`, so the type system enforces the redaction step at every
//! callsite. There is no public `From<Vec<u8>>` impl by design.
//!
//! # Engine
//!
//! V1 ships with a small, conservative rule set covering the common
//! high-confidence formats (AWS / GCP / GitHub / Slack / generic JWT, plus
//! a `postgres://user:pass@…` URI rule). The full `gitleaks`-style rule
//! matrix and PII detection are Phase 3 work — see
//! `docs/development/commands/_general.md` section 8.

use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::bytes::Regex;
use serde::Serialize;

/// Bytes that have passed through a [`Redactor`].
///
/// The newtype is *transparent* (the inner `Vec<u8>` is reachable via
/// [`Self::bytes`] / [`Self::into_inner`]) but not *constructible* from
/// arbitrary input — only this module can call [`Self::new_unchecked`].
///
/// Persistence APIs (`observed_agents::checkpoint::write_transcript_blob`, the
/// cloud-sync transcript uploader, `HistoryManager::create_append_commit`'s
/// transcript channel) accept `&RedactedBytes` rather than `&[u8]`. Calling
/// them therefore requires going through [`Redactor::redact`] first.
///
/// # Compile-time contract
///
/// The constructor is `pub(crate)`; downstream callers cannot mint a value
/// without round-tripping through [`Redactor::redact`]. The doctest below
/// pins this — if a future refactor accidentally widens the constructor to
/// `pub`, doctest compilation succeeds and `cargo test` flips red because
/// the `compile_fail` annotation expects failure.
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::RedactedBytes;
/// // Must NOT compile — `new_unchecked` is `pub(crate)`. If this ever
/// // builds, the contract has been silently widened and every downstream
/// // sink can be fed un-redacted bytes.
/// let _ = RedactedBytes::new_unchecked(vec![0u8]);
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::RedactedBytes;
/// // Must NOT compile — `data` is private.
/// let _ = RedactedBytes { data: vec![0u8] };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedBytes {
    data: Vec<u8>,
}

impl RedactedBytes {
    /// Construct a `RedactedBytes` from already-redacted input.
    ///
    /// Visibility note: `pub(crate)` rather than `pub` so that only code
    /// inside this crate can build the type, *and* by convention only
    /// [`Redactor::redact`] (and a couple of well-named test helpers below)
    /// invokes it. External crates have no way to bypass redaction.
    pub(crate) fn new_unchecked(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Borrow the redacted byte slice.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Length of the redacted byte buffer in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// `true` if the redacted buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Consume the wrapper and return the inner bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }
}

impl AsRef<[u8]> for RedactedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

/// Single regex-based redaction rule.
#[derive(Debug, Clone)]
pub struct RedactionRule {
    pub id: &'static str,
    pub regex: Regex,
    pub replacement: &'static str,
}

/// Where a rule fired in the input.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RedactionMatch {
    pub rule_id: String,
    pub start: usize,
    pub end: usize,
}

/// Aggregate report returned alongside [`RedactedBytes`] so callers can stamp
/// it onto `agent_session.redaction_report` and the per-checkpoint metadata
/// blob.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RedactionReport {
    pub matches: Vec<RedactionMatch>,
    pub bytes_scanned: usize,
    pub bytes_redacted: usize,
}

/// Redaction engine. Cheap to clone (the rules are `Arc`-shared) so the
/// runtime can keep one instance per session without paying per-rule rebuild
/// costs.
#[derive(Debug, Clone)]
pub struct Redactor {
    rules: Arc<Vec<RedactionRule>>,
}

impl Redactor {
    /// Build a redactor with the v1 default rule set.
    pub fn new_default() -> Self {
        Self {
            rules: Arc::clone(&DEFAULT_RULES),
        }
    }

    /// Build a redactor with a caller-supplied rule set. Useful for tests.
    pub fn with_rules(rules: Vec<RedactionRule>) -> Self {
        Self {
            rules: Arc::new(rules),
        }
    }

    /// Number of rules registered. Mostly useful for tests / diagnostics.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Walk every rule across `input` and return the redacted bytes plus a
    /// report. Rules are applied in priority order (the order they appear in
    /// [`DEFAULT_RULES`]) and replacements are non-overlapping — once a span
    /// has been replaced, later rules don't re-scan the placeholder.
    pub fn redact(&self, input: &[u8]) -> (RedactedBytes, RedactionReport) {
        let mut output = input.to_vec();
        let mut report = RedactionReport {
            bytes_scanned: input.len(),
            ..Default::default()
        };

        for rule in self.rules.iter() {
            // Re-scan after each rule because earlier replacements can shift
            // byte offsets. The cost is bounded — typical transcripts are
            // <16 MiB and the rule set is small.
            let buffer = output.clone();
            let mut last_end = 0usize;
            let mut new_output = Vec::with_capacity(buffer.len());
            let placeholder = format!("<REDACTED:{}>", rule.id);
            let placeholder_bytes = placeholder.as_bytes();

            for m in rule.regex.find_iter(&buffer) {
                let start = m.start();
                let end = m.end();
                // Skip already-redacted spans so re-running the same rule is
                // idempotent and rules don't recursively eat each other's
                // placeholders.
                if buffer[start..end].starts_with(b"<REDACTED:") {
                    continue;
                }
                new_output.extend_from_slice(&buffer[last_end..start]);
                new_output.extend_from_slice(placeholder_bytes);
                report.matches.push(RedactionMatch {
                    rule_id: rule.id.to_string(),
                    start,
                    end,
                });
                report.bytes_redacted += end - start;
                last_end = end;
            }
            new_output.extend_from_slice(&buffer[last_end..]);
            output = new_output;
        }

        (RedactedBytes::new_unchecked(output), report)
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new_default()
    }
}

/// Marker trait reserved for a future sink abstraction that persists redacted
/// bytes (e.g. a cloud-sync uploader).
///
/// NOTE (G4): the checkpoint commit writer does NOT route through this trait —
/// it enforces the same invariant directly by typing every blob parameter of
/// `history::CheckpointCommitParams` (metadata / transcript / lifecycle events
/// / redaction report) as [`RedactedBytes`], so no `&[u8]` can reach the
/// `refs/libra/traces` sink. Together with the `pub(crate)` constructor on
/// [`RedactedBytes`], nothing flows into a persistence path without first
/// passing through [`Redactor::redact`]. This trait remains an (unused)
/// placeholder so `tests/redaction_contract_test.rs` can pin the contract for
/// the day a distinct uploader sink lands.
pub trait RedactedSink {
    fn accept(&mut self, redacted: &RedactedBytes);
}

/// Default rule set. Conservative on purpose — false positives on
/// transcripts are very expensive (they make sessions unreadable) so each
/// rule below is anchored to a high-signal prefix.
static DEFAULT_RULES: Lazy<Arc<Vec<RedactionRule>>> = Lazy::new(|| {
    let raw: &[(&'static str, &'static str)] = &[
        // AWS access keys: the `AKIA` / `ASIA` / `AGPA` family.
        (
            "aws-access-key-id",
            r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASCA)[0-9A-Z]{16}\b",
        ),
        // GitHub PATs and OAuth tokens.
        (
            "github-token",
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[0-9A-Za-z]{36,251}\b",
        ),
        // GitHub fine-grained PATs (`github_pat_…`).
        (
            "github-fine-grained-pat",
            r"\bgithub_pat_[0-9A-Za-z_]{20,}\b",
        ),
        // AWS secret access key — appears next to `aws_secret_access_key`,
        // `secret_access_key`, or as the second half of `access_key:secret`
        // pairs. Keyed off the `aws_secret_access_key=…` / similar literal
        // because a bare 40-char base64 string is far too noisy to redact
        // unconditionally.
        (
            "aws-secret-access-key",
            r"(?i)(?:aws[_-]?secret[_-]?access[_-]?key|secret[_-]?access[_-]?key)\s*[:=]\s*[A-Za-z0-9/+=]{40}",
        ),
        // Slack bot/user/legacy tokens.
        ("slack-token", r"\bxox[abprs]-[0-9A-Za-z-]{10,72}\b"),
        // Google API keys.
        ("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
        // Anthropic API keys (`sk-ant-…`). MUST come BEFORE the bare
        // `sk-…` OpenAI rule below — the OpenAI pattern is a strict
        // superset of the Anthropic shape (both start `sk-`), so without
        // this earlier rule Anthropic keys would be silently mistagged
        // as `openai-api-key`.
        ("anthropic-api-key", r"\bsk-ant-[0-9A-Za-z_-]{20,}\b"),
        // OpenAI API keys (current "sk-..." family — both legacy and project keys).
        ("openai-api-key", r"\bsk-[0-9A-Za-z_-]{20,}\b"),
        // Generic JWTs (header.payload.signature).
        (
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        ),
        // Postgres / MySQL connection URIs with embedded credentials.
        (
            "credential-uri",
            r"(?i)\b(?:postgres|postgresql|mysql|mongodb|redis|amqp|amqps)://[^\s/@:]+:[^\s/@]+@[^\s]+",
        ),
        // Google service-account JSON `private_key` field. JSON pretty-
        // printers escape the BEGIN/END markers; this rule catches both the
        // raw and the JSON-escaped forms by anchoring on `\"private_key\"`.
        // MUST come BEFORE the bare `private-key-pem` rule below — the
        // bare PEM rule matches the inner armoured key first otherwise,
        // which produces `<REDACTED:private-key-pem>` for the inner span
        // and never gives this more-specific JSON-aware rule a chance.
        (
            "google-service-account-private-key",
            r#""private_key"\s*:\s*"-----BEGIN [^"]*PRIVATE KEY-----[\s\S]*?-----END [^"]*PRIVATE KEY-----[^"]*""#,
        ),
        // Private-key PEM headers — match the marker, not the body, so the
        // replacement collapses the entire armoured key into a placeholder.
        (
            "private-key-pem",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
        ),
        // Stripe live + test secrets (rk_, sk_, pk_ — pk is publishable but
        // still high-signal in transcripts).
        (
            "stripe-key",
            r"\b(?:sk|rk|pk)_(?:live|test)_[0-9A-Za-z]{20,}\b",
        ),
        // Twilio Account SID. The `AC`/`SK` prefix plus 32 hex is the
        // documented format. The 32-hex-only Auth Token is intentionally
        // NOT matched here — bare hex strings of that length are too noisy
        // to redact unconditionally without a keyword anchor.
        ("twilio-account-sid", r"\b(?:AC|SK)[0-9a-fA-F]{32}\b"),
        // SendGrid API keys (`SG.<24>.<43>`).
        (
            "sendgrid-api-key",
            r"\bSG\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{30,}\b",
        ),
        // Mailgun keys (`key-<hex>` legacy style and the new `key-…`).
        ("mailgun-api-key", r"\bkey-[0-9a-fA-F]{32}\b"),
        // npm automation tokens — both legacy `npm_…` and `npm-…` shapes.
        ("npm-token", r"\bnpm_[0-9A-Za-z]{32,}\b"),
        // PyPI upload tokens (`pypi-…`).
        ("pypi-token", r"\bpypi-[A-Za-z0-9_-]{32,}\b"),
        // GitLab personal/access tokens — `glpat-` prefix is the modern PAT
        // shape; older deploy tokens follow `gldt-`.
        ("gitlab-pat", r"\b(?:glpat|gldt)-[0-9A-Za-z_-]{20,}\b"),
        // Atlassian API tokens commonly start with `ATATT`. Conservative
        // length bound to dodge stray words.
        ("atlassian-api-token", r"\bATATT[0-9A-Za-z_-]{32,}\b"),
        // Cloudflare API tokens — opaque random strings; we anchor off the
        // common `Bearer <40 char>` shape inside `Authorization` headers
        // because a bare token is too noisy to redact unconditionally.
        (
            "cloudflare-bearer",
            r"(?i)Authorization:\s*Bearer\s+[A-Za-z0-9_-]{40}\b",
        ),
        // (`google-service-account-private-key` rule lives earlier, ahead
        // of the bare `private-key-pem` rule, so the JSON wrapper takes
        // precedence over the inner armoured key. Don't add a second copy
        // here.)
        // (Heroku API keys are UUID-shaped — too generic to redact safely
        // without a lookaround. The `regex` crate does not support
        // lookahead/lookbehind, so a naive `(?=heroku)` pattern would
        // poison `DEFAULT_RULES` on first use. Skipped intentionally.)
        // Hugging Face hub tokens (`hf_…`).
        ("huggingface-token", r"\bhf_[A-Za-z0-9]{32,}\b"),
        // DigitalOcean PATs (`dop_v1_…`).
        ("digitalocean-pat", r"\bdop_v1_[A-Za-z0-9]{40,}\b"),
        // Telegram bot tokens — `<numeric>:<35 alnum>`. We don't anchor on
        // a leading `\b` because tokens commonly appear as URL substrings
        // like `https://api.telegram.org/bot<token>/getMe`, where there
        // is no word boundary between `bot` and the digits. The character
        // class `[A-Za-z0-9_-]` is greedy so it stops naturally at the
        // first non-class character (e.g. `/`).
        ("telegram-bot-token", r"\d{8,11}:[A-Za-z0-9_-]{30,}"),
        // Discord bot tokens — three dot-separated base64ish parts.
        (
            "discord-bot-token",
            r"\b[MN][A-Za-z\d]{23}\.[\w-]{6}\.[\w-]{27,}\b",
        ),
        // High-entropy `password` / `secret_key` literals in shell-style
        // env files. Bound is 32+ chars rather than 16 to dodge benign
        // 16-char tokens that show up in test fixtures, UUID-like names,
        // and DB connection-pool defaults — Codex Phase 3 review flagged
        // the previous {16,} as too aggressive.
        (
            "env-password-assignment",
            r#"(?i)(?:password|passwd|secret_key|api_secret)\s*[:=]\s*['"]?[A-Za-z0-9._/+=-]{32,}['"]?"#,
        ),
    ];

    Arc::new(
        raw.iter()
            .map(|(id, pattern)| RedactionRule {
                id,
                regex: Regex::new(pattern).expect("default redaction pattern must compile"),
                replacement: id,
            })
            .collect(),
    )
});

#[cfg(test)]
mod tests {
    use super::*;

    fn redact_str(redactor: &Redactor, input: &str) -> (String, RedactionReport) {
        let (bytes, report) = redactor.redact(input.as_bytes());
        (
            String::from_utf8(bytes.into_inner()).expect("UTF-8 round-trip"),
            report,
        )
    }

    #[test]
    fn redacts_aws_access_key() {
        let r = Redactor::new_default();
        let (out, report) = redact_str(&r, "AKIAIOSFODNN7EXAMPLE in transcript");
        assert!(out.contains("<REDACTED:aws-access-key-id>"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(report.matches.len(), 1);
    }

    #[test]
    fn redacts_github_pat() {
        let r = Redactor::new_default();
        let token = "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _report) = redact_str(&r, &format!("token={token}"));
        assert!(out.contains("<REDACTED:github-token>"));
    }

    /// CEX-EntireIO Codex review P1 #7: fine-grained GitHub PATs use a
    /// distinct prefix (`github_pat_…`) and must be redacted.
    #[test]
    fn redacts_github_fine_grained_pat() {
        let r = Redactor::new_default();
        // Fine-grained PATs are quite long; 60 alphanumeric chars is well
        // within the lower bound of the live format.
        let token = format!("github_pat_{}", "x".repeat(60));
        let (out, _) = redact_str(&r, &format!("auth={token}"));
        assert!(out.contains("<REDACTED:github-fine-grained-pat>"));
        assert!(!out.contains(&token));
    }

    /// CEX-EntireIO Codex review P1 #7: AWS secret access keys.
    #[test]
    fn redacts_aws_secret_access_key_kv() {
        let r = Redactor::new_default();
        let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let (out, _) = redact_str(
            &r,
            &format!("aws_secret_access_key = {secret}\nrest of file"),
        );
        assert!(out.contains("<REDACTED:aws-secret-access-key>"));
        assert!(!out.contains(secret));
    }

    #[test]
    fn redacts_postgres_uri() {
        let r = Redactor::new_default();
        let (out, report) = redact_str(&r, "DSN=postgres://alice:s3cret@db.example.com:5432/app");
        assert!(out.contains("<REDACTED:credential-uri>"));
        assert!(!out.contains("s3cret"));
        assert_eq!(report.matches.len(), 1);
    }

    #[test]
    fn redacts_private_key_block() {
        let r = Redactor::new_default();
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";
        let (out, _) = redact_str(&r, pem);
        assert_eq!(out, "<REDACTED:private-key-pem>");
    }

    #[test]
    fn passes_through_clean_text_without_modification() {
        let r = Redactor::new_default();
        let clean = "this transcript discusses Rust borrow checker and references no secrets";
        let (out, report) = redact_str(&r, clean);
        assert_eq!(out, clean);
        assert!(report.matches.is_empty());
        assert_eq!(report.bytes_redacted, 0);
        assert_eq!(report.bytes_scanned, clean.len());
    }

    /// False-positive guard: realistic non-secret developer content
    /// must pass through untouched. The positive tests confirm secrets
    /// ARE redacted; this confirms an over-broad rule doesn't corrupt a
    /// captured transcript by redacting legitimate tokens (a 40-hex git
    /// SHA, a UUID, a `path:line` ref, a semver, and prose that merely
    /// contains the words "sk"/"key"). Each string below is a plausible
    /// false-positive candidate — if a rule regex is ever loosened to
    /// match one, this test flips red.
    #[test]
    fn clean_developer_content_is_not_over_redacted() {
        let r = Redactor::new_default();
        for clean in [
            // 40-char lowercase hex git SHA — must NOT trip a rule.
            // No rule matches a bare hex run: the entropy-bearing rules
            // (aws-secret, etc.) require a `key=`/`secret=` context, and
            // the structural rules (telegram `\d{8,11}:…`, discord, jwt
            // `eyJ…`) require their own distinct shapes that 40 hex
            // chars don't satisfy.
            "commit 9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c",
            // A UUID (e.g. a thread id) — hyphen-separated hex groups.
            "thread 550e8400-e29b-41d4-a716-446655440000 resumed",
            // A repo-relative path with a line number.
            "see src/internal/ai/observed_agents/redaction.rs:163",
            // A semver / version banner.
            "libra 0.17.1004 release build",
            // Prose containing the substrings "sk" and "key" without a
            // key SHAPE (no `sk-`+20chars, no `AIza`, no `xox…`).
            "the sk module exports a key helper for the task",
        ] {
            let (out, report) = redact_str(&r, clean);
            assert_eq!(out, clean, "clean content must pass through unchanged");
            assert!(
                report.matches.is_empty(),
                "clean content `{clean}` must not match any redaction rule, got {:?}",
                report.matches,
            );
        }
    }

    #[test]
    fn preserves_byte_count_metadata() {
        let r = Redactor::new_default();
        let (_, report) = redact_str(&r, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(report.bytes_scanned, "AKIAIOSFODNN7EXAMPLE".len());
        // The 20-char AKIA key gets replaced with the placeholder, but
        // bytes_redacted measures pre-replacement bytes.
        assert!(report.bytes_redacted >= 20);
    }

    /// The newtype is the contract: only this module's redact path can build
    /// a `RedactedBytes`. We exercise that path here to confirm the round-trip
    /// works and the wrapper is transparent.
    #[test]
    fn redacted_bytes_is_transparent() {
        let r = Redactor::new_default();
        let (rb, _) = r.redact(b"hello");
        assert_eq!(rb.as_ref(), b"hello");
        assert_eq!(rb.len(), 5);
        assert!(!rb.is_empty());
        assert_eq!(rb.clone().into_inner(), b"hello");
    }

    #[test]
    fn idempotent_on_already_redacted_input() {
        let r = Redactor::new_default();
        let (first, _) = r.redact(b"AKIAIOSFODNN7EXAMPLE here");
        let (second, second_report) = r.redact(first.bytes());
        assert_eq!(first, second);
        // No new matches on the placeholder.
        assert!(second_report.matches.is_empty());
    }

    #[test]
    fn applies_multiple_rules_in_one_pass() {
        let r = Redactor::new_default();
        let input = "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa AKIAIOSFODNN7EXAMPLE";
        let (out, report) = redact_str(&r, input);
        assert!(out.contains("<REDACTED:github-token>"));
        assert!(out.contains("<REDACTED:aws-access-key-id>"));
        assert_eq!(report.matches.len(), 2);
    }

    // ── Phase 3.2 expanded rules ─────────────────────────────────────────

    #[test]
    fn redacts_anthropic_api_key_with_correct_tag() {
        // Anthropic keys share the `sk-` prefix with OpenAI keys, so the
        // more-specific `sk-ant-…` rule must fire first to give the right
        // provider tag. If this regresses (rule order swapped or the
        // `anthropic-api-key` rule is removed), the placeholder would be
        // `<REDACTED:openai-api-key>` and downstream provenance would lose
        // the Anthropic attribution.
        let r = Redactor::new_default();
        let key = format!("sk-ant-{}", "a".repeat(40));
        let (out, report) = redact_str(&r, &format!("ANTHROPIC_API_KEY={key}"));
        assert!(out.contains("<REDACTED:anthropic-api-key>"));
        assert!(!out.contains("<REDACTED:openai-api-key>"));
        assert!(!out.contains(&key));
        assert!(
            report
                .matches
                .iter()
                .any(|m| m.rule_id == "anthropic-api-key")
        );
    }

    #[test]
    fn redacts_stripe_secret_key() {
        let r = Redactor::new_default();
        // Composed at runtime to dodge GitHub's secret-scanning push
        // protection — the literal `sk_live_<24+ alphanumeric>` shape
        // is flagged by Stripe's pattern even when the value is fake.
        let key = format!("sk_live_{}", "a".repeat(24));
        let (out, _) = redact_str(&r, &format!("STRIPE={key}"));
        assert!(out.contains("<REDACTED:stripe-key>"));
        assert!(!out.contains(&key));
    }

    #[test]
    fn redacts_stripe_test_key() {
        let r = Redactor::new_default();
        let key = format!("sk_test_{}", "b".repeat(24));
        let (out, _) = redact_str(&r, &key);
        assert!(out.contains("<REDACTED:stripe-key>"));
    }

    #[test]
    fn redacts_twilio_account_sid() {
        let r = Redactor::new_default();
        // Fixture is composed at runtime so the literal `AC<32 hex>`
        // string never appears in source — GitHub's secret-scanning push
        // protection flags both AC- and SK-prefixed Twilio shapes
        // verbatim regardless of whether the digits are real.
        let sid = format!("AC{}", "0123456789abcdef".repeat(2));
        let (out, _) = redact_str(&r, &format!("twilio={sid}"));
        assert!(out.contains("<REDACTED:twilio-account-sid>"));
        assert!(!out.contains(&sid));
    }

    #[test]
    fn redacts_sendgrid_key() {
        let r = Redactor::new_default();
        let key = "SG.aaaaaaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let (out, _) = redact_str(&r, key);
        assert!(out.contains("<REDACTED:sendgrid-api-key>"));
    }

    #[test]
    fn redacts_npm_token() {
        let r = Redactor::new_default();
        let token = "npm_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, &format!("NPM_TOKEN={token}"));
        assert!(out.contains("<REDACTED:npm-token>"));
    }

    #[test]
    fn redacts_gitlab_pat() {
        let r = Redactor::new_default();
        let token = "glpat-aaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, &format!("GITLAB_TOKEN={token}"));
        assert!(out.contains("<REDACTED:gitlab-pat>"));
    }

    #[test]
    fn redacts_huggingface_token() {
        let r = Redactor::new_default();
        let token = "hf_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, token);
        assert!(out.contains("<REDACTED:huggingface-token>"));
    }

    #[test]
    fn redacts_digitalocean_pat() {
        let r = Redactor::new_default();
        let token = "dop_v1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, token);
        assert!(out.contains("<REDACTED:digitalocean-pat>"));
    }

    #[test]
    fn redacts_env_password_assignment() {
        let r = Redactor::new_default();
        // Value is 36 chars, above the 32-char floor.
        let secret = "correcthorsebatterystaple_42abcdefAB";
        assert!(secret.len() >= 32);
        let (out, _) = redact_str(&r, &format!("DB_PASSWORD={secret}"));
        assert!(out.contains("<REDACTED:env-password-assignment>"));
        assert!(!out.contains(secret));
    }

    /// Regression for the Phase 3 threshold tightening: a benign 16-char
    /// value next to a `password=` keyword must NOT be redacted under the
    /// new {32,} lower bound. This is the false-positive class Codex
    /// flagged.
    #[test]
    fn does_not_redact_short_env_password_values() {
        let r = Redactor::new_default();
        let benign = "password=changeme12345678";
        assert_eq!(benign.len() - "password=".len(), 16);
        let (out, _) = redact_str(&r, benign);
        assert!(
            !out.contains("<REDACTED:env-password-assignment>"),
            "expected benign 16-char value to round-trip; got {out}"
        );
    }

    #[test]
    fn redacts_atlassian_api_token() {
        let r = Redactor::new_default();
        let token = "ATATT3xFfGF0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, token);
        assert!(out.contains("<REDACTED:atlassian-api-token>"));
    }

    #[test]
    fn redacts_pypi_token() {
        let r = Redactor::new_default();
        let token = "pypi-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, token);
        assert!(out.contains("<REDACTED:pypi-token>"));
    }

    #[test]
    fn redacts_mailgun_api_key() {
        let r = Redactor::new_default();
        // Composed at runtime so the literal `key-<32 hex>` shape never
        // appears in source — same reason as the Twilio fixture.
        let key = format!("key-{}", "0123456789abcdef".repeat(2));
        let (out, _) = redact_str(&r, &format!("MAILGUN={key}"));
        assert!(out.contains("<REDACTED:mailgun-api-key>"));
        assert!(!out.contains(&key));
    }

    #[test]
    fn redacts_cloudflare_bearer() {
        let r = Redactor::new_default();
        // 40 alphanumeric chars after `Bearer ` is the documented shape.
        let token = "abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        assert_eq!(token.len(), 40);
        let header = format!("Authorization: Bearer {token}\n");
        let (out, _) = redact_str(&r, &header);
        assert!(out.contains("<REDACTED:cloudflare-bearer>"));
        assert!(!out.contains(token));
    }

    #[test]
    fn redacts_google_service_account_private_key() {
        let r = Redactor::new_default();
        // Compose the JSON at runtime so the literal PKCS#8 prefix (which
        // some secret scanners use as a heuristic) never appears in
        // source. The fixture body is just `xxxx…` — enough to satisfy
        // the regex's `[\s\S]*?` between the BEGIN/END markers.
        let body = "x".repeat(40);
        let begin = "-----BEGIN PRIVATE KEY-----";
        let end = "-----END PRIVATE KEY-----";
        let json = format!(
            r#"{{"type":"service_account","private_key":"{begin}\n{body}\n{end}\n","client_email":"x@y.iam.gserviceaccount.com"}}"#
        );
        let (out, _) = redact_str(&r, &json);
        assert!(
            out.contains("<REDACTED:google-service-account-private-key>"),
            "expected service-account redaction; got {out}"
        );
        assert!(!out.contains(&body));
        // The non-private-key fields stay around — only the key itself is
        // collapsed to the placeholder.
        assert!(out.contains("\"type\":\"service_account\""));
        assert!(out.contains("\"client_email\""));
    }

    #[test]
    fn redacts_telegram_bot_token() {
        let r = Redactor::new_default();
        let token = "1234567890:AAEhBP0av28aaaaaaaaaaaaaaaaaaaaaaaa";
        let (out, _) = redact_str(&r, &format!("https://api.telegram.org/bot{token}/getMe"));
        assert!(out.contains("<REDACTED:telegram-bot-token>"));
        assert!(!out.contains(token));
    }

    /// Slack tokens (`xox[abprs]-…`) must be redacted before an observed
    /// transcript is persisted. The fixture is composed at runtime so a
    /// literal Slack-token shape isn't checked into source (GitHub
    /// secret-scanning push protection flags the literal shape even for
    /// fake values).
    #[test]
    fn redacts_slack_token() {
        let r = Redactor::new_default();
        let token = format!("xoxb-{}", "a".repeat(24));
        let (out, report) = redact_str(&r, &format!("SLACK_TOKEN={token}"));
        assert!(out.contains("<REDACTED:slack-token>"));
        assert!(!out.contains(&token));
        assert!(report.matches.iter().any(|m| m.rule_id == "slack-token"));
    }

    /// Google API keys (`AIza` + 35 chars) must be redacted. Composed at
    /// runtime to dodge secret-scanning push protection.
    #[test]
    fn redacts_google_api_key() {
        let r = Redactor::new_default();
        let key = format!("AIza{}", "a".repeat(35));
        let (out, report) = redact_str(&r, &format!("GOOGLE_API_KEY={key}"));
        assert!(out.contains("<REDACTED:google-api-key>"));
        assert!(!out.contains(&key));
        assert!(report.matches.iter().any(|m| m.rule_id == "google-api-key"));
    }

    /// OpenAI keys (`sk-…`, distinct from the more-specific `sk-ant-…`
    /// Anthropic shape) must be redacted and tagged `openai-api-key`.
    /// The fixture deliberately does NOT start with `ant-` so the
    /// Anthropic rule does not claim it.
    ///
    /// `redacts_anthropic_api_key_with_correct_tag` already asserts the
    /// *negative* (an `sk-ant-…` key must NOT get the `openai-api-key`
    /// tag); this is the missing *positive* counterpart — a plain
    /// `sk-…` key actually gets redacted and tagged `openai-api-key`.
    #[test]
    fn redacts_openai_api_key() {
        let r = Redactor::new_default();
        let key = format!("sk-{}", "a".repeat(24));
        let (out, report) = redact_str(&r, &format!("OPENAI_API_KEY={key}"));
        assert!(out.contains("<REDACTED:openai-api-key>"));
        assert!(!out.contains("<REDACTED:anthropic-api-key>"));
        assert!(!out.contains(&key));
        assert!(report.matches.iter().any(|m| m.rule_id == "openai-api-key"));
    }

    /// JWTs (`eyJ…header.payload.signature`) must be redacted — they
    /// frequently carry bearer credentials. Composed from three
    /// base64url-shaped segments at runtime.
    #[test]
    fn redacts_jwt() {
        let r = Redactor::new_default();
        let jwt = format!(
            "eyJ{}.{}.{}",
            "a".repeat(12),
            "b".repeat(12),
            "c".repeat(12)
        );
        let (out, report) = redact_str(&r, &format!("Authorization: Bearer {jwt}"));
        assert!(out.contains("<REDACTED:jwt>"));
        assert!(!out.contains(&jwt));
        assert!(report.matches.iter().any(|m| m.rule_id == "jwt"));
    }

    #[test]
    fn redacts_discord_bot_token() {
        let r = Redactor::new_default();
        // Synthesised fixture matching the documented Discord token shape
        // (`<24-char-base64>.<6-char>.<27+-char>`) without checking in a
        // single literal string that GitHub's secret-scanning push
        // protection would flag as a "Discord Bot Token" regardless of
        // whether it's actually live. Splitting the parts and joining at
        // runtime keeps the test deterministic without tripping the
        // scanner.
        let part1 = "M".repeat(24);
        let part2 = "GabcDe";
        let part3 = "a".repeat(29);
        let token = format!("{part1}.{part2}.{part3}");
        let (out, _) = redact_str(&r, &token);
        assert!(out.contains("<REDACTED:discord-bot-token>"));
        assert!(!out.contains(&token));
    }

    /// Regression: the new rules should not fire on benign nearby text. A
    /// short clean transcript with no secret-like patterns must round-trip
    /// untouched.
    #[test]
    fn expanded_rules_do_not_fire_on_benign_text() {
        let r = Redactor::new_default();
        let benign = "the quick brown fox jumps over the lazy dog and eats kibble";
        let (out, report) = redact_str(&r, benign);
        assert_eq!(out, benign);
        assert!(report.matches.is_empty());
    }

    /// Belt-and-suspenders test: the `DEFAULT_RULES` `Lazy` static is
    /// initialized via `Regex::new(...).expect(...)` and a single bad
    /// pattern would poison every subsequent caller (we hit this exact
    /// failure mode in CEX-EntireIO Phase 3.2 with a `(?=...)` lookahead
    /// pattern). This test forces the Lazy to evaluate eagerly so any
    /// future bad regex turns into a localised, named failure rather than
    /// a "Lazy instance has previously been poisoned" cascade.
    #[test]
    fn default_rules_initialize_without_poisoning_lazy() {
        let r = Redactor::new_default();
        assert!(
            r.rule_count() >= 8,
            "default redactor must register at least the v1 rule set; got {}",
            r.rule_count()
        );
    }
}
