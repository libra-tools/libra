//! Show command that resolves object IDs and prints commit, tree, blob, or ref details with formatting suitable for diffable objects.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::IsTerminal,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use colored::Colorize;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        blob::Blob,
        commit::Commit,
        tree::{Tree, TreeItemMode},
        types::ObjectType,
    },
};
use serde::Serialize;

use crate::{
    command::{
        load_object,
        log::{
            ChangeType,
            config::{configured_date, configured_pretty, resolve_cli_date},
            generate_diff, get_changed_files_for_commit, parse_pretty_format,
        },
    },
    common_utils::parse_commit_msg,
    internal::{
        branch::Branch,
        head::Head,
        log::formatter::{CommitFormatter, FormatContext, format_timestamp_with},
        tag,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{ColorChoice, OutputConfig, emit_json_data},
        pager::Pager,
        path, util,
    },
};

const SHOW_EXAMPLES: &str = "\
EXAMPLES:
    libra show HEAD                         Show the latest commit and patch
    libra show --no-patch v1.0.0            Show tag or commit metadata only
    libra show HEAD:src/main.rs             Show a file from a specific revision
    libra show --stat HEAD~1                Show only diff statistics
    libra show --patch-with-stat HEAD       Show the diffstat followed by the full patch
    libra show --name-status HEAD           Show changed files with A/M/D status
    libra show --raw HEAD                    Raw diff format (mode/sha/status per file)
    libra show --summary HEAD               Show created/deleted file mode summary
    libra show --format='%h %s' HEAD        Custom header format (alias for --pretty)
    libra show --abbrev-commit HEAD         Abbreviate the commit hash in the header
    libra --json show HEAD                  Structured JSON output for agents";

/// Shows commits, tags, trees, or blobs.
#[derive(Parser, Debug)]
#[command(after_help = SHOW_EXAMPLES)]
pub struct ShowArgs {
    /// Object name (commit, tag, etc.) or `<object>:<path>`. Defaults to `HEAD`.
    #[clap(value_name = "OBJECT")]
    pub object: Option<String>,

    /// Skip patch output and only show object metadata.
    #[clap(long, short = 's')]
    pub no_patch: bool,

    /// Shorthand for `--pretty=oneline`.
    #[clap(long)]
    pub oneline: bool,

    /// Format the commit header with a pretty format
    /// (`oneline` / `format:<tmpl>` / `tformat:<tmpl>` / custom template).
    #[clap(long, value_name = "FORMAT")]
    pub pretty: Option<String>,

    /// Alias for `--pretty=<format>` (Git's `--format`). Accepts the same preset
    /// names and `%`-placeholder templates as `--pretty`.
    #[clap(long, value_name = "FORMAT", conflicts_with = "pretty")]
    pub format: Option<String>,

    /// Date rendering mode for author/committer dates: default / short / iso /
    /// iso-strict / rfc / unix / raw.
    #[clap(long, value_name = "FORMAT")]
    pub date: Option<String>,

    /// Abbreviate the commit object name in the default header instead of
    /// printing the full (unabbreviated) hash.
    #[clap(long, overrides_with = "no_abbrev_commit")]
    pub abbrev_commit: bool,

    /// Show the full (unabbreviated) commit object name, countermanding an
    /// earlier `--abbrev-commit` (last one on the command line wins), matching
    /// `git show --no-abbrev-commit`. The full hash is the default, so on its
    /// own this is a no-op.
    #[clap(long = "no-abbrev-commit", overrides_with = "abbrev_commit")]
    pub no_abbrev_commit: bool,

    /// Show only changed file names.
    #[clap(long)]
    pub name_only: bool,

    /// Show only names and status (A/M/D) of changed files.
    #[clap(long = "name-status")]
    pub name_status: bool,

    /// Show the diff in the raw format (`:<old-mode> <new-mode> <old-sha>
    /// <new-sha> <status>\t<path>`) instead of a patch, like `git show --raw`.
    #[clap(long)]
    pub raw: bool,

    /// Show diff statistics.
    #[clap(long)]
    pub stat: bool,

    /// Show the diffstat block followed by the full patch (Git's legacy synonym
    /// for `-p --stat`).
    #[clap(long = "patch-with-stat")]
    pub patch_with_stat: bool,

    /// Show a condensed summary of created and deleted files (their mode and
    /// path), like `git show --summary`. Mirrors `libra diff --summary`:
    /// rename/copy and mode-change detection are not implemented.
    #[clap(long)]
    pub summary: bool,

    /// Do not expand tabs in the commit message. Accepted for Git parity and is
    /// a no-op: Libra's show never expands tabs (it prints them verbatim).
    #[clap(long = "no-expand-tabs")]
    pub no_expand_tabs: bool,

    /// Do not show commit notes. Accepted for Git parity and is a no-op: Libra's
    /// show never displays notes inline. (Use `libra notes show <commit>`.)
    #[clap(long = "no-notes")]
    pub no_notes: bool,

    /// Do not use a `.mailmap` to rewrite identities. Accepted for Git parity
    /// and is a no-op: Libra's show never applies a mailmap.
    #[clap(long = "no-mailmap")]
    pub no_mailmap: bool,

    /// Do not display the GPG signature of signed commits. Accepted for Git
    /// parity and is a no-op: Libra's show never displays commit signatures
    /// inline. (Git's opposite `--show-signature` is not implemented.)
    #[clap(long = "no-show-signature")]
    pub no_show_signature: bool,

