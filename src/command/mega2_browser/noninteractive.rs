//! Non-interactive `mega2 browser` operations (plan-20261001 MN-02).
//!
//! Each non-interactive operation is one row of [`OPERATIONS`]: the flag that
//! selects it, the single HTTP endpoint it calls and its class (read or write,
//! directory or tag). An operation reaches its row only through the registry,
//! and the rules shared by a class are implemented once in this module, driven
//! by the row's class, never per operation (ADR-MN-08, GC-MN-11): no terminal and no
//! stdin; local validation first, then exactly one request with no reload,
//! preflight or retry; token flags refused for read operations. Success prints
//! the shared JSON envelope with `data.operation`, or a sanitized plain-text
//! summary (nothing under `--quiet`); failures go through the shared error
//! exit and leave stdout empty.

use std::io::Write;

use super::sanitize;
use crate::{
    command::mega2::browser_data,
    internal::protocol::{
        mega2_diag::{self, Endpoint},
        mega2_tree::{ContentType, Listing, Mega2TreeSession},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data, stdout_write_error},
    },
};

/// The `command` value of every `mega2 browser` JSON envelope.
const COMMAND: &str = "mega2 browser";

/// Whether an operation reads or changes remote state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The remote surface an operation acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Directory,
    Tag,
}

/// One registered non-interactive operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSpec {
    /// `data.operation` value.
    pub name: &'static str,
    /// The flag that selects the operation.
    pub flag: &'static str,
    /// The one request the operation sends.
    pub endpoint: Endpoint,
    pub access: Access,
    pub surface: Surface,
}

/// The registry (ADR-MN-08): one row per non-interactive operation. It is
/// the only source of an operation's flag, endpoint and class.
pub const OPERATIONS: &[OperationSpec] = &[OperationSpec {
    name: "list",
    flag: "--list",
    endpoint: mega2_diag::TREE,
    access: Access::Read,
    surface: Surface::Directory,
}];

/// An operation selected on the command line, with its own arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    List,
}

impl Operation {
    /// The registered name, also the `data.operation` value.
    pub fn name(&self) -> &'static str {
        match self {
            Operation::List => "list",
        }
    }

    /// The registry row of this operation.
    pub fn spec(&self) -> CliResult<&'static OperationSpec> {
        let name = self.name();
        OPERATIONS
            .iter()
            .find(|spec| spec.name == name)
            .ok_or_else(|| {
                CliError::internal(format!(
                    "mega2 browser: operation '{name}' has no registry row"
                ))
            })
    }
}

/// Inputs shared by every operation, validated before any request.
#[derive(Debug)]
pub struct Invocation<'a> {
    /// Canonical, credential-free server origin.
    pub server: &'a str,
    /// Normalized, rooted PATH.
    pub path: &'a str,
    pub git_ref: Option<&'a str>,
    /// Whether `--token` or `--token-file` was given.
    pub token_flags: bool,
}

/// Runs one non-interactive operation: class rules, one request, output.
pub async fn execute(
    operation: Operation,
    invocation: &Invocation<'_>,
    output: &OutputConfig,
) -> CliResult<()> {
    let spec = operation.spec()?;
    check_class_rules(spec, invocation)?;
    match operation {
        Operation::List => {
            let mut session = Mega2TreeSession::new(invocation.server)?;
            let listing = session.fetch(invocation.path, invocation.git_ref).await?;
            if output.is_json() {
                let data = browser_data(
                    invocation.server,
                    invocation.git_ref,
                    invocation.path,
                    &listing,
                );
                emit_json_data(COMMAND, &data, output)
            } else {
                write_human(output, &render_listing(&listing))
            }
        }
    }
}

/// Class rules (ADR-MN-08), driven by the registry row and checked before any
/// request. R5a/R5b: read operations take no credentials, so they refuse the
/// token flags (and never read `LIBRA_MEGA2_TOKEN`, R6).
fn check_class_rules(spec: &OperationSpec, invocation: &Invocation<'_>) -> CliResult<()> {
    if spec.access == Access::Read && invocation.token_flags {
        return Err(CliError::fatal(
            "mega2 browser: --token/--token-file are TUI-only and cannot be combined with --json/--machine or a non-interactive operation",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(
            "remove the token flags, or run the interactive browser (no operation flag, no --json/--machine) to write",
        ));
    }
    Ok(())
}

/// One line per entry, `dir  <name>` or `file  <name>`, in listing order.
fn render_listing(listing: &Listing) -> String {
    listing
        .entries
        .iter()
        .map(|entry| {
            let kind = match entry.content_type {
                ContentType::Directory => "dir",
                ContentType::File => "file",
            };
            format!("{kind}  {}\n", sanitize(&entry.name))
        })
        .collect()
}

/// Writes the human summary unless `--quiet` is set.
fn write_human(output: &OutputConfig, text: &str) -> CliResult<()> {
    if output.quiet {
        return Ok(());
    }
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    writer
        .write_all(text.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|error| stdout_write_error("write mega2 browser output", error))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// The long flags of the CLI's `operation` argument group.
    fn cli_operation_flags() -> Vec<String> {
        let cli = crate::cli::Cli::command();
        let browser = cli
            .find_subcommand("mega2")
            .and_then(|mega2| mega2.find_subcommand("browser"))
            .expect("mega2 browser is registered");
        let group = browser
            .get_groups()
            .find(|group| group.get_id() == "operation")
            .expect("browser has the operation group");
        let mut flags: Vec<String> = group
            .get_args()
            .map(|id| {
                let arg = browser
                    .get_arguments()
                    .find(|arg| arg.get_id() == id)
                    .expect("group member is an argument");
                format!(
                    "--{}",
                    arg.get_long().expect("operation flags are long flags")
                )
            })
            .collect();
        flags.sort();
        flags
    }

    /// Every selectable operation flag has exactly one registry row, and the
    /// registry has no row without a flag.
    #[test]
    fn every_operation_flag_has_exactly_one_registry_row() {
        let mut registered: Vec<String> = OPERATIONS
            .iter()
            .map(|spec| spec.flag.to_string())
            .collect();
        registered.sort();
        assert_eq!(cli_operation_flags(), registered);
    }

    /// Each operation resolves to the registry row of the same name.
    #[test]
    fn operations_resolve_through_the_registry() {
        let spec = Operation::List.spec().expect("list is registered");
        assert_eq!(spec.name, "list");
        assert_eq!(spec.endpoint, mega2_diag::TREE);
    }
}
