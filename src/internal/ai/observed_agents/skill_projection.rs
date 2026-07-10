//! A0-07: searchable skill-event projection + public skill-discovery registry.
//!
//! Skill events are extracted from external-agent transcripts (E7,
//! [`SkillEvent`]) and embedded, redacted, into each checkpoint's metadata
//! blob. This module provides the read side: an in-memory index that ingests
//! those events — tagged with the session/checkpoint/provider they came from —
//! and answers `libra agent skill search/list` queries by skill name,
//! provider, session, and RFC3339 time range. It also exposes the curated
//! per-agent discovery registry ([`discover_skills`]), the public
//! SkillDiscoverer surface built over [`skill_registry_for`].
//!
//! Nothing here is persisted: the checkpoint metadata blob is the durable
//! source of truth, and the projection is rebuilt on demand (read-time
//! projection, no dedicated table).

use serde::{Deserialize, Serialize};

use super::{adapter::AgentKind, capability::SkillEvent, extract::skill_registry_for};

/// Wire schema version for the skill projection / `libra agent skill` JSON
/// surface. Additive-only, like the other agent page schema versions.
pub const SKILL_PROJECTION_SCHEMA_VERSION: u32 = 1;

/// A curated skill an agent kind is known to expose — the public
/// SkillDiscoverer surface consumed by `libra agent skill` and tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredSkill {
    /// Skill invocation name (e.g. `/review`).
    pub name: String,
    /// The agent's CLI slug (e.g. `claude-code`).
    pub provider: String,
}

/// The curated skills a given agent kind is known to expose. Reads through
/// [`skill_registry_for`] so transcript extraction and discovery share one
/// fact source; non-first-batch agents expose none.
pub fn discover_skills(kind: AgentKind) -> Vec<DiscoveredSkill> {
    let provider = kind.as_cli_slug().to_string();
    skill_registry_for(kind)
        .iter()
        .map(|name| DiscoveredSkill {
            name: (*name).to_string(),
            provider: provider.clone(),
        })
        .collect()
}

/// A [`SkillEvent`] tagged with the session/checkpoint/provider context it was
/// projected from — the row shape the projection stores and `libra agent
/// skill` renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedSkillEvent {
    /// Session the event was observed in (`agent_checkpoint.session_id`).
    pub session_id: String,
    /// Checkpoint the event was embedded in, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
    /// CLI slug of the agent that produced the event.
    pub provider: String,
    /// The underlying skill event (E7 wire shape).
    pub event: SkillEvent,
}

/// A skill-search query. `None` fields do not constrain the result.
#[derive(Debug, Clone, Default)]
pub struct SkillQuery {
    /// Exact skill name (e.g. `/review`).
    pub skill: Option<String>,
    /// Provider CLI slug (matched against the row's provider AND the event
    /// source's agent, which are normally identical).
    pub provider: Option<String>,
    /// Session id.
    pub session: Option<String>,
    /// Inclusive lower bound (RFC3339). Events without a parseable timestamp
    /// are excluded once any bound is set.
    pub since: Option<String>,
    /// Inclusive upper bound (RFC3339).
    pub until: Option<String>,
}

/// An in-memory, deduplicated, searchable index of skill events. Populated by
/// the `libra agent skill` command (reading checkpoint metadata) and by the
/// projection tests; never persisted.
#[derive(Debug, Default)]
pub struct SkillEventProjection {
    events: Vec<IndexedSkillEvent>,
    /// Dedup key set — `(session_id, event.id)`. The same skill turn observed
    /// twice (e.g. a re-ingested transcript) is indexed exactly once.
    seen: std::collections::HashSet<(String, String)>,
}

