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
use git_internal::{errors::GitError, hash::ObjectHash, internal::object::types::ObjectType};
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

    /// Reflog reason recorded with the update (Git's `-m`). With `--stdin`
    /// the reason applies to every ref updated by the batch.
    #[clap(short = 'm', value_name = "REASON")]
    pub message: Option<String>,

    /// With `--stdin`, separate the command's arguments with NUL instead of
    /// whitespace (Git's `-z`). The command word and the ref stay in one
    /// space-separated field — `create SP <ref> NUL <newvalue> NUL` — so a
    /// value may contain spaces without C-style quoting.
    #[clap(short = 'z', long = "null")]
    pub nul_terminated: bool,

    /// Read a batch of `update`/`create`/`delete`/`verify`/`option`/
    /// `start`/`prepare`/`commit`/`abort` commands from stdin (Git's
    /// `update-ref --stdin` transaction protocol).
    #[clap(long)]
    pub stdin: bool,

    /// Accepted for Git parity. Within Libra's `refs/heads/*` scope no ref is
    /// a symbolic ref, so the flag never changes the outcome (documented).
    #[clap(long = "no-deref")]
    pub no_deref: bool,

    /// The ref to update, e.g. `refs/heads/main`.
    #[clap(value_name = "REF", required_unless_present = "stdin")]
    pub ref_name: Option<String>,

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
/// inside a single transaction. All failures exit 128 (matching Git's fatals);
/// out-of-scope refs in the `--stdin` batch keep the invalid-target class
/// (`LBR-CLI-003`/129, ADR-PL-07) and leave every ref untouched.
pub async fn execute_safe(args: UpdateRefArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    if args.stdin {
        return execute_stdin_safe(&args, output).await;
    }

    let fatal = |message: String| {
        CliError::fatal(message)
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    };

    // INVARIANT: clap `required_unless_present = "stdin"` guarantees the
    // positional REF exists on the non-stdin path.
    let ref_name = args
        .ref_name
        .as_deref()
        .expect("REF required without --stdin");

    // Only `refs/heads/<branch>` is representable in v1.
    let branch = parse_heads_ref(ref_name).map_err(fatal)?;
    if !util::is_valid_refname(ref_name) {
        return Err(fatal(format!("invalid ref name '{ref_name}'")));
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
            ref_name
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("switch that worktree to another branch first, or run the command there"));
    }

    let hash_kind = git_internal::hash::get_hash_kind();
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
                ref_name
            )));
        };
        let new_hash = resolve_new_value(&new_value, &zero, ref_name).await?;
        (Some(new_hash.to_string()), args.old_value.clone())
    };

    // Parse the optional compare-and-swap operand.
    let old_spec = match old_spec {
        Some(value) => Some(resolve_old_value(&value, &zero, ref_name).await?),
        None => None,
    };

    let reflog_reason = args.message.clone().unwrap_or_default();
    let full_ref = ref_name.to_string();
    let branch_name = branch.to_string();

    // INVARIANT: the non-delete positional disambiguation above always set
    // `new_oid` to `Some`; `None` only appears on the `-d` path.
    let new_value_for_txn = new_oid.clone();

    let db = get_db_conn_instance().await;
    // A compare-and-swap on a ref: it READS the current value and the branch
    // policy before it writes, so it must take the write lock up front or a
    // concurrent writer makes it fail instead of wait (see
    // `db::begin_write_transaction`).
    let outcome = crate::internal::db::write_transaction(&db, move |txn| {
        Box::pin(async move {
            apply_ref_update(
                txn,
                &branch_name,
                &full_ref,
                new_value_for_txn.as_deref(),
                old_spec.as_ref(),
                &reflog_reason,
            )
            .await
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
                ref_name: ref_name.to_string(),
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

/// One parsed `update-ref --stdin` command (ADR-PL-07 / issues/479 UR-01).
#[derive(Debug)]
enum StdinCommand {
    Update {
        ref_name: String,
        new_value: String,
        old_value: Option<String>,
    },
    Create {
        ref_name: String,
        new_value: String,
    },
    Delete {
        ref_name: String,
        old_value: Option<String>,
    },
    Verify {
        ref_name: String,
        old_value: Option<String>,
    },
    /// `option no-deref` — accepted; within `refs/heads/*` no ref is
    /// symbolic, so it never changes the outcome (documented).
    OptionNoDeref,
    Start,
    Prepare,
    Commit,
    Abort,
}

/// Parse one stdin line. Errors carry the exit class: malformed lines and
/// invalid commands are 128 (git's stdin fatals); out-of-scope refs and
/// `symref-*` commands keep the invalid-target class (129 / `LBR-CLI-003`,
/// ADR-PL-07) and abort the whole batch untouched.
fn parse_stdin_command(line: &str) -> Result<StdinCommand, (String, i32)> {
    let mut tokens = line.split_whitespace();
    let Some(keyword) = tokens.next() else {
        return Err(("empty update-ref stdin command".to_string(), 128));
    };
    match keyword {
        "update" => {
            let ref_name = tokens
                .next()
                .ok_or_else(|| ("update: missing ref".to_string(), 128))?
                .to_string();
            let new_value = tokens
                .next()
                .ok_or_else(|| ("update: missing <newvalue>".to_string(), 128))?
                .to_string();
            let old_value = tokens.next().map(str::to_string);
            if tokens.next().is_some() {
                return Err(("update: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Update {
                ref_name,
                new_value,
                old_value,
            })
        }
        "create" => {
            let ref_name = tokens
                .next()
                .ok_or_else(|| ("create: missing ref".to_string(), 128))?
                .to_string();
            let new_value = tokens
                .next()
                .ok_or_else(|| ("create: missing <newvalue>".to_string(), 128))?
                .to_string();
            if tokens.next().is_some() {
                return Err(("create: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Create {
                ref_name,
                new_value,
            })
        }
        "delete" => {
            let ref_name = tokens
                .next()
                .ok_or_else(|| ("delete: missing ref".to_string(), 128))?
                .to_string();
            let old_value = tokens.next().map(str::to_string);
            if tokens.next().is_some() {
                return Err(("delete: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Delete {
                ref_name,
                old_value,
            })
        }
        "verify" => {
            let ref_name = tokens
                .next()
                .ok_or_else(|| ("verify: missing ref".to_string(), 128))?
                .to_string();
            let old_value = tokens.next().map(str::to_string);
            if tokens.next().is_some() {
                return Err(("verify: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Verify {
                ref_name,
                old_value,
            })
        }
        "option" => match tokens.next() {
            Some("no-deref") => {
                if tokens.next().is_some() {
                    return Err(("option no-deref: too many arguments".to_string(), 128));
                }
                Ok(StdinCommand::OptionNoDeref)
            }
            Some(other) => Err((format!("unknown option '{other}'"), 128)),
            None => Err(("option: missing value".to_string(), 128)),
        },
        "start" => {
            if tokens.next().is_some() {
                return Err(("start: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Start)
        }
        "prepare" => {
            if tokens.next().is_some() {
                return Err(("prepare: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Prepare)
        }
        "commit" => {
            if tokens.next().is_some() {
                return Err(("commit: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Commit)
        }
        "abort" => {
            if tokens.next().is_some() {
                return Err(("abort: too many arguments".to_string(), 128));
            }
            Ok(StdinCommand::Abort)
        }
        // DEFER-04: symbolic-ref update commands are out of scope.
        "symref-update" | "symref-create" | "symref-delete" | "symref-verify" => Err((
            format!("{keyword}: symbolic refs are not supported by update-ref"),
            129,
        )),
        other => Err((format!("invalid update-ref stdin command '{other}'"), 128)),
    }
}

/// Parse `update-ref --stdin -z` input (Git's wire format, ADR-PL-07 /
/// issues/479 UR-01).
///
/// Unlike `ls-files -z` and `status --porcelain -z` — where NUL terminates a
/// RECORD — `update-ref --stdin -z` uses NUL as the separator between a
/// command's ARGUMENTS. The command word and the ref stay in one
/// space-separated field, so a value may contain spaces (for example the
/// revision syntax `:/fix bug`) without C-style quoting:
///
/// ```text
/// create SP <ref> NUL <newvalue> NUL
/// update SP <ref> NUL <newvalue> NUL <oldvalue> NUL
/// delete SP <ref> NUL [<oldvalue>] NUL
/// verify SP <ref> NUL [<oldvalue>] NUL
/// option no-deref NUL | start NUL | prepare NUL | commit NUL | abort NUL
/// ```
///
/// Errors carry the exit class: malformed layouts and end-of-input are 128
/// (Git's stdin fatals); `symref-*` stays the invalid-target class (129).
fn parse_stdin_z(buf: &[u8]) -> Vec<Result<StdinCommand, (String, i32)>> {
    let text = String::from_utf8_lossy(buf);
    // Keep EMPTY fields: Git distinguishes an absent field (end of input, which
    // is fatal) from an empty one, which means "no value" — `delete <ref> NUL
    // NUL` is an unconditional delete, and `verify <ref> NUL NUL` asserts the
    // ref does NOT exist (an empty value is the all-zero id). Only the empty
    // fragment after the final NUL is an artifact of the terminator.
    let mut fields: Vec<&str> = text.split('\0').collect();
    if fields.last() == Some(&"") {
        fields.pop();
    }
    let mut parsed: Vec<Result<StdinCommand, (String, i32)>> = Vec::new();
    let mut index = 0usize;
    while index < fields.len() {
        let head = fields[index];
        index += 1;
        if head.is_empty() {
            parsed.push(Err(("empty update-ref stdin command".to_string(), 128)));
            break;
        }
        let command = parse_stdin_z_command(head, &fields, &mut index);
        let failed = command.is_err();
        parsed.push(command);
        // A malformed command leaves the field cursor meaningless; Git stops at
        // the first one, and the caller reports it after echoing the earlier
        // acknowledgements.
        if failed {
            break;
        }
    }
    parsed
}

/// Parse one `-z` command whose head field is `<keyword>[ SP<ref>]`.
fn parse_stdin_z_command(
    head: &str,
    fields: &[&str],
    index: &mut usize,
) -> Result<StdinCommand, (String, i32)> {
    let (keyword, rest) = match head.split_once(' ') {
        Some((keyword, rest)) => (keyword, Some(rest)),
        None => (head, None),
    };
    match keyword {
        "create" => Ok(StdinCommand::Create {
            ref_name: z_ref_field(keyword, rest)?,
            new_value: z_required_value(fields, index, keyword, "<newvalue>")?,
        }),
        "update" => Ok(StdinCommand::Update {
            ref_name: z_ref_field(keyword, rest)?,
            new_value: z_required_value(fields, index, keyword, "<newvalue>")?,
            old_value: z_optional_value(fields, index, keyword, "<oldvalue>")?,
        }),
        "delete" => Ok(StdinCommand::Delete {
            ref_name: z_ref_field(keyword, rest)?,
            old_value: z_optional_value(fields, index, keyword, "<oldvalue>")?,
        }),
        "verify" => Ok(StdinCommand::Verify {
            ref_name: z_ref_field(keyword, rest)?,
            old_value: z_optional_value(fields, index, keyword, "<oldvalue>")?,
        }),
        "option" => match rest {
            Some("no-deref") => Ok(StdinCommand::OptionNoDeref),
            Some(other) => Err((format!("unknown option '{other}'"), 128)),
            None => Err(("option: missing value".to_string(), 128)),
        },
        "start" | "prepare" | "commit" | "abort" => {
            if rest.is_some() {
                return Err((format!("{keyword}: too many arguments"), 128));
            }
            Ok(match keyword {
                "start" => StdinCommand::Start,
                "prepare" => StdinCommand::Prepare,
                "commit" => StdinCommand::Commit,
                _ => StdinCommand::Abort,
            })
        }
        // DEFER-04: symbolic-ref update commands are out of scope.
        "symref-update" | "symref-create" | "symref-delete" | "symref-verify" => Err((
            format!("{keyword}: symbolic refs are not supported by update-ref"),
            129,
        )),
        other => Err((format!("invalid update-ref stdin command '{other}'"), 128)),
    }
}

/// The `<cmd> SP <ref>` head field of a `-z` record. A space inside the ref
/// means the caller used a non-Git layout (Git keeps the ref in the head field
/// and rejects it as `invalid ref format`).
fn z_ref_field(keyword: &str, rest: Option<&str>) -> Result<String, (String, i32)> {
    let Some(ref_name) = rest else {
        return Err((format!("{keyword}: missing ref"), 128));
    };
    if ref_name.is_empty() {
        return Err((format!("{keyword}: missing ref"), 128));
    }
    if ref_name.contains(' ') || ref_name.contains('\t') {
        return Err((format!("invalid ref format: {ref_name}"), 128));
    }
    Ok(ref_name.to_string())
}

/// A required `-z` value field: it must be PRESENT (an absent field is
/// end-of-input, which Git treats as fatal) and non-empty.
fn z_required_value(
    fields: &[&str],
    index: &mut usize,
    keyword: &str,
    what: &str,
) -> Result<String, (String, i32)> {
    let value = z_field(fields, index, keyword, what)?;
    if value.is_empty() {
        return Err((format!("{keyword}: missing {what}"), 128));
    }
    Ok(value.to_string())
}

/// An optional `-z` value field: it must be PRESENT, and an empty field means
/// "no value" (Git's absent-vs-empty distinction).
fn z_optional_value(
    fields: &[&str],
    index: &mut usize,
    keyword: &str,
    what: &str,
) -> Result<Option<String>, (String, i32)> {
    let value = z_field(fields, index, keyword, what)?;
    Ok((!value.is_empty()).then(|| value.to_string()))
}

fn z_field<'a>(
    fields: &[&'a str],
    index: &mut usize,
    keyword: &str,
    what: &str,
) -> Result<&'a str, (String, i32)> {
    let Some(field) = fields.get(*index) else {
        return Err((
            format!("{keyword}: unexpected end of input when reading {what}"),
            128,
        ));
    };
    *index += 1;
    Ok(field)
}

/// A fully parsed and value-resolved batch command, ready for the transaction.
enum PreparedKind {
    Update { new: String, old: Option<OldValue> },
    Create { new: String },
    Delete { old: Option<OldValue> },
    Verify { old: Option<OldValue> },
}

struct PreparedCommand {
    branch: String,
    full_ref: String,
    kind: PreparedKind,
}

/// The `--stdin` batch path: stream-parse the commands, resolve values, then
/// apply every update in ONE SQLite write transaction. `start`/`prepare`/
/// `commit`/`abort` drive git's transaction protocol (`<cmd>: ok` lines are
/// printed as the commands are processed, exactly like git); an `abort`
/// applies nothing. Any failure leaves refs + reflogs byte-identical
/// (GC-PL-02).
async fn execute_stdin_safe(args: &UpdateRefArgs, output: &OutputConfig) -> CliResult<()> {
    use std::io::{BufRead, Read, Write};

    let stdin_fatal = |message: String, code: i32| {
        CliError::fatal(message)
            .with_exit_code(code)
            .with_stable_code(if code == 129 {
                StableErrorCode::CliInvalidTarget
            } else {
                StableErrorCode::CliInvalidArguments
            })
    };

    // Parse the input. `-z` separates the command's ARGUMENTS with NUL (git's
    // format, ADR-PL-07 / issues/479 UR-01); the LF form splits on whitespace.
    //
    // Parsing is done in input order and a malformed command is kept as an
    // `Err` item instead of aborting here: Git acknowledges each command as it
    // reads it (`start: ok`, `prepare: ok`, ...), so those lines must already be
    // on stdout when a LATER command turns out to be malformed.
    let stdin = std::io::stdin();
    let mut records: Vec<Result<StdinCommand, (String, i32)>> = Vec::new();
    if args.nul_terminated {
        let mut buf = Vec::new();
        stdin.lock().read_to_end(&mut buf).map_err(|error| {
            stdin_fatal(format!("failed to read update-ref stdin: {error}"), 128)
        })?;
        records = parse_stdin_z(&buf);
    } else {
        for line in stdin.lock().lines() {
            let line = line.map_err(|error| {
                stdin_fatal(format!("failed to read update-ref stdin: {error}"), 128)
            })?;
            if line.trim().is_empty() {
                continue;
            }
            records.push(parse_stdin_command(&line).map_err(|(message, code)| {
                (
                    format!("{message} (input: {})", truncate_input(&line)),
                    code,
                )
            }));
        }
    }

    // First pass: state machine + scope/uniqueness validation. git prints the
    // `<cmd>: ok` lines as the commands are processed (they stay on stdout
    // even when a later command fails), so they are written immediately.
    let mut seen_refs: std::collections::HashMap<String, ()> = std::collections::HashMap::new();
    let mut active = false;
    let mut closed = false;
    let mut aborted = false;
    let mut stdout = std::io::stdout().lock();

    for parsed in &records {
        let command = match parsed {
            Ok(command) => command,
            Err((message, code)) => return Err(stdin_fatal(message.clone(), *code)),
        };
        match command {
            StdinCommand::Start => {
                if active {
                    return Err(stdin_fatal(
                        "cannot restart ongoing transaction".to_string(),
                        128,
                    ));
                }
                active = true;
                let _ = writeln!(stdout, "start: ok");
            }
            StdinCommand::Prepare => {
                let _ = writeln!(stdout, "prepare: ok");
            }
            StdinCommand::Commit => {
                closed = true;
                let _ = writeln!(stdout, "commit: ok");
            }
            StdinCommand::Abort => {
                closed = true;
                aborted = true;
                let _ = writeln!(stdout, "abort: ok");
            }
            StdinCommand::OptionNoDeref => {}
            StdinCommand::Update { ref_name, .. }
            | StdinCommand::Create { ref_name, .. }
            | StdinCommand::Delete { ref_name, .. }
            | StdinCommand::Verify { ref_name, .. } => {
                if closed {
                    return Err(stdin_fatal("transaction is closed".to_string(), 128));
                }
                let branch = parse_heads_ref(ref_name)
                    .map_err(|message| stdin_fatal(message, 129))?
                    .to_string();
                if !util::is_valid_refname(ref_name) {
                    return Err(stdin_fatal(format!("invalid ref name '{ref_name}'"), 129));
                }
                if seen_refs.insert(ref_name.to_string(), ()).is_some() {
                    return Err(stdin_fatal(
                        format!("multiple updates for ref '{ref_name}'"),
                        128,
                    ));
                }
                let _ = branch;
            }
        }
    }

    if aborted {
        // git prints the `<cmd>: ok` lines and applies nothing.
        if output.is_json() {
            emit_json_data(
                "update-ref",
                &serde_json::json!({ "refs": [], "committed": false }),
                output,
            )?;
        }
        return Ok(());
    }

    // Second pass: resolve the values (async) and build the prepared list.
    let zero = ObjectHash::zero_str(git_internal::hash::get_hash_kind());
    let mut prepared: Vec<PreparedCommand> = Vec::new();
    for command in records.iter().filter_map(|parsed| parsed.as_ref().ok()) {
        let resolved = match command {
            StdinCommand::Update {
                ref_name,
                new_value,
                old_value,
            } => {
                let branch = parse_heads_ref(ref_name)
                    .map_err(|message| stdin_fatal(message, 129))?
                    .to_string();
                let new = resolve_new_value(new_value, &zero, ref_name).await?;
                let old = match old_value {
                    Some(value) => Some(resolve_old_value(value, &zero, ref_name).await?),
                    None => None,
                };
                PreparedCommand {
                    branch,
                    full_ref: ref_name.to_string(),
                    kind: PreparedKind::Update {
                        new: new.to_string(),
                        old,
                    },
                }
            }
            StdinCommand::Create {
                ref_name,
                new_value,
            } => {
                let branch = parse_heads_ref(ref_name)
                    .map_err(|message| stdin_fatal(message, 129))?
                    .to_string();
                let new = resolve_new_value(new_value, &zero, ref_name).await?;
                PreparedCommand {
                    branch,
                    full_ref: ref_name.to_string(),
                    kind: PreparedKind::Create {
                        new: new.to_string(),
                    },
                }
            }
            StdinCommand::Delete {
                ref_name,
                old_value,
            } => {
                let branch = parse_heads_ref(ref_name)
                    .map_err(|message| stdin_fatal(message, 129))?
                    .to_string();
                let old = match old_value {
                    Some(value) => Some(resolve_old_value(value, &zero, ref_name).await?),
                    None => None,
                };
                PreparedCommand {
                    branch,
                    full_ref: ref_name.to_string(),
                    kind: PreparedKind::Delete { old },
                }
            }
            StdinCommand::Verify {
                ref_name,
                old_value,
            } => {
                let branch = parse_heads_ref(ref_name)
                    .map_err(|message| stdin_fatal(message, 129))?
                    .to_string();
                // Git: `verify` with no <oldvalue> (LF) or an empty one (`-z`)
                // carries the all-zero id, so the ref must NOT exist — it is not
                // "any value". An absent value therefore differs from `update`
                // and `delete`, where it means "no CAS".
                let old = match old_value.as_deref().filter(|value| !value.is_empty()) {
                    Some(value) => Some(resolve_old_value(value, &zero, ref_name).await?),
                    None => Some(OldValue::MustNotExist),
                };
                PreparedCommand {
                    branch,
                    full_ref: ref_name.to_string(),
                    kind: PreparedKind::Verify { old },
                }
            }
            _ => continue,
        };
        prepared.push(resolved);
    }

    // Part C W0 (§C.11) per unique branch, fail closed.
    let mut checked: std::collections::HashMap<String, ()> = std::collections::HashMap::new();
    for command in &prepared {
        if checked.contains_key(&command.branch) {
            continue;
        }
        checked.insert(command.branch.clone(), ());
        let elsewhere =
            crate::internal::head::Head::branch_checked_out_elsewhere_result(&command.branch)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "cannot verify whether '{}' is checked out in another worktree: {error}",
                        command.branch
                    ))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                })?;
        if let Some(other) = elsewhere {
            return Err(CliError::fatal(format!(
                "cannot update '{}': branch '{}' is checked out at worktree '{other}'",
                command.full_ref, command.branch
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::ConflictOperationBlocked));
        }
    }

    let message = args.message.clone().unwrap_or_default();
    let db = get_db_conn_instance().await;
    let ref_names: Vec<String> = prepared.iter().map(|c| c.full_ref.clone()).collect();
    crate::internal::db::write_transaction(&db, move |txn| {
        Box::pin(async move {
            for command in &prepared {
                match &command.kind {
                    PreparedKind::Update { new, old } => {
                        apply_ref_update(
                            txn,
                            &command.branch,
                            &command.full_ref,
                            Some(new),
                            old.as_ref(),
                            &message,
                        )
                        .await?;
                    }
                    PreparedKind::Create { new } => {
                        let must_not_exist = OldValue::MustNotExist;
                        apply_ref_update(
                            txn,
                            &command.branch,
                            &command.full_ref,
                            Some(new),
                            Some(&must_not_exist),
                            &message,
                        )
                        .await?;
                    }
                    PreparedKind::Delete { old } => {
                        apply_ref_update(
                            txn,
                            &command.branch,
                            &command.full_ref,
                            None,
                            old.as_ref(),
                            &message,
                        )
                        .await?;
                    }
                    PreparedKind::Verify { old } => {
                        verify_ref_value(txn, &command.branch, &command.full_ref, old.as_ref())
                            .await?;
                    }
                }
            }
            Ok::<_, UpdateRefTxError>(())
        })
    })
    .await
    .map_err(|error| {
        let message = match error {
            TransactionError::Connection(error) => error.to_string(),
            TransactionError::Transaction(error) => error.to_string(),
        };
        CliError::fatal(message)
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;

    if output.is_json() {
        emit_json_data(
            "update-ref",
            &serde_json::json!({ "refs": ref_names, "committed": true }),
            output,
        )?;
    }
    Ok(())
}

/// Truncate an input line for error messages (GC-PL-02: never echo long lines).
fn truncate_input(line: &str) -> String {
    const CAP: usize = 200;
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(CAP).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

struct UpdateRefOutcome {
    old: Option<String>,
    new: Option<String>,
}

/// Apply one ref update inside an open write transaction: branch-policy gate,
/// CAS precondition, delete/update write, and the reflog entry. Shared by the
/// positional path and every `--stdin` batch command (ADR-PL-07 / issues/479
/// UR-01) so the two surfaces can never diverge.
async fn apply_ref_update<C: sea_orm::ConnectionTrait>(
    txn: &C,
    branch_name: &str,
    full_ref: &str,
    new_value: Option<&str>,
    old_spec: Option<&OldValue>,
    reflog_reason: &str,
) -> Result<UpdateRefOutcome, UpdateRefTxError> {
    // Branch policy (lore.md 1.13): protect/archive metadata is enforced
    // INSIDE the authoritative txn for every local-head writer — update-ref
    // would otherwise be a silent bypass of `branch reset`'s policy layer.
    // Fail-closed: metadata read errors refuse the update. (update-ref stays
    // plumbing-sharp otherwise — it may still move the checked-out branch,
    // like git update-ref.)
    let protected = crate::internal::metadata::MetadataKv::is_protected_with_conn(txn, branch_name)
        .await
        .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
    if protected {
        return Err(UpdateRefTxError::PolicyBlocked {
            branch: branch_name.to_string(),
            policy: "protected".to_string(),
        });
    }
    let archived = crate::internal::metadata::MetadataKv::is_archived_with_conn(txn, branch_name)
        .await
        .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
    if archived {
        return Err(UpdateRefTxError::PolicyBlocked {
            branch: branch_name.to_string(),
            policy: "archived".to_string(),
        });
    }
    let current = Branch::find_branch_result_with_conn(txn, branch_name, None)
        .await
        .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?
        .map(|b| b.commit.to_string());

    // Compare-and-swap precondition.
    if let Some(expected) = old_spec {
        match (expected, &current) {
            // `0{40}`/`0{64}` => the ref must not exist.
            (OldValue::MustNotExist, Some(actual)) => {
                return Err(UpdateRefTxError::MustNotExist {
                    ref_name: full_ref.to_string(),
                    actual: actual.clone(),
                });
            }
            (OldValue::MustNotExist, None) => {}
            (OldValue::Exact(want), actual) if actual.as_deref() != Some(want.as_str()) => {
                return Err(UpdateRefTxError::CasMismatch {
                    ref_name: full_ref.to_string(),
                    expected: want.clone(),
                    actual: actual.clone().unwrap_or_else(|| {
                        ObjectHash::zero_str(git_internal::hash::get_hash_kind())
                    }),
                });
            }
            (OldValue::Exact(_), _) => {}
        }
    }

    if let Some(new) = new_value {
        Branch::update_branch_with_conn(txn, branch_name, new, None)
            .await
            .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
        let zero = ObjectHash::zero_str(git_internal::hash::get_hash_kind());
        let old = current.clone().unwrap_or_else(|| zero.clone());
        write_reflog(txn, full_ref, &old, new, reflog_reason).await?;
        Ok::<_, UpdateRefTxError>(UpdateRefOutcome {
            old: current,
            new: Some(new.to_string()),
        })
    } else {
        let Some(old) = current.clone() else {
            return Err(UpdateRefTxError::DoesNotExist {
                ref_name: full_ref.to_string(),
            });
        };
        Branch::delete_branch_result_with_conn(txn, branch_name, None)
            .await
            .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?;
        let zero = ObjectHash::zero_str(git_internal::hash::get_hash_kind());
        write_reflog(txn, full_ref, &old, &zero, reflog_reason).await?;
        Ok(UpdateRefOutcome {
            old: Some(old),
            new: None,
        })
    }
}

/// Verify a ref's current value inside an open write transaction (stdin
/// `verify` command; git compares and fails without writing anything).
/// `None` means "the ref must exist" (git's `verify <ref>` form).
async fn verify_ref_value<C: sea_orm::ConnectionTrait>(
    txn: &C,
    branch_name: &str,
    full_ref: &str,
    expected: Option<&OldValue>,
) -> Result<(), UpdateRefTxError> {
    let current = Branch::find_branch_result_with_conn(txn, branch_name, None)
        .await
        .map_err(|error| UpdateRefTxError::Storage(error.to_string()))?
        .map(|b| b.commit.to_string());
    let Some(expected) = expected else {
        if current.is_none() {
            return Err(UpdateRefTxError::DoesNotExist {
                ref_name: full_ref.to_string(),
            });
        }
        return Ok(());
    };
    let zero = ObjectHash::zero_str(git_internal::hash::get_hash_kind());
    match (expected, &current) {
        (OldValue::MustNotExist, Some(actual)) => Err(UpdateRefTxError::MustNotExist {
            ref_name: full_ref.to_string(),
            actual: actual.clone(),
        }),
        (OldValue::MustNotExist, None) => Ok(()),
        (OldValue::Exact(want), actual) if actual.as_deref() != Some(want.as_str()) => {
            Err(UpdateRefTxError::CasMismatch {
                ref_name: full_ref.to_string(),
                expected: want.clone(),
                actual: actual.clone().unwrap_or(zero),
            })
        }
        (OldValue::Exact(_), _) => Ok(()),
    }
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
    let object_type = util::objects_storage()?
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
        CommitBaseError::Ambiguous { name, candidates } => {
            util::ambiguous_short_oid_error(&name, &candidates)
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

#[cfg(test)]
mod stdin_parse_tests {
    use super::{StdinCommand, parse_stdin_command};

    fn parse(line: &str) -> StdinCommand {
        parse_stdin_command(line).unwrap_or_else(|(message, code)| {
            panic!("line {line:?} must parse: {message} (exit {code})")
        })
    }

    #[test]
    fn stdin_line_forms_parse() {
        match parse(
            "update refs/heads/a 1111111111111111111111111111111111111111 2222222222222222222222222222222222222222",
        ) {
            StdinCommand::Update {
                ref_name,
                new_value,
                old_value,
            } => {
                assert_eq!(ref_name, "refs/heads/a");
                assert_eq!(new_value, "1111111111111111111111111111111111111111");
                assert_eq!(
                    old_value.as_deref(),
                    Some("2222222222222222222222222222222222222222")
                );
            }
            other => panic!("update form: {other:?}"),
        }
        match parse("create refs/heads/b 1111111111111111111111111111111111111111") {
            StdinCommand::Create {
                ref_name,
                new_value,
            } => {
                assert_eq!(ref_name, "refs/heads/b");
                assert_eq!(new_value, "1111111111111111111111111111111111111111");
            }
            other => panic!("create form: {other:?}"),
        }
        match parse("delete refs/heads/c") {
            StdinCommand::Delete {
                ref_name,
                old_value,
            } => {
                assert_eq!(ref_name, "refs/heads/c");
                assert!(old_value.is_none());
            }
            other => panic!("delete form: {other:?}"),
        }
        match parse("verify refs/heads/d 1111111111111111111111111111111111111111") {
            StdinCommand::Verify {
                ref_name,
                old_value,
            } => {
                assert_eq!(ref_name, "refs/heads/d");
                assert_eq!(
                    old_value.as_deref(),
                    Some("1111111111111111111111111111111111111111")
                );
            }
            other => panic!("verify form: {other:?}"),
        }
        assert!(matches!(
            parse("option no-deref"),
            StdinCommand::OptionNoDeref
        ));
        assert!(matches!(parse("start"), StdinCommand::Start));
        assert!(matches!(parse("prepare"), StdinCommand::Prepare));
        assert!(matches!(parse("commit"), StdinCommand::Commit));
        assert!(matches!(parse("abort"), StdinCommand::Abort));
    }

    #[test]
    fn stdin_line_errors_classify() {
        // Malformed lines are 128.
        assert_eq!(parse_stdin_command("").unwrap_err().1, 128);
        assert_eq!(
            parse_stdin_command("frobnicate refs/heads/a")
                .unwrap_err()
                .1,
            128
        );
        assert_eq!(parse_stdin_command("update").unwrap_err().1, 128);
        assert_eq!(
            parse_stdin_command("update refs/heads/a").unwrap_err().1,
            128
        );
        assert_eq!(
            parse_stdin_command("update refs/heads/a 1 2 3")
                .unwrap_err()
                .1,
            128
        );
        assert_eq!(parse_stdin_command("option deref").unwrap_err().1, 128);
        // symref-* commands keep the invalid-target class (129).
        assert_eq!(
            parse_stdin_command("symref-update HEAD refs/heads/main")
                .unwrap_err()
                .1,
            129
        );
    }
}
