//! Agent profile parser: markdown + YAML frontmatter → [`AgentProfile`].
//!
//! Profiles are author-friendly: a markdown file fronted by a `---` fenced YAML block
//! whose keys (`name`, `description`, `tools`, `model`) describe the agent and whose
//! body contains the system prompt. The intentionally minimal parser tolerates only the
//! shapes that the embedded defaults and project agents actually use, keeping startup
//! costs low and avoiding a YAML dependency on this hot path.
//!
//! Companion modules:
//! - [`super::router`] — discovers profile files on disk, resolves the three-tier
//!   hierarchy, and matches a profile against user input.
//! - [`super`] — re-exports the public API and pins deprecated aliases.

use std::path::Path;

use super::spec::{
    AgentExecutionSpec, AgentMode, AgentPermissionSpec, ModelBinding, ToolSelection,
};

/// A parsed agent profile from a markdown file with YAML frontmatter.
///
/// One instance corresponds to one `agents/<name>.md` file. The struct is constructed
/// only by [`parse_agent_profile`] (or callers that already validated the content) so
/// every field is guaranteed to come from a fenced frontmatter block.
///
/// OC-Phase 2 P2.1 added the optional `mode`, `variant`, `temperature`, `top_p`,
/// `max_steps`, and `model_binding` fields. The original "system prompt + tool
/// allow-list" shape is preserved verbatim, so every existing embedded profile
/// and project-local `.libra/agents/*.md` keeps round-tripping unchanged. The
/// new fields are populated only when the frontmatter explicitly carries them.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentProfile {
    /// Unique name for this agent.
    pub name: String,
    /// Human-readable description (used for auto-selection matching).
    pub description: String,
    /// List of tool names this agent is allowed to use.
    pub tools: Vec<String>,
    /// Parsed permission categories from nested `permission:` frontmatter.
    pub permission: AgentPermissionSpec,
    /// Model preference (e.g., "default", "fast", "powerful").
    ///
    /// Holds the **literal** `model:` frontmatter string when it does **not**
    /// match the `provider/model` form. Legacy values like `default` / `fast`
    /// stay here so existing slash-command surfaces continue to work; the
    /// `model_binding` field below carries the lifted `provider/model` value
    /// when the frontmatter is explicit.
    pub model_preference: String,
    /// The system prompt body (everything after the frontmatter).
    pub system_prompt: String,
    /// Whether this profile may serve as a primary agent, a sub-agent
    /// dispatched via the `task` tool, or both. Defaults to
    /// [`AgentMode::Primary`] so existing profiles do not silently appear
    /// in OC-Phase 3 sub-agent lists.
    pub mode: AgentMode,
    /// Lifted form of `model:` when the frontmatter contains `provider/model`
    /// (or `provider/model@variant`). `None` for legacy `default` / `fast`
    /// values; those keep their string form in [`Self::model_preference`].
    pub model_binding: Option<ModelBinding>,
    /// Optional reasoning / thinking variant tag, separate from the binding so
    /// it survives round-tripping through `model_preference`.
    ///
    /// When the frontmatter writes `model: anthropic/claude-opus-4@thinking`
    /// the parser stores `variant = Some("thinking")` and the lifted binding
    /// also carries the variant; when it writes `variant: thinking` on its
    /// own line that value is captured here without affecting the binding.
    /// If both forms are present the model-string form wins regardless of
    /// declaration order — the structured `provider/model@variant` shape is
    /// the more explicit source.
    pub variant: Option<String>,
    /// Per-agent sampling temperature override.
    pub temperature: Option<f32>,
    /// Per-agent top-p sampling override.
    pub top_p: Option<f32>,
    /// Per-agent maximum tool-loop steps override (parsed from `steps:`).
    pub max_steps: Option<u32>,
}

