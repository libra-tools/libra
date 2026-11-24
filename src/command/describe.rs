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
use describe_types::{DescribeError, DescribeOutput, WorktreeCheck};

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
    #[clap(long, value_name = "N", overrides_with = "no_abbrev")]
    pub abbrev: Option<usize>,

    /// Show the full commit hash (Git's `--abbrev=0`; last one wins)
    #[clap(long, overrides_with = "abbrev")]
    pub no_abbrev: bool,

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
    #[clap(long, overrides_with = "no_long")]
    pub long: bool,

    /// Cancel an earlier `--long` (last one wins, like Git).
    #[clap(long, overrides_with = "long")]
    pub no_long: bool,

    /// Append MARK when tracked content differs from HEAD.
    #[clap(long, value_name = "MARK", num_args = 0..=1, require_equals = true, default_missing_value = "-dirty")]
    pub dirty: Option<String>,

    /// Append MARK (default `-broken`) when the worktree check fails (Git's
    /// `--broken`); the failure-mark path itself is delivered by DESC-08a.
    #[clap(long = "broken", value_name = "MARK", num_args = 0..=1, require_equals = true, default_missing_value = "-broken")]
    pub broken: Option<String>,

    /// Follow only the first parent of merge commits when walking history.
    #[clap(long = "first-parent")]
    pub first_parent: bool,

    /// Only consider tags whose name matches the glob (repeatable; OR semantics).
    #[clap(long = "match", value_name = "PATTERN")]
    pub match_patterns: Vec<String>,

    /// Clear every `--match` given so far (Git's `--no-match`); a later
    /// `--match` still applies, and `--exclude` is unaffected.
    #[clap(long = "no-match", overrides_with = "match_patterns")]
    pub no_match: bool,

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
    /// Name used in the rendered result (internal tag name unless `--all`).
    name: String,
    /// Reference name the tag is known by (`refs/tags/<ref>` basename).
    ref_name: String,
    is_annotated: bool,
    /// The annotated tag object's `tag_name` differs from its ref name.
    misnamed: bool,
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
        no_abbrev: false,
        broken: None,
        always: false,
        exact_match: false,
        candidates: None,
        long: false,
        no_long: false,
        dirty: None,
        first_parent: false,
        match_patterns,
        no_match: false,
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
    // ADR-DESC-01/GAP-DESC-01: `--dirty` is only valid without a commit-ish;
    // `--contains` is exempt (Git's contains branch never inspects the worktree).
    if !args.contains && args.commit.is_some() {
        for (present, option) in [
            (args.broken.is_some(), "--broken"),
            (args.dirty.is_some(), "--dirty"),
        ] {
            if present {
                return Err(DescribeError::CommitishIncompatible {
                    option: option.to_string(),
                });
            }
        }
    }
    let input = args.commit.clone().unwrap_or_else(|| "HEAD".to_string());
    let start_hash = util::get_commit_base_typed(&input)
        .await
        .map_err(DescribeError::from)?;
    let resolved_commit = start_hash.to_string();
    let abbrev = if args.no_abbrev {
        0
    } else {
        args.abbrev.unwrap_or(7)
    };
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
    // `--broken` enables the suffix machinery with Git's default `-dirty` mark
    // when `--dirty` gives no explicit one.
    let dirty_mark = if args.dirty.is_some() || args.broken.is_some() {
        Some(args.dirty.clone().unwrap_or_else(|| "-dirty".to_string()))
    } else {
        None
    };
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
            let ref_name = t.name.clone();
            // `--contains` and `--all` name tags by their reference; the plain
            // path uses an annotated tag's internal name (Git `append_name`).
            let mut misnamed = false;
            let internal_name = match &t.object {
                TagObject::Tag(tg) => Some(tg.tag_name.clone()),
                _ => None,
            };
            let tag_name = if include_all {
                format!("tags/{ref_name}")
            } else if args.contains {
                ref_name.clone()
            } else if let Some(internal) = &internal_name {
                if *internal != ref_name {
                    misnamed = true;
                }
                internal.clone()
            } else {
                ref_name.clone()
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
                        ref_name,
                        is_annotated,
                        misnamed,
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
            // Branch candidates obey the same `--match`/`--exclude` filters as
            // tags, matched on the short branch name (`br-x`).
            let short = branch
                .name
                .strip_prefix("refs/heads/")
                .unwrap_or(&branch.name)
                .to_string();
            if !tag_passes_filters(&short, &matchers, &excluders) {
                continue;
            }
            tag_map.entry(branch.commit).or_insert_with(|| TagInfo {
                name: format!("heads/{short}"),
                ref_name: format!("heads/{short}"),
                is_annotated: false,
                misnamed: false,
            });
        }

        for remote in describe_remote_names().await? {
            let mut remote_branches = Branch::list_branches_result(Some(&remote))
                .await
                .map_err(|e| DescribeError::CorruptReference(e.to_string()))?;
            remote_branches.sort_by(|a, b| a.name.cmp(&b.name));
            for branch in remote_branches {
                // Remote-tracking candidates match on `<remote>/<branch>`; the
                // stored name is sometimes already the full ref.
                let suffix = branch
                    .name
                    .strip_prefix(&format!("refs/remotes/{remote}/"))
                    .unwrap_or(&branch.name);
                let short = format!("{remote}/{suffix}");
                if !tag_passes_filters(&short, &matchers, &excluders) {
                    continue;
                }
                tag_map.entry(branch.commit).or_insert_with(|| TagInfo {
                    name: format!("remotes/{short}"),
                    ref_name: format!("remotes/{short}"),
                    is_annotated: false,
                    misnamed: false,
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
            if tag_info.misnamed && !include_all && !args.contains {
                eprintln!(
                    "warning: tag '{}' is externally known as '{}'",
                    tag_info.ref_name, tag_info.name
                );
            }
            let output = describe_output(
                input.clone(),
                resolved_commit.clone(),
                &tag_info.name,
                dist,
                abbrev,
                long_format,
                tag_info.misnamed && !include_all && !args.contains,
            );
            return apply_dirty_mark(output, dirty_mark.clone(), args.broken.clone()).await;
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
        return apply_dirty_mark(output, dirty_mark.clone(), args.broken.clone()).await;
    }

    Err(DescribeError::NoNamesFound)
}

/// `git describe --contains` (name-rev): name the target relative to the nearest
/// tag that descends from it. A Dijkstra-style walk runs backward from every tag
/// commit; first-parent steps cost 1 and other-parent steps cost `MERGE_COST`,
/// so the straightest path from the closest descendant tag wins. The resulting
/// name is `<tag>^0` for an annotated tag pointing directly at the target,
/// `<tag>` for a lightweight direct tag, `<tag>~<n>` (n first-parent steps), or a
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
                // An annotated tag that points directly at the target renders
                // as `<tag>^0` (the tag object, not the commit), matching the
                // main `--contains` loop; a lightweight tag stays `<tag>`.
                let display = if info.is_annotated {
                    format!("{}^0", info.name)
                } else {
                    info.name.clone()
                };
                let output = DescribeOutput {
                    input,
                    resolved_commit,
                    result: display,
                    tag: Some(info.name.clone()),
                    distance: None,
                    abbreviated_commit: None,
                    exact_match: true,
                    used_always: false,
                    long_format: false,
                    dirty: false,
                    dirty_mark: None,
                };
                Ok(output)
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
    let mut states: Vec<(String, u32, bool)> = Vec::new();
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
        states.push((info.name.clone(), 0, info.is_annotated));
        heap.push(Reverse((0, seq)));
    }

    while let Some(Reverse((weight, seq))) = heap.pop() {
        let commit = nodes[seq as usize];
        if !finalized.insert(commit) {
            continue; // already finalized with a weight <= this one
        }
        let (base, generation, annotated_seed) = states[seq as usize].clone();
        let display = if weight == 0 && annotated_seed {
            format!("{base}^0")
        } else if generation == 0 {
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
            // relative; `exact_match` is true only for a direct tag seed
            // (`weight == 0`), even when an annotated seed renders as `<tag>^0`.
            let exact_match = weight == 0;
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
            return Ok(output);
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
            let (new_weight, new_base, new_gen, new_annotated) = if i == 0 {
                (weight + 1, base.clone(), generation + 1, false)
            } else {
                let propagated = if weight == 0 { &base } else { &display };
                (
                    weight + MERGE_COST,
                    format!("{propagated}^{}", i + 1),
                    0,
                    false,
                )
            };
            let next = nodes.len() as u64;
            nodes.push(*parent);
            states.push((new_base, new_gen, new_annotated));
            heap.push(Reverse((new_weight, next)));
        }
    }

    // No tag descends from the target.
    Err(DescribeError::NoContainingTag {
        commit_id: resolved_commit,
    })
}

