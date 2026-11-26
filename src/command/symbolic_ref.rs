//! Implements `symbolic-ref` for reading and updating Libra's symbolic HEAD.

use std::io::Write;

use clap::Parser;
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use crate::{
    command::branch::is_valid_git_branch_name,
    internal::{branch::BranchStoreError, head::Head},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

const HEAD_REF: &str = "HEAD";
const HEADS_PREFIX: &str = "refs/heads/";
const REMOTES_PREFIX: &str = "refs/remotes/";

/// `--help` examples shown in `libra symbolic-ref --help` output.
///
/// `symbolic-ref` reads or updates the symbolic target of `HEAD` (the
/// only symbolic ref Libra currently supports). The banner pins the
/// read, short-read, set, quiet, and JSON forms so users see all
/// supported forms without reading the design doc. Cross-cutting
/// `--help` EXAMPLES rollout per `docs/development/commands/_general.md` item B.
pub const SYMBOLIC_REF_EXAMPLES: &str = "\
EXAMPLES:
    libra symbolic-ref HEAD                       Print HEAD's symbolic target (refs/heads/<branch>)
    libra symbolic-ref --short HEAD               Print only the short branch name
    libra symbolic-ref HEAD refs/heads/main       Update HEAD to point at refs/heads/main
    libra symbolic-ref -q HEAD                    Suppress error output when HEAD is detached
    libra symbolic-ref --json HEAD                Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = SYMBOLIC_REF_EXAMPLES)]
pub struct SymbolicRefArgs {
    /// Suppress error output when the ref is not symbolic.
    #[clap(short = 'q', long)]
    pub quiet: bool,

    /// Print only the short branch name.
    #[clap(long)]
    pub short: bool,

    /// Request deleting the symbolic ref. In Libra the only symbolic ref is
    /// `HEAD`, and Git refuses to delete it, so `-d HEAD` always fails with
    /// `deleting 'HEAD' is not allowed` (128); any other name fails with
    /// `Cannot delete <name>, not a symbolic ref` (128). Nothing is ever
    /// deleted — the flag exists for the git `t1401` contract.
    #[clap(short = 'd', long = "delete", conflicts_with = "target")]
    pub delete: bool,

    /// Symbolic ref to read or update. Libra currently supports HEAD.
    #[clap(value_name = "NAME")]
    pub name: Option<String>,

    #[cfg_attr(doc, doc = " New symbolic target. Must be refs/heads/`<branch>`.")]
    #[cfg_attr(not(doc), doc = " New symbolic target. Must be refs/heads/<branch>.")]
    #[clap(value_name = "REF")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SymbolicRefOutput {
    name: String,
    target: String,
    short: Option<String>,
    action: &'static str,
}