impl AgentProfile {
    /// Convert this parsed profile into an [`AgentExecutionSpec`].
    ///
    /// This is the OC-Phase 2 P2.2 entry point: the rest of the runtime
    /// (factory, dispatcher, registry pre-filter) speaks the spec dialect,
    /// while the parser keeps the legacy frontmatter shape for backward
    /// compatibility. Conversion is purely structural — there is no I/O
    /// and no defaulting beyond the rules below:
    ///
    /// - `mode`, `temperature`, `top_p`, `max_steps`, and `model_binding`
    ///   round-trip verbatim.
    /// - The top-level [`AgentProfile::variant`] field is **not** mirrored
    ///   onto the spec: `AgentExecutionSpec` carries variants only inside
    ///   [`ModelBinding::variant`]. When the parser captured a variant via
    ///   the model-string form (`model: provider/model@variant`), it lives
    ///   on the resulting `model_binding` and survives this conversion;
    ///   stand-alone `variant:` lines without a `model:` binding are
    ///   intentionally dropped here because the runtime has no place to
    ///   apply them on a non-bound provider.
    /// - `tools` becomes `ToolSelection::Allow(_)` when the parsed list
    ///   is non-empty, [`ToolSelection::Inherit`] when empty (the runtime
    ///   resolves "inherit" contextually — primary agents inherit the
    ///   session allow-list, sub-agents fall back to deny-everything per
    ///   S2-INV-05).
    /// - `permission` round-trips from nested `permission:` frontmatter.
    ///   Missing permission frontmatter keeps the default-deny
    ///   [`AgentPermissionSpec`].
    /// - `system_prompt`, `name`, and `description` round-trip verbatim
    ///   so UI projections can render the same text.
    pub fn to_execution_spec(&self) -> AgentExecutionSpec {
        AgentExecutionSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            mode: self.mode,
            model: self.model_binding.clone(),
            system_prompt: self.system_prompt.clone(),
            tools: if self.tools.is_empty() {
                ToolSelection::Inherit
            } else {
                ToolSelection::Allow(self.tools.clone())
            },
            permission: self.permission.clone(),
            temperature: self.temperature,
            top_p: self.top_p,
            max_steps: self.max_steps,
        }
    }
}

/// Parse a markdown string with YAML frontmatter into an AgentProfile.
///
/// Functional scope:
/// - Locates the opening `---` fence, finds the matching closing fence, and treats
///   everything between them as flat `key: value` lines. Anything after the closing
///   fence becomes the system prompt body.
/// - Accepts a small fixed set of keys: the legacy four (`name`, `description`,
///   `tools`, `model`) plus the OC-Phase 2 additions (`mode`, `variant`,
///   `temperature`, `top_p`, `steps`, `permission`). Unknown keys are ignored
///   so future schema additions stay forward-compatible.
/// - When `model:` carries a `provider/model[@variant]` value, the parser lifts
///   it into [`AgentProfile::model_binding`] alongside the literal string in
///   `model_preference`. Legacy aliases like `default` / `fast` / `powerful`
///   stay in `model_preference` unchanged with `model_binding = None`.
///
/// Boundary conditions:
/// - Returns `None` when the content does not start with `---`, when no closing fence
///   exists, or when the mandatory `name` field is absent. `description` defaults to
///   the empty string, `tools` to an empty list, and `model_preference` to `"default"`.
/// - The parser is intentionally simple and supports only single-line `key: value`
///   fields and array-style tool lists like `tools: ["read_file", "list_dir"]`. It
///   does not currently support multiline values or quoted values containing `:`.
/// - Numeric coercions for `temperature` / `top_p` accept any **finite**
///   `f32` (NaN and `±inf` are rejected so the resulting `AgentProfile`
///   stays `PartialEq`-reflexive). `steps` accepts any `u32`. Malformed or
///   non-finite values are silently ignored (the field stays `None`) so a
///   typo never blocks profile loading.
///
/// Expected format:
/// ```text
/// ---
/// name: planner
/// description: Implementation planning specialist...
/// tools: ["read_file", "list_dir", "grep_files"]
/// model: anthropic/claude-3-5-sonnet-latest
/// mode: primary
/// temperature: 0.5
/// permission:
///   edit: deny
///   task: allow
/// steps: 30
/// ---
///
/// You are an implementation planner...
/// ```
///
/// See: `tests::test_parse_agent_profile`, `tests::test_parse_no_frontmatter`,
/// `tests::test_parse_missing_name`.
pub fn parse_agent_profile(content: &str) -> Option<AgentProfile> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }

    // Skip the opening fence and split frontmatter / body at the first subsequent `---`.
    let after_first_fence = &content[3..];
    let end_fence = after_first_fence.find("---")?;
    let frontmatter = after_first_fence[..end_fence].trim();
    let body = after_first_fence[end_fence + 3..].trim();

    let mut name = None;
    let mut description = None;
    let mut tools = Vec::new();
    let mut model_preference = "default".to_string();
    let mut mode = AgentMode::default();
    let mut model_binding: Option<ModelBinding> = None;
    // Two separate buckets for variant so the model-string form wins
    // **regardless** of whether the `model:` line appears before or after a
    // stand-alone `variant:` line in the frontmatter (line order is not
    // controllable for the user).
    let mut variant_from_model: Option<String> = None;
    let mut variant_from_line: Option<String> = None;
    let mut temperature: Option<f32> = None;
    let mut top_p: Option<f32> = None;
    let mut max_steps: Option<u32> = None;
    let mut permission = AgentPermissionSpec::default();

    let frontmatter_lines: Vec<&str> = frontmatter.lines().collect();
    let mut index = 0usize;
    while index < frontmatter_lines.len() {
        let raw_line = frontmatter_lines[index];
        let line = raw_line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("model:") {
            // Strip surrounding whitespace, then strip an optional matching
            // pair of `"`/`'` so `model: "openai/gpt-4"` lifts the same way
            // as `model: openai/gpt-4`. An empty value is treated as absent
            // — we keep the documented "default" preference rather than
            // overwriting it with the empty string.
            let raw = unquote(val.trim()).trim().to_string();
            if !raw.is_empty() {
                // Lift `provider/model[@variant]` into a structured binding;
                // legacy aliases (`default`, `fast`, `powerful`, …) stay in
                // `model_preference` and report `model_binding = None`.
                if let Some(binding) = ModelBinding::parse(&raw) {
                    if let Some(v) = binding.variant.clone() {
                        variant_from_model = Some(v);
                    }
                    model_binding = Some(binding);
                }
                model_preference = raw;
            }
        } else if let Some(val) = line.strip_prefix("tools:") {
            tools = parse_string_list(val.trim());
        } else if let Some(val) = line.strip_prefix("mode:")
            && let Some(parsed) = parse_agent_mode(val.trim())
        {
            mode = parsed;
        } else if let Some(val) = line.strip_prefix("variant:") {
            // Trim once for outer whitespace, unquote a single layer, then
            // trim again so a quoted whitespace-only value (`variant: "  "`)
            // also resolves to "absent" rather than `Some("  ")`.
            let trimmed = unquote(val.trim()).trim();
            if !trimmed.is_empty() {
                variant_from_line = Some(trimmed.to_string());
            }
        } else if let Some(val) = line.strip_prefix("temperature:")
            && let Ok(v) = unquote(val.trim()).parse::<f32>()
            && v.is_finite()
        {
            temperature = Some(v);
        } else if let Some(val) = line.strip_prefix("top_p:")
            && let Ok(v) = unquote(val.trim()).parse::<f32>()
            && v.is_finite()
        {
            top_p = Some(v);
        } else if let Some(val) = line.strip_prefix("steps:")
            && let Ok(v) = unquote(val.trim()).parse::<u32>()
        {
            max_steps = Some(v);
        } else if is_permission_header(line) {
            let (parsed_permission, consumed) =
                parse_permission_block(&frontmatter_lines[index + 1..], leading_indent(raw_line));
            permission = parsed_permission;
            index += consumed;
        }
        index += 1;
    }

    // The structured `model: provider/model@variant` form always wins over a
    // stand-alone `variant:` line, regardless of declaration order — the
    // model string is the more explicit source.
    let variant = variant_from_model.or(variant_from_line);

    Some(AgentProfile {
        name: name?,
        description: description.unwrap_or_default(),
        tools,
        permission,
        model_preference,
        system_prompt: body.to_string(),
        mode,
        model_binding,
        variant,
        temperature,
        top_p,
        max_steps,
    })
}

