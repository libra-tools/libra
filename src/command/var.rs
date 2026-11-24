//! Implements `var` for printing Libra's identity variables.

use clap::Parser;
use git_internal::internal::object::signature::SignatureType;
use serde::Serialize;

use crate::{
    internal::config::LocalIdentityTarget,
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

#[derive(Clone, Copy)]
enum VarKind {
    Author,
    Committer,
    Editor,
    SequenceEditor,
    Pager,
    DefaultBranch,
    AttrSystem,
    AttrGlobal,
}

/// Variables this command understands, in Git's `var -l` order.
const SUPPORTED: [(&str, VarKind); 8] = [
    ("GIT_COMMITTER_IDENT", VarKind::Committer),
    ("GIT_AUTHOR_IDENT", VarKind::Author),
    ("GIT_EDITOR", VarKind::Editor),
    ("GIT_SEQUENCE_EDITOR", VarKind::SequenceEditor),
    ("GIT_PAGER", VarKind::Pager),
    ("GIT_DEFAULT_BRANCH", VarKind::DefaultBranch),
    ("GIT_ATTR_SYSTEM", VarKind::AttrSystem),
    ("GIT_ATTR_GLOBAL", VarKind::AttrGlobal),
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
            if let Ok(value) = variable_value(kind).await {
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
    let value = variable_value(kind).await?;
    let result = VarOutput { name, value };
    if output.is_json() {
        return emit_json_data("var", &result, output);
    }
    write_var_output(&format!("{}\n", result.value))
}

/// Resolve one supported variable; the identity kinds reuse the commit
/// signature resolvers, the editor kinds reuse the editor resolvers.
async fn variable_value(kind: VarKind) -> CliResult<String> {
    match kind {
        VarKind::Author | VarKind::Committer => ident_value(kind).await,
        VarKind::Editor => editor_value().await,
        VarKind::SequenceEditor => sequence_editor_value().await,
        VarKind::Pager => Ok(pager_value()),
        VarKind::DefaultBranch => default_branch_value().await,
        VarKind::AttrSystem => Err(CliError::silent_exit(1)),
        VarKind::AttrGlobal => attr_global_value().await,
    }
}

/// `GIT_PAGER`: the command Libra would spawn (`less -R -F`), or `cat` when
/// paging is explicitly disabled. `GIT_PAGER`/`core.pager`/`PAGER` are
/// deliberately not consulted (`DEFER-PM-05`).
fn pager_value() -> String {
    let disabled = std::env::var(crate::utils::pager::LIBRA_PAGER_ENV)
        .map(|value| value.trim().eq_ignore_ascii_case("never"))
        .unwrap_or(false);
    if disabled {
        "cat".to_string()
    } else {
        "less -R -F".to_string()
    }
}

/// `GIT_DEFAULT_BRANCH`: what `libra init` would use.
async fn default_branch_value() -> CliResult<String> {
    let configured = crate::internal::config::read_cascaded_config_value(
        LocalIdentityTarget::CurrentRepo,
        "init.defaultBranch",
    )
    .await
    .map_err(|error| CliError::failure(format!("failed to read init.defaultBranch: {error:#}")))?;
    Ok(configured
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "main".to_string()))
}

/// `GIT_ATTR_GLOBAL`: `core.attributesFile` (expanded), else the XDG/HOME
/// default; with neither configured nor discoverable, Git exits 1 with no
/// output and so do we.
async fn attr_global_value() -> CliResult<String> {
    let workdir = std::env::current_dir().map_err(|error| {
        CliError::io(format!("failed to resolve the current directory: {error}"))
    })?;
    if let Some(configured) =
        crate::utils::util::optional_cascaded_config_path("core.attributesFile", &workdir)
    {
        return Ok(configured.display().to_string());
    }
    default_global_attributes_path()
        .map(|path| path.display().to_string())
        .ok_or_else(|| CliError::silent_exit(1))
}

fn default_global_attributes_path() -> Option<std::path::PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = std::path::PathBuf::from(xdg);
        if path.is_absolute() && !path.as_os_str().is_empty() {
            return Some(path.join("git").join("attributes"));
        }
    }
    dirs::home_dir().map(|home| home.join(".config").join("git").join("attributes"))
}

/// `GIT_EDITOR`: `GIT_EDITOR` > `core.editor` > `VISUAL` > `EDITOR`, then Git's
/// fallback (`vi` when `TERM` is usable, otherwise exit 1 with no output).
async fn editor_value() -> CliResult<String> {
    if let Some(editor) = crate::command::editor::resolve_editor().await {
        return Ok(editor);
    }
    fallback_editor()
}

/// `GIT_SEQUENCE_EDITOR`: the `rebase -i` chain (`GIT_SEQUENCE_EDITOR` >
/// `sequence.editor` > the editor resolution), then Git's terminal fallback.
async fn sequence_editor_value() -> CliResult<String> {
    if let Some(editor) = crate::command::editor::resolve_sequence_editor().await {
        return Ok(editor);
    }
    fallback_editor()
}

fn fallback_editor() -> CliResult<String> {
    match std::env::var("TERM") {
        Ok(term) if term != "dumb" => Ok("vi".to_string()),
        _ => Err(CliError::silent_exit(1)),
    }
}

/// Build `Name <email> <unix-ts> <±hhmm>` from the same signature resolvers
/// `libra commit` uses, so `var` reports exactly what a commit would write.
async fn ident_value(kind: VarKind) -> CliResult<String> {
    let signature_kind = match kind {
        VarKind::Author => SignatureType::Author,
        _ => SignatureType::Committer,
    };
    let (signature, _) = if matches!(signature_kind, SignatureType::Author) {
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
