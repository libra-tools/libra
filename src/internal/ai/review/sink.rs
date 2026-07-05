//! Bounded reviewer-output capture and the redaction/render pipeline.
//!
//! Every byte a reviewer process writes flows through here before it can
//! be persisted:
//!
//! 1. [`drain_capped`] — the per-reviewer bounded buffer (64 KiB per
//!    sink, `agent.md` 强制补强项 #12). Past the cap the reader keeps
//!    *draining* the pipe but stops retaining, so a flooding reviewer can
//!    never block the serial sink or its sibling reviewers, and memory
//!    stays bounded.
//! 2. [`redact_for_log`] — `Redactor::new_default()` + control-character
//!    scrub (the `redacted_stderr_excerpt` model from
//!    `observed_agents/rpc.rs`): what lands in
//!    `reviewers/<slug>.{stdout,stderr}.redacted.log`.
//! 3. [`redact_untrusted`] — `Redactor` only, preserving ANSI escapes:
//!    what lands in `findings.md` (raw-redacted, provenance=untrusted).
//!    Display goes through [`render_untrusted_findings`], which strips
//!    ANSI/terminal control sequences so a hostile reviewer cannot forge
//!    terminal output in `review show`.

use tokio::io::{AsyncRead, AsyncReadExt};

use super::store::RedactionReportSummary;
use crate::internal::ai::observed_agents::Redactor;

/// Per-sink in-memory cap for captured reviewer output
/// (`agent.md:519-525`: review sink 内存缓冲区 64 KiB).
pub const REVIEW_SINK_BUFFER_BYTES: usize = 64 * 1024;

/// Marker appended to a reviewer log when output was dropped past the
/// 64 KiB per-sink cap.
pub const REVIEW_SINK_TRUNCATION_MARKER: &str = "…[reviewer output truncated at 64 KiB cap]";

/// Spotlighting delimiter opening an untrusted reviewer excerpt inside
/// `findings.md` (provenance=untrusted; plan.md:948).
pub const UNTRUSTED_FINDINGS_OPEN_PREFIX: &str = "<<<untrusted-reviewer-output";
/// Spotlighting delimiter closing an untrusted reviewer excerpt.
pub const UNTRUSTED_FINDINGS_CLOSE: &str = "<<<end-untrusted-reviewer-output>>>";

/// Fixed-capacity byte buffer that counts (instead of storing) overflow.
#[derive(Debug)]
pub struct BoundedSinkBuffer {
    bytes: Vec<u8>,
    cap: usize,
    dropped: u64,
}

impl BoundedSinkBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
            dropped: 0,
        }
    }

    /// Retain up to the cap; count everything past it as dropped.
    pub fn push(&mut self, chunk: &[u8]) {
        let room = self.cap.saturating_sub(self.bytes.len());
        let keep = room.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..keep]);
        self.dropped += (chunk.len() - keep) as u64;
    }

    pub fn truncated(&self) -> bool {
        self.dropped > 0
    }

    pub fn dropped_bytes(&self) -> u64 {
        self.dropped
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Drain `reader` to EOF into a [`BoundedSinkBuffer`] with the given cap.
///
/// Crucially this **never stops reading** at the cap: the pipe is drained
/// until the child closes it, so a reviewer flooding stdout is never
/// blocked on a full pipe (which would stall its process and, without
/// this, could deadlock the whole run), and other reviewers are never
/// affected — each stream owns its own reader task and buffer.
pub async fn drain_capped<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> BoundedSinkBuffer {
    let mut buffer = BoundedSinkBuffer::new(cap);
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => buffer.push(&chunk[..n]),
            // A read error (e.g. the child was killed mid-write) ends the
            // stream; whatever was captured so far is kept.
            Err(_) => break,
        }
    }
    buffer
}

/// Replace every control character except `\n` / `\t` with U+FFFD (the
/// `redacted_stderr_excerpt` model from `observed_agents/rpc.rs` — a
/// hostile reviewer must not be able to persist terminal escapes).
pub fn scrub_controls(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Redact + control-scrub reviewer bytes for the persisted
/// `*.redacted.log` files: `Redactor::new_default()` followed by
/// [`scrub_controls`].
pub fn redact_for_log(input: &[u8]) -> (String, RedactionReportSummary) {
    let (redacted, summary) = redact_untrusted(input);
    (scrub_controls(&redacted), summary)
}

/// Redact reviewer bytes for `findings.md` storage: `Redactor` only —
/// the text stays "raw-redacted" (ANSI/control sequences preserved) so
/// the findings document is a faithful redacted transcript. It is
/// provenance=untrusted: any display or prompt injection MUST go through
/// [`render_untrusted_findings`] first.
pub fn redact_untrusted(input: &[u8]) -> (String, RedactionReportSummary) {
    let (redacted, report) = Redactor::new_default().redact(input);
    let mut summary = RedactionReportSummary::default();
    summary.absorb(&report);
    (
        String::from_utf8_lossy(redacted.as_ref()).into_owned(),
        summary,
    )
}

/// Render untrusted findings text for display: strips ANSI/terminal
/// control sequences **entirely** (CSI `ESC [ … final`, OSC
/// `ESC ] … BEL`/`ESC \`, and two-character `ESC x` sequences) and maps
/// any remaining control character except `\n` / `\t` to U+FFFD. The CLI
/// slice calls this before `review show` prints `findings.md`
/// (plan.md:948: reviewer stdout free text can embed escape sequences
/// that forge terminal output).
pub fn render_untrusted_findings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek().copied() {
                // CSI: ESC [ <params/intermediates 0x20-0x3f> <final 0x40-0x7e>
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] … terminated by BEL or ST (ESC \)
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{07}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            // Consume the ST's `\` when present.
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Two-character escape (ESC c, ESC 7, ESC =, …).
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            out.push('\u{FFFD}');
            continue;
        }
        out.push(c);
    }
    out
}

