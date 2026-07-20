//! Apply plain-text `format-patch` mail messages as commits.
//!
//! This is the deliberately small P2 mail-flow surface: one or more patch
//! files plus `--continue`, `--skip`, and `--abort`. The parser accepts the
//! common 8-bit, quoted-printable, and base64 single-part mail forms. MIME
//! multipart messages and the wider Git `am` option set remain out of scope.

use std::{collections::HashSet, fs, str::FromStr};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::{index::Index, object::commit::Commit},
};
use serde::{Deserialize, Serialize};

use crate::{
    command::{
        add::{AddArgs, run_add},
        apply::{
            MAX_PATCH_BYTES, PatchPreparationError, patch_targets, prepare_patch,
            validate_patch_target,
        },
        commit::create_commit_signatures,
        mailinfo::{invalid_mail, parse_mail, validate_author},
        save_object, status,
    },
    common_utils::format_commit_msg,
    internal::{
        branch::Branch,
        head::Head,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        sequencer::{self, AmSequenceState},
        tree_plumbing,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path,
        text::short_display_hash,
        util,
    },
};

const MAX_MAILS: usize = 10_000;

pub const AM_EXAMPLES: &str = "\
EXAMPLES:
    libra am 0001-fix.patch              Apply one format-patch mail
    libra am 0001.patch 0002.patch       Apply a mail series in order
    libra am --continue                  Commit a staged conflict resolution
    libra am --skip                      Skip the current mail
    libra am --abort                     Restore the pre-am branch tip";