impl SkillEventProjection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest the skill events extracted for one checkpoint/session. Returns
    /// the number of NEWLY-indexed events (duplicates by `(session_id,
    /// event.id)` are skipped). Insertion order is preserved so output is
    /// deterministic.
    pub fn ingest(
        &mut self,
        session_id: &str,
        checkpoint_id: Option<&str>,
        provider: &str,
        events: Vec<SkillEvent>,
    ) -> usize {
        let mut added = 0;
        for event in events {
            let key = (session_id.to_string(), event.id.clone());
            if !self.seen.insert(key) {
                continue;
            }
            self.events.push(IndexedSkillEvent {
                session_id: session_id.to_string(),
                checkpoint_id: checkpoint_id.map(str::to_string),
                provider: provider.to_string(),
                event,
            });
            added += 1;
        }
        added
    }

    /// Every indexed event, in ingestion order.
    pub fn list(&self) -> &[IndexedSkillEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Events matching every constraint in `query` (in ingestion order).
    pub fn search(&self, query: &SkillQuery) -> Vec<&IndexedSkillEvent> {
        let since = query.since.as_deref().and_then(parse_rfc3339);
        let until = query.until.as_deref().and_then(parse_rfc3339);
        self.events
            .iter()
            .filter(|row| {
                if let Some(skill) = &query.skill
                    && &row.event.skill.name != skill
                {
                    return false;
                }
                if let Some(provider) = &query.provider
                    && &row.provider != provider
                    && &row.event.source.agent != provider
                {
                    return false;
                }
                if let Some(session) = &query.session
                    && &row.session_id != session
                {
                    return false;
                }
                if since.is_some() || until.is_some() {
                    let Some(ts) = parse_rfc3339(&row.event.timestamp) else {
                        return false;
                    };
                    if let Some(lo) = since
                        && ts < lo
                    {
                        return false;
                    }
                    if let Some(hi) = until
                        && ts > hi
                    {
                        return false;
                    }
                }
                true
            })
            .collect()
    }
}

/// Parse an RFC3339 timestamp to a UTC instant for range comparison.
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::observed_agents::capability::{
        SkillEventSignal, SkillEventSource, SkillEventType, SkillRef,
    };

    fn event(id: &str, name: &str, agent: &str, ts: &str) -> SkillEvent {
        SkillEvent {
            id: id.to_string(),
            event_type: SkillEventType::PromptInvocation,
            skill: SkillRef {
                name: name.to_string(),
            },
            source: SkillEventSource {
                agent: agent.to_string(),
                signal: SkillEventSignal::InputSlashCommand,
                confidence: 1.0,
            },
            turn_id: id.split(':').next().unwrap_or(id).to_string(),
            timestamp: ts.to_string(),
            transcript_anchor: None,
            native: false,
            collapse: false,
        }
    }

    #[test]
    fn discover_skills_matches_curated_registry() {
        let claude = discover_skills(AgentKind::ClaudeCode);
        assert_eq!(
            claude.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["/review", "/security-review", "/simplify"]
        );
        assert!(claude.iter().all(|s| s.provider == "claude-code"));
        assert_eq!(discover_skills(AgentKind::Codex).len(), 1);
        assert!(discover_skills(AgentKind::Gemini).is_empty());
    }

    #[test]
    fn ingest_dedups_by_session_and_id() {
        let mut proj = SkillEventProjection::new();
        let evs = vec![event(
            "t1:/review",
            "/review",
            "codex",
            "2026-07-09T00:00:00Z",
        )];
        assert_eq!(proj.ingest("s1", Some("c1"), "codex", evs.clone()), 1);
        // Same session + same id → no new rows.
        assert_eq!(proj.ingest("s1", Some("c1"), "codex", evs.clone()), 0);
        assert_eq!(proj.len(), 1);
        // Different session → indexed separately.
        assert_eq!(proj.ingest("s2", Some("c2"), "codex", evs), 1);
        assert_eq!(proj.len(), 2);
    }

    #[test]
    fn search_filters_by_skill_provider_session_and_time() {
        let mut proj = SkillEventProjection::new();
        proj.ingest(
            "s1",
            Some("c1"),
            "claude-code",
            vec![event(
                "t1:/review",
                "/review",
                "claude-code",
                "2026-07-09T01:00:00Z",
            )],
        );
        proj.ingest(
            "s2",
            Some("c2"),
            "codex",
            vec![event(
                "t2:/simplify",
                "/simplify",
                "codex",
                "2026-07-09T03:00:00Z",
            )],
        );

        assert_eq!(
            proj.search(&SkillQuery {
                skill: Some("/review".to_string()),
                ..Default::default()
            })
            .len(),
            1
        );
        assert_eq!(
            proj.search(&SkillQuery {
                provider: Some("codex".to_string()),
                ..Default::default()
            })
            .len(),
            1
        );
        assert_eq!(
            proj.search(&SkillQuery {
                session: Some("s1".to_string()),
                ..Default::default()
            })
            .len(),
            1
        );
        // Time window that only includes the second event.
        assert_eq!(
            proj.search(&SkillQuery {
                since: Some("2026-07-09T02:00:00Z".to_string()),
                ..Default::default()
            })
            .len(),
            1
        );
        // Empty projection / no match.
        assert!(
            proj.search(&SkillQuery {
                skill: Some("/nonexistent".to_string()),
                ..Default::default()
            })
            .is_empty()
        );
    }
}
