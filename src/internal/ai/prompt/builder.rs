//! System prompt builder that composes rules into a final prompt string.

use std::path::{Path, PathBuf};

use super::{
    context::ContextMode,
    dynamic_context::build_dynamic_prompt_section_with_budget,
    loader::load_all_rules,
    rules::{RuleCategory, RuleFile},
};
use crate::internal::ai::{
    agent::TaskIntent,
    context_budget::{
        AuditedMemoryContextBundleV1, ContextBudget, MemoryAnchor,
        build_memory_anchor_prompt_section,
    },
};

/// Builds a complete system prompt from modular rule files.
///
/// Rules are loaded from a three-tier hierarchy (project-local > user-global > embedded)
/// and composed in a fixed order. The `{working_dir}` placeholder in rule content is
/// substituted with the actual working directory path.
pub struct SystemPromptBuilder {
    working_dir: PathBuf,
    rules: Vec<RuleFile>,
    context: Option<ContextMode>,
    intent: Option<TaskIntent>,
    dynamic_context: bool,
    context_budget: Option<ContextBudget>,
    memory_anchors: Vec<MemoryAnchor>,
    audited_memory_context: Option<String>,
    extra_sections: Vec<(String, String)>,
}

impl SystemPromptBuilder {
    /// Create a new builder that loads all default rules for the given working directory.
    ///
    /// Unreadable repository/overlay `rules` fail closed.
    pub fn new(working_dir: &Path) -> Result<Self, String> {
        let rules = load_all_rules(working_dir)?;
        Ok(Self {
            working_dir: working_dir.to_path_buf(),
            rules,
            context: None,
            intent: None,
            dynamic_context: false,
            context_budget: None,
            memory_anchors: Vec::new(),
            audited_memory_context: None,
            extra_sections: Vec::new(),
        })
    }

    /// Replace the content of a specific rule category.
    pub fn override_rule(mut self, category: RuleCategory, content: impl Into<String>) -> Self {
        if let Some(rule) = self.rules.iter_mut().find(|r| r.category == category) {
            rule.content = content.into();
        }
        self
    }

    /// Set the operating context mode (dev, review, research).
    ///
    /// The context is appended after all rules, adjusting the agent's
    /// behavior and priorities for the given mode.
    pub fn with_context(mut self, mode: ContextMode) -> Self {
        self.context = Some(mode);
        self
    }

    /// Set the task intent used to shape dynamic prompt guidance and tool policy.
    pub fn with_intent(mut self, intent: TaskIntent) -> Self {
        self.intent = Some(intent);
        self
    }

    /// Append a cached dynamic workspace context section to the prompt.
    pub fn with_dynamic_context(mut self) -> Self {
        self.dynamic_context = true;
        self
    }

    /// Set the context budget profile rendered into the dynamic prompt section.
    pub fn with_context_budget(mut self, budget: ContextBudget) -> Self {
        self.context_budget = Some(budget);
        self
    }

    /// Include active confirmed memory anchors as a high-priority prompt section.
    pub fn with_memory_anchors(mut self, anchors: impl Into<Vec<MemoryAnchor>>) -> Self {
        self.memory_anchors = anchors.into();
        self
    }

