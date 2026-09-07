//! Merge-base computation over the commit graph: the lowest common ancestors
//! (LCAs) of two commits.
//!
//! This is the single, correct implementation behind `libra merge-base`, the
//! `diff A...B` three-dot range, `merge`, `rebase` and `am`. Unlike a
//! first-found walk it returns true LCAs: a common ancestor is a merge base
//! only when it is not a *strict* ancestor of another common ancestor, so
//! criss-cross histories yield every maximal common ancestor (with `--all`) and
//! a deterministic single base otherwise.
//!
//! `log A...B` is the one remaining call site with its own reachable-set
//! implementation; migrating it (with golden-output regression and a legacy
//! toggle) is tracked as a follow-up.
//!
//! # Algorithm
//!
//! [`merge_bases`] paints down from both tips at once, mirroring Git's
//! `paint_down_to_common` (`commit-reach.c:187` at git@`3cb9185f6`): commits
//! come off a committer-date-ordered priority queue, each carrying the flags of
//! the tips that reached it. A commit reached from BOTH tips is a merge-base
//! candidate and is marked STALE, so the paint stops treating its ancestors as
//! interesting. Only the (small) candidate set is then reduced to maximal
//! elements.
//!
//! Termination is **conservative**: the queue is drained. Git's side-exhaustion
//! and single-result early exits are ordered by commit-graph generation
//! numbers, and this repository has a commit-graph *writer* but no reader — with
//! only committer dates (which skew, and are attacker-controlled in the general
//! case) an early exit is not sound.
//!
//! Complexity, against the BFS-intersection implementation this replaces:
//!
//! | step | before | after |
//! |---|---|---|
//! | reachability | two full walks, one per tip, each materializing a `HashSet` of every ancestor | one walk that visits a commit at most once per flag combination it gains (≤ 3) |
//! | candidates | `HashSet` intersection over both full ancestor sets | commits the walk itself flagged from both sides |
//! | maximal filter | a fresh full walk from EVERY common ancestor — O(\|common\| × graph) | ONE multi-source walk seeded from every candidate's parents, skipped entirely for a single candidate |
//!
//! The old shape was quadratic in the size of the shared history; a long shared
//! trunk made `\|common\|` the whole trunk. Both steps here are linear in the
//! graph the walk actually touches: reads are bounded by
//! `3 × (commits + edges)` — a commit is enqueued at most once per distinct
//! paint value it takes (`lhs`, then `lhs|rhs`, then `+stale`), and each pop
//! reads the commit plus one lookup per parent — plus the two tip seeds.

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use git_internal::{hash::ObjectHash, internal::object::commit::Commit};

use crate::utils::object_ext::CommitExt;

/// Error raised when a commit in the graph cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum MergeBaseError {
    /// A commit object could not be loaded (missing, corrupt, or not a commit).
    #[error("failed to load commit {0}")]
    Load(String),
}

/// The walk-relevant facts about one commit: who its parents are, and the
/// committer date the priority queue orders by.
#[derive(Clone)]
struct CommitNode {
    parents: Vec<ObjectHash>,
    /// Committer timestamp. Ordering only — correctness never depends on it
    /// (see the module note on conservative termination), so skewed or equal
    /// dates change the visit order and nothing else.
    date: u64,
}

/// Where the walk reads commits from.
///
/// The object store is the real source; the unit tests and the scaling
/// benchmark supply a synthetic in-memory graph, which is what lets a
/// 10^4-commit history be exercised without writing 10^4 objects.
trait CommitSource {
    fn node(&mut self, id: &ObjectHash) -> Result<CommitNode, MergeBaseError>;
}

/// Lazily-loaded, cached view of the repository's commit graph, so each commit
/// is read from the object store at most once per call.
struct ObjectStoreCommits {
    nodes: HashMap<ObjectHash, CommitNode>,
}

impl ObjectStoreCommits {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }
}

impl CommitSource for ObjectStoreCommits {
    fn node(&mut self, id: &ObjectHash) -> Result<CommitNode, MergeBaseError> {
        if let Some(node) = self.nodes.get(id) {
            return Ok(node.clone());
        }
        let commit: Commit =
            Commit::try_load(id).ok_or_else(|| MergeBaseError::Load(id.to_string()))?;
        let node = CommitNode {
            parents: commit.parent_commit_ids.clone(),
            date: commit.committer.timestamp as u64,
        };
        self.nodes.insert(*id, node.clone());
        Ok(node)
    }
}

