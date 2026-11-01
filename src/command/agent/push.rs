//! `libra agent push [--remote <name>] [--force-rewrite]` transport wrapper.
//!
//! The external-agent capture catalogue lives on the local
//! `traces` branch, but the remote contract reserves
//! `refs/libra/traces` so it does not appear as a user branch.
//!
//! AG-20 push-after-prune contract (plan.md Task A5, option (a) — no new
//! stable error code): `refs/libra/traces` is Libra-managed and an
//! `agent clean` prune rewrites the whole chain, so the follow-up push is
//! never fast-forward. Without `--force-rewrite` the rejection carries an
//! actionable hint pointing at the flag; with it the push runs under
//! `--force-with-lease=refs/libra/traces:<last-pushed-tip>` — never an
//! unconditional force — so a remote that moved behind our back (another
//! machine pushed its own rewrite) still fails closed. The "last pushed
//! tip" lease basis is the remote-tracking row this wrapper records after
//! every successful push (the core push pipeline only auto-tracks
//! `refs/heads/*`).

use super::PushArgs;
use crate::{
    command::push as push_command,
    internal::branch::{Branch, TRACES_BRANCH},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

const DEFAULT_TRACES_REMOTE: &str = "origin";
const TRACES_REMOTE_REF: &str = "refs/libra/traces";

pub async fn execute_safe(args: PushArgs, output: &OutputConfig) -> CliResult<()> {
    // The public command is still the ordinary traces ref push. Mega2 mode
    // stays false until LTR-12 wires `--mega2`. The guard itself rejects
    // Mega2 force-rewrite before any network attempt.
    if let Err(message) =
        crate::internal::protocol::mega2_traces::admit_before_network(false, args.force_rewrite)
    {
        return Err(CliError::conflict(message));
    }
    let remote = args
        .remote
        .unwrap_or_else(|| DEFAULT_TRACES_REMOTE.to_string());
    let refspec = format!("{TRACES_BRANCH}:{TRACES_REMOTE_REF}");

    // The tip we are about to push — recorded as the remote-tracking row on
    // success so the next `--force-rewrite` has a lease basis.
    let local_tip = Branch::find_branch_result(TRACES_BRANCH, None)
        .await
        .map_err(|err| {
            CliError::fatal(format!(
                "failed to read the local '{TRACES_BRANCH}' branch: {err}"
            ))
        })?
        .map(|branch| branch.commit.to_string());

    let push_args = if args.force_rewrite {
        let lease_expect = Branch::find_branch_result(TRACES_BRANCH, Some(&remote))
            .await
            .map_err(|err| {
                CliError::fatal(format!(
                    "failed to read the '{remote}' tracking tip for '{TRACES_BRANCH}': {err}"
                ))
            })?
            .map(|branch| branch.commit.to_string());
        let Some(expect) = lease_expect else {
            // Never fall back to an unconditional force: without a recorded
            // last-pushed tip there is no lease basis, so fail closed.
            return Err(CliError::conflict(format!(
                "--force-rewrite has no recorded last-pushed tip for '{TRACES_REMOTE_REF}' \
                 on remote '{remote}' to lease against"
            ))
            .with_hint(
                "run 'libra agent push' once without --force-rewrite first — successful \
                 pushes record the tip this flag leases against",
            )
            .with_hint(format!(
                "if the remote was rewritten elsewhere, inspect it with 'libra ls-remote \
                 {remote}' and push manually with 'libra push {remote} \
                 {TRACES_BRANCH}:{TRACES_REMOTE_REF} \
                 --force-with-lease={TRACES_REMOTE_REF}:<remote-tip>'"
            )));
        };
        push_command::PushArgs::for_refspecs_with_lease(
            remote.clone(),
            vec![refspec],
            format!("{TRACES_REMOTE_REF}:{expect}"),
        )
    } else {
        push_command::PushArgs::for_refspecs(remote.clone(), vec![refspec])
    };

    match push_command::execute_safe(push_args, output).await {
        Ok(()) => {
            if let Some(tip) = local_tip {
                // Best-effort tracking record: a failure here only means a
                // later --force-rewrite leases against a stale tip and
                // fails closed, never that it forces blindly.
                if let Err(err) = Branch::update_branch(TRACES_BRANCH, &tip, Some(&remote)).await {
                    tracing::warn!(
                        remote = %remote,
                        error = %err,
                        "failed to record the traces remote-tracking tip after push"
                    );
                }
            }
            Ok(())
        }
        Err(err)
            if !args.force_rewrite
                && err.stable_code() == StableErrorCode::ConflictOperationBlocked =>
        {
            // The generic push hints ('libra pull' / '--force') are wrong
            // for the Libra-managed traces ref; the 2-hint budget means we
            // must front-insert ours so they replace the generic pair.
            Err(err
                .with_priority_hint(
                    "retry with 'libra agent push --force-rewrite' — it uses \
                     force-with-lease against the last tip this repository pushed, so a \
                     remote rewritten elsewhere still fails closed",
                )
                .with_priority_hint(format!(
                    "'{TRACES_REMOTE_REF}' is Libra-managed: 'libra agent clean' prunes \
                     rewrite the whole chain, so the next push is never fast-forward"
                )))
        }
        Err(err) => Err(err),
    }
}
