//! In-process harness for capture integration tests.
//!
//! Production hooks read their frame through [`super::ingress`]. Integration
//! tests hand this module only a typed ingress outcome; the raw byte boundary
//! remains entirely within `capture::ingress`.

use std::path::Path;
#[cfg(test)]
use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
};

#[cfg(test)]
use anyhow::Context;
use anyhow::Result;
use sea_orm::DatabaseConnection;
#[cfg(test)]
use sea_orm::{ConnectionTrait, Statement};

use crate::internal::ai::{
    capture::{ingress::CaptureIngressOutcome, live, live_pipeline},
    hooks::provider::{HookProvider, ProviderHookCommand},
    observed_agents::live_capture::LiveCaptureBinding,
};

#[cfg(test)]
static DELETE_RESERVED_SESSIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[cfg(test)]
fn delete_reserved_sessions() -> &'static Mutex<HashSet<String>> {
    DELETE_RESERVED_SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Arm the deterministic in-process race seam used to prove that a catalog
/// reservation erased before owner confirmation cannot publish a checkpoint.
/// This control exists only in test builds and is not reachable from hooks.
#[cfg(test)]
pub(crate) fn delete_reserved_session_once(session_id: impl Into<String>) {
    delete_reserved_sessions()
        .lock()
        .expect("capture test reservation-erase lock")
        .insert(session_id.into());
}

/// Execute the test-only erase after a catalog reservation has been acquired.
/// Keeping the SQL in capture test support prevents the production runtime
/// from acquiring a second catalog mutation path solely for fault injection.
#[cfg(test)]
pub(crate) async fn delete_reserved_session_if_armed(
    conn: &DatabaseConnection,
    session_id: &str,
) -> Result<()> {
    if !delete_reserved_sessions()
        .lock()
        .expect("capture test reservation-erase lock")
        .remove(session_id)
    {
        return Ok(());
    }

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "DELETE FROM agent_session WHERE session_id = ?",
        [session_id.to_owned().into()],
    ))
    .await
    .context("delete reserved capture session for post-reservation race test")?;
    Ok(())
}

/// Invoke the normal typed AgentTraces runtime from an already-lowered test
/// ingress outcome. This harness never accepts raw hook bytes. Like the hook
/// entry, it resolves the provider's live-capture binding exactly once.
#[doc(hidden)]
pub async fn ingest_agent_traces_ingress_outcome_for_test(
    outcome: Result<CaptureIngressOutcome>,
    command: ProviderHookCommand,
    provider: &dyn HookProvider,
    conn: &DatabaseConnection,
    repo_path: Option<&Path>,
) -> Result<()> {
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            live::record_in_process_ingress_validation_failure(command, provider);
            return Err(error);
        }
    };
    live_pipeline::ingest_agent_traces_ingress_outcome_for_test(
        outcome,
        command,
        provider,
        LiveCaptureBinding::resolve(provider),
        conn,
        repo_path,
    )
    .await
}
