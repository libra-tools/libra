//! Agent profile router: discovers profiles on disk and auto-selects the appropriate
//! one based on free-form user input.
//!
//! The router is the bridge between the profile authoring format ([`super::parser`])
//! and the agent runtime ([`super::super::runtime`]). The Code UI dispatches a
//! chat turn to the matched profile so the LLM sees the right system prompt and
//! tool whitelist.

use super::parser::AgentProfile;

/// Minimum number of description-keyword matches required for a profile to be considered.
///
/// Two matches is the empirical sweet spot: a single match would route generic words
/// (e.g. "test", "build") to specialized agents, while requiring three would drop too
/// many legitimate matches against short user prompts.
const MIN_MATCH_SCORE: usize = 2;
/// Hard upper bound on the size of a profile file we are willing to read from disk.
///
/// 1 MiB is far more than legitimate prompts need; the limit guards against accidental
/// or malicious large files in `.libra/agents/` from being slurped into memory.
const MAX_PROFILE_FILE_BYTES: u64 = 1024 * 1024;

/// Routes user input to the most appropriate agent profile.
///
/// Holds an immutable list of profiles. Selection is purely lexical: the description
/// of each profile is tokenized into keywords, and the user input is scored by how
/// many of those keywords appear (case-insensitively).
#[derive(Clone)]
pub struct AgentProfileRouter {
    profiles: Vec<AgentProfile>,
}

impl AgentProfileRouter {
    /// Create a new router with the given agent profiles.
    ///
    /// Functional scope: stores the list verbatim. The relative order is preserved so
    /// callers can rely on first-wins tie-breaking in [`Self::select`].
    pub fn new(profiles: Vec<AgentProfile>) -> Self {
        Self { profiles }
    }

    /// Select the best matching profile for the given user input.
    ///
    /// Functional scope:
    /// - Lower-cases the input once to avoid repeated work in the inner loop.
    /// - Computes a keyword-overlap score for every profile and keeps the highest one.
    ///
    /// Boundary conditions:
    /// - Returns `None` when no profile clears [`MIN_MATCH_SCORE`]. The caller should
    ///   then fall back to its default agent.
    /// - When two profiles tie on score the first one wins (registration order). See
    ///   `tests::test_router_tie_breaking_prefers_first`.
    pub fn select(&self, input: &str) -> Option<&AgentProfile> {
        let input_lower = input.to_lowercase();
        let mut best: Option<(&AgentProfile, usize)> = None;

        for profile in &self.profiles {
            let score = Self::match_score(&input_lower, profile);
            // Require at least 2 keyword matches to avoid false positives
            // on short or generic inputs like "test", "build", etc.
            if score >= MIN_MATCH_SCORE
                && best
                    .as_ref()
                    .is_none_or(|(_, best_score)| score > *best_score)
            {
                best = Some((profile, score));
            }
        }

        best.map(|(profile, _)| profile)
    }

    /// Get all registered profiles.
    ///
    /// Used by UI projections that render an "/agents" menu and by introspection
    /// helpers. The retired TUI was a former consumer.
    pub fn profiles(&self) -> &[AgentProfile] {
        &self.profiles
    }

    /// Get a profile by name.
    ///
    /// Returns `None` for unknown names; case-sensitive comparison matches the way
    /// names are written in the frontmatter `name:` field.
    pub fn get(&self, name: &str) -> Option<&AgentProfile> {
        self.profiles.iter().find(|a| a.name == name)
    }

    /// Get an `AgentExecutionSpec` by name.
    ///
    /// This is the OC-Phase 2 P2.2 entry point for callers that need the
    /// runtime-shaped agent contract instead of the parser-shaped
    /// [`AgentProfile`]. The two coexist deliberately:
    ///
    /// - [`Self::get`] returns the parsed profile so legacy callers (former UI
    ///   prompt assembly, slash-command surfaces) keep working unchanged.
    /// - [`Self::execution_spec`] materialises a fresh spec on every call.
    ///   Materialising on demand keeps the router cheap to construct and
    ///   avoids parallel storage that could drift out of sync with the
    ///   parsed source.
    ///
    /// Returns `None` for unknown names. Case-sensitive comparison; matches
    /// [`Self::get`] semantics.
    pub fn execution_spec(&self, name: &str) -> Option<super::spec::AgentExecutionSpec> {
        self.get(name).map(AgentProfile::to_execution_spec)
    }

