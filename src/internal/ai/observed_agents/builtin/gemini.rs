//! Gemini [`ObservedAgent`] adapter.
//!
//! Pairs with the existing Gemini `HookProvider` (see
//! `src/internal/ai/hooks/providers/gemini.rs`): the hook layer owns
//! `LifecycleEvent` parsing and install/uninstall of the
//! `~/.gemini/settings.json` shell-hooks, while this adapter owns the
//! `ObservedAgent` surface — transcript ingestion plus the
//! [`ObservedAgent::protected_dirs`] manifest that `rewind` / `clean`
//! consume to leave Gemini's session storage alone.
//!
//! `docs/development/commands/_general.md` §16.1 lists `builtin/gemini.rs` as a
//! required v1 file, and `builtin/stable_promoted.rs::promoted_specs_cover_every_v1_preview_kind`
//! asserts that `AgentKind::Gemini` is a "dedicated stable" type rather
//! than a row in `STABLE_PROMOTED_SPECS`. Until v0.17.672 the codebase
//! satisfied the stable-promoted exclusion but never landed the
//! dedicated type the assertion implies — this module closes that gap.
//!
//! `TranscriptTruncator` is intentionally not implemented here: per
//! `docs/development/commands/_general.md` §7.3, v1 leaves the agent's transcript
//! file untouched on `rewind --apply` and prints a warning. Adding the
//! truncator is gated on understanding Gemini's session JSONL schema
//! and is tracked alongside the Claude Code truncator's evolution.

use std::{
    fs,
    io::{self},
    path::Path,
};

use anyhow::{Context, Result, anyhow};

use super::super::adapter::{AgentKind, AgentSessionCtx, ObservedAgent};

/// Hard cap on how many bytes the transcript reader will pull off disk.
/// Mirrors the Claude Code adapter's 16 MiB ceiling: Gemini transcripts
/// also grow with conversation length but sit well under a few MB in
/// practice. The cap protects against a runaway file or a malformed
/// path that points at a giant binary.
const MAX_TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;

/// Stable adapter for Gemini (`AgentKind::Gemini`).
///
/// Held as a unit struct so the registry can hand out a
/// `&'static dyn ObservedAgent` without lifetime gymnastics; identical
/// shape to [`super::claude_code::ClaudeCodeObservedAgent`].
#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiObservedAgent;

impl GeminiObservedAgent {
    pub const fn new() -> Self {
        Self
    }
}

impl ObservedAgent for GeminiObservedAgent {
    fn provider_kind(&self) -> AgentKind {
        AgentKind::Gemini
    }

    fn provider_name(&self) -> &'static str {
        "gemini"
    }

    /// Read the raw transcript bytes from `session.transcript_path`.
    ///
    /// Returns `Ok(None)` when the path is absent (caller hasn't
    /// pointed at a transcript yet) or when the file is missing
    /// (session that never produced output); returns an actionable
    /// error when the file exceeds `MAX_TRANSCRIPT_BYTES` or the
    /// `stat`/`read` system calls fail with anything other than
    /// `NotFound`. Empty files round-trip as `Ok(Some(Vec::new()))` so
    /// the caller can distinguish "empty transcript" from "no
    /// transcript path configured".
    ///
    /// The returned bytes are **not yet redacted** — the
    /// `super::super::redaction::Redactor` layer is the only sanctioned
    /// path into persistence storage, per `entire.md` §13 Risk #1.
    fn read_transcript(&self, session: &AgentSessionCtx) -> Result<Option<Vec<u8>>> {
        let Some(path) = session.transcript_path.as_ref() else {
            return Ok(None);
        };
        read_transcript_capped(path, MAX_TRANSCRIPT_BYTES)
    }

    /// `~/.gemini/` holds Gemini's session storage and shell-hooks
    /// config. `rewind` / `clean` must not delete or rewrite these
    /// paths even when the AI worktree strategy says "scrub everything
    /// outside the writable roots".
    fn protected_dirs(&self) -> &'static [&'static str] {
        &[".gemini"]
    }
}

