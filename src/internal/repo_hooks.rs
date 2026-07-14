//! Sandboxed repository-local hooks under `.libra/hooks`.
//!
//! This is deliberately separate from [`crate::internal::ai::hooks`], which
//! ingests lifecycle events emitted by external AI agents. Repository hooks
//! are user-authored executables invoked around VCS mutations. They therefore
//! run with structured argv (never `sh -c`) inside the required workspace-write
//! sandbox with network denied.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::{
    internal::ai::sandbox::{
        CommandSpec, NetworkAccess, SandboxEnforcement, SandboxPermissions, SandboxPolicy,
        SandboxRuntimeConfig, ToolSandboxContext, run_command_spec,
    },
    utils::{output::OutputConfig, path, util},
};

const HOOK_TIMEOUT_MS: u64 = 15 * 60 * 1_000;
const HOOK_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const HOOK_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const LIBRA_NO_HOOKS_ENV: &str = "LIBRA_NO_HOOKS";

/// Repository-hook lifecycle names. `as_str()` is also the exact canonical
/// filename under `.libra/hooks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoHook {
    PreCommit,
    PrepareCommitMsg,
    CommitMsg,
    PostCommit,
    PostCheckout,
    PreRebase,
    PreMergeCommit,
    PostMerge,
    PostRewrite,
}

impl RepoHook {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreCommit => "pre-commit",
            Self::PrepareCommitMsg => "prepare-commit-msg",
            Self::CommitMsg => "commit-msg",
            Self::PostCommit => "post-commit",
            Self::PostCheckout => "post-checkout",
            Self::PreRebase => "pre-rebase",
            Self::PreMergeCommit => "pre-merge-commit",
            Self::PostMerge => "post-merge",
            Self::PostRewrite => "post-rewrite",
        }
    }
}

#[derive(Debug)]
pub struct RepoHookOutput {
    pub path: PathBuf,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RepoHookError {
    #[error("failed to locate the repository root for hook execution: {0}")]
    Repository(#[source] std::io::Error),
    #[error("failed to inspect repository hooks directory '{path}': {source}")]
    HooksDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("repository hooks directory '{path}' must not be a symbolic link")]
    SymlinkedHooksDirectory { path: PathBuf },
    #[error("repository hooks path '{path}' is not a directory")]
    HooksPathNotDirectory { path: PathBuf },
    #[error("failed to inspect repository hook '{path}': {source}")]
    HookMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("repository hook '{path}' must be a regular file, not a symbolic link")]
    SymlinkedHook { path: PathBuf },
    #[error("repository hook '{path}' is not a regular file")]
    HookNotFile { path: PathBuf },
    #[cfg(unix)]
    #[error("repository hook '{path}' is not executable; run 'chmod +x {path}'")]
    HookNotExecutable { path: PathBuf },
    #[error("repository hook '{path}' is {size} bytes, exceeding the {limit}-byte safety limit")]
    HookTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    #[error("repository hook path '{path}' is not valid UTF-8")]
    NonUtf8HookPath { path: PathBuf },
    #[error("failed to create a private execution copy for repository hook '{path}': {source}")]
    HookCopy {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "repository hook requested an unexpected writable message file '{actual}' (expected '{expected}')"
    )]
    UnexpectedMessageFile { actual: PathBuf, expected: PathBuf },
    #[error("repository hook message file '{path}' must be a regular file, not a symbolic link")]
    SymlinkedMessageFile { path: PathBuf },
    #[error("repository hook message file '{path}' is not a regular file")]
    MessageFileNotFile { path: PathBuf },
    #[error("failed to inspect repository hook message file '{path}': {source}")]
    MessageFileMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sandboxed repository hook '{path}' could not run: {detail}")]
    Sandbox { path: PathBuf, detail: String },
}

/// Run one repository hook if its canonical file exists.
///
/// Resolution is deterministic: the extensionless Git-shaped name wins, then
/// the platform compatibility suffix (`.sh` on Unix, `.ps1` on Windows). Only
/// one file runs. A present but unsafe higher-precedence candidate is an error;
/// it never silently falls through to a different script.
pub async fn run_repo_hook(
    hook: RepoHook,
    args: &[String],
) -> Result<Option<RepoHookOutput>, RepoHookError> {
    run_repo_hook_with_io(hook, args, None, None).await
}