    /// Calculate a match score for a profile against user input.
    ///
    /// Functional scope: counts how many keywords extracted from the profile's
    /// description appear as substrings of the lower-cased input. The substring check
    /// is intentional so that plurals and inflections still match (e.g. "reviews" hits
    /// the "review" keyword).
    fn match_score(input_lower: &str, profile: &AgentProfile) -> usize {
        let keywords = Self::extract_keywords(&profile.description);
        keywords
            .iter()
            .filter(|kw| input_lower.contains(kw.as_str()))
            .count()
    }

    /// Extract meaningful keywords from a description string.
    ///
    /// Functional scope: lower-cases the description, splits on non-alphanumeric
    /// characters, drops tokens shorter than three characters, and removes a
    /// hand-curated English stop-word list. The result is the keyword set used by
    /// [`Self::match_score`].
    ///
    /// Boundary conditions: the stop-word list is intentionally conservative — it only
    /// removes pronouns, articles, common conjunctions/prepositions, and auxiliary
    /// verbs. Domain-specific words (e.g. "code", "build", "review") are preserved
    /// because they carry the routing signal.
    fn extract_keywords(description: &str) -> Vec<String> {
        let stop_words = [
            "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has",
            "had", "do", "does", "did", "will", "would", "could", "should", "may", "might",
            "shall", "can", "for", "and", "but", "or", "nor", "not", "so", "yet", "to", "of", "in",
            "on", "at", "by", "with", "from", "up", "about", "into", "through", "during", "before",
            "after", "above", "below", "between", "use", "that", "this", "it", "its",
        ];

        description
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2 && !stop_words.contains(w))
            .map(String::from)
            .collect()
    }
}

/// Load all embedded default agent profiles.
///
/// Functional scope: parses the markdown blobs that are baked into the binary at
/// compile time via `include_str!`. Profiles that fail to parse are silently dropped
/// (the parser already emits diagnostics) so the binary still boots even if a default
/// is malformed in a future change.
///
/// Boundary conditions: the list of embedded files is hard-coded; adding a new default
/// requires editing both this function and the matching markdown file under
/// `embedded/`.
pub fn load_embedded_profiles() -> Vec<AgentProfile> {
    let sources = [
        include_str!("embedded/planner.md"),
        include_str!("embedded/code_reviewer.md"),
        include_str!("embedded/coder.md"),
        include_str!("embedded/orchestrator.md"),
        include_str!("embedded/architect.md"),
        include_str!("embedded/build_error_resolver.md"),
        include_str!("embedded/compaction.md"),
    ];

    sources
        .iter()
        .filter_map(|src| super::parser::parse_agent_profile(src))
        .collect()
}

/// Load agent profiles from a directory, with embedded profiles as fallback.
///
/// Functional scope:
/// - Walks the project-local then user-global directories, collecting profiles into
///   a single list while remembering already-seen names to enforce override priority.
/// - Adds any embedded default whose `name` was not already loaded so projects can
///   override individual defaults without re-shipping all of them.
///
/// Boundary conditions:
/// - Missing directories are not an error; they simply contribute nothing.
/// - When `dirs::config_dir()` returns `None` (no `$HOME`, sandboxed runtime, ...) the
///   user-global tier is skipped and we go straight to embedded fallback.
///
/// Lookup order (highest priority first):
/// 1. Linked-worktree overlay `.libra/agents/*.md` (W4-12 resolver)
/// 2. Repository `.libra/agents/*.md`
/// 3. `~/.config/libra/agents/*.md`
/// 4. Embedded defaults
///
/// See: `tests::test_load_profiles_with_project_override`,
/// `tests::test_load_profiles_skips_oversized_files`.
pub fn load_profiles(working_dir: &std::path::Path) -> Vec<AgentProfile> {
    let mut profiles = Vec::new();
    let mut loaded_names = std::collections::HashSet::new();

    // 1. Repository (+ overlay-wins) profiles via W4-06 resolver.
    match crate::internal::ai::sources::resolved_dir_paths(working_dir, "agents") {
        Ok((repository, overlay)) => {
            if let Some(overlay) = overlay.as_ref().filter(|path| !path.as_os_str().is_empty()) {
                load_profiles_from_dir(overlay, &mut profiles, &mut loaded_names);
            }
            if !repository.as_os_str().is_empty() {
                load_profiles_from_dir(&repository, &mut profiles, &mut loaded_names);
            }
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to resolve agent profiles; using user/embedded tiers only"
            );
        }
    }

    // 2. User-global profiles
    if let Some(config_dir) = dirs::config_dir() {
        let user_dir = config_dir.join("libra").join("agents");
        load_profiles_from_dir(&user_dir, &mut profiles, &mut loaded_names);
    }

    // 3. Embedded defaults (only for names not yet loaded)
    for profile in load_embedded_profiles() {
        if loaded_names.insert(profile.name.clone()) {
            profiles.push(profile);
        }
    }

    profiles
}

