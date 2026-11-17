//! Non-interactive `mega2 browser` operations (plan-20261001 MN-02 onward).
//!
//! Each non-interactive operation is one row of [`OPERATIONS`]: the flag that
//! selects it, the single HTTP endpoint it calls and its class (read or write,
//! directory or tag). An operation reaches its row only through the registry,
//! and the rules shared by a class are implemented once in this module, driven
//! by the row's class, never per operation (ADR-MN-08, GC-MN-11): no terminal
//! and no stdin; local validation first, then exactly one request with no
//! reload, preflight or retry; write operations refuse `--ref`. Read and write
//! operations accept an optional token and carry it as one Bearer header.
//! Success prints the shared JSON envelope
//! with `data.operation`, or a sanitized plain-text summary (nothing under
//! `--quiet`); failures go through the shared error exit and leave stdout
//! empty.

use std::io::Write;

use serde::Serialize;

use super::{sanitize, tag_panel::MAX_TAG_MESSAGE_BYTES};
use crate::{
    command::mega2::browser_data,
    internal::protocol::{
        mega2_auth::Mega2Token,
        mega2_diag::{self, Endpoint},
        mega2_entry::{Mega2EntryClient, RemoteCreateReceipt},
        mega2_mutate::{
            DeleteReceipt as RemoteDeleteReceipt, Mega2MutateClient,
            MoveReceipt as RemoteMoveReceipt,
        },
        mega2_tag::{
            CreateTagOptions, DeleteTagReceipt as RemoteDeleteTagReceipt, Mega2TagClient, TagInfo,
            TagPage,
        },
        mega2_tree::{ContentType, Listing, Mega2TreeSession, normalize_path},
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
    OperationSpec {
        name: "delete-dir",
        flag: "--delete-dir",
        endpoint: mega2_diag::DELETE_ENTRY,
        access: Access::Write,
        surface: Surface::Directory,
    },
    OperationSpec {
        name: "move-dir",
        flag: "--move-dir",
        endpoint: mega2_diag::MOVE_ENTRY,
        access: Access::Write,
        surface: Surface::Directory,
    },
    OperationSpec {
        name: "rename-dir",
        flag: "--rename-dir",
        endpoint: mega2_diag::MOVE_ENTRY,
        access: Access::Write,
        surface: Surface::Directory,
    },
    OperationSpec {
        name: "list-tags",
        flag: "--list-tags",
        endpoint: mega2_diag::LIST_TAGS,
        access: Access::Read,
        surface: Surface::Tag,
    },
    OperationSpec {
        name: "create-tag",
        flag: "--create-tag",
        endpoint: mega2_diag::CREATE_TAG,
        access: Access::Write,
        surface: Surface::Tag,
    },
    OperationSpec {
        name: "delete-tag",
        flag: "--delete-tag",
        endpoint: mega2_diag::DELETE_TAG,
        access: Access::Write,
        surface: Surface::Tag,
    },
];

/// An operation selected on the command line, with its own arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    List,
    CreateDir {
        name: String,
    },
    DeleteDir {
        name: String,
    },
    /// Move NAME from PATH into `to_parent`, keeping the name.
    MoveDir {
        name: String,
        to_parent: String,
    },
    /// Rename NAME to `new_name`, keeping the parent PATH.
    RenameDir {
        name: String,
        new_name: String,
    },
    /// One page of root tags.
    ListTags {
        page: u64,
        per_page: u64,
    },
    /// Create root tag NAME: lightweight without `message`, annotated with it.
    CreateTag {
        name: String,
        message: Option<String>,
    },
    /// Delete root tag NAME.
    DeleteTag {
        name: String,
    },
}

