//! Provider-native transcript sources for operator-facing capture commands.
//!
//! Hook envelopes may name arbitrary local paths, but those locators are not
//! durable authority.  Commands that need to touch a provider transcript must
//! instead derive it from the scoped session identity that the ingress has
//! already persisted: `(agent_kind, working_dir, provider_session_id)`.
//!
//! Today only Claude Code has a stable, independently verifiable on-disk
//! layout.  Keep this module deliberately narrow: adding another provider
//! requires a similarly fail-closed derivation and a descriptor-pinned open;
//! do not add a metadata-path fallback.

use std::path::{Component, PathBuf};

use anyhow::{Context, Result, bail};

use crate::internal::ai::observed_agents::{
    AgentKind, AgentSessionCtx, AuthorizedTranscriptFile, TranscriptSource, agent_for,
    resolve_session_file, resolve_transcript_source,
};

/// A provider transcript obtained from durable capture identity rather than a
/// hook-supplied locator.  `file` is already opened through the provider-root
/// no-follow seam; callers must consume that descriptor instead of reopening
/// `path` for reads.
#[derive(Debug)]
pub(super) struct DerivedTranscriptFile {
    pub(super) file: AuthorizedTranscriptFile,
}

/// Result of trying to recover a live provider transcript from durable
/// identity.  An unavailable source is ordinary (the provider may have
/// cleaned it up); an unsupported kind is explicit rather than silently
/// guessing a path convention.
#[derive(Debug)]
pub(super) enum DerivedTranscriptSource {
    Available(DerivedTranscriptFile),
    Unavailable,
    UnsupportedKind,
}

/// Resolve a provider transcript from fields that are persisted only after
/// ingress scope validation.
///
/// This deliberately does not accept `metadata_json` or a hook envelope.  A
/// stored raw pointer could target an unrelated local file after a replay or
/// catalog restore.  Claude Code is the sole supported source until each
/// additional provider has an equivalent identity-to-source proof.
pub(super) fn resolve_derived_transcript_source(
    agent_kind: &str,
    session_id: &str,
    working_dir: &str,
    provider_session_id: &str,
) -> Result<DerivedTranscriptSource> {
    let Some(kind) = AgentKind::from_db_str(agent_kind) else {
        return Ok(DerivedTranscriptSource::UnsupportedKind);
    };
    if kind != AgentKind::ClaudeCode {
        return Ok(DerivedTranscriptSource::UnsupportedKind);
    }

    let working_dir = durable_working_dir(working_dir)?;
    let Some(path) = resolve_session_file(&working_dir, provider_session_id)
        .context("derive Claude Code transcript source from captured identity")?
    else {
        return Ok(DerivedTranscriptSource::Unavailable);
    };

    // The context's path is constructed above from the provider's fixed
    // layout, never copied from a hook payload.  The common resolver then
    // opens it beneath the Claude provider root with no-follow semantics and
    // returns the held descriptor that callers must read.
    let context = AgentSessionCtx {
        session_id: session_id.to_string(),
        provider_session_id: provider_session_id.to_string(),
        working_dir,
        transcript_path: Some(path),
    };
    match resolve_transcript_source(agent_for(kind), &context)
        .context("open derived Claude Code transcript source")?
    {
        Some(TranscriptSource::File { file, .. }) => {
            Ok(DerivedTranscriptSource::Available(DerivedTranscriptFile {
                file,
            }))
        }
        // Claude Code has no export bridge.  Treat an unexpected bytes source
        // as a contract violation rather than allowing a future provider
        // change to bypass identity-to-source verification.
        Some(TranscriptSource::Bytes { .. }) => {
            bail!("derived Claude Code source unexpectedly resolved to exported bytes")
        }
        None => Ok(DerivedTranscriptSource::Unavailable),
    }
}

/// Validate the stored working directory before using it as input to a
/// provider layout derivation.  Ingress persists a canonical absolute scope;
/// this recheck keeps legacy/corrupt catalog rows from smuggling relative or
/// traversal-shaped values into a future layout implementation.
fn durable_working_dir(working_dir: &str) -> Result<PathBuf> {
    let path = PathBuf::from(working_dir);
    if working_dir.trim().is_empty()
        || !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("captured working directory is not a canonical absolute path")
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_source_rejects_noncanonical_working_directory_before_lookup() {
        let err = resolve_derived_transcript_source(
            "claude_code",
            "claude__session",
            "../untrusted",
            "session",
        )
        .expect_err("relative durable working_dir must be rejected");
        assert!(
            err.to_string().contains("canonical absolute path"),
            "error stays actionable without echoing an untrusted path: {err}"
        );
    }

    #[test]
    fn derived_source_never_falls_back_to_an_unknown_provider_layout() {
        let result = resolve_derived_transcript_source(
            "codex",
            "codex__session",
            "/workspace/repo",
            "session",
        )
        .expect("unsupported kind is a normal no-source outcome");
        assert!(matches!(result, DerivedTranscriptSource::UnsupportedKind));
    }
}
