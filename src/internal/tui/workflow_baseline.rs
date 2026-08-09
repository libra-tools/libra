//! W0-02 frozen TUI-owned workflow contracts.
//!
//! These labels and repair hints are the machine-readable baseline for
//! IntentSpec review, Plan review, network policy, and the automatic plan
//! repair threshold. Later Web harness work (W2-16 / W3-02 / W3-15) must
//! **retarget** these contracts, not delete the behaviors they pin.

/// IntentSpec review dialog choices shown while
/// [`crate::internal::tui::AgentStatus::AwaitingIntentReviewChoice`].
pub const INTENT_REVIEW_CHOICES: &[&str] = &["Confirm Intent", "Modify Intent", "Cancel"];

/// Post-plan dialog choices shown while
/// [`crate::internal::tui::AgentStatus::AwaitingPostPlanChoice`].
pub const POST_PLAN_CHOICES: &[&str] = &["Execute Plan", "Modify Plan", "Cancel"];

/// Network policy dialog choices shown while
/// [`crate::internal::tui::AgentStatus::AwaitingNetworkPolicyChoice`].
pub const NETWORK_POLICY_CHOICES: &[&str] = &["Network: Deny", "Network: Allow", "Back"];

/// Help line fragments for each IntentSpec review choice.
pub const INTENT_REVIEW_HELP: &[&str] = &["Generate plan", "Revise spec", "Return to chat"];

/// Help line fragments for each post-plan choice.
pub const POST_PLAN_HELP: &[&str] = &["Run the Scheduler", "Edit the plan", "Return to chat"];

/// Help line fragments for each network policy choice.
pub const NETWORK_POLICY_HELP: &[&str] = &[
    "Run shell/gates offline",
    "Shell/gates may use network",
    "Return to plan choices",
];

/// Frozen threshold message when automatic plan repair stops for developer
/// confirmation. Callers must keep the `/plan continue` affordance.
pub fn plan_repair_threshold_baseline_message(
    report: &str,
    attempts: u8,
    max_attempts: u8,
) -> String {
    format!(
        "Automatic plan repair stopped after {attempts} failed repair attempts (automatic threshold: {max_attempts}).\n\
Developer confirmation is required before more automatic correction.\n\
{report}\n\
Reply `continue` or `/plan continue <max-attempts>` to allow more automatic repair attempts, describe specific Plan repair guidance, or use `/plan cancel` to stop."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_baseline_choice_sets_are_phase_specific() {
        assert_eq!(INTENT_REVIEW_CHOICES.len(), 3);
        assert_eq!(POST_PLAN_CHOICES.len(), 3);
        assert_eq!(NETWORK_POLICY_CHOICES.len(), 3);
        assert!(INTENT_REVIEW_CHOICES.contains(&"Confirm Intent"));
        assert!(POST_PLAN_CHOICES.contains(&"Execute Plan"));
        assert!(NETWORK_POLICY_CHOICES.contains(&"Network: Deny"));
        assert!(!INTENT_REVIEW_CHOICES.iter().any(|c| c.contains("Execute")));
        assert!(!POST_PLAN_CHOICES.iter().any(|c| c.contains("Network")));
        assert!(!NETWORK_POLICY_CHOICES.iter().any(|c| c.contains("Execute")));
    }

    #[test]
    fn plan_repair_threshold_baseline_keeps_plan_continue_affordance() {
        let message = plan_repair_threshold_baseline_message("boom", 3, 3);
        assert!(message.contains("Automatic plan repair stopped after 3 failed repair attempts"));
        assert!(message.contains("/plan continue"));
        assert!(message.contains("boom"));
    }
}
