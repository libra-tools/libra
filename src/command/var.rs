//! Implements `var` for printing Libra's identity variables.

use clap::Parser;
use git_internal::internal::object::signature::SignatureType;
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
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
            if let Ok(value) = ident_value(kind).await {
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
    let value = ident_value(kind).await?;
    let result = VarOutput { name, value };
    if output.is_json() {
        return emit_json_data("var", &result, output);
    }
    write_var_output(&format!("{}\n", result.value))
}

/// Build `Name <email> <unix-ts> <±hhmm>` from the same signature resolvers
/// `libra commit` uses, so `var` reports exactly what a commit would write.
async fn ident_value(kind: SignatureType) -> CliResult<String> {
    let (signature, _) = if matches!(kind, SignatureType::Author) {
        crate::command::commit::create_author_signature().await
    } else {
        crate::command::commit::create_committer_signature().await
    }
    .map_err(CliError::from)?;
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