/// Parse the `mode:` value into an [`AgentMode`].
///
/// Accepts the snake_case forms used by serde — `primary`, `subagent`, `all` —
/// case-insensitively. Returns `None` for anything else so the caller falls
/// back to the documented [`AgentMode::Primary`] default rather than silently
/// promoting a typo into a sub-agent role.
fn parse_agent_mode(raw: &str) -> Option<AgentMode> {
    match unquote(raw).to_ascii_lowercase().as_str() {
        "primary" => Some(AgentMode::Primary),
        "subagent" => Some(AgentMode::Subagent),
        "all" => Some(AgentMode::All),
        _ => None,
    }
}

/// Strip a single surrounding pair of `"` or `'` if present. Preserves the
/// inner text verbatim, including spaces and inner quote characters.
fn unquote(s: &str) -> &str {
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn is_permission_header(line: &str) -> bool {
    line.strip_prefix("permission:")
        .is_some_and(|value| unquote(value.trim()).trim().is_empty())
}

fn parse_permission_block(lines: &[&str], parent_indent: usize) -> (AgentPermissionSpec, usize) {
    let mut permission = AgentPermissionSpec::default();
    let mut consumed = 0usize;

    for raw_line in lines {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            consumed += 1;
            continue;
        }
        if leading_indent(raw_line) <= parent_indent {
            break;
        }
        consumed += 1;

        let Some((permission_key, policy)) = trimmed.split_once(':') else {
            continue;
        };
        apply_permission_rule(&mut permission, permission_key, policy);
    }

    (permission, consumed)
}

fn apply_permission_rule(permission: &mut AgentPermissionSpec, key: &str, policy: &str) {
    let key = normalize_permission_key(unquote(key.trim()).trim());
    if key.is_empty() {
        return;
    }
    match unquote(policy.trim()).trim().to_ascii_lowercase().as_str() {
        "allow" | "ask" => {
            permission.allowed_tools.insert(key);
        }
        "deny" => {
            permission.denied_tools.insert(key);
        }
        _ => {}
    }
}

