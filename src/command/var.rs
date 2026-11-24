//! Implements `var` for printing Libra's identity variables.

use clap::Parser;
use git_internal::internal::object::signature::{Signature, SignatureType};
use serde::Serialize;

use crate::{
    internal::config::{LocalIdentityTarget, resolve_user_identity_sources},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// `--help` examples shown in `libra var --help` output.
pub const VAR_EXAMPLES: &str = "\
EXAMPLES:
    libra var GIT_COMMITTER_IDENT    Print the configured committer identity
    libra var GIT_AUTHOR_IDENT       Print the configured author identity
    libra var -l                     List every supported variable
    libra var --json GIT_AUTHOR_IDENT  Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = VAR_EXAMPLES)]
pub struct VarArgs {
    /// List every supported variable as `NAME=value`.
    #[clap(short = 'l', long = "list")]
    pub list: bool,

    /// Variable name, for example `GIT_COMMITTER_IDENT`.
    #[clap(value_name = "NAME")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct VarOutput {
    name: String,
    value: String,
}

/// Variables this command understands, in Git's `var -l` order.
const SUPPORTED: [(&str, SignatureType); 2] = [
    ("GIT_COMMITTER_IDENT", SignatureType::Committer),
    ("GIT_AUTHOR_IDENT", SignatureType::Author),
];

pub async fn execute(args: VarArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

pub async fn execute_safe(args: VarArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    if args.list {
        let mut variables = Vec::with_capacity(SUPPORTED.len());
        for (name, kind) in SUPPORTED {
            // Git's `var -l` skips variables it cannot resolve; the same applies
            // here so an unset identity does not make the listing fail.
            if let Ok(value) = ident_value(name, kind).await {
                variables.push(VarOutput {
                    name: name.to_string(),
                    value,
                });
            }
        }
        if output.is_json() {
            return emit_json_data("var", &variables, output);
        }
        let mut text = String::new();
        for variable in &variables {
            text.push_str(&format!("{}={}\n", variable.name, variable.value));
        }
        return write_var_output(&text);
    }

    let name = match (args.name.as_deref(), args.list) {
        (Some(name), _) => name.to_string(),
        (None, _) => {
            return Err(CliError::failure(
                "var requires a variable name (or -l to list every variable)",
            )
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("try 'libra var -l' to list the supported variables."));
        }
    };
    let Some(kind) = SUPPORTED
        .iter()
        .find(|(supported, _)| *supported == name)
        .map(|(_, kind)| kind.clone())
    else {
        return Err(CliError::failure(format!(
            "unknown variable '{name}'; supported variables are GIT_COMMITTER_IDENT and GIT_AUTHOR_IDENT"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    };
    let value = ident_value(&name, kind).await?;
    let result = VarOutput { name, value };
    if output.is_json() {
        return emit_json_data("var", &result, output);
    }
    write_var_output(&format!("{}\n", result.value))
}

/// Build `Name <email> <unix-ts> <±hhmm>` from the configured identity and the
/// current local time. This card deliberately ignores the identity and date
/// environment variables; `PM-10a`/`PM-37` rewire the author/committer sides.
async fn ident_value(name: &str, kind: SignatureType) -> CliResult<String> {
    if matches!(kind, SignatureType::Author) {
        let (signature, _) = crate::command::commit::create_author_signature()
            .await
            .map_err(CliError::from)?;
        return Ok(format!(
            "{} <{}> {} {}",
            signature.name, signature.email, signature.timestamp, signature.timezone
        ));
    }
    let sources = resolve_user_identity_sources(LocalIdentityTarget::CurrentRepo)
        .await
        .map_err(|error| {
            CliError::failure(format!("failed to read the configured identity: {error:#}"))
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
        })?;
    let (Some(config_name), Some(config_email)) = (sources.config_name, sources.config_email)
    else {
        return Err(CliError::failure(format!(
            "{name} is unavailable: author identity unknown: name and email are not configured"
        ))
        .with_stable_code(StableErrorCode::AuthMissingCredentials)
        .with_hint(
            "run 'libra config user.name \"Your Name\"' and 'libra config user.email \"you@example.com\"'.",
        ));
    };
    let signature = Signature::new(kind, config_name, config_email);
    // Git's `var` prints one line; `Signature`'s `Display` is the multi-line
    // object header, so build the ident form from the fields instead.
    Ok(format!(
        "{} <{}> {} {}",
        signature.name, signature.email, signature.timestamp, signature.timezone
    ))
}

fn write_var_output(text: &str) -> CliResult<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .map_err(|error| CliError::io(format!("failed to write 'var' output: {error}")))?;
    stdout
        .flush()
        .map_err(|error| CliError::io(format!("failed to flush 'var' output: {error}")))
}
