//! `libra update-ref` — update or delete a ref safely, a focused subset of
//! `git update-ref`. The ref read, the ref write/delete, and the reflog entry
//! all happen inside a single SQLite transaction so a compare-and-swap failure
//! rolls everything back atomically.
//!
//! Scope (v1): operates on `refs/heads/<branch>` only — the branch-tip case
//! Libra's `reference` table models cleanly. `HEAD`, `refs/tags/*`,
//! `refs/remotes/*`, and arbitrary ref namespaces are rejected with guidance
//! (use `symbolic-ref` / `switch` / `tag`), since they are not directly
//! representable here.

use clap::Parser;
use git_internal::{
    errors::GitError,
    hash::{ObjectHash, get_hash_kind},
    internal::object::types::ObjectType,
};
use sea_orm::TransactionError;
use serde::Serialize;

use crate::{
    internal::{
        branch::Branch,
        db::get_db_conn_instance,
        reflog::{Reflog, ReflogAction, ReflogContext},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util::{self, CommitBaseError},
    },
};

const HEADS_PREFIX: &str = "refs/heads/";

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const UPDATE_REF_EXAMPLES: &str = "\
EXAMPLES:
    libra update-ref refs/heads/main <newoid>            Point a branch at a commit
    libra update-ref refs/heads/main <newoid> <oldoid>   Compare-and-swap update
    libra update-ref refs/heads/topic <oid> 0000000...   Create only if absent
    libra update-ref -d refs/heads/old                   Delete a branch ref
    libra update-ref -d refs/heads/old <oldoid>          Delete only if it matches";

/// Update, create, or delete a `refs/heads/<branch>` ref with an optional
/// compare-and-swap against its current value.
#[derive(Parser, Debug)]
#[command(after_help = UPDATE_REF_EXAMPLES)]
pub struct UpdateRefArgs {
    /// Delete the ref instead of updating it.
    #[clap(short = 'd', long = "delete")]
    pub delete: bool,

    /// Reflog reason recorded with the update (Git's `-m`).
    #[clap(short = 'm', value_name = "REASON")]
    pub message: Option<String>,

    /// The ref to update, e.g. `refs/heads/main`.
    #[clap(value_name = "REF")]
    pub ref_name: String,

    /// The new object id (omit with `-d`; with `-d` this position is the
    /// optional old value to verify before deleting).
    #[clap(value_name = "NEWVALUE")]
    pub value: Option<String>,

    /// The expected current object id for a compare-and-swap (`0{40}`/`0{64}`
    /// means "must not already exist"). Only valid without `-d`.
    #[clap(value_name = "OLDVALUE")]
    pub old_value: Option<String>,
}

#[derive(Debug, Serialize)]
struct UpdateRefOutput {
    #[serde(rename = "ref")]
    ref_name: String,
    old: Option<String>,
    new: Option<String>,
    deleted: bool,
}

/// Transaction-internal error, mapped to a 128 `CliError` by the caller.
#[derive(Debug, thiserror::Error)]
enum UpdateRefTxError {
    #[error("cannot lock ref '{ref_name}': is at {actual} but expected {expected}")]
    CasMismatch {
        ref_name: String,
        expected: String,
        actual: String,
    },
    #[error("cannot create ref '{ref_name}': it already exists at {actual}")]
    MustNotExist { ref_name: String, actual: String },
    #[error("cannot delete ref '{ref_name}': it does not exist")]
    DoesNotExist { ref_name: String },
    #[error("ref storage error: {0}")]
    Storage(String),
    #[error("branch '{branch}' is {policy}; refusing to update its ref")]
    PolicyBlocked { branch: String, policy: String },
}

