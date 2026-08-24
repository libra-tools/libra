//! MCP bridge handler: exposes [`LibraMcpServer`] tools as [`ToolHandler`]
//! implementations so the Code agent can call MCP workflow tools without going
//! through the HTTP transport.
//!
//! CEX-14 keeps this legacy registration API intact while delegating schema,
//! mutability classification, and dispatch to [`McpSource`]. That gives the
//! Source Pool and old bridge one shared implementation during the Phase A
//! migration.

use std::sync::Arc;

use async_trait::async_trait;

use crate::internal::ai::{
    mcp::server::LibraMcpServer,
    sources::{McpSource, Source, SourceCallLog, SourceToolHandler},
    tools::{
        context::{ToolInvocation, ToolKind, ToolOutput},
        error::ToolResult,
        registry::ToolHandler,
        spec::ToolSpec,
    },
};

/// Legacy MCP bridge handler retained temporarily as an internal test seam.
///
/// New source-prefixed handlers are produced by
/// [`crate::internal::ai::sources::SourcePool`]. There are no current
/// production registrations of [`Self::all_handlers`]; the type remains as an
/// internal test seam that pins the legacy aggregate-handler contract while
/// source-prefixed handlers complete their staged migration.
pub struct McpBridgeHandler {
    delegate: SourceToolHandler,
}

impl McpBridgeHandler {
    fn new(source: Arc<dyn Source>, tool_name: &str) -> Result<Self, String> {
        let delegate = SourceToolHandler::new(
            source,
            "legacy-mcp-bridge",
            tool_name,
            tool_name,
            SourceCallLog::new(),
        )
        .map_err(|error| error.to_string())?;
        Ok(Self { delegate })
    }

    /// Build all MCP bridge handlers ready for bulk-registration in a
    /// [`ToolRegistryBuilder`](crate::internal::ai::tools::ToolRegistryBuilder).
    ///
    /// Returns legacy `(tool_name, handler)` pairs with the same public names and
    /// schemas as the pre-CEX-14 bridge.
    pub fn all_handlers(server: Arc<LibraMcpServer>) -> Vec<(String, Arc<dyn ToolHandler>)> {
        let source: Arc<dyn Source> = Arc::new(McpSource::builtin(server));
        source
            .manifest()
            .tools
            .iter()
            .filter_map(|capability| {
                let handler = match Self::new(source.clone(), &capability.name) {
                    Ok(handler) => handler,
                    Err(error) => {
                        tracing::error!(
                            tool = capability.name,
                            "failed to build MCP bridge handler: {error}"
                        );
                        return None;
                    }
                };
                Some((
                    capability.name.clone(),
                    Arc::new(handler) as Arc<dyn ToolHandler>,
                ))
            })
            .collect()
    }
}

#[async_trait]
impl ToolHandler for McpBridgeHandler {
    fn kind(&self) -> ToolKind {
        self.delegate.kind()
    }

    async fn is_mutating(&self, invocation: &ToolInvocation) -> bool {
        self.delegate.is_mutating(invocation).await
    }

    async fn requires_network(&self, invocation: &ToolInvocation) -> bool {
        self.delegate.requires_network(invocation).await
    }

    async fn handle(&self, invocation: ToolInvocation) -> ToolResult<ToolOutput> {
        self.delegate.handle(invocation).await
    }

    fn schema(&self) -> ToolSpec {
        self.delegate.schema()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_handlers_exposes_libra_vcs_tool() {
        let server = Arc::new(LibraMcpServer::new(None, None));
        let handlers = McpBridgeHandler::all_handlers(server);
        assert!(
            handlers
                .iter()
                .any(|(name, _handler)| name == "run_libra_vcs")
        );
    }
}
