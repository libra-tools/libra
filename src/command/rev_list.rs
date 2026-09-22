//! Implements `rev-list` to enumerate commits reachable from revisions.

use std::collections::{HashMap, HashSet};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        commit::Commit,
        tree::{Tree, TreeItemMode},
    },
};

use crate::{
    command::load_object,
    internal::{branch::Branch, config::ConfigKv, head::Head, tag},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[path = "rev_list_cherry.rs"]
mod rev_list_cherry;
#[path = "rev_list_children.rs"]
mod rev_list_children;
#[path = "rev_list_filter.rs"]
mod rev_list_filter;
#[path = "rev_list_output.rs"]
mod rev_list_output;
#[path = "rev_list_spec.rs"]
mod rev_list_spec;

use rev_list_cherry::{RevListSelectedCommit, apply_cherry_filters, attach_cherry_metadata};
use rev_list_children::build_rev_list_children;
#[cfg(test)]
use rev_list_filter::ParentCountFilter;
use rev_list_filter::{
    commit_matches_author, commit_matches_committer, commit_matches_message,
    commit_matches_parent_count, commit_matches_time_window, filter_commits_by_pathspecs,
    parent_count_filter, rev_list_author_filter, rev_list_committer_filter,
    rev_list_message_filter, rev_list_time_window, sort_rev_list_commits,
};
use rev_list_output::{
    REV_LIST_EXAMPLES, RevListEntry, RevListObject, RevListOutput, emit_human_rev_list,
};
use rev_list_spec::resolve_revision_selection;

#[derive(Parser, Debug)]
#[command(after_help = REV_LIST_EXAMPLES)]
pub struct RevListArgs {
    /// Limit output to at most N commits
    #[clap(short = 'n', long = "max-count", value_name = "N")]
    pub max_count: Option<usize>,

    /// Skip the first N commits before output or counting
    #[clap(long, value_name = "N", default_value_t = 0)]
    pub skip: usize,

    /// Output the selected commits in reverse order. Commit limiting
    /// (`--max-count`/`--skip`) is applied first, then the result is reversed.
    #[clap(long)]
    pub reverse: bool,

    /// Pretend as if all refs (branches, remote-tracking branches, and
    /// tags) and the current HEAD are listed as `<SPEC>`, in addition to any
    /// explicit revisions.
    #[clap(long)]
    pub all: bool,

    /// Show commits in committer-date order (newest first). This is Libra's
    /// default ordering, so the flag is accepted for Git compatibility and
    /// makes the ordering explicit. Libra does not additionally enforce Git's
    /// topo constraint, which is only observable under committer-date skew.
    #[clap(long)]
    pub date_order: bool,

    /// Print only the number of commits after filters
    #[clap(long)]
    pub count: bool,

    /// Print parent commit IDs after each commit
    #[clap(long, conflicts_with = "children")]
    pub parents: bool,

    /// Print child commit IDs after each commit
    #[clap(long)]
    pub children: bool,

    /// Prefix each output line with the commit timestamp
    #[clap(long)]
    pub timestamp: bool,

    /// Follow only the first parent of merge commits
    #[clap(long = "first-parent")]
    pub first_parent: bool,

    /// Also print the boundary commits at the frontier — the parents of a listed
    /// commit that are not themselves listed (excluded by a `^spec`/range start, or
    /// beyond a `--max-count`/`--skip` cut) — each prefixed with `-`. Normally placed
    /// after the listed commits; under `--reverse` the whole stream is reversed, so
    /// they lead. Boundary rows carry `--parents`/`--children`/`--timestamp` metadata.
    #[clap(long)]
    pub boundary: bool,

    /// Filter commits by author name or email
    #[clap(long, value_name = "PATTERN")]
    pub author: Option<String>,

    /// Filter commits by committer name or email
    #[clap(long, value_name = "PATTERN")]
    pub committer: Option<String>,

    /// Filter commits by message using a regular expression
    #[clap(long, value_name = "PATTERN")]
    pub grep: Vec<String>,

    /// Prefix symmetric-difference commits with '<' or '>'
    #[clap(long = "left-right")]
    pub left_right: bool,

    /// Show only the left side of a symmetric difference
    #[clap(long = "left-only", conflicts_with = "right_only")]
    pub left_only: bool,

    /// Show only the right side of a symmetric difference
    #[clap(long = "right-only", conflicts_with = "left_only")]
    pub right_only: bool,

    /// Omit patch-equivalent commits across symmetric-difference sides
    #[clap(long = "cherry-pick", conflicts_with = "cherry_mark")]
    pub cherry_pick: bool,

    /// Mark patch-equivalent commits with '=' and others with '+'
    #[clap(long = "cherry-mark", conflicts_with = "cherry_pick")]
    pub cherry_mark: bool,

    /// Show right-side commits and mark patch-equivalent commits
    #[clap(long = "cherry")]
    pub cherry: bool,

    /// Show commits more recent than DATE
    #[clap(long, visible_alias = "after", value_name = "DATE")]
    pub since: Option<String>,

    /// Show commits older than DATE
    #[clap(long, visible_alias = "before", value_name = "DATE")]
    pub until: Option<String>,

    /// Print only commits with at least two parents
    #[clap(long)]
    pub merges: bool,

    /// Omit commits with at least two parents
    #[clap(long = "no-merges")]
    pub no_merges: bool,

    /// Print only commits with at least N parents
    #[clap(long = "min-parents", value_name = "N")]
    pub min_parents: Option<usize>,

    /// Print only commits with at most N parents
    #[clap(long = "max-parents", value_name = "N")]
    pub max_parents: Option<usize>,

    /// Clear the lower parent-count bound
    #[clap(long = "no-min-parents")]
    pub no_min_parents: bool,

    /// Clear the upper parent-count bound
    #[clap(long = "no-max-parents")]
    pub no_max_parents: bool,

    /// Also list the tree and blob objects reachable from the printed commits
    /// (deduplicated), each as `<oid> <path>` after the commit lines.
    #[clap(long)]
    pub objects: bool,

    /// Like `--objects`, and additionally print the excluded boundary commits
    /// (the frontier) prefixed with `-` so a pack builder can treat them as
    /// edges. Implies `--objects`.
    #[clap(long = "objects-edge")]
    pub objects_edge: bool,

    /// Accepted as an alias of `--objects-edge`. Git's aggressive variant marks
    /// more edge commits to build thinner packs; Libra emits the same boundary
    /// frontier (a documented narrowing). Implies `--objects`.
    #[clap(long = "objects-edge-aggressive")]
    pub objects_edge_aggressive: bool,

    /// Revisions to include or exclude. Defaults to HEAD when omitted.
    #[clap(value_name = "SPEC")]
    pub specs: Vec<String>,

    /// Paths to limit the commit list after an explicit `--` separator
    #[clap(last = true, value_name = "PATH")]
    pub pathspecs: Vec<String>,
}

pub async fn execute(args: RevListArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

pub async fn execute_safe(args: RevListArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // `--count --objects` adds the enumerated objects to a single total, which has
    // no meaning alongside the per-side / cherry-equivalence count buckets of
    // `--left-right` / `--cherry-mark` / `--cherry` (objects carry no side). Git
    // rejects this combination; so does Libra, rather than report an inflated
    // first bucket.
    let want_objects = args.objects || args.objects_edge || args.objects_edge_aggressive;
    if want_objects && args.count && (args.left_right || args.cherry_mark || args.cherry) {
        return Err(CliError::command_usage(
            "rev-list --count with object enumeration (--objects/--objects-edge) cannot be combined with --left-right, --cherry-mark, or --cherry",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("drop --objects, or count without the marked-count modes"));
    }

    let result = resolve_rev_list(&args).await?;

    if output.is_json() {
        emit_json_data("rev-list", &result, output)
    } else {
        emit_human_rev_list(output, &result)
    }
}

async fn resolve_rev_list(args: &RevListArgs) -> CliResult<RevListOutput> {
    // `--all` seeds the walk with every ref tip; explicit specs (incl `^`
    // exclusions) are appended so they still apply.
    let specs = if args.all {
        let mut specs = all_ref_specs().await?;
        specs.extend(args.specs.iter().cloned());
        specs
    } else {
        args.specs.clone()
    };
    // `--all` supplies the ref set as the input; don't fall back to HEAD when
    // that set (plus explicit specs) is empty (e.g. an unborn repository).
    let selection = resolve_revision_selection(&specs, args.first_parent, !args.all).await?;
    let mut commits = selection.commits;
    sort_rev_list_commits(&mut commits);
    let children = build_rev_list_children(&commits);
    let commits = filter_commits_by_pathspecs(commits, &args.pathspecs).await?;
    let commits = attach_cherry_metadata(commits, &selection.sides);
    let commits = apply_cherry_filters(commits, args)?;
    let time_window = rev_list_time_window(args)?;
    let author_filter = rev_list_author_filter(args);
    let committer_filter = rev_list_committer_filter(args);
    let message_filter = rev_list_message_filter(args)?;
    let parent_filter = parent_count_filter(args);

    let mut commits = commits
        .into_iter()
        .filter(|selected| commit_matches_author(&selected.commit, author_filter.as_deref()))
        .filter(|selected| commit_matches_committer(&selected.commit, committer_filter.as_deref()))
        .filter(|selected| commit_matches_message(&selected.commit, message_filter.as_ref()))
        .filter(|selected| commit_matches_time_window(&selected.commit, time_window))
        .filter(|selected| commit_matches_parent_count(&selected.commit, parent_filter))
        .skip(args.skip)
        .take(args.max_count.unwrap_or(usize::MAX))
        .collect::<Vec<_>>();
    // `--boundary`: the frontier — the parents of a listed commit that are NOT
    // themselves listed (excluded by a `^spec`/range start, or beyond the
    // `--max-count`/`--skip` cut). Computed from the limited output set so limiting
    // yields the cut point (matching Git), honoring `--first-parent` for parent
    // rewriting, and formatted with the same metadata flags. Computed even under
    // `--count` because Git counts boundary commits in the total. Computed BEFORE the
    // `--reverse` flip so boundary child lists keep the un-reversed traversal order
    // (Git reverses only the output ROWS, not a boundary's child list).
    // `--objects-edge[-aggressive]` emits the excluded frontier commits with a
    // leading `-` (the pack-builder "edge" markers), which is exactly the
    // `--boundary` frontier — so compute it for those flags too.
    let want_objects = args.objects || args.objects_edge || args.objects_edge_aggressive;
    let want_edge = args.objects_edge || args.objects_edge_aggressive;
    let boundary = if args.boundary || want_edge {
        compute_boundary_entries(&commits, args)
    } else {
        Vec::new()
    };

    // `--objects`: enumerate the deduplicated tree + blob objects reachable from
    // the printed commits, in pre-order (root tree, then each subtree before its
    // contents) matching `git rev-list --objects`. Objects shared with excluded
    // commits are dropped and a `-- <pathspec>` limit prunes the walk.
    let objects = if want_objects {
        // Normalize pathspecs the same way the commit-level filter does
        // (`util::to_workdir_path`) so the object walk compares git-compatible
        // spellings (e.g. `./src`) against the workdir-relative tree paths.
        let normalized_pathspecs: Vec<String> = args
            .pathspecs
            .iter()
            .map(|spec| {
                util::to_workdir_path(spec)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        collect_rev_list_objects(&commits, &selection.excluded, &normalized_pathspecs)?
    } else {
        Vec::new()
    };
    // `--reverse` reverses the already-limited selection (Git applies commit
    // limiting first, then reverses for output). Order-independent `--count` is
    // unaffected; boundary row placement is handled in `human_lines`.
    if args.reverse {
        commits.reverse();
    }
    let count_fields = if args.count {
        let mut fields = rev_list_count_fields(&commits, args);
        // Git's `--count --boundary` includes the boundary commits, and
        // `--count --objects` counts the enumerated tree/blob objects too; both
        // carry no side, so they fall into the first (total / left) field. The
        // boundary frontier is counted ONLY when the user asked for `--boundary`,
        // not when it was computed solely as `--objects-edge` edge markers (Git
        // counts commits + objects for `--objects-edge`, not the edge commits).
        if let Some(first) = fields.first_mut() {
            if args.boundary {
                *first += boundary.len();
            }
            *first += objects.len();
        }
        fields
    } else {
        Vec::new()
    };
    let entries = if args.count
        || (!args.parents
            && !args.children
            && !args.timestamp
            && !args.left_right
            && !args.cherry_mark
            && !args.cherry)
    {
        None
    } else {
        Some(
            commits
                .iter()
                .map(|selected| RevListEntry {
                    commit: selected.commit.id.to_string(),
                    side: selected.side,
                    cherry_equivalent: (args.cherry_mark || args.cherry)
                        .then_some(selected.cherry_equivalent),
                    parents: if args.parents {
                        selected
                            .commit
                            .parent_commit_ids // SHALLOW-DISPLAY: --parents prints recorded ids
                            .iter()
                            .map(ToString::to_string)
                            .collect()
                    } else {
                        Vec::new()
                    },
                    children: if args.children {
                        children
                            .get(&selected.commit.id.to_string())
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    timestamp: args
                        .timestamp
                        .then_some(selected.commit.committer.timestamp),
                    boundary: false,
                })
                .collect(),
        )
    };
    let commits = commits
        .iter()
        .map(|selected| selected.commit.id.to_string())
        .collect::<Vec<_>>();
    let total = commits.len();

    Ok(RevListOutput {
        input: selection.input,
        inputs: selection.inputs,
        commits,
        boundary,
        objects,
        entries,
        total,
        count_fields,
        count_only: args.count,
        parents: args.parents,
        children: args.children,
        timestamp: args.timestamp,
        reverse: args.reverse,
        first_parent: args.first_parent,
        author: args.author.clone(),
        committer: args.committer.clone(),
        grep: args.grep.clone(),
        pathspecs: args.pathspecs.clone(),
        left_right: args.left_right,
        left_only: args.left_only,
        right_only: args.right_only,
        cherry_pick: args.cherry_pick,
        cherry_mark: args.cherry_mark,
        cherry: args.cherry,
        since: args.since.clone(),
        until: args.until.clone(),
        merges: args.merges,
        no_merges: args.no_merges,
        min_parents: args.min_parents,
        max_parents: args.max_parents,
        no_min_parents: args.no_min_parents,
        no_max_parents: args.no_max_parents,
        max_count: args.max_count,
        skip: args.skip,
    })
}

/// Enumerate the tree and blob objects reachable from the printed commits for
/// `--objects`, deduplicated by object id across the whole walk and matching
/// `git rev-list --objects`:
///
/// - Objects reachable from EXCLUDED commits (the parents of a printed commit
///   that are not themselves printed — e.g. the `A` side of `A..B`, or a `^spec`)
///   are pre-marked "uninteresting": their tree closures are loaded into `seen`
///   WITHOUT being emitted, so a range walk emits only the objects new to the
///   included side. The excluded-side seed is tolerant of missing objects (a
///   shallow boundary cannot be walked) — it just pre-marks what it can.
/// - Each printed commit's root tree is then walked in pre-order (the tree
///   itself, then each entry in tree order, recursing into a subtree immediately
///   after emitting it). Gitlink (`TreeItemMode::Commit`) entries are skipped, as
///   Git does. A corrupt/missing tree on the INCLUDED side is a hard error
///   (`LBR-REPO-002`): an object-enumeration plumbing command must not silently
///   emit an incomplete closure.
/// - With a `-- <pathspec>` limit, the walk is pruned to the trees on the path to
///   a pathspec plus everything under a matched pathspec; blobs are emitted only
///   when their path is under a pathspec. The root tree is always emitted (Git
///   does the same), so a pathspec narrows the object set but keeps the root.
///
/// Paths are worktree-relative; a root tree has an empty path (rendered as
/// `<oid> ` with a trailing space).
/// Tracks object enumeration state across all printed commits. `emitted` is the
/// per-oid output-dedup set (each object printed at most once); `fully_walked`
/// records trees whose ENTIRE subtree has already been covered (emitted or marked
/// uninteresting) so traversal can be pruned. The two are kept separate because
/// under a `-- <pathspec>` limit the same tree object can be reached at one path
/// with a narrow scope and at another with a broader scope: deduping emission by
/// itself must NOT prune the second, broader traversal.
struct ObjectWalk {
    emitted: HashSet<ObjectHash>,
    fully_walked: HashSet<ObjectHash>,
    out: Vec<RevListObject>,
}

fn collect_rev_list_objects(
    commits: &[RevListSelectedCommit],
    excluded_ids: &HashSet<String>,
    pathspecs: &[String],
) -> CliResult<Vec<RevListObject>> {
    let mut walk = ObjectWalk {
        emitted: HashSet::new(),
        fully_walked: HashSet::new(),
        out: Vec::new(),
    };

    // Pre-mark the uninteresting closure from the EXPLICITLY excluded commits
    // (`^spec` / range start / `...` merge base) — their full reachability
    // closure as computed by revision resolution. Each excluded commit's tree
    // closure is marked covered WITHOUT being emitted, so the interesting walk
    // emits only objects new to the included side. Commits merely omitted by
    // `--max-count`/`--skip`/filters are NOT here, so their objects are NOT
    // suppressed (matching Git).
    for id in excluded_ids {
        if let Ok(commit_id) = id.parse::<ObjectHash>() {
            seed_uninteresting_objects(commit_id, &mut walk);
        }
    }

    let fully_included = pathspecs.is_empty();
    for selected in commits {
        collect_tree_objects(
            selected.commit.tree_id,
            String::new(),
            fully_included,
            pathspecs,
            &mut walk,
        )?;
    }
    Ok(walk.out)
}

/// Load an excluded commit and mark its entire tree closure as covered WITHOUT
/// emitting it, so the interesting walk skips objects shared with the excluded
/// side. Tolerant of missing objects (a shallow boundary): it pre-marks what it
/// can and gives up on what it cannot load.
fn seed_uninteresting_objects(commit_id: ObjectHash, walk: &mut ObjectWalk) {
    let Ok(commit) = load_object::<Commit>(&commit_id) else {
        return;
    };
    seed_uninteresting_tree(commit.tree_id, walk);
}

fn seed_uninteresting_tree(tree_id: ObjectHash, walk: &mut ObjectWalk) {
    if walk.fully_walked.contains(&tree_id) {
        return;
    }
    // Load BEFORE marking covered: a tree we cannot load must NOT be recorded as
    // uninteresting, otherwise the interesting walk would skip it (returning a
    // truncated listing) instead of raising the corruption error. Trees form a
    // DAG, so deferring the insert cannot cause infinite recursion.
    let Ok(tree) = load_object::<Tree>(&tree_id) else {
        return;
    };
    walk.emitted.insert(tree_id);
    walk.fully_walked.insert(tree_id);
    for item in &tree.tree_items {
        match item.mode {
            TreeItemMode::Tree => seed_uninteresting_tree(item.id, walk),
            TreeItemMode::Commit => {} // gitlink — not an object Libra stores
            _ => {
                walk.emitted.insert(item.id);
            }
        }
    }
}

fn collect_tree_objects(
    tree_id: ObjectHash,
    prefix: String,
    fully_included: bool,
    pathspecs: &[String],
    walk: &mut ObjectWalk,
) -> CliResult<()> {
    // A fully-covered tree (whole subtree already emitted/uninteresting) needs no
    // re-traversal. A merely-`emitted` tree may still need re-walking at a
    // broader pathspec scope, so we do NOT prune on `emitted` alone.
    if walk.fully_walked.contains(&tree_id) {
        return Ok(());
    }
    if walk.emitted.insert(tree_id) {
        walk.out.push(RevListObject {
            oid: tree_id.to_string(),
            path: prefix.clone(),
        });
    }
    let tree = load_object::<Tree>(&tree_id).map_err(|error| {
        CliError::fatal(format!(
            "failed to load tree object {tree_id} for rev-list --objects: {error}"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    for item in &tree.tree_items {
        let path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{prefix}/{}", item.name)
        };
        match item.mode {
            TreeItemMode::Tree => {
                // Descend into a subtree when it is fully included, sits under a
                // pathspec, or is an ancestor of one (so we reach matches below).
                if fully_included || pathspec_descend_tree(&path, pathspecs) {
                    let child_full = fully_included || pathspec_matches(&path, pathspecs);
                    collect_tree_objects(item.id, path, child_full, pathspecs, walk)?;
                }
            }
            // Gitlink/submodule pointer: Git omits these from `--objects`, and the
            // commit object it names is not part of this tree's object closure.
            TreeItemMode::Commit => {}
            _ => {
                if (fully_included || pathspec_matches(&path, pathspecs))
                    && walk.emitted.insert(item.id)
                {
                    walk.out.push(RevListObject {
                        oid: item.id.to_string(),
                        path,
                    });
                }
            }
        }
    }
    // Only a fully-included pass guarantees the whole subtree was emitted; a
    // pathspec-narrowed pass may have skipped siblings, so it must not prune a
    // later broader visit to the same tree object.
    if fully_included {
        walk.fully_walked.insert(tree_id);
    }
    Ok(())
}

/// A path is "under" a pathspec when it equals, or is nested below, one of the
/// limit paths (a trailing `/` on the pathspec is ignored; `.`/`` match all).
fn pathspec_matches(path: &str, pathspecs: &[String]) -> bool {
    pathspecs.iter().any(|spec| {
        let spec = spec.trim_end_matches('/');
        spec.is_empty() || spec == "." || path == spec || path.starts_with(&format!("{spec}/"))
    })
}

/// A tree should be descended into when it is under a pathspec OR an ancestor of
/// one (i.e. some pathspec lives inside it), so the walk reaches matches below
/// while pruning unrelated sibling subtrees.
fn pathspec_descend_tree(path: &str, pathspecs: &[String]) -> bool {
    if path.is_empty() {
        return true; // the root tree is the ancestor of every pathspec
    }
    pathspecs.iter().any(|spec| {
        let spec = spec.trim_end_matches('/');
        spec.is_empty()
            || spec == "."
            || path == spec
            || path.starts_with(&format!("{spec}/"))
            || spec.starts_with(&format!("{path}/"))
    })
}

/// Compute the `--boundary` frontier from the FINAL output set: the parents of a
/// listed commit that are not themselves listed (because they were excluded by a
/// `^spec`/range start, or fall beyond the `--max-count`/`--skip` cut). This matches
/// Git's "parents of returned commits that are not themselves returned" rule, so
/// limiting yields the cut point rather than the original range start.
///
/// All parents (not just first) of a listed commit that are not listed count as
/// boundary commits — verified against `git rev-list --first-parent --boundary`,
/// which marks BOTH parents of a merge at the frontier (the un-walked second parent
/// becomes a boundary). The `--first-parent` effect on the boundary SET is already
/// captured by the smaller output set the walk produces, not by restricting parent
/// selection here. Returns boundary `RevListEntry` rows in committer-date-descending
/// order (id tiebreak), carrying `--parents`/`--children`/`--timestamp` metadata so
/// they format identically to listed commits. Parents that fail to load are skipped.
///
/// Two Git-faithfulness nuances on merges (verified against git 2.x):
/// - `--children`: a boundary's children are derived from the output set (the listed
///   commits naming it as a parent), iterated oldest-first to match Git's order.
/// - `--first-parent --parents`: Git prints NO parents for a boundary that was never
///   entered by the walk (an un-walked second parent); only first-parent boundaries
///   keep their parents.
fn compute_boundary_entries(
    output: &[RevListSelectedCommit],
    args: &RevListArgs,
) -> Vec<RevListEntry> {
    let output_ids: HashSet<String> = output
        .iter()
        .map(|selected| selected.commit.id.to_string())
        .collect();

    // Children for boundary commits must be derived from the OUTPUT set: a boundary
    // commit is by definition NOT in the selected set, so the traversal child map
    // (which only records edges whose parent is selected) would yield none. Here a
    // boundary commit's children are exactly the listed commits that name it as a
    // parent — matching `git rev-list --boundary --children`. The output is iterated
    // in REVERSE (oldest-first) so multi-child boundary lists match Git's ordering.
    let mut boundary_children: HashMap<String, Vec<String>> = HashMap::new();
    if args.children {
        for selected in output.iter().rev() {
            let child_id = selected.commit.id.to_string();
            for parent in &selected.commit.parent_commit_ids {
                // SHALLOW-DISPLAY: --boundary/--children edges among shown commits
                let pid = parent.to_string();
                if !output_ids.contains(&pid) {
                    boundary_children
                        .entry(pid)
                        .or_default()
                        .push(child_id.clone());
                }
            }
        }
    }

    // `--first-parent --parents`: Git rewrites away the parents of a boundary that was
    // never entered by the walk (an un-walked second parent of a merge), printing it
    // bare. Only boundaries reached AS the first parent of a listed commit keep their
    // parents. Without `--first-parent`, every boundary shows its real parents.
    let first_parent_boundary: HashSet<String> = if args.first_parent && args.parents {
        output
            .iter()
            .filter_map(|selected| selected.commit.parent_commit_ids.first()) // SHALLOW-DISPLAY
            .map(ToString::to_string)
            .filter(|pid| !output_ids.contains(pid))
            .collect()
    } else {
        HashSet::new()
    };

    let mut seen = HashSet::new();
    let mut boundary: Vec<Commit> = Vec::new();
    for selected in output {
        for parent in &selected.commit.parent_commit_ids {
            // SHALLOW-DISPLAY: --boundary lists recorded missing-side parents
            let pid = parent.to_string();
            if !output_ids.contains(&pid)
                && seen.insert(pid.clone())
                && let Ok(parent_commit) = load_object::<Commit>(parent)
            {
                boundary.push(parent_commit);
            }
        }
    }
    boundary.sort_by(|a, b| {
        b.committer
            .timestamp
            .cmp(&a.committer.timestamp)
            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
    });
    boundary
        .into_iter()
        .map(|commit| RevListEntry {
            commit: commit.id.to_string(),
            side: None,
            cherry_equivalent: None,
            parents: if args.parents
                && (!args.first_parent || first_parent_boundary.contains(&commit.id.to_string()))
            {
                commit
                    .parent_commit_ids // SHALLOW-DISPLAY: boundary --parents metadata
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            } else {
                Vec::new()
            },
            children: if args.children {
                boundary_children
                    .get(&commit.id.to_string())
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            timestamp: args.timestamp.then_some(commit.committer.timestamp),
            boundary: true,
        })
        .collect()
}

/// Collect a resolvable spec for every ref (local branches, remote-tracking
/// branches, and tags) for `--all`. Branches/remote-tracking refs contribute
/// their tip commit hash directly (unambiguous); tags contribute their name so
/// the normal spec resolver peels annotated tags. The resolver de-duplicates
/// the resulting commits.
async fn all_ref_specs() -> CliResult<Vec<String>> {
    let mut specs = Vec::new();

    // Git's `--all` seeds from every ref in refs/ AND the current HEAD, so a
    // detached-HEAD commit not pointed to by any branch/tag is still walked.
    // An unborn HEAD (None) contributes nothing; the resolver de-duplicates a
    // HEAD that coincides with a branch tip.
    if let Some(head_commit) = Head::current_commit_result().await.map_err(|source| {
        CliError::fatal(format!("failed to resolve HEAD: {source}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        specs.push(head_commit.to_string());
    }

    let branches = Branch::list_branches_result(None).await.map_err(|source| {
        CliError::fatal(format!("failed to list branches: {source}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for branch in branches {
        specs.push(branch.commit.to_string());
    }

    let remotes = ConfigKv::all_remote_configs().await.map_err(|source| {
        CliError::fatal(format!("failed to list remotes: {source}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for remote in remotes {
        let remote_branches = Branch::list_branches_result(Some(&remote.name))
            .await
            .map_err(|source| {
                CliError::fatal(format!("failed to list remote branches: {source}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
        for branch in remote_branches {
            specs.push(branch.commit.to_string());
        }
    }

    let tags = tag::list().await.map_err(|source| {
        CliError::fatal(format!("failed to list tags: {source}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for t in tags {
        // Seed by the ref target's object id (unambiguous) rather than the tag
        // name — a same-named branch would otherwise shadow the tag and drop
        // its tag-only commits. The resolver peels annotated-tag objects.
        let oid = match &t.object {
            tag::TagObject::Commit(commit) => commit.id,
            tag::TagObject::Tag(tag_obj) => tag_obj.id,
            tag::TagObject::Tree(tree) => tree.id,
            tag::TagObject::Blob(blob) => blob.id,
        };
        specs.push(oid.to_string());
    }

    Ok(specs)
}

fn rev_list_count_fields(commits: &[RevListSelectedCommit], args: &RevListArgs) -> Vec<usize> {
    if args.left_right && (args.cherry_mark || args.cherry) {
        return vec![
            side_count(commits, rev_list_spec::RevListSide::Left, false),
            side_count(commits, rev_list_spec::RevListSide::Right, false),
            commits
                .iter()
                .filter(|selected| selected.cherry_equivalent)
                .count(),
        ];
    }

    if args.left_right {
        return vec![
            side_total(commits, rev_list_spec::RevListSide::Left),
            side_total(commits, rev_list_spec::RevListSide::Right),
        ];
    }

    if args.cherry_mark || args.cherry {
        return vec![
            commits
                .iter()
                .filter(|selected| !selected.cherry_equivalent)
                .count(),
            commits
                .iter()
                .filter(|selected| selected.cherry_equivalent)
                .count(),
        ];
    }

    vec![commits.len()]
}

fn side_total(commits: &[RevListSelectedCommit], side: rev_list_spec::RevListSide) -> usize {
    commits
        .iter()
        .filter(|selected| selected.side == Some(side))
        .count()
}

fn side_count(
    commits: &[RevListSelectedCommit],
    side: rev_list_spec::RevListSide,
    cherry_equivalent: bool,
) -> usize {
    commits
        .iter()
        .filter(|selected| {
            selected.side == Some(side) && selected.cherry_equivalent == cherry_equivalent
        })
        .count()
}

#[cfg(test)]
#[path = "rev_list_output_tests.rs"]
mod output_tests;
#[cfg(test)]
#[path = "rev_list_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "rev_list_write_tests.rs"]
mod write_tests;