    /// Include one transient Memory bundle whose shared selection receipt was
    /// already committed. The builder cannot accept raw `MemoryNote` values,
    /// so an unaudited retrieval cannot cross this prompt-delivery seam.
    #[allow(
        dead_code,
        reason = "M2-13 connects this audited bundle at the command/runtime boundary"
    )]
    pub(crate) fn with_memory_bundle(mut self, bundle: &AuditedMemoryContextBundleV1) -> Self {
        self.audited_memory_context =
            (!bundle.prompt_section().is_empty()).then(|| bundle.prompt_section().to_string());
        self
    }

    /// Append a custom section to the end of the prompt.
    pub fn extra_section(mut self, heading: impl Into<String>, content: impl Into<String>) -> Self {
        self.extra_sections.push((heading.into(), content.into()));
        self
    }

    /// Build the final system prompt string.
    ///
    /// Unreadable repository/overlay `contexts` fail closed.
    pub fn build(self) -> Result<String, String> {
        let working_dir_str = self.working_dir.display().to_string();
        let mut parts: Vec<String> = Vec::with_capacity(
            self.rules.len()
                + self.extra_sections.len()
                + usize::from(self.context.is_some())
                + usize::from(self.dynamic_context)
                + usize::from(!self.memory_anchors.is_empty())
                + usize::from(self.audited_memory_context.is_some()),
        );

        for rule in &self.rules {
            let content = rule.content.replace("{working_dir}", &working_dir_str);
            parts.push(content);
        }

        if let Some(ref context) = self.context {
            let content = context.load_content(&self.working_dir)?;
            parts.push(content.replace("{working_dir}", &working_dir_str));
        }

        if self.dynamic_context {
            let intent = self.intent.unwrap_or(TaskIntent::Unknown);
            parts.push(build_dynamic_prompt_section_with_budget(
                &self.working_dir,
                intent,
                self.context_budget.as_ref(),
            )?);
        }

        if let Some(section) =
            build_memory_anchor_prompt_section(&self.memory_anchors, chrono::Utc::now())
        {
            parts.push(section);
        }

        if let Some(section) = self.audited_memory_context {
            parts.push(section);
        }

        for (heading, content) in &self.extra_sections {
            let section = format!("## {}\n\n{}", heading, content);
            parts.push(section.replace("{working_dir}", &working_dir_str));
        }

        Ok(parts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_build_contains_base_content() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");
        assert!(
            prompt.contains("Libra"),
            "prompt should contain Libra identity"
        );
        assert!(
            prompt.contains("coding assistant"),
            "prompt should contain role description"
        );
    }

    #[test]
    fn test_working_dir_substituted() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");
        let dir_str = tmp.path().display().to_string();
        assert!(
            prompt.contains(&dir_str),
            "prompt should contain the actual working directory path"
        );
        assert!(
            !prompt.contains("{working_dir}"),
            "prompt should not contain raw placeholder"
        );
    }

    #[test]
    fn test_all_rule_sections_present() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");

        assert!(
            prompt.contains("Coding Style"),
            "missing coding style section"
        );
        assert!(
            prompt.contains("Error Handling"),
            "missing error handling section"
        );
        assert!(
            prompt.contains("Git Workflow"),
            "missing git workflow section"
        );
        assert!(prompt.contains("Testing"), "missing testing section");
        assert!(prompt.contains("Security"), "missing security section");
        assert!(prompt.contains("Tool Use"), "missing tool use section");
    }

    #[test]
    fn test_tool_use_mentions_semantic_tools() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");

        assert!(
            prompt.contains("list_symbols"),
            "prompt should mention semantic symbol listing"
        );
        assert!(
            prompt.contains("trace_callers"),
            "prompt should mention semantic caller tracing"
        );
        assert!(
            prompt.contains("approximate"),
            "prompt should warn that semantic relationship tools are approximate"
        );
    }

    #[test]
    fn test_override_rule() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .override_rule(RuleCategory::Security, "Custom security rules here.")
            .build()
            .expect("prompt");

        assert!(prompt.contains("Custom security rules here."));
    }

    #[test]
    fn test_extra_section_appended() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .extra_section("Project Context", "This is a Rust CLI tool.")
            .build()
            .expect("prompt");

        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("This is a Rust CLI tool."));
    }

    #[test]
    fn test_extra_section_working_dir_substituted() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .extra_section("Extra", "Path is {working_dir}")
            .build()
            .expect("prompt");

        let dir_str = tmp.path().display().to_string();
        assert!(prompt.contains(&format!("Path is {}", dir_str)));
    }

    #[test]
    fn test_prompt_is_nontrivial_length() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");
        // The composed prompt should be substantial (all 7 rule files)
        assert!(
            prompt.len() > 1000,
            "composed prompt should be substantial, got {} bytes",
            prompt.len()
        );
    }

    #[test]
    fn test_project_local_override() {
        let tmp = TempDir::new().unwrap();
        let rules_dir = tmp.path().join(".libra").join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(
            rules_dir.join("base.md"),
            "You are a custom assistant for ProjectX.\n\nWorking directory: {working_dir}",
        )
        .unwrap();

        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");
        assert!(prompt.contains("custom assistant for ProjectX"));
        assert!(prompt.contains(&tmp.path().display().to_string()));
    }

    #[test]
    fn test_with_context_dev() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .with_context(ContextMode::Dev)
            .build()
            .expect("prompt");

        assert!(prompt.contains("Development Mode"));
    }

    #[test]
    fn test_with_context_review() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .with_context(ContextMode::Review)
            .build()
            .expect("prompt");

        assert!(prompt.contains("Code Review Mode"));
    }

    #[test]
    fn test_with_context_research() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .with_context(ContextMode::Research)
            .build()
            .expect("prompt");

        assert!(prompt.contains("Research Mode"));
    }

    #[test]
    fn test_context_appears_after_rules() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .with_context(ContextMode::Dev)
            .build()
            .expect("prompt");

        let tool_use_pos = prompt.find("Tool Use").expect("should contain Tool Use");
        let context_pos = prompt
            .find("Development Mode")
            .expect("should contain context");
        assert!(
            context_pos > tool_use_pos,
            "context should appear after rules"
        );
    }

    #[test]
    fn test_no_context_by_default() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path())
            .expect("builder")
            .build()
            .expect("prompt");

        assert!(!prompt.contains("Development Mode"));
        assert!(!prompt.contains("Code Review Mode"));
        assert!(!prompt.contains("Research Mode"));
    }
}
