//! Implements `for-each-ref` to enumerate refs with filtering and formatting.

use std::{collections::HashMap, io::IsTerminal, str::FromStr};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{commit::Commit, tag::Tag as GitTag, types::ObjectType},
};
use serde::Serialize;

use crate::{
    command::load_object,
    common_utils::parse_commit_msg,
    internal::{
        branch::Branch, config::ConfigKv, head::Head, log::formatter::format_timestamp_with, tag,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        text, util,
    },
};

/// `--help` examples for for-each-ref
pub const FOR_EACH_REF_EXAMPLES: &str = "\
EXAMPLES:
    libra for-each-ref                  List all refs with commit info
    libra for-each-ref --heads          List only branches (refs/heads/)
    libra for-each-ref --tags           List only tags (refs/tags/)
    libra for-each-ref --remotes        List only remote-tracking refs
    libra for-each-ref --all            List all refs (default)
    libra for-each-ref --format='%(refname) %(objectname)'  Custom format
    libra for-each-ref --format='%(refname:short) %(objectname:short)'  Short ref/object forms
    libra for-each-ref --sort=refname   Sort by ref name
    libra for-each-ref --sort=version:refname   Version-aware sort (v1.9 before v1.10)
    libra for-each-ref --sort=-committerdate    Most recently committed refs first
    libra for-each-ref --format='%(refname:short) %(committerdate:relative)'  Date in a chosen format (iso/short/unix/relative/...)
    libra --color=always for-each-ref --format='%(color:green)%(refname:short)%(color:reset)'  Colorize output with ANSI escapes
    libra for-each-ref --sort=objectsize --format='%(objectsize) %(refname)'  Sort by object size
    libra for-each-ref --tags --format='%(refname:short) -> %(*objectname:short)'  Show each tag's dereferenced target
    libra for-each-ref --tags --format='%(refname:short) %(*objecttype) %(*objectsize)'  Dereferenced target type and size
    libra for-each-ref --tags --sort=*objectsize  Sort tags by their dereferenced target's size
    libra for-each-ref --format='%(align:20,left)%(refname:short)%(end)%(objectname:short)'  Pad the ref name to a 20-column field
    libra for-each-ref --format='%(refname:short)%(if)%(HEAD)%(then) (current)%(end)'  Mark the checked-out branch conditionally
    libra for-each-ref --format='%(refname:short) %(describe)'  Describe each ref's tip relative to the nearest tag
    libra for-each-ref --shell --format='%(refname)'  Shell-quote each field for eval
    libra for-each-ref --points-at HEAD List refs that point at HEAD
    libra for-each-ref --merged=main    List refs already merged into main
    libra for-each-ref --no-merged=main List refs not yet merged into main
    libra for-each-ref --exclude=wip refs/heads/  Skip refs whose name matches the pattern
    libra for-each-ref --count=10       Limit output to 10 refs";

#[derive(Parser, Debug)]
#[command(after_help = FOR_EACH_REF_EXAMPLES)]
pub struct ForEachRefArgs {
    /// Show only branches (refs/heads/)
    #[clap(long)]
    pub heads: bool,

    /// Show only tags (refs/tags/)
    #[clap(long)]
    pub tags: bool,

    /// Show only remote-tracking refs (refs/remotes/)
    #[clap(long)]
    pub remotes: bool,

    /// Show all refs (default)
    #[clap(long)]
    pub all: bool,

    /// Custom output format with %(atoms)
    #[clap(long, value_name = "FORMAT")]
    pub format: Option<String>,

    /// Sort output by key: `refname`, `objectname`, `version:refname`
    /// (alias `v:refname`), `objectsize`, the dereference keys `*objectname` /
    /// `*objecttype` / `*objectsize` (an annotated tag's target), or the date
    /// keys `committerdate` / `authordate` / `creatordate`; prefix with `-` to
    /// reverse.
    #[clap(long, value_name = "KEY")]
    pub sort: Option<String>,

    /// Limit output to N refs
    #[clap(long, value_name = "COUNT")]
    pub count: Option<usize>,

    /// Show only refs that point at OBJECT
    #[clap(long, value_name = "OBJECT")]
    pub points_at: Option<String>,

    /// Show only refs whose commit contains COMMIT (i.e. COMMIT is an ancestor).
    #[clap(long, value_name = "COMMIT")]
    pub contains: Option<String>,

    /// Show only refs whose commit does NOT contain COMMIT.
    #[clap(long = "no-contains", value_name = "COMMIT")]
    pub no_contains: Option<String>,

    /// Show only refs whose commit is merged into COMMIT (reachable from COMMIT).
    #[clap(long, value_name = "COMMIT")]
    pub merged: Option<String>,

    /// Show only refs whose commit is NOT merged into COMMIT.
    #[clap(long = "no-merged", value_name = "COMMIT")]
    pub no_merged: Option<String>,

    /// Do not list refs matching PATTERN (repeatable; applied after the
    /// positional include patterns).
    #[clap(long = "exclude", value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Quote each interpolated field for `eval` in `sh` (single-quote escaping).
    #[clap(long = "shell", conflicts_with_all = ["perl", "python", "tcl"])]
    pub shell: bool,

    /// Quote each interpolated field as a Perl string literal.
    #[clap(long = "perl", conflicts_with_all = ["python", "tcl"])]
    pub perl: bool,

    /// Quote each interpolated field as a Python string literal.
    #[clap(long = "python", conflicts_with_all = ["tcl"])]
    pub python: bool,

    /// Quote each interpolated field as a Tcl string literal.
    #[clap(long = "tcl")]
    pub tcl: bool,

    /// Refname patterns to match
    #[clap(value_name = "PATTERN")]
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefEntry {
    refname: String,
    objectname: String,
    objecttype: String,
    /// For a symbolic ref (e.g. `refs/remotes/<remote>/HEAD`), the full name of
    /// the ref it points to; `None` for ordinary refs. Drives `%(symref)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    symref: Option<String>,
    #[serde(skip_serializing)]
    points_at: Vec<String>,
    /// Precomputed `%(describe[:opts])` values, keyed by the atom's option string
    /// (`""` for a bare `%(describe)`, e.g. `"tags"`/`"abbrev=4"` otherwise). Filled
    /// by an async pass before rendering because `git describe` is async and the
    /// render pipeline is synchronous; absent from the `--json` schema.
    #[serde(skip)]
    describe: HashMap<String, String>,
}

pub async fn execute(args: ForEachRefArgs) -> CliResult<()> {
    execute_safe(args, &OutputConfig::default()).await
}

pub async fn execute_safe(args: ForEachRefArgs, output: &OutputConfig) -> CliResult<()> {
    let mut result = run_for_each_ref(&args).await?;
    // Precompute `%(describe[:opts])` values (async) before the sync render pass.
    // Only for the human render path: `--json` ignores `--format` entirely (and the
    // describe cache is `#[serde(skip)]`), and `--quiet` emits nothing — so neither
    // should pay for, or be failed by, describe computation/validation.
    if let Some(format) = &args.format
        && !output.is_json()
        && !output.quiet
    {
        populate_describe_cache(&mut result, format).await?;
    }
    // Resolve the current HEAD branch so `%(HEAD)` can mark it with `*`.
    let head_refname = match Head::current().await {
        Head::Branch(name) => Some(format!("refs/heads/{name}")),
        Head::Detached(_) => None,
    };
    // Resolve each branch's upstream tracking ref for `%(upstream)`.
    let upstreams = resolve_upstreams(&result).await;
    // Resolve each branch's push tracking ref for `%(push)`.
    let pushes = resolve_pushes(&result).await;
    render_output(
        &result,
        &args,
        output,
        head_refname.as_deref(),
        &upstreams,
        &pushes,
    )?;
    Ok(())
}

/// Map each `refs/heads/<branch>` entry to its upstream tracking ref
/// (`refs/remotes/<remote>/<branch>`), computed from `branch.<name>.remote` and
/// `branch.<name>.merge`. Branches without a configured upstream are omitted.
/// This is the standard tracking computation (default fetch refspec); custom
/// refspec mappings are not modeled.
async fn resolve_upstreams(entries: &[RefEntry]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for entry in entries {
        let Some(branch) = entry.refname.strip_prefix("refs/heads/") else {
            continue;
        };
        let remote = ConfigKv::get(&format!("branch.{branch}.remote"))
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        let merge = ConfigKv::get(&format!("branch.{branch}.merge"))
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        if let (Some(remote), Some(merge)) = (remote, merge) {
            let merge_short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
            map.insert(
                entry.refname.clone(),
                format!("refs/remotes/{remote}/{merge_short}"),
            );
        }
    }
    map
}

/// Resolve a Git config variable case-insensitively in its variable name (the
/// segment after `prefix`) — Git config variable names are case-insensitive, so
/// both the documented camelCase spelling (e.g. `pushRemote`) and the lowercase
/// form emitted by `git config --list` / imports (`pushremote`) resolve to the
/// same logical variable. See [`ConfigKv::get_var_case_insensitive`] for the
/// single-row-vs-anomaly semantics.
async fn config_var(prefix: &str, variable: &str) -> Option<String> {
    ConfigKv::get_var_case_insensitive(prefix, variable)
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value)
}

/// Map each `refs/heads/<branch>` entry to its push tracking ref for `%(push)`.
/// The push remote follows Git's precedence — `branch.<name>.pushRemote`, then
/// `remote.pushDefault`, then `branch.<name>.remote` — combined with
/// `branch.<name>.merge` to form `refs/remotes/<push-remote>/<branch>`. Like
/// `resolve_upstreams`, this is a config-derived computation (the standard refspec);
/// it does not check that the tracking ref exists and does not model custom refspecs.
async fn resolve_pushes(entries: &[RefEntry]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let push_default = config_var("remote.", "pushDefault").await;
    for entry in entries {
        let Some(branch) = entry.refname.strip_prefix("refs/heads/") else {
            continue;
        };
        let mut push_remote = config_var(&format!("branch.{branch}."), "pushRemote")
            .await
            .or_else(|| push_default.clone());
        if push_remote.is_none() {
            push_remote = ConfigKv::get(&format!("branch.{branch}.remote"))
                .await
                .ok()
                .flatten()
                .map(|e| e.value);
        }
        let merge = ConfigKv::get(&format!("branch.{branch}.merge"))
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        if let (Some(push_remote), Some(merge)) = (push_remote, merge) {
            let merge_short = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
            map.insert(
                entry.refname.clone(),
                format!("refs/remotes/{push_remote}/{merge_short}"),
            );
        }
    }
    map
}

