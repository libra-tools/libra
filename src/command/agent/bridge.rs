//! `libra agent bridge --stdio` — the repository-scoped DeepSeek Harness
//! ingress (plan-20260818 LB-01/03, ADR-LB-01).
//!
//! This is the ONLY standard inbound write transport for the Harness plugin.
//! It is deliberately NOT `libra code --control stdio` (a client that
//! controls a live Code session) and NOT an MCP server.
//!
//! stdout carries exactly one JSON-RPC 2.0 NDJSON frame per response; all
//! diagnostics go to stderr (GC-LB-04). The protocol, method allowlist,
//! limits and error catalogue live in
//! `crate::internal::ai::agent_bridge::protocol` (GC-LB-02); session/event
//! ingress lives in `crate::internal::ai::agent_bridge::ingress`.

use async_trait::async_trait;
use clap::Args;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use tracing::Instrument;

use crate::{
    internal::{
        ai::agent_bridge::{
            ingress::{BridgeContext, dispatch as dispatch_ingress},
            methods::dispatch as dispatch_read,
            mutations::dispatch as dispatch_mutation,
            protocol::{BridgeError, BridgeRequest, BridgeResponse, dispatch_handshake},
            transport::{BridgeHandler, run},
            workspace::dispatch as dispatch_workspace,
        },
        db::get_db_conn_instance,
    },
    utils::{error::CliResult, output::OutputConfig},
};

#[derive(Args, Debug)]
pub struct BridgeArgs {
    /// Run the bridge over stdin/stdout using newline-delimited JSON-RPC 2.0.
    /// Required: the bridge has no other transport.
    #[arg(long)]
    pub stdio: bool,
}

/// The bridge command handler: serves the handshake, the session/event
/// ingress methods (LB-03), and reports every other allowlisted method as
/// not-yet-implemented (LB-04..LB-06 implement them). Made `pub` so the
/// span/crash integration tests can drive it against an in-memory store.
pub struct IngressBridgeHandler {
    pub ctx: BridgeContext,
}

impl IngressBridgeHandler {
    /// Build a handler over a trusted scope. The caller (the command or a
    /// test) supplies the repository context; the request body never does.
    pub fn new(ctx: BridgeContext) -> Self {
        Self { ctx }
    }
}

#[async_trait(?Send)]
impl BridgeHandler for IngressBridgeHandler {
    async fn handle(&self, request: &BridgeRequest) -> Result<Option<BridgeResponse>, BridgeError> {
        // One tracing span per request (GC-LB-10). It carries only stable,
        // low-cardinality fields (method, request id, scope) — never the raw
        // payload, prompt, token, or response body (GC-LB-08). `.instrument`
        // attaches the span for the whole async body and closes it when the
        // future completes, so a request that panics or stalls still closes
        // its span.
        let span = tracing::info_span!(
            "agent.bridge.request",
            method = %request.method,
            id = ?request.id,
            repository_id = %self.ctx.repository_id,
        );
        async move {
            match request.method.as_str() {
                "initialize" => dispatch_handshake(request),
                _ => {
                    // Read methods (LB-04): context.get / status.get /
                    // history.search / checkpoint.list / checkpoint.show / diff.get.
                    if let Some(resp) = dispatch_read(&self.ctx, request).await? {
                        return Ok(Some(resp));
                    }
                    // Session/event ingress methods (LB-03).
                    if let Some(resp) = dispatch_ingress(&self.ctx, request).await? {
                        return Ok(Some(resp));
                    }
                    // Mutations (LB-05): approval/actor/operation-id binding.
                    if let Some(resp) = dispatch_mutation(&self.ctx, request).await? {
                        return Ok(Some(resp));
                    }
                    // Workspace leases (LB-06): claim/renew/release via the store.
                    if let Some(resp) = dispatch_workspace(&self.ctx, request).await? {
                        return Ok(Some(resp));
                    }
                    // Allowlisted but not yet implemented (LB-05/LB-06 remainder).
                    Ok(Some(
                        BridgeError::not_implemented(&request.method)
                            .into_object(request.id.clone().unwrap_or(serde_json::Value::Null)),
                    ))
                }
            }
        }
        .instrument(span)
        .await
    }
}

/// Run `libra agent bridge --stdio`. Reads NDJSON from stdin and writes one
/// NDJSON frame per response to stdout. Repository/worktree scope is resolved
/// from the trusted context (GC-LB-06/07), never from the request body.
/// Returns a CLI error only on a fatal I/O failure; protocol-level failures
/// are answered as JSON-RPC error frames (ER-LB-09).
pub async fn execute_safe(args: BridgeArgs, _output: &OutputConfig) -> CliResult<()> {
    if !args.stdio {
        return Err(crate::utils::error::CliError::command_usage(
            "libra agent bridge requires --stdio (the bridge has no other transport)",
        ));
    }

    let conn = get_db_conn_instance().await;
    let repository_id = resolve_repository_id(&conn).await.map_err(|msg| {
        crate::utils::error::CliError::fatal(format!("libra agent bridge: {msg}"))
    })?;

    let ctx = BridgeContext {
        conn,
        repository_id,
        // Worktree binding is refined in LB-06; the bridge command currently
        // binds repository scope only.
        worktree_id: None,
    };

    let handler = IngressBridgeHandler::new(ctx);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let outcome = {
        let stdout = stdout.lock();
        run(
            stdin.lock(),
            stdout,
            &handler,
            std::time::Duration::from_secs(
                crate::internal::ai::agent_bridge::REQUEST_DEADLINE_SECS,
            ),
        )
        .await
    };
    // GC-LB-10: whatever ended the loop — EOF, broken pipe, or a fatal I/O
    // error — no review run this bridge started may outlive it. Cancel and
    // drain them (bounded) BEFORE returning, so a client disconnect can never
    // leave orphaned reviewer process groups behind.
    crate::internal::ai::agent_bridge::vcs::supervisor::shutdown().await;
    outcome.map_err(|error| {
        crate::utils::error::CliError::fatal(format!(
            "libra agent bridge: fatal I/O error on the stdio transport: {error}"
        ))
    })?;
    Ok(())
}

/// Resolve the trusted repository identity from `libra.repoid` (the same
/// identity the workspace store and sequencer control use). A repository
/// without a recorded identity cannot be a bridge target.
async fn resolve_repository_id(conn: &sea_orm::DatabaseConnection) -> Result<String, String> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT `value` FROM `config_kv` WHERE `key` = 'libra.repoid' \
             ORDER BY `id` DESC LIMIT 1",
            [],
        ))
        .await
        .map_err(|e| format!("cannot read this repository's identity: {e}"))?;
    let value: Option<String> = match row {
        Some(row) => row
            .try_get_by_index(0)
            .map_err(|e| format!("this repository's identity is unreadable: {e}"))?,
        None => None,
    };
    value
        .filter(|v| !v.trim().is_empty() && v != "unknown-repo")
        .ok_or_else(|| {
            "this repository has no recorded identity (`libra.repoid`); run `libra status` once \
             to record one before starting the bridge"
                .to_string()
        })
}
