//! Implements `ls-remote` to list refs advertised by a remote repository.

use std::{io::Write, path::Path};

use clap::Parser;
use git_internal::errors::GitError;
use serde::Serialize;
use url::Url;

use crate::{
    command::fetch::{RemoteClient, resolve_remote_default_branch},
    git_protocol::ServiceType::UploadPack,
    internal::{
        config::ConfigKv,
        protocol::{DiscRef, ssh_client::is_ssh_spec},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[path = "ls_remote_filter.rs"]
mod ls_remote_filter;
#[path = "ls_remote_redaction.rs"]
mod ls_remote_redaction;
#[cfg(test)]
#[path = "ls_remote_tests.rs"]
mod ls_remote_tests;

use ls_remote_filter::{compile_patterns, include_reference, sort_entries};
use ls_remote_redaction::{
    sanitize_discovery_error, sanitize_remote_error_reason, visible_remote_display,
    visible_remote_url,
};

const LS_REMOTE_EXAMPLES: &str = "\
EXAMPLES:
    libra ls-remote origin                          List all refs on a configured remote
    libra ls-remote https://example.com/repo.git    List all refs on a remote URL (no remote setup)
    libra ls-remote --get-url origin                Resolve a remote URL without contacting it
    libra ls-remote --heads origin main             List only branch heads matching `main`
    libra ls-remote --exit-code origin main         Exit 2 when no refs match
    libra ls-remote --symref origin                 Show symbolic-ref targets (e.g. HEAD)
    libra --json ls-remote --tags origin            Structured JSON output for agents (tags only)";

#[derive(Parser, Debug)]
#[command(after_help = LS_REMOTE_EXAMPLES)]
pub struct LsRemoteArgs {
    /// Show only branch refs (refs/heads/)
    #[clap(long)]
    pub heads: bool,

    /// Show only tag refs (refs/tags/)
    #[clap(long, short = 't')]
    pub tags: bool,

    /// Do not show HEAD or peeled tag refs (refs ending in ^{})
    #[clap(long)]
    pub refs: bool,

    /// Expand the remote URL and exit without contacting the remote
    #[clap(long)]
    pub get_url: bool,

    /// Exit with status 2 when no refs match
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Sort refs by key: refname, -refname, version:refname, or -version:refname
    #[clap(long, value_name = "KEY")]
    pub sort: Option<String>,

    /// Show the targets of symbolic refs advertised by the remote (e.g.
    /// `ref: refs/heads/main\tHEAD`)
    #[clap(long)]
    pub symref: bool,

    /// Remote name, URL, or local repository path
    pub repository: String,

    /// Optional ref patterns. Plain names match full refs or path components.
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LsRemoteEntry {
    hash: String,
    refname: String,
}

/// A symbolic ref advertised by the remote: `name` (e.g. `HEAD`) points at
/// `target` (e.g. `refs/heads/main`).
#[derive(Debug, Clone, Serialize)]
struct LsRemoteSymref {
    name: String,
    target: String,
}

#[derive(Debug, Clone, Serialize)]
struct LsRemoteOutput {
    remote: String,
    url: String,
    heads_only: bool,
    tags_only: bool,
    refs_only: bool,
    get_url: bool,
    exit_code: bool,
    sort: Option<String>,
    patterns: Vec<String>,
    entries: Vec<LsRemoteEntry>,
    /// Symbolic-ref targets, populated only with `--symref`. Prefer advertised
    /// `symref=` capabilities; when they are absent, a visible HEAD may be
    /// derived from advertised branch tips (notably for local Libra sources).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    symrefs: Vec<LsRemoteSymref>,
}

/// Parse `symref=<from>:<to>` capability tokens advertised by `git-upload-pack`
/// into `(name, target)` pairs (e.g. `symref=HEAD:refs/heads/main` →
/// `("HEAD", "refs/heads/main")`). Capabilities without a `symref=` prefix or a
/// well-formed `from:to` body are ignored.
fn parse_symrefs(capabilities: &[String]) -> Vec<LsRemoteSymref> {
    capabilities
        .iter()
        .filter_map(|cap| {
            let body = cap.strip_prefix("symref=")?;
            let (name, target) = body.split_once(':')?;
            if name.is_empty() || target.is_empty() {
                return None;
            }
            Some(LsRemoteSymref {
                name: name.to_string(),
                target: target.to_string(),
            })
        })
        .collect()
}

/// Resolve the symbolic refs to surface for `--symref`: parse the remote's
/// advertised `symref=` capabilities (e.g. `symref=HEAD:refs/heads/main`) and
/// keep only those whose `name` survives the active ref filters. Returns empty
/// when `--symref` was not requested. When a transport has no `symref=`
/// capability (notably a local Libra source), derive HEAD from the advertised
/// HEAD and branch tips with the same deterministic resolver used by fetch.
fn resolve_output_symrefs(
    capabilities: &[String],
    entries: &[LsRemoteEntry],
    discovered: &[DiscRef],
    want: bool,
) -> Vec<LsRemoteSymref> {
    if !want {
        return Vec::new();
    }
    let parsed = parse_symrefs(capabilities)
        .into_iter()
        .filter(|symref| entries.iter().any(|entry| entry.refname == symref.name))
        .collect::<Vec<_>>();
    if !parsed.is_empty() {
        return parsed;
    }
    if !entries.iter().any(|entry| entry.refname == "HEAD") {
        return Vec::new();
    }
    let remote_head = discovered.iter().find(|reference| reference._ref == "HEAD");
    let heads = discovered
        .iter()
        .filter(|reference| reference._ref.starts_with("refs/heads/"))
        .cloned()
        .collect::<Vec<_>>();
    resolve_remote_default_branch(capabilities, &heads, remote_head)
        .map(|branch| {
            vec![LsRemoteSymref {
                name: "HEAD".to_string(),
                target: format!("refs/heads/{branch}"),
            }]
        })
        .unwrap_or_default()
}

#[derive(thiserror::Error, Debug)]
enum LsRemoteError {
    #[error("failed to read remote configuration: {0}")]
    ConfigRead(String),
    #[error("invalid remote '{spec}': {reason}")]
    InvalidRemote { spec: String, reason: String },
    #[error("invalid ref pattern '{pattern}': {reason}")]
    InvalidPattern { pattern: String, reason: String },
    #[error("unsupported ls-remote sort key '{0}'")]
    UnsupportedSortKey(String),
    #[error("failed to discover references from '{remote}': {source}")]
    Discovery { remote: String, source: GitError },
}

impl From<LsRemoteError> for CliError {
    fn from(error: LsRemoteError) -> Self {
        match &error {
            LsRemoteError::ConfigRead(_) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            LsRemoteError::InvalidRemote { .. } | LsRemoteError::InvalidPattern { .. } => {
                CliError::command_usage(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra remote -v' to inspect configured remotes")
            }
            LsRemoteError::UnsupportedSortKey(_) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use '--sort=refname' or '--sort=version:refname'."),
            LsRemoteError::Discovery { source, .. } => match source {
                GitError::UnAuthorized(_) => CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::AuthPermissionDenied)
                    .with_hint("check SSH key / HTTP credentials and repository access rights"),
                GitError::NetworkError(_) | GitError::IOError(_) => {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::NetworkUnavailable)
                        .with_hint("check the remote URL and network connectivity")
                }
                _ => CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::NetworkProtocol),
            },
        }
    }
}

pub async fn execute_safe(args: LsRemoteArgs, output: &OutputConfig) -> CliResult<()> {
    let data = run_ls_remote(args).await.map_err(CliError::from)?;
    render_ls_remote_output(&data, output)?;
    if data.exit_code && !data.get_url && data.entries.is_empty() {
        return Err(CliError::silent_exit(2));
    }
    Ok(())
}

async fn run_ls_remote(args: LsRemoteArgs) -> Result<LsRemoteOutput, LsRemoteError> {
    let (remote_display, remote_url, remote_name) = resolve_remote(&args.repository).await?;
    let visible_remote = visible_remote_display(&remote_display, remote_name.as_deref());
    if args.get_url {
        return Ok(LsRemoteOutput {
            remote: visible_remote,
            url: visible_remote_url(&remote_url),
            heads_only: args.heads,
            tags_only: args.tags,
            refs_only: args.refs,
            get_url: true,
            exit_code: args.exit_code,
            sort: args.sort,
            patterns: args.patterns,
            entries: Vec::new(),
            symrefs: Vec::new(),
        });
    }

    let client = RemoteClient::from_spec_with_remote(&remote_url, remote_name.as_deref()).map_err(
        |reason| LsRemoteError::InvalidRemote {
            spec: visible_remote.clone(),
            reason: sanitize_remote_error_reason(&reason, &remote_url),
        },
    )?;
    let discovery = client
        .discovery_reference(UploadPack)
        .await
        .map_err(|source| LsRemoteError::Discovery {
            remote: visible_remote.clone(),
            source: sanitize_discovery_error(source, &remote_url),
        })?;
    let patterns = compile_patterns(&args.patterns)?;
    let mut entries: Vec<LsRemoteEntry> = discovery
        .refs
        .iter()
        .filter(|reference| include_reference(reference, &args, &patterns))
        .map(|reference| LsRemoteEntry {
            hash: reference._hash.clone(),
            refname: reference._ref.clone(),
        })
        .collect();
    sort_entries(&mut entries, args.sort.as_deref())?;

    let symrefs = resolve_output_symrefs(
        &discovery.capabilities,
        &entries,
        &discovery.refs,
        args.symref,
    );

    Ok(LsRemoteOutput {
        remote: visible_remote,
        url: visible_remote_url(&remote_url),
        heads_only: args.heads,
        tags_only: args.tags,
        refs_only: args.refs,
        get_url: false,
        exit_code: args.exit_code,
        sort: args.sort,
        patterns: args.patterns,
        entries,
        symrefs,
    })
}

async fn resolve_remote(
    repository: &str,
) -> Result<(String, String, Option<String>), LsRemoteError> {
    if is_unambiguous_direct_remote_spec(repository) {
        return Ok((repository.to_string(), repository.to_string(), None));
    }

    if util::try_get_storage_path(None).is_ok() {
        let configured = ConfigKv::remote_config(repository)
            .await
            .map_err(|error| LsRemoteError::ConfigRead(error.to_string()))?;
        if let Some(remote) = configured {
            return Ok((remote.name.clone(), remote.url, Some(remote.name)));
        }
    }

    Ok((repository.to_string(), repository.to_string(), None))
}

fn is_unambiguous_direct_remote_spec(repository: &str) -> bool {
    if is_ssh_spec(repository) || Url::parse(repository).is_ok() {
        return true;
    }

    let path = Path::new(repository);
    path.is_absolute()
        || repository.starts_with("./")
        || repository.starts_with("../")
        || repository.starts_with(".\\")
        || repository.starts_with("..\\")
}

fn render_ls_remote_output(data: &LsRemoteOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        emit_json_data("ls-remote", data, output)
    } else if output.quiet {
        Ok(())
    } else if data.get_url {
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        writeln!(writer, "{}", data.url)
            .map_err(|error| CliError::io(format!("failed to write ls-remote URL: {error}")))
    } else {
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        write_ref_lines(&mut writer, data)
            .map_err(|error| CliError::io(format!("failed to write ls-remote output: {error}")))
    }
}

/// Write the human-readable `<oid>\t<name>` ref lines, emitting a
/// `ref: <target>\t<name>` line immediately before a symref's own OID line
/// (matching `git ls-remote --symref`). Generic over the writer so the exact
/// line layout — including symref placement — is unit-testable.
fn write_ref_lines<W: Write>(writer: &mut W, data: &LsRemoteOutput) -> std::io::Result<()> {
    for entry in &data.entries {
        if let Some(symref) = data.symrefs.iter().find(|s| s.name == entry.refname) {
            writeln!(writer, "ref: {}\t{}", symref.target, symref.name)?;
        }
        writeln!(writer, "{}\t{}", entry.hash, entry.refname)?;
    }
    Ok(())
}
