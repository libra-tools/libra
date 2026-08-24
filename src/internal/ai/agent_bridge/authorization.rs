//! Bridge mutation authorization gate (plan-20260818 LB-05).
//!
//! Rust is the final boundary for approvals, actor binding and dangerous
//! actions. High-risk operations (`restore`, `branch-switch`, `reset`, `push`,
//! `publish`, `delete-*`) default to **deny** and may only run with an
//! explicit approval decision. Approval/denial outcomes are always
//! attributable (stable `denied` error) and never swallowed as success.
//!
//! This module is the audit/decision seam that later cards wire to the real
//! `approved_permission` provenance; the decision policy itself is enforced
//! here and pinned by tests.

use super::protocol::BridgeError;

/// Risk class of a bridge mutation, used to decide whether an approval is
/// mandatory (plan-20260818 成功定义/ADR-LB: dangerous operations require an
/// approval decision; Rust is the final deny/execute boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionClass {
    /// Safe / bounded writes that do not mutate HEAD/index/worktree or move
    /// state outside the repository (e.g. recording a bridge checkpoint,
    /// evidence, provenance).
    Normal,
    /// Operations that can mutate working state, move refs, or publish —
    /// denied by default, allowed only with an explicit approval.
    Dangerous,
}

/// Classify a bridge mutation by its risk. Anything unknown is treated as
/// `Dangerous` (fail-closed): the allowlist gates the method name earlier, and
/// here we err toward requiring approval.
pub fn classify(action: &str) -> ActionClass {
    match action {
        "checkpoint.create" | "checkpoint.list" | "checkpoint.show" | "evidence.append"
        | "provenance.append" | "context.get" | "status.get" | "history.search" | "diff.get" => {
            ActionClass::Normal
        }
        // Default deny for anything that can change working state / refs /
        // remote: restore, branch-switch, reset, push, publish, delete-*,
        // commit (bounded but stateful — still approval-gated here).
        _ => ActionClass::Dangerous,
    }
}

/// A caller-supplied approval decision attached to a mutation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// `"approved"` or `"denied"`; anything else is a malformed approval.
    pub decision: String,
    /// Approver identity for audit (attributable, never a secret).
    pub approver: Option<String>,
}

impl Approval {
    pub fn from_params(
        params: &Option<serde_json::Value>,
    ) -> Option<Result<Approval, BridgeError>> {
        let obj = params.as_ref()?;
        let approval = obj.get("approval")?;
        if !approval.is_object() {
            return Some(Err(BridgeError::invalid_params(
                "approval must be an object { decision, approver? }",
            )));
        }
        let decision = approval.get("decision")?.as_str()?.to_string();
        if decision != "approved" && decision != "denied" {
            return Some(Err(BridgeError::invalid_params(format!(
                "approval.decision must be 'approved' or 'denied', got '{decision}'"
            ))));
        }
        let approver = approval
            .get("approver")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Some(Ok(Approval { decision, approver }))
    }
}

/// Enforce the approval gate for a mutation.
///
/// - `Dangerous` actions with no approval, or an explicit `denied` approval,
///   are refused with a stable `denied` error (never a silent success).
/// - `Dangerous` actions with `approved` proceed (fence validation is layered
///   on top by the caller for restore-type operations).
/// - `Normal` actions do not require approval.
pub fn enforce(action: &str, approval: Option<&Approval>) -> Result<(), BridgeError> {
    if classify(action) == ActionClass::Normal {
        return Ok(());
    }
    match approval {
        Some(a) if a.decision == "approved" => Ok(()),
        Some(a) => Err(BridgeError::denied(format!(
            "mutation '{action}' was denied by approval (approver: {})",
            a.approver.as_deref().unwrap_or("unknown")
        ))),
        None => Err(BridgeError::denied(format!(
            "mutation '{action}' is dangerous and requires an explicit approval decision; none was provided"
        ))),
    }
}

/// Bind the request's self-reported actor to the trusted scope derived from
/// the session/handshake (GC-LB-07). The request body is never a credential:
/// a mismatch is a stable `scope_mismatch` error.
pub fn bind_actor(
    action: &str,
    requested_actor: Option<(&str, &str)>, // (actor_kind, actor_id) self-reported
    trusted_actor_kind: Option<&str>,
    trusted_actor_id: Option<&str>,
) -> Result<(), BridgeError> {
    match (requested_actor, trusted_actor_kind, trusted_actor_id) {
        // No self-reported actor: the trusted scope is authoritative.
        (None, _, _) => Ok(()),
        (Some((rk, ri)), Some(tk), Some(ti)) if rk == tk && ri == ti => Ok(()),
        (Some((rk, ri)), _, _) => Err(BridgeError::scope_mismatch(format!(
            "mutation '{action}' self-reports actor ({rk}, {ri}) which does not match the trusted scope; refusing actor spoof"
        ))),
    }
}