fn read_transcript_capped(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    match fs::metadata(path) {
        Ok(meta) if meta.len() == 0 => Ok(Some(Vec::new())),
        Ok(meta) if meta.len() > max_bytes => Err(anyhow!(
            "transcript at {} exceeds {} byte cap; refusing to load",
            path.display(),
            max_bytes
        )),
        Ok(_) => {
            let bytes =
                fs::read(path).with_context(|| format!("read transcript {}", path.display()))?;
            Ok(Some(bytes))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(anyhow!(err)).with_context(|| format!("stat transcript {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, path::PathBuf};

    use tempfile::tempdir;

    use super::{
        super::super::adapter::{AgentKind, AgentSessionCtx, AgentStability},
        *,
    };

    fn ctx_with_transcript(path: Option<PathBuf>) -> AgentSessionCtx {
        AgentSessionCtx {
            session_id: "test-session-id".to_string(),
            provider_session_id: "gemini-session-id".to_string(),
            working_dir: PathBuf::from("/tmp/libra-gemini-test"),
            transcript_path: path,
        }
    }

    /// Pin the static identity surface: `AgentKind::Gemini`, the
    /// `gemini` provider name, the `.gemini` protected directory, and
    /// the `Stable` tier (so `is_preview` continues to return false
    /// for this kind even with no preview spec).
    #[test]
    fn gemini_observed_agent_reports_stable_identity_and_protected_dir() {
        let agent = GeminiObservedAgent::new();
        assert_eq!(agent.provider_kind(), AgentKind::Gemini);
        assert_eq!(agent.provider_name(), "gemini");
        assert_eq!(agent.protected_dirs(), &[".gemini"]);
        assert_eq!(agent.stability(), AgentStability::Stable);
    }

    /// `read_transcript` returns `Ok(None)` when no transcript path is
    /// configured on the session context. Mirrors the Claude Code
    /// adapter's contract so the "no transcript yet" branch is the
    /// same shape across stable adapters.
    #[test]
    fn read_transcript_returns_none_when_no_path_configured() {
        let agent = GeminiObservedAgent::new();
        let ctx = ctx_with_transcript(None);
        let result = agent.read_transcript(&ctx).expect("read must not fail");
        assert!(result.is_none());
    }

    /// `read_transcript` returns `Ok(None)` when the configured path
    /// does not exist on disk (session that pointed at a file the
    /// agent never created). Distinct from "empty file" (next test)
    /// because the caller pipeline branches on the `Option`.
    #[test]
    fn read_transcript_returns_none_when_path_missing() {
        let agent = GeminiObservedAgent::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("never-created.jsonl");
        let ctx = ctx_with_transcript(Some(path));
        let result = agent.read_transcript(&ctx).expect("read must not fail");
        assert!(result.is_none());
    }

    /// `read_transcript` returns `Ok(Some(<bytes>))` for non-empty
    /// files. Empty-file round-trip is exercised separately.
    #[test]
    fn read_transcript_returns_bytes_for_existing_file() {
        let agent = GeminiObservedAgent::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let payload = b"{\"role\":\"user\",\"text\":\"hello\"}\n";
        fs::write(&path, payload).unwrap();
        let ctx = ctx_with_transcript(Some(path));
        let result = agent
            .read_transcript(&ctx)
            .expect("read must succeed")
            .expect("payload must be Some");
        assert_eq!(result, payload);
    }

    /// `read_transcript` round-trips empty files as `Ok(Some(Vec::new()))`
    /// — distinct from the "no path" / "missing path" branches above so
    /// downstream code can tell "agent created an empty transcript" from
    /// "no transcript file is around".
    #[test]
    fn read_transcript_returns_empty_vec_for_empty_file() {
        let agent = GeminiObservedAgent::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::File::create(&path).unwrap().flush().unwrap();
        let ctx = ctx_with_transcript(Some(path));
        let result = agent
            .read_transcript(&ctx)
            .expect("read must succeed")
            .expect("payload must be Some");
        assert!(result.is_empty());
    }

    /// `read_transcript` refuses to load files larger than the
    /// `MAX_TRANSCRIPT_BYTES` cap, surfacing a clear actionable error
    /// rather than silently materialising a huge buffer. Uses the
    /// `read_transcript_capped` free function with a deliberately tiny
    /// cap so the test stays fast and deterministic.
    #[test]
    fn read_transcript_capped_rejects_oversized_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.jsonl");
        fs::write(&path, vec![b'x'; 1024]).unwrap();
        let err = read_transcript_capped(&path, 64).expect_err("must reject oversized file");
        let message = format!("{err:#}");
        assert!(
            message.contains("exceeds 64 byte cap"),
            "unexpected error message: {message}",
        );
    }
}
