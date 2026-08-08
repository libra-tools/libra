//! Apply `format-patch` mail messages as commits.
//!
//! The PD-09 mail-flow surface: patch files or mbox series (a file — or
//! stdin `-` — whose first line is an mbox `From ` envelope is split into
//! its messages), `-3/--3way` fallback, plus `--continue`, `--skip`, and
//! `--abort`. The shared mail parser accepts 7bit/8bit/binary,
//! quoted-printable, and base64 transfer encodings and bounded MIME
//! `multipart/*` nesting with `text/plain` leaf parts; the apply seam
//! handles binary (literal/delta), rename/copy, and mode-only sections;
//! and the `applypatch-msg` / `pre-applypatch` / `post-applypatch` hooks
//! run via `repo_hooks`. Still explicitly refused: non-`text/plain` leaf
//! parts, charsets other than UTF-8/us-ascii, other transfer encodings,
//! NUL in decoded bodies, unsafe patch targets (absolute, `..`, `.libra`,
//! symlink components, non-canonical paths), and the wider Git `am`
//! option set (e.g. `--keep`, `--scissors`, `--whitespace`,
//! `--interactive`).

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
            MAX_PATCH_BYTES, PatchPreparationError, patch_targets, prepare_patch_opts,
            resolve_local_base_blob, validate_patch_target,
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
        repo_hooks::{
            RepoHook, replay_repo_hook_output, run_advisory_repo_hook, run_repo_hook_with_io,
        },
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
    libra am series.mbox                 Apply every message of an mbox in order
    libra format-patch --stdout base.. | libra am -   Apply an mbox from stdin
    libra am -3 series.mbox              Fall back to three-way merge on conflicts
    libra am --continue                  Commit a staged conflict resolution
    libra am --skip                      Skip the current mail
    libra am --abort                     Restore the pre-am branch tip";

#[derive(Parser, Debug)]
#[command(after_help = AM_EXAMPLES)]
pub struct AmArgs {
    /// Plain-text format-patch mail files, applied in order. A file whose
    /// first line is an mbox `From ` envelope is split into its messages
    /// (body content preserved byte-for-byte); `-` reads one mail or mbox
    /// from stdin (at most once).
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

    /// When a patch does not apply, fall back to a three-way merge using
    /// the blob baselines recorded in its `index` headers (PD-09 ③).
    /// A conflicting merge writes conflict markers and pauses like any
    /// other am conflict; the choice persists for the whole series.
    #[clap(short = '3', long = "3way", conflicts_with_all = ["continue_am", "skip", "abort"])]
    pub three_way: bool,
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
    /// PD-09 ③: the series was started with `-3/--3way`; resumes keep
    /// the same fallback semantics.
    #[serde(default)]
    three_way: bool,
    /// The current mail paused with conflict markers written (#13): a
    /// pristine-looking worktree on `--continue` then means "resolution
    /// not staged", never "silently re-apply and re-clobber".
    #[serde(default)]
    paused_in_conflict: bool,
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

    /// The first persistence of a STARTING series, as an atomic claim.
    async fn claim_start(&self) -> CliResult<()> {
        let sequence = self.to_sequence()?;
        sequencer::claim_start_am(&sequence)
            .await
            .map_err(am_state_error)
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
        start_am(&args.patches, args.three_way, output).await?
    };

    render_output(&result, output)
}