#[derive(Parser, Debug)]
#[command(after_help = AM_EXAMPLES)]
pub struct AmArgs {
    /// Plain-text format-patch mail files, applied in order.
    #[clap(
        value_name = "PATCH",
        required_unless_present_any = ["continue_am", "skip", "abort"]
    )]
    pub patches: Vec<String>,

    /// Commit the staged resolution and resume the mail series.
    #[clap(
        long = "continue",
        conflicts_with_all = ["patches", "skip", "abort"]
    )]
    pub continue_am: bool,

    /// Discard the current mail's changes and resume with the next mail.
    #[clap(
        long,
        conflicts_with_all = ["patches", "continue_am", "abort"]
    )]
    pub skip: bool,

    /// Restore the original branch tip, index, and tracked worktree.
    #[clap(
        long,
        conflicts_with_all = ["patches", "continue_am", "skip"]
    )]
    pub abort: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MailPatch {
    source: String,
    author: String,
    author_date: String,
    message: String,
    patch: String,
    targets: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AmPayload {
    patches: Vec<MailPatch>,
    current: usize,
}

#[derive(Clone, Debug)]
struct AmState {
    head_name: String,
    head_orig: ObjectHash,
    expected_head: ObjectHash,
    payload: AmPayload,
}

impl AmState {
    fn to_sequence(&self) -> CliResult<AmSequenceState> {
        let payload = serde_json::to_string(&self.payload)
            .map_err(|error| am_state_error(format!("failed to serialize am state: {error}")))?;
        Ok(AmSequenceState {
            head_name: self.head_name.clone(),
            head_orig: self.head_orig.to_string(),
            current_oid: self.expected_head.to_string(),
            todo: self.payload.patches[self.payload.current.saturating_add(1)..]
                .iter()
                .map(|mail| mail.source.clone())
                .collect(),
            payload,
        })
    }

    fn from_sequence(sequence: AmSequenceState) -> CliResult<Self> {
        let head_orig = ObjectHash::from_str(sequence.head_orig.trim()).map_err(|error| {
            am_state_error(format!("saved am original HEAD is invalid: {error}"))
        })?;
        let expected_head = ObjectHash::from_str(sequence.current_oid.trim()).map_err(|error| {
            am_state_error(format!("saved am expected HEAD is invalid: {error}"))
        })?;
        let payload: AmPayload = serde_json::from_str(&sequence.payload)
            .map_err(|error| am_state_error(format!("saved am state is invalid: {error}")))?;
        if payload.patches.is_empty()
            || payload.patches.len() > MAX_MAILS
            || payload.current >= payload.patches.len()
        {
            return Err(am_state_error(
                "saved am state has an invalid patch position".to_string(),
            ));
        }
        let mut total = 0usize;
        for mail in &payload.patches {
            total = total
                .checked_add(mail.patch.len())
                .ok_or_else(|| am_state_error("saved am patch size overflow".to_string()))?;
            if total > MAX_PATCH_BYTES
                || validate_author(&mail.author).is_err()
                || mail.message.is_empty()
                || mail.message.contains('\0')
            {
                return Err(am_state_error(
                    "saved am state contains invalid mail metadata".to_string(),
                ));
            }
            let targets = patch_targets(&mail.patch, 1)
                .map_err(|error| am_state_error(format!("saved am patch is invalid: {error}")))?;
            if targets.is_empty() || targets != mail.targets {
                return Err(am_state_error(
                    "saved am patch target list does not match its patch".to_string(),
                ));
            }
        }
        Ok(Self {
            head_name: sequence.head_name,
            head_orig,
            expected_head,
            payload,
        })
    }

    async fn save(&self) -> CliResult<()> {
        let sequence = self.to_sequence()?;
        sequencer::save_am(&sequence)
            .await
            .map_err(|error| am_state_error(format!("failed to save am state: {error}")))
    }

    async fn load() -> CliResult<Option<Self>> {
        match sequencer::load_am()
            .await
            .map_err(|error| am_state_error(format!("failed to load am state: {error}")))?
        {
            Some(sequence) => Self::from_sequence(sequence).map(Some),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Serialize)]
struct AppliedMail {
    source: String,
    subject: String,
    commit: String,
}

#[derive(Debug, Serialize)]
struct AmOutput {
    action: String,
    applied: Vec<AppliedMail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored_head: Option<String>,
}

pub async fn execute_safe(args: AmArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    // Part C W1 (§C.4.2): `am` is safe in a LINKED worktree — its entire
    // persistent state is the worktree-scoped `sequence_state` row (kind `am`,
    // the patch queue serialized into its `payload`; no common-storage sidecar),
    // the start-time mutex is scope-aware, and it applies patches to THIS
    // worktree's own index/worktree and advances its own branch. So the
    // `ensure_main_worktree` guard is lifted here, matching `cherry-pick`.

    let result = if args.continue_am {
        continue_am(output).await?
    } else if args.skip {
        skip_am(output).await?
    } else if args.abort {
        abort_am(output).await?
    } else {
        sequencer::ensure_none_for_am().await?;
        if AmState::load().await?.is_some() {
            return Err(CliError::conflict("an am operation is already in progress")
                .with_hint("use 'libra am --continue', '--skip', or '--abort'"));
        }
        start_am(&args.patches, output).await?
    };

    render_output(&result, output)
}

async fn start_am(paths: &[String], output: &OutputConfig) -> CliResult<AmOutput> {
    let patches = read_mail_patches(paths)?;
    ensure_clean_start(&patches).await?;
    let head_name = current_branch_name().await?;
    let head_orig = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| {
            CliError::fatal("am requires an existing HEAD commit")
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("create an initial commit before applying a mail series")
        })?;
    let mut state = AmState {
        head_name,
        head_orig,
        expected_head: head_orig,
        payload: AmPayload {
            patches,
            current: 0,
        },
    };
    state.save().await?;
    if test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_STATE") {
        return Err(am_state_error(
            "test-injected am interruption after saving initial state".to_string(),
        )
        .with_hint("run 'libra am --continue' to resume the mail series")
        .with_hint("or run 'libra am --abort' to restore the original branch"));
    }
    let applied = apply_remaining(&mut state, output).await?;
    Ok(AmOutput {
        action: "apply".to_string(),
        applied,
        restored_head: None,
    })
}

async fn continue_am(output: &OutputConfig) -> CliResult<AmOutput> {
    let mut state = load_state_or_error().await?;
    ensure_state_branch(&state).await?;
    ensure_expected_head(&state).await?;
    let mail = state.payload.patches[state.payload.current].clone();
    if recovery_state_is_pristine(&mail.targets).await? {
        let applied = apply_remaining(&mut state, output).await?;
        return Ok(AmOutput {
            action: "continue".to_string(),
            applied,
            restored_head: None,
        });
    }
    ensure_resolution_staged(&mail.targets).await?;

    let first = commit_current(&mut state, mail).await?;
    let mut applied = vec![first];
    applied.extend(apply_remaining(&mut state, output).await?);
    Ok(AmOutput {
        action: "continue".to_string(),
        applied,
        restored_head: None,
    })
}