/// Which tips have reached a commit, and whether the paint has stopped caring
/// about its ancestors.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Paint {
    /// Reachable from the first tip.
    lhs: bool,
    /// Reachable from the second tip.
    rhs: bool,
    /// Reached from both sides (or descended from something that was), so its
    /// ancestors cannot be maximal common ancestors.
    stale: bool,
}

impl Paint {
    fn both(self) -> bool {
        self.lhs && self.rhs
    }

    fn merge(&mut self, other: Paint) {
        self.lhs |= other.lhs;
        self.rhs |= other.rhs;
        self.stale |= other.stale;
    }
}

/// Priority-queue entry: pop the newest commit first, matching Git's
/// date-ordered `prio_queue`. The object id breaks ties so the walk order is
/// deterministic for equal dates.
#[derive(PartialEq, Eq)]
struct Queued {
    date: u64,
    id: ObjectHash,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.date
            .cmp(&other.date)
            .then_with(|| self.id.to_string().cmp(&other.id.to_string()))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// What one paint cost, for the tests that pin the walk's shape.
#[derive(Debug, Default, Clone)]
struct WalkStats {
    /// Largest number of commits queued at once — the FRONTIER. It tracks how
    /// WIDE the history is where the walk currently is, not how deep it has
    /// gone.
    peak_frontier: usize,
    /// Largest number of commits held in the paint map. This is the walk's
    /// resident state, and it is NOT sublinear: a commit stays painted once
    /// visited, because a later path could still add a flag to it. The claim
    /// the card makes is comparative — ONE such map, where the implementation
    /// this replaced held two full ancestor sets plus a `common` set plus a
    /// fresh visited set per candidate.
    peak_painted: usize,
    /// Committer date of each commit as it was popped, in order. TEST-ONLY: it
    /// grows with the number of pops, so it must never exist in a production
    /// walk — the whole point of the residency claim is that nothing here
    /// scales with the history.
    #[cfg(test)]
    pop_dates: Vec<u64>,
    /// Whether every pop took the newest commit the queue held AT THAT MOMENT.
    ///
    /// Note what this does NOT say: the sequence of popped dates is not
    /// globally descending, and cannot be. Parents are discovered lazily, so a
    /// commit newer than the one just popped can still be pushed afterwards —
    /// committer dates are not monotone along parent edges (which is precisely
    /// why Git's generation-ordered early exits do not transfer to a
    /// date-ordered queue, see the module note). The checkable property is the
    /// one a FIFO would violate: each pop dominates what remains.
    priority_respected: bool,
}

/// Paint down from both tips and return every commit reached from both sides.
///
/// The result is the merge-base CANDIDATE set: it always contains the true
/// LCAs, and may additionally contain commits shadowed by them, which
/// [`remove_redundant`] strips.
fn paint_down_to_common<S: CommitSource>(
    source: &mut S,
    lhs: &ObjectHash,
    rhs: &ObjectHash,
) -> Result<(Vec<ObjectHash>, WalkStats), MergeBaseError> {
    let mut painted: HashMap<ObjectHash, Paint> = HashMap::new();
    let mut queue: BinaryHeap<Queued> = BinaryHeap::new();
    let mut result = Vec::new();
    let mut stats = WalkStats {
        priority_respected: true,
        ..WalkStats::default()
    };

    // Identical tips are seeded ONCE carrying both marks. Seeding twice would
    // queue the same commit twice for no gain, and would put the read count
    // over the `3 * (commits + edges)` bound on the degenerate one-commit
    // graph.
    let seeds: &[(&ObjectHash, Paint)] = if lhs == rhs {
        &[(
            lhs,
            Paint {
                lhs: true,
                rhs: true,
                stale: false,
            },
        )]
    } else {
        &[
            (
                lhs,
                Paint {
                    lhs: true,
                    ..Paint::default()
                },
            ),
            (
                rhs,
                Paint {
                    rhs: true,
                    ..Paint::default()
                },
            ),
        ]
    };
    for (tip, paint) in seeds {
        let date = source.node(tip)?.date;
        painted.entry(**tip).or_default().merge(*paint);
        queue.push(Queued { date, id: **tip });
    }

    while let Some(Queued { date, id }) = queue.pop() {
        stats.peak_frontier = stats.peak_frontier.max(queue.len() + 1);
        stats.peak_painted = stats.peak_painted.max(painted.len());
        #[cfg(test)]
        stats.pop_dates.push(date);
        stats.priority_respected &= queue.peek().is_none_or(|next| next.date <= date);
        let mut paint = painted.get(&id).copied().unwrap_or_default();
        if paint.both() && !paint.stale {
            // First time this commit is known to be common: record it and stop
            // treating its ancestors as candidates.
            result.push(id);
            paint.stale = true;
            painted.insert(id, paint);
        }
        let node = source.node(&id)?;
        for parent in &node.parents {
            let current = painted.get(parent).copied();
            let mut next = current.unwrap_or_default();
            next.merge(paint);
            if current == Some(next) {
                // The parent already carries every flag this child would add,
                // so re-queuing it could only repeat work. This is what bounds
                // the walk: a commit is enqueued at most once per flag it
                // gains, i.e. at most three times.
                continue;
            }
            let date = source.node(parent)?.date;
            painted.insert(*parent, next);
            queue.push(Queued { date, id: *parent });
        }
    }

    Ok((result, stats))
}

/// Reduce merge-base candidates to the maximal ones: drop any candidate that is
/// a *strict* ancestor of another.
///
/// ONE multi-source walk, seeded from the parents of every candidate at once.
/// A candidate met by that walk is a strict ancestor of some candidate — and it
/// cannot be its own, because a DAG has no cycles, so no per-source attribution
/// is needed. That is what keeps this linear in the graph instead of running a
/// full ancestor walk per candidate the way the implementation this replaced
/// did (which was quadratic whenever the candidate set was large).
fn remove_redundant<S: CommitSource>(
    source: &mut S,
    candidates: &[ObjectHash],
) -> Result<Vec<ObjectHash>, MergeBaseError> {
    if candidates.len() <= 1 {
        return Ok(candidates.to_vec());
    }
    let candidate_set: HashSet<ObjectHash> = candidates.iter().copied().collect();
    let mut dominated: HashSet<ObjectHash> = HashSet::new();
    let mut seen: HashSet<ObjectHash> = HashSet::new();
    let mut queue: VecDeque<ObjectHash> = VecDeque::new();
    for start in candidates {
        queue.extend(source.node(start)?.parents);
    }
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if candidate_set.contains(&id) {
            dominated.insert(id);
        }
        for parent in source.node(&id)?.parents {
            queue.push_back(parent);
        }
    }
    Ok(candidates
        .iter()
        .copied()
        .filter(|id| !dominated.contains(id))
        .collect())
}