/// Map a worktree check result to the suffix to append (DESC-08).
fn worktree_suffix(
    dirty_mark: Option<&str>,
    broken_mark: Option<&str>,
    check: WorktreeCheck,
) -> Option<String> {
    match check {
        WorktreeCheck::Clean => None,
        WorktreeCheck::Dirty => Some(dirty_mark.unwrap_or("-dirty").to_string()),
        WorktreeCheck::Failed => Some(broken_mark.unwrap_or("-broken").to_string()),
    }
}

async fn apply_dirty_mark(
    mut output: DescribeOutput,
    dirty_mark: Option<String>,
    broken_mark: Option<String>,
) -> Result<DescribeOutput, DescribeError> {
    if dirty_mark.is_none() && broken_mark.is_none() {
        return Ok(output);
    }
    let check = match has_tracked_dirty_changes().await {
        Ok(true) => WorktreeCheck::Dirty,
        Ok(false) => WorktreeCheck::Clean,
        Err(error) => {
            // `--broken` turns a failed check into a suffix; without it the
            // failure is still fatal (Git's `--dirty` alone).
            if broken_mark.is_none() {
                return Err(error);
            }
            WorktreeCheck::Failed
        }
    };
    if let Some(mark) = worktree_suffix(dirty_mark.as_deref(), broken_mark.as_deref(), check) {
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
        DescribeError::CommitishIncompatible { option } => CliError::command_usage(format!(
            "option '{option}' and commit-ishes cannot be used together"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(format!("omit '{option}' or the commit-ish.")),
        DescribeError::LoadCommit { commit_id, detail } => {
            CliError::fatal(format!("failed to load commit '{commit_id}': {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}

#[cfg(test)]
#[path = "describe_tests.rs"]
mod tests;