async fn skip_am(output: &OutputConfig) -> CliResult<AmOutput> {
    let mut state = load_state_or_error().await?;
    ensure_state_branch(&state).await?;
    ensure_expected_head(&state).await?;
    let current_targets = state.payload.patches[state.payload.current].targets.clone();
    reset_hard("HEAD", output).await?;
    cleanup_untracked_patch_targets(&current_targets)?;

    state.payload.current += 1;
    if state.payload.current == state.payload.patches.len() {
        sequencer::clear_am()
            .await
            .map_err(|error| am_state_error(format!("failed to clear am state: {error}")))?;
        return Ok(AmOutput {
            action: "skip".to_string(),
            applied: Vec::new(),
            restored_head: None,
        });
    }
    state.save().await?;
    let applied = apply_remaining(&mut state, output).await?;
    Ok(AmOutput {
        action: "skip".to_string(),
        applied,
        restored_head: None,
    })
}

async fn abort_am(output: &OutputConfig) -> CliResult<AmOutput> {
    let state = load_state_or_error().await?;
    ensure_state_branch(&state).await?;
    let current_targets = state.payload.patches[state.payload.current].targets.clone();
    let restored = state.head_orig.to_string();
    reset_hard(&restored, output).await?;
    cleanup_untracked_patch_targets(&current_targets)?;
    sequencer::clear_am()
        .await
        .map_err(|error| am_state_error(format!("failed to clear am state: {error}")))?;
    Ok(AmOutput {
        action: "abort".to_string(),
        applied: Vec::new(),
        restored_head: Some(restored),
    })
}

async fn apply_remaining(
    state: &mut AmState,
    _output: &OutputConfig,
) -> CliResult<Vec<AppliedMail>> {
    let mut applied = Vec::new();
    while state.payload.current < state.payload.patches.len() {
        ensure_expected_head(state).await?;
        let mail = state.payload.patches[state.payload.current].clone();
        let prepared = match prepare_patch(&mail.patch, 1, &util::working_dir()) {
            Ok(prepared) => prepared,
            Err(PatchPreparationError::DoesNotApply(detail)) => {
                return Err(am_conflict(&mail, detail));
            }
            Err(PatchPreparationError::Invalid(detail)) => {
                return Err(
                    am_state_error(format!("cannot apply '{}': {detail}", mail.source))
                        .with_hint("run 'libra am --abort' to restore the original branch"),
                );
            }
        };
        prepared.write().map_err(|detail| {
            am_state_error(format!("cannot apply '{}': {detail}", mail.source))
                .with_hint("fix and stage the affected paths, then run 'libra am --continue'")
                .with_hint("or run 'libra am --abort' to restore the original branch")
        })?;
        if test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_WRITE") {
            return Err(am_state_error(
                "test-injected am interruption after worktree write".to_string(),
            )
            .with_hint("run 'libra am --abort' to restore the original branch"));
        }
        stage_targets(&mail.targets).await?;
        ensure_resolution_staged(&mail.targets).await?;
        applied.push(commit_current(state, mail).await?);
        if state.payload.current < state.payload.patches.len()
            && test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_COMMIT")
        {
            return Err(am_state_error(
                "test-injected am interruption between commits".to_string(),
            )
            .with_hint("run 'libra am --continue' to resume the mail series")
            .with_hint("or run 'libra am --abort' to restore the original branch"));
        }
    }
    Ok(applied)
}

async fn commit_current(state: &mut AmState, mail: MailPatch) -> CliResult<AppliedMail> {
    let index = Index::load(path::index())
        .map_err(|error| am_state_error(format!("failed to load the index for am: {error}")))?;
    let tree_id = tree_plumbing::write_tree_from_index(&index)
        .map_err(|error| am_state_error(format!("failed to create am commit tree: {error}")))?;
    let parent = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| am_state_error("HEAD disappeared during am".to_string()))?;
    if parent != state.expected_head {
        return Err(am_head_moved(state, parent));
    }
    let (author, committer, _) =
        create_commit_signatures(Some(mail.author.as_str()), Some(mail.author_date.as_str()))
            .await
            .map_err(CliError::from)?;
    let commit = Commit::new(
        author,
        committer,
        tree_id,
        vec![parent],
        &format_commit_msg(&mail.message, None),
    );
    save_object(&commit, &commit.id).map_err(|error| {
        am_state_error(format!(
            "failed to save commit for '{}': {error}",
            mail.source
        ))
    })?;

    let mut next = state.clone();
    next.payload.current += 1;
    next.expected_head = commit.id;
    let next_sequence = if next.payload.current < next.payload.patches.len() {
        Some(next.to_sequence()?)
    } else {
        None
    };
    let branch = state.head_name.clone();
    let new_id = commit.id.to_string();
    let transaction_id = new_id.clone();
    let context = ReflogContext {
        old_oid: parent.to_string(),
        new_oid: new_id.clone(),
        action: ReflogAction::Commit {
            message: mail.message.clone(),
        },
    };
    with_reflog(
        context,
        move |txn| {
            Box::pin(async move {
                Branch::update_branch_with_conn(txn, &branch, &transaction_id, None).await?;
                match next_sequence {
                    Some(sequence) => sequencer::save_am_with_conn(txn, &sequence).await?,
                    None => sequencer::clear_am_with_conn(txn).await?,
                }
                Ok(())
            })
        },
        true,
    )
    .await
    .map_err(|error| {
        am_state_error(format!(
            "failed to update the branch, reflog, and am state for '{}': {error}",
            mail.source
        ))
    })?;
    *state = next;

    Ok(AppliedMail {
        source: mail.source,
        subject: mail_subject(&mail.message),
        commit: new_id,
    })
}

