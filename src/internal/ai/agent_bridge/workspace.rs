//! Bridge workspace lease adapter (plan-20260818 LB-06).
//!
//! Maps `workspace.claim`, `workspace.renew` and `workspace.release` onto the
//! existing [`crate::internal::workspace::WorkspaceStore`] — the single lease
//! authority — binding every lease mutation to the trusted bridge session,
//! actor, owner, monotonic fence and operation id. The store's owner + fence
//! conditional writes are the enforcement: a stale owner whose lease was
//! reclaimed with a higher fence cannot renew or release the new owner's
//! lease, and an expired lease is never silently taken over (GC-LB-10/11).
//!
//! Nothing here introduces a second lease algorithm; the store remains the
//! only authority (ADR-LB-01, GC-LB-06). Request bodies may not supply a
//! repository id, worktree id or actor as an override — they must match the
//! trusted context or the request fails closed.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    ingress::BridgeContext,
    protocol::{
        BridgeError, BridgeRequest, BridgeResponse, JSONRPC_VERSION, SOURCE_DEEPSEEK_HARNESS,
    },
    provenance::{require_param_str, trusted_session_scope},
    storage,
};
use crate::internal::workspace::{
    AcquireRequest, RepoIdentity, WorkspaceError, WorkspaceKind, WorkspaceLease,
    WorkspaceOwnerKind, WorkspaceStore,
};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn ok_result(data: Value, id: Value) -> BridgeResponse {
    BridgeResponse {
        jsonrpc: JSONRPC_VERSION,
        result: Some(data),
        error: None,
        id,
    }
}

/// Map a `WorkspaceError` onto a stable, actionable bridge error.
fn map_workspace_error(err: WorkspaceError, action: &str) -> BridgeError {
    match err {
        // Another live record holds the identity/path: a lease-held refusal.
        // Use the dedicated LBR-AGENT-022 code so a client can tell \"lease
        // held, backoff and retry\" from an unrelated denial.
        WorkspaceError::LeaseHeld(detail) => BridgeError::workspace_lease_held(format!(
            "{action} refused: {detail} (another agent or runtime already holds the workspace)"
        )),
        // Owner/fence mismatch: the lease was reclaimed or already settled.
        // Use the dedicated LBR-AGENT-023 code so a client can tell \"lease
        // lost\" from a generic scope mismatch.
        WorkspaceError::LeaseLost {
            workspace_id,
            owner,
            fence,
        } => BridgeError::workspace_lease_lost(format!(
            "{action} refused: workspace lease for '{workspace_id}' is no longer held by '{owner}' at fence {fence} — it was reclaimed or already released"
        )),
        WorkspaceError::NotFound(id) => BridgeError::scope_mismatch(format!(
            "{action} refused: workspace '{id}' does not exist in this repository"
        )),
        WorkspaceError::Invalid(msg) => BridgeError::invalid_params(format!("{action}: {msg}")),
        WorkspaceError::Corrupt(msg) => BridgeError::internal(format!("{action}: {msg}")),
        WorkspaceError::ReadFailed(msg) => BridgeError::internal(format!("{action}: {msg}")),
        WorkspaceError::WriteFailed(msg) => BridgeError::internal(format!("{action}: {msg}")),
    }
}

/// Derive the lease owner token from a trusted bridge session (GC-LB-07): the
/// actor identity is `deepseek-harness:<session_id>`. Only the authenticated
/// `bridge_session_id` — the session's primary key, which is what a peer must
/// hold to operate on the session at all — is used as the credential basis.
///
/// A request must NEVER be able to influence this token. In particular the
/// client-supplied `agent_id` is NOT used: it is a free-form, non-unique label
/// taken verbatim from the `session.open` body, so two colluding sessions
/// could claim the same `agent_id` and thereby forge the same lease owner
/// (cross-session ownership forgery). Basing the owner on `bridge_session_id`
/// alone keeps the owner equal to the session identity, which cannot be
/// aliased by another session (the primary key is unique).
fn lease_owner_for(session: &storage::BridgeSessionRow) -> String {
    format!("{SOURCE_DEEPSEEK_HARNESS}:{}", session.bridge_session_id)
}

/// Dispatch the workspace lease methods. Returns `None` for a method this
/// module does not own so the surrounding dispatcher can route it elsewhere.
pub async fn dispatch(
    ctx: &BridgeContext,
    request: &BridgeRequest,
) -> Result<Option<BridgeResponse>, BridgeError> {
    let id = request.id.clone().unwrap_or(Value::Null);
    match request.method.as_str() {
        "workspace.claim" => Ok(Some(workspace_claim(ctx, request, id).await?)),
        "workspace.renew" => Ok(Some(workspace_renew(ctx, request, id).await?)),
        "workspace.release" => Ok(Some(workspace_release(ctx, request, id).await?)),
        _ => Ok(None),
    }
}

/// Parse `params.path` into a canonical absolute `PathBuf`.
fn claim_path(params: &Option<Value>) -> Result<PathBuf, BridgeError> {
    let path_str = require_param_str(params, "path")?;
    let path = PathBuf::from(&path_str);
    if !path.is_absolute() {
        return Err(BridgeError::invalid_params(format!(
            "workspace.claim path must be absolute; got '{path_str}'"
        )));
    }
    Ok(path)
}