fn normalize_permission_key(permission: &str) -> String {
    match permission {
        "write" => "edit".to_string(),
        "bash" => "shell".to_string(),
        other => other.to_string(),
    }
}

fn leading_indent(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .count()
}

/// Load an agent profile from a file path.
///
/// Functional scope: reads `path` synchronously and forwards the contents to
/// [`parse_agent_profile`]. On any IO or parse failure the function returns `None` and
/// emits a `tracing::warn!` so misconfigured files do not abort the whole router but
/// still surface in operator logs.
///
/// Boundary conditions:
/// - File-not-found, permission errors, and any other `std::io::Error` are downgraded
///   to a warning and `None`.
/// - A successfully read file that lacks the `---` frontmatter or omits the `name`
///   field is logged separately so the operator can distinguish IO problems from
///   schema problems.
pub fn load_agent_profile_from_file(path: &Path) -> Option<AgentProfile> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read agent file");
            return None;
        }
    };
    let result = parse_agent_profile(&content);
    if result.is_none() {
        tracing::warn!(path = %path.display(), "failed to parse agent profile");
    }
    result
}

/// Backward compatible type name for legacy callers.
#[deprecated(note = "Use AgentProfile instead.")]
pub type AgentDefinition = AgentProfile;

/// Backward compatible parser function name.
#[deprecated(note = "Use parse_agent_profile instead.")]
pub fn parse_agent_definition(content: &str) -> Option<AgentProfile> {
    parse_agent_profile(content)
}

/// Backward compatible loader name.
#[deprecated(note = "Use load_agent_profile_from_file instead.")]
pub fn load_agent_from_file(path: &Path) -> Option<AgentProfile> {
    load_agent_profile_from_file(path)
}

/// Parse a YAML-style string list: `["a", "b", "c"]` → `Vec<String>`.
///
/// Functional scope: strips optional `[` / `]` brackets, splits on commas, and trims
/// surrounding whitespace as well as one layer of single or double quotes around each
/// element.
///
/// Boundary conditions:
/// - Empty list (`[]`) yields an empty `Vec`.
/// - A bare comma-separated string without brackets is also accepted, which keeps the
///   parser permissive for hand-written profiles.
/// - Items that become empty after trimming are filtered out so a stray trailing comma
///   does not introduce a phantom tool name.
fn parse_string_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let s = s.strip_prefix('[').unwrap_or(s);
    let s = s.strip_suffix(']').unwrap_or(s);
    s.split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_AGENT: &str = r#"---
name: planner
description: Implementation planning specialist
tools: ["read_file", "list_dir", "grep_files"]
model: default
---

You are an implementation planner.

## Planning Process