/// Compose one reviewer's `findings.md` section: a header line plus the
/// raw-redacted stdout excerpt fenced in explicit spotlighting
/// delimiters so downstream prompt assembly can never mistake reviewer
/// free text for instructions.
pub fn findings_section(
    slug: &str,
    status_line: &str,
    redacted_stdout: &str,
    truncated: bool,
) -> String {
    let mut section = format!("## {slug} — {status_line}\n\n");
    section.push_str(&format!(
        "{UNTRUSTED_FINDINGS_OPEN_PREFIX} slug=\"{slug}\">>>\n"
    ));
    section.push_str(redacted_stdout);
    if !redacted_stdout.ends_with('\n') && !redacted_stdout.is_empty() {
        section.push('\n');
    }
    if truncated {
        section.push_str(REVIEW_SINK_TRUNCATION_MARKER);
        section.push('\n');
    }
    section.push_str(UNTRUSTED_FINDINGS_CLOSE);
    section.push('\n');
    section
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_buffer_caps_and_counts_overflow() {
        let mut buffer = BoundedSinkBuffer::new(8);
        buffer.push(b"12345");
        assert!(!buffer.truncated());
        buffer.push(b"67890");
        assert_eq!(buffer.as_bytes(), b"12345678");
        assert!(buffer.truncated());
        assert_eq!(buffer.dropped_bytes(), 2);
        // Everything after the cap is counted, never stored.
        buffer.push(&[b'x'; 1000]);
        assert_eq!(buffer.as_bytes().len(), 8);
        assert_eq!(buffer.dropped_bytes(), 1002);
    }

    #[tokio::test]
    async fn drain_capped_reads_past_the_cap_without_retaining() {
        // 200 KiB source, 64 KiB cap: the drain must consume everything
        // (a flooding reviewer never blocks) while retaining only the cap.
        let source = vec![b'a'; 200 * 1024];
        let buffer = drain_capped(source.as_slice(), REVIEW_SINK_BUFFER_BYTES).await;
        assert_eq!(buffer.as_bytes().len(), REVIEW_SINK_BUFFER_BYTES);
        assert!(buffer.truncated());
        assert_eq!(
            buffer.dropped_bytes() as usize,
            source.len() - REVIEW_SINK_BUFFER_BYTES
        );
    }

    #[test]
    fn redact_for_log_scrubs_controls_and_redacts_secrets() {
        // `sk-` style keys are in the default redaction rule set.
        let input = b"token sk-abcdefghijklmnopqrstuvwx123456 end\x1b[31mred\x07";
        let (clean, summary) = redact_for_log(input);
        assert!(
            !clean.contains("sk-abcdefghijklmnopqrstuvwx123456"),
            "secret must be redacted: {clean}"
        );
        assert!(summary.matches >= 1);
        assert!(summary.bytes_redacted > 0);
        assert!(!clean.contains('\u{1b}'), "ESC must be scrubbed");
        assert!(!clean.contains('\u{07}'), "BEL must be scrubbed");
        assert!(clean.contains('\u{FFFD}'));
    }

    #[test]
    fn render_untrusted_findings_strips_ansi_sequences_entirely() {
        let input =
            "plain \u{1b}[31mred\u{1b}[0m \u{1b}]0;title\u{07}tail \u{1b}c ok\nnext\tline\u{08}";
        let rendered = render_untrusted_findings(input);
        assert_eq!(
            rendered, "plain red tail  ok\nnext\tline\u{FFFD}",
            "CSI/OSC/2-char escapes removed, newline+tab kept, stray control replaced"
        );
        // OSC terminated by ST (ESC \) as well as BEL.
        let osc_st = "a\u{1b}]8;;http://x\u{1b}\\b";
        assert_eq!(render_untrusted_findings(osc_st), "ab");
    }

    #[test]
    fn findings_section_wraps_excerpt_in_spotlighting_delimiters() {
        let section = findings_section("codex", "ok (exit code 0)", "looks good", true);
        assert!(section.starts_with("## codex — ok (exit code 0)\n"));
        assert!(section.contains(&format!(
            "{UNTRUSTED_FINDINGS_OPEN_PREFIX} slug=\"codex\">>>"
        )));
        assert!(section.contains("looks good"));
        assert!(section.contains(REVIEW_SINK_TRUNCATION_MARKER));
        assert!(section.trim_end().ends_with(UNTRUSTED_FINDINGS_CLOSE));
    }
}
