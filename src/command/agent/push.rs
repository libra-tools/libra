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

use bytes::{Bytes, BytesMut};
use serde::Serialize;

use super::PushArgs;
use crate::{
    command::{fetch::RemoteClient, mega2_traces::git as traces_git, push as push_command},
    git_protocol::{ServiceType, add_pkt_line_string, read_pkt_line},
    info_println,
    internal::{
        branch::{Branch, TRACES_BRANCH},
        protocol::mega2_traces as mega2_traces_kind,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

const DEFAULT_TRACES_REMOTE: &str = "origin";
const TRACES_REMOTE_REF: &str = "refs/libra/traces";

pub async fn execute_safe(args: PushArgs, output: &OutputConfig) -> CliResult<()> {
    if let Err(message) = crate::internal::protocol::mega2_traces::admit_before_network(
        args.mega2,
        args.force_rewrite,
    ) {
        return Err(CliError::conflict(message).with_hint(
            "Mega2 traces mode does not turn a rejected update into a force or a new epoch",
        ));
    }
    if args.mega2 {
        return execute_mega2(args, output).await;
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

async fn execute_mega2(args: PushArgs, output: &OutputConfig) -> CliResult<()> {
    use super::mega2_stream::{self, EpochIntent};
    use crate::internal::protocol::mega2_traces::{self, Admission, ByPathLocator};

    let workdir = std::env::current_dir().map_err(|error| {
        CliError::fatal(format!(
            "cannot resolve the working directory for the Mega2 traces push: {error}"
        ))
    })?;
    let intent = if args.new_stream {
        EpochIntent::NewStream
    } else {
        EpochIntent::Current
    };
    let remote = args
        .remote
        .clone()
        .unwrap_or_else(|| DEFAULT_TRACES_REMOTE.to_string());
    let binding = mega2_stream::bind_source_epoch_at(&workdir, intent)
        .await
        .map_err(|error| {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
    let stopped = |reason: String| {
        CliError::fatal(format!(
            "{reason}; the Mega2 traces push stopped before any network attempt. Saved epoch {} will be reused on retry",
            binding.epoch
        ))
    };
    let url_key = format!("remote.{remote}.url");
    let url = mega2_stream::repository_config_at(&workdir, &url_key)
        .await
        .map_err(|error| CliError::fatal(error.to_string()))?;
    let Some(url) = url.filter(|value| !value.is_empty()) else {
        return Err(stopped(format!("remote '{remote}' has no URL"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("set the remote URL, then retry without --new-stream unless you mean to open another epoch"));
    };
    let ssh = url.starts_with("ssh://");
    if !ssh && !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(stopped(format!(
            "remote '{remote}' is not an http(s):// or ssh:// Mega2 URL"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    let target = match args.target.as_deref() {
        Some(raw) => mega2_traces::normalize_target_path(raw).map_err(|message| {
            CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidArguments)
        })?,
        None => mega2_traces::target_from_remote_url(&url).ok_or_else(|| {
            stopped(format!(
                "remote '{remote}' URL does not name a Mega2 project path"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("pass --target /project/<name> to choose the project that owns these traces")
        })?,
    };
    let tip = Branch::find_branch_result(TRACES_BRANCH, None)
        .await
        .map_err(|err| {
            CliError::fatal(format!(
                "failed to read the local '{TRACES_BRANCH}' branch: {err}"
            ))
        })?
        .map(|branch| branch.commit);
    let Some(tip) = tip else {
        return Err(stopped(format!(
            "the local '{TRACES_BRANCH}' branch has no checkpoint commit"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("capture a checkpoint with an enabled agent, then retry"));
    };
    let tip_hex = tip.to_string();
    let repo_kind = git_internal::hash::get_hash_kind();
    let traces_kind = match repo_kind {
        git_internal::hash::HashKind::Sha1 => mega2_traces::HashKind::Sha1,
        git_internal::hash::HashKind::Sha256 => mega2_traces::HashKind::Sha256,
        other => {
            return Err(stopped(format!(
                "repository object format {other:?} is not sha1 or sha256"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid));
        }
    };
    let token = if ssh {
        None
    } else {
        let (token, _) = crate::internal::protocol::mega2_auth::resolve_token_from_process(
            args.token_file.as_deref(),
            None,
        )?;
        Some(token.ok_or_else(|| {
            stopped("the HTTP traces push has no token".to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("set LIBRA_MEGA2_TOKEN or pass --token-file <PATH>")
        })?)
    };

    let connection = mega2_stream::open_repository_at(&workdir)
        .await
        .map_err(|error| CliError::fatal(error.to_string()))?;
    let reuse = mega2_stream::reusable_scope(&connection, &target, &binding.epoch)
        .await
        .map_err(|error| CliError::fatal(error.to_string()))?;
    mega2_stream::begin_push_attempt(&connection, EpochIntent::Current, &tip_hex)
        .await
        .map_err(|error| {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoStateInvalid)
        })?;
    let locator_path = match &reuse {
        Some(reuse) => mega2_traces::by_scope_git_path(&reuse.scope),
        None => mega2_traces::by_path_git_path(&ByPathLocator {
            target_path: target.clone(),
            source_id: binding.source.clone(),
            epoch: binding.epoch.clone(),
            hash_kind: traces_kind,
        }),
    }
    .map_err(CliError::fatal)?;
    let client = crate::command::mega2_traces::git::open_client(
        &url,
        &remote,
        &locator_path,
        token.as_ref(),
        output,
    )
    .await?;

    let discovery = traces_git::discover(&client, ServiceType::ReceivePack).await?;
    if discovery.hash_kind != repo_kind {
        return Err(CliError::fatal(format!(
            "traces remote object format {:?} does not match the repository format {repo_kind:?}",
            discovery.hash_kind
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid));
    }
    let advertisement = discovery.capabilities.join(" ");
    let remote_tip = discovery
        .refs
        .iter()
        .find(|reference| reference.name() == TRACES_REMOTE_REF)
        .map(|reference| reference.hash().to_string());
    let up_to_date = remote_tip.as_deref() == Some(tip_hex.as_str());
    let non_fast_forward = match remote_tip.as_deref() {
        None => false,
        Some(old) if old == tip_hex => false,
        Some(old) => match crate::internal::object_format::parse_repo_oid(old) {
            Ok(old) => !push_command::is_ancestor(&old, &tip),
            Err(_) => true,
        },
    };
    if let Admission::Reject { message, .. } = mega2_traces::admit_scoped_push(
        true,
        false,
        ssh,
        &advertisement,
        non_fast_forward,
        &locator_path,
    ) {
        let hint = if non_fast_forward {
            "a local `libra agent clean` rewrote the traces chain; open a new epoch with `libra agent push --mega2 --new-stream`"
        } else {
            "check that the remote is a Mega2 server with traces enabled"
        };
        return Err(CliError::conflict(message)
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint(hint));
    }
    if let (Some(reuse), Some(advertised)) =
        (&reuse, mega2_traces::advertised_scope(&advertisement))
        && advertised != reuse.scope
    {
        return Err(CliError::fatal(format!(
            "traces remote advertised scope {advertised}, not the requested {}",
            reuse.scope
        ))
        .with_stable_code(StableErrorCode::NetworkProtocol));
    }

    let mut objects_pushed = 0usize;
    if !up_to_date {
        let (entries, warnings) = push_command::collect_objects_for_tip(tip, &discovery.refs)
            .await
            .map_err(CliError::from)?;
        for warning in warnings {
            crate::utils::error::emit_warning(warning);
        }
        objects_pushed = entries.len();
        // Window 0: no delta entries. The TraceRepo refuses delta PACKs.
        let pack = crate::internal::pack_writer::encode_pack_bytes(entries, repo_kind)
            .await
            .map_err(|error| CliError::fatal(format!("cannot encode the traces pack: {error}")))?;
        let old = remote_tip
            .clone()
            .unwrap_or_else(|| "0".repeat(repo_kind.hex_len()));
        let request = receive_request(&old, &tip_hex, traces_kind, &pack);
        let response = traces_git::retry_busy(|| send(&client, request.clone())).await?;
        check_receive_status(response)?;
    }

    // The standard `ok` line carries no scope UUID. A read-only discovery
    // learns it; a failure there keeps the push successful (pending scope).
    let scope = match mega2_traces::advertised_scope(&advertisement) {
        Some(scope) if up_to_date => Some(scope),
        _ => match traces_git::discover(&client, ServiceType::ReceivePack).await {
            Ok(after) => mega2_traces::advertised_scope(&after.capabilities.join(" ")),
            Err(error) => {
                tracing::warn!(error = %error.message(), "traces scope discovery after push failed");
                None
            }
        },
    };
    let receipt = mega2_stream::confirm_push_attempt(&connection, &tip_hex, scope.as_deref())
        .await
        .map_err(|error| CliError::fatal(error.to_string()))?;
    mega2_stream::record_receipt_binding(
        &connection,
        &target,
        &binding.epoch,
        receipt.scope.as_deref(),
    )
    .await
    .map_err(|error| CliError::fatal(error.to_string()))?;

    let report = Mega2PushReport {
        remote,
        transport: if ssh { "ssh" } else { "http" },
        target,
        source: binding.source,
        epoch: binding.epoch,
        locator: locator_path,
        old_tip: remote_tip,
        tip: tip_hex,
        up_to_date,
        objects_pushed,
        scope: receipt.scope,
        state: receipt.state,
    };
    if output.is_json() {
        return emit_json_data("agent_push", &report, output);
    }
    let scope_text = report
        .scope
        .clone()
        .unwrap_or_else(|| mega2_stream::STATE_SCOPE_PENDING.to_string());
    if report.up_to_date {
        info_println!(
            output,
            "Mega2 traces already at {} (scope {scope_text}, epoch {})",
            report.tip,
            report.epoch
        );
    } else {
        info_println!(
            output,
            "pushed {} to Mega2 traces {} over {} ({} objects; scope {scope_text}, epoch {})",
            report.tip,
            report.target,
            report.transport,
            report.objects_pushed,
            report.epoch
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct Mega2PushReport {
    remote: String,
    transport: &'static str,
    target: String,
    source: String,
    epoch: String,
    locator: String,
    old_tip: Option<String>,
    tip: String,
    up_to_date: bool,
    objects_pushed: usize,
    scope: Option<String>,
    state: String,
}

fn receive_request(old: &str, new: &str, kind: mega2_traces_kind::HashKind, pack: &[u8]) -> Bytes {
    let mut capabilities = String::from("report-status");
    if kind == mega2_traces_kind::HashKind::Sha256 {
        capabilities.push_str(" object-format=sha256");
    }
    capabilities.push_str(&format!(" agent=libra/{}", env!("CARGO_PKG_VERSION")));
    let mut data = BytesMut::new();
    add_pkt_line_string(
        &mut data,
        format!("{old} {new} {TRACES_REMOTE_REF}\0{capabilities}\n"),
    );
    data.extend_from_slice(b"0000");
    data.extend_from_slice(pack);
    data.freeze()
}

async fn send(client: &RemoteClient, request: Bytes) -> CliResult<Bytes> {
    match client {
        RemoteClient::Ssh(client) => client.send_pack(request).await.map_err(|error| {
            let detail = error.to_string();
            if detail.contains("traces locator not found") {
                traces_git::remote_error("traces receive-pack", &detail)
            } else {
                CliError::network(format!(
                    "traces receive-pack over SSH failed (the remote kept its previous tip): {detail}"
                ))
                .with_stable_code(StableErrorCode::NetworkProtocol)
            }
        }),
        RemoteClient::Http(client) => {
            let response = client.send_pack(request).await.map_err(|error| {
                CliError::network(format!(
                    "traces receive-pack request failed: {}",
                    crate::utils::redact::redact_url_credentials(&error.to_string())
                ))
                .with_stable_code(StableErrorCode::NetworkUnavailable)
            })?;
            let status = response.status().as_u16();
            let body = response.bytes().await.map_err(|error| {
                CliError::network(format!("traces receive-pack response read failed: {error}"))
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
            })?;
            if status == 200 {
                Ok(body)
            } else {
                Err(http_rejection(status, &body))
            }
        }
        _ => Err(CliError::internal("unexpected traces transport")),
    }
}

const SERVER_CODES: &[&str] = &[
    "unauthenticated",
    "unknown_scope",
    "invalid_parameter",
    "invalid_cursor",
    "unsupported_schema",
    "invalid_capture",
    "object_integrity",
    "response_budget",
    "query_timeout",
    "search_unavailable",
    "trace_history_in_code",
];

fn http_rejection(status: u16, body: &[u8]) -> CliError {
    let text = String::from_utf8_lossy(&body[..body.len().min(4096)]);
    let code = SERVER_CODES
        .iter()
        .find(|code| text.contains(*code))
        .copied()
        .unwrap_or("unknown");
    let message = match crate::internal::protocol::mega2_traces::propagate_server_rejection(code) {
        Err(message) => format!("{message} (HTTP {status}); the remote kept its previous tip"),
        Ok(()) => format!("traces remote rejected the push (HTTP {status})"),
    };
    let stable = match status {
        401 | 403 | 404 => StableErrorCode::AuthPermissionDenied,
        503 => StableErrorCode::NetworkUnavailable,
        _ => StableErrorCode::ConflictOperationBlocked,
    };
    let error = if stable == StableErrorCode::AuthPermissionDenied {
        CliError::auth(message)
    } else {
        CliError::conflict(message)
    };
    error.with_stable_code(stable)
}

/// Accept only `unpack ok` and `ok refs/libra/traces` followed by a flush.
fn check_receive_status(mut body: Bytes) -> CliResult<()> {
    let protocol = |detail: &str| {
        CliError::network(format!("traces receive-pack status is invalid: {detail}"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
    };
    let mut lines = Vec::new();
    loop {
        if body.is_empty() {
            return Err(protocol("missing status flush"));
        }
        let (len, line) = read_pkt_line(&mut body).map_err(|error| protocol(&error.to_string()))?;
        if len == 0 {
            break;
        }
        lines.push(String::from_utf8_lossy(&line).trim_end().to_string());
    }
    match lines.first().map(String::as_str) {
        Some("unpack ok") => {}
        Some(other) => {
            return Err(CliError::conflict(format!(
                "traces remote did not unpack the push: {other}"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked));
        }
        None => return Err(protocol("empty status")),
    }
    let expected = format!("ok {TRACES_REMOTE_REF}");
    for line in &lines[1..] {
        if *line == expected {
            return Ok(());
        }
        if let Some(rest) = line.strip_prefix(&format!("ng {TRACES_REMOTE_REF} ")) {
            return Err(CliError::conflict(format!(
                "traces remote rejected {TRACES_REMOTE_REF}: {rest}"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked));
        }
    }
    Err(protocol("no status for refs/libra/traces"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(lines: &[&str]) -> Bytes {
        let mut data = BytesMut::new();
        for line in lines {
            add_pkt_line_string(&mut data, format!("{line}\n"));
        }
        data.extend_from_slice(b"0000");
        data.freeze()
    }

    #[test]
    fn mega2_receive_status_matrix() {
        assert!(check_receive_status(frame(&["unpack ok", "ok refs/libra/traces"])).is_ok());
        let rejected = check_receive_status(frame(&[
            "unpack ok",
            "ng refs/libra/traces non-fast-forward",
        ]))
        .expect_err("ng");
        assert!(rejected.message().contains("non-fast-forward"));
        assert!(check_receive_status(frame(&["unpack failed"])).is_err());
        assert!(check_receive_status(frame(&["unpack ok"])).is_err());
        assert!(check_receive_status(Bytes::from_static(b"000eunpack ok\n")).is_err());

        let request = receive_request(
            &"0".repeat(40),
            &"a".repeat(40),
            mega2_traces_kind::HashKind::Sha1,
            b"PACK",
        );
        let text = String::from_utf8_lossy(&request);
        assert!(text.contains("refs/libra/traces\0report-status agent=libra/"));
        assert!(!text.contains("object-format"));
        assert!(text.ends_with("0000PACK"));
        let sha256 = receive_request(
            &"0".repeat(64),
            &"b".repeat(64),
            mega2_traces_kind::HashKind::Sha256,
            b"",
        );
        assert!(String::from_utf8_lossy(&sha256).contains("object-format=sha256"));

        let denied = http_rejection(404, br#"{"err_message":"unknown_scope"}"#);
        assert!(denied.message().contains("unknown_scope"));
        assert_eq!(denied.stable_code(), StableErrorCode::AuthPermissionDenied);
        let integrity = http_rejection(409, b"object_integrity");
        assert_eq!(
            integrity.stable_code(),
            StableErrorCode::ConflictOperationBlocked
        );
    }
}