/// Run one repository hook with optional stdin and a single writable commit
/// message file. The exception is deliberately fixed to the current
/// worktree's `COMMIT_EDITMSG`; callers cannot use this API to make arbitrary
/// `.libra` metadata writable inside the hook sandbox.
pub async fn run_repo_hook_with_io(
    hook: RepoHook,
    args: &[String],
    stdin: Option<&[u8]>,
    writable_message_file: Option<&Path>,
) -> Result<Option<RepoHookOutput>, RepoHookError> {
    if repo_hooks_disabled() {
        return Ok(None);
    }
    let repo_root = util::try_working_dir().map_err(RepoHookError::Repository)?;
    let worktree_gitdir = util::try_get_worktree_gitdir(None).map_err(RepoHookError::Repository)?;
    let hooks_dir = path::hooks();
    let hooks_metadata = match fs::symlink_metadata(&hooks_dir) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(RepoHookError::HooksDirectory {
                path: hooks_dir,
                source,
            });
        }
    };
    if hooks_metadata.file_type().is_symlink() {
        return Err(RepoHookError::SymlinkedHooksDirectory { path: hooks_dir });
    }
    if !hooks_metadata.is_dir() {
        return Err(RepoHookError::HooksPathNotDirectory { path: hooks_dir });
    }

    let Some(hook_path) = resolve_hook_path(&hooks_dir, hook)? else {
        return Ok(None);
    };
    let execution_dir = tempfile::Builder::new()
        .prefix("hook-run-")
        .tempdir_in(&worktree_gitdir)
        .map_err(|source| RepoHookError::HookCopy {
            path: hook_path.clone(),
            source,
        })?;
    let execution_path = copy_hook_for_execution(&hook_path, execution_dir.path())?;
    if hook == RepoHook::PreCommit && is_shipped_noop_pre_commit(&execution_path)? {
        // `libra init` has historically installed an inert pre-commit example.
        // Treat the exact shipped bytes as an absent hook: this avoids paying
        // sandbox startup cost for a no-op and, importantly, keeps initialized
        // Windows repositories usable until the restricted-token backend lands.
        // Any customization changes the bytes and therefore remains subject to
        // required, fail-closed sandbox enforcement.
        return Ok(None);
    }
    let hook_program = execution_path
        .to_str()
        .ok_or_else(|| RepoHookError::NonUtf8HookPath {
            path: execution_path.clone(),
        })?;

    let mut writable_roots = vec![repo_root.clone()];
    if let Some(message_file) = writable_message_file {
        let expected = worktree_gitdir.join("COMMIT_EDITMSG");
        if message_file != expected {
            return Err(RepoHookError::UnexpectedMessageFile {
                actual: message_file.to_path_buf(),
                expected,
            });
        }
        let metadata = fs::symlink_metadata(message_file).map_err(|source| {
            RepoHookError::MessageFileMetadata {
                path: message_file.to_path_buf(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(RepoHookError::SymlinkedMessageFile {
                path: message_file.to_path_buf(),
            });
        }
        if !metadata.is_file() {
            return Err(RepoHookError::MessageFileNotFile {
                path: message_file.to_path_buf(),
            });
        }
        writable_roots.push(message_file.to_path_buf());
    }

    let mut env = inherited_hook_environment();
    env.insert("LIBRA_HOOK_NAME".to_string(), hook.as_str().to_string());
    env.insert(
        "LIBRA_DIR".to_string(),
        worktree_gitdir.to_string_lossy().into_owned(),
    );
    env.insert(
        "LIBRA_COMMON_DIR".to_string(),
        util::storage_path().to_string_lossy().into_owned(),
    );
    env.insert(
        "LIBRA_HOOK_SOURCE".to_string(),
        hook_path.to_string_lossy().into_owned(),
    );
    env.insert(
        "LIBRA_WORK_TREE".to_string(),
        repo_root.to_string_lossy().into_owned(),
    );

    #[cfg(target_os = "windows")]
    let (program, command_args) =
        if execution_path.extension().and_then(|ext| ext.to_str()) == Some("ps1") {
            let mut command_args = vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-File".to_string(),
                hook_program.to_string(),
            ];
            command_args.extend_from_slice(args);
            ("powershell".to_string(), command_args)
        } else {
            (hook_program.to_string(), args.to_vec())
        };
    #[cfg(not(target_os = "windows"))]
    let (program, command_args) = (hook_program.to_string(), args.to_vec());

    let spec = CommandSpec {
        program,
        args: command_args,
        cwd: repo_root.clone(),
        env,
        clear_env: true,
        stdin: stdin.map(ToOwned::to_owned),
        timeout_ms: Some(HOOK_TIMEOUT_MS),
        sandbox_permissions: SandboxPermissions::UseDefault,
        justification: None,
    };
    let sandbox = ToolSandboxContext {
        policy: SandboxPolicy::WorkspaceWrite {
            writable_roots,
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        permissions: SandboxPermissions::UseDefault,
    };
    let runtime = SandboxRuntimeConfig {
        enforcement: SandboxEnforcement::Required,
        use_linux_sandbox_bwrap: true,
        ..Default::default()
    };
    let output = run_command_spec(
        spec,
        HOOK_MAX_OUTPUT_BYTES,
        Some(sandbox),
        Some(&runtime),
        None,
        None,
    )
    .await
    .map_err(|detail| RepoHookError::Sandbox {
        path: hook_path.clone(),
        detail,
    })?;

    Ok(Some(RepoHookOutput {
        path: hook_path,
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        timed_out: output.timed_out,
    }))
}

/// Repository content controls hook code, so never pass through arbitrary
/// caller variables such as API tokens or cloud credentials. Keep only the
/// small process/locale surface needed to locate system tools and render text;
/// command-private temp variables are injected by the sandbox runner.
fn inherited_hook_environment() -> HashMap<String, String> {
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "TZ",
        "SYSTEMROOT",
        "COMSPEC",
        "PATHEXT",
    ];
    ALLOWED
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_string(), value))
        })
        .collect()
}