async fn run_for_each_ref(_args: &ForEachRefArgs) -> CliResult<Vec<RefEntry>> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let show_all = _args.all || (!_args.heads && !_args.tags && !_args.remotes);
    let mut entries = Vec::new();

    if show_all || _args.heads {
        let branches = Branch::list_branches_result(None)
            .await
            .map_err(branch_error)?;
        for branch in branches {
            entries.push(direct_ref_entry(
                format!("refs/heads/{}", branch.name),
                branch.commit.to_string(),
                "commit",
            ));
        }
    }

    if show_all || _args.remotes {
        let remotes = ConfigKv::all_remote_configs().await.map_err(|source| {
            CliError::fatal(format!("failed to list remotes: {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for remote in remotes {
            let branches = Branch::list_branches_result(Some(&remote.name))
                .await
                .map_err(branch_error)?;
            for branch in &branches {
                let refname = if branch.name.starts_with("refs/remotes/") {
                    branch.name.clone()
                } else {
                    format!("refs/remotes/{}/{}", remote.name, branch.name)
                };
                entries.push(direct_ref_entry(
                    refname,
                    branch.commit.to_string(),
                    "commit",
                ));
            }

            // A configured remote HEAD (`refs/remotes/<remote>/HEAD`, e.g. via
            // `remote set-head`) is a symbolic ref. Git lists it with the object
            // its target resolves to and exposes the target via `%(symref)`.
            if let Some(Head::Branch(target_branch)) = Head::remote_current(&remote.name).await {
                let target = if target_branch.starts_with("refs/") {
                    target_branch
                } else {
                    format!("refs/remotes/{}/{}", remote.name, target_branch)
                };
                // Resolve the target's object from the branches just enumerated;
                // skip a dangling HEAD whose target ref does not exist.
                let target_short = target
                    .strip_prefix(&format!("refs/remotes/{}/", remote.name))
                    .unwrap_or(&target);
                if let Some(b) = branches.iter().find(|b| {
                    b.name == target_short
                        || b.name == target
                        || format!("refs/remotes/{}/{}", remote.name, b.name) == target
                }) {
                    entries.push(symbolic_ref_entry(
                        format!("refs/remotes/{}/HEAD", remote.name),
                        target,
                        b.commit.to_string(),
                    ));
                }
            }
        }
    }

    if show_all || _args.tags {
        let tags = tag::list().await.map_err(|source| {
            CliError::fatal(format!("failed to list tags: {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for t in tags {
            entries.push(tag_ref_entry(&t));
        }
    }

    if let Some(object_ref) = _args.points_at.as_deref() {
        let target = resolve_points_at_target(object_ref).await?;
        entries.retain(|entry| entry.points_at.iter().any(|hash| hash == &target));
    }

    if let Some(commit_ref) = _args.contains.as_deref() {
        let target = resolve_commit_target(commit_ref).await?;
        entries = retain_refs_containing(entries, &target, true).await?;
    }
    if let Some(commit_ref) = _args.no_contains.as_deref() {
        let target = resolve_commit_target(commit_ref).await?;
        entries = retain_refs_containing(entries, &target, false).await?;
    }

    if let Some(commit_ref) = _args.merged.as_deref() {
        let target = resolve_commit_target(commit_ref).await?;
        entries = retain_refs_merged_into(entries, &target, true).await?;
    }
    if let Some(commit_ref) = _args.no_merged.as_deref() {
        let target = resolve_commit_target(commit_ref).await?;
        entries = retain_refs_merged_into(entries, &target, false).await?;
    }

    if !_args.patterns.is_empty() {
        entries.retain(|entry| {
            _args
                .patterns
                .iter()
                .any(|pattern| matches_ref_pattern(&entry.refname, pattern))
        });
    }

    // `--exclude <pattern>` drops refs matching any exclude pattern (applied
    // after the include patterns, matching Git).
    if !_args.exclude.is_empty() {
        entries.retain(|entry| {
            !_args
                .exclude
                .iter()
                .any(|pattern| matches_ref_pattern(&entry.refname, pattern))
        });
    }

    // Date-based sort keys (`committerdate` / `authordate` / `creatordate`)
    // resolve each ref's timestamp by loading its object (peeling tags), so they
    // are handled separately; all other keys go through the plain key sorter.
    // Date and object-size keys require loading each ref's object, so they are
    // handled separately; all other keys go through the plain key sorter.
    let sort = _args.sort.as_deref();
    if let Some((date_key, reverse)) = sort.and_then(parse_date_sort_key) {
        sort_entries_by_date(&mut entries, date_key, reverse);
    } else if let Some(reverse) = sort.and_then(parse_objectsize_sort_key) {
        sort_entries_by_objectsize(&mut entries, reverse)?;
    } else if let Some(reverse) = sort.and_then(parse_deref_objectname_sort_key) {
        sort_entries_by_deref_objectname(&mut entries, reverse)?;
    } else if let Some(reverse) = sort.and_then(parse_deref_objecttype_sort_key) {
        sort_entries_by_deref_objecttype(&mut entries, reverse)?;
    } else if let Some(reverse) = sort.and_then(parse_deref_objectsize_sort_key) {
        sort_entries_by_deref_objectsize(&mut entries, reverse)?;
    } else {
        sort_entries(&mut entries, sort)?;
    }
    if let Some(count) = _args.count {
        entries.truncate(count);
    }
    Ok(entries)
}

/// Keep (or, when `want` is false, drop) refs whose commit has `target` as an
/// ancestor — i.e. the ref "contains" `target` (`--contains`/`--no-contains`).
/// A ref's commit is its peeled commit id (see [`peeled_commit`]); reachability
/// reuses `log::get_reachable_commits`, so this walks history once per ref.
async fn retain_refs_containing(
    entries: Vec<RefEntry>,
    target: &str,
    want: bool,
) -> CliResult<Vec<RefEntry>> {
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        let contains = match peeled_commit(&entry) {
            Some(commit) => crate::command::log::get_reachable_commits(commit.clone(), None)
                .await?
                .iter()
                .any(|reachable| reachable.id.to_string().as_str() == target),
            None => false,
        };
        if contains == want {
            kept.push(entry);
        }
    }
    Ok(kept)
}

/// Keep (or, when `want` is false, drop) refs whose commit is reachable from
/// `target` — i.e. the ref is already merged into `target`
/// (`--merged`/`--no-merged`). Unlike [`retain_refs_containing`], the set of
/// commits reachable from `target` is computed once and each ref's first peeled
/// commit is tested for membership.
async fn retain_refs_merged_into(
    entries: Vec<RefEntry>,
    target: &str,
    want: bool,
) -> CliResult<Vec<RefEntry>> {
    let reachable: std::collections::HashSet<String> =
        crate::command::log::get_reachable_commits(target.to_string(), None)
            .await?
            .iter()
            .map(|commit| commit.id.to_string())
            .collect();

    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        let merged = match peeled_commit(&entry) {
            Some(commit) => reachable.contains(commit),
            None => false,
        };
        if merged == want {
            kept.push(entry);
        }
    }
    Ok(kept)
}

/// The commit id a ref ultimately resolves to for reachability filters
/// (`--contains` / `--merged`). Direct refs and lightweight tags carry a single
/// id; annotated tags carry `[tag_id, peeled_target]`, so the peeled target (the
/// last element) is the commit to test. Returns `None` for refs that peel to a
/// non-commit object (tree/blob), which never satisfy a commit-reachability
/// filter.
fn peeled_commit(entry: &RefEntry) -> Option<&String> {
    entry.points_at.last()
}

fn direct_ref_entry(refname: String, objectname: String, objecttype: &str) -> RefEntry {
    RefEntry {
        refname,
        points_at: vec![objectname.clone()],
        objectname,
        objecttype: objecttype.to_string(),
        symref: None,
        describe: HashMap::new(),
    }
}

/// A symbolic ref entry (e.g. `refs/remotes/<remote>/HEAD`): its object is the
/// commit the target resolves to, and `symref` records the target ref name.
fn symbolic_ref_entry(refname: String, target: String, objectname: String) -> RefEntry {
    RefEntry {
        refname,
        points_at: vec![objectname.clone()],
        objectname,
        objecttype: "commit".to_string(),
        symref: Some(target),
        describe: HashMap::new(),
    }
}

fn tag_ref_entry(tag: &tag::Tag) -> RefEntry {
    let (objectname, objecttype, points_at) = tag_object_info(&tag.object);
    RefEntry {
        refname: format!("refs/tags/{}", tag.name),
        objectname,
        objecttype,
        symref: None,
        points_at,
        describe: HashMap::new(),
    }
}

fn tag_object_info(object: &tag::TagObject) -> (String, String, Vec<String>) {
    match object {
        tag::TagObject::Commit(commit) => {
            let id = commit.id.to_string();
            (id.clone(), "commit".to_string(), vec![id])
        }
        tag::TagObject::Tag(tag) => (
            tag.id.to_string(),
            "tag".to_string(),
            vec![tag.id.to_string(), tag.object_hash.to_string()],
        ),
        tag::TagObject::Tree(tree) => {
            let id = tree.id.to_string();
            (id.clone(), "tree".to_string(), vec![id])
        }
        tag::TagObject::Blob(blob) => {
            let id = blob.id.to_string();
            (id.clone(), "blob".to_string(), vec![id])
        }
    }
}

/// Resolve the COMMIT argument of a reachability filter (`--contains` /
/// `--no-contains` / `--merged` / `--no-merged`) to a commit id, peeling
/// annotated tag names and tag objects to their underlying commit. Unlike
/// [`resolve_points_at_target`] — which keeps the raw tag object so
/// `--points-at` can match tag refs — this follows tags to a commit via
/// `util::get_commit_base`, so the reachability walk always starts from a
/// commit (matching Git's commit-ish resolution for these filters).
async fn resolve_commit_target(commit_ref: &str) -> CliResult<String> {
    // Fully-qualified refs name a namespace explicitly and must resolve only
    // within it — a same-named ref in another namespace must not shadow it.
    if let Some(tag_name) = commit_ref.strip_prefix("refs/tags/") {
        // Tag namespace: peel annotated tags to their commit.
        return match tag::find_tag_and_commit(tag_name).await {
            Ok(Some((_object, commit))) => Ok(commit.id.to_string()),
            Ok(None) => Err(invalid_object_name(commit_ref)),
            Err(source) => Err(CliError::fatal(format!(
                "failed to resolve tag '{commit_ref}': {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)),
        };
    }
    if let Some(branch_name) = commit_ref.strip_prefix("refs/heads/") {
        // Local-branch namespace: the branch store is keyed by short names, so
        // strip the prefix and look it up directly without falling back to tags.
        return match Branch::find_branch_result(branch_name, None).await {
            Ok(Some(branch)) => Ok(branch.commit.to_string()),
            Ok(None) => Err(invalid_object_name(commit_ref)),
            Err(source) => Err(branch_error(source)),
        };
    }
    if let Some(remote_path) = commit_ref.strip_prefix("refs/remotes/") {
        // Remote-tracking namespace: resolve only against remote-tracking
        // branches, trying each `<remote>/<branch>` split (multi-segment
        // remotes). All lookups are scoped to `Some(remote)`, with no
        // local-branch/tag/hash fallback — so a local branch literally named
        // `refs/remotes/<...>` cannot shadow the remote ref. Fetched refs are
        // stored under the full `refs/remotes/<remote>/<branch>` name (see
        // `fetch.rs`/`remote.rs`); an older/short form stores just the branch
        // name, so try the full ref first, then the short branch.
        for (remote, branch_name) in util::remote_tracking_candidates(remote_path) {
            for key in [commit_ref, branch_name] {
                match Branch::find_branch_result(key, Some(remote)).await {
                    Ok(Some(branch)) => return Ok(branch.commit.to_string()),
                    Ok(None) => {}
                    Err(source) => return Err(branch_error(source)),
                }
            }
        }
        return Err(invalid_object_name(commit_ref));
    }

    // Everything else — HEAD, short branch/tag/remote names, and commit hashes —
    // goes through the general commit-ish resolver, which peels annotated tags
    // and honors Git's resolution precedence for short names.
    match util::get_commit_base(commit_ref).await {
        Ok(hash) => Ok(hash.to_string()),
        Err(_) => Err(invalid_object_name(commit_ref)),
    }
}

/// Build the standard "Not a valid object name" error for an unresolvable
/// reachability target.
fn invalid_object_name(commit_ref: &str) -> CliError {
    CliError::fatal(format!("Not a valid object name {commit_ref}"))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

async fn resolve_points_at_target(object_ref: &str) -> CliResult<String> {
    let tag_name = object_ref.strip_prefix("refs/tags/").unwrap_or(object_ref);
    if let Some(tag_ref) = tag::find_tag_ref(tag_name).await.map_err(|source| {
        CliError::fatal(format!("failed to resolve tag '{object_ref}': {source}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        let target = tag_ref.target.ok_or_else(|| {
            CliError::fatal(format!("tag '{object_ref}' is missing target object"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        ObjectHash::from_str(&target).map_err(|source| {
            CliError::fatal(format!(
                "tag '{object_ref}' has invalid target object '{target}': {source}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        return Ok(target);
    }

    if let Ok(hash) = util::get_commit_base(object_ref).await {
        return Ok(hash.to_string());
    }
    if let Ok(hash) = ObjectHash::from_str(object_ref) {
        return Ok(hash.to_string());
    }

    Err(
        CliError::fatal(format!("Not a valid object name {object_ref}"))
            .with_stable_code(StableErrorCode::CliInvalidTarget),
    )
}

fn branch_error(source: crate::internal::branch::BranchStoreError) -> CliError {
    CliError::fatal(format!("failed to list branches: {source}"))
        .with_stable_code(StableErrorCode::IoReadFailed)
}

fn matches_ref_pattern(refname: &str, pattern: &str) -> bool {
    refname == pattern || refname.ends_with(pattern) || refname.contains(pattern)
}

/// A date-based `--sort` key. `committerdate`/`authordate` use the (peeled)
/// commit's committer/author date; `creatordate` uses the annotated tag's
/// tagger date, falling back to the commit's committer date for everything else
/// (commits and lightweight tags), matching Git.
#[derive(Clone, Copy)]
enum DateSortKey {
    Committer,
    Author,
    Creator,
}

/// Recognise a date-based sort key, returning the key and whether a leading `-`
/// requested a reversed (descending) order. Non-date keys return `None` so they
/// fall through to [`sort_entries`].
fn parse_date_sort_key(sort: &str) -> Option<(DateSortKey, bool)> {
    let (reverse, name) = match sort.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, sort),
    };
    let key = match name {
        "committerdate" => DateSortKey::Committer,
        "authordate" => DateSortKey::Author,
        "creatordate" => DateSortKey::Creator,
        _ => return None,
    };
    Some((key, reverse))
}

/// Recognise the `objectsize` sort key, returning whether a leading `-` requested
/// reversed (descending) order. Non-matching keys return `None`.
fn parse_objectsize_sort_key(sort: &str) -> Option<bool> {
    match sort.strip_prefix('-') {
        Some("objectsize") => Some(true),
        _ if sort == "objectsize" => Some(false),
        _ => None,
    }
}

/// The byte size of the object a ref points at directly (the tag object for an
/// annotated tag, the commit for a branch) — Git's `%(objectsize)`.
/// `ClientStorage::get` returns the decompressed object content, whose length is
/// the size Git reports. A missing/unreadable object is a real corruption and is
/// surfaced as an error (rather than silently reported as size 0).
fn ref_object_size(entry: &RefEntry) -> CliResult<i64> {
    let hash = ObjectHash::from_str(&entry.objectname).map_err(|source| {
        CliError::fatal(format!(
            "ref '{}' has an invalid object id '{}': {source}",
            entry.refname, entry.objectname
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let data = util::objects_storage().get(&hash).map_err(|source| {
        CliError::fatal(format!(
            "failed to read object {} for ref '{}': {source}",
            entry.objectname, entry.refname
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    Ok(data.len() as i64)
}

/// The raw, decompressed contents of the ref's object — Git's `%(raw)`. For a
/// branch this is the commit object's canonical text (`tree …`/`parent …`/
/// `author …`/`committer …`/blank/message), for an annotated tag the tag object.
/// Same byte source as [`ref_object_size`], so `%(raw:size)` == `%(objectsize)`.
///
/// The for-each-ref render pipeline is UTF-8 text based (like `%(contents)`), so
/// rather than lossily transcode binary content — which would silently corrupt
/// the output and make it disagree with `%(raw:size)` — a non-UTF-8 object is
/// rejected. Commit and (annotated) tag objects, the only objects a branch/tag
/// ref normally names, are text.
fn ref_raw_content(entry: &RefEntry) -> CliResult<String> {
    let hash = ObjectHash::from_str(&entry.objectname).map_err(|source| {
        CliError::fatal(format!(
            "ref '{}' has an invalid object id '{}': {source}",
            entry.refname, entry.objectname
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let data = util::objects_storage().get(&hash).map_err(|source| {
        CliError::fatal(format!(
            "failed to read object {} for ref '{}': {source}",
            entry.objectname, entry.refname
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    String::from_utf8(data).map_err(|_| {
        CliError::fatal(format!(
            "object {} for ref '{}' is not valid UTF-8; %(raw) is only supported for text objects",
            entry.objectname, entry.refname
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
    })
}

/// Sort entries by their object's byte size (`objectsize`); ties break by refname
/// ascending, matching Git's final ordering key.
fn sort_entries_by_objectsize(entries: &mut [RefEntry], reverse: bool) -> CliResult<()> {
    let mut sizes: Vec<i64> = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        sizes.push(ref_object_size(entry)?);
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        let primary = sizes[a].cmp(&sizes[b]);
        let primary = if reverse { primary.reverse() } else { primary };
        primary.then_with(|| entries[a].refname.cmp(&entries[b].refname))
    });
    let reordered: Vec<RefEntry> = order.into_iter().map(|i| entries[i].clone()).collect();
    entries.clone_from_slice(&reordered);
    Ok(())
}

/// Recognise the `*objectname` (dereferenced object name) sort key, returning
/// whether a leading `-` requested reversed order. Non-matching keys return
/// `None`.
fn parse_deref_objectname_sort_key(sort: &str) -> Option<bool> {
    match sort.strip_prefix('-') {
        Some("*objectname") => Some(true),
        _ if sort == "*objectname" => Some(false),
        _ => None,
    }
}

/// Recognise the `*objecttype` (dereferenced object type) sort key.
fn parse_deref_objecttype_sort_key(sort: &str) -> Option<bool> {
    match sort.strip_prefix('-') {
        Some("*objecttype") => Some(true),
        _ if sort == "*objecttype" => Some(false),
        _ => None,
    }
}

/// Recognise the `*objectsize` (dereferenced object size) sort key.
fn parse_deref_objectsize_sort_key(sort: &str) -> Option<bool> {
    match sort.strip_prefix('-') {
        Some("*objectsize") => Some(true),
        _ if sort == "*objectsize" => Some(false),
        _ => None,
    }
}

/// Sort entries by `*objecttype` (the type of the object an annotated tag
/// dereferences to, empty for non-tag refs); ties break by refname ascending.
/// Empty values sort together (lexicographically first), matching Git.
fn sort_entries_by_deref_objecttype(entries: &mut [RefEntry], reverse: bool) -> CliResult<()> {
    let mut types: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        types.push(ref_deref_objecttype(entry)?);
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        let primary = types[a].cmp(&types[b]);
        let primary = if reverse { primary.reverse() } else { primary };
        primary.then_with(|| entries[a].refname.cmp(&entries[b].refname))
    });
    let reordered: Vec<RefEntry> = order.into_iter().map(|i| entries[i].clone()).collect();
    entries.clone_from_slice(&reordered);
    Ok(())
}

/// Sort entries by `*objectsize` (the byte size of the object an annotated tag
/// dereferences to); non-tag refs have no dereferenced size and sort first
/// ascending (matching Git's empty-first ordering), so they map to [`i64::MIN`].
/// Ties break by refname ascending.
fn sort_entries_by_deref_objectsize(entries: &mut [RefEntry], reverse: bool) -> CliResult<()> {
    let mut sizes: Vec<i64> = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        // `None` (a non-tag ref with no dereferenced object) sorts as the
        // smallest value so it groups ahead of real sizes ascending.
        sizes.push(ref_deref_objectsize(entry)?.unwrap_or(i64::MIN));
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        let primary = sizes[a].cmp(&sizes[b]);
        let primary = if reverse { primary.reverse() } else { primary };
        primary.then_with(|| entries[a].refname.cmp(&entries[b].refname))
    });
    let reordered: Vec<RefEntry> = order.into_iter().map(|i| entries[i].clone()).collect();
    entries.clone_from_slice(&reordered);
    Ok(())
}

/// Sort entries by `*objectname` (the object an annotated tag dereferences to,
/// empty for non-tag refs); ties break by refname ascending, matching Git's
/// final ordering key. Empty values sort together (lexicographically first).
fn sort_entries_by_deref_objectname(entries: &mut [RefEntry], reverse: bool) -> CliResult<()> {
    let mut derefs: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        derefs.push(ref_deref_objectname(entry)?);
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        let primary = derefs[a].cmp(&derefs[b]);
        let primary = if reverse { primary.reverse() } else { primary };
        primary.then_with(|| entries[a].refname.cmp(&entries[b].refname))
    });
    let reordered: Vec<RefEntry> = order.into_iter().map(|i| entries[i].clone()).collect();
    entries.clone_from_slice(&reordered);
    Ok(())
}

/// Sort entries by a date key. The timestamp for each ref is resolved by loading
/// its object (peeling annotated tags to their commit); ties break by refname
/// ascending, matching Git's final ordering key.
fn sort_entries_by_date(entries: &mut [RefEntry], key: DateSortKey, reverse: bool) {
    let mut times: Vec<i64> = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        times.push(ref_sort_timestamp(entry, key));
    }
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        let primary = times[a].cmp(&times[b]);
        let primary = if reverse { primary.reverse() } else { primary };
        primary.then_with(|| entries[a].refname.cmp(&entries[b].refname))
    });
    let reordered: Vec<RefEntry> = order.into_iter().map(|i| entries[i].clone()).collect();
    entries.clone_from_slice(&reordered);
}

/// Resolve the timestamp a ref contributes for a date sort key (`0` when the
/// object cannot be loaded or carries no such date — e.g. a tag pointing at a
/// tree/blob).
fn ref_sort_timestamp(entry: &RefEntry, key: DateSortKey) -> i64 {
    // `creatordate` of an annotated tag is its OWN tagger date (not the peeled
    // commit's). `entry.objecttype` is the object's actual type, determined when
    // the ref was listed, so loading it as a tag here is sound.
    if matches!(key, DateSortKey::Creator) && entry.objecttype == "tag" {
        return ObjectHash::from_str(&entry.objectname)
            .ok()
            .and_then(|hash| load_object::<GitTag>(&hash).ok())
            .map(|tag| tag.tagger.timestamp as i64)
            .unwrap_or(0);
    }
    match ref_commit(entry) {
        Some(commit) => match key {
            DateSortKey::Author => commit.author.timestamp as i64,
            // Committer and creatordate (for commits / lightweight tags).
            _ => commit.committer.timestamp as i64,
        },
        None => 0,
    }
}

/// Maximum number of annotated-tag dereferences when peeling to a commit (the
/// terminal commit itself does not count against this); guards against tag
/// cycles and pathological chains.
pub const MAX_TAG_PEEL_DEPTH: usize = 16;

/// Resolve the commit a ref ultimately points to, dereferencing annotated tags
/// (tag → tag → … → commit). Returns `None` when the chain resolves to a
/// tree/blob or cannot be loaded.
fn ref_commit(entry: &RefEntry) -> Option<Commit> {
    let hash = ObjectHash::from_str(&entry.objectname).ok()?;
    peel_to_commit(hash)
}

/// Peel an object to the commit it ultimately names, following annotated-tag
/// targets. The object database's **actual** stored type is consulted (via
/// `get_object_type`) before every typed load — never a tag's declared `type`
/// line — so a corrupt or mismatched object is never handed to a typed parser
/// that assumes the wrong kind (the `from_bytes` parsers are not defensive
/// against the wrong object type). Allows up to [`MAX_TAG_PEEL_DEPTH`] tag
/// dereferences plus the terminal commit (so a chain of exactly that many tags
/// still resolves) before giving up; returns `None` for a chain ending at a
/// tree/blob or an unreadable object.
fn peel_to_commit(start: ObjectHash) -> Option<Commit> {
    let storage = util::objects_storage();
    let mut current = start;
    // `..=` so the terminal commit can be checked after the deepest allowed tag.
    for _ in 0..=MAX_TAG_PEEL_DEPTH {
        match storage.get_object_type(&current).ok()? {
            ObjectType::Commit => return load_object::<Commit>(&current).ok(),
            ObjectType::Tag => current = load_object::<GitTag>(&current).ok()?.object_hash,
            _ => return None,
        }
    }
    None
}

/// Git's `%(*objectname)`: the object an annotated tag dereferences to (empty
/// string for non-tag refs). Only annotated tags dereference; the value is the
/// tag's recorded target object id, following nested tags via the tag objects'
/// own `object_type`/`object_hash` (no need to read the target object itself,
/// matching Git, which reports the recorded id). A tag whose chain cannot be
/// resolved is a corruption and is surfaced as an error rather than rendered
/// empty (which would be indistinguishable from a legitimate non-tag ref).
fn ref_deref_objectname(entry: &RefEntry) -> CliResult<String> {
    Ok(ref_deref_target(entry)?
        .map(|(hash, _)| hash.to_string())
        .unwrap_or_default())
}

/// Git's `%(*objecttype)`: the type of the object an annotated tag dereferences
/// to (empty for non-tag refs). Read from the tag's recorded `object_type` (the
/// final non-tag tag in a nested chain), so no target read is needed.
fn ref_deref_objecttype(entry: &RefEntry) -> CliResult<String> {
    Ok(ref_deref_target(entry)?
        .map(|(_, object_type)| object_type_name(object_type).to_string())
        .unwrap_or_default())
}

/// Git's `%(*objectsize)`: the byte size of the object an annotated tag
/// dereferences to (`None` → empty for non-tag refs). Unlike `*objecttype`, the
/// size is not recorded in the tag, so the dereferenced object is read; a
/// missing/unreadable target is surfaced as an error rather than a silent 0.
fn ref_deref_objectsize(entry: &RefEntry) -> CliResult<Option<i64>> {
    let Some((target, _)) = ref_deref_target(entry)? else {
        return Ok(None);
    };
    let data = util::objects_storage().get(&target).map_err(|source| {
        CliError::fatal(format!(
            "failed to read the object {target} dereferenced from ref '{}': {source}",
            entry.refname
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    Ok(Some(data.len() as i64))
}

/// Shared helper for the `*`-dereference atoms: for an annotated-tag ref,
/// `Ok(Some((target object id, target type)))`; `Ok(None)` for any non-tag ref
/// (branches, lightweight tags), matching Git, whose `*` atoms are empty unless
/// the ref points at a tag object. A tag ref whose chain cannot be peeled (a
/// missing/corrupt object id, or an unreadable intermediate tag) returns `Err`
/// so the failure is surfaced rather than silently collapsing to the non-tag
/// empty case.
fn ref_deref_target(entry: &RefEntry) -> CliResult<Option<(ObjectHash, ObjectType)>> {
    if entry.objecttype != "tag" {
        return Ok(None);
    }
    let start = ObjectHash::from_str(&entry.objectname).map_err(|source| {
        CliError::fatal(format!(
            "ref '{}' has an invalid object id '{}': {source}",
            entry.refname, entry.objectname
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    Ok(Some(peel_tag_to_target(start, &entry.refname)?))
}

/// Follow an annotated-tag object to the first non-tag object it points at,
/// returning that object's id and type. Uses each tag's recorded `object_type`/
/// `object_hash` (not the target's stored type) and is bounded by
/// [`MAX_TAG_PEEL_DEPTH`]. An unreadable tag in the chain, or a chain deeper than
/// the bound (e.g. a cycle), is surfaced as an error.
fn peel_tag_to_target(tag_hash: ObjectHash, refname: &str) -> CliResult<(ObjectHash, ObjectType)> {
    let mut current = tag_hash;
    for _ in 0..=MAX_TAG_PEEL_DEPTH {
        let tag = load_object::<GitTag>(&current).map_err(|source| {
            CliError::fatal(format!(
                "failed to read tag object {current} while dereferencing ref '{refname}': {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        if tag.object_type == ObjectType::Tag {
            current = tag.object_hash;
        } else {
            return Ok((tag.object_hash, tag.object_type));
        }
    }
    Err(CliError::fatal(format!(
        "ref '{refname}' has a tag chain deeper than {MAX_TAG_PEEL_DEPTH} (possible cycle)"
    ))
    .with_stable_code(StableErrorCode::RepoCorrupt))
}

/// The Git object-type name for an [`ObjectType`], matching the strings used for
/// `%(objecttype)`. A tag only ever dereferences to one of the four canonical
/// loose object types; any other (pack-internal delta) variant is not a valid
/// stored object type and degrades to an empty string.
fn object_type_name(object_type: ObjectType) -> &'static str {
    match object_type {
        ObjectType::Commit => "commit",
        ObjectType::Tree => "tree",
        ObjectType::Blob => "blob",
        ObjectType::Tag => "tag",
        _ => "",
    }
}

fn sort_entries(entries: &mut [RefEntry], sort: Option<&str>) -> CliResult<()> {
    match sort.unwrap_or("refname") {
        "refname" => entries.sort_by(|a, b| a.refname.cmp(&b.refname)),
        "-refname" => entries.sort_by(|a, b| b.refname.cmp(&a.refname)),
        "objectname" => entries.sort_by(|a, b| a.objectname.cmp(&b.objectname)),
        "-objectname" => entries.sort_by(|a, b| b.objectname.cmp(&a.objectname)),
        // `version:refname` (and the `v:refname` alias) order embedded numbers
        // numerically, so `v1.9` sorts before `v1.10`. Shared comparator.
        "version:refname" | "v:refname" => {
            entries.sort_by(|a, b| util::version_refname_cmp(&a.refname, &b.refname))
        }
        "-version:refname" | "-v:refname" => {
            entries.sort_by(|a, b| util::version_refname_cmp(&b.refname, &a.refname))
        }
        other => {
            return Err(CliError::command_usage(format!(
                "unsupported for-each-ref sort key '{other}'"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
    }
    Ok(())
}

/// Output quoting style (`--shell` / `--perl` / `--python` / `--tcl`): each
/// interpolated field value is wrapped as a string literal of the target
/// language so the output can be `eval`-ed/sourced. Literal text in the format
/// (and the default `<oid> <refname>` separators) is left unquoted.
#[derive(Clone, Copy)]
enum QuoteStyle {
    Shell,
    Perl,
    Python,
    Tcl,
}

/// Resolve the active quoting style from the mutually-exclusive flags (clap
/// already rejects more than one).
fn resolve_quote_style(args: &ForEachRefArgs) -> Option<QuoteStyle> {
    if args.shell {
        Some(QuoteStyle::Shell)
    } else if args.perl {
        Some(QuoteStyle::Perl)
    } else if args.python {
        Some(QuoteStyle::Python)
    } else if args.tcl {
        Some(QuoteStyle::Tcl)
    } else {
        None
    }
}

/// Quote `value` as a string literal in the given style, matching `git
/// for-each-ref`'s `--shell`/`--perl`/`--python`/`--tcl` output.
fn quote_value(value: &str, style: QuoteStyle) -> String {
    match style {
        // Single-quote; both `'` and `!` close the quote, emit a backslash-escaped
        // char, and reopen (e.g. `'` → `'\''`, `!` → `'\!'`), matching git's
        // `sq_quote_buf`. Other bytes (incl. newlines) are kept verbatim.
        QuoteStyle::Shell => {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('\'');
            for ch in value.chars() {
                if ch == '\'' || ch == '!' {
                    out.push_str("'\\");
                    out.push(ch);
                    out.push('\'');
                } else {
                    out.push(ch);
                }
            }
            out.push('\'');
            out
        }
        // Single-quote; escape backslash first, then the single-quote (git's
        // `perl_quote_buf`). Newlines stay literal.
        QuoteStyle::Perl => {
            format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
        }
        // Like Perl, but also convert a newline to a literal `\n` so the result
        // stays a single-line Python literal (git's `python_quote_buf`).
        QuoteStyle::Python => {
            let escaped = value
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n");
            format!("'{escaped}'")
        }
        // Double-quote; backslash-escape the Tcl specials and name the control
        // characters, matching Git's `tcl_quote_buf`.
        QuoteStyle::Tcl => {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('"');
            for ch in value.chars() {
                match ch {
                    '[' | ']' | '{' | '}' | '$' | '\\' | '"' => {
                        out.push('\\');
                        out.push(ch);
                    }
                    '\u{c}' => out.push_str("\\f"),
                    '\r' => out.push_str("\\r"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\u{b}' => out.push_str("\\v"),
                    _ => out.push(ch),
                }
            }
            out.push('"');
            out
        }
    }
}

/// Push an interpolated field value, quoting it when a `--shell`/etc. style is
/// active (literal format text bypasses this and is pushed directly).
fn push_field(out: &mut String, value: &str, quote: Option<QuoteStyle>) {
    match quote {
        Some(style) => out.push_str(&quote_value(value, style)),
        None => out.push_str(value),
    }
}

/// Parsed `%(describe:<opts>)` parameters: `(tags, abbrev, match_globs,
/// exclude_globs)`.
type DescribeOpts = (bool, Option<usize>, Vec<String>, Vec<String>);

/// Parse a `%(describe:<opts>)` option string into the `git describe` parameters
/// the atom exposes: `tags`, `abbrev=<n>`, and repeatable `match=<glob>` /
/// `exclude=<glob>`. An unrecognized option is a usage error, matching Git's
/// `fatal: unrecognized %(describe) argument`.
fn parse_describe_options(opts: &str) -> CliResult<DescribeOpts> {
    let mut tags = false;
    let mut abbrev = None;
    let mut match_patterns = Vec::new();
    let mut exclude = Vec::new();
    if !opts.is_empty() {
        for part in opts.split(',') {
            if part == "tags" {
                tags = true;
            } else if let Some(n) = part.strip_prefix("abbrev=") {
                abbrev = Some(n.parse().map_err(|_| {
                    CliError::command_usage(format!(
                        "unrecognized %(describe) argument: abbrev={n}"
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                })?);
            } else if let Some(p) = part.strip_prefix("match=") {
                match_patterns.push(p.to_string());
            } else if let Some(p) = part.strip_prefix("exclude=") {
                exclude.push(p.to_string());
            } else {
                return Err(CliError::command_usage(format!(
                    "unrecognized %(describe) argument: {part}"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
        }
    }
    Ok((tags, abbrev, match_patterns, exclude))
}

/// Collect the distinct `%(describe[:opts])` option strings present in `format`
/// (`""` for a bare `%(describe)`). `%(describexyz)` and other non-`describe`
/// atoms are ignored.
fn describe_specs_in_format(format: &str) -> Vec<String> {
    let mut specs: Vec<String> = Vec::new();
    let mut rest = format;
    while let Some(idx) = rest.find("%(describe") {
        let after = &rest[idx + "%(describe".len()..];
        let Some(end) = after.find(')') else { break };
        let inner = &after[..end];
        // Accept only a bare `%(describe)` or `%(describe:...)`; anything else
        // (e.g. `%(describexyz)`) is not a describe atom.
        if inner.is_empty() || inner.starts_with(':') {
            let opts = inner.strip_prefix(':').unwrap_or("").to_string();
            if !specs.contains(&opts) {
                specs.push(opts);
            }
        }
        rest = &after[end + 1..];
    }
    specs
}

/// Precompute every `%(describe[:opts])` value for each ref before the
/// synchronous render pass. Option strings are validated up front (so a bad
/// `%(describe:bogus)` fails even when no ref matches, matching Git); each
/// (entry, spec) describe runs once and an unreachable-tag result is cached as
/// the empty string.
async fn populate_describe_cache(entries: &mut [RefEntry], format: &str) -> CliResult<()> {
    let specs = describe_specs_in_format(format);
    if specs.is_empty() {
        return Ok(());
    }
    // Validate all option strings up front (independent of the ref set).
    let parsed: Vec<(String, DescribeOpts)> = specs
        .into_iter()
        .map(|spec| parse_describe_options(&spec).map(|p| (spec, p)))
        .collect::<CliResult<_>>()?;

    for entry in entries.iter_mut() {
        for (spec, (tags, abbrev, match_patterns, exclude)) in &parsed {
            let value = super::describe::describe_commit_for_atom(
                &entry.objectname,
                *tags,
                *abbrev,
                match_patterns.clone(),
                exclude.clone(),
            )
            .await?
            .unwrap_or_default();
            entry.describe.insert(spec.clone(), value);
        }
    }
    Ok(())
}

fn render_output(
    entries: &[RefEntry],
    args: &ForEachRefArgs,
    output: &OutputConfig,
    head_refname: Option<&str>,
    upstreams: &HashMap<String, String>,
    pushes: &HashMap<String, String>,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("for-each-ref", &entries.to_vec(), output);
    }
    if output.quiet {
        return Ok(());
    }

    let quote = resolve_quote_style(args);
    // `%(raw)` is binary-unsafe under the quoting modes, so Git rejects it with
    // `--python`/`--shell`/`--tcl` (but allows `--perl`); match that.
    if let Some(format) = &args.format
        && format.contains("%(raw)")
        && matches!(
            quote,
            Some(QuoteStyle::Shell | QuoteStyle::Python | QuoteStyle::Tcl)
        )
    {
        return Err(
            CliError::fatal("--format=raw cannot be used with --python, --shell, --tcl")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128),
        );
    }
    // `%(color:…)` atoms emit ANSI only when color is enabled: forced on by
    // `--color=always`, off by `--color=never`/NO_COLOR, and tty-gated under the
    // `auto` default (mirroring Git's `want_color()`).
    let color_enabled = match output.color {
        crate::utils::output::ColorChoice::Always => true,
        crate::utils::output::ColorChoice::Never => false,
        crate::utils::output::ColorChoice::Auto => std::io::stdout().is_terminal(),
    };
    for entry in entries {
        if let Some(format) = &args.format {
            // Tracks (per row) whether the last emitted `%(color:…)` atom left
            // color active — Git's `need_color_reset_at_eol`. It is driven by the
            // color atoms during rendering, not by scanning the output, so a
            // literal CSI such as `\x1b[K` in the format text never confuses it.
            let need_color_reset = std::cell::Cell::new(false);
            let mut line = render_format(
                format,
                entry,
                head_refname,
                upstreams,
                pushes,
                color_enabled,
                &need_color_reset,
                quote,
            )?;
            // Git appends a trailing `GIT_COLOR_RESET` (`\x1b[m`) when the row
            // leaves color active, so color never bleeds into the next row or the
            // caller's prompt. Under `--shell`/etc. it is a separate quoted field.
            if color_enabled && need_color_reset.get() {
                // Git appends the reset adjacent to the last field (no separating
                // space) — under `--shell`/etc. it is a second quoted literal
                // butted against the previous one (e.g. `'…''\x1b[m'`).
                match quote {
                    Some(style) => line.push_str(&quote_value("\x1b[m", style)),
                    None => line.push_str("\x1b[m"),
                }
            }
            println!("{line}");
        } else if let Some(style) = quote {
            // The default format is the two fields `<objectname> <refname>`; each
            // is quoted independently, the separating space is literal.
            println!(
                "{} {}",
                quote_value(&entry.objectname, style),
                quote_value(&entry.refname, style)
            );
        } else {
            println!("{} {}", entry.objectname, entry.refname);
        }
    }
    Ok(())
}

/// Render `format` (for-each-ref atom syntax) for each `(refname, objectname)`
/// pair using the full for-each-ref atom engine, returning one rendered line per
/// ref. Shared with `branch --format` so it inherits the exact atom set, lazy
/// commit/tag field loading, `%(align)`/`%(if)` blocks, `%(color)` support, and
/// the trailing-`GIT_COLOR_RESET` behavior. All refs are treated as commit
/// objects (branch tips); no `--shell`-style quoting is applied.
pub(crate) async fn render_ref_format_lines(
    refs: &[(String, String)],
    format: &str,
    color_enabled: bool,
) -> CliResult<Vec<String>> {
    let mut entries: Vec<RefEntry> = refs
        .iter()
        .map(|(refname, objectname)| {
            direct_ref_entry(refname.clone(), objectname.clone(), "commit")
        })
        .collect();
    // Precompute `%(describe[:opts])` values (async) before the sync render pass.
    populate_describe_cache(&mut entries, format).await?;
    let head_refname = match Head::current().await {
        Head::Branch(name) => Some(format!("refs/heads/{name}")),
        Head::Detached(_) => None,
    };
    let upstreams = resolve_upstreams(&entries).await;
    let pushes = resolve_pushes(&entries).await;

    let mut lines = Vec::with_capacity(entries.len());
    for entry in &entries {
        let need_color_reset = std::cell::Cell::new(false);
        let mut line = render_format(
            format,
            entry,
            head_refname.as_deref(),
            &upstreams,
            &pushes,
            color_enabled,
            &need_color_reset,
            None,
        )?;
        if color_enabled && need_color_reset.get() {
            line.push_str("\x1b[m");
        }
        lines.push(line);
    }
    Ok(lines)
}

#[allow(clippy::too_many_arguments)]
fn render_format(
    format: &str,
    entry: &RefEntry,
    head_refname: Option<&str>,
    upstreams: &HashMap<String, String>,
    pushes: &HashMap<String, String>,
    color_enabled: bool,
    need_color_reset: &std::cell::Cell<bool>,
    quote: Option<QuoteStyle>,
) -> CliResult<String> {
    // `:short` modifiers: the short ref name (namespace prefix stripped) and the
    // 7-char abbreviated object id. Substituted before the bare atoms (the
    // strings are distinct, so order is not load-bearing, only for clarity).
    let refname_short = short_refname(&entry.refname);
    let objectname_short: String = entry.objectname.chars().take(7).collect();
    // `%(objectsize)`: the byte size of the ref's object (computed lazily only
    // when the atom is present, to avoid an extra object read per ref).
    let objectsize = if format.contains("%(objectsize)") {
        ref_object_size(entry)?.to_string()
    } else {
        String::new()
    };
    // `%(raw)` / `%(raw:size)`: the raw decompressed object content and its byte
    // length (the latter equals `%(objectsize)`); both computed lazily. Note
    // `%(raw)` is not a substring of `%(raw:size)`, so the two are distinguished.
    let raw_size = if format.contains("%(raw:size)") {
        ref_object_size(entry)?.to_string()
    } else {
        String::new()
    };
    let raw = if format.contains("%(raw)") {
        ref_raw_content(entry)?
    } else {
        String::new()
    };
    // `%(*objectname)` / `%(*objectname:short)`: the object an annotated tag
    // dereferences to (empty for non-tag refs); computed lazily.
    let deref_objectname = if format.contains("%(*objectname") {
        ref_deref_objectname(entry)?
    } else {
        String::new()
    };
    let deref_objectname_short: String = deref_objectname.chars().take(7).collect();
    // `%(*objecttype)` / `%(*objectsize)`: the type / byte size of the object an
    // annotated tag dereferences to (empty for non-tag refs); computed lazily.
    let deref_objecttype = if format.contains("%(*objecttype)") {
        ref_deref_objecttype(entry)?
    } else {
        String::new()
    };
    let deref_objectsize = if format.contains("%(*objectsize)") {
        ref_deref_objectsize(entry)?
            .map(|size| size.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    // `%(HEAD)`: `*` for the currently checked-out branch, a space otherwise.
    let head_marker = if head_refname == Some(entry.refname.as_str()) {
        "*"
    } else {
        " "
    };
    // `%(upstream)`: the tracking ref (empty when none); `:short` strips the
    // `refs/remotes/` prefix.
    let upstream = upstreams
        .get(&entry.refname)
        .map(String::as_str)
        .unwrap_or("");
    let upstream_short = upstream.strip_prefix("refs/remotes/").unwrap_or(upstream);
    // `%(push)`: the push-tracking ref (empty when none); `:short` strips the
    // `refs/remotes/` prefix.
    let push = pushes.get(&entry.refname).map(String::as_str).unwrap_or("");
    let push_short = push.strip_prefix("refs/remotes/").unwrap_or(push);
    // Commit-field atoms (`%(subject)`, author/committer name+email) require
    // loading the ref's object. Load it once, only when at least one such atom
    // is present, to avoid extra object reads.
    const COMMIT_FIELD_ATOMS: [&str; 19] = [
        "%(subject)",
        "%(contents)",
        "%(contents:subject)",
        "%(body)",
        "%(contents:body)",
        "%(authorname)",
        "%(authoremail)",
        "%(authordate)",
        "%(committername)",
        "%(committeremail)",
        "%(committerdate)",
        "%(taggername)",
        "%(taggeremail)",
        "%(taggerdate)",
        "%(tree)",
        "%(tree:short)",
        "%(parent)",
        "%(parent:short)",
        "%(numparent)",
    ];
    // Date `:<format>` modifiers (e.g. `%(committerdate:iso)`) and bare
    // `%(creatordate)` also need the loaded object but are not exact members of
    // COMMIT_FIELD_ATOMS, so detect their prefixes too.
    const DATE_ATOM_PREFIXES: [&str; 4] = [
        "%(committerdate:",
        "%(authordate:",
        "%(taggerdate:",
        "%(creatordate",
    ];
    let needs_fields = COMMIT_FIELD_ATOMS.iter().any(|a| format.contains(a))
        || DATE_ATOM_PREFIXES.iter().any(|p| format.contains(p));
    let fields = if needs_fields {
        commit_fields_for(entry)
    } else {
        CommitFields::default()
    };
    // `%(worktreepath)`: the absolute path of the worktree that has this ref
    // checked out, else empty. Libra worktrees share a single HEAD, so the
    // checked-out branch is the current HEAD branch and the reported path is the
    // CURRENT worktree — the one the command runs in (`util::working_dir()`).
    // Computed lazily, only when the atom is present. Matches Git for a
    // single-worktree repository.
    let worktreepath =
        if format.contains("%(worktreepath)") && head_refname == Some(entry.refname.as_str()) {
            util::working_dir()
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
    // Atom name (inside `%(...)`) -> value. Single-pass substitution below
    // writes each value literally, so a value containing `%(` is never
    // re-parsed as an atom and never trips the unknown-atom check.
    let atoms: [(&str, &str); 37] = [
        ("worktreepath", worktreepath.as_str()),
        ("raw:size", raw_size.as_str()),
        ("raw", raw.as_str()),
        ("objectsize", objectsize.as_str()),
        ("tree:short", fields.tree_short.as_str()),
        ("tree", fields.tree.as_str()),
        ("parent:short", fields.parent_short.as_str()),
        ("parent", fields.parent.as_str()),
        ("numparent", fields.numparent.as_str()),
        ("*objectname:short", deref_objectname_short.as_str()),
        ("*objectname", deref_objectname.as_str()),
        ("*objecttype", deref_objecttype.as_str()),
        ("*objectsize", deref_objectsize.as_str()),
        ("HEAD", head_marker),
        ("upstream:short", upstream_short),
        ("upstream", upstream),
        ("push:short", push_short),
        ("push", push),
        ("subject", fields.subject.as_str()),
        ("contents:subject", fields.subject.as_str()),
        ("contents:body", fields.body.as_str()),
        ("contents", fields.contents.as_str()),
        ("body", fields.body.as_str()),
        ("authorname", fields.author_name.as_str()),
        ("authoremail", fields.author_email.as_str()),
        ("authordate", fields.author_date.as_str()),
        ("committername", fields.committer_name.as_str()),
        ("committeremail", fields.committer_email.as_str()),
        ("committerdate", fields.committer_date.as_str()),
        ("taggername", fields.tagger_name.as_str()),
        ("taggeremail", fields.tagger_email.as_str()),
        ("taggerdate", fields.tagger_date.as_str()),
        ("refname:short", refname_short.as_str()),
        ("objectname:short", objectname_short.as_str()),
        ("refname", entry.refname.as_str()),
        ("objectname", entry.objectname.as_str()),
        ("objecttype", entry.objecttype.as_str()),
    ];
    let date_ctx = DateAtomContext::from_fields(&fields);
    render_fragment(
        format,
        &atoms,
        entry,
        &date_ctx,
        color_enabled,
        need_color_reset,
        quote,
    )
}

/// Column position for a `%(align)` block.
#[derive(Debug, Clone, Copy)]
enum AlignPosition {
    Left,
    Right,
    Middle,
}

/// Render a format fragment by substituting atoms, handling `%(align:…)`…
/// `%(end)` blocks (which pad their rendered contents to a column width). Called
/// recursively for the contents of each align block (so nested alignment and
/// inner atoms both work).
fn render_fragment(
    fragment: &str,
    atoms: &[(&str, &str)],
    entry: &RefEntry,
    date_ctx: &DateAtomContext,
    color_enabled: bool,
    need_color_reset: &std::cell::Cell<bool>,
    quote: Option<QuoteStyle>,
) -> CliResult<String> {
    let mut out = String::with_capacity(fragment.len());
    let mut rest = fragment;
    while let Some(pos) = rest.find("%(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..];
        let Some(close) = after.find(')') else {
            return Err(unsupported_atom_error());
        };
        let token = &after[2..close];
        // Everything after this token's closing `)`.
        let body = &after[close + 1..];

        // `%(align:<width>[,<position>])` … `%(end)`: render the enclosed
        // fragment, then pad it to `width` columns (no truncation when it is
        // already wider), matching Git's alignment atom.
        if let Some(spec) = token.strip_prefix("align:") {
            let (width, position) = parse_align_spec(spec)?;
            let Some((end_start, after_end)) = find_block_end(body) else {
                return Err(align_missing_end_error());
            };
            // The block's contents render WITHOUT per-field quoting (pass `None`),
            // so inner atoms, literal text, and any nested align block are all
            // raw; the whole padded block is then quoted once at the active level.
            // This matches Git, where under `--shell`/`--perl`/`--python`/`--tcl`
            // only the topmost align block is quoted as a single string literal —
            // nested align blocks and block literals do not quote separately.
            let inner = render_fragment(
                &body[..end_start],
                atoms,
                entry,
                date_ctx,
                color_enabled,
                need_color_reset,
                None,
            )?;
            let padded = pad_aligned(&inner, width, position);
            push_field(&mut out, &padded, quote);
            rest = &body[after_end..];
            continue;
        }

        // `%(if[:equals=<v>|:notequals=<v>])` … `%(then)` … [`%(else)` …]
        // `%(end)`: evaluate the condition fragment (everything between
        // `%(if…)` and `%(then)`) and emit the then- or else-branch. Plain
        // `%(if)` is true when the rendered condition is non-empty after
        // trimming whitespace; `equals`/`notequals` compare the raw rendered
        // condition. Blocks nest (sharing the `%(end)` terminator with align).
        if token == "if" || token.starts_with("if:") {
            let cond_spec = &token[2..];
            let Some((end_start, after_end)) = find_block_end(body) else {
                return Err(if_missing_end_error());
            };
            let content = &body[..end_start];
            let Some((then_start, then_after)) = find_if_marker(content, "then") else {
                return Err(if_missing_then_error());
            };
            let condition = &content[..then_start];
            let branches = &content[then_after..];
            let (then_branch, else_branch) = match find_if_marker(branches, "else") {
                Some((else_start, else_after)) => {
                    (&branches[..else_start], &branches[else_after..])
                }
                None => (branches, ""),
            };
            // The condition renders raw (it is tested, not emitted); the chosen
            // branch renders at the active quote level like any other output.
            let cond_value = render_fragment(
                condition,
                atoms,
                entry,
                date_ctx,
                color_enabled,
                need_color_reset,
                None,
            )?;
            let chosen = if eval_if_condition(cond_spec, &cond_value)? {
                then_branch
            } else {
                else_branch
            };
            out.push_str(&render_fragment(
                chosen,
                atoms,
                entry,
                date_ctx,
                color_enabled,
                need_color_reset,
                quote,
            )?);
            rest = &body[after_end..];
            continue;
        }

        // `%(align)` without a spec, or block markers/terminators that are not
        // enclosed by their opener, are usage errors.
        if token == "align" {
            return Err(align_missing_width_error());
        }
        if token == "then" || token == "else" {
            return Err(if_stray_marker_error(token));
        }
        if token == "end" {
            return Err(stray_end_error());
        }

        // Parameterized atoms (`%(refname:lstrip=N)` / `%(refname:rstrip=N)` and
        // `%(objectname:short=N)`) are handled first; everything else is an exact
        // atom-name match.
        // Each interpolated field value is quoted (when a `--shell`/etc. style is
        // active); the literal format text between atoms is pushed verbatim above.
        if let Some(value) = refname_strip_atom(token, &entry.refname) {
            push_field(&mut out, &value, quote);
        } else if let Some(value) = symref_atom(token, entry.symref.as_deref()) {
            push_field(&mut out, &value, quote);
        } else if let Some(n) = token
            .strip_prefix("objectname:short=")
            .and_then(|s| s.parse::<usize>().ok())
        {
            push_field(
                &mut out,
                &entry.objectname.chars().take(n).collect::<String>(),
                quote,
            );
        } else if let Some(value) = date_atom_value(token, date_ctx) {
            push_field(&mut out, &value, quote);
        } else if token == "describe" || token.starts_with("describe:") {
            // `%(describe[:opts])`: the value was precomputed per ref (git describe
            // is async, this render path is not), keyed by the verbatim option
            // string. A commit with no reachable tag has an empty cached value,
            // matching Git's empty `%(describe)` output.
            let key = token.strip_prefix("describe:").unwrap_or("");
            let value = entry.describe.get(key).map(String::as_str).unwrap_or("");
            push_field(&mut out, value, quote);
        } else if let Some(spec) = token.strip_prefix("color:") {
            // Validate the spec regardless of color state (a bad `%(color:…)` is a
            // format error even when output is not colored, matching Git). The
            // resolved value (the escape when color is enabled, else empty) goes
            // through `push_field` so the active `--shell`/etc. quoting still
            // applies to the color atom, like Git.
            let escape = color_spec_to_ansi(spec)?;
            if color_enabled {
                // Git's rule: the trailing reset is needed unless the last color
                // atom was itself the bare reset (`GIT_COLOR_RESET` = `\x1b[m`).
                need_color_reset.set(escape != "\x1b[m");
            }
            let value = if color_enabled { escape } else { String::new() };
            push_field(&mut out, &value, quote);
        } else {
            match atoms.iter().find(|(name, _)| *name == token) {
                Some((_, value)) => push_field(&mut out, value, quote),
                None => return Err(unsupported_atom_error()),
            }
        }
        rest = body;
    }
    out.push_str(rest);
    Ok(out)
}

/// Parse a `%(align:…)` spec into `(width, position)`. Tokens are
/// comma-separated and order-independent: a bare number or `width=<n>` sets the
/// width (required), and `left`/`right`/`middle` or `position=<p>` sets the
/// position (default `left`).
fn parse_align_spec(spec: &str) -> CliResult<(usize, AlignPosition)> {
    let mut width: Option<usize> = None;
    let mut position = AlignPosition::Left;
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(w) = part.strip_prefix("width=") {
            width = Some(w.parse().map_err(|_| align_spec_error(spec))?);
        } else if let Some(p) = part.strip_prefix("position=") {
            position = parse_align_position(p).ok_or_else(|| align_spec_error(spec))?;
        } else if let Ok(w) = part.parse::<usize>() {
            width = Some(w);
        } else if let Some(p) = parse_align_position(part) {
            position = p;
        } else {
            return Err(align_spec_error(spec));
        }
    }
    let width = width.ok_or_else(|| align_spec_error(spec))?;
    Ok((width, position))
}

fn parse_align_position(value: &str) -> Option<AlignPosition> {
    match value {
        "left" => Some(AlignPosition::Left),
        "right" => Some(AlignPosition::Right),
        "middle" => Some(AlignPosition::Middle),
        _ => None,
    }
}

/// Whether a token opens a block that must be closed by `%(end)` (`%(align…)`
/// or `%(if…)`).
fn is_block_opener(token: &str) -> bool {
    token == "align" || token.starts_with("align:") || token == "if" || token.starts_with("if:")
}

/// Find the `%(end)` that closes the current block within `s`, accounting for
/// nested `%(align)`/`%(if)` blocks. Returns the byte range of the `%(end)`
/// token (`(start, after)`), or `None` when it is missing.
fn find_block_end(s: &str) -> Option<(usize, usize)> {
    let mut depth = 1usize;
    let mut i = 0;
    while let Some(rel) = s[i..].find("%(") {
        let at = i + rel;
        let after = &s[at + 2..];
        let close = after.find(')')?;
        let token = &after[..close];
        let token_end = at + 2 + close + 1;
        if token == "end" {
            depth -= 1;
            if depth == 0 {
                return Some((at, token_end));
            }
        } else if is_block_opener(token) {
            depth += 1;
        }
        i = token_end;
    }
    None
}

/// Find a `%(then)` / `%(else)` marker at the top level of an `%(if)` block's
/// content (depth 0 — markers belonging to nested `%(if)` blocks are skipped).
/// Returns the byte range of the marker token, or `None`.
fn find_if_marker(content: &str, marker: &str) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(rel) = content[i..].find("%(") {
        let at = i + rel;
        let after = &content[at + 2..];
        let close = after.find(')')?;
        let token = &after[..close];
        let token_end = at + 2 + close + 1;
        if is_block_opener(token) {
            depth += 1;
        } else if token == "end" {
            depth = depth.saturating_sub(1);
        } else if token == marker && depth == 0 {
            return Some((at, token_end));
        }
        i = token_end;
    }
    None
}

/// Evaluate an `%(if)` condition. `spec` is the text after `if` in the token:
/// empty for a plain `%(if)` (true when `value` is non-empty after trimming),
/// `:equals=<v>` (true when `value == v`), or `:notequals=<v>` (true when
/// `value != v`); `equals`/`notequals` compare the raw, untrimmed value.
fn eval_if_condition(spec: &str, value: &str) -> CliResult<bool> {
    if spec.is_empty() {
        Ok(!value.trim().is_empty())
    } else if let Some(v) = spec.strip_prefix(":equals=") {
        Ok(value == v)
    } else if let Some(v) = spec.strip_prefix(":notequals=") {
        Ok(value != v)
    } else {
        Err(if_invalid_spec_error(spec))
    }
}

/// Pad `content` to `width` display columns at the given position. Content that
/// already meets or exceeds the width is returned unchanged (Git does not
/// truncate). Width is measured in Unicode display columns.
fn pad_aligned(content: &str, width: usize, position: AlignPosition) -> String {
    use unicode_width::UnicodeWidthStr;
    let content_width = UnicodeWidthStr::width(content);
    if content_width >= width {
        return content.to_string();
    }
    let pad = width - content_width;
    match position {
        AlignPosition::Left => format!("{content}{}", " ".repeat(pad)),
        AlignPosition::Right => format!("{}{content}", " ".repeat(pad)),
        AlignPosition::Middle => {
            // Odd padding biases the extra space to the right, matching Git.
            let left = pad / 2;
            let right = pad - left;
            format!("{}{content}{}", " ".repeat(left), " ".repeat(right))
        }
    }
}

fn align_spec_error(spec: &str) -> CliError {
    CliError::command_usage(format!(
        "invalid %(align) spec '{spec}' (expected a width and optional left/right/middle position)"
    ))
    .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn align_missing_width_error() -> CliError {
    CliError::command_usage("%(align) requires a width, e.g. %(align:20,left)")
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn align_missing_end_error() -> CliError {
    CliError::command_usage("format: %(end) atom missing for %(align)")
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn if_missing_end_error() -> CliError {
    CliError::command_usage("format: %(end) atom missing for %(if)")
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn if_missing_then_error() -> CliError {
    CliError::command_usage("format: %(if) atom used without a %(then) atom")
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn if_stray_marker_error(marker: &str) -> CliError {
    CliError::command_usage(format!(
        "format: %({marker}) atom used without a %(if) atom"
    ))
    .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn if_invalid_spec_error(spec: &str) -> CliError {
    CliError::command_usage(format!(
        "invalid %(if{spec}) condition (expected :equals=<value> or :notequals=<value>)"
    ))
    .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn stray_end_error() -> CliError {
    CliError::command_usage("format: %(end) atom used without a %(align) or %(if) atom")
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn unsupported_atom_error() -> CliError {
    CliError::command_usage("unsupported for-each-ref format atom")
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

/// Commit-field atom values for one ref. `author_*`/`committer_*` are populated
/// only for refs pointing directly at a commit (empty for annotated tags, which
/// carry a tagger rather than an author, and for trees/blobs); `subject` is the
/// first message line of a commit or annotated-tag object. The `*_email` values
/// include the surrounding angle brackets, matching Git.
#[derive(Default)]
struct CommitFields {
    subject: String,
    /// Full message (`%(contents)`): gpgsig-stripped for commits, the raw
    /// message for annotated tags.
    contents: String,
    /// Message body (`%(body)`): everything after the first blank line.
    body: String,
    author_name: String,
    author_email: String,
    committer_name: String,
    committer_email: String,
    author_date: String,
    committer_date: String,
    tagger_name: String,
    tagger_email: String,
    tagger_date: String,
    /// Raw Unix timestamps backing the `:<format>` date modifiers (e.g.
    /// `%(committerdate:iso)`). `None` when the field does not apply to the ref's
    /// object type. `creator_ts` is the committer date for commits / lightweight
    /// tags and the tagger date for annotated tags (`%(creatordate)`).
    author_ts: Option<i64>,
    committer_ts: Option<i64>,
    tagger_ts: Option<i64>,
    creator_ts: Option<i64>,
    /// `%(tree)` / `%(tree:short)`: the commit's tree id (empty for non-commits).
    tree: String,
    tree_short: String,
    /// `%(parent)` / `%(parent:short)`: the commit's parent ids, space-separated
    /// (empty for a root commit or a non-commit).
    parent: String,
    parent_short: String,
    /// `%(numparent)`: the commit's parent count (empty for a non-commit).
    numparent: String,
}

/// Load the ref's object (once) and extract its commit-field atom values.
fn commit_fields_for(entry: &RefEntry) -> CommitFields {
    let Ok(hash) = ObjectHash::from_str(&entry.objectname) else {
        return CommitFields::default();
    };
    match entry.objecttype.as_str() {
        "commit" => match load_object::<Commit>(&hash) {
            // Strip a leading `gpgsig`/`gpgsig-sha256` header before the subject.
            Ok(c) => {
                let contents = parse_commit_msg(&c.message).0.to_string();
                let parent = c
                    .parent_commit_ids
                    .iter()
                    .map(|h| h.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                let parent_short = c
                    .parent_commit_ids
                    .iter()
                    .map(|h| h.to_string().chars().take(7).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" ");
                let tree = c.tree_id.to_string();
                let tree_short = tree.chars().take(7).collect();
                CommitFields {
                    subject: first_subject_line(&contents),
                    body: message_body(&contents),
                    contents,
                    author_name: c.author.name.clone(),
                    author_email: format!("<{}>", c.author.email),
                    committer_name: c.committer.name.clone(),
                    committer_email: format!("<{}>", c.committer.email),
                    author_date: format_timestamp_with(c.author.timestamp as i64, ""),
                    committer_date: format_timestamp_with(c.committer.timestamp as i64, ""),
                    author_ts: Some(c.author.timestamp as i64),
                    committer_ts: Some(c.committer.timestamp as i64),
                    creator_ts: Some(c.committer.timestamp as i64),
                    tree,
                    tree_short,
                    parent,
                    parent_short,
                    numparent: c.parent_commit_ids.len().to_string(),
                    ..CommitFields::default()
                }
            }
            Err(_) => CommitFields::default(),
        },
        // Annotated tags have a message (subject) and a tagger, but no
        // author/committer.
        "tag" => match load_object::<GitTag>(&hash) {
            Ok(t) => CommitFields {
                subject: first_subject_line(&t.message),
                body: message_body(&t.message),
                contents: t.message.clone(),
                tagger_name: t.tagger.name.clone(),
                tagger_email: format!("<{}>", t.tagger.email),
                tagger_date: format_timestamp_with(t.tagger.timestamp as i64, ""),
                tagger_ts: Some(t.tagger.timestamp as i64),
                creator_ts: Some(t.tagger.timestamp as i64),
                ..CommitFields::default()
            },
            Err(_) => CommitFields::default(),
        },
        _ => CommitFields::default(),
    }
}

/// Resolve a date atom that carries a `:<format>` modifier
/// (`%(committerdate:iso)`) or the `%(creatordate)` atom (bare or modified).
/// Bare `committerdate`/`authordate`/`taggerdate` are served from the exact atom
/// table (default format), so this returns `None` for them and for any non-date
/// token. An empty string results when the date does not apply to the ref's
/// object type (e.g. `authordate` on an annotated tag).
/// The raw timestamps backing the date `:<format>` modifiers, carried into the
/// fragment renderer (which otherwise only sees the pre-formatted atom table).
#[derive(Clone, Copy, Default)]
struct DateAtomContext {
    author_ts: Option<i64>,
    committer_ts: Option<i64>,
    tagger_ts: Option<i64>,
    creator_ts: Option<i64>,
}

impl DateAtomContext {
    fn from_fields(fields: &CommitFields) -> Self {
        Self {
            author_ts: fields.author_ts,
            committer_ts: fields.committer_ts,
            tagger_ts: fields.tagger_ts,
            creator_ts: fields.creator_ts,
        }
    }
}

fn date_atom_value(token: &str, ctx: &DateAtomContext) -> Option<String> {
    let (base, modifier, has_modifier) = match token.split_once(':') {
        Some((base, modifier)) => (base, modifier, true),
        None => (token, "", false),
    };
    let ts = match base {
        "committerdate" if has_modifier => ctx.committer_ts,
        "authordate" if has_modifier => ctx.author_ts,
        "taggerdate" if has_modifier => ctx.tagger_ts,
        "creatordate" => ctx.creator_ts,
        _ => return None,
    };
    Some(
        ts.map(|t| format_for_each_ref_date(t, modifier))
            .unwrap_or_default(),
    )
}

/// Translate a `%(color:<spec>)` spec into the ANSI SGR escape it requests
/// (`\x1b[<codes>m`). The spec is a space-separated list of colors and
/// attributes, mirroring Git's color syntax: the first color word is the
/// foreground, the second is the background. Returns an empty string for a spec
/// that requests no codes (e.g. `normal`), and a usage error for an
/// unrecognized word (matching Git, which rejects bad color names).
fn color_spec_to_ansi(spec: &str) -> CliResult<String> {
    // Git parses the words then serializes in a fixed order — reset, then
    // attributes (ascending code), then foreground, then background — regardless
    // of input order, so e.g. `red reset` resets THEN reapplies red.
    let mut reset = false;
    let mut attrs: Vec<u32> = Vec::new();
    let mut fg: Option<String> = None;
    let mut bg: Option<String> = None;
    let mut color_slot: u32 = 0; // 0 = next color is foreground, 1 = background
    for word in spec.split_whitespace() {
        if word == "reset" {
            reset = true;
            continue;
        }
        if let Some(code) = attr_code(word) {
            attrs.push(code);
            continue;
        }
        // Otherwise it is a color word (or invalid). Git allows at most two
        // colors (foreground then background); a third is a spec error.
        if color_slot >= 2 {
            return Err(
                CliError::command_usage(format!("too many colors in color spec '{spec}'"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments),
            );
        }
        // `normal` yields no code but still consumes a color slot.
        let code = color_word_code(word, color_slot)?;
        if color_slot == 0 {
            fg = code;
        } else {
            bg = code;
        }
        color_slot += 1;
    }

    attrs.sort_unstable();
    attrs.dedup();
    let has_other = !attrs.is_empty() || fg.is_some() || bg.is_some();
    // A bare `reset` is Git's `GIT_COLOR_RESET` — `\x1b[m` (empty params), not
    // `\x1b[0m`. Combined with other codes it contributes a leading `0`.
    if reset && !has_other {
        return Ok("\x1b[m".to_string());
    }
    let mut codes: Vec<String> = Vec::new();
    if reset {
        codes.push("0".to_string());
    }
    codes.extend(attrs.into_iter().map(|c| c.to_string()));
    codes.extend(fg);
    codes.extend(bg);
    if codes.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("\x1b[{}m", codes.join(";")))
}

/// Map an attribute word to its SGR code, accepting both the compact (`nobold`)
/// and hyphenated (`no-bold`) negation forms Git supports. Returns `None` when
/// the word is not an attribute (so the caller treats it as a color word).
/// `reset` is handled by the caller (it has special ordering).
fn attr_code(word: &str) -> Option<u32> {
    // (positive code, "off" code) for each attribute.
    let positive = |w: &str| -> Option<(u32, u32)> {
        match w {
            "bold" => Some((1, 22)),
            "dim" => Some((2, 22)),
            "italic" => Some((3, 23)),
            "ul" | "underline" => Some((4, 24)),
            "blink" => Some((5, 25)),
            "reverse" => Some((7, 27)),
            "strike" => Some((9, 29)),
            _ => None,
        }
    };
    if let Some((on, _)) = positive(word) {
        return Some(on);
    }
    // Negated attribute: `no-bold` (hyphenated) or `nobold` (compact). Try the
    // hyphenated form first so `no-...` is not mis-stripped by the `no` prefix.
    let base = word
        .strip_prefix("no-")
        .or_else(|| word.strip_prefix("no"))?;
    positive(base).map(|(_, off)| off)
}

/// Resolve a single color word to its SGR code for the given slot (0 =
/// foreground, ≥1 = background). `normal` resolves to `None` (no code). Supports
/// the 8 basic names, their `bright<name>` variants, `default`, 256-color
/// indices (0–255), and `#rrggbb` truecolor. Errors on anything else.
fn color_word_code(word: &str, slot: u32) -> CliResult<Option<String>> {
    let is_bg = slot >= 1;
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    if word == "normal" {
        return Ok(None);
    }
    if word == "default" {
        return Ok(Some(if is_bg { "49" } else { "39" }.to_string()));
    }
    if let Some(idx) = NAMES.iter().position(|&n| n == word) {
        let base = if is_bg { 40 } else { 30 };
        return Ok(Some((base + idx as u32).to_string()));
    }
    if let Some(rest) = word.strip_prefix("bright")
        && let Some(idx) = NAMES.iter().position(|&n| n == rest)
    {
        let base = if is_bg { 100 } else { 90 };
        return Ok(Some((base + idx as u32).to_string()));
    }
    if let Ok(n) = word.parse::<u32>()
        && n <= 255
    {
        // Git maps 0–7 to the basic ANSI colors and 8–15 to the bright variants;
        // only 16–255 use the `38/48;5;n` 256-color form.
        let code = if n <= 7 {
            (if is_bg { 40 } else { 30 }) + n
        } else if n <= 15 {
            (if is_bg { 100 } else { 90 }) + (n - 8)
        } else {
            return Ok(Some(format!("{};5;{n}", if is_bg { 48 } else { 38 })));
        };
        return Ok(Some(code.to_string()));
    }
    if let Some(hex) = word.strip_prefix('#')
        && hex.len() == 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        )
    {
        return Ok(Some(format!(
            "{};2;{r};{g};{b}",
            if is_bg { 48 } else { 38 }
        )));
    }
    Err(
        CliError::command_usage(format!("unrecognized color '{word}'"))
            .with_stable_code(StableErrorCode::CliInvalidArguments),
    )
}

/// Format a timestamp for a for-each-ref date atom. Reuses the shared
/// `format_timestamp_with` for the deterministic formats it supports —
/// `default`, `short`, `iso`/`iso8601`, `iso-strict`/`iso8601-strict`,
/// `rfc`/`rfc2822`, `unix`, `raw` — and computes `relative` locally. `local`,
/// `human`, and `format:<strftime>` are not yet supported and fall back to the
/// default format (a documented narrowing).
fn format_for_each_ref_date(ts: i64, modifier: &str) -> String {
    if modifier == "relative" {
        text::relative_date(ts)
    } else {
        format_timestamp_with(ts, modifier)
    }
}

/// First non-empty line of a commit/tag message (messages can carry leading
/// newlines from the header separator).
fn first_subject_line(message: &str) -> String {
    message
        .trim_start_matches('\n')
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Message body for `%(body)`: everything after the first blank line that
/// separates the subject from the body (empty when there is no body), matching
/// `git for-each-ref`.
fn message_body(message: &str) -> String {
    message
        .trim_start_matches('\n')
        .split_once("\n\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default()
}

/// The `:short` form of a ref name: strip the well-known namespace prefix
/// (`refs/heads/`, `refs/tags/`, `refs/remotes/`), falling back to stripping a
/// leading `refs/`, otherwise the name unchanged.
fn short_refname(refname: &str) -> String {
    for prefix in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(rest) = refname.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    refname.strip_prefix("refs/").unwrap_or(refname).to_string()
}

/// Handle `%(refname:lstrip=N)` / `%(refname:rstrip=N)`, returning the stripped
/// ref name. `N > 0` removes that many leading (lstrip) or trailing (rstrip)
/// slash-separated components; `N < 0` keeps the last `|N|` (lstrip) or first
/// `|N|` (rstrip) components. Returns `None` for any other token (including a
/// non-integer N), so the caller treats it as an unknown atom.
fn refname_strip_atom(token: &str, refname: &str) -> Option<String> {
    let (from_left, num) = if let Some(n) = token.strip_prefix("refname:lstrip=") {
        (true, n)
    } else {
        let n = token.strip_prefix("refname:rstrip=")?;
        (false, n)
    };
    Some(strip_ref_components(refname, from_left, num.parse().ok()?))
}

/// Drop/keep slash-separated components of a ref name (Git's `lstrip`/`rstrip`).
/// `N > 0` removes that many leading (lstrip) or trailing (rstrip) components;
/// `N < 0` keeps the last `|N|` (lstrip) or first `|N|` (rstrip).
fn strip_ref_components(refname: &str, from_left: bool, n: i64) -> String {
    let comps: Vec<&str> = refname.split('/').collect();
    let len = comps.len() as i64;
    let kept: &[&str] = match (from_left, n >= 0) {
        (true, true) => comps.get(n.min(len) as usize..).unwrap_or(&[]),
        (true, false) => &comps[(len - (-n).min(len)) as usize..],
        (false, true) => &comps[..(len - n.min(len)) as usize],
        (false, false) => comps.get(..(-n).min(len) as usize).unwrap_or(&comps),
    };
    kept.join("/")
}

/// Handle the `%(symref)` family: `%(symref)` (the full target ref of a
/// symbolic ref, empty for ordinary refs), `%(symref:short)`, and
/// `%(symref:lstrip=N)` / `%(symref:rstrip=N)`. Returns `None` for any token
/// outside the family so the caller treats it as a different/unknown atom.
fn symref_atom(token: &str, symref: Option<&str>) -> Option<String> {
    let target = symref.unwrap_or("");
    if token == "symref" {
        return Some(target.to_string());
    }
    if token == "symref:short" {
        // An empty target (ordinary ref) stays empty rather than shortening "".
        return Some(if target.is_empty() {
            String::new()
        } else {
            short_refname(target)
        });
    }
    if let Some(num) = token.strip_prefix("symref:lstrip=") {
        return Some(strip_ref_components(target, true, num.parse().ok()?));
    }
    if let Some(num) = token.strip_prefix("symref:rstrip=") {
        return Some(strip_ref_components(target, false, num.parse().ok()?));
    }
    None
}

#[cfg(test)]
mod color_spec_tests {
    use super::color_spec_to_ansi;

    #[test]
    fn basic_colors_and_attributes() {
        assert_eq!(color_spec_to_ansi("red").unwrap(), "\x1b[31m");
        assert_eq!(color_spec_to_ansi("reset").unwrap(), "\x1b[m"); // git GIT_COLOR_RESET
        // First color is fg, second is bg; attributes can lead.
        assert_eq!(
            color_spec_to_ansi("bold green blue").unwrap(),
            "\x1b[1;32;44m"
        );
        assert_eq!(color_spec_to_ansi("brightred").unwrap(), "\x1b[91m");
        assert_eq!(color_spec_to_ansi("ul").unwrap(), "\x1b[4m");
    }

    #[test]
    fn extended_colors() {
        // 0-7 are basic, 8-15 bright, 16-255 the 256-color form (git numeric map).
        assert_eq!(color_spec_to_ansi("1").unwrap(), "\x1b[31m");
        assert_eq!(color_spec_to_ansi("9").unwrap(), "\x1b[91m");
        assert_eq!(color_spec_to_ansi("214").unwrap(), "\x1b[38;5;214m");
        assert_eq!(
            color_spec_to_ansi("#ff8800").unwrap(),
            "\x1b[38;2;255;136;0m"
        );
        // 256-color as background (second color slot).
        assert_eq!(color_spec_to_ansi("red 240").unwrap(), "\x1b[31;48;5;240m");
    }

    #[test]
    fn serialization_order_is_git_faithful() {
        // Output order is always reset, attrs (ascending), fg, bg — regardless of
        // input order — so `red reset` resets THEN reapplies red.
        assert_eq!(color_spec_to_ansi("red reset").unwrap(), "\x1b[0;31m");
        assert_eq!(color_spec_to_ansi("red bold").unwrap(), "\x1b[1;31m");
        assert_eq!(color_spec_to_ansi("bold red").unwrap(), "\x1b[1;31m");
    }

    #[test]
    fn negated_attributes_compact_and_hyphenated() {
        // Both `nobold` and `no-bold` map to the off code (22); same for others.
        assert_eq!(color_spec_to_ansi("no-bold").unwrap(), "\x1b[22m");
        assert_eq!(color_spec_to_ansi("nobold").unwrap(), "\x1b[22m");
        assert_eq!(color_spec_to_ansi("no-ul").unwrap(), "\x1b[24m");
        assert_eq!(color_spec_to_ansi("noreverse").unwrap(), "\x1b[27m");
    }

    #[test]
    fn normal_yields_no_code_and_bad_color_errors() {
        assert_eq!(color_spec_to_ansi("normal").unwrap(), "");
        assert!(color_spec_to_ansi("bogus").is_err());
        assert!(color_spec_to_ansi("256").is_err()); // out of 0..=255 range
        // At most two colors (fg, bg); a third is a spec error.
        assert!(color_spec_to_ansi("red green blue").is_err());
        assert_eq!(color_spec_to_ansi("red green").unwrap(), "\x1b[31;42m");
    }
}
