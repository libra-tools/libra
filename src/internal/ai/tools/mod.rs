//! Tool calling infrastructure for AI agents.
//!
//! AI user story: this module gives an agent a bounded, auditable interface for
//! reading project context, proposing plans, editing files, running checks, and
//! recording workflow provenance without bypassing Libra's sandbox and approval
//! policy. Tool contracts should stay aligned with `docs/ai/workflow.md`
//! and the IntentSpec examples in `docs/ai/intentspec_*.yaml`.

use std::{error::Error, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod apply_patch;
pub mod context;
pub mod error;
pub mod handlers;
pub mod registry;
pub mod semantic;
pub mod spec;
pub mod utils;

pub use context::{
    AiOperationContext, GrepFilesArgs, ListDirArgs, ReadFileArgs, ShellArgs, ToolInvocation,
    ToolKind, ToolOutput, ToolPayload, WebSearchArgs,
};
pub use error::{ToolError, ToolResult};
pub use registry::{ToolHandler, ToolRegistry, ToolRegistryBuilder};
pub use spec::{FunctionDefinition, FunctionParameters, ToolSpec, ToolSpecBuilder};

pub use crate::internal::ai::sandbox::ToolRuntimeContext;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub trait Tool: Send + Sync {
    fn name(&self) -> String {
        self.definition().name
    }

    fn description(&self) -> String {
        self.definition().description
    }

    fn definition(&self) -> ToolDefinition;

    fn call(&self, args: Value) -> Result<Value, Box<dyn Error + Send + Sync>>;
}

#[derive(Default, Clone)]
pub struct ToolSet {
    pub tools: Vec<Arc<dyn Tool>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_specs_creation() {
        let read_file_spec = ToolSpec::read_file();
        assert_eq!(read_file_spec.function.name, "read_file");

        let list_dir_spec = ToolSpec::list_dir();
        assert_eq!(list_dir_spec.function.name, "list_dir");

        let grep_spec = ToolSpec::grep_files();
        assert_eq!(grep_spec.function.name, "grep_files");

        let patch_spec = ToolSpec::apply_patch();
        assert_eq!(patch_spec.function.name, "apply_patch");

        let shell_spec = ToolSpec::shell();
        assert_eq!(shell_spec.function.name, "shell");

        let web_search_spec = ToolSpec::web_search();
        assert_eq!(web_search_spec.function.name, "web_search");

        let intent_spec = ToolSpec::submit_intent_draft();
        assert_eq!(intent_spec.function.name, "submit_intent_draft");
    }
}