fn repo_hooks_disabled() -> bool {
    std::env::var(LIBRA_NO_HOOKS_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
}

/// Replay captured hook output only on the human surface. JSON and machine
/// output remain single-envelope streams.
pub fn replay_repo_hook_output(
    hook_output: &RepoHookOutput,
    output: &OutputConfig,
) -> Result<(), String> {
    if output.is_json() || output.quiet {
        return Ok(());
    }
    if let Err(error) = std::io::stdout().write_all(hook_output.stdout.as_bytes())
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(format!(
            "failed to write output from hook '{}': {error}",
            hook_output.path.display()
        ));
    }
    if let Err(error) = std::io::stderr().write_all(hook_output.stderr.as_bytes())
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(format!(
            "failed to write diagnostics from hook '{}': {error}",
            hook_output.path.display()
        ));
    }
    Ok(())
}

/// Run a post-operation hook. Post hooks observe an already-completed state
/// transition, so launch errors, timeouts, and non-zero exits are warnings and
/// never claim the mutation was rolled back.
pub async fn run_advisory_repo_hook(
    hook: RepoHook,
    args: &[String],
    stdin: Option<&[u8]>,
    output: &OutputConfig,
) {
    match run_repo_hook_with_io(hook, args, stdin, None).await {
        Ok(Some(hook_output)) => {
            if let Err(detail) = replay_repo_hook_output(&hook_output, output) {
                warn_advisory_hook(output, hook, &detail);
            }
            if hook_output.timed_out {
                warn_advisory_hook(
                    output,
                    hook,
                    &format!(
                        "hook '{}' exceeded the 15 minute timeout",
                        hook_output.path.display()
                    ),
                );
            } else if hook_output.exit_code != 0 {
                warn_advisory_hook(
                    output,
                    hook,
                    &format!(
                        "hook '{}' exited with code {}",
                        hook_output.path.display(),
                        hook_output.exit_code
                    ),
                );
            }
        }
        Ok(None) => {}
        Err(error) => warn_advisory_hook(output, hook, &error.to_string()),
    }
}

fn warn_advisory_hook(output: &OutputConfig, hook: RepoHook, detail: &str) {
    crate::utils::output::record_warning();
    tracing::warn!(hook = hook.as_str(), "repository hook warning: {detail}");
    if !output.is_json() && !output.quiet {
        eprintln!("warning: {} hook: {detail}", hook.as_str());
    }
}

