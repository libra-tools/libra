//! Controlled execution driver for `libra review --fix`.
//!
//! This module owns no mutating capability. It submits the fixed request to an
//! already-running Code session, renders that session's pending interactions to
//! a caller-supplied user responder, and reports only an observed patch or the
//! runtime's existing deterministic repair state.

use std::{collections::BTreeSet, path::Path, time::Duration};

use async_trait::async_trait;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep, sleep_until};

use super::{
    ReviewFixBridgeError, ReviewFixInput,
    fix_control::ReviewFixControlClient,
    fix_protocol::{
        ReviewFixInteraction, ReviewFixInteractionKind, ReviewFixInteractionResponse,
        ReviewFixSessionSnapshot, ReviewFixSessionStatus,
    },
    fix_response::{denial_for, requires_code_session_handoff, validate_response},
};

const EXECUTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const CONTROLLER_RENEW_INTERVAL: Duration = Duration::from_secs(30);
const IDLE_POLLS_BEFORE_FAILURE: u16 = 30;
const MAX_INTERACTIONS_PER_EXECUTION: usize = 256;

/// Terminal result reported only after the existing Code runtime completed a
/// controlled path. `RepairRequired` deliberately leaves failure handling to
/// the runtime's plan-execution repair contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewFixExecutionOutcome {
    PatchApplied,
    RepairRequired { patch_applied: bool },
}

/// The CLI or another presentation layer supplies explicit user decisions.
/// The bridge never chooses approvals, sandbox access, plans, or answers.
#[async_trait]
pub trait ReviewFixInteractionResponder: Send + Sync {
    async fn respond(
        &self,
        interaction: ReviewFixInteraction,
    ) -> Result<ReviewFixInteractionResponse, String>;
}

/// Submit and drive one controlled review-fix request through an active Code
/// runtime. Every response remains subject to that runtime's existing gates.
pub async fn execute_review_fix(
    working_dir: &Path,
    input: ReviewFixInput,
    responder: &dyn ReviewFixInteractionResponder,
) -> Result<ReviewFixExecutionOutcome, ReviewFixBridgeError> {
    let admission_message = input.admission_message()?;

    let controller = ReviewFixControlClient::connect(working_dir).await?;
    let execution = execute_with_controller(&controller, admission_message, responder)
        .await
        .map_err(normalize_active_execution_error);
    let detach = controller
        .detach()
        .await
        .map_err(normalize_active_execution_error);
    finish_execution(execution, detach)
}