pub async fn execute(args: SymbolicRefArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

pub async fn execute_safe(args: SymbolicRefArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let result = run_symbolic_ref(&args, !output.is_json()).await?;

    if output.is_json() {
        emit_json_data("symbolic-ref", &result, output)
    } else if output.quiet || result.action == "set" {
        Ok(())
    } else {
        let value = if args.short {
            result.short.as_deref().unwrap_or(result.target.as_str())
        } else {
            result.target.as_str()
        };
        write_symbolic_ref_output(value)
    }
}

async fn run_symbolic_ref(
    args: &SymbolicRefArgs,
    quiet_detached_head_is_silent: bool,
) -> CliResult<SymbolicRefOutput> {
    let name = args.name.as_deref().unwrap_or(HEAD_REF);
    if let Some(remote) = remote_head_remote(name) {
        return run_remote_head(args, name, remote).await;
    }
    if let Some(branch) = name.strip_prefix(HEADS_PREFIX) {
        return run_local_symref(args, name, branch).await;
    }
    if args.delete {
        // ADR-PL-08: `-d` is accepted for git parity and never deletes
        // anything. The messages match git's `builtin/symbolic-ref.c`; the
        // name is reported as typed (HEAD is the only symbolic ref Libra
        // supports, so every other name is "not a symbolic ref").
        return Err(CliError::fatal(if name == HEAD_REF {
            "deleting 'HEAD' is not allowed".to_string()
        } else {
            format!("Cannot delete {name}, not a symbolic ref")
        })
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    validate_name(name)?;

    if let Some(target) = args.target.as_deref() {
        set_head_target(target).await?;
        return Ok(SymbolicRefOutput {
            name: name.to_string(),
            target: target.to_string(),
            short: Some(branch_name_from_full_ref(target)?.to_string()),
            action: "set",
        });
    }

    let branch_name = match Head::current_result().await.map_err(map_head_error)? {
        Head::Branch(branch_name) => branch_name,
        Head::Detached(_) if args.quiet && quiet_detached_head_is_silent => {
            return Err(CliError::silent_exit(1));
        }
        Head::Detached(_) => {
            let error = CliError::failure("HEAD is not a symbolic ref")
                .with_stable_code(StableErrorCode::CliInvalidTarget);
            let error = if args.quiet {
                error.with_exit_code(1)
            } else {
                error.with_hint("switch to a branch before reading HEAD as a symbolic ref.")
            };
            return Err(error);
        }
    };

    Ok(SymbolicRefOutput {
        name: name.to_string(),
        target: format!("{HEADS_PREFIX}{branch_name}"),
        short: Some(branch_name),
        action: "read",
    })
}

/// The remote name when `name` is `refs/remotes/<remote>/HEAD`.
fn remote_head_remote(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(REMOTES_PREFIX)?;
    let remote = rest.strip_suffix("/HEAD")?;
    (!remote.is_empty() && !remote.contains('/')).then_some(remote)
}

/// Read, write or delete `refs/remotes/<remote>/HEAD`.
async fn run_remote_head(
    args: &SymbolicRefArgs,
    name: &str,
    remote: &str,
) -> CliResult<SymbolicRefOutput> {
    if args.delete {
        Head::delete_remote_head(remote)
            .await
            .map_err(map_head_error)?;
        return Ok(SymbolicRefOutput {
            name: name.to_string(),
            target: String::new(),
            short: None,
            action: "delete",
        });
    }
    if let Some(target) = args.target.as_deref() {
        // Libra stores only the same remote's branch name in a remote HEAD row.
        let Some(branch) = target
            .strip_prefix(REMOTES_PREFIX)
            .and_then(|rest| rest.strip_prefix(remote))
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|branch| !branch.is_empty())
        else {
            return Err(CliError::failure(format!(
                "unsupported target '{target}' for {name}: a remote HEAD can only point at refs/remotes/{remote}/<branch>"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget));
        };
        if !is_valid_git_branch_name(branch) {
            return Err(
                CliError::failure(format!("invalid branch name '{branch}' for {name}"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget),
            );
        }
        Head::update_result(Head::Branch(branch.to_string()), Some(remote))
            .await
            .map_err(map_head_error)?;
        return Ok(SymbolicRefOutput {
            name: name.to_string(),
            target: target.to_string(),
            short: Some(branch.to_string()),
            action: "set",
        });
    }
    let target = match Head::remote_current_result(remote)
        .await
        .map_err(map_head_error)?
    {
        Some(Head::Branch(branch)) => format!("{REMOTES_PREFIX}{remote}/{branch}"),
        _ => {
            return Err(CliError::failure(format!("{name} is not a symbolic ref"))
                .with_stable_code(StableErrorCode::CliInvalidTarget));
        }
    };
    let short = target.strip_prefix(REMOTES_PREFIX).map(str::to_string);
    Ok(SymbolicRefOutput {
        name: name.to_string(),
        target,
        short,
        action: "read",
    })
}

/// Read, write or delete `refs/heads/<branch>` symbolic references stored in
/// the `symbolic_ref` table (PM-01). Creating one refuses a branch that already
/// has a local entity row; the migration's triggers are the second line of
/// defence.
async fn run_local_symref(
    args: &SymbolicRefArgs,
    name: &str,
    branch: &str,
) -> CliResult<SymbolicRefOutput> {
    if !is_valid_git_branch_name(branch) {
        return Err(CliError::failure(format!(
            "invalid branch name '{branch}' in symbolic ref '{name}'"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    match crate::storage::runtime::detect_current_repo_backend().map_err(|error| {
        CliError::fatal(format!(
            "failed to detect storage backend for symbolic ref '{name}': {error}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        crate::storage::StorageBackendKind::Redb => {
            if args.delete {
                return Err(
                    CliError::fatal(format!("Cannot delete {name}, not a symbolic ref"))
                        .with_exit_code(128)
                        .with_stable_code(StableErrorCode::CliInvalidTarget),
                );
            }
            if let Some(target) = args.target.as_deref() {
                validate_symref_target(target)?;
                return Err(CliError::failure(format!(
                    "cannot create symbolic ref '{name}': local branch symbolic refs are not supported on the redb storage backend"
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget));
            }
            Err(not_symbolic(name))
        }
        crate::storage::StorageBackendKind::Sqlite => {
            let db = crate::internal::db::get_db_conn_instance().await;
            if args.delete {
                let deleted = db
                    .execute_raw(Statement::from_sql_and_values(
                        db.get_database_backend(),
                        "DELETE FROM symbolic_ref WHERE name = ?",
                        [name.into()],
                    ))
                    .await
                    .map_err(|error| db_error("delete", name, error))?;
                if deleted.rows_affected() == 0 {
                    // Keep Git's delete wording and exit code for a ref that exists as
                    // an entity branch (or not at all): `Cannot delete <name>, not a
                    // symbolic ref` (128).
                    return Err(CliError::fatal(format!(
                        "Cannot delete {name}, not a symbolic ref"
                    ))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::CliInvalidTarget));
                }
                return Ok(SymbolicRefOutput {
                    name: name.to_string(),
                    target: String::new(),
                    short: None,
                    action: "delete",
                });
            }
            if let Some(target) = args.target.as_deref() {
                validate_symref_target(target)?;
                let txn = crate::internal::db::begin_write_transaction(&db)
                    .await
                    .map_err(|error| db_error("write", name, error))?;
                let entity = crate::internal::branch::Branch::find_branch_result_with_conn(
                    &txn, branch, None,
                )
                .await
                .map_err(map_head_error)?;
                if entity.is_some() {
                    return Err(CliError::failure(format!(
                        "cannot create symbolic ref '{name}': local branch '{branch}' already exists"
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidTarget));
                }
                txn.execute_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "INSERT INTO symbolic_ref (name, target) VALUES (?, ?) \
                     ON CONFLICT(name) DO UPDATE SET target = excluded.target",
                    [name.into(), target.into()],
                ))
                .await
                .map_err(|error| db_error("write", name, error))?;
                txn.commit()
                    .await
                    .map_err(|error| db_error("write", name, error))?;
                return Ok(SymbolicRefOutput {
                    name: name.to_string(),
                    target: target.to_string(),
                    short: Some(branch.to_string()),
                    action: "set",
                });
            }
            let target = query_symref_target(name)
                .await
                .map_err(|error| db_error("read", name, error))?
                .ok_or_else(|| not_symbolic(name))?;
            Ok(SymbolicRefOutput {
                name: name.to_string(),
                target,
                short: Some(branch.to_string()),
                action: "read",
            })
        }
    }
}

async fn query_symref_target(name: &str) -> Result<Option<String>, sea_orm::DbErr> {
    match crate::storage::runtime::detect_current_repo_backend()
        .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?
    {
        crate::storage::StorageBackendKind::Redb => Ok(None),
        crate::storage::StorageBackendKind::Sqlite => {
            let db = crate::internal::db::get_db_conn_instance().await;
            let row = db
                .query_one_raw(Statement::from_sql_and_values(
                    db.get_database_backend(),
                    "SELECT target FROM symbolic_ref WHERE name = ? LIMIT 1",
                    [name.into()],
                ))
                .await?;
            Ok(row.and_then(|row| row.try_get::<String>("", "target").ok()))
        }
    }
}

fn validate_symref_target(target: &str) -> CliResult<()> {
    let branch = if let Some(branch) = target.strip_prefix(HEADS_PREFIX) {
        Some(branch)
    } else {
        target
            .strip_prefix(REMOTES_PREFIX)
            .and_then(|rest| rest.split_once('/'))
            .filter(|(remote, _)| !remote.is_empty())
            .map(|(_, branch)| branch)
    };
    match branch {
        Some(branch) if is_valid_git_branch_name(branch) => Ok(()),
        _ => Err(CliError::failure(format!(
            "invalid symbolic ref target '{target}': expected refs/heads/<branch> or refs/remotes/<remote>/<branch>"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)),
    }
}

fn not_symbolic(name: &str) -> CliError {
    CliError::failure(format!("{name} is not a symbolic ref"))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

fn db_error(action: &str, name: &str, error: sea_orm::DbErr) -> CliError {
    CliError::fatal(format!("failed to {action} symbolic ref '{name}': {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

fn validate_name(name: &str) -> CliResult<()> {
    if name == HEAD_REF {
        return Ok(());
    }

    Err(CliError::failure(format!(
        "unsupported symbolic ref '{name}'; Libra currently supports HEAD"
    ))
    .with_stable_code(StableErrorCode::CliInvalidTarget)
    .with_hint("use 'libra symbolic-ref HEAD' to inspect the current branch."))
}

async fn set_head_target(target: &str) -> CliResult<()> {
    let branch_name = branch_name_from_full_ref(target)?;
    // Part C W0 (§C.11): pointing this worktree's HEAD at a branch already
    // checked out in ANOTHER worktree would create a forbidden duplicate
    // checkout. `branch_checked_out_elsewhere` excludes the current worktree, so
    // re-pointing at this worktree's own current branch is still allowed.
    let checked_out = Head::branch_checked_out_elsewhere_result(branch_name)
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "cannot determine whether branch '{branch_name}' is checked out in another \
                 worktree: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
    if let Some(other) = checked_out {
        return Err(CliError::fatal(format!(
            "cannot point HEAD at '{branch_name}': it is already checked out at worktree '{other}'"
        ))
        // §C.13: branch checked out in another worktree → LBR-CONFLICT-002.
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("check out a different branch, or run the command in that worktree"));
    }
    Head::update_result(Head::Branch(branch_name.to_string()), None)
        .await
        .map_err(map_head_write_error)?;
    Ok(())
}

fn branch_name_from_full_ref(target: &str) -> CliResult<&str> {
    let Some(branch_name) = target.strip_prefix(HEADS_PREFIX) else {
        return Err(CliError::failure(format!(
            "unsupported symbolic ref target '{target}'; expected refs/heads/<branch>"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("use a local branch ref such as 'refs/heads/main'."));
    };

    validate_branch_name(branch_name)?;
    Ok(branch_name)
}

fn validate_branch_name(branch_name: &str) -> CliResult<()> {
    if is_valid_git_branch_name(branch_name) {
        Ok(())
    } else {
        Err(invalid_branch_target(branch_name))
    }
}

fn invalid_branch_target(branch_name: &str) -> CliError {
    CliError::failure(format!(
        "invalid branch name in symbolic ref target: '{branch_name}'"
    ))
    .with_stable_code(StableErrorCode::CliInvalidTarget)
    .with_hint("use a valid local branch name under refs/heads/.")
}

fn write_symbolic_ref_output(value: &str) -> CliResult<()> {
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    match writeln!(writer, "{value}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(
            CliError::fatal(format!("failed to write symbolic-ref output: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        ),
    }
}

fn map_head_error(error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to read HEAD symbolic ref: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        other => CliError::fatal(format!("failed to read HEAD symbolic ref: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

fn map_head_write_error(error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to update HEAD symbolic ref: {detail}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        }
        other => CliError::fatal(format!("failed to update HEAD symbolic ref: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::SymbolicRefArgs;

    #[test]
    fn parses_read_head_defaults() {
        let args = SymbolicRefArgs::try_parse_from(["symbolic-ref"]).unwrap();
        assert!(!args.quiet);
        assert!(!args.short);
        assert!(args.name.is_none());
        assert!(args.target.is_none());
    }

    #[test]
    fn parses_short_head() {
        let args = SymbolicRefArgs::try_parse_from(["symbolic-ref", "--short", "HEAD"]).unwrap();
        assert!(args.short);
        assert_eq!(args.name.as_deref(), Some("HEAD"));
    }

    #[test]
    fn parses_set_head_target() {
        let args = SymbolicRefArgs::try_parse_from(["symbolic-ref", "HEAD", "refs/heads/feature"])
            .unwrap();
        assert_eq!(args.name.as_deref(), Some("HEAD"));
        assert_eq!(args.target.as_deref(), Some("refs/heads/feature"));
    }
}
