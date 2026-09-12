//! Resolution of merge-commit signing policy.

use crate::command::history_config::{CommitSigningPolicy, HistoryConfigError};

/// Resolve the explicit merge flags before a merge can write state. The
/// default branch intentionally delegates to the commit helper, keeping
/// local-to-global-to-system configuration parsing and precedence identical.
pub(crate) async fn resolve_signing_policy(
    gpg_sign: bool,
    no_gpg_sign: bool,
) -> Result<CommitSigningPolicy, HistoryConfigError> {
    if gpg_sign {
        return Ok(CommitSigningPolicy::Force);
    }
    crate::command::history_config::commit_signing_policy(no_gpg_sign).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn explicit_gpg_sign_forces_vault_signing_without_reading_config() {
        let policy = resolve_signing_policy(true, false)
            .await
            .expect("explicit signing must not require config access");
        assert_eq!(policy, CommitSigningPolicy::Force);
    }
}
