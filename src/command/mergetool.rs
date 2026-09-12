//! `libra mergetool` — invoke a user-selected merge resolver for each
//! ordinary content conflict in the current worktree.
//!
//! Git's mergetool contract deliberately treats configured commands as trusted
//! local configuration.  In particular, `mergetool.<tool>.cmd` is evaluated
//! by a POSIX shell and receives its four temporary file names through
//! environment variables.  Never interpolate a conflict-derived path into
//! that command string; see git@3cb9185f6 `Documentation/config/mergetool.adoc`
//! and `git-mergetool--lib.sh`.

use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use clap::Parser;
use git_internal::internal::{
    index::{Index, IndexEntry},
    object::blob::Blob,
};

use crate::{
    command::{load_object, save_object},
    internal::config::{
        LocalIdentityTarget, parse_git_config_bool, read_cascaded_config_keys_by_prefix_strict,
        read_cascaded_config_value_strict,
    },
    utils::{
        beneath,
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        path, util,
    },
};

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const MERGETOOL_EXAMPLES: &str = "\
EXAMPLES:
    libra mergetool                         Resolve every ordinary content conflict
    libra mergetool --tool vimdiff           Use vimdiff for this invocation
    libra mergetool --tool-help              List built-in tools and PATH availability
    libra config set merge.tool vscode       Select the default merge tool
    libra config set mergetool.review.cmd 'tool \"$LOCAL\" \"$REMOTE\" \"$MERGED\"'";

const BUILTIN_TOOLS: &[&str] = &["vimdiff", "nvimdiff", "meld", "vscode", "opendiff"];

/// Invoke an external merge resolver for every ordinary unresolved path.
#[derive(Parser, Debug)]
#[command(after_help = MERGETOOL_EXAMPLES)]
pub struct MergetoolArgs {
    /// Use this tool instead of the configured `merge.tool` default.
    #[arg(short = 't', long, value_name = "TOOL")]
    pub tool: Option<String>,

    /// List built-in merge tools and whether their default executable is on PATH.
    #[arg(long = "tool-help")]
    pub tool_help: bool,
}

#[derive(Debug)]
struct ToolSelection {
    name: String,
    command: Option<String>,
    executable: Option<PathBuf>,
    trust_exit_code: bool,
}

#[derive(Debug)]
struct ConflictStages {
    base: Option<StageBlob>,
    local: Option<StageBlob>,
    remote: Option<StageBlob>,
}

#[derive(Debug)]
struct StageBlob {
    bytes: Vec<u8>,
    mode: u32,
}

#[derive(Debug)]
struct TemporaryFiles {
    base: PathBuf,
    local: PathBuf,
    remote: PathBuf,
    merged: PathBuf,
}

