//! Bridge provenance association (plan-20260818 LB-05).
//!
//! Mutations must produce an auditable association that links the bridge
//! session, parent session/agent, workspace, actor, provider/model (when
//! available) and evidence ids — **without** encoding metadata into commit
//! messages. This module builds that provenance projection and records it
//! through the durable `agent_bridge_link` catalog.

use serde_json::{Value, json};

use super::{
    ingress::BridgeContext,
    protocol::{BridgeError, BridgeRequest},
    storage,
};

/// Build the provenance association record for a mutation. All scope fields
/// are derived from the trusted context + the session row, never from the
/// request body. The explicit per-field parameters keep the record literal
/// (fields map 1:1 to the JSON shape); grouped into a struct they would
/// obscure that mapping, so the arg count is allowed deliberately.
#[allow(clippy::too_many_arguments)]
pub fn build_provenance(
    ctx: &BridgeContext,
    session_id: &str,
    operation_id: &str,
    actor_kind: Option<&str>,
    actor_id: Option<&str>,
    parent_session_id: Option<&str>,
    workspace_id: Option<&str>,
    provider_model: Option<(&str, &str)>,
    evidence_ids: &[String],
) -> Value {
    json!({
        "operation_id": operation_id,
        "session_id": session_id,
        "repository_id": ctx.repository_id,
        "worktree_id": ctx.worktree_id,
        "workspace_id": workspace_id,
        "parent_session_id": parent_session_id,
        "actor": {
            "kind": actor_kind,
            "id": actor_id,
        },
        "provider": {
            "kind": provider_model.map(|(p, _)| p),
            "model": provider_model.map(|(_, m)| m),
        },
        "evidence_ids": evidence_ids,
    })
}

/// The relation kinds a mutation result may be associated with.
///
/// Each names one edge from the result to a scope object. `Evidence` is the
/// only multi-valued relation: a result may cite several evidence ids, but it
/// has exactly one operation, one workspace and one parent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// The operation that produced this result.
    Operation,
    /// The workspace the producing session was bound to.
    Workspace,
    /// The parent session in the agent lineage (GC-LB-07).
    ParentSession,
    /// One evidence id the result cites.
    Evidence,
}

impl Relation {
    /// The stored `target_type`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::Workspace => "workspace",
            Self::ParentSession => "parent_session",
            Self::Evidence => "evidence",
        }
    }

    /// How many targets this relation may have.
    fn singularity(self) -> storage::LinkSingularity {
        match self {
            Self::Evidence => storage::LinkSingularity::Multi,
            _ => storage::LinkSingularity::ByRelation,
        }
    }
}

/// One association edge to persist: the result's kind and id, the relation, and
/// the scope object it points at.
pub struct Association<'a> {
    /// What produced the association (`checkpoint`, `commit`, `restore`, `review`).
    pub source_type: &'a str,
    /// The result's stable identity.
    pub source_id: &'a str,
    /// The relation kind.
    pub relation: Relation,
    /// The scope object the result is associated with.
    pub target_id: &'a str,
}

/// Persist the association links of a mutation into `agent_bridge_link`.
///
/// Replaying the same edge is an idempotent no-op. Pointing a **singular**
/// relation at a different target is a fail-closed `digest_conflict`: the
/// recorded provenance is never silently overwritten (R1-P1-2 discipline).
pub async fn record_links(
    ctx: &BridgeContext,
    session_id: &str,
    links: &[Association<'_>],
    now_ms: i64,
) -> Result<(), BridgeError> {
    for link in links {
        let outcome = storage::insert_link(
            &ctx.conn,
            session_id,
            link.source_type,
            link.source_id,
            link.relation.as_str(),
            link.target_id,
            link.relation.singularity(),
            now_ms,
        )
        .await
        .map_err(|e| BridgeError::internal(format!("record provenance link: {e}")))?;
        match outcome {
            storage::LinkOutcome::Inserted | storage::LinkOutcome::Existing => {}
            storage::LinkOutcome::Conflict { stored_target_id } => {
                return Err(BridgeError::digest_conflict(format!(
                    "provenance association {}:{} ({}) already points at '{stored_target_id}', not '{}'; refusing",
                    link.source_type,
                    link.source_id,
                    link.relation.as_str(),
                    link.target_id
                )));
            }
        }
    }
    Ok(())
}

/// Require a string param that identifies the operation (used by every
/// mutation for idempotency + digest conflict).
pub fn require_param_str(params: &Option<Value>, key: &str) -> Result<String, BridgeError> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            BridgeError::invalid_params(format!("mutation requires string param '{key}'"))
        })
}

/// Resolve the trusted session scope for a mutation and return the fields a
/// mutation needs to bind actor/provenance. Missing session or a scope
/// mismatch is a fail-closed error.
pub async fn trusted_session_scope(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<(storage::BridgeSessionRow, String), BridgeError> {
    let session_id = require_param_str(&request.params, "session_id")?;
    let row = storage::get_session(&ctx.conn, &session_id)
        .await
        .map_err(|e| BridgeError::internal(format!("load bridge session: {e}")))?
        .ok_or_else(|| {
            BridgeError::scope_mismatch(format!(
                "bridge session '{session_id}' is not open in this repository; open it first"
            ))
        })?;
    if row.repository_id != ctx.repository_id {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge session '{session_id}' belongs to repository '{}', not '{}'",
            row.repository_id, ctx.repository_id
        )));
    }
    Ok((row, session_id))
}
