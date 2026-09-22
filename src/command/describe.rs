//! Implementation of `describe` command, which finds the most recent tag reachable from a commit.
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{commit::Commit, types::ObjectType},
};

use crate::{
    command::{load_object, status},
    internal::{
        branch::Branch,
        config::ConfigKv,
        shallow::ShallowSet,
        tag::{self, TagObject},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[path = "describe_format.rs"]
mod describe_format;
#[path = "describe_types.rs"]
mod describe_types;
use describe_format::{abbreviate_hash, describe_output};
use describe_types::{DescribeError, DescribeOutput};

const DESCRIBE_EXAMPLES: &str = "\
EXAMPLES:
    libra describe                  Describe HEAD using the nearest annotated tag
    libra describe --tags           Include lightweight tags (not just annotated ones) in the search
    libra describe --all            Use any ref (branches/remotes/tags), shown with heads/remotes/tags prefix
    libra describe --always         Fall back to abbreviated commit hash when no tag matches
    libra describe --exact-match    Only succeed when HEAD exactly matches a tag
    libra describe --long           Force tag-0-gHASH form for exact tag matches
    libra describe --dirty          Append -dirty when tracked content differs from HEAD
    libra describe --first-parent   Follow only the first parent of merge commits when walking history
    libra describe --match 'v1.*'   Only consider tags whose name matches the glob
    libra describe --exclude '*rc*' Skip tags whose name matches the glob
    libra describe HEAD~1           Describe a specific commit-ish (hash, ref, or HEAD~N)
    libra describe --candidates 0   Only succeed on an exact tag match (like --exact-match)
    libra describe --abbrev 12      Use 12 hex digits instead of the default 7 in the hash portion
    libra describe --contains HEAD~2   Name a commit by its nearest descendant tag (e.g. v1.0~2)
    libra describe --json           Structured JSON output for agents";

/// Maximum byte length accepted for a `--match`/`--exclude` glob pattern, guarding
/// against pathological inputs. Longer patterns are rejected up front with
/// [`DescribeError::InvalidArgument`] (`CliInvalidArguments`, exit 129).
const MAX_GLOB_LEN: usize = 256;

#[derive(Parser, Debug)]
#[command(after_help = DESCRIBE_EXAMPLES)]
pub struct DescribeArgs {
    /// Commit-ish (hash, ref, or tag) to describe. Defaults to HEAD
    pub commit: Option<String>,

    /// Consider any tag in refs/tags (not just annotated tags) when describing
    #[clap(long)]
    pub tags: bool,

    /// Use any ref (local branches, remote-tracking branches, and tags), not
    /// just tags; names are shown with their `heads/`, `remotes/`, or `tags/`
    /// prefix (Git's `--all`).
    #[clap(long)]
    pub all: bool,

    /// Use N hex digits for the abbreviated commit hash (default: 7)
    #[clap(long, value_name = "N")]
    pub abbrev: Option<usize>,

    /// Show an abbreviated commit hash when no tag can describe the target.
    #[clap(long)]
    pub always: bool,

    /// Only output exact tag matches.
    #[clap(long)]
    pub exact_match: bool,

    /// N=0 requires an exact tag match; N>=1 keeps the deterministic nearest-tag search.
    #[clap(long, value_name = "N")]
    pub candidates: Option<usize>,

    /// Always output the long format when a tag describes the target.
    #[clap(long)]
    pub long: bool,

    /// Append MARK when tracked content differs from HEAD.
    #[clap(long, value_name = "MARK", num_args = 0..=1, require_equals = true, default_missing_value = "-dirty")]
    pub dirty: Option<String>,

    /// Follow only the first parent of merge commits when walking history.
    #[clap(long = "first-parent")]
    pub first_parent: bool,

    /// Only consider tags whose name matches the glob (repeatable; OR semantics).
    #[clap(long = "match", value_name = "PATTERN")]
    pub match_patterns: Vec<String>,

    /// Exclude tags whose name matches the glob (repeatable; takes precedence over --match).
    #[clap(long, value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Find the tag that *contains* the commit (the nearest descendant tag),
    /// printing a `git name-rev`-style name (`<tag>`, `<tag>~<n>`, or
    /// `<tag>~<n>^<m>~<k>`), like `git describe --contains`.
    #[clap(long = "contains")]
    pub contains: bool,
}

// Entry in tag lookup map
struct TagInfo {
    name: String,
    is_annotated: bool,
}

pub async fn execute(args: DescribeArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
pub async fn execute_safe(args: DescribeArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let describe_output = run_describe(args).await.map_err(describe_cli_error)?;

    if output.is_json() {
        emit_json_data("describe", &describe_output, output)?;
    } else if !output.quiet {
        println!("{}", describe_output.result);
    }

    Ok(())
}

/// Describe a single commit for `for-each-ref`'s `%(describe)` atom.
///
/// Returns the describe string (e.g. `v1.0-2-gabc1234`), or `None` when no tag
/// is reachable from the commit — Git's `%(describe)` renders an empty string in
/// that case rather than failing. Mirrors the `git describe` options the atom
/// exposes: `tags` (include lightweight tags), `abbrev=<n>`, and repeatable
/// `match`/`exclude` glob filters. Genuine I/O or corruption errors propagate as
/// a `CliError`.
pub(crate) async fn describe_commit_for_atom(
    commit: &str,
    tags: bool,
    abbrev: Option<usize>,
    match_patterns: Vec<String>,
    exclude: Vec<String>,
) -> crate::utils::error::CliResult<Option<String>> {
    let args = DescribeArgs {
        commit: Some(commit.to_string()),
        tags,
        all: false,
        abbrev,
        always: false,
        exact_match: false,
        candidates: None,
        long: false,
        dirty: None,
        first_parent: false,
        match_patterns,
        exclude,
        contains: false,
    };
    match run_describe(args).await {
        Ok(output) => Ok(Some(output.result)),
        // No reachable tag for this commit -> empty `%(describe)` output, matching
        // Git (these are not hard failures in the for-each-ref context).
        Err(
            DescribeError::NoNamesFound
            | DescribeError::NoContainingTag { .. }
            | DescribeError::NoExactMatch { .. },
        ) => Ok(None),
        // Real failures (bad I/O, corrupt object, malformed abbrev) propagate.
        Err(other) => Err(describe_cli_error(other)),
    }
}

async fn run_describe(args: DescribeArgs) -> Result<DescribeOutput, DescribeError> {
    let input = args.commit.unwrap_or_else(|| "HEAD".to_string());
    let start_hash = util::get_commit_base_typed(&input)
        .await
        .map_err(DescribeError::from)?;
    let resolved_commit = start_hash.to_string();
    let abbrev = args.abbrev.unwrap_or(7);
    let long_format = args.long;
    if long_format && abbrev == 0 {
        return Err(DescribeError::LongWithAbbrevZero);
    }
    // `--all` considers every ref (branches + remotes + tags), which implies
    // even lightweight tags are candidates.
    let include_all = args.all;
    // `--contains` mirrors `git name-rev --tags`, which considers every tag in
    // refs/tags (annotated and lightweight), so it implies lightweight inclusion.
    let include_lightweight = args.tags || include_all || args.contains;
    // `--candidates 0` means "only exact matches", which is exactly the
    // `--exact-match` behavior (Git documents `--candidates 0` this way).
    let exact_match = args.exact_match || args.candidates == Some(0);
    let always = args.always;
    let dirty_mark = args.dirty;
    let first_parent = args.first_parent;
    let shallow = ShallowSet::load()
        .map_err(|error| DescribeError::CorruptReference(format!("{error}; {}", error.hint())))?;

    // Compile the --match / --exclude name filters once. Overly long or malformed
    // patterns are rejected up front as usage errors (CliInvalidArguments, 129).
    let matchers = compile_globs(&args.match_patterns)?;
    let excluders = compile_globs(&args.exclude)?;

    // 2. Load all tags and build a mapping table: commit hash -> tag info (name, is_annotated)
    let all_tags = tag::list()
        .await
        .map_err(|e| DescribeError::CorruptReference(e.to_string()))?;
    let mut tag_map: HashMap<ObjectHash, TagInfo> = HashMap::new();

    for t in all_tags {
        let is_annotated = t.object.get_type() == ObjectType::Tag;

        // Only include light-weight tags if --tags is specified
        if is_annotated || include_lightweight {
            // Apply --match / --exclude name filters to the bare tag name
            // (exclude wins over match), then prefix with `tags/` under --all.
            if !tag_passes_filters(&t.name, &matchers, &excluders) {
                continue;
            }
            let tag_name = if include_all {
                format!("tags/{}", t.name)
            } else {
                t.name
            };
            let target_commit_hash = match t.object {
                TagObject::Commit(c) => c.id,
                TagObject::Tag(tg) => tg.object_hash,
                _ => continue,
            };

            let should_replace = tag_map
                .get(&target_commit_hash)
                .is_none_or(|existing| prefer_tag(existing, &tag_name, is_annotated));
            if should_replace {
                tag_map.insert(
                    target_commit_hash,
                    TagInfo {
                        name: tag_name,
                        is_annotated,
                    },
                );
            }
        }
    }

    // Under --all, also consider local branches (`heads/<name>`) and
    // remote-tracking branches (`remotes/<remote>/<name>`) as candidates.
    // Tags take precedence at a shared commit (inserted first), then heads,
    // then remotes; `or_insert_with` never overrides an existing tag entry.
    if include_all {
        let mut locals = Branch::list_branches_result(None)
            .await
            .map_err(|e| DescribeError::CorruptReference(e.to_string()))?;
        locals.sort_by(|a, b| a.name.cmp(&b.name));
        for branch in locals {
            tag_map.entry(branch.commit).or_insert_with(|| TagInfo {
                name: format!("heads/{}", branch.name),
                is_annotated: false,
            });
        }

        for remote in describe_remote_names().await? {
            let mut remote_branches = Branch::list_branches_result(Some(&remote))
                .await
                .map_err(|e| DescribeError::CorruptReference(e.to_string()))?;
            remote_branches.sort_by(|a, b| a.name.cmp(&b.name));
            for branch in remote_branches {
                tag_map.entry(branch.commit).or_insert_with(|| TagInfo {
                    name: format!("remotes/{}/{}", remote, branch.name),
                    is_annotated: false,
                });
            }
        }
    }

    // `--contains` is the inverse query (git name-rev): find the nearest tag
    // that DESCENDS from the target and name the target relative to it. It uses
    // its own walk over `tag_map` rather than the ancestor BFS below.
    if args.contains {
        return run_describe_contains(
            input,
            start_hash,
            resolved_commit,
            &tag_map,
            first_parent,
            exact_match,
            dirty_mark,
        )
        .await;
    }

    // 3. Search for  the closest tag using BFS (to find the shortest distance)
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    // Queue storage format: (current_commit_hash, distance_from_start)
    queue.push_back((start_hash, 0));
    visited.insert(start_hash);

    while let Some((curr_hash, dist)) = queue.pop_front() {
        // Check if current commit has a matching tag
        if let Some(tag_info) = tag_map.get(&curr_hash) {
            let output = describe_output(
                input.clone(),
                resolved_commit.clone(),
                &tag_info.name,
                dist,
                abbrev,
                long_format,
            );
            return apply_dirty_mark(output, dirty_mark).await;
        }

        if exact_match {
            break;
        }

        // Load commit to find parents
        let commit =
            load_object::<Commit>(&curr_hash).map_err(|error| DescribeError::LoadCommit {
                commit_id: curr_hash.to_string(),
                detail: error.to_string(),
            })?;

        // With --first-parent only the first parent is followed, so merge commits
        // do not pull in their merged-in side history.
        let parents = shallow.parents_for_walk(&commit.id, &commit.parent_commit_ids);
        let parents: &[ObjectHash] = if first_parent {
            &parents[..parents.len().min(1)]
        } else {
            parents
        };
        for parent_id_str in parents {
            if !visited.contains(parent_id_str) {
                visited.insert(*parent_id_str);
                queue.push_back((*parent_id_str, dist + 1));
            }
        }
    }

    if exact_match {
        return Err(DescribeError::NoExactMatch {
            commit_id: resolved_commit,
        });
    }

    if always {
        let abbreviated = abbreviate_hash(&resolved_commit, abbrev);
        let output = DescribeOutput {
            input,
            resolved_commit,
            result: abbreviated.clone(),
            tag: None,
            distance: None,
            abbreviated_commit: Some(abbreviated),
            exact_match: false,
            used_always: true,
            long_format,
            dirty: false,
            dirty_mark: None,
        };
        return apply_dirty_mark(output, dirty_mark).await;
    }

    Err(DescribeError::NoNamesFound)
}

/// `git describe --contains` (name-rev): name the target relative to the nearest
/// tag that descends from it. A Dijkstra-style walk runs backward from every tag
/// commit; first-parent steps cost 1 and other-parent steps cost `MERGE_COST`,
/// so the straightest path from the closest descendant tag wins. The resulting
/// name is `<tag>` (the tag itself), `<tag>~<n>` (n first-parent steps), or a
/// `<tag>~<n>^<m>~<k>` chain when the path crosses a merge's non-first parent.
///
/// When `exact_match` is set (`--exact-match` or `--candidates 0`), only a tag
/// that points directly at the target — a bare `<tag>` name with no `~`/`^`
/// suffix — is accepted; any relative name fails with `NoExactMatch`, matching
/// the exact-match contract of the forward describe path.
async fn run_describe_contains(
    input: String,
    start_hash: ObjectHash,
    resolved_commit: String,
    tag_map: &HashMap<ObjectHash, TagInfo>,
    first_parent: bool,
    exact_match_only: bool,
    dirty_mark: Option<String>,
) -> Result<DescribeOutput, DescribeError> {
    // Under `--exact-match`/`--candidates 0` the only acceptable answer is a tag
    // that points directly at the target — a relative `<tag>~N`/`^M` name is not
    // an exact hit, and neither is "no descendant tag at all". Decide that up
    // front (the Dijkstra walk is irrelevant here) so every non-direct outcome
    // funnels to the same `NoExactMatch` the forward path returns.
    let shallow = ShallowSet::load()
        .map_err(|error| DescribeError::CorruptReference(format!("{error}; {}", error.hint())))?;

    if exact_match_only {
        return match tag_map.get(&start_hash) {
            Some(info) => {
                let output = DescribeOutput {
                    input,
                    resolved_commit,
                    result: info.name.clone(),
                    tag: Some(info.name.clone()),
                    distance: None,
                    abbreviated_commit: None,
                    exact_match: true,
                    used_always: false,
                    long_format: false,
                    dirty: false,
                    dirty_mark: None,
                };
                apply_dirty_mark(output, dirty_mark).await
            }
            None => Err(DescribeError::NoExactMatch {
                commit_id: resolved_commit,
            }),
        };
    }

    // A non-first-parent step costs far more than a first-parent step, so a
    // first-parent path is strongly preferred (matching git name-rev's metric).
    const MERGE_COST: u64 = 65_535;

    // Heap entries are (weight, seq); `nodes`/`states` hold the commit and its
    // propagation state (base name + first-parent generation) for that seq.
    let mut heap: BinaryHeap<Reverse<(u64, u64)>> = BinaryHeap::new();
    let mut nodes: Vec<ObjectHash> = Vec::new();
    let mut states: Vec<(String, u32)> = Vec::new();
    // A commit is finalized the first time it is popped (lowest weight).
    let mut finalized: HashSet<ObjectHash> = HashSet::new();

    // Seed every tag commit as a name-rev source. Iterate in a deterministic
    // (tag-name) order so equal-weight ties — two tags equidistant from the
    // target — resolve to a stable, reproducible name rather than HashMap order.
    let mut seeds: Vec<(&ObjectHash, &TagInfo)> = tag_map.iter().collect();
    seeds.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    for (commit, info) in seeds {
        let seq = nodes.len() as u64;
        nodes.push(*commit);
        states.push((info.name.clone(), 0));
        heap.push(Reverse((0, seq)));
    }

    while let Some(Reverse((weight, seq))) = heap.pop() {
        let commit = nodes[seq as usize];
        if !finalized.insert(commit) {
            continue; // already finalized with a weight <= this one
        }
        let (base, generation) = states[seq as usize].clone();
        let display = if generation == 0 {
            base.clone()
        } else {
            format!("{base}~{generation}")
        };

        if commit == start_hash {
            // Tag names cannot contain '~' or '^', so the base tag is the prefix
            // up to the first of those.
            let tag = display
                .split(['~', '^'])
                .next()
                .unwrap_or(&display)
                .to_string();
            // `exact_match_only` is handled up front, so here the name may be
            // relative; `exact_match` is true only when the target carries the
            // tag directly (display has no `~`/`^` suffix).
            let exact_match = display == tag;
            let output = DescribeOutput {
                input,
                resolved_commit,
                result: display,
                tag: Some(tag),
                distance: None,
                abbreviated_commit: None,
                exact_match,
                used_always: false,
                long_format: false,
                dirty: false,
                dirty_mark: None,
            };
            return apply_dirty_mark(output, dirty_mark).await;
        }

        let commit_obj =
            load_object::<Commit>(&commit).map_err(|error| DescribeError::LoadCommit {
                commit_id: commit.to_string(),
                detail: error.to_string(),
            })?;
        for (i, parent) in shallow
            .parents_for_walk(&commit_obj.id, &commit_obj.parent_commit_ids)
            .iter()
            .enumerate()
        {
            if first_parent && i > 0 {
                break;
            }
            if finalized.contains(parent) {
                continue;
            }
            let (new_weight, new_base, new_gen) = if i == 0 {
                (weight + 1, base.clone(), generation + 1)
            } else {
                (weight + MERGE_COST, format!("{display}^{}", i + 1), 0)
            };
            let next = nodes.len() as u64;
            nodes.push(*parent);
            states.push((new_base, new_gen));
            heap.push(Reverse((new_weight, next)));
        }
    }

    // No tag descends from the target.
    Err(DescribeError::NoContainingTag {
        commit_id: resolved_commit,
    })
}

async fn apply_dirty_mark(
    mut output: DescribeOutput,
    dirty_mark: Option<String>,
) -> Result<DescribeOutput, DescribeError> {
    if let Some(mark) = dirty_mark
        && has_tracked_dirty_changes().await?
    {
        output.result.push_str(&mark);
        output.dirty = true;
        output.dirty_mark = Some(mark);
    }

    Ok(output)
}

async fn has_tracked_dirty_changes() -> Result<bool, DescribeError> {
    let staged = status::changes_to_be_committed_safe()
        .await
        .map_err(|error| DescribeError::ReadFailure(format!("{error}")))?;
    if !staged.is_empty() {
        return Ok(true);
    }

    let unstaged = status::changes_to_be_staged()
        .map_err(|error| DescribeError::ReadFailure(format!("{error}")))?;
    Ok(!unstaged.modified.is_empty()
        || !unstaged.deleted.is_empty()
        || !unstaged.renamed.is_empty())
}

/// Compile `--match`/`--exclude` glob patterns, rejecting overly long or malformed
/// patterns with [`DescribeError::InvalidArgument`] (`CliInvalidArguments`, exit 129).
/// Returned globs borrow `patterns`, so the slice must outlive the filter loop.
fn compile_globs(patterns: &[String]) -> Result<Vec<wax::Glob<'_>>, DescribeError> {
    let mut globs = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        if pattern.len() > MAX_GLOB_LEN {
            return Err(DescribeError::InvalidArgument(format!(
                "glob pattern too long ({} chars); the limit is {MAX_GLOB_LEN}",
                pattern.len()
            )));
        }
        let glob = wax::Glob::new(pattern.as_str()).map_err(|error| {
            DescribeError::InvalidArgument(format!("invalid glob pattern '{pattern}': {error}"))
        })?;
        globs.push(glob);
    }
    Ok(globs)
}

/// Whether a tag name survives the `--match`/`--exclude` filters. An exclude match
/// always rejects; with no `--match` patterns every non-excluded name passes,
/// otherwise the name must match at least one `--match` glob.
fn tag_passes_filters(name: &str, matchers: &[wax::Glob<'_>], excluders: &[wax::Glob<'_>]) -> bool {
    if excluders
        .iter()
        .any(|glob| wax::Program::is_match(glob, name))
    {
        return false;
    }
    if matchers.is_empty() {
        return true;
    }
    matchers
        .iter()
        .any(|glob| wax::Program::is_match(glob, name))
}

/// Enumerate configured remote names (for `--all`) from `remote.<name>.*`
/// config keys, mirroring `remote`'s own listing. Returns a sorted, de-duped
/// list so describe output is deterministic.
async fn describe_remote_names() -> Result<Vec<String>, DescribeError> {
    let entries = ConfigKv::get_by_prefix("remote.")
        .await
        .map_err(|e| DescribeError::CorruptReference(e.to_string()))?;
    let mut names = std::collections::BTreeSet::new();
    for entry in entries {
        if let Some(rest) = entry.key.strip_prefix("remote.")
            && let Some((name, _subkey)) = rest.rsplit_once('.')
        {
            names.insert(name.to_string());
        }
    }
    Ok(names.into_iter().collect())
}

fn prefer_tag(existing: &TagInfo, candidate_name: &str, candidate_is_annotated: bool) -> bool {
    match (existing.is_annotated, candidate_is_annotated) {
        (false, true) => true,
        (true, false) => false,
        _ => candidate_name < existing.name.as_str(),
    }
}

fn describe_cli_error(error: DescribeError) -> CliError {
    match error {
        DescribeError::HeadUnborn => CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("create a commit before running 'libra describe'."),
        DescribeError::InvalidReference(message) => CliError::command_usage(message)
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("check the revision and try again."),
        DescribeError::ReadFailure(message) => {
            CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
        }
        DescribeError::CorruptReference(message) => {
            CliError::fatal(message).with_stable_code(StableErrorCode::RepoCorrupt)
        }
        DescribeError::NoNamesFound => CliError::fatal("no names found, cannot describe anything")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(
                "create a tag, pass '--tags' to include lightweight tags, or use '--always'.",
            ),
        DescribeError::NoContainingTag { commit_id } => {
            CliError::fatal(format!("cannot describe '{commit_id}': no tag contains it"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint(
                    "`--contains` names a commit by a tag that descends from it; create or fetch a tag on a descendant commit.",
                )
        }
        DescribeError::NoExactMatch { commit_id } => {
            CliError::fatal(format!("no tag exactly matches '{commit_id}'"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("move to a tagged commit or omit '--exact-match'.")
        }
        DescribeError::LongWithAbbrevZero => CliError::command_usage(error.to_string())
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("omit '--long' or choose a positive '--abbrev <N>'."),
        DescribeError::InvalidArgument(message) => CliError::command_usage(message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("check the --match/--exclude glob syntax."),
        DescribeError::LoadCommit { commit_id, detail } => {
            CliError::fatal(format!("failed to load commit '{commit_id}': {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