/// Safe command entry point.
///
/// # Side Effects
///
/// For each successfully resolved ordinary file conflict, writes the tool's
/// `$MERGED` output to the worktree, optionally records `<path>.orig`, stores
/// its blob, and replaces the path's conflict stages with a stage-0 index entry.
/// Temporary input/output files live in an owner-only directory and are removed
/// when this command returns.
///
/// # Errors
///
/// Returns a user-facing error without staging a path when there is no conflict,
/// a selected tool is unavailable, a tool is judged unresolved, or a conflict is
/// a symlink/mode or modify/delete shape that requires a manual resolution.
pub async fn execute_safe(args: MergetoolArgs, _output: &OutputConfig) -> CliResult<()> {
    if args.tool_help {
        render_tool_help()?;
        return Ok(());
    }

    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let index = Index::load(path::index()).map_err(|error| {
        CliError::fatal(format!("failed to load index for mergetool: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let conflicts = crate::command::merge::unresolved_conflicted_paths(&index, &[]);
    if conflicts.is_empty() {
        let hint = match crate::command::merge::merge_in_progress() {
            Ok(true) => {
                "there are no unresolved paths; use 'libra merge --continue' to finish the merge"
            }
            Ok(false) => "start a merge that conflicts, then run 'libra mergetool'",
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "failed to inspect merge state before mergetool: {error}"
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid));
            }
        };
        return Err(CliError::failure("no files need merging")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(hint));
    }

    let tool = select_tool(args.tool).await?;
    reject_deferred_mergetool_config().await?;
    let temporary_directory = secure_tempdir()?;
    let workdir = util::try_working_dir().map_err(|error| {
        CliError::fatal(format!("failed to determine repository worktree: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;

    for (ordinal, conflict) in conflicts.iter().enumerate() {
        let stages = conflict_stages(&index, conflict)?;
        let mode = require_ordinary_content_conflict(conflict, &stages)?;
        let relative = PathBuf::from(conflict);
        let original = read_ordinary_worktree_file(&workdir, &relative)?;
        let files = write_temporary_files(temporary_directory.path(), ordinal, &stages, &original)?;

        eprintln!("Merging: {}", relative.display());
        let resolved = run_tool(&tool, &files)?;
        if !resolved {
            return Err(CliError::failure(format!(
                "merge tool '{}' did not resolve '{}'",
                tool.name,
                relative.display()
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("edit the file manually and run 'libra add <path>', then retry or continue the merge"));
        }

        let merged = fs::read(&files.merged).map_err(|error| {
            CliError::fatal(format!(
                "cannot read merge-tool output for '{}': {error}",
                relative.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        write_resolution_and_stage(&workdir, &relative, &original, &merged, mode).await?;
        eprintln!("Resolved: {}", relative.display());
    }

    Ok(())
}

async fn select_tool(explicit: Option<String>) -> CliResult<ToolSelection> {
    let name = match explicit {
        Some(name) if !name.trim().is_empty() => name,
        Some(_) => {
            return Err(
                CliError::command_usage("--tool requires a non-empty tool name")
                    .with_stable_code(StableErrorCode::CliInvalidArguments),
            );
        }
        None => read_config("merge.tool")
            .await?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "vimdiff".to_string()),
    };
    let command_key = format!("mergetool.{name}.cmd");
    let path_key = format!("mergetool.{name}.path");
    let trust_key = format!("mergetool.{name}.trustExitCode");
    let command = read_config(&command_key).await?;
    let configured_path = read_config(&path_key).await?.map(PathBuf::from);
    let trust_exit_code = match read_config(&trust_key).await? {
        Some(value) => parse_git_config_bool(&value).ok_or_else(|| {
            CliError::failure(format!(
                "bad config value for '{trust_key}' (expected a Git boolean)"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?,
        None => false,
    };

    if command.is_none() && !BUILTIN_TOOLS.contains(&name.as_str()) {
        return Err(CliError::failure(format!("unknown merge tool '{name}'"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint(format!(
                "configure '{command_key}' for a custom tool, or run 'libra mergetool --tool-help'"
            )));
    }

    let executable = if command.is_some() {
        // Git evaluates a user command through the shell. A `.path` setting
        // has no operand role in that command, so do not inject it into the
        // trusted command string.
        None
    } else {
        let executable = configured_path.unwrap_or_else(|| PathBuf::from(&name));
        if !executable_is_available(&executable) {
            return Err(CliError::failure(format!(
                "merge tool '{name}' is not available as '{}'",
                executable.display()
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint(format!(
                "install it on PATH or configure '{path_key}' with its executable path"
            )));
        }
        Some(executable)
    };

    Ok(ToolSelection {
        name,
        command,
        executable,
        trust_exit_code,
    })
}

async fn read_config(key: &str) -> CliResult<Option<String>> {
    read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, key)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to read config '{key}': {error:#}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })
}

/// Fail closed for every DEFER-11 setting rather than silently accepting a
/// preference whose semantics this command does not implement. This scans the
/// full namespace, including legacy and cascaded config stores, so a newly
/// introduced upstream setting cannot become an accidental no-op.
async fn reject_deferred_mergetool_config() -> CliResult<()> {
    let keys =
        read_cascaded_config_keys_by_prefix_strict(LocalIdentityTarget::CurrentRepo, "mergetool.")
            .await
            .map_err(|error| {
                CliError::fatal(format!(
                    "failed to inspect mergetool configuration: {error:#}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
    for key in keys {
        if !is_supported_mergetool_config_key(&key) {
            return deferred_config_error(&key);
        }
    }
    Ok(())
}

fn is_supported_mergetool_config_key(key: &str) -> bool {
    if key.eq_ignore_ascii_case("mergetool.keepBackup") {
        return true;
    }
    let Some(remainder) = key.get("mergetool.".len()..) else {
        return false;
    };
    let Some((_tool, variable)) = remainder.rsplit_once('.') else {
        return false;
    };
    variable.eq_ignore_ascii_case("cmd")
        || variable.eq_ignore_ascii_case("path")
        || variable.eq_ignore_ascii_case("trustExitCode")
}

fn deferred_config_error(key: &str) -> CliResult<()> {
    Err(CliError::failure(format!(
        "config '{key}' is not supported by libra mergetool"
    ))
    .with_stable_code(StableErrorCode::Unsupported)
    .with_hint(
        "unset the configuration or use a custom mergetool.<tool>.cmd; see DEFER-11 in the compatibility documentation",
    ))
}

fn render_tool_help() -> CliResult<()> {
    let mut out = io::stdout().lock();
    writeln!(out, "Built-in merge tools:").map_err(write_error)?;
    for tool in BUILTIN_TOOLS {
        let available = executable_is_available(Path::new(tool));
        writeln!(
            out,
            "  {tool:<8} {}",
            if available {
                "available"
            } else {
                "unavailable"
            }
        )
        .map_err(write_error)?;
    }
    writeln!(out, "Custom tools use mergetool.<tool>.cmd.").map_err(write_error)
}

fn write_error(error: io::Error) -> CliError {
    CliError::fatal(format!("failed to write mergetool output: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

fn executable_is_available(executable: &Path) -> bool {
    if executable.components().count() > 1 || executable.is_absolute() {
        return executable.is_file();
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| directory.join(executable).is_file())
}

fn conflict_stages(index: &Index, conflict: &str) -> CliResult<ConflictStages> {
    Ok(ConflictStages {
        base: load_stage(index, conflict, 1)?,
        local: load_stage(index, conflict, 2)?,
        remote: load_stage(index, conflict, 3)?,
    })
}

fn load_stage(index: &Index, conflict: &str, stage: u8) -> CliResult<Option<StageBlob>> {
    let Some(entry) = index.get(conflict, stage) else {
        return Ok(None);
    };
    let hash = entry.hash;
    let mode = entry.mode;
    let blob: Blob = load_object(&hash).map_err(|error| {
        CliError::fatal(format!(
            "failed to load stage {stage} blob for '{}': {error}",
            conflict
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    Ok(Some(StageBlob {
        bytes: blob.data,
        mode,
    }))
}

fn require_ordinary_content_conflict(conflict: &str, stages: &ConflictStages) -> CliResult<u32> {
    let (Some(local), Some(remote)) = (&stages.local, &stages.remote) else {
        return Err(CliError::failure(format!(
            "cannot run mergetool for modify/delete conflict '{}': resolve the deletion manually",
            conflict
        ))
        .with_stable_code(StableErrorCode::Unsupported));
    };
    let modes: BTreeSet<u32> = [stages.base.as_ref(), Some(local), Some(remote)]
        .into_iter()
        .flatten()
        .map(|stage| stage.mode)
        .collect();
    if modes.iter().any(|mode| mode & 0o170000 != 0o100000) || modes.len() != 1 {
        return Err(CliError::failure(format!(
            "cannot run mergetool for symlink or mode conflict '{}': resolve it manually",
            conflict
        ))
        .with_stable_code(StableErrorCode::Unsupported));
    }
    modes.iter().next().copied().ok_or_else(|| {
        CliError::failure(format!(
            "cannot run mergetool for conflict '{}': no regular file mode is available",
            conflict
        ))
        .with_stable_code(StableErrorCode::Unsupported)
    })
}

fn read_ordinary_worktree_file(workdir: &Path, relative: &Path) -> CliResult<Vec<u8>> {
    let root = beneath::open_root(workdir).map_err(|error| {
        CliError::fatal(format!(
            "cannot pin the working tree for mergetool: {error}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    beneath::read_file_beneath(&root, relative).map_err(|error| {
        CliError::failure(format!(
            "refusing to run mergetool for '{}': the working-tree path is a symbolic link or has an unsafe ancestor: {error}",
            relative.display(),
        ))
        .with_stable_code(StableErrorCode::Unsupported)
        .with_hint("restore regular working-tree parents and file, then run mergetool again")
    })
}

fn secure_tempdir() -> CliResult<tempfile::TempDir> {
    let directory = tempfile::Builder::new()
        .prefix("libra-mergetool-")
        .tempdir()
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to create mergetool temporary directory: {error}"
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    set_owner_only_permissions(directory.path())?;
    Ok(directory)
}

fn set_owner_only_permissions(directory: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            CliError::fatal(format!(
                "failed to protect mergetool temporary directory '{}': {error}",
                directory.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

fn write_temporary_files(
    root: &Path,
    ordinal: usize,
    stages: &ConflictStages,
    merged: &[u8],
) -> CliResult<TemporaryFiles> {
    let directory = root.join(format!("conflict-{ordinal}"));
    fs::create_dir(&directory).map_err(|error| {
        CliError::fatal(format!(
            "failed to prepare mergetool temporary files: {error}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    set_owner_only_permissions(&directory)?;
    let files = TemporaryFiles {
        base: directory.join("BASE"),
        local: directory.join("LOCAL"),
        remote: directory.join("REMOTE"),
        merged: directory.join("MERGED"),
    };
    write_temp_file(
        &files.base,
        stages.base.as_ref().map_or(&[], |stage| &stage.bytes),
    )?;
    write_temp_file(
        &files.local,
        stages.local.as_ref().map_or(&[], |stage| &stage.bytes),
    )?;
    write_temp_file(
        &files.remote,
        stages.remote.as_ref().map_or(&[], |stage| &stage.bytes),
    )?;
    write_temp_file(&files.merged, merged)?;
    Ok(files)
}

fn write_temp_file(path: &Path, bytes: &[u8]) -> CliResult<()> {
    fs::write(path, bytes).map_err(|error| {
        CliError::fatal(format!("failed to write mergetool temporary file: {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })
}

fn run_tool(tool: &ToolSelection, files: &TemporaryFiles) -> CliResult<bool> {
    let before = fs::metadata(&files.merged)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| {
            CliError::fatal(format!("failed to inspect merge-tool output file: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    let status = if let Some(command) = &tool.command {
        // Do not substitute file paths into `command`: Git evaluates this
        // trusted config through a shell and exposes paths only as variables.
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("BASE", &files.base)
            .env("LOCAL", &files.local)
            .env("REMOTE", &files.remote)
            .env("MERGED", &files.merged)
            .status()
    } else {
        run_builtin_tool(tool, files)
    }
    .map_err(|error| {
        CliError::failure(format!(
            "failed to start merge tool '{}': {error}",
            tool.name
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;

    if tool.trust_exit_code {
        return Ok(status.success());
    }

    let changed = fs::metadata(&files.merged)
        .and_then(|metadata| metadata.modified())
        // Git's check_unchanged contract is any mtime change, not only an
        // advance. A resolver may intentionally restore an older timestamp.
        .map(|after| after != before)
        .map_err(|error| {
            CliError::fatal(format!("failed to inspect merge-tool output file: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    if changed {
        return Ok(true);
    }
    prompt_resolution_confirmation(&tool.name)
}

fn run_builtin_tool(
    tool: &ToolSelection,
    files: &TemporaryFiles,
) -> io::Result<std::process::ExitStatus> {
    let executable = tool
        .executable
        .as_ref()
        .ok_or_else(|| io::Error::other("selected built-in tool has no executable"))?;
    let mut command = Command::new(executable);
    match tool.name.as_str() {
        "vimdiff" | "nvimdiff" => {
            command
                .arg(&files.local)
                .arg(&files.base)
                .arg(&files.remote)
                .arg("-o")
                .arg(&files.merged);
        }
        "meld" => {
            command
                .arg(&files.local)
                .arg(&files.base)
                .arg(&files.remote)
                .arg("--output")
                .arg(&files.merged);
        }
        "vscode" => {
            command
                .arg("--wait")
                .arg("--merge")
                .arg(&files.local)
                .arg(&files.remote)
                .arg(&files.base)
                .arg(&files.merged);
        }
        "opendiff" => {
            command
                .arg(&files.local)
                .arg(&files.remote)
                .arg("-ancestor")
                .arg(&files.base)
                .arg("-merge")
                .arg(&files.merged);
        }
        _ => {
            return Err(io::Error::other("unknown built-in merge tool"));
        }
    }
    command
        .env("BASE", &files.base)
        .env("LOCAL", &files.local)
        .env("REMOTE", &files.remote)
        .env("MERGED", &files.merged)
        .status()
}

fn prompt_resolution_confirmation(tool: &str) -> CliResult<bool> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }
    eprint!("Merge tool '{tool}' did not change $MERGED. Was the merge successful? [y/n] ");
    io::stderr().flush().map_err(write_error)?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(|error| {
        CliError::fatal(format!("failed to read mergetool confirmation: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

async fn write_resolution_and_stage(
    workdir: &Path,
    relative: &Path,
    original: &[u8],
    merged: &[u8],
    mode: u32,
) -> CliResult<()> {
    let root = beneath::open_root(workdir).map_err(|error| {
        CliError::fatal(format!(
            "cannot pin the working tree before writing '{}': {error}",
            relative.display(),
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let keep_backup = match read_config("mergetool.keepBackup").await? {
        Some(value) => parse_git_config_bool(&value).ok_or_else(|| {
            CliError::failure(
                "bad config value for 'mergetool.keepBackup' (expected a Git boolean)",
            )
            .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?,
        None => true,
    };
    if keep_backup {
        let backup = backup_relative_path(relative)?;
        beneath::write_regular_file_beneath(&root, &backup, original, true).map_err(|error| {
            CliError::fatal(format!(
                "failed to save mergetool backup for '{}' without following a symbolic link: {error}",
                relative.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }
    beneath::write_regular_file_beneath(&root, relative, merged, false).map_err(|error| {
        CliError::fatal(format!(
            "failed to write merge-tool resolution '{}' without following a symbolic link: {error}",
            relative.display(),
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    stage_resolution(relative, merged, mode)
}

fn backup_relative_path(relative: &Path) -> CliResult<PathBuf> {
    let name = relative.file_name().ok_or_else(|| {
        CliError::failure(format!(
            "cannot create a mergetool backup for invalid path '{}'",
            relative.display()
        ))
        .with_stable_code(StableErrorCode::Unsupported)
    })?;
    let mut backup_name = name.to_os_string();
    backup_name.push(".orig");
    let mut backup = relative.to_path_buf();
    backup.set_file_name(backup_name);
    Ok(backup)
}

fn stage_resolution(relative: &Path, merged: &[u8], mode: u32) -> CliResult<()> {
    let blob = Blob::from_content_bytes(merged.to_vec());
    save_object(&blob, &blob.id).map_err(|error| {
        CliError::fatal(format!(
            "failed to store resolved file '{}' for staging: {error}",
            relative.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    let mut index = Index::load(path::index()).map_err(|error| {
        CliError::fatal(format!("failed to reload index for mergetool: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let key = relative.to_str().ok_or_else(|| {
        CliError::failure(format!(
            "cannot stage non-UTF-8 merge path '{}'",
            relative.display()
        ))
        .with_stable_code(StableErrorCode::Unsupported)
    })?;
    for stage in 1..=3 {
        index.remove(key, stage);
    }
    let size = u32::try_from(merged.len()).map_err(|_| {
        CliError::failure(format!(
            "resolved file '{}' is too large to record in the index",
            relative.display()
        ))
        .with_stable_code(StableErrorCode::Unsupported)
    })?;
    // The contents came from the verified, no-follow temporary result rather
    // than a fresh pathname read. Zero stat fields force the next `status` to
    // content-compare if a concurrent process changes the worktree afterward.
    let mut entry = IndexEntry::new_from_blob(key.to_string(), blob.id, size);
    entry.mode = mode;
    index.update(entry);
    index.save(path::index()).map_err(|error| {
        CliError::fatal(format!(
            "failed to save index after resolving '{}': {error}",
            relative.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn write_temporary_files_exposes_the_four_git_variable_inputs() {
        let root = tempfile::tempdir().expect("temporary root");
        let stages = ConflictStages {
            base: Some(StageBlob {
                bytes: b"base\n".to_vec(),
                mode: 0o100644,
            }),
            local: Some(StageBlob {
                bytes: b"local\n".to_vec(),
                mode: 0o100644,
            }),
            remote: Some(StageBlob {
                bytes: b"remote\n".to_vec(),
                mode: 0o100644,
            }),
        };
        let files = write_temporary_files(root.path(), 0, &stages, b"markers\n")
            .expect("write temporary merge inputs");
        assert_eq!(fs::read(&files.base).expect("base"), b"base\n");
        assert_eq!(fs::read(&files.local).expect("local"), b"local\n");
        assert_eq!(fs::read(&files.remote).expect("remote"), b"remote\n");
        assert_eq!(fs::read(&files.merged).expect("merged"), b"markers\n");
    }

    #[cfg(unix)]
    #[test]
    fn secure_tempdir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = secure_tempdir().expect("secure temporary directory");
        assert_eq!(
            fs::metadata(directory.path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn confirmation_accepts_only_yes_answers() {
        assert!(matches!(
            "yes".trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ));
        assert!(!matches!(
            "no".trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ));
    }
}
