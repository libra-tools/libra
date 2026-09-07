//! Auto-upgrade CLI surface (plan-20260714 §A.7/§A.10).
//!
//! The only user-visible aspect here is the HIDDEN, front-of-argv
//! `__upgrade-probe` entry: the auto-upgrade machinery spawns a downloaded
//! candidate (and, after install, the installed target) as
//! `libra __upgrade-probe --kind <version|pre-install|post-install>
//! --expected-version <X.Y.Z>` to self-check it. The probe is recognized at
//! the very front of argv parsing, BEFORE clap, repo preflight, schema
//! migration, transaction recovery, config writes and background tasks
//! (§A.7): it performs ONLY a side-effect-free identity self-check and exits,
//! never forwarding to a real user command.
//!
//! Because it is front-scanned (like `help error-codes`) rather than a clap
//! subcommand, it is invisible to help, the Command-Groups banner, and every
//! `docs`/`COMPATIBILITY` compat guard — no allowlist edits are required.

use crate::utils::error::{CliError, CliResult};

/// The literal front-of-argv token that selects the probe entry.
pub const UPGRADE_PROBE_TOKEN: &str = "__upgrade-probe";

/// Parsed probe request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub kind: String,
    pub expected_version: String,
}

/// Recognize a `__upgrade-probe …` invocation from raw argv (the first argv element is the
/// program name). Returns `None` for every other command so normal dispatch
/// is untouched.
///
/// The grammar is fixed and closed: exactly
/// `__upgrade-probe --kind <k> --expected-version <v>` (order-independent,
/// each flag once). Anything else returns a rejection so a malformed probe
/// invocation fails closed rather than silently self-checking.
pub fn parse_probe_argv(
    argv: &[std::ffi::OsString],
) -> Option<Result<ProbeRequest, ProbeArgError>> {
    // Positions are taken from the RAW argv, never from a filtered view: the
    // probe token must be argv[1] exactly. Filtering non-UTF-8 tokens out
    // first would shift indices, so `libra <non-utf8> __upgrade-probe …`
    // would be accepted as a probe — a command line that is not one.
    if argv.get(1)?.to_str() != Some(UPGRADE_PROBE_TOKEN) {
        return None;
    }
    // The tail is a fixed set of ASCII flags and values; a non-UTF-8 token in
    // it is a malformed probe, reported as such rather than dropped.
    let mut tail = Vec::with_capacity(argv.len().saturating_sub(2));
    for arg in &argv[2..] {
        match arg.to_str() {
            Some(text) => tail.push(text.to_string()),
            None => {
                return Some(Err(ProbeArgError::Unexpected(
                    arg.to_string_lossy().into_owned(),
                )));
            }
        }
    }
    Some(parse_probe_tail(&tail))
}