    /// Limit output to matching paths.
    #[clap(value_name = "PATHS", num_args = 0..)]
    pub pathspec: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ShowOutput {
    #[serde(rename = "commit")]
    Commit(ShowCommitData),
    #[serde(rename = "tag")]
    Tag(ShowTagData),
    #[serde(rename = "tree")]
    Tree(ShowTreeData),
    #[serde(rename = "blob")]
    Blob(ShowBlobData),
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowCommitData {
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    pub subject: String,
    pub body: String,
    pub parents: Vec<String>,
    pub refs: Vec<String>,
    pub files: Vec<ShowFileChange>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowFileChange {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowTagData {
    pub tag_name: String,
    pub tagger_name: Option<String>,
    pub tagger_email: Option<String>,
    pub tagger_date: Option<String>,
    pub message: String,
    pub target_hash: String,
    pub target_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowTreeData {
    pub entries: Vec<ShowTreeEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowTreeEntry {
    pub mode: String,
    pub object_type: String,
    pub hash: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowBlobData {
    pub hash: String,
    pub size: usize,
    pub is_binary: bool,
    pub content: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum ShowError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("bad revision '{revision}'")]
    BadRevision { revision: String },

    #[error("path '{path}' does not exist in '{revision}'")]
    PathNotFound { path: String, revision: String },

    #[error("failed to load object '{object_id}': {detail}")]
    ObjectLoad { object_id: String, detail: String },

    #[error("unsupported object type for display: {object_type}")]
    UnsupportedObjectType { object_type: String },
}

impl From<ShowError> for CliError {
    fn from(error: ShowError) -> Self {
        match error {
            ShowError::NotInRepo => CliError::repo_not_found(),
            ShowError::BadRevision { revision } => {
                CliError::fatal(format!("bad revision '{revision}'"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra log --oneline' to see available commits, or 'libra tag -l' to see available tags.")
            }
            ShowError::PathNotFound { path, revision } => {
                CliError::fatal(format!("path '{path}' does not exist in '{revision}'"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("check the path and revision; use 'libra show <rev>:' to list the tree")
            }
            ShowError::ObjectLoad { object_id, detail } => {
                CliError::fatal(format!("failed to load object '{object_id}': {detail}"))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                    .with_hint("the object store may be corrupted")
            }
            ShowError::UnsupportedObjectType { object_type } => {
                CliError::fatal(format!("unsupported object type for display: {object_type}"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            }
        }
    }
}

/// Executes the show command.
pub async fn execute(args: ShowArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Resolves a revision (commit, tag, tree, blob, or
/// `<rev>:<path>`) and prints its contents with diff formatting.
pub async fn execute_safe(mut args: ShowArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::from(ShowError::NotInRepo))?;

    if let Some(date) = args.date.take() {
        args.date = Some(resolve_cli_date(&date)?);
    }

    if output.is_json() {
        let result = run_show(&args).await?;
        return emit_json_data("show", &result, output);
    }

    if output.quiet {
        return validate_show_quiet(&args).await;
    }

    // These are commit-display defaults. Invalid values must not block
    // tree/blob/REV:path output, which never renders either setting.
    if show_target_uses_commit_display(&args).await? {
        if !args.oneline && args.pretty.is_none() && args.format.is_none() {
            let configured = configured_pretty().await?;
            // `medium` is Git's default renderer, including the full commit id.
            if configured.as_deref() != Some("medium") {
                args.pretty = configured;
            }
        }
        if args.date.is_none() {
            args.date = configured_date().await?;
        }
    }

    let rendered = render_show_human(&args, color_enabled_for_output(output)).await?;
    if rendered.is_empty() {
        return Ok(());
    }

    let mut pager = Pager::with_config(output)?;
    pager.write_str(&rendered)?;
    pager.finish()
}

fn color_enabled_for_output(output: &OutputConfig) -> bool {
    match output.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::io::stdout().is_terminal(),
    }
}

async fn show_target_uses_commit_display(args: &ShowArgs) -> CliResult<bool> {
    let object_ref = args.object.as_deref().unwrap_or("HEAD");
    if object_ref.contains(':') {
        return Ok(false);
    }

    if let Some(hash) = resolve_existing_object_hash(object_ref) {
        let storage = ClientStorage::init(path::objects());
        let object_type = storage
            .get_object_type(&hash)
            .map_err(|error| show_object_load_error(hash, error))?;
        return Ok(matches!(object_type, ObjectType::Commit | ObjectType::Tag));
    }

    Ok(util::get_commit_base(object_ref).await.is_ok())
}

async fn render_show_human(args: &ShowArgs, color_enabled: bool) -> CliResult<String> {
    let object_ref = args.object.as_deref().unwrap_or("HEAD");

    // Handle `<revision>:<path>` lookups before generic revision resolution.
    if let Some((rev, path)) = object_ref.split_once(':') {
        return show_commit_file(rev, path).await;
    }

    // Raw object IDs should keep their native schema, including annotated tag
    // objects, but hash-like ref names must still fall back to ref resolution.
    if let Some(hash) = resolve_existing_object_hash(object_ref) {
        return show_object_by_hash(&hash, args, color_enabled).await;
    }

    // Resolve refs first so tags keep their custom rendering.
    if let Ok(commit_hash) = util::get_commit_base(object_ref).await {
        // Use find_tag_and_commit to check if it's a tag and get tag info
        match tag::find_tag_and_commit(object_ref).await {
            Ok(Some((object, _))) if object.get_type() == ObjectType::Tag => {
                // For annotated tags, show tag details
                let tag_hash = if let tag::TagObject::Tag(tag_obj) = &object {
                    tag_obj.id
                } else {
                    commit_hash
                };
                return show_tag_by_hash(&tag_hash, args, color_enabled).await;
            }
            _ => {
                // Not a tag, lightweight tag, or tag doesn't exist: show as commit.
                return show_commit(&commit_hash, args, color_enabled).await;
            }
        }
    }

    Err(show_bad_revision_error(object_ref))
}

async fn validate_show_quiet(args: &ShowArgs) -> CliResult<()> {
    let object_ref = args.object.as_deref().unwrap_or("HEAD");

    if let Some((rev, path)) = object_ref.split_once(':') {
        return validate_commit_file(rev, path).await;
    }

    if let Some(hash) = resolve_existing_object_hash(object_ref) {
        return validate_object_by_hash(&hash, args).await;
    }

    if let Ok(commit_hash) = util::get_commit_base(object_ref).await {
        match tag::find_tag_and_commit(object_ref).await {
            Ok(Some((object, _))) if object.get_type() == ObjectType::Tag => {
                let tag_hash = if let tag::TagObject::Tag(tag_obj) = &object {
                    tag_obj.id
                } else {
                    commit_hash
                };
                return validate_tag_by_hash(&tag_hash, args).await;
            }
            _ => {
                return validate_commit_output(&commit_hash, args).await;
            }
        }
    }

    Err(show_bad_revision_error(object_ref))
}

/// Shows an object by hash after resolving its object type.
fn show_object_by_hash<'a>(
    hash: &'a ObjectHash,
    args: &'a ShowArgs,
    color_enabled: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = CliResult<String>> + 'a>> {
    Box::pin(async move {
        let storage = ClientStorage::init(path::objects());

        let obj_type = storage
            .get_object_type(hash)
            .map_err(|e| show_object_load_error(hash, e))?;

        match obj_type {
            ObjectType::Commit => show_commit(hash, args, color_enabled).await,
            ObjectType::Tag => show_tag_by_hash(hash, args, color_enabled).await,
            ObjectType::Tree => show_tree(hash).await,
            ObjectType::Blob => show_blob(hash).await,
            _ => Err(show_unsupported_object_type_error(format!("{obj_type:?}"))),
        }
    })
}

fn validate_object_by_hash<'a>(
    hash: &'a ObjectHash,
    args: &'a ShowArgs,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = CliResult<()>> + 'a>> {
    Box::pin(async move {
        let storage = ClientStorage::init(path::objects());

        let obj_type = storage
            .get_object_type(hash)
            .map_err(|e| show_object_load_error(hash, e))?;

        match obj_type {
            ObjectType::Commit => validate_commit_output(hash, args).await,
            ObjectType::Tag => validate_tag_by_hash(hash, args).await,
            ObjectType::Tree => validate_tree(hash),
            ObjectType::Blob => validate_blob(hash),
            _ => Err(show_unsupported_object_type_error(format!("{obj_type:?}"))),
        }
    })
}

/// Shows a commit together with optional diff output.
async fn show_commit(
    commit_hash: &ObjectHash,
    args: &ShowArgs,
    color_enabled: bool,
) -> CliResult<String> {
    // Load the commit before rendering any metadata or diff output.
    let commit =
        load_object::<Commit>(commit_hash).map_err(|e| show_object_load_error(commit_hash, e))?;

    let mut output = String::new();
    display_commit_info(&mut output, &commit, args, color_enabled);

    // Render patch-style details when requested.
    if !args.no_patch {
        let paths: Vec<PathBuf> = args.pathspec.iter().map(util::to_workdir_path).collect();

        if args.patch_with_stat {
            // `--patch-with-stat` (Git's `-p --stat`): the diffstat block followed
            // by the full patch.
            let diffstat = show_diffstat(&commit, paths.clone()).await?;
            if !diffstat.is_empty() {
                output.push_str(&diffstat);
            }
            let diff_output = generate_diff(&commit, paths).await?;
            if !diff_output.is_empty() {
                output.push('\n');
                output.push_str(&diff_output);
            }
        } else if args.stat {
            // Show the summary view.
            let diffstat = show_diffstat(&commit, paths.clone()).await?;
            if !diffstat.is_empty() {
                output.push_str(&diffstat);
            }
        } else if args.summary {
            // Show only the created/deleted-file summary, parsed out of the
            // commit's unified diff (the same text the patch view renders).
            let diff_output = generate_diff(&commit, paths).await?;
            let summary = format_show_summary(&diff_output);
            if !summary.is_empty() {
                output.push('\n');
                output.push_str(&summary);
                output.push('\n');
            }
        } else if args.name_status {
            // Show changed file names prefixed by their status letter (A/M/D),
            // tab-separated, matching `git show --name-status`.
            let changed_files = get_changed_files_for_commit(&commit, &paths).await?;
            if !changed_files.is_empty() {
                output.push('\n');
                for file in changed_files {
                    let status = match file.status {
                        ChangeType::Added => "A",
                        ChangeType::Modified => "M",
                        ChangeType::Deleted => "D",
                    };
                    output.push_str(&format!("{}\t{}\n", status, file.path.display()));
                }
            }
        } else if args.raw {
            // Show the raw diff format, matching `git show --raw`:
            // `:<old-mode> <new-mode> <old-sha> <new-sha> <status>\t<path>`.
            let raw_lines = raw_diff_lines_for_commit(&commit, &paths).await?;
            if !raw_lines.is_empty() {
                output.push('\n');
                output.push_str(&raw_lines);
            }
        } else if args.name_only {
            // Show only changed file names.
            let changed_files = get_changed_files_for_commit(&commit, &paths).await?;
            if !changed_files.is_empty() {
                output.push('\n');
                for file in changed_files {
                    output.push_str(&format!("{}\n", file.path.display()));
                }
            }
        } else {
            // Show the full patch.
            let diff_output = generate_diff(&commit, paths).await?;
            if !diff_output.is_empty() {
                output.push('\n');
                output.push_str(&diff_output);
            }
        }
    }
    Ok(output)
}

/// Build the `--summary` view from a commit's full unified diff text: one
/// ` create mode <mode> <path>` line per created file and ` delete mode <mode>
/// <path>` per deleted file. Mirrors `libra diff --summary` (created/deleted
/// files only — rename/copy and mode-change detection are not implemented).
/// Returns an empty string when nothing was created or deleted.
fn format_show_summary(diff_text: &str) -> String {
    let mut lines = Vec::new();
    // Path of the file in the current `diff --git` block, recovered from the
    // header (see `reconstruct_identical_diff_path`) and reset at every header.
    // The `new file mode`/`deleted file mode` line — which is present for binary
    // files too (their hunks carry no `---`/`+++` path lines) — then emits using
    // that path, so binary creates/deletes are handled and a stale path can
    // never leak into the next file's block.
    let mut current_path: Option<String> = None;
    for line in diff_text.lines() {
        if let Some(body) = line.strip_prefix("diff --git a/") {
            current_path = reconstruct_identical_diff_path(body);
        } else if line.starts_with("diff --git ") {
            // A header not in the expected `a/<P> b/<P>` shape (should not occur
            // for created/deleted files): drop any path so we never misattribute.
            current_path = None;
        } else if let Some(mode) = line.strip_prefix("new file mode ")
            && let Some(path) = &current_path
        {
            lines.push(format!(" create mode {} {}", mode.trim(), path));
        } else if let Some(mode) = line.strip_prefix("deleted file mode ")
            && let Some(path) = &current_path
        {
            lines.push(format!(" delete mode {} {}", mode.trim(), path));
        }
    }
    lines.join("\n")
}

/// Recover the file path from the body of a `diff --git a/<P> b/<P>` header (the
/// text after the leading `a/`). For created/deleted files the a-side and
/// b-side are the identical `<P>`, so the body is exactly `<P> b/<P>`; we split
/// at the unique midpoint and verify the reconstruction, which recovers paths
/// containing " b/" or spaces. Returns `None` for rename headers (a-side !=
/// b-side), since those are neither creates nor deletes.
fn reconstruct_identical_diff_path(body: &str) -> Option<String> {
    // `body` == "<P> b/<P>" ⇒ len == 2*len(P) + len(" b/").
    const SEP: &str = " b/";
    if body.len() <= SEP.len() || !(body.len() - SEP.len()).is_multiple_of(2) {
        return None;
    }
    let plen = (body.len() - SEP.len()) / 2;
    // `plen` is a char boundary for any genuine `<P> b/<P>` body (it lands at the
    // end of the first `<P>`); `get` returns None otherwise, declining safely.
    let candidate = body.get(..plen)?;
    if body == format!("{candidate} b/{candidate}") {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Build the `git show --raw` body for a commit: one line per changed file,
/// `:<old-mode> <new-mode> <old-sha> <new-sha> <status>\t<path>` (object ids
/// abbreviated to 7, the absent side rendered as zeros). The change set is built
/// directly from the commit and its first-parent trees so it is mode-aware (a
/// same-content file whose mode changed still shows as `M`, matching Git —
/// reachable e.g. for history imported from Git) and path-ordered.
async fn raw_diff_lines_for_commit(commit: &Commit, paths: &[PathBuf]) -> CliResult<String> {
    let new_map = tree_entry_map(&commit.tree_id)?;
    let old_map = if let Some(parent) = commit.parent_commit_ids.first() {
        let parent_commit =
            load_object::<Commit>(parent).map_err(|e| show_object_load_error(parent, e))?;
        tree_entry_map(&parent_commit.tree_id)?
    } else {
        BTreeMap::new()
    };
    Ok(build_raw_lines(&old_map, &new_map, paths))
}

/// Map each blob path in a tree to its `(octal-mode, abbreviated-id)`.
fn tree_entry_map(tree_id: &ObjectHash) -> CliResult<BTreeMap<PathBuf, (String, String)>> {
    let tree = load_object::<Tree>(tree_id).map_err(|e| show_object_load_error(tree_id, e))?;
    Ok(tree
        .get_plain_items_with_mode()
        .into_iter()
        .map(|(path, hash, mode)| {
            let mode_str = String::from_utf8_lossy(mode.to_bytes()).into_owned();
            let sha = hash.to_string().chars().take(7).collect::<String>();
            (path, (mode_str, sha))
        })
        .collect())
}

/// Render the raw diff lines for the union of `old_map`/`new_map` paths (filtered
/// by `paths`): added paths show a zeroed old side, deleted paths a zeroed new
/// side, and a path present on both sides is `M` when its `(mode, id)` differs
/// (so a mode-only change is reported, like Git). Output is path-ordered.
fn build_raw_lines(
    old_map: &BTreeMap<PathBuf, (String, String)>,
    new_map: &BTreeMap<PathBuf, (String, String)>,
    paths: &[PathBuf],
) -> String {
    const ZERO_MODE: &str = "000000";
    let zero_sha = "0".repeat(7);
    let absent = || (ZERO_MODE.to_string(), zero_sha.clone());
    let matches_filter = |path: &Path| {
        paths.is_empty() || paths.iter().any(|filter| util::is_sub_path(path, filter))
    };

    let mut all_paths: BTreeSet<&PathBuf> = BTreeSet::new();
    all_paths.extend(old_map.keys());
    all_paths.extend(new_map.keys());

    let mut out = String::new();
    for path in all_paths {
        if !matches_filter(path) {
            continue;
        }
        let (status, (old_mode, old_sha), (new_mode, new_sha)) =
            match (old_map.get(path), new_map.get(path)) {
                (None, Some(new)) => ("A", absent(), new.clone()),
                (Some(old), None) => ("D", old.clone(), absent()),
                (Some(old), Some(new)) => {
                    if old == new {
                        continue;
                    }
                    ("M", old.clone(), new.clone())
                }
                (None, None) => continue,
            };
        out.push_str(&format!(
            ":{old_mode} {new_mode} {old_sha} {new_sha} {status}\t{}\n",
            path.display()
        ));
    }
    out
}

async fn validate_commit_output(commit_hash: &ObjectHash, args: &ShowArgs) -> CliResult<()> {
    let commit =
        load_object::<Commit>(commit_hash).map_err(|e| show_object_load_error(commit_hash, e))?;

    if args.no_patch {
        return Ok(());
    }

    let paths: Vec<PathBuf> = args.pathspec.iter().map(util::to_workdir_path).collect();
    if args.stat || args.name_only || args.name_status || args.raw {
        // --stat / --name-only / --name-status / --raw human paths only need
        // tree-level file lists, not blob contents.  Use the same function so
        // quiet mode has the same success/failure semantics as the visible
        // rendering path.
        let _ = get_changed_files_for_commit(&commit, &paths).await?;
    } else {
        let _ = generate_diff(&commit, paths).await?;
    }

    Ok(())
}

/// Shows an annotated or lightweight tag.
async fn show_tag_by_hash(
    hash: &ObjectHash,
    args: &ShowArgs,
    color_enabled: bool,
) -> CliResult<String> {
    match tag::load_object_trait(hash).await {
        Ok(tag::TagObject::Tag(tag_obj)) => {
            let mut output = String::new();
            // Render the annotated tag header.
            output.push_str(&format!("{} {}\n", "tag".yellow(), tag_obj.tag_name));
            output.push_str(&format!(
                "Tagger: {} <{}>",
                tag_obj.tagger.name.trim(),
                tag_obj.tagger.email.trim()
            ));
            output.push('\n');

            output.push_str(&format!(
                "Date:   {}\n\n",
                format_timestamp_with(
                    tag_obj.tagger.timestamp as i64,
                    args.date.as_deref().unwrap_or_default(),
                )
            ));
            output.push_str(tag_obj.message.trim());
            output.push_str("\n\n");

            // Continue with the tagged object.
            output.push_str(&show_object_by_hash(&tag_obj.object_hash, args, color_enabled).await?);
            Ok(output)
        }
        Ok(tag::TagObject::Commit(commit)) => {
            // Lightweight tags point directly to commits.
            show_commit(&commit.id, args, color_enabled).await
        }
        Ok(_) => Err(show_unsupported_object_type_error("tag target")),
        Err(e) => Err(show_object_load_error(hash, e)),
    }
}

async fn validate_tag_by_hash(hash: &ObjectHash, args: &ShowArgs) -> CliResult<()> {
    match tag::load_object_trait(hash).await {
        Ok(tag::TagObject::Tag(tag_obj)) => {
            validate_object_by_hash(&tag_obj.object_hash, args).await
        }
        Ok(tag::TagObject::Commit(commit)) => validate_commit_output(&commit.id, args).await,
        Ok(_) => Err(show_unsupported_object_type_error("tag target")),
        Err(e) => Err(show_object_load_error(hash, e)),
    }
}

/// Shows a tree object.
async fn show_tree(hash: &ObjectHash) -> CliResult<String> {
    let tree = load_object::<Tree>(hash).map_err(|e| show_object_load_error(hash, e))?;

    let mut output = format!("{} {}\n\n", "tree".yellow(), hash);

    for item in &tree.tree_items {
        output.push_str(&format!(
            "{:06o} {} {}\t{}\n",
            tree_item_mode_to_u32(item.mode),
            tree_item_mode_to_object_type(item.mode),
            item.id,
            item.name
        ));
    }
    Ok(output)
}

fn validate_tree(hash: &ObjectHash) -> CliResult<()> {
    load_object::<Tree>(hash).map_err(|e| show_object_load_error(hash, e))?;
    Ok(())
}

/// Shows a blob as text when possible.
async fn show_blob(hash: &ObjectHash) -> CliResult<String> {
    let blob = load_object::<Blob>(hash).map_err(|e| show_object_load_error(hash, e))?;

    // Print text blobs directly and summarize binary blobs.
    match String::from_utf8(blob.data.clone()) {
        Ok(text) => Ok(text),
        Err(_) => Ok(format!("Binary file (size: {} bytes)\n", blob.data.len())),
    }
}

fn validate_blob(hash: &ObjectHash) -> CliResult<()> {
    load_object::<Blob>(hash).map_err(|e| show_object_load_error(hash, e))?;
    Ok(())
}

/// Shows a file from a specific revision.
async fn show_commit_file(rev: &str, file_path: &str) -> CliResult<String> {
    // Resolve the revision before looking up the path.
    let commit_hash = util::get_commit_base(rev)
        .await
        .map_err(|_| show_bad_revision_error(rev))?;

    let commit =
        load_object::<Commit>(&commit_hash).map_err(|e| show_object_load_error(commit_hash, e))?;

    // Load the tree for the resolved commit.
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| show_object_load_error(commit.tree_id, e))?;

    // Find the target path inside the tree.
    let items = tree.get_plain_items();
    let target_path = PathBuf::from(file_path);

    if let Some((_, blob_hash)) = items.iter().find(|(path, _)| path == &target_path) {
        show_blob(blob_hash).await
    } else {
        Err(show_path_not_found_error(file_path, rev))
    }
}

async fn validate_commit_file(rev: &str, file_path: &str) -> CliResult<()> {
    let commit_hash = util::get_commit_base(rev)
        .await
        .map_err(|_| show_bad_revision_error(rev))?;

    let commit =
        load_object::<Commit>(&commit_hash).map_err(|e| show_object_load_error(commit_hash, e))?;

    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| show_object_load_error(commit.tree_id, e))?;

    let items = tree.get_plain_items();
    let target_path = PathBuf::from(file_path);

    if let Some((_, blob_hash)) = items.iter().find(|(path, _)| path == &target_path) {
        validate_blob(blob_hash)
    } else {
        Err(show_path_not_found_error(file_path, rev))
    }
}

/// Renders the commit header using the selected format.
fn display_commit_info(output: &mut String, commit: &Commit, args: &ShowArgs, color_enabled: bool) {
    // `--format` is Git's alias for `--pretty` (mutually exclusive in clap).
    if let Some(pretty) = args
        .pretty
        .as_ref()
        .or(args.format.as_ref())
        .filter(|pretty| pretty.as_str() != "medium")
    {
        // `--pretty=<fmt>` renders the commit header through the shared log
        // formatter (oneline / format:<tmpl> / tformat:<tmpl> / custom template).
        let formatter = CommitFormatter::new(parse_pretty_format(pretty.clone()))
            .with_date_mode(args.date.clone().unwrap_or_default())
            .with_color_enabled(color_enabled);
        let ctx = FormatContext {
            graph_prefix: "",
            decoration: "",
            abbrev_len: 7,
            extra_hashes: "",
        };
        output.push_str(&formatter.format(commit, &ctx));
        output.push('\n');
        return;
    }
    if args.oneline {
        // Oneline format prints the short hash and the first subject line.
        let short_hash = &commit.id.to_string()[..7];
        let (msg, _) = parse_commit_msg(&commit.message);
        let first_line = msg.lines().next().unwrap_or("");
        output.push_str(&format!("{} {}\n", short_hash.yellow(), first_line));
    } else {
        // Full format matches the default `show` header layout. `--abbrev-commit`
        // shortens the object name to a 7-character prefix.
        let full = commit.id.to_string();
        let hash = if args.abbrev_commit {
            &full[..7.min(full.len())]
        } else {
            full.as_str()
        };
        output.push_str(&format!("{} {}\n", "commit".yellow(), hash.yellow()));
        output.push_str(&format!(
            "Author: {} <{}>\n",
            commit.author.name.trim(),
            commit.author.email.trim()
        ));

        // Format the commit timestamp for display.
        output.push_str(&format!(
            "Date:   {}\n",
            format_timestamp_with(
                commit.committer.timestamp as i64,
                args.date.as_deref().unwrap_or_default(),
            )
        ));

        // Print the commit message body.
        let (msg, _) = parse_commit_msg(&commit.message);
        for line in msg.lines() {
            output.push_str(&format!("    {}\n", line));
        }
    }
}

/// Renders a simple diffstat summary.
async fn show_diffstat(commit: &Commit, paths: Vec<PathBuf>) -> CliResult<String> {
    let changed_files = get_changed_files_for_commit(commit, &paths).await?;

    if changed_files.is_empty() {
        return Ok(String::new());
    }

    let mut output = String::from("\n");

    // Count summary totals while printing each changed path.
    let mut additions = 0;
    let mut deletions = 0;

    for change in &changed_files {
        match change.status {
            ChangeType::Added => additions += 1,
            ChangeType::Deleted => deletions += 1,
            ChangeType::Modified => {
                additions += 1;
                deletions += 1;
            }
        }
        let status = match change.status {
            ChangeType::Added => "A",
            ChangeType::Modified => "M",
            ChangeType::Deleted => "D",
        };
        output.push_str(&format!("{}  {}\n", status, change.path.display()));
    }

    output.push_str(&format!(
        "\n{} file{} changed, {} insertion{}(+), {} deletion{}(-)",
        changed_files.len(),
        if changed_files.len() != 1 { "s" } else { "" },
        additions,
        if additions != 1 { "s" } else { "" },
        deletions,
        if deletions != 1 { "s" } else { "" }
    ));
    output.push('\n');
    Ok(output)
}

fn show_bad_revision_error(object_ref: &str) -> CliError {
    ShowError::BadRevision {
        revision: object_ref.to_string(),
    }
    .into()
}

fn show_path_not_found_error(path: &str, revision: &str) -> CliError {
    ShowError::PathNotFound {
        path: path.to_string(),
        revision: revision.to_string(),
    }
    .into()
}

fn show_object_load_error(object_id: impl ToString, detail: impl ToString) -> CliError {
    ShowError::ObjectLoad {
        object_id: object_id.to_string(),
        detail: detail.to_string(),
    }
    .into()
}

fn show_unsupported_object_type_error(object_type: impl Into<String>) -> CliError {
    ShowError::UnsupportedObjectType {
        object_type: object_type.into(),
    }
    .into()
}

async fn run_show(args: &ShowArgs) -> CliResult<ShowOutput> {
    let object_ref = args.object.as_deref().unwrap_or("HEAD");
    let paths: Vec<PathBuf> = args.pathspec.iter().map(util::to_workdir_path).collect();

    if let Some((rev, path)) = object_ref.split_once(':') {
        return collect_commit_file_output(rev, path).await;
    }

    // Raw object IDs should keep their native schema, including annotated tag
    // objects, but hash-like ref names must still fall back to ref resolution.
    if let Some(hash) = resolve_existing_object_hash(object_ref) {
        return collect_object_output(&hash, &paths).await;
    }

    if let Ok(commit_hash) = util::get_commit_base(object_ref).await {
        match tag::find_tag_and_commit(object_ref).await {
            Ok(Some((object, _))) if object.get_type() == ObjectType::Tag => {
                let tag_hash = if let tag::TagObject::Tag(tag_obj) = &object {
                    tag_obj.id
                } else {
                    commit_hash
                };
                return collect_tag_output(&tag_hash, &paths).await;
            }
            _ => {
                return collect_commit_output(&commit_hash, &paths).await;
            }
        }
    }

    Err(show_bad_revision_error(object_ref))
}

async fn collect_object_output(hash: &ObjectHash, paths: &[PathBuf]) -> CliResult<ShowOutput> {
    let storage = ClientStorage::init(path::objects());
    let obj_type = storage
        .get_object_type(hash)
        .map_err(|e| show_object_load_error(hash, e))?;

    match obj_type {
        ObjectType::Commit => collect_commit_output(hash, paths).await,
        ObjectType::Tag => collect_tag_output(hash, paths).await,
        ObjectType::Tree => collect_tree_output(hash).await,
        ObjectType::Blob => collect_blob_output(hash).await,
        _ => Err(show_unsupported_object_type_error(format!("{obj_type:?}"))),
    }
}

fn resolve_existing_object_hash(object_ref: &str) -> Option<ObjectHash> {
    let hash = ObjectHash::from_str(object_ref).ok()?;
    let storage = ClientStorage::init(path::objects());
    storage.exist(&hash).then_some(hash)
}

async fn collect_commit_output(
    commit_hash: &ObjectHash,
    paths: &[PathBuf],
) -> CliResult<ShowOutput> {
    let commit =
        load_object::<Commit>(commit_hash).map_err(|e| show_object_load_error(commit_hash, e))?;
    let (subject, body) = split_subject_and_body(&commit.message);
    let files = get_changed_files_for_commit(&commit, paths).await?;

    Ok(ShowOutput::Commit(ShowCommitData {
        hash: commit.id.to_string(),
        short_hash: commit.id.to_string()[..7].to_string(),
        author_name: commit.author.name.trim().to_string(),
        author_email: commit.author.email.trim().to_string(),
        author_date: format_timestamp(commit.author.timestamp as i64),
        committer_name: commit.committer.name.trim().to_string(),
        committer_email: commit.committer.email.trim().to_string(),
        committer_date: format_timestamp(commit.committer.timestamp as i64),
        subject,
        body,
        parents: commit
            .parent_commit_ids
            .iter()
            .map(ToString::to_string)
            .collect(),
        refs: collect_reference_names(commit.id).await,
        files: files
            .into_iter()
            .map(|file| ShowFileChange {
                path: file.path.display().to_string(),
                status: change_type_name(file.status).to_string(),
            })
            .collect(),
    }))
}

async fn collect_tag_output(hash: &ObjectHash, paths: &[PathBuf]) -> CliResult<ShowOutput> {
    match tag::load_object_trait(hash).await {
        Ok(tag::TagObject::Tag(tag_obj)) => {
            // Validate the target object is accessible so that quiet / JSON
            // paths fail consistently with the human path, which dereferences
            // the tagged object via show_object_by_hash().
            let storage = ClientStorage::init(path::objects());
            let _ = storage
                .get_object_type(&tag_obj.object_hash)
                .map_err(|e| show_object_load_error(tag_obj.object_hash, e))?;

            Ok(ShowOutput::Tag(ShowTagData {
                tag_name: tag_obj.tag_name,
                tagger_name: Some(tag_obj.tagger.name.trim().to_string()),
                tagger_email: Some(tag_obj.tagger.email.trim().to_string()),
                tagger_date: chrono::DateTime::from_timestamp(tag_obj.tagger.timestamp as i64, 0)
                    .map(|date| date.to_rfc3339()),
                message: tag_obj.message.trim().to_string(),
                target_hash: tag_obj.object_hash.to_string(),
                target_type: format!("{:?}", tag_obj.object_type).to_lowercase(),
            }))
        }
        Ok(tag::TagObject::Commit(commit)) => collect_commit_output(&commit.id, paths).await,
        Ok(_) => Err(show_unsupported_object_type_error("tag target")),
        Err(e) => Err(show_object_load_error(hash, e)),
    }
}

async fn collect_tree_output(hash: &ObjectHash) -> CliResult<ShowOutput> {
    let tree = load_object::<Tree>(hash).map_err(|e| show_object_load_error(hash, e))?;

    Ok(ShowOutput::Tree(ShowTreeData {
        entries: tree
            .tree_items
            .iter()
            .map(|item| ShowTreeEntry {
                mode: format!("{:06o}", tree_item_mode_to_u32(item.mode)),
                object_type: tree_item_mode_to_object_type(item.mode).to_string(),
                hash: item.id.to_string(),
                name: item.name.clone(),
            })
            .collect(),
    }))
}

async fn collect_blob_output(hash: &ObjectHash) -> CliResult<ShowOutput> {
    let blob = load_object::<Blob>(hash).map_err(|e| show_object_load_error(hash, e))?;
    let content = String::from_utf8(blob.data.clone()).ok();

    Ok(ShowOutput::Blob(ShowBlobData {
        hash: hash.to_string(),
        size: blob.data.len(),
        is_binary: content.is_none(),
        content,
    }))
}

async fn collect_commit_file_output(rev: &str, file_path: &str) -> CliResult<ShowOutput> {
    let commit_hash = util::get_commit_base(rev)
        .await
        .map_err(|_| show_bad_revision_error(rev))?;
    let commit =
        load_object::<Commit>(&commit_hash).map_err(|e| show_object_load_error(commit_hash, e))?;
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| show_object_load_error(commit.tree_id, e))?;
    let items = tree.get_plain_items();
    let target_path = PathBuf::from(file_path);

    if let Some((_, blob_hash)) = items.iter().find(|(path, _)| path == &target_path) {
        collect_blob_output(blob_hash).await
    } else {
        Err(show_path_not_found_error(file_path, rev))
    }
}

fn split_subject_and_body(message: &str) -> (String, String) {
    let trimmed = parse_commit_msg(message).0.trim_end_matches('\n');
    match trimmed.split_once('\n') {
        Some((subject, body)) => (
            subject.to_string(),
            body.trim_start_matches('\n').to_string(),
        ),
        None => (trimmed.to_string(), String::new()),
    }
}

fn format_timestamp(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.to_rfc3339())
        .unwrap_or_else(|| timestamp.to_string())
}

fn change_type_name(change: ChangeType) -> &'static str {
    match change {
        ChangeType::Added => "added",
        ChangeType::Modified => "modified",
        ChangeType::Deleted => "deleted",
    }
}

fn tree_item_mode_to_u32(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Tree => 0o040000,
        TreeItemMode::Commit => 0o160000,
    }
}

fn tree_item_mode_to_object_type(mode: TreeItemMode) -> &'static str {
    match mode {
        TreeItemMode::Blob | TreeItemMode::BlobExecutable | TreeItemMode::Link => "blob",
        TreeItemMode::Tree => "tree",
        TreeItemMode::Commit => "commit",
    }
}

async fn collect_reference_names(commit_id: ObjectHash) -> Vec<String> {
    let mut refs = Vec::new();
    let mut head_branch = None;
    let mut include_head_ref = false;

    match Head::current_commit_result().await {
        Ok(Some(head_commit)) if head_commit == commit_id => {
            include_head_ref = true;
            match Head::current_result().await {
                Ok(Head::Branch(name)) => head_branch = Some(name),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error = %error,
                    "failed to resolve HEAD while collecting show JSON refs"
                ),
            }
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(
            error = %error,
            "failed to resolve HEAD commit while collecting show JSON refs"
        ),
    }

    if include_head_ref {
        if let Some(branch) = &head_branch {
            refs.push(format!("HEAD -> {branch}"));
        } else {
            refs.push("HEAD".to_string());
        }
    }

    for branch in Branch::list_branches_best_effort(None).await {
        if branch.commit != commit_id {
            continue;
        }
        if head_branch.as_deref() == Some(branch.name.as_str()) {
            continue;
        }
        refs.push(branch.name);
    }

    match tag::list().await {
        Ok(tags) => {
            for tag in tags {
                let tagged_commit = match tag.object {
                    tag::TagObject::Commit(commit) => Some(commit.id),
                    tag::TagObject::Tag(tag_object) => Some(tag_object.object_hash),
                    _ => None,
                };
                if tagged_commit == Some(commit_id) {
                    refs.push(format!("tag: {}", tag.name));
                }
            }
        }
        Err(err) => tracing::warn!("failed to collect tag refs for show JSON output: {err}"),
    };

    refs.sort();
    refs.dedup();
    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error::StableErrorCode;

    #[test]
    fn build_raw_lines_reports_adds_deletes_and_mode_only_changes() {
        let entry = |mode: &str, sha: &str| (mode.to_string(), sha.to_string());
        let mut old = BTreeMap::new();
        old.insert(PathBuf::from("f"), entry("100644", "587be6b"));
        old.insert(PathBuf::from("gone"), entry("100644", "1111111"));
        let mut new = BTreeMap::new();
        // `f` keeps its blob id but flips to executable: a mode-only change that
        // a sha-only diff would miss, but Git's --raw reports as `M`.
        new.insert(PathBuf::from("f"), entry("100755", "587be6b"));
        new.insert(PathBuf::from("added"), entry("100644", "2222222"));

        let out = build_raw_lines(&old, &new, &[]);
        assert!(
            out.contains(":000000 100644 0000000 2222222 A\tadded\n"),
            "added: {out}"
        );
        assert!(
            out.contains(":100644 100755 587be6b 587be6b M\tf\n"),
            "mode-only change must be reported as M: {out}"
        );
        assert!(
            out.contains(":100644 000000 1111111 0000000 D\tgone\n"),
            "deleted: {out}"
        );
        // Path order (BTreeSet): added, f, gone.
        let order: Vec<&str> = out
            .lines()
            .map(|l| l.rsplit('\t').next().unwrap())
            .collect();
        assert_eq!(order, vec!["added", "f", "gone"]);

        // An identical (mode, id) pair on both sides emits nothing.
        let same = old.clone();
        assert_eq!(build_raw_lines(&same, &same, &[]), "");

        // A pathspec filter keeps only matching paths.
        let filtered = build_raw_lines(&old, &new, &[PathBuf::from("added")]);
        assert!(filtered.contains("A\tadded"));
        assert!(!filtered.contains("\tf\n"), "filtered out f: {filtered}");
    }

    #[test]
    fn show_error_to_cli_error_pins_stable_codes_and_hints() {
        let cases = [
            (
                ShowError::NotInRepo,
                StableErrorCode::RepoNotFound,
                "not a libra repository (or any of the parent directories): .libra",
                "run 'libra init' to create a repository",
            ),
            (
                ShowError::BadRevision {
                    revision: "missing".to_string(),
                },
                StableErrorCode::CliInvalidTarget,
                "bad revision 'missing'",
                "use 'libra log --oneline'",
            ),
            (
                ShowError::PathNotFound {
                    path: "src/missing.rs".to_string(),
                    revision: "HEAD".to_string(),
                },
                StableErrorCode::CliInvalidTarget,
                "path 'src/missing.rs' does not exist in 'HEAD'",
                "use 'libra show <rev>:'",
            ),
            (
                ShowError::ObjectLoad {
                    object_id: "abc123".to_string(),
                    detail: "missing object".to_string(),
                },
                StableErrorCode::RepoCorrupt,
                "failed to load object 'abc123': missing object",
                "the object store may be corrupted",
            ),
            (
                ShowError::UnsupportedObjectType {
                    object_type: "ofs-delta".to_string(),
                },
                StableErrorCode::CliInvalidTarget,
                "unsupported object type for display: ofs-delta",
                "",
            ),
        ];

        for (show_error, stable_code, message, hint) in cases {
            let cli_error: CliError = show_error.into();
            assert_eq!(cli_error.stable_code(), stable_code);
            assert_eq!(cli_error.message(), message);
            if !hint.is_empty() {
                assert!(
                    cli_error
                        .hints()
                        .iter()
                        .any(|actual| actual.as_str().contains(hint)),
                    "expected hint containing {hint:?}, got {:?}",
                    cli_error.hints()
                );
            }
        }
    }

    #[test]
    fn test_args_parsing() {
        // Default object is `HEAD`.
        let args = ShowArgs::try_parse_from(["show"]).unwrap();
        assert_eq!(args.object, None);
        assert!(!args.no_patch);
        assert!(!args.oneline);

        // Explicit object argument.
        let args = ShowArgs::try_parse_from(["show", "abc123"]).unwrap();
        assert_eq!(args.object, Some("abc123".to_string()));

        // `--no-patch` flag.
        let args = ShowArgs::try_parse_from(["show", "--no-patch"]).unwrap();
        assert!(args.no_patch);

        // `--oneline` flag.
        let args = ShowArgs::try_parse_from(["show", "--oneline"]).unwrap();
        assert!(args.oneline);

        // `--name-only` flag.
        let args = ShowArgs::try_parse_from(["show", "--name-only"]).unwrap();
        assert!(args.name_only);

        // `--stat` flag.
        let args = ShowArgs::try_parse_from(["show", "--stat"]).unwrap();
        assert!(args.stat);

        // `--summary` flag.
        let args = ShowArgs::try_parse_from(["show", "--summary"]).unwrap();
        assert!(args.summary);

        // `<revision>:<path>` syntax.
        let args = ShowArgs::try_parse_from(["show", "HEAD:test.txt"]).unwrap();
        assert_eq!(args.object, Some("HEAD:test.txt".to_string()));
    }

    #[test]
    fn format_show_summary_reports_only_created_and_deleted_files() {
        // A text create (whose path contains " b/", proving the path comes from
        // the reconstructed header, not a naive split), a BINARY create with no
        // `---`/`+++` lines (proving the mode line alone drives emission and a
        // stale path cannot leak into the next block), a delete, and a modified
        // file (omitted). Each emitted line carries the file's mode.
        let diff = "\
diff --git a/has b/space.txt b/has b/space.txt
new file mode 100644
index 0000000..89b24ec
--- /dev/null
+++ b/has b/space.txt
@@ -0,0 +1 @@
+hello
diff --git a/logo.png b/logo.png
new file mode 100755
index 0000000..0a1b2c3
Binary files /dev/null and b/logo.png differ
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 89b24ec..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/edited.txt b/edited.txt
index 1111111..2222222 100644
--- a/edited.txt
+++ b/edited.txt
@@ -1 +1 @@
-old
+new
";
        assert_eq!(
            super::format_show_summary(diff),
            " create mode 100644 has b/space.txt\n create mode 100755 logo.png\n delete mode 100644 gone.txt"
        );
        // No created or deleted files → empty summary.
        assert_eq!(
            super::format_show_summary("diff --git a/x b/x\nindex 1..2 100644\n"),
            ""
        );
        // Header path reconstruction: identical a/b sides (incl. " b/" paths)
        // recover the path; a rename (a != b) is declined.
        assert_eq!(
            super::reconstruct_identical_diff_path("dir/f.txt b/dir/f.txt"),
            Some("dir/f.txt".to_string())
        );
        assert_eq!(
            super::reconstruct_identical_diff_path("has b/x.txt b/has b/x.txt"),
            Some("has b/x.txt".to_string())
        );
        assert_eq!(
            super::reconstruct_identical_diff_path("old.txt b/new.txt"),
            None
        );
    }

    #[test]
    fn test_tree_item_mode_helpers_use_git_modes_and_types() {
        assert_eq!(tree_item_mode_to_u32(TreeItemMode::Blob), 0o100644);
        assert_eq!(
            tree_item_mode_to_u32(TreeItemMode::BlobExecutable),
            0o100755
        );
        assert_eq!(tree_item_mode_to_u32(TreeItemMode::Link), 0o120000);
        assert_eq!(tree_item_mode_to_u32(TreeItemMode::Tree), 0o040000);
        assert_eq!(tree_item_mode_to_u32(TreeItemMode::Commit), 0o160000);

        assert_eq!(tree_item_mode_to_object_type(TreeItemMode::Blob), "blob");
        assert_eq!(
            tree_item_mode_to_object_type(TreeItemMode::BlobExecutable),
            "blob"
        );
        assert_eq!(tree_item_mode_to_object_type(TreeItemMode::Link), "blob");
        assert_eq!(tree_item_mode_to_object_type(TreeItemMode::Tree), "tree");
        assert_eq!(
            tree_item_mode_to_object_type(TreeItemMode::Commit),
            "commit"
        );
    }
}