async fn execute_with_controller(
    controller: &ReviewFixControlClient,
    admission_message: &'static str,
    responder: &dyn ReviewFixInteractionResponder,
) -> Result<ReviewFixExecutionOutcome, ReviewFixBridgeError> {
    let initial = controller.snapshot().await?;
    ensure_ready_for_submission(&initial)?;
    let baseline = initial.patchsets()?;
    controller.submit_admission(admission_message).await?;

    let deadline = Instant::now() + EXECUTION_TIMEOUT;
    let mut next_renewal = Instant::now() + CONTROLLER_RENEW_INTERVAL;
    let mut responded = BTreeSet::new();
    let mut idle_polls = 0_u16;

    loop {
        if Instant::now() >= deadline {
            return Err(ReviewFixBridgeError::TimedOut);
        }
        if Instant::now() >= next_renewal {
            controller.renew_controller().await?;
            next_renewal = Instant::now() + CONTROLLER_RENEW_INTERVAL;
        }

        let snapshot = controller.snapshot().await?;
        let patch_applied = snapshot.patchsets()? != baseline;
        if snapshot.plan_execution_repair.is_some() {
            return Ok(ReviewFixExecutionOutcome::RepairRequired { patch_applied });
        }

        let interactions = snapshot.pending_interactions()?;
        if interactions
            .iter()
            .any(|interaction| interaction.kind == ReviewFixInteractionKind::PlanExecutionRepair)
        {
            return Ok(ReviewFixExecutionOutcome::RepairRequired { patch_applied });
        }
        if let Some(interaction) = interactions
            .into_iter()
            .find(|interaction| !responded.contains(&interaction.id))
        {
            if responded.len() >= MAX_INTERACTIONS_PER_EXECUTION {
                return Err(ReviewFixBridgeError::ExecutionFailed);
            }
            responded.insert(interaction.id.clone());
            let response =
                respond_with_lease_renewal(controller, responder, interaction.clone(), deadline)
                    .await?;
            next_renewal = Instant::now() + CONTROLLER_RENEW_INTERVAL;
            validate_response(&interaction, &response)?;
            let denial = denial_for(&interaction, &response);
            controller
                .respond_interaction(&interaction.id, &response)
                .await?;
            if let Some(error) = denial {
                if patch_applied {
                    return Err(ReviewFixBridgeError::MutationBeforeDenial);
                }
                return Err(error);
            }
            if requires_code_session_handoff(&interaction, &response) {
                return Err(ReviewFixBridgeError::ExecutionFailed);
            }
            idle_polls = 0;
            continue;
        }

        match snapshot.status {
            ReviewFixSessionStatus::Error | ReviewFixSessionStatus::IndeterminateSideEffect => {
                return Err(ReviewFixBridgeError::ExecutionFailed);
            }
            ReviewFixSessionStatus::Completed => {
                return patch_applied
                    .then_some(ReviewFixExecutionOutcome::PatchApplied)
                    .ok_or(ReviewFixBridgeError::ExecutionFailed);
            }
            ReviewFixSessionStatus::Idle if patch_applied => {
                return Ok(ReviewFixExecutionOutcome::PatchApplied);
            }
            ReviewFixSessionStatus::Idle => {
                idle_polls = idle_polls.saturating_add(1);
                if idle_polls >= IDLE_POLLS_BEFORE_FAILURE {
                    return Err(ReviewFixBridgeError::ExecutionFailed);
                }
            }
            ReviewFixSessionStatus::Thinking
            | ReviewFixSessionStatus::ExecutingTool
            | ReviewFixSessionStatus::AwaitingInteraction => idle_polls = 0,
        }
        sleep(POLL_INTERVAL).await;
    }
}

fn ensure_ready_for_submission(
    snapshot: &ReviewFixSessionSnapshot,
) -> Result<(), ReviewFixBridgeError> {
    if snapshot.status != ReviewFixSessionStatus::Idle
        || snapshot.plan_execution_repair.is_some()
        || !snapshot.pending_interactions()?.is_empty()
    {
        return Err(ReviewFixBridgeError::ExecutionFailed);
    }
    Ok(())
}

fn finish_execution(
    execution: Result<ReviewFixExecutionOutcome, ReviewFixBridgeError>,
    detach: Result<(), ReviewFixBridgeError>,
) -> Result<ReviewFixExecutionOutcome, ReviewFixBridgeError> {
    match (execution, detach) {
        (Err(error), _) => Err(error),
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Ok(_), Err(error)) => Err(error),
    }
}

async fn respond_with_lease_renewal(
    controller: &ReviewFixControlClient,
    responder: &dyn ReviewFixInteractionResponder,
    interaction: ReviewFixInteraction,
    deadline: Instant,
) -> Result<ReviewFixInteractionResponse, ReviewFixBridgeError> {
    let response = responder.respond(interaction);
    tokio::pin!(response);
    let mut renewal = interval_at(
        Instant::now() + CONTROLLER_RENEW_INTERVAL,
        CONTROLLER_RENEW_INTERVAL,
    );
    renewal.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut response => {
                return result.map_err(|_| ReviewFixBridgeError::InvalidInteractionResponse);
            }
            _ = sleep_until(deadline) => {
                return Err(ReviewFixBridgeError::TimedOut);
            }
            _ = renewal.tick() => controller.renew_controller().await?,
        }
    }
}

fn normalize_active_execution_error(error: ReviewFixBridgeError) -> ReviewFixBridgeError {
    match error {
        ReviewFixBridgeError::Unavailable => ReviewFixBridgeError::ExecutionFailed,
        other => other,
    }
}

#[cfg(test)]
#[path = "fix_execution_tests.rs"]
mod tests;