/// Every lowest common ancestor of `a` and `b`, sorted deterministically by hex
/// id. Empty when the two commits share no history.
pub fn merge_bases(a: &ObjectHash, b: &ObjectHash) -> Result<Vec<ObjectHash>, MergeBaseError> {
    let mut source = ObjectStoreCommits::new();
    merge_bases_with(&mut source, a, b)
}

fn merge_bases_with<S: CommitSource>(
    source: &mut S,
    a: &ObjectHash,
    b: &ObjectHash,
) -> Result<Vec<ObjectHash>, MergeBaseError> {
    let (candidates, _stats) = paint_down_to_common(source, a, b)?;
    let mut lcas = remove_redundant(source, &candidates)?;
    lcas.sort_by_key(|id| id.to_string());
    Ok(lcas)
}

/// A single "best" merge base of `a` and `b` (the lowest-hex LCA for
/// determinism), or `None` when there is no common ancestor.
pub fn merge_base(a: &ObjectHash, b: &ObjectHash) -> Result<Option<ObjectHash>, MergeBaseError> {
    Ok(merge_bases(a, b)?.into_iter().next())
}

/// Whether `ancestor` is an ancestor of `descendant`. Reflexive: a commit is its
/// own ancestor, matching `git merge-base --is-ancestor X X` (exit 0).
pub fn is_ancestor(ancestor: &ObjectHash, descendant: &ObjectHash) -> Result<bool, MergeBaseError> {
    let mut source = ObjectStoreCommits::new();
    is_ancestor_with(&mut source, ancestor, descendant)
}