async fn ensure_clean_start(patches: &[MailPatch]) -> CliResult<()> {
    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| am_state_error(format!("failed to inspect staged changes: {error}")))?;
    let unstaged = status::changes_to_be_staged().map_err(|error| {
        am_state_error(format!("failed to inspect working tree changes: {error}"))
    })?;
    if !staged.is_empty() || !unstaged.modified.is_empty() || !unstaged.deleted.is_empty() {
        return Err(CliError::conflict(
            "cannot start am with staged or tracked working-tree changes",
        )
        .with_hint("commit, stash, or restore the changes before running 'libra am'"));
    }
    let index = Index::load(path::index())
        .map_err(|error| am_state_error(format!("failed to load the index for am: {error}")))?;
    let workdir = util::working_dir();
    let mut checked = HashSet::new();
    for target in patches.iter().flat_map(|mail| &mail.targets) {
        if !checked.insert(target.as_str()) || index.tracked(target, 0) {
            continue;
        }
        let absolute = validate_patch_target(target, &workdir).map_err(|error| {
            am_state_error(format!("unsafe mail patch target '{target}': {error}"))
        })?;
        match fs::symlink_metadata(&absolute) {
            Ok(_) => {
                return Err(CliError::conflict(format!(
                    "untracked working-tree path '{target}' would be overwritten by am"
                ))
                .with_hint("move or remove the untracked path before running 'libra am'"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(am_state_error(format!(
                    "failed to inspect untracked patch target '{target}': {error}"
                )));
            }
        }
    }
    Ok(())
}

async fn ensure_resolution_staged(targets: &[String]) -> CliResult<()> {
    let expected: HashSet<&str> = targets.iter().map(String::as_str).collect();
    let index = Index::load(path::index()).map_err(|error| {
        am_state_error(format!(
            "failed to load the index for am --continue: {error}"
        ))
    })?;
    let unresolved = crate::command::merge::unresolved_conflicted_paths(&index, &[]);
    if !unresolved.is_empty() {
        return Err(am_resolution_error(
            "the current am patch still has unresolved index entries",
        ));
    }

    let unstaged = status::changes_to_be_staged().map_err(|error| {
        am_state_error(format!("failed to inspect working tree changes: {error}"))
    })?;
    let has_target_unstaged = unstaged
        .polymerization()
        .iter()
        .any(|path| expected.contains(path.to_string_lossy().as_ref()));
    if has_target_unstaged {
        return Err(am_resolution_error(
            "the current am patch has changes that are not staged",
        ));
    }
    let unexpected_tracked = unstaged
        .modified
        .iter()
        .chain(&unstaged.deleted)
        .find(|path| !expected.contains(path.to_string_lossy().as_ref()))
        .cloned()
        .or_else(|| {
            unstaged
                .renamed
                .iter()
                .find(|(old, new)| {
                    !expected.contains(old.to_string_lossy().as_ref())
                        || !expected.contains(new.to_string_lossy().as_ref())
                })
                .map(|(_, new)| new.clone())
        });
    if let Some(path) = unexpected_tracked {
        return Err(am_resolution_error(&format!(
            "tracked path '{}' outside the current am patch has unstaged changes",
            path.display()
        )));
    }

    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| am_state_error(format!("failed to inspect staged changes: {error}")))?;
    if staged.is_empty() {
        return Err(am_resolution_error(
            "the current am patch has no staged resolution",
        ));
    }
    let unexpected = staged
        .polymerization()
        .into_iter()
        .find(|path| !expected.contains(path.to_string_lossy().as_ref()));
    if let Some(path) = unexpected {
        return Err(am_resolution_error(&format!(
            "staged path '{}' is outside the current am patch",
            path.display()
        )));
    }
    Ok(())
}