pub async fn execute(args: UpdateRefArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Validates inputs, then performs the read/CAS/write+reflog
/// inside a single transaction. All failures exit 128 (matching Git's fatals).
pub async fn execute_safe(args: UpdateRefArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let fatal = |message: String| {
        CliError::fatal(message)
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    };

    // Only `refs/heads/<branch>` is representable in v1.
    let branch = parse_heads_ref(&args.ref_name).map_err(fatal)?;
    if !util::is_valid_refname(&args.ref_name) {
        return Err(fatal(format!("invalid ref name '{}'", args.ref_name)));
    }
    // Part C W0 (§C.11): refuse to move or delete a branch checked out in
    // ANOTHER worktree — its HEAD would be left dangling or its working tree
    // would silently diverge. `branch_checked_out_elsewhere` excludes the
    // current worktree, so updating this worktree's own branch is still allowed.
    // Fail CLOSED: a probe failure must refuse, never silently allow the
    // cross-worktree move (§C.4.4).
    let checked_out_elsewhere =
        crate::internal::head::Head::branch_checked_out_elsewhere_result(branch)
            .await
            .map_err(|error| {
                CliError::fatal(format!(
                    "cannot verify whether '{branch}' is checked out in another worktree: {error}"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("repair the repository database, then retry")
            })?;
    if let Some(other) = checked_out_elsewhere {
        return Err(CliError::fatal(format!(
            "cannot update '{}': branch '{branch}' is checked out at worktree '{other}'",
            args.ref_name
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("switch that worktree to another branch first, or run the command there"));
    }

    let hash_kind = get_hash_kind();
    let zero = ObjectHash::zero_str(hash_kind);

    // Disambiguate positionals: `-d <ref> [<old>]` vs `<ref> <new> [<old>]`.
    let (new_oid, old_spec) = if args.delete {
        if args.old_value.is_some() {
            return Err(fatal(
                "too many arguments: `update-ref -d <ref> [<oldvalue>]` takes at most one value"
                    .to_string(),
            ));
        }
        (None, args.value.clone())
    } else {
        let Some(new_value) = args.value.clone() else {
            return Err(fatal(format!(
                "missing new value for '{}' (use -d to delete)",
                args.ref_name
            )));
        };
        let new_hash = resolve_new_value(&new_value, &zero, &args.ref_name).await?;
        (Some(new_hash.to_string()), args.old_value.clone())
    };

    // Parse the optional compare-and-swap operand.
    let old_spec = match old_spec {
        Some(value) => Some(resolve_old_value(&value, &zero, &args.ref_name).await?),
        None => None,
    };

    let reflog_reason = args.message.clone().unwrap_or_default();
    let full_ref = args.ref_name.clone();
    let branch_name = branch.to_string();
    let delete = args.delete;

    let db = get_db_conn_instance().await;
    // A compare-and-swap on a ref: it READS the current value and the branch
    // policy before it writes, so it must take the write lock up front or a
    // concurrent writer makes it fail instead of wait (see
    // `db::begin_write_transaction`).
    let outcome = crate::internal::db::write_transaction(&db, move |txn| {
        Box::pin(async move {
            // Branch policy (lore.md 1.13): protect/archive metadata is
            // enforced INSIDE the authoritative txn for every local-head
            // writer — update-ref would otherwise be a silent bypass of
            // `branch reset`'s policy layer. Fail-closed: metadata read
            // errors refuse the update. (update-ref stays plumbing-sharp
            // otherwise — it may still move the checked-out branch, like
            // git update-ref.)
            let protected =
                crate::internal::metadata::MetadataKv::is_protected_with_conn(txn, &branch_name)
                    .await
                    .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
            if protected {
                return Err(UpdateRefTxError::PolicyBlocked {
                    branch: branch_name.clone(),
                    policy: "protected".to_string(),
                });
            }
            let archived =
                crate::internal::metadata::MetadataKv::is_archived_with_conn(txn, &branch_name)
                    .await
                    .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
            if archived {
                return Err(UpdateRefTxError::PolicyBlocked {
                    branch: branch_name.clone(),
                    policy: "archived".to_string(),
                });
            }
            let current = Branch::find_branch_result_with_conn(txn, &branch_name, None)
                .await
                .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?
                .map(|b| b.commit.to_string());

            // Compare-and-swap precondition.
            if let Some(expected) = &old_spec {
                match (expected, &current) {
                    // `0{40}` => the ref must not exist.
                    (OldValue::MustNotExist, Some(actual)) => {
                        return Err(UpdateRefTxError::MustNotExist {
                            ref_name: full_ref.clone(),
                            actual: actual.clone(),
                        });
                    }
                    (OldValue::MustNotExist, None) => {}
                    (OldValue::Exact(want), actual) if actual.as_deref() != Some(want.as_str()) => {
                        return Err(UpdateRefTxError::CasMismatch {
                            ref_name: full_ref.clone(),
                            expected: want.clone(),
                            actual: actual.clone().unwrap_or_else(|| zero.clone()),
                        });
                    }
                    (OldValue::Exact(_), _) => {}
                }
            }

            if delete {
                let Some(old) = current.clone() else {
                    return Err(UpdateRefTxError::DoesNotExist {
                        ref_name: full_ref.clone(),
                    });
                };
                Branch::delete_branch_result_with_conn(txn, &branch_name, None)
                    .await
                    .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
                write_reflog(txn, &full_ref, &old, &zero, &reflog_reason).await?;
                Ok(UpdateRefOutcome {
                    old: Some(old),
                    new: None,
                })
            } else {
                // INVARIANT: in the non-delete branch the positional
                // disambiguation above always set `new_oid` to `Some`.
                let new = new_oid.expect("new value validated for non-delete");
                Branch::update_branch_with_conn(txn, &branch_name, &new, None)
                    .await
                    .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
                let old = current.clone().unwrap_or_else(|| zero.clone());
                write_reflog(txn, &full_ref, &old, &new, &reflog_reason).await?;
                Ok::<_, UpdateRefTxError>(UpdateRefOutcome {
                    old: current,
                    new: Some(new),
                })
            }
        })
    })
    .await
    .map_err(|error| {
        // Preserve the policy refusal's dedicated stable code.
        if let TransactionError::Transaction(UpdateRefTxError::PolicyBlocked { branch, policy }) =
            &error
        {
            let policy_key = if policy == "protected" {
                "protect"
            } else {
                "archive"
            };
            CliError::fatal(format!(
                "branch '{branch}' is {policy}; refusing to update its ref"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::PolicyRefUpdateBlocked)
            .with_hint(format!(
                "clear it first: 'libra metadata unset --branch {branch} {policy_key}'"
            ))
        } else {
            let message = match error {
                TransactionError::Connection(error) => error.to_string(),
                TransactionError::Transaction(error) => error.to_string(),
            };
            CliError::fatal(message)
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        }
    })?;

    if output.is_json() {
        emit_json_data(
            "update-ref",
            &UpdateRefOutput {
                ref_name: args.ref_name,
                old: outcome.old,
                new: outcome.new,
                deleted: args.delete,
            },
            output,
        )
    } else {
        Ok(())
    }
}

struct UpdateRefOutcome {
    old: Option<String>,
    new: Option<String>,
}

/// The parsed compare-and-swap operand.
#[derive(Clone)]
enum OldValue {
    /// `0{40}` / `0{64}`: the ref must not already exist.
    MustNotExist,
    /// An exact object id the ref must currently point to.
    Exact(String),
}

/// Write a single `update-ref` reflog entry (never leaks the user's CAS operand).
async fn write_reflog<C: sea_orm::ConnectionTrait>(
    txn: &C,
    full_ref: &str,
    old: &str,
    new: &str,
    reason: &str,
) -> Result<(), UpdateRefTxError> {
    let context = ReflogContext {
        old_oid: old.to_string(),
        new_oid: new.to_string(),
        action: ReflogAction::UpdateRef {
            message: reason.to_string(),
        },
    };
    Reflog::insert_single_entry(txn, &context, full_ref)
        .await
        .map_err(|error| UpdateRefTxError::Storage(error.to_string()))
}

/// Require a `refs/heads/<branch>` ref and return the short branch name.
fn parse_heads_ref(ref_name: &str) -> Result<&str, String> {
    if ref_name == "HEAD" {
        return Err(
            "update-ref cannot operate on HEAD; use `symbolic-ref` or `switch` instead".to_string(),
        );
    }
    if let Some(branch) = ref_name.strip_prefix(HEADS_PREFIX) {
        if branch.is_empty() {
            return Err("missing branch name after refs/heads/".to_string());
        }
        return Ok(branch);
    }
    Err(format!(
        "unsupported ref '{ref_name}': update-ref supports refs/heads/<branch> only \
         (use `tag` for refs/tags/*, `symbolic-ref`/`switch` for HEAD)"
    ))
}

/// Resolve the `<newvalue>` operand to the commit the ref will point at.
///
/// Two syntax-layer spellings are refused before any lookup and keep the
/// usage class (`LBR-CLI-002`): `ref:` (that is `symbolic-ref`'s job) and the
/// null id (that is `-d`'s job). Everything else goes through the shared
/// revision engine, so branch names, tags, `HEAD`, `~`/`^` navigation and
/// abbreviated ids all work.
///
/// There is deliberately **no implicit peel**: whatever the expression names
/// must itself be a commit. That is Git's rule — a lightweight tag points at
/// the commit and is accepted, a bare annotated tag names a tag object and is
/// refused, and `<tag>^{commit}` peels explicitly and is accepted. Peeling
/// silently would let `update-ref refs/heads/x v1.0` write a branch the user
/// never named.
async fn resolve_new_value(value: &str, zero: &str, ref_name: &str) -> CliResult<ObjectHash> {
    if value.starts_with("ref:") {
        return Err(usage_fatal(
            "symbolic refs are not supported by update-ref; use `symbolic-ref`".to_string(),
        ));
    }
    if value == zero {
        return Err(usage_fatal(
            "refusing to point a ref at the null object id; use -d to delete".to_string(),
        ));
    }

    let object_id = util::resolve_object_spec_typed(value)
        .await
        .map_err(|error| resolver_error(error, value, ref_name))?;

    // Git's update-ref refuses to point a ref at an object that is not in the
    // store; do the same so we never create a dangling ref. Reading the type
    // proves existence and answers the commit question in one lookup.
    let object_type = util::objects_storage()
        .get_object_type(&object_id)
        .map_err(|error| match error {
            GitError::ObjectNotFound(_) => invalid_target_fatal(format!(
                "cannot update '{ref_name}': object {value} does not exist in the repository"
            )),
            other => CliError::fatal(format!(
                "cannot update '{ref_name}': could not read object {object_id}: {other}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt),
        })?;

    if object_type != ObjectType::Commit {
        return Err(invalid_target_fatal(format!(
            "cannot update '{ref_name}': '{value}' resolves to a {object_type}              ({object_id}), not a commit; use '{value}^{{commit}}' to peel it"
        )));
    }

    Ok(object_id)
}

/// A syntax-layer refusal of an operand: the user's command line is wrong.
fn usage_fatal(message: String) -> CliError {
    CliError::fatal(message)
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

/// The operand parsed, but does not name something this command can use.
fn invalid_target_fatal(message: String) -> CliError {
    CliError::fatal(message)
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

/// Resolve a compare-and-swap operand (`0{40}` => must-not-exist).
///
/// Same revision entry point as `<newvalue>`, and deliberately **no commit
/// type check**: `<oldvalue>` states what the ref is expected to point at
/// right now, so the resolved id is compared verbatim. Naming an annotated tag
/// here therefore produces an ordinary CAS mismatch — Git behaves the same
/// way, because a ref that points at a tag object is a state you are allowed
/// to assert and be wrong about, not a malformed request.
async fn resolve_old_value(value: &str, zero: &str, ref_name: &str) -> CliResult<OldValue> {
    if value == zero {
        return Ok(OldValue::MustNotExist);
    }
    if value.starts_with("ref:") {
        return Err(usage_fatal(
            "symbolic refs are not supported by update-ref; use `symbolic-ref`".to_string(),
        ));
    }
    let object_id = util::resolve_object_spec_typed(value)
        .await
        .map_err(|error| resolver_error(error, value, ref_name))?;
    Ok(OldValue::Exact(object_id.to_string()))
}

/// Map a resolver failure onto this command's error classes.
///
/// A resolver failure caused by the repository itself must keep its own class
/// — reporting it as bad user input would send the operator looking at their
/// command line instead of at their objects.
fn resolver_error(error: CommitBaseError, value: &str, ref_name: &str) -> CliError {
    match error {
        CommitBaseError::HeadUnborn | CommitBaseError::InvalidReference(_) => {
            invalid_target_fatal(format!(
                "cannot update '{ref_name}': '{value}' is not a valid revision in this repository"
            ))
        }
        CommitBaseError::ReadFailure(detail) => {
            CliError::fatal(format!("cannot update '{ref_name}': {detail}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CommitBaseError::CorruptReference(detail) => {
            CliError::fatal(format!("cannot update '{ref_name}': {detail}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}
