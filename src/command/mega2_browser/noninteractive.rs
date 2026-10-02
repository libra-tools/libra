//! Non-interactive `mega2 browser` operations (plan-20261001 MN-02 onward).
//!
//! Each non-interactive operation is one row of [`OPERATIONS`]: the flag that
//! selects it, the single HTTP endpoint it calls and its class (read or write,
//! directory or tag). An operation reaches its row only through the registry,
//! and the rules shared by a class are implemented once in this module, driven
//! by the row's class, never per operation (ADR-MN-08, GC-MN-11): no terminal and no
//! stdin; local validation first, then exactly one request with no reload,
//! preflight or retry; token flags refused where the class takes none; write
//! operations refuse `--ref`. Success prints
//! the shared JSON envelope with `data.operation`, or a sanitized plain-text
//! summary (nothing under `--quiet`); failures go through the shared error
//! exit and leave stdout empty.

use std::io::Write;

use serde::Serialize;

use super::sanitize;
use crate::{
    command::mega2::browser_data,
    internal::protocol::{
        mega2_diag::{self, Endpoint},
        mega2_entry::{Mega2EntryClient, RemoteCreateReceipt},
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

impl Access {
    /// Whether operations of this class take `--token`/`--token-file`
    /// (ADR-MN-05). Read operations never do; write credentials arrive with
    /// plan-20261001 MN-11, so until then no class does.
    fn accepts_token_flags(self) -> bool {
        match self {
            Access::Read | Access::Write => false,
        }
    }
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
pub const OPERATIONS: &[OperationSpec] = &[
    OperationSpec {
        name: "list",
        flag: "--list",
        endpoint: mega2_diag::TREE,
        access: Access::Read,
        surface: Surface::Directory,
    },
    OperationSpec {
        name: "create-dir",
        flag: "--create-dir",
        endpoint: mega2_diag::CREATE_ENTRY,
        access: Access::Write,
        surface: Surface::Directory,
    },
];

/// An operation selected on the command line, with its own arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    List,
    CreateDir { name: String },
}

impl Operation {
    /// The registered name, also the `data.operation` value.
    pub fn name(&self) -> &'static str {
        match self {
            Operation::List => "list",
            Operation::CreateDir { .. } => "create-dir",
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
        Operation::CreateDir { name } => {
            let client = Mega2EntryClient::new(invocation.server, None)?;
            let receipt = CreateReceipt::from(
                client
                    .create_directory(invocation.path, &name)
                    .await
                    .map_err(anonymous_write_error)?,
            );
            let target = Target::new(invocation.path, &name);
            if output.is_json() {
                let data = CreateDirData {
                    operation: spec.name,
                    server: invocation.server,
                    target: &target,
                    receipt: &receipt,
                };
                emit_json_data(COMMAND, &data, output)
            } else {
                write_human(
                    output,
                    &format!(
                        "created directory {} (commit {})\n",
                        sanitize(&target.path),
                        sanitize(&receipt.commit_id)
                    ),
                )
            }
        }
    }
}

/// Class rules (ADR-MN-08), driven by the registry row and checked before any
/// request:
/// - R8: write operations refuse `--ref`; writes always target the server's
///   default revision;
/// - R5a/R5b: a class that takes no credentials refuses the token flags (read
///   operations also never read `LIBRA_MEGA2_TOKEN`, R6).
fn check_class_rules(spec: &OperationSpec, invocation: &Invocation<'_>) -> CliResult<()> {
    if spec.access == Access::Write && invocation.git_ref.is_some() {
        return Err(CliError::fatal(format!(
            "mega2 browser: {} cannot be combined with --ref; write operations always target the server's default revision",
            spec.flag
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("remove --ref"));
    }
    if invocation.token_flags && !spec.access.accepts_token_flags() {
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

/// Non-interactive writes are anonymous until write credentials arrive
/// (plan-20261001 MN-11), so the clients' 401 hint — supply `--token-file` or
/// `LIBRA_MEGA2_TOKEN` — does not apply to them. Keep the code, message and
/// details of a 401 and replace only the hint with the paths that work.
fn anonymous_write_error(error: CliError) -> CliError {
    if error.stable_code() != StableErrorCode::AuthMissingCredentials {
        return error;
    }
    let mut adapted = CliError::fatal(error.message())
        .with_stable_code(error.stable_code())
        .with_hint(
            "non-interactive writes are anonymous for now: write through the interactive browser with --token-file, or use a server with push_auth=none",
        );
    for (key, value) in error.details() {
        adapted = adapted.with_detail(key.clone(), value.clone());
    }
    adapted
}

/// What a directory write targeted, built only from validated local input,
/// never from the server's receipt (ADR-MN-03).
#[derive(Serialize, Debug, PartialEq, Eq)]
struct Target {
    /// The rooted parent directory (PATH).
    parent: String,
    /// The entry name acted on.
    name: String,
    /// `parent` joined with `name`.
    path: String,
}

impl Target {
    fn new(parent: &str, name: &str) -> Self {
        let path = if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        };
        Self {
            parent: parent.to_string(),
            name: name.to_string(),
            path,
        }
    }
}

/// The server's create-entry receipt, verbatim (`path`/`cl_link` may be null).
#[derive(Serialize, Debug)]
struct CreateReceipt {
    commit_id: String,
    new_oid: String,
    path: Option<String>,
    cl_link: Option<String>,
}

impl From<RemoteCreateReceipt> for CreateReceipt {
    fn from(receipt: RemoteCreateReceipt) -> Self {
        Self {
            commit_id: receipt.commit_id,
            new_oid: receipt.new_oid,
            path: receipt.path,
            cl_link: receipt.cl_link,
        }
    }
}

/// The `create-dir` machine payload: `target` (local input) and `receipt`
/// (server answer) are kept apart.
#[derive(Serialize, Debug)]
struct CreateDirData<'a> {
    operation: &'static str,
    server: &'a str,
    target: &'a Target,
    receipt: &'a CreateReceipt,
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
        let spec = Operation::CreateDir {
            name: "x".to_string(),
        }
        .spec()
        .expect("create-dir is registered");
        assert_eq!(spec.name, "create-dir");
        assert_eq!(spec.endpoint, mega2_diag::CREATE_ENTRY);
    }

    /// MN-03: the `create-dir` payload keeps local `target` and server
    /// `receipt` apart.
    #[test]
    fn create_dir_payload_separates_target_and_receipt() {
        let target = Target::new("/src", "pkg");
        let receipt = CreateReceipt {
            commit_id: "c1".to_string(),
            new_oid: "o1".to_string(),
            path: None,
            cl_link: None,
        };
        let data = CreateDirData {
            operation: "create-dir",
            server: "https://mega2.example.com",
            target: &target,
            receipt: &receipt,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            serde_json::json!({
                "operation": "create-dir",
                "server": "https://mega2.example.com",
                "target": {"parent": "/src", "name": "pkg", "path": "/src/pkg"},
                "receipt": {"commit_id": "c1", "new_oid": "o1", "path": null, "cl_link": null},
            })
        );
    }
}