impl Operation {
    /// The registered name, also the `data.operation` value.
    pub fn name(&self) -> &'static str {
        match self {
            Operation::List => "list",
            Operation::CreateDir { .. } => "create-dir",
            Operation::DeleteDir { .. } => "delete-dir",
            Operation::MoveDir { .. } => "move-dir",
            Operation::RenameDir { .. } => "rename-dir",
            Operation::ListTags { .. } => "list-tags",
            Operation::CreateTag { .. } => "create-tag",
            Operation::DeleteTag { .. } => "delete-tag",
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
    /// The resolved token by ADR-MB-03 precedence (`--token-file` →
    /// `LIBRA_MEGA2_TOKEN` → `--token`); `None` sends the request anonymously.
    pub token: Option<&'a Mega2Token>,
}

/// Runs one non-interactive operation: class rules, one request, output.
pub async fn execute(
    operation: Operation,
    invocation: &Invocation<'_>,
    output: &OutputConfig,
) -> CliResult<()> {
    let spec = operation.spec()?;
    check_class_rules(spec, invocation)?;
    // Issue #18: the token is optional for every operation; reads attach it
    // exactly like writes, so a token-gated server works for both.
    let token = invocation.token.cloned();
    match operation {
        Operation::List => {
            let mut session = Mega2TreeSession::with_token(invocation.server, token)?;
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
            let client = Mega2EntryClient::new(invocation.server, token)?;
            let receipt =
                CreateReceipt::from(client.create_directory(invocation.path, &name).await?);
            let target = Target::new(invocation.path, &name);
            let summary = format!(
                "created directory {} (commit {})\n",
                sanitize(&target.path),
                sanitize(&receipt.commit_id)
            );
            emit_write(spec, invocation, output, &target, &receipt, &summary)
        }
        Operation::DeleteDir { name } => {
            // No preflight (ADR-MN-02): whether NAME exists and is a directory
            // is the server's answer, so the listing type is not known here.
            let client = Mega2MutateClient::new(invocation.server, token)?;
            let receipt = DeleteReceipt::from(
                client
                    .delete_directory(invocation.path, &name, None)
                    .await?,
            );
            let target = Target::new(invocation.path, &name);
            let summary = format!(
                "deleted directory {} (commit {})\n",
                sanitize(&target.path),
                sanitize(&receipt.commit_id)
            );
            emit_write(spec, invocation, output, &target, &receipt, &summary)
        }
        Operation::MoveDir { name, to_parent } => {
            // The destination parent is validated like PATH, as the TUI's `m`
            // does, before the one request.
            let to_parent = normalize_path(&to_parent)?;
            let client = Mega2MutateClient::new(invocation.server, token)?;
            let receipt = MoveReceipt::from(
                client
                    .move_entry(invocation.path, &name, &to_parent, &name, None)
                    .await?,
            );
            let target = MoveTarget {
                from: Target::new(invocation.path, &name),
                to: Target::new(&to_parent, &name),
            };
            let summary = format!(
                "moved directory {} -> {} (commit {})\n",
                sanitize(&target.from.path),
                sanitize(&target.to.path),
                sanitize(&receipt.commit_id)
            );
            emit_write(spec, invocation, output, &target, &receipt, &summary)
        }
        Operation::RenameDir { name, new_name } => {
            // A rename is the same-parent form of the move request.
            let client = Mega2MutateClient::new(invocation.server, token)?;
            let receipt = MoveReceipt::from(
                client
                    .rename_directory(invocation.path, &name, &new_name, None)
                    .await?,
            );
            let target = MoveTarget {
                from: Target::new(invocation.path, &name),
                to: Target::new(invocation.path, &new_name),
            };
            let summary = format!(
                "renamed directory {} -> {} (commit {})\n",
                sanitize(&target.from.path),
                sanitize(&target.to.path),
                sanitize(&receipt.commit_id)
            );
            emit_write(spec, invocation, output, &target, &receipt, &summary)
        }
        Operation::ListTags { page, per_page } => {
            let client = Mega2TagClient::new(invocation.server, token)?;
            let tags = client.list_tags(page, per_page, invocation.path).await?;
            if output.is_json() {
                let data = ListTagsData::new(spec.name, invocation, page, per_page, &tags);
                emit_json_data(COMMAND, &data, output)
            } else {
                write_human(output, &render_tags(page, per_page, &tags))
            }
        }
        Operation::CreateTag { name, message } => {
            if let Some(message) = &message {
                validate_tag_message(message)?;
            }
            let client = Mega2TagClient::new(invocation.server, token)?;
            let options = CreateTagOptions {
                name: &name,
                message: message.as_deref(),
                ..CreateTagOptions::default()
            };
            let tag = client.create_tag(&options).await?;
            let target = TagTarget {
                name: &name,
                kind: if message.is_some() {
                    "annotated"
                } else {
                    "lightweight"
                },
                path: invocation.path,
            };
            let receipt = TagItem::from(&tag);
            let summary = format!(
                "created {} tag {} -> {}\n",
                target.kind,
                sanitize(target.name),
                sanitize(receipt.object_id)
            );
            emit_write(spec, invocation, output, &target, &receipt, &summary)
        }
        Operation::DeleteTag { name } => {
            let client = Mega2TagClient::new(invocation.server, token)?;
            let receipt = DeleteTagReceipt::from(client.delete_tag(&name, invocation.path).await?);
            let target = TagSelector {
                name: &name,
                path: invocation.path,
            };
            let summary = format!("deleted tag {}\n", sanitize(target.name));
            emit_write(spec, invocation, output, &target, &receipt, &summary)
        }
    }
}

/// ADR-MN-06: a non-interactive tag message must be non-empty, at most
/// [`MAX_TAG_MESSAGE_BYTES`] bytes and free of control characters (newlines
/// included). It is refused, never rewritten; a lightweight tag omits
/// `--message` instead of passing an empty one.
fn validate_tag_message(message: &str) -> CliResult<()> {
    let problem = if message.is_empty() {
        "must not be empty; omit --message to create a lightweight tag".to_string()
    } else if message.len() > MAX_TAG_MESSAGE_BYTES {
        format!("exceeds the {MAX_TAG_MESSAGE_BYTES}-byte limit")
    } else if message.chars().any(char::is_control) {
        "must not contain control characters, including newlines".to_string()
    } else {
        return Ok(());
    };
    Err(
        CliError::fatal(format!("mega2 browser: --message {problem}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments),
    )
}

/// Prints a successful write: the machine payload, or the one-line human
/// summary (already sanitized by the caller).
fn emit_write<T: Serialize, R: Serialize>(
    spec: &OperationSpec,
    invocation: &Invocation<'_>,
    output: &OutputConfig,
    target: &T,
    receipt: &R,
    summary: &str,
) -> CliResult<()> {
    if output.is_json() {
        let data = WriteData {
            operation: spec.name,
            server: invocation.server,
            target,
            receipt,
        };
        emit_json_data(COMMAND, &data, output)
    } else {
        write_human(output, summary)
    }
}

/// Class rules (ADR-MN-08), driven by the registry row and checked before any
/// request:
/// - R8: write operations refuse `--ref`; writes always target the server's
///   default revision;
/// - R10b: read tag operations refuse `--ref` as well: tags are not listed
///   per revision;
/// - R10a: tag operations act on root tags only, so PATH must be `/`;
/// - R5a/R5b/R6: read and write operations alike take one optional token,
///   resolved once in the command layer (R9a–R9c).
fn check_class_rules(spec: &OperationSpec, invocation: &Invocation<'_>) -> CliResult<()> {
    if spec.access == Access::Write && invocation.git_ref.is_some() {
        return Err(CliError::fatal(format!(
            "mega2 browser: {} cannot be combined with --ref; write operations always target the server's default revision",
            spec.flag
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("remove --ref"));
    }
    if spec.surface == Surface::Tag && invocation.git_ref.is_some() {
        return Err(CliError::fatal(format!(
            "mega2 browser: {} cannot be combined with --ref; tags are not selected by revision",
            spec.flag
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("remove --ref"));
    }
    if spec.surface == Surface::Tag && invocation.path != "/" {
        return Err(CliError::fatal(format!(
            "mega2 browser: {} operates on root tags only; PATH must be /",
            spec.flag
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("omit PATH or pass /"));
    }
    Ok(())
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

/// The server's delete-entry receipt, verbatim (`path`/`cl_link` may be null).
#[derive(Serialize, Debug)]
struct DeleteReceipt {
    commit_id: String,
    path: Option<String>,
    cl_link: Option<String>,
}

impl From<RemoteDeleteReceipt> for DeleteReceipt {
    fn from(receipt: RemoteDeleteReceipt) -> Self {
        Self {
            commit_id: receipt.commit_id,
            path: receipt.path,
            cl_link: receipt.cl_link,
        }
    }
}

/// What a move or rename targeted: the source and destination entries, both
/// built only from validated local input (ADR-MN-03).
#[derive(Serialize, Debug, PartialEq, Eq)]
struct MoveTarget {
    from: Target,
    to: Target,
}

/// The server's move-entry receipt, verbatim (`from_path`, `to_path` and
/// `cl_link` may be null).
#[derive(Serialize, Debug)]
struct MoveReceipt {
    commit_id: String,
    from_path: Option<String>,
    to_path: Option<String>,
    cl_link: Option<String>,
}

impl From<RemoteMoveReceipt> for MoveReceipt {
    fn from(receipt: RemoteMoveReceipt) -> Self {
        Self {
            commit_id: receipt.commit_id,
            from_path: receipt.from_path,
            to_path: receipt.to_path,
            cl_link: receipt.cl_link,
        }
    }
}

/// The machine payload of every write operation: `target` (local input) and
/// `receipt` (server answer) are kept apart (ADR-MN-03).
#[derive(Serialize, Debug)]
struct WriteData<'a, T, R> {
    operation: &'static str,
    server: &'a str,
    target: &'a T,
    receipt: &'a R,
}

/// What a tag write targeted, built only from validated local input
/// (ADR-MN-03): the tag name, its kind and the root path.
#[derive(Serialize, Debug)]
struct TagTarget<'a> {
    name: &'a str,
    /// `lightweight` (no `--message`) or `annotated`.
    kind: &'static str,
    path: &'a str,
}

/// Which tag a delete targeted: the validated name and the root path.
#[derive(Serialize, Debug)]
struct TagSelector<'a> {
    name: &'a str,
    path: &'a str,
}

/// The server's tag-delete receipt, verbatim.
#[derive(Serialize, Debug)]
struct DeleteTagReceipt {
    deleted_tag: String,
    message: String,
}

impl From<RemoteDeleteTagReceipt> for DeleteTagReceipt {
    fn from(receipt: RemoteDeleteTagReceipt) -> Self {
        Self {
            deleted_tag: receipt.deleted_tag,
            message: receipt.message,
        }
    }
}

/// One tag as the server reports it, every field verbatim: an item of a
/// `list-tags` page, or the receipt of `create-tag`.
#[derive(Serialize, Debug)]
struct TagItem<'a> {
    name: &'a str,
    tag_id: &'a str,
    object_id: &'a str,
    object_type: &'a str,
    tagger: &'a str,
    message: &'a str,
    created_at: &'a str,
}

impl<'a> From<&'a TagInfo> for TagItem<'a> {
    fn from(tag: &'a TagInfo) -> Self {
        Self {
            name: &tag.name,
            tag_id: &tag.tag_id,
            object_id: &tag.object_id,
            object_type: &tag.object_type,
            tagger: &tag.tagger,
            message: &tag.message,
            created_at: &tag.created_at,
        }
    }
}

/// The `list-tags` machine payload: the page asked for, the server's total and
/// whether a later page exists.
#[derive(Serialize, Debug)]
struct ListTagsData<'a> {
    operation: &'static str,
    server: &'a str,
    path: &'a str,
    page: u64,
    per_page: u64,
    total: u64,
    has_next: bool,
    items: Vec<TagItem<'a>>,
}

impl<'a> ListTagsData<'a> {
    fn new(
        operation: &'static str,
        invocation: &Invocation<'a>,
        page: u64,
        per_page: u64,
        tags: &'a TagPage,
    ) -> Self {
        Self {
            operation,
            server: invocation.server,
            path: invocation.path,
            page,
            per_page,
            total: tags.total,
            has_next: page.saturating_mul(per_page) < tags.total,
            items: tags.items.iter().map(TagItem::from).collect(),
        }
    }
}

/// One line per tag, `<name>  <object_type>  <tagger>`, a four-space indented
/// message line when the message is not empty, then the paging line. Server
/// strings are sanitized.
fn render_tags(page: u64, per_page: u64, tags: &TagPage) -> String {
    let mut out = String::new();
    for tag in &tags.items {
        out.push_str(&format!(
            "{}  {}  {}\n",
            sanitize(&tag.name),
            sanitize(&tag.object_type),
            sanitize(&tag.tagger)
        ));
        if !tag.message.is_empty() {
            out.push_str(&format!("    {}\n", sanitize(&tag.message)));
        }
    }
    out.push_str(&format!(
        "page {page} · per_page {per_page} · total {}\n",
        tags.total
    ));
    out
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
        let spec = Operation::DeleteDir {
            name: "x".to_string(),
        }
        .spec()
        .expect("delete-dir is registered");
        assert_eq!(spec.name, "delete-dir");
        assert_eq!(spec.endpoint, mega2_diag::DELETE_ENTRY);
        let spec = Operation::MoveDir {
            name: "x".to_string(),
            to_parent: "/y".to_string(),
        }
        .spec()
        .expect("move-dir is registered");
        assert_eq!(spec.name, "move-dir");
        assert_eq!(spec.endpoint, mega2_diag::MOVE_ENTRY);
        let spec = Operation::RenameDir {
            name: "x".to_string(),
            new_name: "y".to_string(),
        }
        .spec()
        .expect("rename-dir is registered");
        assert_eq!(spec.name, "rename-dir");
        assert_eq!(spec.endpoint, mega2_diag::MOVE_ENTRY);
        let spec = Operation::ListTags {
            page: 1,
            per_page: 20,
        }
        .spec()
        .expect("list-tags is registered");
        assert_eq!(spec.name, "list-tags");
        assert_eq!(spec.endpoint, mega2_diag::LIST_TAGS);
        let spec = Operation::CreateTag {
            name: "v1".to_string(),
            message: None,
        }
        .spec()
        .expect("create-tag is registered");
        assert_eq!(spec.name, "create-tag");
        assert_eq!(spec.endpoint, mega2_diag::CREATE_TAG);
        let spec = Operation::DeleteTag {
            name: "v1".to_string(),
        }
        .spec()
        .expect("delete-tag is registered");
        assert_eq!(spec.name, "delete-tag");
        assert_eq!(spec.endpoint, mega2_diag::DELETE_TAG);
    }

    /// Issue #18: a token is accepted by read and write operations alike.
    #[test]
    fn token_gate_accepts_read_and_write_operations() {
        let token = Mega2Token::new("t").expect("token");
        let invocation = Invocation {
            server: "https://mega2.example.com",
            path: "/",
            git_ref: None,
            token: Some(&token),
        };
        let list = Operation::List.spec().expect("list is registered");
        let create_dir = Operation::CreateDir {
            name: "x".to_string(),
        }
        .spec()
        .expect("create-dir is registered");
        assert!(check_class_rules(list, &invocation).is_ok());
        assert!(check_class_rules(create_dir, &invocation).is_ok());
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
        let data = WriteData {
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

    /// MN-04: the `delete-dir` payload keeps local `target` and server
    /// `receipt` apart.
    #[test]
    fn delete_dir_payload_separates_target_and_receipt() {
        let target = Target::new("/src", "pkg");
        let receipt = DeleteReceipt {
            commit_id: "c1".to_string(),
            path: Some("/src/pkg".to_string()),
            cl_link: None,
        };
        let data = WriteData {
            operation: "delete-dir",
            server: "https://mega2.example.com",
            target: &target,
            receipt: &receipt,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            serde_json::json!({
                "operation": "delete-dir",
                "server": "https://mega2.example.com",
                "target": {"parent": "/src", "name": "pkg", "path": "/src/pkg"},
                "receipt": {"commit_id": "c1", "path": "/src/pkg", "cl_link": null},
            })
        );
    }

    /// MN-08: the `move-dir` payload keeps local `target` (source and
    /// destination) and server `receipt` apart.
    #[test]
    fn move_payload_separates_target_and_receipt() {
        let target = MoveTarget {
            from: Target::new("/src", "pkg"),
            to: Target::new("/lib", "pkg"),
        };
        let receipt = MoveReceipt {
            commit_id: "c1".to_string(),
            from_path: Some("/src/pkg".to_string()),
            to_path: Some("/lib/pkg".to_string()),
            cl_link: None,
        };
        let data = WriteData {
            operation: "move-dir",
            server: "https://mega2.example.com",
            target: &target,
            receipt: &receipt,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            serde_json::json!({
                "operation": "move-dir",
                "server": "https://mega2.example.com",
                "target": {
                    "from": {"parent": "/src", "name": "pkg", "path": "/src/pkg"},
                    "to": {"parent": "/lib", "name": "pkg", "path": "/lib/pkg"},
                },
                "receipt": {"commit_id": "c1", "from_path": "/src/pkg", "to_path": "/lib/pkg", "cl_link": null},
            })
        );
    }

    /// MN-12: a rename keeps the parent, so `target.to.parent` is
    /// `target.from.parent`.
    #[test]
    fn rename_payload_keeps_parent() {
        let target = MoveTarget {
            from: Target::new("/src", "old"),
            to: Target::new("/src", "new"),
        };
        let receipt = MoveReceipt {
            commit_id: "c1".to_string(),
            from_path: Some("/src/old".to_string()),
            to_path: Some("/src/new".to_string()),
            cl_link: None,
        };
        let data = WriteData {
            operation: "rename-dir",
            server: "https://mega2.example.com",
            target: &target,
            receipt: &receipt,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            serde_json::json!({
                "operation": "rename-dir",
                "server": "https://mega2.example.com",
                "target": {
                    "from": {"parent": "/src", "name": "old", "path": "/src/old"},
                    "to": {"parent": "/src", "name": "new", "path": "/src/new"},
                },
                "receipt": {"commit_id": "c1", "from_path": "/src/old", "to_path": "/src/new", "cl_link": null},
            })
        );
    }

    /// MN-05: `has_next` is `page * per_page < total`.
    #[test]
    fn list_tags_payload_computes_has_next() {
        let invocation = Invocation {
            server: "https://mega2.example.com",
            path: "/",
            git_ref: None,
            token: None,
        };
        let tags = |total| TagPage {
            total,
            items: Vec::new(),
        };
        let has_next = |page, per_page, total| {
            ListTagsData::new("list-tags", &invocation, page, per_page, &tags(total)).has_next
        };
        assert_eq!(
            (
                has_next(1, 20, 21),
                has_next(1, 20, 20),
                has_next(2, 10, 21),
                has_next(3, 10, 21),
                has_next(1, 20, 0),
            ),
            (true, false, true, false, false)
        );
    }

    /// MN-06: the `create-tag` payload keeps local `target` (name, kind,
    /// path) and the server's tag `receipt` apart.
    #[test]
    fn create_tag_payload_separates_target_and_receipt() {
        let tag = TagInfo {
            name: "v1".to_string(),
            tag_id: "t1".to_string(),
            object_id: "o1".to_string(),
            object_type: "commit".to_string(),
            tagger: "Ann <ann@example.com>".to_string(),
            message: "release one".to_string(),
            created_at: "2026-10-02T00:00:00Z".to_string(),
        };
        let target = TagTarget {
            name: "v1",
            kind: "annotated",
            path: "/",
        };
        let receipt = TagItem::from(&tag);
        let data = WriteData {
            operation: "create-tag",
            server: "https://mega2.example.com",
            target: &target,
            receipt: &receipt,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            serde_json::json!({
                "operation": "create-tag",
                "server": "https://mega2.example.com",
                "target": {"name": "v1", "kind": "annotated", "path": "/"},
                "receipt": {
                    "name": "v1",
                    "tag_id": "t1",
                    "object_id": "o1",
                    "object_type": "commit",
                    "tagger": "Ann <ann@example.com>",
                    "message": "release one",
                    "created_at": "2026-10-02T00:00:00Z",
                },
            })
        );
    }

    /// MN-06 (ADR-MN-06): `--message` is refused when empty, longer than
    /// 1024 bytes or carrying a control character; a bounded printable
    /// message passes.
    #[test]
    fn tag_message_rules() {
        let long = "a".repeat(MAX_TAG_MESSAGE_BYTES + 1);
        let at_limit = "a".repeat(MAX_TAG_MESSAGE_BYTES);
        assert_eq!(
            [
                validate_tag_message("").is_err(),
                validate_tag_message(&long).is_err(),
                validate_tag_message("a\nb").is_err(),
                validate_tag_message(&at_limit).is_ok(),
                validate_tag_message("release one").is_ok(),
            ],
            [true; 5]
        );
    }

    /// MN-09: the `delete-tag` payload keeps local `target` (name, path) and
    /// the server's `receipt` apart.
    #[test]
    fn delete_tag_payload_separates_target_and_receipt() {
        let target = TagSelector {
            name: "v1",
            path: "/",
        };
        let receipt = DeleteTagReceipt {
            deleted_tag: "v1".to_string(),
            message: "Tag deleted".to_string(),
        };
        let data = WriteData {
            operation: "delete-tag",
            server: "https://mega2.example.com",
            target: &target,
            receipt: &receipt,
        };
        assert_eq!(
            serde_json::to_value(&data).expect("serialize"),
            serde_json::json!({
                "operation": "delete-tag",
                "server": "https://mega2.example.com",
                "target": {"name": "v1", "path": "/"},
                "receipt": {"deleted_tag": "v1", "message": "Tag deleted"},
            })
        );
    }
}
