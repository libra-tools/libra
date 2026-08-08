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

/// Recognize a `__upgrade-probe …` invocation from raw argv (argv[0] is the
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