async fn start_am(paths: &[String], three_way: bool, output: &OutputConfig) -> CliResult<AmOutput> {
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
            three_way,
            paused_in_conflict: false,
        },
    };
    // The first write of a STARTING series is a claim, not a replace (§C.4.4).
    state.claim_start().await?;
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
    if !state.payload.paused_in_conflict && recovery_state_is_pristine(&mail.targets).await? {
        let applied = apply_remaining(&mut state, output).await?;
        return Ok(AmOutput {
            action: "continue".to_string(),
            applied,
            restored_head: None,
        });
    }
    ensure_resolution_staged(&mail.targets).await?;

    // PD-09 ⑤: the resolved-continue commit passes the same
    // `pre-applypatch` gate as a clean apply (Git's --resolved path);
    // `applypatch-msg` does not re-run — the message was settled when
    // the mail was first processed.
    run_pre_applypatch_hook(&mail, output).await?;
    state.payload.paused_in_conflict = false;
    let first = commit_current(&mut state, mail).await?;
    run_advisory_repo_hook(RepoHook::PostApplypatch, &[], None, output).await;
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
    state.payload.paused_in_conflict = false;
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
    // #11: never hard-reset AWAY commits made on top of the paused
    // sequencer state (e.g. a manual `libra commit` of the resolution) —
    // like `git am --abort`, a descendant tip is protected. Any other
    // movement (e.g. a rescue reset backwards) keeps the historical
    // restore-to-pre-am behavior.
    let current_head = Head::current_commit_result()
        .await
        .map_err(|error| am_state_error(format!("failed to resolve HEAD commit: {error}")))?
        .ok_or_else(|| am_state_error("HEAD disappeared during am".to_string()))?;
    if current_head != state.expected_head
        && crate::internal::merge_base::is_ancestor(&state.expected_head, &current_head)
            .unwrap_or(false)
    {
        return Err(CliError::fatal(
            "the branch gained commits since am paused; refusing to discard them with a \
             hard reset",
        )
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint(format!(
            "keep them: move the branch back to the paused tip first ('libra reset --hard {}'), then re-run 'libra am --abort' or '--skip'",
            short_display_hash(&state.expected_head.to_string())
        ))
        .with_hint("the new commits stay reachable via the reflog either way"));
    }
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
    output: &OutputConfig,
) -> CliResult<Vec<AppliedMail>> {
    let mut applied = Vec::new();
    while state.payload.current < state.payload.patches.len() {
        ensure_expected_head(state).await?;
        let mut mail = state.payload.patches[state.payload.current].clone();
        // PD-09 ③ (#9): pre-resolve every `index` old-blob id through the
        // FULL object store (packs included, abbreviations expanded via
        // object search) so the three-way fallback also works in cloned /
        // repacked repos where nothing is loose.
        let resolved_bases = if state.payload.three_way {
            resolve_three_way_bases(&mail.patch).await
        } else {
            std::collections::HashMap::new()
        };
        // PD-09 ⑤: `applypatch-msg` may edit the proposed commit message
        // and gates the mail BEFORE any worktree write; the state was
        // saved already, so a refusal leaves a resumable series.
        if let Some(edited) = run_applypatch_msg_hook(&mail, output).await? {
            if edited.trim().is_empty() {
                return Err(am_hook_error(
                    RepoHook::ApplypatchMsg,
                    &mail,
                    "the hook emptied the commit message".to_string(),
                ));
            }
            if edited != mail.message {
                // Persist the hook's edit with the series so a later
                // `--continue` (after a conflict pause) commits the SAME
                // message instead of silently reverting the edit.
                mail.message = edited.clone();
                state.payload.patches[state.payload.current].message = edited;
                state.save().await?;
            }
        }
        let map_resolver = |oid: &str| -> Option<Vec<u8>> {
            resolved_bases
                .get(oid)
                .cloned()
                .or_else(|| resolve_local_base_blob(oid))
        };
        let resolver: crate::command::apply::BaseBlobResolver<'_> = &map_resolver;
        let prepared = match prepare_patch_opts(
            &mail.patch,
            1,
            &util::working_dir(),
            state.payload.three_way.then_some(resolver),
        ) {
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
        let mode_overrides = prepared.mode_overrides();
        let conflicted = prepared.conflicted().to_vec();
        prepared.write().map_err(|detail| {
            am_state_error(format!("cannot apply '{}': {detail}", mail.source))
                .with_hint("fix and stage the affected paths, then run 'libra am --continue'")
                .with_hint("or run 'libra am --abort' to restore the original branch")
        })?;
        if !conflicted.is_empty() {
            // PD-09 ③: the three-way fallback left conflict markers in the
            // worktree. Stage the patch's mode overrides NOW (a chmod
            // section must not be dropped from the eventual `--continue`
            // commit), mark the pause so a resolution that happens to
            // restore HEAD content is treated as "nothing staged" instead
            // of silently re-applying the mail, then pause exactly like a
            // plain conflict.
            stage_mode_overrides(&mode_overrides)?;
            state.payload.paused_in_conflict = true;
            state.save().await?;
            return Err(am_conflict(
                &mail,
                format!(
                    "three-way merge left conflicts in: {} (resolve the <<<<<<< markers)",
                    conflicted.join(", ")
                ),
            ));
        }
        if test_failpoint_enabled("LIBRA_TEST_AM_FAIL_AFTER_WRITE") {
            return Err(am_state_error(
                "test-injected am interruption after worktree write".to_string(),
            )
            .with_hint("run 'libra am --abort' to restore the original branch"));
        }
        stage_targets(&mail.targets).await?;
        stage_mode_overrides(&mode_overrides)?;
        ensure_resolution_staged(&mail.targets).await?;
        // PD-09 ⑤: `pre-applypatch` inspects the applied+staged result and
        // gates the commit; `post-applypatch` is advisory like Git's.
        run_pre_applypatch_hook(&mail, output).await?;
        state.payload.paused_in_conflict = false;
        applied.push(commit_current(state, mail).await?);
        run_advisory_repo_hook(RepoHook::PostApplypatch, &[], None, output).await;
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

/// PD-09 ③ (#9): resolve every `index` old-blob id of one mail through
/// the full object store — packed objects included, abbreviated ids
/// expanded via object search when unambiguous. Misses are simply
/// absent from the map (the loose-scan fallback still runs).
async fn resolve_three_way_bases(patch_text: &str) -> std::collections::HashMap<String, Vec<u8>> {
    use crate::command::apply::patch_index_old_ids;

    let mut bases = std::collections::HashMap::new();
    let storage = util::objects_storage();
    for oid_prefix in patch_index_old_ids(patch_text, 1) {
        let candidates = storage.search(&oid_prefix).await;
        let [oid] = candidates.as_slice() else {
            continue; // missing or ambiguous — leave unresolved
        };
        if let Ok(bytes) = storage.get(oid)
            && bytes.len() <= MAX_PATCH_BYTES
        {
            bases.insert(oid_prefix, bytes);
        }
    }
    bases
}

/// PD-09 ⑤ `applypatch-msg`: the proposed commit message is written to
/// the worktree's `COMMIT_EDITMSG` (the one writable hook file), the
/// hook may edit it in place, and a non-zero exit refuses the mail.
/// Returns the possibly-edited message when the hook ran.
async fn run_applypatch_msg_hook(
    mail: &MailPatch,
    output: &OutputConfig,
) -> CliResult<Option<String>> {
    // §C.4.3: the hook's writable buffer belongs to the worktree this `am` is
    // acting on, resolved from the pinned workdir rather than the cwd.
    let message_path = util::request_worktree_gitdir()
        .map(|gitdir| gitdir.join("COMMIT_EDITMSG"))
        .map_err(|error| {
            am_state_error(format!(
                "failed to locate the worktree metadata directory for am hooks: {error}"
            ))
        })?;
    let Some(message_path_str) = message_path.to_str() else {
        return Err(am_state_error(format!(
            "commit message path '{}' is not valid UTF-8",
            message_path.display()
        )));
    };
    crate::utils::atomic_write::write_atomic(&message_path, mail.message.as_bytes(), false)
        .map_err(|error| {
            am_state_error(format!(
                "failed to write the am commit message file '{}': {error}",
                message_path.display()
            ))
        })?;
    let hook_output = run_repo_hook_with_io(
        RepoHook::ApplypatchMsg,
        &[message_path_str.to_string()],
        None,
        Some(&message_path),
    )
    .await
    .map_err(|error| am_hook_error(RepoHook::ApplypatchMsg, mail, error.to_string()))?;
    let Some(hook_output) = hook_output else {
        return Ok(None);
    };
    replay_repo_hook_output(&hook_output, output)
        .map_err(|detail| am_hook_error(RepoHook::ApplypatchMsg, mail, detail))?;
    if hook_output.timed_out {
        return Err(am_hook_error(
            RepoHook::ApplypatchMsg,
            mail,
            format!(
                "hook '{}' exceeded the 15 minute timeout",
                hook_output.path.display()
            ),
        ));
    }
    if hook_output.exit_code != 0 {
        return Err(am_hook_error(
            RepoHook::ApplypatchMsg,
            mail,
            format!(
                "hook '{}' exited with code {}",
                hook_output.path.display(),
                hook_output.exit_code
            ),
        ));
    }
    let edited = fs::read_to_string(&message_path).map_err(|error| {
        am_state_error(format!(
            "failed to read the am commit message file '{}': {error}",
            message_path.display()
        ))
    })?;
    Ok(Some(edited))
}

/// PD-09 ⑤ `pre-applypatch`: no arguments, gates the commit after the
/// worktree write + staging. A refusal pauses the series with the
/// applied changes staged, exactly like a conflict resolution stop.
async fn run_pre_applypatch_hook(mail: &MailPatch, output: &OutputConfig) -> CliResult<()> {
    let hook_output = run_repo_hook_with_io(RepoHook::PreApplypatch, &[], None, None)
        .await
        .map_err(|error| am_hook_error(RepoHook::PreApplypatch, mail, error.to_string()))?;
    let Some(hook_output) = hook_output else {
        return Ok(());
    };
    replay_repo_hook_output(&hook_output, output)
        .map_err(|detail| am_hook_error(RepoHook::PreApplypatch, mail, detail))?;
    if hook_output.timed_out {
        return Err(am_hook_error(
            RepoHook::PreApplypatch,
            mail,
            format!(
                "hook '{}' exceeded the 15 minute timeout",
                hook_output.path.display()
            ),
        ));
    }
    if hook_output.exit_code != 0 {
        return Err(am_hook_error(
            RepoHook::PreApplypatch,
            mail,
            format!(
                "hook '{}' exited with code {}",
                hook_output.path.display(),
                hook_output.exit_code
            ),
        ));
    }
    Ok(())
}

/// A gating am hook refused the current mail: the sequencer state is
/// already saved, so the series pauses with the usual resume verbs.
fn am_hook_error(hook: RepoHook, mail: &MailPatch, detail: String) -> CliError {
    CliError::fatal(format!(
        "{} hook refused mail '{}': {detail}",
        hook.as_str(),
        mail.source
    ))
    .with_stable_code(StableErrorCode::RepoStateInvalid)
    .with_hint("fix the hook (or bypass with LIBRA_NO_HOOKS=1), then run 'libra am --continue'")
    .with_hint("or run 'libra am --skip' / 'libra am --abort'")
}

/// PD-09 ②: force the index modes a patch's `old mode`/`new mode`/`new
/// file mode` headers demand. The regular add path cannot do this — a
/// mode-only flip leaves content unchanged, so the worktree-change scan
/// never re-stages the file.
fn stage_mode_overrides(overrides: &[(String, u32)]) -> CliResult<()> {
    if overrides.is_empty() {
        return Ok(());
    }
    let index_path = path::index();
    let mut index = Index::load(&index_path)
        .map_err(|error| am_state_error(format!("failed to load the index for am: {error}")))?;
    let workdir = util::working_dir();
    let mut changed = false;
    for (target, mode) in overrides {
        let Some((current_mode, hash)) = index.get(target, 0).map(|entry| (entry.mode, entry.hash))
        else {
            continue;
        };
        // Only regular blobs carry an executable bit.
        if current_mode & 0o170000 != 0o100000 {
            continue;
        }
        let wanted = if mode & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        };
        if current_mode == wanted {
            continue;
        }
        // No content read happens here (the hash is inherited), so the
        // fresh stat cannot be verified against it — pass no pre-read
        // stat and let the entry smudge to a content-compare-next-status
        // shape (2026-08-06 R0-8 review).
        let mut updated = crate::command::verified_index_entry(
            std::path::Path::new(target),
            hash,
            &workdir,
            None,
        )
        .map_err(|error| {
            am_state_error(format!(
                "failed to restage mode for am target '{target}': {error}"
            ))
        })?;
        updated.mode = wanted;
        index.update(updated);
        changed = true;
    }
    if changed {
        index.save(&index_path).map_err(|error| {
            am_state_error(format!("failed to save index mode changes for am: {error}"))
        })?;
    }
    Ok(())
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
    if paths.iter().filter(|path| path.as_str() == "-").count() > 1 {
        return Err(CliError::command_usage(
            "stdin ('-') can be given at most once",
        ));
    }
    let mut total = 0usize;
    let mut patches = Vec::with_capacity(paths.len());
    for source in paths {
        let bytes = if source == "-" {
            read_mail_stdin()?
        } else {
            fs::read(source).map_err(|error| {
                CliError::fatal(format!("cannot read mail patch '{source}': {error}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
        };
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
        // PD-09 ①: an input whose first line is an mbox envelope carries a
        // whole message series; anything else stays one single mail
        // (byte-identical to the pre-mbox behavior).
        let messages = split_mbox(&text);
        let labelled = messages.len() > 1;
        for (index, message) in messages.iter().enumerate() {
            let label = if labelled {
                format!("{source}#{}", index + 1)
            } else {
                source.to_string()
            };
            if patches.len() >= MAX_MAILS {
                return Err(CliError::command_usage(format!(
                    "am accepts at most {MAX_MAILS} mails per invocation"
                )));
            }
            let parsed = parse_mail(&label, message)?;
            let author = parsed.author();
            let commit_message = parsed.commit_message();
            let targets = patch_targets(&parsed.apply_patch, 1)
                .map_err(|detail| invalid_mail(&label, &detail))?;
            if targets.is_empty() {
                return Err(invalid_mail(&label, "mail patch contains no file changes"));
            }
            patches.push(MailPatch {
                source: label,
                author,
                author_date: parsed.author_date,
                message: commit_message,
                patch: parsed.apply_patch,
                targets,
            });
        }
    }
    Ok(patches)
}

/// Read one mail (or mbox) from stdin, bounded by the shared patch-series
/// byte cap so a hostile pipe cannot balloon memory.
fn read_mail_stdin() -> CliResult<Vec<u8>> {
    use std::io::Read;

    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_PATCH_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            CliError::fatal(format!("cannot read mail patch from stdin: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    if bytes.len() > MAX_PATCH_BYTES {
        return Err(CliError::fatal(format!(
            "mail patch series exceeds the {} MiB limit",
            MAX_PATCH_BYTES / (1024 * 1024)
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    Ok(bytes)
}

/// Split raw mail text into individual messages (PD-09 ①). Only input
/// whose FIRST line is an mbox `From ` envelope line is treated as an
/// mbox: every later envelope line starts a new message. Body content is
/// preserved byte-for-byte — like `git am`'s default (mboxo) reading, a
/// quoted `>From ` line is NOT unquoted, and the strict ctime-shaped
/// envelope test keeps prose `From …` lines inside their message.
fn split_mbox(text: &str) -> Vec<String> {
    let first_line = text.split_inclusive('\n').next().unwrap_or("");
    if !is_mbox_from_line(first_line.trim_end_matches(['\n', '\r'])) {
        return vec![text.to_string()];
    }
    let mut messages: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if is_mbox_from_line(line.trim_end_matches(['\n', '\r'])) && !current.is_empty() {
            messages.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        messages.push(current);
    }
    messages
}

/// mbox envelope test matching `git mailsplit`'s `is_from_line` shape:
/// `From ` + a sender token + a ctime-like date tail — an `hh:mm:ss`
/// time token immediately followed by a 4-digit year, with trailing
/// timezone/UUCP tokens allowed after the year. Ordinary
/// commit-message prose (`From my reading of RFC 9110`) has no time
/// token and never splits.
fn is_mbox_from_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("From ") else {
        return false;
    };
    let mut fields = rest.split_whitespace();
    if fields.next().is_none() {
        return false;
    }
    let tail: Vec<&str> = fields.collect();
    tail.len() >= 4
        && tail
            .windows(2)
            .any(|pair| is_time_of_day_token(pair[0]) && is_year_token(pair[1]))
}

fn is_time_of_day_token(token: &str) -> bool {
    let parts: Vec<&str> = token.split(':').collect();
    parts.len() == 3
        && (1..=2).contains(&parts[0].len())
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_year_token(token: &str) -> bool {
    token.len() == 4 && token.bytes().all(|byte| byte.is_ascii_digit())
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