async fn recovery_state_is_pristine(targets: &[String]) -> CliResult<bool> {
    let index = Index::load(path::index()).map_err(|error| {
        am_state_error(format!(
            "failed to load the index for am --continue: {error}"
        ))
    })?;
    if !crate::command::merge::unresolved_conflicted_paths(&index, &[]).is_empty() {
        return Ok(false);
    }
    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| am_state_error(format!("failed to inspect staged changes: {error}")))?;
    if !staged.is_empty() {
        return Ok(false);
    }
    let unstaged = status::changes_to_be_staged().map_err(|error| {
        am_state_error(format!("failed to inspect working tree changes: {error}"))
    })?;
    if !unstaged.modified.is_empty() || !unstaged.deleted.is_empty() || !unstaged.renamed.is_empty()
    {
        return Ok(false);
    }
    let expected: HashSet<&str> = targets.iter().map(String::as_str).collect();
    Ok(!unstaged
        .new
        .iter()
        .any(|path| expected.contains(path.to_string_lossy().as_ref())))
}

fn test_failpoint_enabled(name: &str) -> bool {
    std::env::var_os("LIBRA_TEST").is_some() && std::env::var_os(name).is_some()
}

async fn stage_targets(targets: &[String]) -> CliResult<()> {
    let args = AddArgs {
        pathspec: targets.to_vec(),
        all: false,
        update: false,
        refresh: false,
        verbose: false,
        force: true,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    };
    run_add(&args).await.map(|_| ()).map_err(|error| {
        am_state_error(format!(
            "failed to stage the applied mail patch: {}",
            error.message()
        ))
        .with_hint("fix and stage the affected paths, then run 'libra am --continue'")
        .with_hint("or run 'libra am --abort' to restore the original branch")
    })
}