/// `workspace.claim` — acquire a lease through `WorkspaceStore`, bound to the
/// trusted session/actor, and return the capability token (id + owner +
/// fence). The canonical path and worktree identity are validated by the
/// store; a request cannot override the repository id.
async fn workspace_claim(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    // 1. Trusted session scope (repository + actor lineage). A request that
    //    self-reports a different repository is rejected fail-closed.
    let (session, session_id) = trusted_session_scope(ctx, request).await?;

    // 1b. The context's repository id must match the store's canonical
    //     identity (GC-LB-06): a spoofed context can never claim a workspace
    //     under a different repository id.
    trusted_repo_identity(ctx).await?;

    // 2. Path + optional worktree/ttl from params; canonicalized by the store.
    let path = claim_path(&request.params)?;
    let worktree_id = request
        .params
        .as_ref()
        .and_then(|p| p.get("worktree_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let lease_ttl_ms = request
        .params
        .as_ref()
        .and_then(|p| p.get("lease_ttl_ms"))
        .and_then(Value::as_i64)
        .unwrap_or(crate::internal::ai::agent_bridge::REQUEST_DEADLINE_SECS as i64 * 1000);

    // 3. Derive the lease owner from the session lineage (never self-reported).
    let lease_owner = lease_owner_for(&session);

    // 4. Build the store request. Linked workspaces need a worktree id; a
    //    task workspace owns its own directory. The owner is the bridge actor.
    let acquire = match worktree_id {
        Some(wt) => AcquireRequest::linked(wt, path.clone(), &lease_owner, lease_ttl_ms)
            .with_association(None, Some(session_id.clone())),
        None => AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            path.clone(),
            &lease_owner,
            lease_ttl_ms,
        )
        .with_association(None, Some(session_id.clone())),
    }
    .with_owner(WorkspaceOwnerKind::Agent, session.agent_id.clone());

    let lease = WorkspaceStore::acquire(&ctx.conn, &acquire, now_ms())
        .await
        .map_err(|err| map_workspace_error(err, "workspace.claim"))?;

    Ok(ok_result(lease_to_json(&lease, &session_id), id))
}

/// `workspace.renew` — extend the lease using the presented owner/fence.
/// A stale owner or fence (already reclaimed) is a fail-closed scope error.
async fn workspace_renew(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    let (session, session_id) = trusted_session_scope(ctx, request).await?;
    let lease = lease_from_params(&request.params)?;
    let lease_ttl_ms = request
        .params
        .as_ref()
        .and_then(|p| p.get("lease_ttl_ms"))
        .and_then(Value::as_i64)
        .unwrap_or(crate::internal::ai::agent_bridge::REQUEST_DEADLINE_SECS as i64 * 1000);

    // The lease owner must be derived from this session's lineage.
    if lease.owner != lease_owner_for(&session) {
        return Err(BridgeError::scope_mismatch(format!(
            "workspace.renew refused: lease owner '{0}' does not belong to this bridge session",
            lease.owner
        )));
    }

    let renewed = WorkspaceStore::renew_with_conn(&ctx.conn, &lease, lease_ttl_ms, now_ms())
        .await
        .map_err(|err| map_workspace_error(err, "workspace.renew"))?;

    Ok(ok_result(lease_to_json(&renewed, &session_id), id))
}

/// `workspace.release` — settle the lease with the presented owner/fence.
async fn workspace_release(
    ctx: &BridgeContext,
    request: &BridgeRequest,
    id: Value,
) -> Result<BridgeResponse, BridgeError> {
    let (session, session_id) = trusted_session_scope(ctx, request).await?;
    let lease = lease_from_params(&request.params)?;

    if lease.owner != lease_owner_for(&session) {
        return Err(BridgeError::scope_mismatch(format!(
            "workspace.release refused: lease owner '{0}' does not belong to this bridge session",
            lease.owner
        )));
    }

    WorkspaceStore::release_with_conn(&ctx.conn, &lease, now_ms())
        .await
        .map_err(|err| map_workspace_error(err, "workspace.release"))?;

    Ok(ok_result(
        json!({
            "session_id": session_id,
            "workspace_id": lease.workspace_id,
            "released": true,
        }),
        id,
    ))
}

/// Parse a `WorkspaceLease` from request params (workspace_id/owner/fence).
fn lease_from_params(params: &Option<Value>) -> Result<WorkspaceLease, BridgeError> {
    let workspace_id = require_param_str(params, "workspace_id")?;
    let owner = require_param_str(params, "owner")?;
    let fence = params
        .as_ref()
        .and_then(|p| p.get("fence"))
        .and_then(Value::as_i64)
        .ok_or_else(|| BridgeError::invalid_params("workspace lease requires integer 'fence'"))?;
    Ok(WorkspaceLease {
        workspace_id,
        owner,
        fence,
        expires_at: None,
    })
}

fn lease_to_json(lease: &WorkspaceLease, session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "workspace_id": lease.workspace_id,
        "owner": lease.owner,
        "fence": lease.fence,
        "expires_at": lease.expires_at,
    })
}

/// Resolve the trusted repository identity (used to double-check the bridge
/// context against the store's canonical identity). Kept as a small helper so
/// the adapter never trusts a request-supplied repo id.
pub async fn trusted_repo_identity(ctx: &BridgeContext) -> Result<RepoIdentity, BridgeError> {
    let identity = RepoIdentity::resolve(&ctx.conn)
        .await
        .map_err(|err| map_workspace_error(err, "resolve repository identity"))?;
    if identity.as_str() != ctx.repository_id {
        return Err(BridgeError::scope_mismatch(format!(
            "bridge repository scope '{}' disagrees with the workspace registry identity '{}'; refusing",
            ctx.repository_id,
            identity.as_str()
        )));
    }
    Ok(identity)
}