/// Malformed probe argv (still consumed by the front entry — never forwarded).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeArgError {
    #[error("unexpected argument '{0}' for __upgrade-probe")]
    Unexpected(String),
    #[error("--{0} was supplied more than once")]
    Duplicate(&'static str),
    #[error("--{0} requires a value")]
    MissingValue(&'static str),
    #[error("--kind must be one of version, pre-install, post-install")]
    BadKind,
    #[error("--kind and --expected-version are both required")]
    MissingRequired,
}

fn parse_probe_tail(tail: &[String]) -> Result<ProbeRequest, ProbeArgError> {
    let mut kind: Option<String> = None;
    let mut expected: Option<String> = None;
    let mut i = 0;
    while i < tail.len() {
        match tail[i].as_str() {
            "--kind" => {
                if kind.is_some() {
                    return Err(ProbeArgError::Duplicate("kind"));
                }
                let value = tail.get(i + 1).ok_or(ProbeArgError::MissingValue("kind"))?;
                if !matches!(value.as_str(), "version" | "pre-install" | "post-install") {
                    return Err(ProbeArgError::BadKind);
                }
                kind = Some(value.clone());
                i += 2;
            }
            "--expected-version" => {
                if expected.is_some() {
                    return Err(ProbeArgError::Duplicate("expected-version"));
                }
                let value = tail
                    .get(i + 1)
                    .ok_or(ProbeArgError::MissingValue("expected-version"))?;
                expected = Some(value.clone());
                i += 2;
            }
            other => return Err(ProbeArgError::Unexpected(other.to_string())),
        }
    }
    match (kind, expected) {
        (Some(kind), Some(expected_version)) => Ok(ProbeRequest {
            kind,
            expected_version,
        }),
        _ => Err(ProbeArgError::MissingRequired),
    }
}

/// The running binary's compiled version — the identity a probe checks.
fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Execute a probe request and return the process result. Success (exit 0)
/// means the running binary IS the expected version; any mismatch or
/// malformed request is a silent nonzero exit so the caller fails closed.
///
/// The check is intentionally minimal and side-effect free: it reads only the
/// compile-time version, touches no repository, config, network or filesystem
/// state, and prints nothing (the probe is spawned with null stdio anyway).
pub fn run_probe(request: Result<ProbeRequest, ProbeArgError>) -> CliResult<()> {
    let healthy = match request {
        Ok(req) => req.expected_version == running_version(),
        Err(_) => false,
    };
    if healthy {
        Ok(())
    } else {
        // Silent nonzero exit — the orchestrator interprets any failure as an
        // unhealthy candidate and rolls back / discards it.
        Err(CliError::silent_exit(1))
    }
}

/// Whether `argv` selects the probe entry (used by the CLI front scan to
/// decide before doing anything else).
pub fn is_probe_invocation(argv: &[std::ffi::OsString]) -> bool {
    argv.get(1).and_then(|arg| arg.to_str()) == Some(UPGRADE_PROBE_TOKEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<std::ffi::OsString> {
        parts.iter().map(std::ffi::OsString::from).collect()
    }

    /// A non-UTF-8 token BEFORE the probe token must not be filtered away:
    /// the probe is recognised by position, so dropping it would turn a
    /// command line that is not a probe into one.
    #[cfg(unix)]
    #[test]
    fn a_token_before_the_probe_token_is_not_skipped() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let argv = vec![
            OsString::from("libra"),
            OsString::from_vec(vec![0xff]),
            OsString::from(UPGRADE_PROBE_TOKEN),
            OsString::from("--kind"),
            OsString::from("version"),
        ];
        assert!(
            parse_probe_argv(&argv).is_none(),
            "the probe token must be argv[1] exactly"
        );
    }

    /// A non-UTF-8 token INSIDE the probe tail is a malformed probe, not a
    /// silently dropped argument.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_probe_tail_token_is_rejected() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let argv = vec![
            OsString::from("libra"),
            OsString::from(UPGRADE_PROBE_TOKEN),
            OsString::from_vec(vec![0xff]),
        ];
        assert!(matches!(
            parse_probe_argv(&argv),
            Some(Err(ProbeArgError::Unexpected(_)))
        ));
    }

    #[test]
    fn non_probe_argv_is_ignored() {
        assert!(parse_probe_argv(&argv(&["libra", "status"])).is_none());
        assert!(parse_probe_argv(&argv(&["libra"])).is_none());
        assert!(!is_probe_invocation(&argv(&["libra", "commit"])));
    }

    #[test]
    fn well_formed_probe_parses_all_kinds() {
        for kind in ["version", "pre-install", "post-install"] {
            let parsed = parse_probe_argv(&argv(&[
                "libra",
                "__upgrade-probe",
                "--kind",
                kind,
                "--expected-version",
                "1.2.3",
            ]))
            .unwrap()
            .unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.expected_version, "1.2.3");
        }
        // Order-independent.
        let parsed = parse_probe_argv(&argv(&[
            "libra",
            "__upgrade-probe",
            "--expected-version",
            "9.9.9",
            "--kind",
            "version",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(parsed.expected_version, "9.9.9");
    }

    #[test]
    fn malformed_probe_argv_is_rejected_not_forwarded() {
        // Still recognized as a probe invocation (Some), but an Err tail.
        for bad in [
            vec!["libra", "__upgrade-probe"],
            vec!["libra", "__upgrade-probe", "--kind", "version"],
            vec![
                "libra",
                "__upgrade-probe",
                "--kind",
                "bogus",
                "--expected-version",
                "1.0.0",
            ],
            vec![
                "libra",
                "__upgrade-probe",
                "--kind",
                "version",
                "--kind",
                "version",
                "--expected-version",
                "1.0.0",
            ],
            vec!["libra", "__upgrade-probe", "--kind"],
            vec!["libra", "__upgrade-probe", "status"],
            vec!["libra", "__upgrade-probe", "--expected-version", "1.0.0"],
        ] {
            let parsed = parse_probe_argv(&argv(&bad)).expect("recognized as probe");
            assert!(parsed.is_err(), "{bad:?} should be a malformed probe");
            // And run_probe fails closed on it.
            assert!(run_probe(parsed).is_err());
        }
    }

    #[test]
    fn probe_passes_only_for_the_running_version() {
        let ok = ProbeRequest {
            kind: "version".into(),
            expected_version: running_version().to_string(),
        };
        assert!(run_probe(Ok(ok)).is_ok());
        let mismatch = ProbeRequest {
            kind: "post-install".into(),
            expected_version: "0.0.0-not-this".into(),
        };
        let err = run_probe(Ok(mismatch)).unwrap_err();
        assert_eq!(err.exit_code(), 1);
    }
}

// ─── the visible `libra upgrade` command (manual signed-channel upgrade) ─────

use std::io::{IsTerminal, Write};

use clap::Parser;

use crate::{
    internal::upgrade::{
        flow::FlowError,
        orchestrator::{
            ManualCheckOutcome, ManualInstallReport, ManualUpgrade, ManualUpgradeError,
            manual_upgrade_check,
        },
    },
    utils::{error::StableErrorCode, output::OutputConfig},
};

/// Install-script one-liner surfaced whenever this binary cannot upgrade
/// itself (dev build / unofficial install).
const INSTALL_SCRIPT_HINT: &str =
    "install the official build: curl -fsSL https://download.libra.tools/install.sh | sh";

pub const UPGRADE_EXAMPLES: &str = "\
EXAMPLES:
  libra upgrade                 Check the signed release channel; ask before installing
  libra upgrade --check         Only report whether a newer version exists
  libra upgrade --yes           Install a newer version without the confirmation prompt
  libra --json upgrade --check  Machine-readable status for scripts

The check and the install both run the fully verified pipeline: the Ed25519-signed
stable manifest, anti-rollback floors, sha256/size-enforced download, and a locked
install transaction with a self-check probe and automatic rollback. Machine modes
(--json/--machine) never prompt: combine them with --check or --yes.
";

#[derive(Parser, Debug)]
pub struct UpgradeArgs {
    /// Only check whether a newer signed version exists; never install
    #[clap(long, conflicts_with = "yes")]
    pub check: bool,
    /// Install a newer version without asking for confirmation
    #[clap(short = 'y', long)]
    pub yes: bool,
}

/// `libra upgrade` — check the signed stable channel and (after confirmation)
/// replace the running binary with the latest release.
pub async fn execute_safe(args: UpgradeArgs, output: &OutputConfig) -> CliResult<()> {
    let local_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let outcome = manual_upgrade_check(local_now)
        .await
        .map_err(|e| manual_error_to_cli("upgrade check failed", e))?;

    match outcome {
        ManualCheckOutcome::NotOfficialInstall => {
            if args.check {
                report_status(
                    output,
                    "not_official_install",
                    None,
                    None,
                    "this libra binary is not an official signed install, so it cannot report \
                     or install signed releases for itself",
                )?;
                return Ok(());
            }
            Err(CliError::failure(
                "this libra binary is not an official signed install, so it cannot upgrade itself",
            )
            .with_stable_code(StableErrorCode::Unsupported)
            .with_hint(INSTALL_SCRIPT_HINT)
            .with_hint("for a source checkout, pull and rebuild instead (cargo build --release)"))
        }
        ManualCheckOutcome::UnsupportedPlatform => {
            if args.check {
                report_status(
                    output,
                    "unsupported_platform",
                    None,
                    None,
                    "this platform is outside the signed release matrix; upgrades are not \
                     published for it",
                )?;
                return Ok(());
            }
            Err(CliError::failure(
                "this platform is outside the signed release matrix, so there is nothing to \
                 install",
            )
            .with_stable_code(StableErrorCode::Unsupported)
            .with_hint("supported platforms: linux-amd64, linux-arm64, darwin-arm64"))
        }
        ManualCheckOutcome::UpToDate { installed, latest } => {
            report_status(
                output,
                "up_to_date",
                Some(installed.to_string()),
                Some(latest.to_string()),
                &format!("libra v{installed} is up to date (latest signed release: v{latest})"),
            )?;
            Ok(())
        }
        ManualCheckOutcome::Paused { installed } => {
            report_status(
                output,
                "paused",
                Some(installed.to_string()),
                None,
                "the publisher has PAUSED releases (emergency stop) — no upgrade is offered \
                 right now; staying on the current version",
            )?;
            Ok(())
        }
        ManualCheckOutcome::RevokedLatest { installed, revoked } => {
            report_status(
                output,
                "latest_revoked",
                Some(installed.to_string()),
                Some(revoked.to_string()),
                &format!(
                    "the latest published version v{revoked} is REVOKED by the publisher — \
                     staying on v{installed} until a fixed release ships"
                ),
            )?;
            Ok(())
        }
        ManualCheckOutcome::Available(upgrade) => run_available(args, output, *upgrade).await,
    }
}

async fn run_available(
    args: UpgradeArgs,
    output: &OutputConfig,
    upgrade: ManualUpgrade,
) -> CliResult<()> {
    let installed = upgrade.installed();
    let latest = upgrade.latest();
    let size_mb = upgrade.artifact_size() as f64 / 1_048_576.0;

    if args.check {
        // The accepted manifest's floors are already durable (the check
        // persisted them before returning Available).
        report_status(
            output,
            "available",
            Some(installed.to_string()),
            Some(latest.to_string()),
            &format!(
                "a newer version is available: v{installed} -> v{latest} ({size_mb:.1} MB, \
                 signed) — run `libra upgrade` to install"
            ),
        )?;
        return Ok(());
    }

    let proceed = if args.yes {
        true
    } else if output.is_json() || output.quiet {
        // Machine/quiet modes never prompt: stdout must stay a clean
        // machine document and quiet must stay quiet.
        return Err(CliError::failure(format!(
            "a newer version v{latest} is available, but machine/quiet output modes never \
             prompt for confirmation"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("re-run with --yes to install non-interactively")
        .with_hint("or use --check to only report availability"));
    } else if std::io::stdin().is_terminal() {
        println!("  installed  v{installed}");
        println!("  latest     v{latest}  ({size_mb:.1} MB, signed stable channel)");
        println!();
        confirm(&format!("Upgrade to v{latest} now?")).map_err(|error| {
            CliError::failure(format!("could not read the confirmation answer: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?
    } else {
        return Err(CliError::failure(format!(
            "a newer version v{latest} is available, but stdin is not a terminal so libra \
             cannot ask for confirmation"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("re-run with --yes to install non-interactively")
        .with_hint("or use --check to only report availability"));
    };

    if !proceed {
        report_status(
            output,
            "declined",
            Some(installed.to_string()),
            Some(latest.to_string()),
            &format!("upgrade cancelled — staying on v{installed}"),
        )?;
        return Ok(());
    }

    if !output.is_json() && !output.quiet {
        println!("  downloading v{latest} ({size_mb:.1} MB, sha256-verified) ...");
    }
    let report = upgrade
        .install()
        .await
        .map_err(|e| manual_error_to_cli("upgrade failed", e))?;
    match report {
        ManualInstallReport::Installed(version) => {
            report_status(
                output,
                "installed",
                Some(installed.to_string()),
                Some(version.to_string()),
                &format!(
                    "upgraded to v{version} — the new version takes effect on your next command"
                ),
            )?;
            Ok(())
        }
        ManualInstallReport::ControlChanged { detail } => {
            // The publisher's decision changed while the prompt was open;
            // refusing the stale plan is CORRECT — but an install was
            // REQUESTED and did not happen, so the exit is non-zero (a CI
            // `upgrade --yes` must not sail on as if it upgraded).
            Err(CliError::failure(format!(
                "nothing was installed — the publisher's control decision changed while \
                 confirming: {detail}"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint("run `libra upgrade` again to see the current state"))
        }
        ManualInstallReport::RolledBack => Err(CliError::failure(
            "the previous binary was RESTORED — the downloaded version failed its \
             post-install self-check, a newer publisher control decision superseded it at \
             the last moment, or the policy fence could not obtain its lock in time; \
             nothing changed",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint(
            "try again later; if this repeats, report it at github.com/libra-tools/libra/issues",
        )),
        ManualInstallReport::NotApplied => Err(CliError::failure(
            "the upgrade was not applied: another libra process is upgrading concurrently, \
             or the candidate failed its pre-install check",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("re-run `libra upgrade` in a moment")),
    }
}

/// Emit one status line (human) or one JSON document (machine mode).
fn report_status(
    output: &OutputConfig,
    status: &str,
    installed: Option<String>,
    latest: Option<String>,
    human: &str,
) -> CliResult<()> {
    if output.is_json() {
        return crate::utils::output::emit_json_data(
            "upgrade",
            &serde_json::json!({
                "status": status,
                "installed": installed,
                "latest": latest,
            }),
            output,
        );
    }
    if !output.quiet {
        match status {
            "up_to_date" | "installed" => println!("✓ {human}"),
            "paused" | "latest_revoked" | "control_changed" => println!("! {human}"),
            _ => println!("{human}"),
        }
    }
    Ok(())
}

/// `y`/`yes` (any case) accepts; everything else — including EOF — declines.
fn parse_confirmation(raw: &str) -> bool {
    matches!(raw.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The prompt goes to STDERR so a piped stdout never receives it.
fn confirm(question: &str) -> std::io::Result<bool> {
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line)?;
    if read == 0 {
        // EOF declines (the [y/N] default).
        eprintln!();
        return Ok(false);
    }
    Ok(parse_confirmation(&line))
}

/// Map a manual-flow error onto the closest stable code: network transport
/// issues are NET-001, protocol/verification violations NET-002, persisted
/// anti-rollback rejections and upgrade-state problems REPO-003 (the
/// upgrade subsystem's established state code), floor-write failures the IO
/// write code.
fn manual_error_to_cli(prefix: &str, error: ManualUpgradeError) -> CliError {
    use crate::internal::upgrade::http::UpgradeHttpError;
    let code = match &error {
        // Transport-level trouble (unreachable, TLS, HTTP status, stall).
        ManualUpgradeError::Fetch(
            UpgradeHttpError::Request { .. }
            | UpgradeHttpError::ClientBuild(_)
            | UpgradeHttpError::Status { .. }
            | UpgradeHttpError::Sink(_),
        )
        | ManualUpgradeError::Timeout(_) => StableErrorCode::NetworkUnavailable,
        // The peer violated the channel's contract (redirects, wrong URL,
        // size bounds, digest mismatch, non-https) — protocol, not weather.
        ManualUpgradeError::Fetch(_) => StableErrorCode::NetworkProtocol,
        // A missing/out-of-lifetime HTTPS Date is the PEER violating the
        // channel contract, not local state damage.
        ManualUpgradeError::Verify(FlowError::State(
            crate::internal::upgrade::state::StateRejection::MissingHttpsDate
            | crate::internal::upgrade::state::StateRejection::HttpsDateOutsideLifetime { .. },
        )) => StableErrorCode::NetworkProtocol,
        ManualUpgradeError::Verify(FlowError::State(_)) => StableErrorCode::RepoStateInvalid,
        ManualUpgradeError::Verify(_) => StableErrorCode::NetworkProtocol,
        ManualUpgradeError::State(_) | ManualUpgradeError::Txn(_) => {
            StableErrorCode::RepoStateInvalid
        }
        ManualUpgradeError::FloorPersist(_) => StableErrorCode::IoWriteFailed,
    };
    let mut cli = CliError::failure(format!("{prefix}: {error}")).with_stable_code(code);
    cli = match &error {
        ManualUpgradeError::Fetch(_) | ManualUpgradeError::Timeout(_) => {
            cli.with_hint("check the network connection and retry")
        }
        ManualUpgradeError::Txn(_) => cli.with_hint(
            "the next libra command runs automatic recovery for any interrupted transaction",
        ),
        ManualUpgradeError::FloorPersist(_) => cli.with_hint(
            "the install directory next to the libra binary must be writable by this user",
        ),
        _ => cli,
    };
    cli
}

#[cfg(test)]
mod upgrade_cli_tests {
    use super::*;

    #[test]
    fn confirmation_accepts_only_yes_forms() {
        for yes in ["y", "Y", "yes", "YES", " y \n"] {
            assert!(parse_confirmation(yes), "{yes:?} must accept");
        }
        for no in ["", "n", "no", "nope", "yy", "es", "yes!", "\n"] {
            assert!(!parse_confirmation(no), "{no:?} must decline");
        }
    }

    #[test]
    fn check_and_yes_are_mutually_exclusive() {
        use clap::Parser as _;
        assert!(UpgradeArgs::try_parse_from(["upgrade", "--check", "--yes"]).is_err());
        assert!(UpgradeArgs::try_parse_from(["upgrade", "--check"]).is_ok());
        assert!(UpgradeArgs::try_parse_from(["upgrade", "-y"]).is_ok());
    }

    #[test]
    fn error_mapping_distinguishes_state_rejections_from_protocol() {
        // Anti-rollback/state rejections are LBR-REPO-003, not a network code.
        let state_rejection = ManualUpgradeError::State("corrupt".into());
        assert_eq!(
            manual_error_to_cli("x", state_rejection).stable_code(),
            StableErrorCode::RepoStateInvalid
        );
        let floors = ManualUpgradeError::FloorPersist("read-only".into());
        assert_eq!(
            manual_error_to_cli("x", floors).stable_code(),
            StableErrorCode::IoWriteFailed
        );
        let timeout = ManualUpgradeError::Timeout("manifest fetch");
        assert_eq!(
            manual_error_to_cli("x", timeout).stable_code(),
            StableErrorCode::NetworkUnavailable
        );
        // Digest mismatches are the peer violating the contract (NET-002),
        // not transport weather (NET-001).
        let digest = ManualUpgradeError::Fetch(
            crate::internal::upgrade::http::UpgradeHttpError::DigestMismatch {
                expected: "a".repeat(64),
                actual: "b".repeat(64),
            },
        );
        assert_eq!(
            manual_error_to_cli("x", digest).stable_code(),
            StableErrorCode::NetworkProtocol
        );
    }
}