fn copy_hook_for_execution(
    hook_path: &Path,
    destination_dir: &Path,
) -> Result<PathBuf, RepoHookError> {
    let source = open_hook_without_following_symlinks(hook_path).map_err(|source| {
        RepoHookError::HookCopy {
            path: hook_path.to_path_buf(),
            source,
        }
    })?;
    let metadata = source
        .metadata()
        .map_err(|source| RepoHookError::HookCopy {
            path: hook_path.to_path_buf(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(RepoHookError::HookNotFile {
            path: hook_path.to_path_buf(),
        });
    }
    if metadata.len() > HOOK_MAX_FILE_BYTES {
        return Err(RepoHookError::HookTooLarge {
            path: hook_path.to_path_buf(),
            size: metadata.len(),
            limit: HOOK_MAX_FILE_BYTES,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(RepoHookError::HookNotExecutable {
                path: hook_path.to_path_buf(),
            });
        }
    }

    let file_name = hook_path
        .file_name()
        .ok_or_else(|| RepoHookError::NonUtf8HookPath {
            path: hook_path.to_path_buf(),
        })?;
    let destination = destination_dir.join(file_name);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|source| RepoHookError::HookCopy {
            path: hook_path.to_path_buf(),
            source,
        })?;
    let mut limited_source = Read::take(source, HOOK_MAX_FILE_BYTES + 1);
    let copied =
        io::copy(&mut limited_source, &mut output).map_err(|source| RepoHookError::HookCopy {
            path: hook_path.to_path_buf(),
            source,
        })?;
    if copied > HOOK_MAX_FILE_BYTES {
        return Err(RepoHookError::HookTooLarge {
            path: hook_path.to_path_buf(),
            size: copied,
            limit: HOOK_MAX_FILE_BYTES,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o500)).map_err(|source| {
            RepoHookError::HookCopy {
                path: hook_path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(destination)
}

fn is_shipped_noop_pre_commit(path: &Path) -> Result<bool, RepoHookError> {
    let size = fs::metadata(path)
        .map_err(|source| RepoHookError::HookCopy {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if size != include_bytes!("../../template/pre-commit.sh").len() as u64
        && size != include_bytes!("../../template/pre-commit.ps1").len() as u64
    {
        return Ok(false);
    }
    let contents = fs::read(path).map_err(|source| RepoHookError::HookCopy {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(contents == include_bytes!("../../template/pre-commit.sh")
        || contents == include_bytes!("../../template/pre-commit.ps1"))
}

#[cfg(unix)]
fn open_hook_without_following_symlinks(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_hook_without_following_symlinks(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

fn resolve_hook_path(
    hooks_dir: &std::path::Path,
    hook: RepoHook,
) -> Result<Option<PathBuf>, RepoHookError> {
    let mut candidates = vec![hooks_dir.join(hook.as_str())];
    #[cfg(unix)]
    candidates.push(hooks_dir.join(format!("{}.sh", hook.as_str())));
    #[cfg(target_os = "windows")]
    candidates.push(hooks_dir.join(format!("{}.ps1", hook.as_str())));

    for candidate in candidates {
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(RepoHookError::HookMetadata {
                    path: candidate,
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(RepoHookError::SymlinkedHook { path: candidate });
        }
        if !metadata.is_file() {
            return Err(RepoHookError::HookNotFile { path: candidate });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(RepoHookError::HookNotExecutable { path: candidate });
            }
        }
        return Ok(Some(candidate));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_names_are_git_shaped_and_stable() {
        assert_eq!(RepoHook::PreCommit.as_str(), "pre-commit");
        assert_eq!(RepoHook::PrepareCommitMsg.as_str(), "prepare-commit-msg");
        assert_eq!(RepoHook::CommitMsg.as_str(), "commit-msg");
        assert_eq!(RepoHook::PostCommit.as_str(), "post-commit");
        assert_eq!(RepoHook::PostCheckout.as_str(), "post-checkout");
        assert_eq!(RepoHook::PreRebase.as_str(), "pre-rebase");
        assert_eq!(RepoHook::PreMergeCommit.as_str(), "pre-merge-commit");
        assert_eq!(RepoHook::PostMerge.as_str(), "post-merge");
        assert_eq!(RepoHook::PostRewrite.as_str(), "post-rewrite");
    }

    #[test]
    fn shipped_noop_pre_commit_templates_are_recognized_exactly() {
        let directory = tempfile::tempdir().expect("create hook fixture directory");
        let hook = directory.path().join("pre-commit");

        fs::write(&hook, include_bytes!("../../template/pre-commit.sh"))
            .expect("write shipped shell template");
        assert!(is_shipped_noop_pre_commit(&hook).expect("inspect shipped shell template"));

        fs::write(&hook, b"#!/bin/sh\nexit 0\n# customized\n").expect("write customized hook");
        assert!(
            !is_shipped_noop_pre_commit(&hook).expect("inspect customized hook"),
            "only the exact inert template may bypass sandbox execution"
        );
    }

    #[cfg(unix)]
    #[test]
    fn oversized_hook_is_rejected_before_copying() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("create oversized hook fixture directory");
        let hook = directory.path().join("pre-commit");
        let file = File::create(&hook).expect("create sparse oversized hook");
        file.set_len(HOOK_MAX_FILE_BYTES + 1)
            .expect("size sparse oversized hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
            .expect("make oversized hook executable");
        let destination = directory.path().join("execution");
        fs::create_dir(&destination).expect("create execution directory");

        let error = copy_hook_for_execution(&hook, &destination)
            .expect_err("oversized hooks must fail before copying");
        assert!(matches!(error, RepoHookError::HookTooLarge { .. }));
        assert!(
            fs::read_dir(&destination)
                .expect("inspect execution directory")
                .next()
                .is_none(),
            "rejected hook must not leave a partial private copy"
        );
    }
}