/// Helper for [`load_profiles`]: scan one directory and append fresh profiles.
///
/// Functional scope: lists the directory, filters to `.md` files, applies the size
/// limit, parses each, and appends every profile whose `name` is not already in
/// `loaded_names`.
///
/// Boundary conditions:
/// - Missing directory: the function silently returns. Callers can call it for both
///   the project and user tiers without pre-checking existence.
/// - Oversized file (`> MAX_PROFILE_FILE_BYTES`): logs a warning and skips, so a stray
///   gigabyte file does not OOM startup.
/// - Files with no extension or non-`md` extension are skipped without warning.
fn load_profiles_from_dir(
    dir: &std::path::Path,
    profiles: &mut Vec<AgentProfile>,
    loaded_names: &mut std::collections::HashSet<String>,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }

            let metadata = match path.metadata() {
                Ok(meta) => meta,
                Err(error) => {
                    tracing::warn!(path = %path.display(), error = %error, "failed to read agent file metadata");
                    continue;
                }
            };

            if metadata.len() > MAX_PROFILE_FILE_BYTES {
                tracing::warn!(
                    path = %path.display(),
                    size = metadata.len(),
                    max_bytes = MAX_PROFILE_FILE_BYTES,
                    "skipped oversized agent profile",
                );
                continue;
            }

            if let Some(profile) = super::parser::load_agent_profile_from_file(&path)
                && loaded_names.insert(profile.name.clone())
            {
                profiles.push(profile);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: a malicious or accidental large file in `.libra/agents/` is skipped
    /// while siblings of normal size still load.
    #[test]
    fn test_load_profiles_skips_oversized_files() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join(".libra").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let valid_profile = agents_dir.join("valid.md");
        std::fs::write(
            &valid_profile,
            "---\nname: valid\ndescription: Valid planner\ntools: []\nmodel: default\n---\nbody",
        )
        .unwrap();

        let mut oversized = String::from(
            "---\nname: oversized\ndescription: Oversized profile\ntools: []\nmodel: default\n---\n",
        );
        oversized.push_str(&"a".repeat((MAX_PROFILE_FILE_BYTES + 1) as usize));
        std::fs::write(agents_dir.join("oversized.md"), oversized).unwrap();

        let profiles = load_profiles(tmp.path());
        let names: Vec<_> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"valid"));
        assert!(!names.contains(&"oversized"));
    }

    /// Scenario: every embedded markdown file ships with a parseable frontmatter and
    /// the expected canonical names. Acts as a regression guard against renaming an
    /// embedded default without updating downstream lookups.
    #[test]
    fn test_load_embedded_profiles() {
        let profiles = load_embedded_profiles();
        assert_eq!(profiles.len(), 7);
        let names: Vec<&str> = profiles.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"planner"));
        assert!(names.contains(&"code_reviewer"));
        assert!(names.contains(&"coder"));
        assert!(names.contains(&"orchestrator"));
        assert!(names.contains(&"architect"));
        assert!(names.contains(&"build_error_resolver"));
        assert!(names.contains(&"compaction"));
    }

    /// Scenario: a planning-flavored prompt routes to the `planner` profile.
    #[test]
    fn test_router_select_planner() {
        let profiles = load_embedded_profiles();
        let router = AgentProfileRouter::new(profiles);

        let selected = router.select("create an intentspec planning for the new feature pipeline");
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "planner");
    }

    /// Scenario: a code-review prompt routes to the `code_reviewer` profile.
    #[test]
    fn test_router_select_reviewer() {
        let profiles = load_embedded_profiles();
        let router = AgentProfileRouter::new(profiles);

        let selected = router.select("review this code for quality and security");
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "code_reviewer");
    }

    /// Scenario: a system-design prompt routes to the `architect` profile.
    #[test]
    fn test_router_select_architect() {
        let profiles = load_embedded_profiles();
        let router = AgentProfileRouter::new(profiles);

        let selected = router.select("design the system architecture");
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "architect");
    }

    /// Scenario: a build-failure prompt routes to the `build_error_resolver` profile.
    #[test]
    fn test_router_select_build_resolver() {
        let profiles = load_embedded_profiles();
        let router = AgentProfileRouter::new(profiles);

        let selected = router.select("fix the build error compilation failure");
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "build_error_resolver");
    }

    /// Scenario: a generic greeting matches nothing and the router returns `None`,
    /// allowing the caller to fall back to the default agent.
    #[test]
    fn test_router_no_match() {
        let profiles = load_embedded_profiles();
        let router = AgentProfileRouter::new(profiles);

        let selected = router.select("hello world");
        assert!(selected.is_none());
    }

    /// Scenario: lookup by canonical name returns the profile, unknown names yield
    /// `None`.
    #[test]
    fn test_router_get_by_name() {
        let profiles = load_embedded_profiles();
        let router = AgentProfileRouter::new(profiles);

        assert!(router.get("planner").is_some());
        assert!(router.get("nonexistent").is_none());
    }

    /// Scenario: when two profiles have identical descriptions and therefore identical
    /// scores, registration order wins (first-seen, not last-seen).
    #[test]
    fn test_router_tie_breaking_prefers_first() {
        use super::super::spec::AgentMode;
        // When two profiles have the same score, the first one encountered wins
        let profiles = vec![
            AgentProfile {
                name: "agent_a".to_string(),
                description: "review code quality".to_string(),
                tools: vec![],
                permission: super::super::spec::AgentPermissionSpec::default(),
                model_preference: "default".to_string(),
                system_prompt: "A".to_string(),
                mode: AgentMode::Primary,
                model_binding: None,
                variant: None,
                temperature: None,
                top_p: None,
                max_steps: None,
            },
            AgentProfile {
                name: "agent_b".to_string(),
                description: "review code quality".to_string(),
                tools: vec![],
                permission: super::super::spec::AgentPermissionSpec::default(),
                model_preference: "default".to_string(),
                system_prompt: "B".to_string(),
                mode: AgentMode::Primary,
                model_binding: None,
                variant: None,
                temperature: None,
                top_p: None,
                max_steps: None,
            },
        ];
        let router = AgentProfileRouter::new(profiles);

        // Both profiles have identical descriptions, so same score
        let selected = router.select("review code quality");
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "agent_a");
    }

    /// Scenario: `execution_spec(name)` returns a freshly-built
    /// [`AgentExecutionSpec`] for known names and `None` for unknown ones.
    /// Calling it twice with the same name produces equal specs (the
    /// router holds the source-of-truth profile and rematerialises on
    /// every call).
    #[test]
    fn test_router_execution_spec_round_trip() {
        use super::super::spec::{AgentExecutionSpec, AgentMode, ToolSelection};

        let profile = AgentProfile {
            name: "planner".to_string(),
            description: "Implementation planner".to_string(),
            tools: vec!["read_file".to_string()],
            permission: super::super::spec::AgentPermissionSpec::default(),
            model_preference: "anthropic/claude-3-5-sonnet-latest".to_string(),
            system_prompt: "You plan.".to_string(),
            mode: AgentMode::Primary,
            model_binding: super::super::spec::ModelBinding::parse(
                "anthropic/claude-3-5-sonnet-latest",
            ),
            variant: None,
            temperature: Some(0.4),
            top_p: None,
            max_steps: Some(20),
        };
        let router = AgentProfileRouter::new(vec![profile.clone()]);

        // Unknown name: None.
        assert!(router.execution_spec("nope").is_none());

        // Known name: spec carries every parsed field.
        let spec = router.execution_spec("planner").expect("planner spec");
        let expected = AgentExecutionSpec {
            name: "planner".to_string(),
            description: "Implementation planner".to_string(),
            mode: AgentMode::Primary,
            model: super::super::spec::ModelBinding::parse("anthropic/claude-3-5-sonnet-latest"),
            system_prompt: "You plan.".to_string(),
            tools: ToolSelection::Allow(vec!["read_file".to_string()]),
            permission: super::super::spec::AgentPermissionSpec::default(),
            temperature: Some(0.4),
            top_p: None,
            max_steps: Some(20),
        };
        assert_eq!(spec, expected);

        // Re-fetching produces an equal value (the router does not own
        // the spec; it materialises one on every call).
        let spec_again = router.execution_spec("planner").unwrap();
        assert_eq!(spec, spec_again);
    }

    /// Scenario: a project-local `.libra/agents/planner.md` shadows the embedded
    /// `planner` default — verifies the three-tier override logic.
    #[test]
    fn test_load_profiles_with_project_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".libra").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("planner.md"),
            "---\nname: planner\ndescription: Custom planner\ntools: []\nmodel: fast\n---\nCustom body",
        )
        .unwrap();

        let profiles = load_profiles(tmp.path());
        let planner = profiles.iter().find(|a| a.name == "planner").unwrap();
        assert_eq!(planner.description, "Custom planner");
        assert_eq!(planner.model_preference, "fast");
    }
}