fn read_mail_patches(paths: &[String]) -> CliResult<Vec<MailPatch>> {
    if paths.len() > MAX_MAILS {
        return Err(CliError::command_usage(format!(
            "am accepts at most {MAX_MAILS} patch files"
        )));
    }
    let mut total = 0usize;
    let mut patches = Vec::with_capacity(paths.len());
    for source in paths {
        let bytes = fs::read(source).map_err(|error| {
            CliError::fatal(format!("cannot read mail patch '{source}': {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        total = total.checked_add(bytes.len()).ok_or_else(|| {
            CliError::fatal("mail patch input size overflow")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        if total > MAX_PATCH_BYTES {
            return Err(CliError::fatal(format!(
                "mail patch series exceeds the {} MiB limit",
                MAX_PATCH_BYTES / (1024 * 1024)
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            CliError::fatal(format!("mail patch '{source}' is not valid UTF-8"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        let parsed = parse_mail(source, &text)?;
        let author = parsed.author();
        let message = parsed.commit_message();
        let targets = patch_targets(&parsed.apply_patch, 1)
            .map_err(|detail| invalid_mail(source, &detail))?;
        if targets.is_empty() {
            return Err(invalid_mail(source, "mail patch contains no file changes"));
        }
        patches.push(MailPatch {
            source: source.to_string(),
            author,
            author_date: parsed.author_date,
            message,
            patch: parsed.apply_patch,
            targets,
        });
    }
    Ok(patches)
}

async fn load_state_or_error() -> CliResult<AmState> {
    AmState::load().await?.ok_or_else(|| {
        CliError::conflict("no am operation is in progress")
            .with_hint("start one with 'libra am <patch>...' ")
    })
}

async fn current_branch_name() -> CliResult<String> {
    match Head::current_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD: {error}")))?
    {
        Head::Branch(name) => Ok(name),
        Head::Detached(_) => Err(CliError::conflict("am cannot run on a detached HEAD")
            .with_hint("switch to a local branch before running 'libra am'")),
    }
}

async fn ensure_state_branch(state: &AmState) -> CliResult<()> {
    let current = current_branch_name().await?;
    if current != state.head_name {
        return Err(CliError::conflict(format!(
            "am started on branch '{}' but HEAD is now on '{}'",
            state.head_name, current
        ))
        .with_hint(format!(
            "switch back to '{}' before continuing or aborting",
            state.head_name
        )));
    }
    Ok(())
}

async fn ensure_expected_head(state: &AmState) -> CliResult<()> {
    let current = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| am_state_error("HEAD disappeared during am".to_string()))?;
    if current != state.expected_head {
        return Err(am_head_moved(state, current));
    }
    Ok(())
}

fn am_head_moved(state: &AmState, current: ObjectHash) -> CliError {
    CliError::conflict(format!(
        "branch '{}' moved during am (expected {}, found {})",
        state.head_name,
        short_display_hash(&state.expected_head.to_string()),
        short_display_hash(&current.to_string())
    ))
    .with_hint("restore the expected branch tip before continuing or skipping")
    .with_hint("or run 'libra am --abort' to restore the pre-am tip")
}

/// A crash or write/stage error can leave a new-file patch target untracked.
/// After reset, remove only current-mail targets that the restored index does
/// not own; pre-existing untracked collisions were rejected before am began.
fn cleanup_untracked_patch_targets(targets: &[String]) -> CliResult<()> {
    let index = Index::load(path::index())
        .map_err(|error| am_state_error(format!("failed to load the restored index: {error}")))?;
    let workdir = util::working_dir();
    for target in targets {
        if index.tracked(target, 0) {
            continue;
        }
        let absolute = validate_patch_target(target, &workdir).map_err(|error| {
            am_state_error(format!(
                "refusing to clean unsafe patch target '{target}': {error}"
            ))
        })?;
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => {
                return Err(am_state_error(format!(
                    "cannot clean patch target '{}': it became a directory",
                    target
                )));
            }
            Ok(_) => fs::remove_file(&absolute).map_err(|error| {
                am_state_error(format!("failed to clean patch target '{target}': {error}"))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(am_state_error(format!(
                    "failed to inspect patch target '{target}': {error}"
                )));
            }
        }

        let mut parent = absolute.parent();
        while let Some(directory) = parent {
            if directory == workdir || !directory.starts_with(&workdir) {
                break;
            }
            match fs::remove_dir(directory) {
                Ok(()) => parent = directory.parent(),
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    parent = directory.parent();
                }
                Err(error) => {
                    return Err(am_state_error(format!(
                        "failed to remove empty patch directory '{}': {error}",
                        directory.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

async fn reset_hard(target: &str, output: &OutputConfig) -> CliResult<()> {
    let mut child = output.child_output_config();
    child.quiet = true;
    crate::command::reset::execute_safe(
        crate::command::reset::ResetArgs {
            target: Some(target.to_string()),
            soft: false,
            mixed: false,
            hard: true,
            merge: false,
            keep: false,
            pathspecs: Vec::new(),
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
        },
        &child,
    )
    .await
    .map_err(|error| {
        am_state_error(format!(
            "failed to reset am worktree to '{target}': {}",
            error.message()
        ))
    })
}

fn render_output(result: &AmOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("am", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    if result.action == "abort" {
        if let Some(head) = &result.restored_head {
            println!("am aborted; HEAD reset to {}", short_display_hash(head));
        } else {
            println!("am aborted");
        }
        return Ok(());
    }
    for applied in &result.applied {
        println!("Applying: {}", applied.subject);
    }
    if result.action == "skip" && result.applied.is_empty() {
        println!("Skipped the current patch.");
    }
    Ok(())
}

fn am_state_error(message: String) -> CliError {
    CliError::fatal(message).with_stable_code(StableErrorCode::RepoStateInvalid)
}

fn am_conflict(mail: &MailPatch, detail: String) -> CliError {
    CliError::fatal(format!("patch failed: {}: {detail}", mail.source))
        .with_stable_code(StableErrorCode::ConflictUnresolved)
        .with_hint("resolve the affected files and stage them with 'libra add'")
        .with_hint("then run 'libra am --continue', '--skip', or '--abort'")
}

fn am_resolution_error(message: &str) -> CliError {
    CliError::fatal(message)
        .with_stable_code(StableErrorCode::ConflictUnresolved)
        .with_hint("resolve the current patch and stage only its paths with 'libra add'")
        .with_hint("then run 'libra am --continue', '--skip', or '--abort'")
}

fn mail_subject(message: &str) -> String {
    message.lines().next().unwrap_or("(no subject)").to_string()
}