1. Understand requirements
2. Explore codebase
"#;

    /// Scenario: a complete profile with all four legacy frontmatter keys parses
    /// round-trip. The new OC-Phase 2 fields stay at their documented defaults
    /// (`mode = Primary`, no model binding, no overrides) so existing profiles
    /// do not silently appear in sub-agent lists or get a phantom binding.
    #[test]
    fn test_parse_agent_profile() {
        let def = parse_agent_profile(SAMPLE_AGENT).unwrap();
        assert_eq!(def.name, "planner");
        assert_eq!(def.description, "Implementation planning specialist");
        assert_eq!(def.tools, vec!["read_file", "list_dir", "grep_files"]);
        assert_eq!(def.model_preference, "default");
        assert!(def.system_prompt.contains("implementation planner"));
        // OC-Phase 2 P2.1 defaults — preserved compatibility.
        assert_eq!(def.mode, AgentMode::Primary);
        assert!(def.model_binding.is_none());
        assert!(def.variant.is_none());
        assert!(def.temperature.is_none());
        assert!(def.top_p.is_none());
        assert!(def.max_steps.is_none());
    }

    /// Scenario: a markdown blob with no `---` fence is rejected with `None` rather
    /// than silently producing an empty profile.
    #[test]
    fn test_parse_no_frontmatter() {
        assert!(parse_agent_profile("No frontmatter here").is_none());
    }

    /// Scenario: the mandatory `name:` field is missing — parser must return `None`
    /// instead of relying on a default.
    #[test]
    fn test_parse_missing_name() {
        let content = "---\ndescription: test\n---\nbody";
        assert!(parse_agent_profile(content).is_none());
    }

    /// Scenario: bracket/quote stripping behaves correctly, including for empty lists
    /// and single-element lists.
    #[test]
    fn test_parse_string_list() {
        assert_eq!(parse_string_list(r#"["a", "b", "c"]"#), vec!["a", "b", "c"]);
        assert_eq!(parse_string_list("[]"), Vec::<String>::new());
        assert_eq!(parse_string_list(r#"["single"]"#), vec!["single"]);
    }

    /// Scenario: every embedded default profile (six today) ships inside the
    /// binary and must remain parseable with this minimal grammar. None of
    /// them opt into the OC-Phase 2 `mode:` or `model: provider/model` form,
    /// so `mode` must default to `Primary` and `model_binding` must be
    /// `None` for each. (One profile, `orchestrator.md`, sets a numeric
    /// `temperature` — that is allowed and still leaves the binding-related
    /// defaults alone.)
    #[test]
    fn test_parse_embedded_agents() {
        for src in [
            include_str!("embedded/planner.md"),
            include_str!("embedded/code_reviewer.md"),
            include_str!("embedded/architect.md"),
            include_str!("embedded/build_error_resolver.md"),
            include_str!("embedded/coder.md"),
            include_str!("embedded/orchestrator.md"),
        ] {
            let def = parse_agent_profile(src).expect("embedded profile must parse");
            assert!(!def.name.is_empty(), "embedded profile must declare a name");
            assert_eq!(
                def.mode,
                AgentMode::Primary,
                "embedded profile {} silently became a sub-agent",
                def.name
            );
            assert!(
                def.model_binding.is_none(),
                "embedded profile {} silently lifted a model binding",
                def.name
            );
        }
    }

    /// Scenario: a `model: provider/model` value is lifted into a structured
    /// `ModelBinding` while the literal string stays in `model_preference`
    /// for backward compatibility with anything that reads the legacy field.
    #[test]
    fn test_parse_lifts_provider_slash_model_into_binding() {
        let content = "---\nname: planner\nmodel: anthropic/claude-3-5-sonnet-latest\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        assert_eq!(def.model_preference, "anthropic/claude-3-5-sonnet-latest");
        let binding = def.model_binding.expect("provider/model must lift");
        assert_eq!(binding.provider_id, "anthropic");
        assert_eq!(binding.model_id, "claude-3-5-sonnet-latest");
        assert!(binding.variant.is_none());
        // The frontmatter did not write `variant:` either, so the
        // top-level field stays None.
        assert!(def.variant.is_none());
    }

    /// Scenario: `model: provider/model@variant` populates both the binding
    /// (with the variant qualifier) and the top-level `variant` field so
    /// downstream code can read either.
    #[test]
    fn test_parse_lifts_model_with_variant_qualifier() {
        let content = "---\nname: planner\nmodel: anthropic/claude-opus-4@thinking\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        let binding = def.model_binding.expect("binding lifts");
        assert_eq!(binding.provider_id, "anthropic");
        assert_eq!(binding.model_id, "claude-opus-4");
        assert_eq!(binding.variant.as_deref(), Some("thinking"));
        assert_eq!(def.variant.as_deref(), Some("thinking"));
    }

    /// Scenario: legacy aliases like `default` / `fast` / `powerful` do **not**
    /// lift into a binding. They stay in `model_preference` so the existing
    /// CLI / UI surfaces that look up a default model by alias keep working.
    #[test]
    fn test_parse_keeps_legacy_model_aliases_unstructured() {
        for alias in ["default", "fast", "powerful"] {
            let content = format!("---\nname: planner\nmodel: {alias}\n---\nbody");
            let def = parse_agent_profile(&content).unwrap();
            assert_eq!(def.model_preference, alias);
            assert!(
                def.model_binding.is_none(),
                "alias `{alias}` must not produce a structured binding"
            );
        }
    }

    /// Scenario: `mode:` accepts the three documented values case-insensitively.
    /// Any other value is silently ignored so a typo falls back to the
    /// documented `Primary` default rather than producing a malformed profile.
    #[test]
    fn test_parse_mode_field_round_trips_and_falls_back() {
        for (raw, expected) in [
            ("primary", AgentMode::Primary),
            ("Primary", AgentMode::Primary),
            ("subagent", AgentMode::Subagent),
            ("all", AgentMode::All),
        ] {
            let content = format!("---\nname: planner\nmode: {raw}\n---\nbody");
            let def = parse_agent_profile(&content).unwrap();
            assert_eq!(def.mode, expected, "mode `{raw}` parsed wrong");
        }
        // Typo falls back to default (Primary), not Subagent.
        let typo = "---\nname: planner\nmode: sub_agent\n---\nbody";
        let def = parse_agent_profile(typo).unwrap();
        assert_eq!(def.mode, AgentMode::Primary);
    }

    /// Scenario: numeric overrides (`temperature`, `top_p`, `steps`) parse to
    /// the right primitive types. A malformed numeric value silently leaves
    /// the field at `None` (default) rather than blocking profile loading.
    #[test]
    fn test_parse_numeric_overrides_and_malformed_fallback() {
        let ok = "---\n\
                  name: planner\n\
                  temperature: 0.5\n\
                  top_p: 0.95\n\
                  steps: 30\n\
                  ---\nbody";
        let def = parse_agent_profile(ok).unwrap();
        assert_eq!(def.temperature, Some(0.5));
        assert_eq!(def.top_p, Some(0.95));
        assert_eq!(def.max_steps, Some(30));

        let malformed = "---\n\
                         name: planner\n\
                         temperature: warm\n\
                         top_p: 1.0e3warm\n\
                         steps: -5\n\
                         ---\nbody";
        let def = parse_agent_profile(malformed).unwrap();
        assert!(def.temperature.is_none());
        assert!(def.top_p.is_none());
        assert!(def.max_steps.is_none());
    }

    /// Scenario: a stand-alone `variant:` line populates the top-level field.
    /// When `model:` already supplied a variant, the model-string form wins
    /// (it is the more explicit source for the binding) but a stand-alone
    /// line still works when `model:` did not.
    #[test]
    fn test_parse_variant_field_standalone_and_with_model_binding() {
        let standalone = "---\nname: planner\nvariant: thinking\n---\nbody";
        let def = parse_agent_profile(standalone).unwrap();
        assert_eq!(def.variant.as_deref(), Some("thinking"));
        assert!(def.model_binding.is_none());

        let from_model = "---\nname: planner\nmodel: anthropic/claude-opus-4@thinking\n---\nbody";
        let def = parse_agent_profile(from_model).unwrap();
        assert_eq!(def.variant.as_deref(), Some("thinking"));
        assert_eq!(
            def.model_binding
                .as_ref()
                .and_then(|b| b.variant.as_deref()),
            Some("thinking")
        );

        // Quoted variant value strips a single layer of quotes so YAML-style
        // quoting in handwritten profiles round-trips cleanly.
        let quoted = "---\nname: planner\nvariant: \"thinking\"\n---\nbody";
        let def = parse_agent_profile(quoted).unwrap();
        assert_eq!(def.variant.as_deref(), Some("thinking"));
    }

    /// Scenario: `unquote` strips a single matching pair of `"` or `'` and
    /// leaves unmatched / mixed quotes intact. Empty strings round-trip.
    #[test]
    fn test_unquote_handles_pairs_and_mismatches() {
        assert_eq!(unquote("\"foo\""), "foo");
        assert_eq!(unquote("'foo'"), "foo");
        // Mismatched pair: returned as-is.
        assert_eq!(unquote("\"foo'"), "\"foo'");
        // Empty / single-char inputs do not panic.
        assert_eq!(unquote(""), "");
        assert_eq!(unquote("\""), "\"");
    }

    /// Scenario: a quoted `model:` value (`model: "openai/gpt-4o"`) lifts the
    /// same way the unquoted form does. A regression here would mean the
    /// parser produced provider id `"openai` (with leading `"`) and model
    /// id `gpt-4o"` (with trailing `"`).
    #[test]
    fn test_parse_model_with_quotes_lifts_correctly() {
        for raw in [
            "\"openai/gpt-4o\"",
            "'openai/gpt-4o'",
            "  openai/gpt-4o  ",
            "\"  openai/gpt-4o  \"",
        ] {
            let content = format!("---\nname: planner\nmodel: {raw}\n---\nbody");
            let def = parse_agent_profile(&content).unwrap();
            let binding = def
                .model_binding
                .unwrap_or_else(|| panic!("expected lifted binding for model frontmatter `{raw}`"));
            assert_eq!(binding.provider_id, "openai");
            assert_eq!(binding.model_id, "gpt-4o");
        }
    }

    /// Scenario: whitespace around the `/` separator is tolerated;
    /// `model: openai / gpt-4o` lifts to `("openai", "gpt-4o")` rather than
    /// keeping the spaces inside the ids.
    #[test]
    fn test_parse_model_tolerates_whitespace_around_slash() {
        let content = "---\nname: planner\nmodel: openai / gpt-4o\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        let binding = def.model_binding.unwrap();
        assert_eq!(binding.provider_id, "openai");
        assert_eq!(binding.model_id, "gpt-4o");
    }

    /// Scenario: an empty `model:` value does NOT clobber the documented
    /// "default" preference. A user who writes a stray `model: ` line
    /// expects the same behaviour as omitting the field entirely.
    #[test]
    fn test_parse_empty_model_preserves_default() {
        for raw in ["", "  ", "\"\"", "'  '"] {
            let content = format!("---\nname: planner\nmodel:{raw}\n---\nbody");
            let def = parse_agent_profile(&content).unwrap();
            assert_eq!(
                def.model_preference, "default",
                "empty model `{raw:?}` clobbered preference"
            );
            assert!(def.model_binding.is_none());
        }
    }

    /// Scenario: an empty `@variant` qualifier (`provider/model@`) is rejected
    /// at the binding layer; the parser surfaces no binding and keeps the
    /// raw string in `model_preference`.
    #[test]
    fn test_parse_model_with_empty_variant_qualifier_rejected() {
        let content = "---\nname: planner\nmodel: anthropic/claude@\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        assert!(
            def.model_binding.is_none(),
            "trailing `@` must not produce a binding"
        );
        assert_eq!(def.model_preference, "anthropic/claude@");
    }

    /// Scenario: a stand-alone `variant:` line MUST NOT overwrite a variant
    /// that came in via `model: provider/model@x`, regardless of which line
    /// appears first. The model-string form is the more explicit source.
    #[test]
    fn test_parse_variant_precedence_is_order_independent() {
        // variant: line BEFORE model: line.
        let before = "---\n\
                      name: planner\n\
                      variant: standalone\n\
                      model: anthropic/claude-opus-4@thinking\n\
                      ---\nbody";
        let def_before = parse_agent_profile(before).unwrap();
        assert_eq!(def_before.variant.as_deref(), Some("thinking"));

        // variant: line AFTER model: line.
        let after = "---\n\
                     name: planner\n\
                     model: anthropic/claude-opus-4@thinking\n\
                     variant: standalone\n\
                     ---\nbody";
        let def_after = parse_agent_profile(after).unwrap();
        assert_eq!(def_after.variant.as_deref(), Some("thinking"));
    }

    /// Scenario: a quoted whitespace-only `variant:` value resolves to absent
    /// (`None`) instead of `Some("   ")`.
    #[test]
    fn test_parse_quoted_whitespace_variant_resolves_to_absent() {
        let content = "---\nname: planner\nvariant: \"   \"\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        assert!(def.variant.is_none());
    }

    /// Scenario: non-finite `f32` values (`NaN`, `Infinity`) for temperature
    /// or top_p are rejected at parse time. Storing them would make
    /// `PartialEq` non-reflexive on the resulting `AgentProfile`.
    #[test]
    fn test_parse_rejects_non_finite_floats() {
        let content = "---\n\
                       name: planner\n\
                       temperature: NaN\n\
                       top_p: inf\n\
                       ---\nbody";
        let def = parse_agent_profile(content).unwrap();
        assert!(def.temperature.is_none(), "NaN must be rejected");
        assert!(def.top_p.is_none(), "infinity must be rejected");
        // The resulting profile must equal itself (PartialEq reflexive).
        assert_eq!(def, def.clone());
    }

    /// Scenario: a fully-populated profile lifts into an `AgentExecutionSpec`
    /// preserving every field. Tools convert to `ToolSelection::Allow(_)`
    /// when non-empty; `permission` stays at default-deny because the
    /// frontmatter does not yet carry structured rules.
    #[test]
    fn test_to_execution_spec_full_profile() {
        let content = "---\n\
                       name: planner\n\
                       description: Implementation planning specialist\n\
                       tools: [\"read_file\", \"list_dir\"]\n\
                       model: anthropic/claude-3-5-sonnet-latest\n\
                       mode: primary\n\
                       temperature: 0.5\n\
                       top_p: 0.95\n\
                       steps: 30\n\
                       ---\n\
                       You are a planner.";
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();
        assert_eq!(spec.name, "planner");
        assert_eq!(spec.description, "Implementation planning specialist");
        assert_eq!(spec.mode, AgentMode::Primary);
        let binding = spec.model.expect("model binding");
        assert_eq!(binding.provider_id, "anthropic");
        assert_eq!(binding.model_id, "claude-3-5-sonnet-latest");
        assert_eq!(spec.system_prompt, "You are a planner.");
        match &spec.tools {
            ToolSelection::Allow(tools) => {
                assert_eq!(
                    tools,
                    &vec!["read_file".to_string(), "list_dir".to_string()]
                );
            }
            other => panic!("expected Allow with two tools, got {other:?}"),
        }
        // Permission stays at default; OC-Phase 2 P2.5 wires the real shape.
        assert_eq!(spec.permission, AgentPermissionSpec::default());
        assert_eq!(spec.temperature, Some(0.5));
        assert_eq!(spec.top_p, Some(0.95));
        assert_eq!(spec.max_steps, Some(30));
    }

    /// Scenario: a profile with an empty `tools:` list lifts to
    /// `ToolSelection::Inherit`, NOT `Allow(vec![])`. The runtime resolves
    /// "inherit" contextually so a primary agent keeps the session allow-
    /// list and a sub-agent falls back to deny-everything.
    #[test]
    fn test_to_execution_spec_empty_tools_becomes_inherit() {
        let content = "---\nname: explorer\ntools: []\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();
        assert_eq!(spec.tools, ToolSelection::Inherit);
    }

    /// Scenario: a profile with a legacy `model: default` produces a spec
    /// whose `model` field is `None` (no binding). OC-Phase 1 P1.3 still
    /// resolves the default model name from the legacy `model_preference`
    /// string via the CLI / UI surface; the spec just records "no
    /// structured binding asked for".
    #[test]
    fn test_to_execution_spec_legacy_alias_carries_no_binding() {
        let content = "---\nname: planner\nmodel: default\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();
        assert!(spec.model.is_none());
    }

    #[test]
    fn test_to_execution_spec_legacy_fast_alias_carries_no_binding() {
        let content = "---\nname: planner\nmodel: fast\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();
        assert_eq!(def.model_preference, "fast");
        assert!(spec.model.is_none());
    }

    /// Scenario: the opencode plan's Markdown profile fixture carries
    /// nested `permission:` frontmatter. The parser must lift those rules
    /// into `AgentExecutionSpec.permission` using the same category
    /// normalization as TOML config (`write -> edit`, `bash -> shell`).
    #[test]
    fn test_to_execution_spec_lifts_permission_frontmatter() {
        let content = concat!(
            "---\n",
            "name: planner\n",
            "mode: primary\n",
            "model: anthropic/claude-3-5-sonnet-latest\n",
            "temperature: 0.5\n",
            "steps: 30\n",
            "permission:\n",
            "  edit: deny\n",
            "  task: allow\n",
            "  write: deny\n",
            "  bash: allow\n",
            "---\n",
            "You are a planner.",
        );
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();

        assert!(spec.permission.denied_tools.contains("edit"));
        assert!(spec.permission.allowed_tools.contains("task"));
        assert!(spec.permission.allowed_tools.contains("shell"));
        assert!(!spec.permission.denied_tools.contains("write"));
        assert!(!spec.permission.allowed_tools.contains("bash"));
        assert_eq!(spec.max_steps, Some(30));
        assert_eq!(spec.temperature, Some(0.5));
        assert!(spec.model.is_some());
    }

    #[test]
    fn test_to_execution_spec_permission_frontmatter_denies_override_alias_allows() {
        let content = concat!(
            "---\n",
            "name: planner\n",
            "permission:\n",
            "  write: deny\n",
            "  edit: allow\n",
            "---\n",
            "You are a planner.",
        );
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();

        assert!(spec.permission.allowed_tools.contains("edit"));
        assert!(spec.permission.denied_tools.contains("edit"));
        assert!(!spec.permission.permits_tool("edit"));
    }

    /// Scenario: a profile that omits everything optional lifts to a spec
    /// with `mode = Primary`, `tools = Inherit`, and every optional field
    /// at `None`. This is the shape the Code UI sees for any embedded
    /// profile that does not opt into OC-Phase 2 frontmatter keys.
    #[test]
    fn test_to_execution_spec_minimal_profile_uses_documented_defaults() {
        let content = "---\nname: planner\n---\nbody";
        let def = parse_agent_profile(content).unwrap();
        let spec = def.to_execution_spec();
        assert_eq!(spec.mode, AgentMode::Primary);
        assert_eq!(spec.tools, ToolSelection::Inherit);
        assert!(spec.model.is_none());
        assert!(spec.temperature.is_none());
        assert!(spec.top_p.is_none());
        assert!(spec.max_steps.is_none());
    }

    /// Scenario: every documented `mode:` value round-trips through
    /// `to_execution_spec()` so the conversion is not silently hard-coded
    /// to `Primary`. Without this test a regression that overwrote
    /// `self.mode` with `AgentMode::Primary` would still pass every other
    /// converter test (the remaining cases all use the default mode).
    #[test]
    fn test_to_execution_spec_preserves_every_mode() {
        for (raw, expected) in [
            ("primary", AgentMode::Primary),
            ("subagent", AgentMode::Subagent),
            ("all", AgentMode::All),
        ] {
            let content = format!("---\nname: planner\nmode: {raw}\n---\nbody");
            let def = parse_agent_profile(&content).unwrap();
            assert_eq!(def.to_execution_spec().mode, expected);
        }
    }

    /// Scenario: a `model: provider/model@variant` value lifts into the
    /// spec's `model_binding` with the variant intact, while a stand-alone
    /// `variant:` line with no `model:` binding does NOT surface anywhere
    /// in the resulting spec (the spec has no top-level `variant` field).
    /// This pins the doc-comment claim about variant handling.
    #[test]
    fn test_to_execution_spec_variant_only_via_model_binding() {
        let with_binding = "---\nname: planner\nmodel: anthropic/claude-opus-4@thinking\n---\nbody";
        let def = parse_agent_profile(with_binding).unwrap();
        let spec = def.to_execution_spec();
        let binding = spec.model.expect("variant binding lifts");
        assert_eq!(binding.variant.as_deref(), Some("thinking"));

        let standalone = "---\nname: planner\nvariant: thinking\n---\nbody";
        let def = parse_agent_profile(standalone).unwrap();
        let spec = def.to_execution_spec();
        // No model binding means the variant has nowhere to land on the
        // spec — it stays only on the parser-level AgentProfile.
        assert!(spec.model.is_none());
    }
}