fn is_ancestor_with<S: CommitSource>(
    source: &mut S,
    ancestor: &ObjectHash,
    descendant: &ObjectHash,
) -> Result<bool, MergeBaseError> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([*descendant]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if id == *ancestor {
            return Ok(true);
        }
        for parent in source.node(&id)?.parents {
            queue.push_back(parent);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// An in-memory commit graph. Everything the walk needs is parents plus a
    /// date, so the tests never write objects — which is what makes the 10^4
    /// scaling benchmark below affordable.
    struct TestGraph {
        nodes: HashMap<ObjectHash, CommitNode>,
        /// How many `node()` lookups the algorithm asked for, and the largest
        /// number of commits it held painted at once — the frontier bound.
        reads: usize,
    }

    impl TestGraph {
        fn new() -> Self {
            Self {
                nodes: HashMap::new(),
                reads: 0,
            }
        }

        /// Deterministic id for commit `n`, so a test can name commits by index.
        fn id(n: u32) -> ObjectHash {
            let mut bytes = [0u8; 20];
            bytes[..4].copy_from_slice(&n.to_be_bytes());
            ObjectHash::new(&bytes)
        }

        /// Add commit `n` with the given parents and committer date.
        fn add(&mut self, n: u32, parents: &[u32], date: u64) -> ObjectHash {
            let id = Self::id(n);
            self.nodes.insert(
                id,
                CommitNode {
                    parents: parents.iter().copied().map(Self::id).collect(),
                    date,
                },
            );
            id
        }

        /// A linear chain `0 <- 1 <- ... <- len-1` with increasing dates.
        fn chain(&mut self, first: u32, len: u32, parent: Option<u32>) -> u32 {
            let mut previous = parent;
            for offset in 0..len {
                let n = first + offset;
                let parents: Vec<u32> = previous.into_iter().collect();
                self.add(n, &parents, u64::from(n));
                previous = Some(n);
            }
            first + len - 1
        }
    }

    impl CommitSource for TestGraph {
        fn node(&mut self, id: &ObjectHash) -> Result<CommitNode, MergeBaseError> {
            self.reads += 1;
            self.nodes
                .get(id)
                .cloned()
                .ok_or_else(|| MergeBaseError::Load(id.to_string()))
        }
    }

    /// The implementation this card replaced: two full ancestor walks, a set
    /// intersection, then a full walk from EVERY common ancestor to drop the
    /// non-maximal ones. Kept as the oracle the new painting is checked
    /// against, and as the baseline the scaling assertion measures.
    fn reference_merge_bases(
        graph: &mut TestGraph,
        a: &ObjectHash,
        b: &ObjectHash,
    ) -> Result<Vec<ObjectHash>, MergeBaseError> {
        fn ancestors(
            graph: &mut TestGraph,
            start: &ObjectHash,
        ) -> Result<HashSet<ObjectHash>, MergeBaseError> {
            let mut seen = HashSet::new();
            let mut queue = VecDeque::from([*start]);
            while let Some(id) = queue.pop_front() {
                if !seen.insert(id) {
                    continue;
                }
                for parent in graph.node(&id)?.parents {
                    queue.push_back(parent);
                }
            }
            Ok(seen)
        }

        let common: HashSet<ObjectHash> = ancestors(graph, a)?
            .intersection(&ancestors(graph, b)?)
            .copied()
            .collect();
        if common.is_empty() {
            return Ok(Vec::new());
        }
        let mut dominated: HashSet<ObjectHash> = HashSet::new();
        for start in &common {
            let mut seen = HashSet::new();
            let mut queue: VecDeque<ObjectHash> = graph.node(start)?.parents.into_iter().collect();
            while let Some(id) = queue.pop_front() {
                if !seen.insert(id) {
                    continue;
                }
                if common.contains(&id) {
                    dominated.insert(id);
                }
                for parent in graph.node(&id)?.parents {
                    queue.push_back(parent);
                }
            }
        }
        let mut lcas: Vec<ObjectHash> = common
            .into_iter()
            .filter(|id| !dominated.contains(id))
            .collect();
        lcas.sort_by_key(|id| id.to_string());
        Ok(lcas)
    }

    /// Assert the painting agrees with the reference implementation, and return
    /// what both produced.
    fn agree(graph: &mut TestGraph, a: ObjectHash, b: ObjectHash) -> Vec<ObjectHash> {
        let painted = merge_bases_with(graph, &a, &b).expect("paint down");
        let reference = reference_merge_bases(graph, &a, &b).expect("reference walk");
        assert_eq!(
            painted, reference,
            "the painting must agree with the BFS-intersection oracle"
        );
        painted
    }

    #[test]
    fn merge_bases_paints_down_to_the_single_base_of_a_fork() {
        let mut graph = TestGraph::new();
        let root = graph.chain(0, 3, None); // 0 <- 1 <- 2
        graph.add(10, &[root], 10);
        graph.add(11, &[10], 11);
        graph.add(20, &[root], 20);
        let left = TestGraph::id(11);
        let right = TestGraph::id(20);

        assert_eq!(agree(&mut graph, left, right), vec![TestGraph::id(2)]);
    }

    #[test]
    fn merge_bases_returns_both_bases_of_a_criss_cross() {
        // A criss-cross: two commits each merge both sides, so neither of the
        // two shared parents dominates the other and BOTH are merge bases.
        let mut graph = TestGraph::new();
        graph.add(0, &[], 0);
        graph.add(1, &[0], 1); // side A
        graph.add(2, &[0], 2); // side B
        graph.add(3, &[1, 2], 3); // merge, left tip
        graph.add(4, &[2, 1], 4); // merge, right tip

        let bases = agree(&mut graph, TestGraph::id(3), TestGraph::id(4));
        let mut expected = vec![TestGraph::id(1), TestGraph::id(2)];
        expected.sort_by_key(|id| id.to_string());
        assert_eq!(bases, expected);
    }

    #[test]
    fn merge_bases_is_reflexive_and_handles_ancestry() {
        let mut graph = TestGraph::new();
        let tip = graph.chain(0, 4, None);
        let tip = TestGraph::id(tip);

        assert_eq!(agree(&mut graph, tip, tip), vec![tip]);
        // An ancestor of the other tip is itself the base.
        assert_eq!(
            agree(&mut graph, TestGraph::id(1), tip),
            vec![TestGraph::id(1)]
        );
        assert!(is_ancestor_with(&mut graph, &TestGraph::id(1), &tip).expect("ancestry"));
        assert!(!is_ancestor_with(&mut graph, &tip, &TestGraph::id(1)).expect("ancestry"));
    }

    #[test]
    fn merge_bases_are_empty_for_unrelated_histories() {
        let mut graph = TestGraph::new();
        let left = graph.chain(0, 3, None);
        let right = graph.chain(100, 3, None);

        assert!(agree(&mut graph, TestGraph::id(left), TestGraph::id(right)).is_empty());
    }

    #[test]
    fn merge_bases_ignore_skewed_committer_dates() {
        // The priority queue orders by committer date, but dates are only a
        // visit order: a history whose dates run BACKWARDS, and one where every
        // date is identical, must both produce the same bases as the oracle.
        for dates in [
            // strictly decreasing towards the tips
            |n: u32| 1_000 - u64::from(n),
            // all equal
            |_: u32| 42,
        ] {
            let mut graph = TestGraph::new();
            for n in 0..3u32 {
                let parents: Vec<u32> = if n == 0 { vec![] } else { vec![n - 1] };
                graph.add(n, &parents, dates(n));
            }
            graph.add(10, &[2], dates(10));
            graph.add(20, &[2], dates(20));

            assert_eq!(
                agree(&mut graph, TestGraph::id(10), TestGraph::id(20)),
                vec![TestGraph::id(2)]
            );
        }
    }

    #[test]
    fn paint_down_keeps_the_frontier_bounded() {
        // A long shared trunk with two short tips. The paint is conservative —
        // it drains the queue, so it does read down the trunk once — but the
        // claim AC10 makes is about RESIDENCY: the queue holds the frontier,
        // not a materialized copy of every ancestor the way the two `HashSet`s
        // of the previous implementation did.
        const TRUNK: u32 = 5_000;
        let mut graph = TestGraph::new();
        let trunk_tip = graph.chain(0, TRUNK, None);
        graph.add(TRUNK + 1, &[trunk_tip], u64::from(TRUNK) + 1);
        graph.add(TRUNK + 2, &[trunk_tip], u64::from(TRUNK) + 2);

        let (candidates, stats) = paint_down_to_common(
            &mut graph,
            &TestGraph::id(TRUNK + 1),
            &TestGraph::id(TRUNK + 2),
        )
        .expect("paint down");

        assert_eq!(candidates, vec![TestGraph::id(trunk_tip)]);
        assert!(
            stats.peak_frontier <= 4,
            "a linear trunk is one commit wide, so the frontier must stay tiny; \
             peaked at {} over a {TRUNK}-commit trunk",
            stats.peak_frontier
        );
    }

    #[test]
    fn paint_down_frontier_tracks_width_not_depth() {
        // Widen the history and the frontier grows with the WIDTH; lengthen it
        // and the frontier does not move. That is the property that makes the
        // queue a frontier rather than an ancestor set.
        fn peak(trunk: u32, width: u32) -> usize {
            let mut graph = TestGraph::new();
            let trunk_tip = graph.chain(0, trunk, None);
            // `width` sibling branches off the trunk, merged by each tip.
            let mut siblings = Vec::new();
            for w in 0..width {
                let n = trunk + 10 + w;
                graph.add(n, &[trunk_tip], u64::from(n));
                siblings.push(n);
            }
            let left = trunk + 1_000;
            let right = trunk + 1_001;
            graph.add(left, &siblings, u64::from(left));
            graph.add(right, &siblings, u64::from(right));
            paint_down_to_common(&mut graph, &TestGraph::id(left), &TestGraph::id(right))
                .expect("paint down")
                .1
                .peak_frontier
        }

        let narrow_short = peak(100, 2);
        let narrow_long = peak(5_000, 2);
        let wide_short = peak(100, 40);
        assert_eq!(
            narrow_short, narrow_long,
            "depth must not change the frontier"
        );
        assert!(
            wide_short > narrow_short,
            "width must: {wide_short} vs {narrow_short}"
        );
    }

    #[test]
    fn paint_down_holds_one_map_where_the_replaced_walk_held_several() {
        // AC10 is a COMPARATIVE claim, and this measures it rather than
        // asserting something stronger than the algorithm delivers: the paint
        // does keep every visited commit in one map (a commit must stay painted
        // because a later path can still add a flag to it), but the
        // implementation it replaced kept two complete ancestor sets, their
        // intersection, and a fresh visited set for every common ancestor.
        const TRUNK: u32 = 2_000;
        let mut graph = TestGraph::new();
        let trunk_tip = graph.chain(0, TRUNK, None);
        graph.add(TRUNK + 1, &[trunk_tip], u64::from(TRUNK) + 1);
        graph.add(TRUNK + 2, &[trunk_tip], u64::from(TRUNK) + 2);
        let left = TestGraph::id(TRUNK + 1);
        let right = TestGraph::id(TRUNK + 2);

        let (_, stats) = paint_down_to_common(&mut graph, &left, &right).expect("paint down");
        let commits = usize::try_from(TRUNK).expect("trunk fits") + 2;

        // One entry per visited commit, and no more.
        assert!(
            stats.peak_painted <= commits,
            "the paint map must not exceed one entry per commit: {} over {commits}",
            stats.peak_painted
        );
        // The replaced implementation's residency on the same history: two full
        // ancestor sets (each the whole trunk) plus their intersection, before
        // its per-candidate visited sets are counted at all.
        let replaced_residency = 3 * commits;
        assert!(
            stats.peak_painted * 2 < replaced_residency,
            "the paint must hold materially less than the two ancestor sets \
             plus intersection it replaced: {} vs {replaced_residency}",
            stats.peak_painted
        );
        // And the queue is the frontier, not the history.
        assert!(
            stats.peak_frontier <= 4,
            "frontier peaked at {} on a linear trunk",
            stats.peak_frontier
        );
    }

    #[test]
    fn paint_down_pops_in_committer_date_order_and_drains_the_queue() {
        // G2 has two halves, and results alone cannot tell them apart — a FIFO
        // walk would return the same bases. Assert the queue's behaviour
        // directly: pops come off newest-date-first, and the walk keeps going
        // after the base is found (no early exit) until nothing is left.
        let mut graph = TestGraph::new();
        //   0 <- 1 <- 2 <- {10, 20}
        // with deliberately scattered dates so date order != topological order
        // and != insertion order.
        graph.add(0, &[], 5);
        graph.add(1, &[0], 900);
        graph.add(2, &[1], 100);
        graph.add(10, &[2], 700);
        graph.add(20, &[2], 300);

        let (candidates, stats) =
            paint_down_to_common(&mut graph, &TestGraph::id(10), &TestGraph::id(20))
                .expect("paint down");

        assert_eq!(candidates, vec![TestGraph::id(2)]);
        assert!(
            stats.priority_respected,
            "every pop must take the newest commit the queue held at that \
             moment (a FIFO walk would not): {:?}",
            stats.pop_dates
        );
        // Conservative termination: commit 0 has the OLDEST date and sits below
        // the base, so an implementation that stopped once the base was found
        // would never pop it. Every commit is popped at least once.
        assert!(
            stats.pop_dates.contains(&5),
            "the queue must be drained past the base, not exited early: {:?}",
            stats.pop_dates
        );
        assert_eq!(
            stats.pop_dates.iter().filter(|date| **date == 900).count(),
            1,
            "a commit whose paint never changes again must not be re-queued"
        );
    }

    #[test]
    fn bench_merge_base_scaling() {
        // Two measurements, deliberately at different sizes so the gate stays
        // fast while still asserting numbers:
        //
        //  (1) At 10^4 commits — the size the card's performance budget names —
        //      the painting must stay LINEAR in the trunk. Only the painting is
        //      run at this size; the implementation it replaced is quadratic
        //      here (see (2)) and would take minutes.
        //  (2) At a small size, both are run, and the replaced implementation's
        //      cost per commit is shown to GROW with the trunk — i.e. the
        //      quadratic term is real, not an assumption.
        const BUDGET_TRUNK: u32 = 12_000;
        let mut graph = TestGraph::new();
        let trunk_tip = graph.chain(0, BUDGET_TRUNK, None);
        graph.chain(BUDGET_TRUNK + 1, 5, Some(trunk_tip));
        graph.chain(BUDGET_TRUNK + 100, 5, Some(trunk_tip));
        let left = TestGraph::id(BUDGET_TRUNK + 5);
        let right = TestGraph::id(BUDGET_TRUNK + 104);

        graph.reads = 0;
        let started = Instant::now();
        // Measure the PAINT, not `merge_bases_with` — the latter also runs the
        // maximal filter, and the bound below is a statement about the paint.
        let (painted_candidates, _) =
            paint_down_to_common(&mut graph, &left, &right).expect("paint down");
        let elapsed = started.elapsed();
        let painted_reads = graph.reads;

        assert_eq!(painted_candidates, vec![TestGraph::id(trunk_tip)]);
        // The bound the ALGORITHM implies, not the one a lucky fixture shows: a
        // commit is enqueued once per distinct paint value it takes (at most
        // three — `lhs`, then `lhs|rhs`, then `+stale`), and each pop reads the
        // commit plus one lookup per parent. On a linear trunk that is at most
        // ~6 reads per commit. Dates only change the ORDER those transitions
        // happen in, so the bound holds for skewed histories too — which the
        // second measurement below checks rather than assumes.
        // The bound is in COMMITS AND EDGES, not commits alone: each pop reads
        // the commit plus one lookup per parent, so a high-fanout history costs
        // more per commit than a linear one. `linear_bound` names both.
        let budget_commits = usize::try_from(BUDGET_TRUNK).expect("trunk fits") + 10;
        let budget_edges = budget_commits - 1;
        let linear_bound = 3 * (budget_commits + budget_edges);
        assert!(
            painted_reads <= linear_bound,
            "the paint must stay linear in the trunk: {painted_reads} reads over \
             {BUDGET_TRUNK} commits, bound {linear_bound} (wall clock {elapsed:?})"
        );

        // The adversarial date layout: one tip NEWER than the whole trunk and
        // one OLDER than all of it, so the priority queue drains one side
        // completely before it ever looks at the other. Still linear.
        let mut skewed = TestGraph::new();
        let skewed_tip = skewed.chain(0, BUDGET_TRUNK, None);
        skewed.add(BUDGET_TRUNK + 1, &[skewed_tip], u64::MAX / 2);
        skewed.add(BUDGET_TRUNK + 2, &[skewed_tip], 0);
        skewed.reads = 0;
        let (skewed_candidates, _) = paint_down_to_common(
            &mut skewed,
            &TestGraph::id(BUDGET_TRUNK + 1),
            &TestGraph::id(BUDGET_TRUNK + 2),
        )
        .expect("paint down");
        assert_eq!(skewed_candidates, vec![TestGraph::id(skewed_tip)]);
        assert!(
            skewed.reads <= linear_bound,
            "skewed dates reorder the walk but must not change its order of \
             growth: {} reads over {BUDGET_TRUNK} commits, bound {linear_bound}",
            skewed.reads
        );

        // High fanout: the same bound, now with edges dominating commits. A
        // merge commit with many parents is where a commits-only bound would
        // have been wrong.
        const FAN: u32 = 500;
        let mut wide = TestGraph::new();
        wide.add(0, &[], 0);
        let leaves: Vec<u32> = (1..=FAN).collect();
        for leaf in &leaves {
            wide.add(*leaf, &[0], u64::from(*leaf));
        }
        // Two octopus tips over every leaf, so both sides reach all of them.
        wide.add(FAN + 1, &leaves, u64::from(FAN) + 1);
        wide.add(FAN + 2, &leaves, u64::from(FAN) + 2);
        wide.reads = 0;
        let (wide_candidates, _) =
            paint_down_to_common(&mut wide, &TestGraph::id(FAN + 1), &TestGraph::id(FAN + 2))
                .expect("paint down");
        let wide_paint_reads = wide.reads;
        let wide_bases =
            merge_bases_with(&mut wide, &TestGraph::id(FAN + 1), &TestGraph::id(FAN + 2))
                .expect("merge bases");
        let mut expected: Vec<ObjectHash> = leaves.iter().copied().map(TestGraph::id).collect();
        expected.sort_by_key(|id| id.to_string());
        assert_eq!(
            wide_bases, expected,
            "every leaf is a maximal common ancestor of the two octopus tips"
        );
        let wide_commits = usize::try_from(FAN).expect("fan fits") + 3;
        // Edges: FAN leaf->root, plus FAN from each of the two tips.
        let wide_edges = 3 * usize::try_from(FAN).expect("fan fits");
        assert_eq!(
            wide_candidates.len(),
            expected.len(),
            "all {FAN} leaves are candidates, so the maximal filter is exercised \
             at its worst case"
        );
        assert!(
            wide_paint_reads <= 3 * (wide_commits + wide_edges),
            "the bound must hold when EDGES dominate: {wide_paint_reads} reads \
             over {wide_commits} commits / {wide_edges} edges"
        );

        // The degenerate graph the bound has to survive too: one commit, no
        // edges, both tips the same. Seeding it twice would blow the bound.
        let mut single = TestGraph::new();
        single.add(0, &[], 1);
        single.reads = 0;
        let (self_candidates, _) =
            paint_down_to_common(&mut single, &TestGraph::id(0), &TestGraph::id(0))
                .expect("paint down");
        assert_eq!(self_candidates, vec![TestGraph::id(0)]);
        assert!(
            single.reads <= 3,
            "a reflexive one-commit graph must stay inside the bound: {} reads",
            single.reads
        );

        // (2) The replaced implementation's growth, measured.
        let reference_cost = |trunk: u32| -> usize {
            let mut graph = TestGraph::new();
            let tip = graph.chain(0, trunk, None);
            graph.chain(trunk + 1, 5, Some(tip));
            graph.chain(trunk + 100, 5, Some(tip));
            graph.reads = 0;
            reference_merge_bases(
                &mut graph,
                &TestGraph::id(trunk + 5),
                &TestGraph::id(trunk + 104),
            )
            .expect("reference walk");
            graph.reads
        };
        let small = reference_cost(200);
        let doubled = reference_cost(400);
        assert!(
            doubled > 3 * small,
            "doubling the trunk must more than double the replaced \
             implementation's work (it is quadratic in the shared history): \
             {small} -> {doubled} reads"
        );
    }
}
