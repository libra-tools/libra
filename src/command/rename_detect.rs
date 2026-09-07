//! Shared rename-detection engine (plan-20260714 Part B §B.4).
//!
//! `match_pairs` implements Git's diffcore-rename stage order — exact OID
//! buckets, unique-basename pairing, then a bounded exhaustive inexact pass —
//! over side-agnostic [`RenameSnapshot`]s so `status` (and, incrementally,
//! `diff`) share one deterministic scorer instead of re-implementing greedy
//! heuristics per command.
//!
//! Content is pulled through a [`RenameContentSource`] so callers control
//! where bytes come from (repository objects, the worktree, or test fixtures)
//! and how read budgets apply. The engine itself never touches the
//! filesystem or the object store.
//!
//! BOTH commands go through [`match_pairs`]. `status` owns the
//! [`ObjectReadBudget`]/[`WorktreeReadBudget`] instances (threaded through
//! the exact-stage OID streaming and the inexact content reads, and shared
//! across its staged and unstaged passes); `diff` supplies its own
//! [`RenameContentSource`] over tree/index/worktree blobs and runs unbudgeted
//! (§B.7 `max_blob_bytes: None`). Neither command implements pairing itself,
//! so a tie-break or eligibility rule cannot drift between them.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use git_internal::hash::ObjectHash;

/// Git's exact-match similarity score (`-M100%`).
pub(crate) const EXACT_SCORE: u32 = 60000;
/// Per-destination retained inexact edges (§B.4.2.5).
pub(crate) const PER_DEST_TOP_K: usize = 4;
/// `status` inexact comparison budget (§B.7); `diff` defaults to `None`.
pub(crate) const STATUS_MAX_SIMILARITY_COMPARISONS: u64 = 500_000;

/// Object-read budget defaults (§B.3.4): per-object cap, total cap, object
/// count cap, and wall-clock deadline for the whole scoring batch.
pub(crate) const OBJECT_READ_MAX_OBJECT_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const OBJECT_READ_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const OBJECT_READ_MAX_OBJECTS: u32 = 64;
pub(crate) const OBJECT_READ_DEADLINE: Duration = Duration::from_secs(5);

/// Worktree-read budget defaults (§B.3.4/§B.7).
pub(crate) const WORKTREE_READ_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const WORKTREE_READ_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const WORKTREE_READ_MAX_TASKS: u32 = 4096;
pub(crate) const WORKTREE_READ_DEADLINE: Duration = Duration::from_secs(5);

/// Blob kind, derived from the Git mode bits. Inexact scoring only ever
/// pairs `Regular` with `Regular`; exact pairing additionally requires the
/// kinds to be equal (and for gitlinks, the modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlobKind {
    Regular,
    Symlink,
    Gitlink,
}

impl BlobKind {
    pub fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o160000 => BlobKind::Gitlink,
            0o120000 => BlobKind::Symlink,
            _ => BlobKind::Regular,
        }
    }
}

/// Where a blob's OID came from (§B.4.1). Only `KnownObjectId` and
/// `ComputedWorktreeThisCall` may participate in exact pairing; `Unknown`
/// blobs join inexact scoring only after their content is successfully read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobEvidence {
    /// OID recorded by HEAD tree / index stage-0 (content-addressed fact).
    KnownObjectId { oid: ObjectHash },
    /// OID streamed from the worktree during this status/diff call.
    ComputedWorktreeThisCall { oid: ObjectHash },
    /// No trustworthy OID; exact pairing is forbidden. Not yet constructed
    /// by the live `status` sides (HEAD/index carry known OIDs and worktree
    /// OIDs are streamed this call) — R0-3's untracked rename destinations
    /// are the intended producer.
    #[allow(dead_code)]
    Unknown,
}

impl BlobEvidence {
    fn oid(&self) -> Option<&ObjectHash> {
        match self {
            BlobEvidence::KnownObjectId { oid }
            | BlobEvidence::ComputedWorktreeThisCall { oid } => Some(oid),
            BlobEvidence::Unknown => None,
        }
    }

    /// True only for a content-addressed fact recorded by HEAD/index.
    fn is_known(&self) -> bool {
        matches!(self, BlobEvidence::KnownObjectId { .. })
    }

    /// §B.4.1 exact allow-list: a pair may skip content scoring only when
    /// AT LEAST ONE side's OID is a recorded fact (K/K, K/C, C/K).
    /// `ComputedWorktreeThisCall ↔ ComputedWorktreeThisCall` is refused —
    /// two same-call worktree hashes prove the bytes match right now but
    /// neither side is anchored in the object store, so the pair must go
    /// through the ordinary inexact path instead of being asserted exact
    /// (fail closed). `Unknown` never pairs exactly at all.
    /// §B.4.1 exact 合法组合硬门禁：任一侧 Unknown 拒绝；C/C 拒绝（须至少
    /// 一侧 known）。匹配引擎的热路径把该判定折叠进 known/computed 分桶
    /// （桶内无 Unknown），本表保留为规范事实源：debug 构建对每条产出边
    /// 交叉断言，单测直接钉住组合表。
    #[cfg(any(test, debug_assertions))]
    fn exact_pair_allowed(old: &Self, new: &Self) -> bool {
        match (old, new) {
            (BlobEvidence::Unknown, _) | (_, BlobEvidence::Unknown) => false,
            (old, new) => old.is_known() || new.is_known(),
        }
    }
}

/// One side of a rename candidate (§B.4.1 唯一定义).
#[derive(Debug, Clone)]
pub struct BlobRef {
    pub kind: BlobKind,
    pub mode: u32,
    pub size: Option<u64>,
    pub evidence: BlobEvidence,
}

/// Snapshot of both sides of a rename-detection run. For the staged side
/// `old = HEAD` / `new = index stage-0`; for the unstaged side `old = index
/// stage-0` / `new = worktree` (§B.4.1). Keys are repo-relative paths.
#[derive(Debug, Default)]
pub struct RenameSnapshot {
    pub old_map: HashMap<PathBuf, BlobRef>,
    pub new_map: HashMap<PathBuf, BlobRef>,
}

/// Engine knobs. `threshold` uses Git's 0..=60000 scale; `rename_limit == 0`
/// means "no per-side cap"; `comparison_budget == None` means unlimited
/// (diff's default per §B.7).
#[derive(Debug, Clone)]
pub struct RenameDetectConfig {
    pub threshold: u32,
    pub rename_limit: usize,
    pub comparison_budget: Option<u64>,
}

/// A matched rename pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameMatch {
    pub old: PathBuf,
    pub new: PathBuf,
    pub exact: bool,
    /// Git-scale similarity (0..=60000). Exact pairs always carry 60000.
    pub internal_score: u32,
}

impl RenameMatch {
    /// Percentage (0..=100) as rendered by porcelain v2 / JSON. Git floors,
    /// except a non-exact 60000 caps at 100 anyway.
    pub fn score_percent(&self) -> u32 {
        (self.internal_score / 600).min(100)
    }
}

/// Why a candidate's content could not be scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
///
/// The size/budget/IO reasons are SIDE-QUALIFIED. Both the object store and
/// the working tree can hit a size cap, exhaust a budget, or fail to read,
/// but the `source` field of the resulting status warning is a frozen enum
/// where `metadata` means the object store and `worktree` means the working
/// tree. A shared reason would erase that distinction on the way out, so the
/// origin is carried in the reason itself and the compiler enforces it at
/// every emit site.
pub enum SkipReason {
    ObjectMissing,
    ObjectCorrupt,
    ObjectUnavailable,
    /// Object side: the blob exceeded the per-object size cap.
    ObjectTooLarge,
    /// Object side: the object-read budget (bytes, slots, or deadline) was
    /// exhausted.
    ObjectBudgetExceeded,
    /// Object side: a read failed for a reason that is not missing, corrupt,
    /// or unavailable.
    ObjectIoFailed,
    /// Worktree side: the file exceeded the per-file size cap.
    WorktreeTooLarge,
    /// Worktree side: the worktree-read budget (bytes or tasks) was
    /// exhausted.
    WorktreeBudgetExceeded,
    /// Worktree side: a worktree read failed (I/O error).
    WorktreeIoFailed,
    /// The read exceeded the per-operation I/O deadline and the run was
    /// reclaimed (§B.3.3) rather than hanging. Worktree-side only: object
    /// reads bound themselves with `ObjectBudgetExceeded` instead.
    IoTimeout,
}

/// Outcome of a content read for inexact scoring.
pub enum ContentOutcome {
    Content(Rc<Vec<u8>>),
    Skipped(SkipReason),
}

/// Caller-supplied content provider. Implementations own read budgets and
/// OID de-duplication; the engine caches spanhash entries per path, so each
/// path is requested at most once per run.
pub trait RenameContentSource {
    fn old_content(&mut self, path: &Path, blob: &BlobRef) -> ContentOutcome;
    fn new_content(&mut self, path: &Path, blob: &BlobRef) -> ContentOutcome;
}

/// Run statistics for warnings and tests (§B.4.2.6).
#[derive(Debug, Default, Clone)]
pub struct RenameDetectStats {
    pub comparisons: u64,
    /// Exhaustive stage skipped because a side exceeded `rename_limit`.
    pub skipped_by_limit: bool,
    /// Comparison budget hit: every exhaustive edge was discarded and only
    /// exact + basename pairs survive (§B.4.2.5 触顶规则).
    pub exhaustive_discarded: bool,
    /// Peak number of retained inexact edges (`≤ PER_DEST_TOP_K × dests`).
    pub peak_edges: usize,
    /// Per-reason counts of candidates dropped by content-read failures.
    pub content_skips: HashMap<SkipReason, u64>,
}

/// Engine output.
#[derive(Debug, Default)]
pub struct RenameDetectOutcome {
    /// Matched pairs, sorted by (old, new) path component order
    /// (§B.4.2.5, 2026-08-05 revision).
    pub matches: Vec<RenameMatch>,
    pub stats: RenameDetectStats,
}

/// Per-path spanhash cache entry: `None` when content was skipped.
struct SpanhashSlot {
    entry: Option<Rc<SpanhashEntry>>,
}

pub(crate) struct SpanhashEntry {
    counts: HashMap<u64, u64>,
    len: u64,
}

/// Match renames between the two snapshot sides (§B.4.2 stage order).
pub fn match_pairs(
    snapshot: &RenameSnapshot,
    config: &RenameDetectConfig,
    source: &mut dyn RenameContentSource,
) -> RenameDetectOutcome {
    let mut stats = RenameDetectStats::default();
    let mut matches: Vec<RenameMatch> = Vec::new();

    // Deterministic path ordering: BTree keys compare with `Path`'s `Ord`,
    // which is component-wise (each component by bytes) — not the raw path
    // byte string. The distinction only matters when candidates diverge at
    // a directory boundary against a byte below `/`; both diff and status
    // share this one engine, so the realized order is identical across
    // commands either way.
    let mut remaining_old: BTreeMap<&PathBuf, &BlobRef> = snapshot.old_map.iter().collect();
    let mut remaining_new: BTreeMap<&PathBuf, &BlobRef> = snapshot.new_map.iter().collect();

    // ---- Stage 1: exact (bucket by oid + kind [+ gitlink mode]) ----------
    type BucketKey = (ObjectHash, BlobKind, Option<u32>);
    // Destinations of one exact bucket, split by evidence kind. Inside a
    // bucket every entry shares the same oid/kind[/mode] and neither side
    // can be Unknown (Unknown has no OID and never enters a bucket), so
    // `BlobEvidence::exact_pair_allowed` reduces to `old_known || new_known`
    // and each pick is two BTreeSet head lookups instead of a scan.
    #[derive(Default)]
    struct ExactBucket {
        known: BTreeSet<PathBuf>,
        computed: BTreeSet<PathBuf>,
    }
    impl ExactBucket {
        /// Smallest path (component order, §B.4.2.5 revision) the old side
        /// may pair with, removed from the bucket. A known old evidence
        /// pairs with any destination;
        /// a computed one pairs only with a known destination — C/C exact is
        /// forbidden (§B.4.1).
        fn pop_first_allowed(&mut self, old_known: bool) -> Option<PathBuf> {
            let picked = if old_known {
                match (self.known.iter().next(), self.computed.iter().next()) {
                    (Some(k), Some(c)) => {
                        if k <= c {
                            Some(k.clone())
                        } else {
                            Some(c.clone())
                        }
                    }
                    (Some(k), None) => Some(k.clone()),
                    (None, Some(c)) => Some(c.clone()),
                    (None, None) => None,
                }
            } else {
                self.known.iter().next().cloned()
            }?;
            self.known.remove(&picked);
            self.computed.remove(&picked);
            Some(picked)
        }
    }
    let mut buckets: HashMap<BucketKey, ExactBucket> = HashMap::new();
    for (path, blob) in &remaining_new {
        if let Some(oid) = blob.evidence.oid() {
            let mode_key = (blob.kind == BlobKind::Gitlink).then_some(blob.mode);
            let bucket = buckets.entry((*oid, blob.kind, mode_key)).or_default();
            if blob.evidence.is_known() {
                bucket.known.insert((*path).clone());
            } else {
                bucket.computed.insert((*path).clone());
            }
        }
    }

    // §B.4.2.2 exact selection is GLOBAL, not per-source-in-order. Every
    // allowed (old, new) candidate edge is enumerated first and sorted
    // `same_basename DESC, old ASC, new ASC`, then consumed greedily. Walking
    // sources in path order and letting each take its best remaining
    // destination gets this wrong: with `src/alpha.txt` and `src/beta.txt`
    // both matching `dst/beta.txt` and `dst/zzz.txt`, `alpha` (first source,
    // no basename match) would claim `dst/beta.txt` and strand the pair that
    // actually shares a name. Git's diffcore runs the same-basename pass
    // first for exactly this reason.
    // Two passes over the SOURCES, never over the old × new product: a
    // repository with thousands of identically-named duplicate blobs must
    // not materialize a cartesian edge set here (that is quadratic in both
    // time and memory, before `rename_limit` has had any say).
    //
    // Pass A gives every source its same-basename destination if the bucket
    // holds one; pass B assigns what remains in path order. The result is
    // the same global "same_basename DESC, old ASC, new ASC" ranking a
    // sorted edge list would produce, because a same-basename edge exists
    // for at most one destination per source.
    let mut consumed_old: BTreeSet<PathBuf> = BTreeSet::new();
    let mut consumed_new: BTreeSet<PathBuf> = BTreeSet::new();
    // (bucket, basename) → destinations, so the same-basename pass is a map
    // lookup instead of a linear scan of the whole OID bucket. Entries are
    // REMOVED as they are consumed (same rule for the path-order level):
    // leaving consumed destinations in place makes every later source
    // rescan them, and N same-OID paths sharing one basename degenerate to
    // Θ(N²) before `rename_limit` has had any say. With consumption every
    // destination is examined O(1) times per stage and the whole selection
    // stays O(N log N).
    let mut by_basename: HashMap<(BucketKey, Option<OsString>), ExactBucket> = HashMap::new();
    for (key, bucket) in &buckets {
        for (is_known, paths) in [(true, &bucket.known), (false, &bucket.computed)] {
            for path in paths {
                let entry = by_basename
                    .entry((*key, path.file_name().map(|name| name.to_os_string())))
                    .or_default();
                if is_known {
                    entry.known.insert(path.clone());
                } else {
                    entry.computed.insert(path.clone());
                }
            }
        }
    }
    // `remaining_old` is a BTreeMap, so both passes walk sources in path
    // component order (§B.4.2.5 revision) without an extra sort.
    let old_keys: Vec<PathBuf> = remaining_old.keys().map(|p| (*p).clone()).collect();
    for same_basename_pass in [true, false] {
        for old_path in &old_keys {
            if consumed_old.contains(old_path) {
                continue;
            }
            let Some(old_blob) = remaining_old.get(old_path) else {
                continue;
            };
            let Some(oid) = old_blob.evidence.oid() else {
                continue;
            };
            let mode_key = (old_blob.kind == BlobKind::Gitlink).then_some(old_blob.mode);
            let bucket_key = (*oid, old_blob.kind, mode_key);
            let old_known = old_blob.evidence.is_known();
            let picked = if same_basename_pass {
                // Basename LOOKUP, not a bucket scan: with N same-OID files
                // whose basenames all differ, scanning each bucket per
                // source is O(N²) — and it happens before `rename_limit` can
                // gate anything.
                by_basename
                    .get_mut(&(bucket_key, old_path.file_name().map(|n| n.to_os_string())))
                    .and_then(|bucket| bucket.pop_first_allowed(old_known))
            } else {
                buckets
                    .get_mut(&bucket_key)
                    .and_then(|bucket| bucket.pop_first_allowed(old_known))
            };
            if let Some(new_path) = picked {
                #[cfg(debug_assertions)]
                {
                    // The split-bucket pick must agree with the legality
                    // table on every emitted pair.
                    let new_blob = remaining_new.get(&new_path);
                    debug_assert!(new_blob.is_some_and(|b| {
                        BlobEvidence::exact_pair_allowed(&old_blob.evidence, &b.evidence)
                    }));
                }
                if same_basename_pass {
                    // Keep the path-order level in sync for pass B.
                    if let Some(bucket) = buckets.get_mut(&bucket_key) {
                        bucket.known.remove(&new_path);
                        bucket.computed.remove(&new_path);
                    }
                }
                consumed_old.insert(old_path.clone());
                consumed_new.insert(new_path.clone());
                matches.push(RenameMatch {
                    old: old_path.clone(),
                    new: new_path,
                    exact: true,
                    internal_score: EXACT_SCORE,
                });
            }
        }
    }
    for path in &consumed_old {
        remaining_old.remove(path);
    }
    for path in &consumed_new {
        remaining_new.remove(path);
    }

    // ---- Stage 2: `-M100%` stops after exact ------------------------------
    if config.threshold >= EXACT_SCORE {
        matches.sort_by(|a, b| a.old.cmp(&b.old).then_with(|| a.new.cmp(&b.new)));
        return RenameDetectOutcome { matches, stats };
    }

    // Spanhash caches;每 path 至多读取一次内容。
    let mut old_hashes: HashMap<PathBuf, SpanhashSlot> = HashMap::new();
    let mut new_hashes: HashMap<PathBuf, SpanhashSlot> = HashMap::new();
    let mut budget_exhausted = false;

    // ---- Stage 3: unique-basename pairing (always runs, §B.4.2.3) --------
    let mut old_by_basename: BTreeMap<OsString, Vec<&PathBuf>> = BTreeMap::new();
    for path in remaining_old.keys() {
        if let Some(name) = path.file_name() {
            old_by_basename
                .entry(name.to_os_string())
                .or_default()
                .push(path);
        }
    }
    let mut new_by_basename: BTreeMap<OsString, Vec<&PathBuf>> = BTreeMap::new();
    for path in remaining_new.keys() {
        if let Some(name) = path.file_name() {
            new_by_basename
                .entry(name.to_os_string())
                .or_default()
                .push(path);
        }
    }

    let mut basename_pairs: Vec<(PathBuf, PathBuf, u32)> = Vec::new();
    for (name, olds) in &old_by_basename {
        if olds.len() != 1 {
            continue;
        }
        let Some(news) = new_by_basename.get(name) else {
            continue;
        };
        if news.len() != 1 {
            continue;
        }
        let (old_path, new_path) = (olds[0], news[0]);
        let old_blob = remaining_old[old_path];
        let new_blob = remaining_new[new_path];
        if !inexact_eligible(old_blob) || !inexact_eligible(new_blob) {
            continue;
        }
        // The basename stage always RUNS before the gate (§B.4.2.3), but the
        // comparison budget is a real bound on the whole detection pass: a
        // tree with 500_001 unique-basename pairs would otherwise score
        // every one of them and report no degradation at all. Once the
        // budget is spent the stage STOPS (keeping what it already paired)
        // and the run is marked degraded.
        if let Some(budget) = config.comparison_budget
            && stats.comparisons >= budget
        {
            stats.exhaustive_discarded = true;
            break;
        }
        stats.comparisons += 1;
        let Some(old_entry) = spanhash_for(
            &mut old_hashes,
            old_path,
            old_blob,
            Side::Old,
            source,
            &mut stats,
        ) else {
            continue;
        };
        let Some(new_entry) = spanhash_for(
            &mut new_hashes,
            new_path,
            new_blob,
            Side::New,
            source,
            &mut stats,
        ) else {
            continue;
        };
        let score = similarity_from_entries(&old_entry, &new_entry);
        if score >= config.threshold {
            basename_pairs.push(((*old_path).clone(), (*new_path).clone(), score));
        }
    }
    for (old, new, score) in basename_pairs {
        remaining_old.remove(&old);
        remaining_new.remove(&new);
        matches.push(RenameMatch {
            old,
            new,
            exact: false,
            internal_score: score,
        });
    }

    // ---- Stage 4: renameLimit gate (per-side OR, §B.7) --------------------
    let sources = remaining_old.len();
    let destinations = remaining_new.len();
    let limit_skips = config.rename_limit > 0
        && (sources > config.rename_limit || destinations > config.rename_limit);
    if limit_skips {
        stats.skipped_by_limit = true;
    }

    // ---- Stage 5: bounded exhaustive inexact ------------------------------
    if !limit_skips && !budget_exhausted {
        // Edge ordering (§B.4.2.5, 2026-08-05 revision): score desc,
        // same-basename first, then old/new paths ascending in `Path`
        // component order. `Reverse`-free: encode as a sortable tuple with
        // inverted score.
        #[derive(PartialEq, Eq, PartialOrd, Ord)]
        struct EdgeKey(u32, bool, PathBuf, PathBuf); // (60000-score, !same_basename, old, new)

        let mut per_dest: BTreeMap<&PathBuf, Vec<(EdgeKey, u32)>> = BTreeMap::new();
        'outer: for (new_path, new_blob) in &remaining_new {
            if !inexact_eligible(new_blob) {
                continue;
            }
            let Some(new_entry) = spanhash_for(
                &mut new_hashes,
                new_path,
                new_blob,
                Side::New,
                source,
                &mut stats,
            ) else {
                continue;
            };
            for (old_path, old_blob) in &remaining_old {
                if !inexact_eligible(old_blob) {
                    continue;
                }
                if !consume_comparison(config, &mut stats, &mut budget_exhausted) {
                    break 'outer;
                }
                let Some(old_entry) = spanhash_for(
                    &mut old_hashes,
                    old_path,
                    old_blob,
                    Side::Old,
                    source,
                    &mut stats,
                ) else {
                    continue;
                };
                let score = similarity_from_entries(&old_entry, &new_entry);
                if score < config.threshold {
                    continue;
                }
                let same_basename = old_path.file_name() == new_path.file_name();
                let key = EdgeKey(
                    EXACT_SCORE - score,
                    !same_basename,
                    (*old_path).clone(),
                    (*new_path).clone(),
                );
                let edges = per_dest.entry(new_path).or_default();
                edges.push((key, score));
                edges.sort();
                if edges.len() > PER_DEST_TOP_K {
                    edges.truncate(PER_DEST_TOP_K);
                }
            }
        }

        if budget_exhausted {
            // 触顶规则：丢弃整个 exhaustive 阶段已评结果。
            stats.exhaustive_discarded = true;
        } else {
            let mut all_edges: Vec<(EdgeKey, u32)> = Vec::new();
            for (_, edges) in per_dest {
                all_edges.extend(edges);
            }
            stats.peak_edges = all_edges.len();
            all_edges.sort_by(|a, b| a.0.cmp(&b.0));
            for (EdgeKey(_, _, old, new), score) in all_edges {
                if remaining_old.contains_key(&old) && remaining_new.contains_key(&new) {
                    remaining_old.remove(&old);
                    remaining_new.remove(&new);
                    matches.push(RenameMatch {
                        old,
                        new,
                        exact: false,
                        internal_score: score,
                    });
                }
            }
        }
    }

    matches.sort_by(|a, b| a.old.cmp(&b.old).then_with(|| a.new.cmp(&b.new)));
    RenameDetectOutcome { matches, stats }
}

enum Side {
    Old,
    New,
}

/// Inexact scoring is Regular↔Regular only, and skips empty files (§B.4.1).
fn inexact_eligible(blob: &BlobRef) -> bool {
    blob.kind == BlobKind::Regular && blob.size != Some(0)
}

/// Consume one comparison from the budget. Returns false (and flags
/// exhaustion) when the budget is already spent.
fn consume_comparison(
    config: &RenameDetectConfig,
    stats: &mut RenameDetectStats,
    exhausted: &mut bool,
) -> bool {
    if let Some(budget) = config.comparison_budget
        && stats.comparisons >= budget
    {
        *exhausted = true;
        return false;
    }
    stats.comparisons += 1;
    true
}

fn spanhash_for(
    cache: &mut HashMap<PathBuf, SpanhashSlot>,
    path: &PathBuf,
    blob: &BlobRef,
    side: Side,
    source: &mut dyn RenameContentSource,
    stats: &mut RenameDetectStats,
) -> Option<Rc<SpanhashEntry>> {
    if let Some(slot) = cache.get(path) {
        return slot.entry.clone();
    }
    let outcome = match side {
        Side::Old => source.old_content(path, blob),
        Side::New => source.new_content(path, blob),
    };
    let entry = match outcome {
        ContentOutcome::Content(bytes) => Some(Rc::new(spanhash_entry(&bytes))),
        ContentOutcome::Skipped(reason) => {
            *stats.content_skips.entry(reason).or_default() += 1;
            None
        }
    };
    cache.insert(
        path.clone(),
        SpanhashSlot {
            entry: entry.clone(),
        },
    );
    entry
}

// ---------------------------------------------------------------------------
// Scoring (moved verbatim in semantics from `diff.rs`; see §B.4.2)
// ---------------------------------------------------------------------------

/// Chunk `data` the way Git's rename spanhash does — a chunk ends at a newline
/// or after 64 bytes; a `\r` in a `\r\n` is ignored for text — and accumulate
/// the byte count per chunk-hash. We hash each chunk with FNV-1a rather than
/// Git's weaker `HASHBASE` rolling hash: for real content the similarity is
/// identical (equal chunks always match; FNV collisions are astronomically
/// rare), but a contrived input engineered to collide under Git's hash can
/// score differently.
pub(crate) fn spanhash_counts(data: &[u8]) -> HashMap<u64, u64> {
    let is_text = !data.contains(&0);
    let mut counts: HashMap<u64, u64> = HashMap::new();
    let mut chunk: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if is_text && c == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n' {
            i += 1;
            continue;
        }
        chunk.push(c);
        i += 1;
        if chunk.len() >= 64 || c == b'\n' {
            *counts.entry(fnv1a(&chunk)).or_default() += chunk.len() as u64;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        *counts.entry(fnv1a(&chunk)).or_default() += chunk.len() as u64;
    }
    counts
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub(crate) fn spanhash_entry(data: &[u8]) -> SpanhashEntry {
    SpanhashEntry {
        counts: spanhash_counts(data),
        len: data.len() as u64,
    }
}

/// Git's similarity score (0..60000) from precomputed spanhash entries.
pub(crate) fn similarity_from_entries(old: &SpanhashEntry, new: &SpanhashEntry) -> u32 {
    let max_size = old.len.max(new.len);
    if max_size == 0 {
        return EXACT_SCORE;
    }
    let mut common: u64 = 0;
    for (hash, &old_bytes) in &old.counts {
        if let Some(&new_bytes) = new.counts.get(hash) {
            common += old_bytes.min(new_bytes);
        }
    }
    ((common * u64::from(EXACT_SCORE)) / max_size) as u32
}

/// Git's similarity score (0..60000): common chunk bytes * 60000 / max file
/// size. Two empty files are identical (full score). The displayed percent is
/// `score / 600`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn similarity_score(old: &[u8], new: &[u8]) -> u32 {
    similarity_from_entries(&spanhash_entry(old), &spanhash_entry(new))
}

// ---------------------------------------------------------------------------
// Bounded content readers (§B.3.4 预算表)
// ---------------------------------------------------------------------------

/// Budgeted, OID-deduplicated repository blob reader for inexact scoring.
/// Known-OID exact pairing never reads objects; only candidates that need
/// content pass through here. Failures map to [`SkipReason`]s so a missing or
/// corrupt object degrades to "skip this inexact candidate" (§B.4.1) instead
/// of failing base status.
pub(crate) struct ObjectReadBudget {
    per_object_cap: u64,
    remaining_total: u64,
    remaining_objects: u32,
    deadline: Instant,
    cache: HashMap<ObjectHash, ContentSlot>,
}

enum ContentSlot {
    Content(Rc<Vec<u8>>),
    Skipped(SkipReason, ObjectReadProvenance),
}

/// Where an object-read skip came from — the worker COMPLETING its attempt
/// and reporting a store outcome, versus the transport layer failing before
/// any outcome existed (helper spawn/dispatch/protocol failure, or the
/// deadline kill). Callers that recover skipped reads through a second,
/// unbounded path (diff, W5-09) may only do so for `Completed` skips: a
/// transport failure says nothing about the store, and retrying it in-process
/// would reopen exactly the unbounded stall the worker exists to bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObjectReadProvenance {
    /// The worker ran the read and reported the outcome.
    Completed,
    /// The read never produced a worker-reported outcome.
    Transport,
}

impl ObjectReadBudget {
    /// Remaining (total bytes, object slots) — the state a caller carries
    /// between detection sides. Plain numbers, so the caller never has to
    /// hold the budget itself (its cache is `Rc`-based and not `Send`).
    pub fn remaining(&self) -> (u64, u32) {
        (self.remaining_total, self.remaining_objects)
    }

    /// A budget resumed from a previous side's remaining amounts, keeping
    /// the ORIGINAL deadline and the OID cache. A fresh deadline would give
    /// the second side another full 5 s (the cap is per scoring BATCH), and
    /// a fresh cache would re-read objects both sides share.
    pub fn resumed(
        remaining_total: u64,
        remaining_objects: u32,
        deadline: Instant,
        cache: Vec<(ObjectHash, Result<Vec<u8>, SkipReason>)>,
    ) -> Self {
        Self {
            per_object_cap: OBJECT_READ_MAX_OBJECT_BYTES,
            remaining_total,
            remaining_objects,
            deadline,
            cache: cache
                .into_iter()
                .map(|(oid, slot)| {
                    let slot = match slot {
                        Ok(bytes) => ContentSlot::Content(Rc::new(bytes)),
                        // The carried form drops provenance; resume it as
                        // Transport — the conservative default that never
                        // authorizes an unbounded fallback retry. Only
                        // status uses `resumed`, and status never falls
                        // back, so this is currently unobservable.
                        Err(reason) => {
                            ContentSlot::Skipped(reason, ObjectReadProvenance::Transport)
                        }
                    };
                    (oid, slot)
                })
                .collect(),
        }
    }

    /// The batch deadline, so a caller can carry it to the next side.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Take the OID cache in a form a caller can hold across an `.await`.
    ///
    /// The live cache stores `Rc<Vec<u8>>`, which is not `Send`; the carried
    /// form owns its bytes so the de-duplication survives the trip between
    /// detection sides without pinning the future to one thread.
    pub fn take_cache(&mut self) -> Vec<(ObjectHash, Result<Vec<u8>, SkipReason>)> {
        std::mem::take(&mut self.cache)
            .into_iter()
            .map(|(oid, slot)| match slot {
                ContentSlot::Content(bytes) => (
                    oid,
                    Ok(Rc::try_unwrap(bytes).unwrap_or_else(|shared| (*shared).clone())),
                ),
                ContentSlot::Skipped(reason, _) => (oid, Err(reason)),
            })
            .collect()
    }

    pub fn with_defaults() -> Self {
        Self::new(
            OBJECT_READ_MAX_OBJECT_BYTES,
            OBJECT_READ_MAX_TOTAL_BYTES,
            OBJECT_READ_MAX_OBJECTS,
            OBJECT_READ_DEADLINE,
        )
    }

    pub fn new(
        per_object_cap: u64,
        max_total_bytes: u64,
        max_objects: u32,
        deadline: Duration,
    ) -> Self {
        Self {
            per_object_cap,
            remaining_total: max_total_bytes,
            remaining_objects: max_objects,
            deadline: Instant::now() + deadline,
            cache: HashMap::new(),
        }
    }

    /// Read a blob's content under budget, de-duplicating by OID: repeated
    /// requests for the same object return the cached outcome without
    /// consuming budget again.
    ///
    /// Content loads go through the out-of-process status I/O worker
    /// (WIO-03) so a hung tier/store read can kill the helper within the
    /// remaining batch deadline instead of stalling the whole rename pass.
    pub fn read_blob(&mut self, oid: &ObjectHash) -> ContentOutcome {
        self.read_blob_tracked(oid).0
    }

    /// [`Self::read_blob`] plus the skip's [`ObjectReadProvenance`]. The
    /// provenance for a successful read is `Completed` by construction.
    pub(crate) fn read_blob_tracked(
        &mut self,
        oid: &ObjectHash,
    ) -> (ContentOutcome, ObjectReadProvenance) {
        if let Some(slot) = self.cache.get(oid) {
            return match slot {
                ContentSlot::Content(bytes) => (
                    ContentOutcome::Content(bytes.clone()),
                    ObjectReadProvenance::Completed,
                ),
                ContentSlot::Skipped(reason, provenance) => {
                    (ContentOutcome::Skipped(*reason), *provenance)
                }
            };
        }
        // Budget-exceeded outcomes are NOT cached per-OID: they describe the
        // batch, not the object, and later slices may retry with fresh budgets.
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || self.remaining_objects == 0 || self.remaining_total == 0 {
            return (
                ContentOutcome::Skipped(SkipReason::ObjectBudgetExceeded),
                ObjectReadProvenance::Transport,
            );
        }

        // Reserve the object slot up front: failed lookups (missing, corrupt,
        // unavailable, too large) must also consume the 64-object cap, or a
        // pathological candidate set could hammer the store until the
        // wall-clock deadline.
        self.remaining_objects -= 1;
        let cap = self.per_object_cap.min(self.remaining_total);
        // Keep replace-peel semantics consistent with `load_object` (diff and
        // show read through the same substitution). Peel on the parent before
        // submitting — the worker never touches replace refs / SQLite.
        let peeled = super::replace::resolve(*oid);
        let objects_root = match crate::utils::path::try_objects() {
            Ok(path) => path,
            Err(_) => {
                let outcome = ContentSlot::Skipped(
                    SkipReason::ObjectUnavailable,
                    ObjectReadProvenance::Transport,
                );
                self.cache.insert(*oid, outcome);
                return (
                    ContentOutcome::Skipped(SkipReason::ObjectUnavailable),
                    ObjectReadProvenance::Transport,
                );
            }
        };
        let outcome = match crate::command::status_io_worker::deadline_read_object_blob(
            &peeled,
            &objects_root,
            cap,
            remaining,
        ) {
            Err(()) => {
                // Worker killed / timed out mid-read, or the transport never
                // reached a worker (spawn/dispatch failure): cache per-OID so
                // the hung object is not resubmitted, map to metadata_*
                // (ObjectUnavailable → metadata_unavailable, not worktree
                // IoTimeout), and record Transport provenance — no worker
                // outcome exists for a fallback path to trust.
                ContentSlot::Skipped(
                    SkipReason::ObjectUnavailable,
                    ObjectReadProvenance::Transport,
                )
            }
            Ok(crate::command::status_io_worker::ObjectBlobOutcome::Bytes(bytes)) => {
                self.remaining_total = self.remaining_total.saturating_sub(bytes.len() as u64);
                ContentSlot::Content(Rc::new(bytes))
            }
            Ok(crate::command::status_io_worker::ObjectBlobOutcome::Missing) => {
                ContentSlot::Skipped(SkipReason::ObjectMissing, ObjectReadProvenance::Completed)
            }
            Ok(crate::command::status_io_worker::ObjectBlobOutcome::Corrupt) => {
                ContentSlot::Skipped(SkipReason::ObjectCorrupt, ObjectReadProvenance::Completed)
            }
            Ok(crate::command::status_io_worker::ObjectBlobOutcome::Unavailable) => {
                ContentSlot::Skipped(
                    SkipReason::ObjectUnavailable,
                    ObjectReadProvenance::Completed,
                )
            }
            Ok(crate::command::status_io_worker::ObjectBlobOutcome::TooLarge) => {
                ContentSlot::Skipped(SkipReason::ObjectTooLarge, ObjectReadProvenance::Completed)
            }
            Ok(crate::command::status_io_worker::ObjectBlobOutcome::Failed) => {
                ContentSlot::Skipped(SkipReason::ObjectIoFailed, ObjectReadProvenance::Completed)
            }
        };
        let result = match &outcome {
            ContentSlot::Content(bytes) => (
                ContentOutcome::Content(bytes.clone()),
                ObjectReadProvenance::Completed,
            ),
            ContentSlot::Skipped(reason, provenance) => {
                (ContentOutcome::Skipped(*reason), *provenance)
            }
        };
        self.cache.insert(*oid, outcome);
        result
    }
}

/// Budgeted worktree reader: bounded per-file and total bytes, task count and
/// a batch deadline. Every path costs a task (even empty files, §B.3.4).
pub(crate) struct WorktreeReadBudget {
    per_file_cap: u64,
    remaining_total: u64,
    remaining_tasks: u32,
    deadline: Instant,
}

/// Debug-only seam for the stat→read divergence (§B.3.4). A budget that
/// charges the pre-read `stat` length bills a file that grew in between at
/// the smaller stale price. The regression exercises that branch by having
/// the seam report a STALE, smaller length for the named path's pre-read
/// stat — the bounded read then pulls more bytes than the stat claimed,
/// exactly as a growing file would, without the hook ever mutating the
/// file. Gated on `LIBRA_TEST`; compiled out of release builds.
/// Debug-only seam: sleep inside a worktree I/O operation so the §B.3.4
/// batch-deadline tests can make one operation slow without a hung mount.
/// Gated on `LIBRA_TEST` like every seam in this family; named per
/// operation so the stat and the content read can be slowed independently.
/// Compiled out of release builds.
#[cfg(debug_assertions)]
fn test_op_delay(var: &str) {
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return;
    }
    if let Ok(value) = std::env::var(var)
        && let Ok(milliseconds) = value.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    }
}

#[cfg(not(debug_assertions))]
fn test_op_delay(_var: &str) {}

#[cfg(debug_assertions)]
fn stale_stat_len_for_test(path: &Path, real_len: u64) -> u64 {
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_none() {
        return real_len;
    }
    let Ok(target) = std::env::var("LIBRA_TEST_STALE_STAT_PATH") else {
        return real_len;
    };
    if Path::new(&target) != path {
        return real_len;
    }
    let shrink: u64 = std::env::var("LIBRA_TEST_STALE_STAT_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    real_len.saturating_sub(shrink)
}

#[cfg(not(debug_assertions))]
fn stale_stat_len_for_test(_path: &Path, real_len: u64) -> u64 {
    real_len
}

impl WorktreeReadBudget {
    /// Remaining (total bytes, task slots); see [`ObjectReadBudget::remaining`].
    pub fn remaining(&self) -> (u64, u32) {
        (self.remaining_total, self.remaining_tasks)
    }

    /// A budget resumed from a previous side's remaining amounts, keeping
    /// the ORIGINAL batch deadline (see [`ObjectReadBudget::resumed`]).
    pub fn resumed(remaining_total: u64, remaining_tasks: u32, deadline: Instant) -> Self {
        Self {
            per_file_cap: WORKTREE_READ_MAX_FILE_BYTES,
            remaining_total,
            remaining_tasks,
            deadline,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(
            WORKTREE_READ_MAX_FILE_BYTES,
            WORKTREE_READ_MAX_TOTAL_BYTES,
            WORKTREE_READ_MAX_TASKS,
            WORKTREE_READ_DEADLINE,
        )
    }

    pub fn new(
        per_file_cap: u64,
        max_total_bytes: u64,
        max_tasks: u32,
        deadline: Duration,
    ) -> Self {
        Self {
            per_file_cap,
            remaining_total: max_total_bytes,
            remaining_tasks: max_tasks,
            deadline: Instant::now() + deadline,
        }
    }

    fn take_task(&mut self) -> bool {
        if Instant::now() >= self.deadline || self.remaining_tasks == 0 {
            return false;
        }
        self.remaining_tasks -= 1;
        true
    }

    /// How long a single read may block WITHOUT overshooting the batch
    /// budget.
    ///
    /// Checking the batch deadline only before starting a read leaves the
    /// read itself unbounded by it: one begun a moment before the batch
    /// expires still gets the full per-operation window, so a 5s batch could
    /// run to 15s while every individual read looked compliant. The tighter
    /// of the two bounds wins, and a batch with nothing left yields zero —
    /// which the I/O helper refuses outright.
    /// Recomputed IMMEDIATELY BEFORE every individual operation: a window
    /// captured once per call and reused would hand each later operation the
    /// time an earlier slow one already spent, so a 5s batch could run to a
    /// multiple of itself while every individual operation looked compliant
    /// (2026-08-05 R0-1 review).
    pub(crate) fn read_window(&self) -> Duration {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        remaining.min(crate::command::status_probe::io_op_timeout())
    }

    /// Read the blob bytes Git would store for a worktree path (regular file
    /// content, LFS pointer, or symlink target bytes) under budget.
    pub fn read_worktree_blob(&mut self, abs_path: &Path) -> ContentOutcome {
        if !self.take_task() {
            return ContentOutcome::Skipped(SkipReason::WorktreeBudgetExceeded);
        }
        // Deadline-guarded: an optional content read must never wedge the
        // command on a hung mount. Every operation below re-reads the
        // remaining batch window — see `read_window`.
        let stat_target = abs_path.to_path_buf();
        let metadata = match crate::command::status_probe::with_io_deadline_bounded(
            self.read_window(),
            move || {
                test_op_delay("LIBRA_TEST_SLOW_WORKTREE_STAT_MS");
                stat_target.symlink_metadata()
            },
        ) {
            Ok(Ok(metadata)) => metadata,
            Ok(Err(_)) => return ContentOutcome::Skipped(SkipReason::WorktreeIoFailed),
            Err(()) => return ContentOutcome::Skipped(SkipReason::IoTimeout),
        };
        if metadata.file_type().is_symlink() {
            let link_target = abs_path.to_path_buf();
            return match crate::command::status_probe::with_io_deadline_bounded(
                self.read_window(),
                move || super::read_symlink_blob_bytes(&link_target),
            ) {
                Ok(Ok(bytes)) => self.account(bytes),
                Ok(Err(_)) => ContentOutcome::Skipped(SkipReason::WorktreeIoFailed),
                Err(()) => ContentOutcome::Skipped(SkipReason::IoTimeout),
            };
        }
        // LFS classification reads `.gitattributes` files, so it is I/O
        // like any other operation here and runs under a freshly computed
        // batch window — a blocked attributes file must cost the candidate,
        // not hang the command (2026-08-05 R0-1 review).
        let attributes_path = abs_path.to_path_buf();
        let is_lfs = match crate::command::status_probe::with_io_deadline_bounded(
            self.read_window(),
            move || {
                test_op_delay("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS");
                crate::utils::lfs::is_lfs_tracked(&attributes_path)
            },
        ) {
            Ok(is_lfs) => is_lfs,
            Err(()) => return ContentOutcome::Skipped(SkipReason::IoTimeout),
        };
        if is_lfs {
            // The budget covers hydrated bytes, not pointer length (§B.9
            // lfs_budget_counts_hydrated_bytes): charge the on-disk size.
            if metadata.len() > self.per_file_cap.min(self.remaining_total) {
                return ContentOutcome::Skipped(SkipReason::WorktreeTooLarge);
            }
            // Bounded AND deadline-guarded: the pointer is tiny, but
            // producing it hashes the whole file, so the cap is re-applied
            // inside the read (the stat above can be defeated by a file that
            // grows) and the run is reclaimable on a hung mount.
            let cap = self.per_file_cap.min(self.remaining_total);
            let lfs_path = abs_path.to_path_buf();
            return match crate::command::status_probe::with_io_deadline_bounded(
                self.read_window(),
                move || crate::utils::lfs::generate_pointer_file_bounded(&lfs_path, cap),
            ) {
                Err(()) => ContentOutcome::Skipped(SkipReason::IoTimeout),
                // Charge the bytes the read ACTUALLY consumed, not the stat
                // length: a file that grew in between would otherwise get the
                // larger read for the smaller price.
                Ok(Ok((Some((pointer, _)), read))) => {
                    self.remaining_total = self.remaining_total.saturating_sub(read);
                    ContentOutcome::Content(Rc::new(pointer.into_bytes()))
                }
                Ok(Ok((None, read))) => {
                    self.remaining_total = self.remaining_total.saturating_sub(read.max(cap));
                    ContentOutcome::Skipped(SkipReason::WorktreeTooLarge)
                }
                Ok(Err(_)) => ContentOutcome::Skipped(SkipReason::WorktreeIoFailed),
            };
        }
        let cap = self.per_file_cap.min(self.remaining_total);
        if stale_stat_len_for_test(abs_path, metadata.len()) > cap {
            return ContentOutcome::Skipped(SkipReason::WorktreeTooLarge);
        }
        // §B.3.4 bounded read: `read` the file through a `take(cap + 1)`
        // limiter so a file that GREW between the metadata check and the
        // read (or a growing FIFO/FUSE path) can never allocate past the
        // budget, and run it under the shared I/O deadline so a hung mount
        // reclaims the run instead of blocking `status` forever.
        let path = abs_path.to_path_buf();
        let outcome =
            crate::command::status_probe::with_io_deadline_bounded(self.read_window(), move || {
                use std::io::Read as _;

                test_op_delay("LIBRA_TEST_SLOW_WORKTREE_READ_MS");
                let mut file = std::fs::File::open(&path)?;
                let mut bytes = Vec::new();
                file.by_ref()
                    .take(cap.saturating_add(1))
                    .read_to_end(&mut bytes)?;
                Ok::<Vec<u8>, std::io::Error>(bytes)
            });
        match outcome {
            Err(()) => ContentOutcome::Skipped(SkipReason::IoTimeout),
            Ok(Ok(bytes)) => self.account(bytes),
            Ok(Err(_)) => ContentOutcome::Skipped(SkipReason::WorktreeIoFailed),
        }
    }

    fn account(&mut self, bytes: Vec<u8>) -> ContentOutcome {
        let len = bytes.len() as u64;
        // The bytes were READ, so they are charged whether or not the result
        // is usable. Refusing without charging let a file that grew after
        // its size check consume real I/O for free: 4096 tasks could each
        // read ~2 MiB while the 64 MiB total budget stayed untouched.
        let over = len > self.per_file_cap.min(self.remaining_total);
        self.remaining_total = self.remaining_total.saturating_sub(len);
        if over {
            return ContentOutcome::Skipped(SkipReason::WorktreeTooLarge);
        }
        ContentOutcome::Content(Rc::new(bytes))
    }

    /// Stream a worktree path's Git blob OID and byte size under budget
    /// (§B.4.1 `worktree_blob_oid_and_size`). Costs one task; the streamed
    /// bytes are charged against the total budget but never buffered.
    /// Streams the OID and returns the kind OBSERVED during that read, so a
    /// caller that stat'ed the path earlier can detect a type change in
    /// between (a regular file replaced by a symlink would otherwise hand
    /// the exact gate a symlink-target OID labelled `Regular`).
    pub fn worktree_blob_oid_and_size(
        &mut self,
        abs_path: &Path,
    ) -> Result<(ObjectHash, u64, BlobKind), SkipReason> {
        if !self.take_task() {
            return Err(SkipReason::WorktreeBudgetExceeded);
        }
        // Deadline-guarded like every other worktree read: a stat that hangs
        // must reclaim the caller and skip the candidate. Every operation
        // re-reads the remaining batch window — see `read_window`.
        let stat_target = abs_path.to_path_buf();
        let metadata = match crate::command::status_probe::with_io_deadline_bounded(
            self.read_window(),
            move || {
                test_op_delay("LIBRA_TEST_SLOW_WORKTREE_STAT_MS");
                stat_target.symlink_metadata()
            },
        ) {
            Ok(Ok(metadata)) => metadata,
            Ok(Err(_)) => return Err(SkipReason::WorktreeIoFailed),
            Err(()) => return Err(SkipReason::IoTimeout),
        };
        if metadata.file_type().is_symlink() {
            let link_target = abs_path.to_path_buf();
            let bytes = match crate::command::status_probe::with_io_deadline_bounded(
                self.read_window(),
                move || super::read_symlink_blob_bytes(&link_target),
            ) {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(_)) => return Err(SkipReason::WorktreeIoFailed),
                Err(()) => return Err(SkipReason::IoTimeout),
            };
            let len = bytes.len() as u64;
            if len > self.per_file_cap.min(self.remaining_total) {
                // The target bytes were already pulled from the OS: charge
                // them even on refusal, like the sibling symlink branch in
                // `read_worktree_blob`, so an over-cap readlink is never
                // free I/O.
                self.remaining_total = self.remaining_total.saturating_sub(len);
                return Err(SkipReason::WorktreeTooLarge);
            }
            self.remaining_total = self.remaining_total.saturating_sub(len);
            let oid = git_internal::internal::object::blob::Blob::from_content_bytes(bytes).id;
            return Ok((oid, len, BlobKind::Symlink));
        }
        let cap = self.per_file_cap.min(self.remaining_total);
        if stale_stat_len_for_test(abs_path, metadata.len()) > cap {
            return Err(SkipReason::WorktreeTooLarge);
        }
        // Both regular-content branches below produce the blob from bytes
        // pulled through an open that FOLLOWS symlinks, so the kind is
        // re-stated AFTER the read (see the tail of this function).
        // LFS classification reads `.gitattributes` — bounded like every
        // other operation (2026-08-05 R0-1 review).
        let attributes_path = abs_path.to_path_buf();
        let is_lfs = match crate::command::status_probe::with_io_deadline_bounded(
            self.read_window(),
            move || {
                test_op_delay("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS");
                crate::utils::lfs::is_lfs_tracked(&attributes_path)
            },
        ) {
            Ok(is_lfs) => is_lfs,
            Err(()) => return Err(SkipReason::IoTimeout),
        };
        let (oid, len) = if is_lfs {
            // The POINTER is tiny, but producing it requires a full-file
            // SHA-256, so this read needs the same deadline as any other:
            // the size cap above already rejected anything over the
            // per-file budget, and the deadline covers a hung mount.
            let pointer_path = abs_path.to_path_buf();
            let (pointer, read) = match crate::command::status_probe::with_io_deadline_bounded(
                self.read_window(),
                move || crate::utils::lfs::generate_pointer_file_bounded(&pointer_path, cap),
            ) {
                Err(()) => return Err(SkipReason::IoTimeout),
                Ok(Ok((Some((pointer, _)), read))) => (pointer, read),
                // Over the cap, or the file changed size mid-read. Charge the
                // attempt anyway — see the note on the success path below.
                Ok(Ok((None, read))) => {
                    self.remaining_total = self.remaining_total.saturating_sub(read.max(cap));
                    return Err(SkipReason::WorktreeTooLarge);
                }
                Ok(Err(_)) => return Err(SkipReason::WorktreeIoFailed),
            };
            // `read` is the hash's real consumption; `metadata.len()` is a
            // pre-read stat a growing file can make stale.
            self.remaining_total = self.remaining_total.saturating_sub(read);
            let len = pointer.len() as u64;
            let oid = git_internal::internal::object::blob::Blob::from_content(&pointer).id;
            (oid, len)
        } else {
            // The cap is re-applied INSIDE the read: the stat above can be
            // defeated by a file that grows before the bytes are consumed, and
            // an unbounded hash of a now-huge file would blow the read budget
            // it was supposed to respect.
            let hash_path = abs_path.to_path_buf();
            let (oid, read) = match crate::command::status_probe::with_io_deadline_bounded(
                self.read_window(),
                move || {
                    test_op_delay("LIBRA_TEST_SLOW_WORKTREE_READ_MS");
                    super::stream_file_blob_hash_bounded(&hash_path, cap)
                },
            ) {
                Err(()) => return Err(SkipReason::IoTimeout),
                Ok(Ok((Some(oid), read))) => (oid, read),
                // Over the cap, or the file changed size mid-read: skip the
                // candidate rather than trust a hash of a moving target — but
                // still charge the bytes the attempt cost, or a growth race
                // would give an attacker unlimited free reads.
                Ok(Ok((None, read))) => {
                    self.remaining_total = self.remaining_total.saturating_sub(read.max(cap));
                    return Err(SkipReason::WorktreeTooLarge);
                }
                Ok(Err(_)) => return Err(SkipReason::WorktreeIoFailed),
            };
            // Charge and report what the read ACTUALLY consumed. `metadata.len()`
            // is a pre-read stat, and a file that grew in between would otherwise
            // have the larger read billed at the smaller stale price.
            self.remaining_total = self.remaining_total.saturating_sub(read);
            (oid, read)
        };
        // Post-read type revalidation: the bytes above were pulled through
        // an open that FOLLOWS symlinks, so a regular→symlink swap between
        // the pre-read stat and the open would label the referent's blob
        // `Regular` — clearing the exact gate against an unrelated blob and
        // suppressing the truthful `D` + `??`. Re-stat AFTER the read and
        // report the kind observed NOW; the caller drops the candidate when
        // it disagrees with its own earlier stat. The residual window (the
        // path flipping back to regular before this stat) is the documented
        // path-level TOCTOU accepted for R0; fd-bound resolution is tracked
        // by plan-20260715 WIO-02.
        let restat_path = abs_path.to_path_buf();
        let observed = match crate::command::status_probe::with_io_deadline_bounded(
            self.read_window(),
            move || restat_path.symlink_metadata(),
        ) {
            Ok(Ok(meta)) if meta.file_type().is_symlink() => BlobKind::Symlink,
            Ok(Ok(meta)) if meta.file_type().is_file() => BlobKind::Regular,
            Ok(Ok(_)) | Ok(Err(_)) => return Err(SkipReason::WorktreeIoFailed),
            Err(()) => return Err(SkipReason::IoTimeout),
        };
        Ok((oid, len, observed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regular(evidence: BlobEvidence, size: u64) -> BlobRef {
        BlobRef {
            kind: BlobKind::Regular,
            mode: 0o100644,
            size: Some(size),
            evidence,
        }
    }

    fn oid_of(data: &[u8]) -> ObjectHash {
        git_internal::internal::object::blob::Blob::from_content_bytes(data.to_vec()).id
    }

    fn known(data: &[u8]) -> BlobEvidence {
        BlobEvidence::KnownObjectId { oid: oid_of(data) }
    }

    /// Test content source backed by in-memory maps; counts reads.
    struct MapSource {
        old: HashMap<PathBuf, Vec<u8>>,
        new: HashMap<PathBuf, Vec<u8>>,
        reads: u64,
    }

    impl MapSource {
        fn new() -> Self {
            Self {
                old: HashMap::new(),
                new: HashMap::new(),
                reads: 0,
            }
        }
    }

    impl RenameContentSource for MapSource {
        fn old_content(&mut self, path: &Path, _blob: &BlobRef) -> ContentOutcome {
            self.reads += 1;
            match self.old.get(path) {
                Some(bytes) => ContentOutcome::Content(Rc::new(bytes.clone())),
                None => ContentOutcome::Skipped(SkipReason::ObjectMissing),
            }
        }

        fn new_content(&mut self, path: &Path, _blob: &BlobRef) -> ContentOutcome {
            self.reads += 1;
            match self.new.get(path) {
                Some(bytes) => ContentOutcome::Content(Rc::new(bytes.clone())),
                None => ContentOutcome::Skipped(SkipReason::ObjectMissing),
            }
        }
    }

    fn config(threshold: u32) -> RenameDetectConfig {
        RenameDetectConfig {
            threshold,
            rename_limit: 1000,
            comparison_budget: None,
        }
    }

    fn snapshot(old: &[(&str, BlobRef)], new: &[(&str, BlobRef)]) -> RenameSnapshot {
        RenameSnapshot {
            old_map: old
                .iter()
                .map(|(p, b)| (PathBuf::from(p), b.clone()))
                .collect(),
            new_map: new
                .iter()
                .map(|(p, b)| (PathBuf::from(p), b.clone()))
                .collect(),
        }
    }

    #[test]
    fn known_oid_exact_does_not_read_blob() {
        let data = b"identical content\n";
        let snap = snapshot(
            &[("a.txt", regular(known(data), data.len() as u64))],
            &[("b.txt", regular(known(data), data.len() as u64))],
        );
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert!(outcome.matches[0].exact);
        assert_eq!(outcome.matches[0].internal_score, EXACT_SCORE);
        assert_eq!(source.reads, 0, "exact pairing must not read content");
    }

    #[test]
    fn exact_same_basename_duplicate_blob_selection_is_linear() {
        // N delete/add pairs sharing one blob id AND one basename: the
        // same-basename pass must consume each destination as it is picked.
        // Leaving consumed entries in place makes every later source rescan
        // them — Θ(N²) before `rename_limit` (implementation-round Codex
        // finding). With consumption the stage is O(N log N); the generous
        // deadline below can only fail on a quadratic regression, never on
        // machine speed.
        let n = 20_000;
        let blob = regular(known(b"duplicate payload\n"), 18);
        let snap = RenameSnapshot {
            old_map: (0..n)
                .map(|i| (PathBuf::from(format!("old/{i:06}/same.txt")), blob.clone()))
                .collect(),
            new_map: (0..n)
                .map(|i| (PathBuf::from(format!("new/{i:06}/same.txt")), blob.clone()))
                .collect(),
        };
        let mut source = MapSource::new();
        let started = std::time::Instant::now();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        let elapsed = started.elapsed();
        assert_eq!(outcome.matches.len(), n);
        assert!(
            outcome
                .matches
                .iter()
                .all(|m| m.exact && m.internal_score == EXACT_SCORE),
            "every duplicate pair must match exactly"
        );
        assert_eq!(source.reads, 0, "known-OID exact must not read content");
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "exact selection regressed to quadratic: {elapsed:?} for {n} pairs"
        );
    }

    #[test]
    fn exact_bucket_prefers_same_basename() {
        let data = b"same bytes\n";
        let blob = || regular(known(data), data.len() as u64);
        let snap = snapshot(
            &[("dir/a.txt", blob())],
            &[("z/a.txt", blob()), ("aaa/first.txt", blob())],
        );
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        // Same-basename destination wins even though "aaa/first.txt" sorts first.
        assert_eq!(outcome.matches[0].new, PathBuf::from("z/a.txt"));
    }

    #[test]
    fn exact_duplicate_hash_order_stable() {
        let data = b"dup\n";
        let blob = || regular(known(data), data.len() as u64);
        // Two identical sources, two identical destinations — pairing must
        // be deterministic in path component order (§B.4.2.5 revision)
        // regardless of map iteration.
        let snap = snapshot(
            &[("b_old", blob()), ("a_old", blob())],
            &[("d_new", blob()), ("c_new", blob())],
        );
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 2);
        assert_eq!(outcome.matches[0].old, PathBuf::from("a_old"));
        assert_eq!(outcome.matches[0].new, PathBuf::from("c_new"));
        assert_eq!(outcome.matches[1].old, PathBuf::from("b_old"));
        assert_eq!(outcome.matches[1].new, PathBuf::from("d_new"));
    }

    #[test]
    fn unknown_evidence_never_pairs_exact() {
        let data = b"content\n";
        let snap = snapshot(
            &[("a.txt", regular(BlobEvidence::Unknown, data.len() as u64))],
            &[("b.txt", regular(known(data), data.len() as u64))],
        );
        let mut source = MapSource::new();
        // Threshold 60000: only exact stage runs; Unknown must not pair.
        let outcome = match_pairs(&snap, &config(EXACT_SCORE), &mut source);
        assert!(outcome.matches.is_empty());
    }

    #[test]
    fn gitlink_exact_requires_same_mode() {
        let oid = oid_of(b"submodule");
        let gitlink = |mode: u32| BlobRef {
            kind: BlobKind::Gitlink,
            mode,
            size: None,
            evidence: BlobEvidence::KnownObjectId { oid },
        };
        let snap = snapshot(
            &[("sub_a", gitlink(0o160000))],
            &[("sub_b", gitlink(0o160001))],
        );
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert!(
            outcome.matches.is_empty(),
            "gitlink mode mismatch must not pair"
        );
    }

    #[test]
    fn cross_kind_never_pairs() {
        let data = b"target";
        let symlink = BlobRef {
            kind: BlobKind::Symlink,
            mode: 0o120000,
            size: Some(data.len() as u64),
            evidence: known(data),
        };
        let snap = snapshot(
            &[("link", symlink)],
            &[("file", regular(known(data), data.len() as u64))],
        );
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert!(outcome.matches.is_empty());
    }

    #[test]
    fn empty_files_pair_exact_but_not_inexact() {
        let empty = b"";
        let exact_snap = snapshot(
            &[("a", regular(known(empty), 0))],
            &[("b", regular(known(empty), 0))],
        );
        let mut source = MapSource::new();
        let outcome = match_pairs(&exact_snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1, "empty Known-OID exact must pair");

        // Different evidence (no shared OID): empty files must not inexact-pair.
        let inexact_snap = snapshot(
            &[("a", regular(BlobEvidence::Unknown, 0))],
            &[("b", regular(BlobEvidence::Unknown, 0))],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("a"), Vec::new());
        source.new.insert(PathBuf::from("b"), Vec::new());
        let outcome = match_pairs(&inexact_snap, &config(30000), &mut source);
        assert!(outcome.matches.is_empty(), "size==0 skips inexact");
    }

    #[test]
    fn basename_unique_pairs_before_exhaustive() {
        let old_bytes = b"one\ntwo\nthree\nfour\n".to_vec();
        let new_bytes = b"one\ntwo\nchanged\nfour\n".to_vec();
        let snap = snapshot(
            &[(
                "src/util.rs",
                regular(BlobEvidence::Unknown, old_bytes.len() as u64),
            )],
            &[(
                "lib/util.rs",
                regular(BlobEvidence::Unknown, new_bytes.len() as u64),
            )],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("src/util.rs"), old_bytes);
        source.new.insert(PathBuf::from("lib/util.rs"), new_bytes);
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert!(!outcome.matches[0].exact);
        assert!(outcome.matches[0].internal_score >= 30000);
    }

    #[test]
    fn duplicate_basenames_skip_basename_stage() {
        // Two sources share a basename → basename stage must not pair them;
        // exhaustive resolves by score.
        let a = b"alpha alpha alpha alpha\n".to_vec();
        let b = b"beta beta beta beta beta\n".to_vec();
        let snap = snapshot(
            &[
                ("x/f.txt", regular(BlobEvidence::Unknown, a.len() as u64)),
                ("y/f.txt", regular(BlobEvidence::Unknown, b.len() as u64)),
            ],
            &[("z/f.txt", regular(BlobEvidence::Unknown, b.len() as u64))],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("x/f.txt"), a);
        source.old.insert(PathBuf::from("y/f.txt"), b.clone());
        source.new.insert(PathBuf::from("z/f.txt"), b);
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].old, PathBuf::from("y/f.txt"));
    }

    #[test]
    fn exhaustive_greedy_two_by_two_matrix() {
        // Score matrix (approx): old1 is near-identical to new1; old2 to new2.
        // Cross scores are below both. Greedy must take the diagonal.
        let o1 = b"aaaa\nbbbb\ncccc\ndddd\n".to_vec();
        let n1 = b"aaaa\nbbbb\ncccc\nxxxx\n".to_vec();
        let o2 = b"1111\n2222\n3333\n4444\n".to_vec();
        let n2 = b"1111\n2222\n3333\nyyyy\n".to_vec();
        let snap = snapshot(
            &[
                ("old1", regular(BlobEvidence::Unknown, o1.len() as u64)),
                ("old2", regular(BlobEvidence::Unknown, o2.len() as u64)),
            ],
            &[
                ("new1", regular(BlobEvidence::Unknown, n1.len() as u64)),
                ("new2", regular(BlobEvidence::Unknown, n2.len() as u64)),
            ],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("old1"), o1);
        source.old.insert(PathBuf::from("old2"), o2);
        source.new.insert(PathBuf::from("new1"), n1);
        source.new.insert(PathBuf::from("new2"), n2);
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 2);
        let by_old: HashMap<_, _> = outcome
            .matches
            .iter()
            .map(|m| (m.old.clone(), m.new.clone()))
            .collect();
        assert_eq!(by_old[&PathBuf::from("old1")], PathBuf::from("new1"));
        assert_eq!(by_old[&PathBuf::from("old2")], PathBuf::from("new2"));
    }

    /// §B.4.2 stage-5 ordering is `EdgeKey(score DESC, same_basename DESC,
    /// old ASC, new ASC)`. The existing basename tests only exercise the
    /// EXACT bucket, and the path-tie test gives its old and new entries
    /// different basenames — so nothing pinned the `same_basename` component
    /// of the INEXACT ranking.
    ///
    /// This constructs a genuine tie: two destinations with byte-identical
    /// content, so their similarity scores are equal and only the tie-break
    /// can decide. The same-basename candidate sorts LATER by path, so if
    /// the basename term were dropped the other one would win.
    #[test]
    fn exhaustive_tie_break_prefers_the_same_basename() {
        let old_bytes = b"alpha\nbeta\ngamma\ndelta\n".to_vec();
        // Same content on both destinations => identical scores.
        let new_bytes = b"alpha\nbeta\ngamma\nCHANGED\n".to_vec();

        let mut source = MapSource::new();
        source
            .old
            .insert(PathBuf::from("src/name.rs"), old_bytes.clone());
        source
            .new
            .insert(PathBuf::from("dst/aaa-different.rs"), new_bytes.clone());
        source
            .new
            .insert(PathBuf::from("dst/name.rs"), new_bytes.clone());

        let snap = RenameSnapshot {
            old_map: [(
                PathBuf::from("src/name.rs"),
                regular(BlobEvidence::Unknown, old_bytes.len() as u64),
            )]
            .into(),
            new_map: [
                (
                    PathBuf::from("dst/aaa-different.rs"),
                    regular(BlobEvidence::Unknown, new_bytes.len() as u64),
                ),
                (
                    PathBuf::from("dst/name.rs"),
                    regular(BlobEvidence::Unknown, new_bytes.len() as u64),
                ),
            ]
            .into(),
        };

        let cfg = RenameDetectConfig {
            threshold: 3000,
            rename_limit: 0,
            comparison_budget: None,
        };
        let outcome = match_pairs(&snap, &cfg, &mut source);
        assert_eq!(outcome.matches.len(), 1, "{outcome:?}");
        assert!(!outcome.matches[0].exact, "this is an INEXACT pairing");
        assert_eq!(
            outcome.matches[0].new,
            PathBuf::from("dst/name.rs"),
            "equal scores must break toward the shared basename, even though \
             the other candidate sorts first by path: {outcome:?}"
        );
    }

    /// §B.7 `rename_limit` is `sources > limit || destinations > limit` —
    /// per side, an OR. The companion test above trips only the SOURCE side
    /// (3 old / 1 new), and neither `status` (2x2) nor `diff` (3x3) can
    /// isolate the destination branch either. A regression that dropped the
    /// `|| destinations > limit` half would leave all of them green, so the
    /// mirror case is pinned here explicitly.
    #[test]
    fn rename_limit_gates_on_the_destination_side_alone() {
        let content = |i: usize| format!("unique line {i}\nshared shared shared\n").into_bytes();
        let mut source = MapSource::new();

        // ONE source, THREE destinations: only the destination side is over
        // a limit of 2.
        let old_bytes = content(0);
        source.old.insert(PathBuf::from("old0"), old_bytes.clone());
        let mut new_map = std::collections::HashMap::new();
        for i in 0..3 {
            let path = format!("new{i}");
            let bytes = content(i);
            source.new.insert(PathBuf::from(&path), bytes.clone());
            new_map.insert(
                PathBuf::from(&path),
                regular(BlobEvidence::Unknown, bytes.len() as u64),
            );
        }
        let snap = RenameSnapshot {
            old_map: [(
                PathBuf::from("old0"),
                regular(BlobEvidence::Unknown, old_bytes.len() as u64),
            )]
            .into(),
            new_map,
        };

        let cfg = RenameDetectConfig {
            threshold: 30000,
            rename_limit: 2,
            comparison_budget: None,
        };
        let outcome = match_pairs(&snap, &cfg, &mut source);
        assert!(
            outcome.stats.skipped_by_limit,
            "1 source but 3 destinations still trips the per-side limit"
        );
        assert!(
            outcome.matches.is_empty(),
            "and the exhaustive stage is skipped: {outcome:?}"
        );

        // Uncapped, the same input pairs old0 to its moved copy.
        let cfg = RenameDetectConfig {
            threshold: 30000,
            rename_limit: 0,
            comparison_budget: None,
        };
        let mut again = MapSource::new();
        again.old = source.old.clone();
        again.new = source.new.clone();
        let outcome = match_pairs(&snap, &cfg, &mut again);
        assert!(!outcome.stats.skipped_by_limit);
        assert_eq!(outcome.matches.len(), 1, "{outcome:?}");
        assert_eq!(outcome.matches[0].new, PathBuf::from("new0"));
    }

    #[test]
    fn rename_limit_per_side_gates_exhaustive_only() {
        let content = |i: usize| format!("unique line {i}\nshared shared shared\n").into_bytes();
        let mut old = Vec::new();
        let mut source = MapSource::new();
        for i in 0..3 {
            let path = format!("old{i}");
            let bytes = content(i);
            source.old.insert(PathBuf::from(&path), bytes.clone());
            old.push((path, bytes.len() as u64));
        }
        let new_bytes = content(0);
        source
            .new
            .insert(PathBuf::from("moved0"), new_bytes.clone());
        let snap = RenameSnapshot {
            old_map: old
                .iter()
                .map(|(p, len)| (PathBuf::from(p), regular(BlobEvidence::Unknown, *len)))
                .collect(),
            new_map: [(
                PathBuf::from("moved0"),
                regular(BlobEvidence::Unknown, new_bytes.len() as u64),
            )]
            .into(),
        };
        // limit=2 < sources=3 → exhaustive skipped entirely.
        let cfg = RenameDetectConfig {
            threshold: 30000,
            rename_limit: 2,
            comparison_budget: None,
        };
        let outcome = match_pairs(&snap, &cfg, &mut source);
        assert!(outcome.stats.skipped_by_limit);
        assert!(
            outcome.matches.is_empty(),
            "unique basenames differ; nothing pairs"
        );

        // limit=0 → uncapped; exhaustive runs and pairs old0→moved0.
        let cfg = RenameDetectConfig {
            threshold: 30000,
            rename_limit: 0,
            comparison_budget: None,
        };
        let mut source2 = MapSource::new();
        source2.old = source.old.clone();
        source2.new = source.new.clone();
        let outcome = match_pairs(&snap, &cfg, &mut source2);
        assert!(!outcome.stats.skipped_by_limit);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].old, PathBuf::from("old0"));
    }

    #[test]
    fn comparison_budget_discards_exhaustive_keeps_exact_and_basename() {
        let exact_data = b"exact\n";
        let base_old = b"one\ntwo\nthree\nfour\n".to_vec();
        let base_new = b"one\ntwo\nthree\nchanged\n".to_vec();
        let filler = |i: usize| format!("filler {i} {i} {i} {i}\n").into_bytes();
        let mut snap = snapshot(
            &[
                (
                    "exact_old",
                    regular(known(exact_data), exact_data.len() as u64),
                ),
                (
                    "base/same.rs",
                    regular(BlobEvidence::Unknown, base_old.len() as u64),
                ),
            ],
            &[
                (
                    "exact_new",
                    regular(known(exact_data), exact_data.len() as u64),
                ),
                (
                    "moved/same.rs",
                    regular(BlobEvidence::Unknown, base_new.len() as u64),
                ),
            ],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("base/same.rs"), base_old);
        source.new.insert(PathBuf::from("moved/same.rs"), base_new);
        for i in 0..4 {
            let old_path = PathBuf::from(format!("bulk/old{i}"));
            let new_path = PathBuf::from(format!("bulk/new{i}"));
            let bytes = filler(i);
            snap.old_map.insert(
                old_path.clone(),
                regular(BlobEvidence::Unknown, bytes.len() as u64),
            );
            snap.new_map.insert(
                new_path.clone(),
                regular(BlobEvidence::Unknown, bytes.len() as u64),
            );
            source.old.insert(old_path, bytes.clone());
            source.new.insert(new_path, bytes);
        }
        // Budget = 1: the single basename comparison consumes it; the
        // exhaustive stage over bulk/* then hits the ceiling immediately and
        // must discard all its scored edges.
        let cfg = RenameDetectConfig {
            threshold: 30000,
            rename_limit: 0,
            comparison_budget: Some(1),
        };
        let outcome = match_pairs(&snap, &cfg, &mut source);
        assert!(outcome.stats.exhaustive_discarded);
        let kinds: Vec<_> = outcome.matches.iter().map(|m| m.exact).collect();
        assert_eq!(
            outcome.matches.len(),
            2,
            "exact + basename survive: {kinds:?}"
        );
        assert!(outcome.matches.iter().any(|m| m.exact));
        assert!(
            outcome
                .matches
                .iter()
                .any(|m| !m.exact && m.new == Path::new("moved/same.rs"))
        );
        assert!(
            !outcome.matches.iter().any(|m| m.new.starts_with("bulk")),
            "exhaustive results must be discarded on budget exhaustion"
        );
    }

    #[test]
    fn top_k_keeps_highest_scoring_old() {
        // Five sources target one destination; top-K=4 must keep the best
        // scorer even when it arrives last in path order.
        let dest = b"alpha\nbravo\ncharlie\ndelta\necho\n".to_vec();
        let near = b"alpha\nbravo\ncharlie\ndelta\nfoxtrot\n".to_vec();
        let far = |i: usize| format!("alpha\n{i} {i} {i}\n{i}{i}\nx\ny\n").into_bytes();
        let mut source = MapSource::new();
        let mut old_map = HashMap::new();
        for i in 0..4 {
            let p = PathBuf::from(format!("a{i}"));
            let bytes = far(i);
            old_map.insert(
                p.clone(),
                regular(BlobEvidence::Unknown, bytes.len() as u64),
            );
            source.old.insert(p, bytes);
        }
        // "z_best" sorts after the four fillers but scores highest.
        let best = PathBuf::from("z_best");
        old_map.insert(
            best.clone(),
            regular(BlobEvidence::Unknown, near.len() as u64),
        );
        source.old.insert(best.clone(), near);
        let snap = RenameSnapshot {
            old_map,
            new_map: [(
                PathBuf::from("dest"),
                regular(BlobEvidence::Unknown, dest.len() as u64),
            )]
            .into(),
        };
        source.new.insert(PathBuf::from("dest"), dest);
        let cfg = RenameDetectConfig {
            threshold: 6000, // 10%: keep the low-score fillers as edges too
            rename_limit: 0,
            comparison_budget: None,
        };
        let outcome = match_pairs(&snap, &cfg, &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(
            outcome.matches[0].old, best,
            "5th edge must replace a worse one"
        );
        assert!(outcome.stats.peak_edges <= PER_DEST_TOP_K);
    }

    #[test]
    fn content_skip_only_drops_affected_candidate() {
        let ok_old = b"one\ntwo\nthree\nfour\n".to_vec();
        let ok_new = b"one\ntwo\nthree\nfive\n".to_vec();
        let snap = snapshot(
            &[
                (
                    "ok/x.rs",
                    regular(BlobEvidence::Unknown, ok_old.len() as u64),
                ),
                ("broken/y.rs", regular(BlobEvidence::Unknown, 10)),
            ],
            &[
                (
                    "moved/x.rs",
                    regular(BlobEvidence::Unknown, ok_new.len() as u64),
                ),
                ("moved/y.rs", regular(BlobEvidence::Unknown, 10)),
            ],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("ok/x.rs"), ok_old);
        source.new.insert(PathBuf::from("moved/x.rs"), ok_new);
        source
            .new
            .insert(PathBuf::from("moved/y.rs"), b"whatever\n".to_vec());
        // broken/y.rs has no content → ObjectMissing skip; x.rs still pairs.
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].old, PathBuf::from("ok/x.rs"));
        assert!(
            outcome.stats.content_skips[&SkipReason::ObjectMissing] >= 1,
            "skip must be recorded: {:?}",
            outcome.stats.content_skips
        );
    }

    #[test]
    fn tie_break_is_deterministic_by_path_component_order() {
        // Two sources with identical content compete for two identical
        // destinations: ties must resolve old↑ then new↑ in `Path`
        // component order (§B.4.2.5, 2026-08-05 revision; these fixtures
        // order identically under raw bytes — the component-boundary
        // distinction is pinned separately below).
        let bytes = b"tie tie tie tie\n".to_vec();
        let snap = snapshot(
            &[
                ("o/b", regular(BlobEvidence::Unknown, bytes.len() as u64)),
                ("o/a", regular(BlobEvidence::Unknown, bytes.len() as u64)),
            ],
            &[
                ("n/d", regular(BlobEvidence::Unknown, bytes.len() as u64)),
                ("n/c", regular(BlobEvidence::Unknown, bytes.len() as u64)),
            ],
        );
        let mut source = MapSource::new();
        for p in ["o/b", "o/a"] {
            source.old.insert(PathBuf::from(p), bytes.clone());
        }
        for p in ["n/d", "n/c"] {
            source.new.insert(PathBuf::from(p), bytes.clone());
        }
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 2);
        assert_eq!(outcome.matches[0].old, PathBuf::from("o/a"));
        assert_eq!(outcome.matches[0].new, PathBuf::from("n/c"));
        assert_eq!(outcome.matches[1].old, PathBuf::from("o/b"));
        assert_eq!(outcome.matches[1].new, PathBuf::from("n/d"));
    }

    /// §B.4.2.5 (2026-08-05 revision) distinguishing pin, EXACT stage: the
    /// path order is `Path` COMPONENT order, which differs from raw path
    /// bytes exactly when candidates diverge at a directory boundary
    /// against a byte below `/`: raw bytes order `a-b/x` before `a/x`
    /// (`-` = 0x2D < `/` = 0x2F), component order puts `a/x` first
    /// (`a` < `a-b`). The same-basename exact pass consumes sources in
    /// path order, so the winner pins which order is normative.
    #[test]
    fn exact_selection_uses_component_order_at_directory_boundaries() {
        let bytes = b"same exact content\n".to_vec();
        let oid = oid_of(&bytes);
        let blob = || regular(BlobEvidence::KnownObjectId { oid }, bytes.len() as u64);
        let snap = snapshot(&[("a-b/x", blob()), ("a/x", blob())], &[("dest/x", blob())]);
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(
            outcome.matches[0].old,
            PathBuf::from("a/x"),
            "component order (a < a-b) is normative, not raw bytes (a-b/x < a/x)"
        );
    }

    /// §B.4.2.5 (2026-08-05 revision) distinguishing pin, EXACT stage on
    /// the DESTINATION side: one source, two identical destinations that
    /// diverge at the component boundary — the bucket's
    /// `pop_first_allowed` must hand out `a/x`, not the raw-byte-first
    /// `a-b/x`.
    #[test]
    fn exact_destination_uses_component_order_at_directory_boundaries() {
        let bytes = b"same exact content\n".to_vec();
        let oid = oid_of(&bytes);
        let blob = || regular(BlobEvidence::KnownObjectId { oid }, bytes.len() as u64);
        let snap = snapshot(&[("src/x", blob())], &[("a-b/x", blob()), ("a/x", blob())]);
        let mut source = MapSource::new();
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(
            outcome.matches[0].new,
            PathBuf::from("a/x"),
            "the destination bucket resolves in component order too"
        );
    }

    /// §B.4.2.5 (2026-08-05 revision) distinguishing pin, EXHAUSTIVE
    /// stage: same component-boundary fixture, driven through the inexact
    /// edge tie-break (both edges tie on score and same-basename, so the
    /// old-path order alone decides).
    #[test]
    fn exhaustive_tie_break_uses_component_order_at_directory_boundaries() {
        let bytes = b"tie tie tie tie\n".to_vec();
        let snap = snapshot(
            &[
                ("a-b/x", regular(BlobEvidence::Unknown, bytes.len() as u64)),
                ("a/x", regular(BlobEvidence::Unknown, bytes.len() as u64)),
            ],
            &[("n/x", regular(BlobEvidence::Unknown, bytes.len() as u64))],
        );
        let mut source = MapSource::new();
        for p in ["a-b/x", "a/x"] {
            source.old.insert(PathBuf::from(p), bytes.clone());
        }
        source.new.insert(PathBuf::from("n/x"), bytes.clone());
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(
            outcome.matches[0].old,
            PathBuf::from("a/x"),
            "the exhaustive tie-break resolves in component order"
        );
    }

    /// §B.4.2.5 (2026-08-05 revision) distinguishing pin, EXHAUSTIVE
    /// stage on the NEW side: one source, two identical destinations tying
    /// on score and same-basename, so the new-path order alone decides.
    #[test]
    fn exhaustive_new_side_tie_break_uses_component_order() {
        let bytes = b"tie tie tie tie\n".to_vec();
        let snap = snapshot(
            &[("src/x", regular(BlobEvidence::Unknown, bytes.len() as u64))],
            &[
                ("a-b/x", regular(BlobEvidence::Unknown, bytes.len() as u64)),
                ("a/x", regular(BlobEvidence::Unknown, bytes.len() as u64)),
            ],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("src/x"), bytes.clone());
        for p in ["a-b/x", "a/x"] {
            source.new.insert(PathBuf::from(p), bytes.clone());
        }
        let outcome = match_pairs(&snap, &config(30000), &mut source);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(
            outcome.matches[0].new,
            PathBuf::from("a/x"),
            "the new-side exhaustive tie-break resolves in component order"
        );
    }

    #[test]
    fn threshold_60000_is_exact_only() {
        let near_old = b"one\ntwo\nthree\nfour\n".to_vec();
        let near_new = b"one\ntwo\nthree\nfour!\n".to_vec();
        let snap = snapshot(
            &[("a", regular(BlobEvidence::Unknown, near_old.len() as u64))],
            &[("b", regular(BlobEvidence::Unknown, near_new.len() as u64))],
        );
        let mut source = MapSource::new();
        source.old.insert(PathBuf::from("a"), near_old);
        source.new.insert(PathBuf::from("b"), near_new);
        let outcome = match_pairs(&snap, &config(EXACT_SCORE), &mut source);
        assert!(outcome.matches.is_empty());
        assert_eq!(source.reads, 0, "-M100% must not read content");
    }

    #[test]
    fn similarity_score_matches_span_semantics() {
        assert_eq!(similarity_score(b"", b""), EXACT_SCORE);
        assert_eq!(similarity_score(b"abc\n", b"abc\n"), EXACT_SCORE);
        let half = similarity_score(b"aaaa\nbbbb\n", b"aaaa\ncccc\n");
        assert!(half > 20000 && half < 40000, "got {half}");
    }

    #[test]
    fn score_percent_floors_and_caps() {
        let m = |score: u32| RenameMatch {
            old: PathBuf::from("a"),
            new: PathBuf::from("b"),
            exact: false,
            internal_score: score,
        };
        assert_eq!(m(EXACT_SCORE).score_percent(), 100);
        assert_eq!(m(59999).score_percent(), 99, "§B.9: 59999→99 floor");
        assert_eq!(m(30000).score_percent(), 50);
    }

    /// §B.7: the worktree batch deadline must bound the READS, not merely
    /// gate their start.
    ///
    /// `take_task` refused a read that BEGAN after the batch expired, but a
    /// read starting a moment before it got the full per-operation window —
    /// so a 5s batch could run to 15s while every individual read looked
    /// compliant, and nothing degraded or warned. `read_window` now hands
    /// each read the tighter of (batch remaining, per-operation default).
    #[cfg(unix)]
    #[test]
    fn worktree_batch_deadline_bounds_each_read_not_just_its_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let link = dir.path().join("a.link");
        std::os::unix::fs::symlink("target", &link).expect("symlink");

        // A batch with time left reads normally.
        let mut fresh = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_secs(30));
        assert!(
            fresh.read_window() <= crate::command::status_probe::io_op_timeout(),
            "a read never gets more than the per-operation default"
        );
        assert!(matches!(
            fresh.read_worktree_blob(&link),
            ContentOutcome::Content(_)
        ));

        // A batch whose remaining time is SHORTER than the per-operation
        // default hands the read that shorter window — this is the whole
        // point: the batch cap, not the per-read cap, is what binds.
        let brief = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_millis(50));
        assert!(
            brief.read_window() < crate::command::status_probe::io_op_timeout(),
            "the batch remainder wins when it is tighter: {:?}",
            brief.read_window()
        );

        // An EXPIRED batch yields a zero window, and a zero window is
        // refused outright rather than silently promoted to the default.
        let expired = WorktreeReadBudget::new(1024, 1024, 8, Duration::ZERO);
        assert_eq!(
            expired.read_window(),
            Duration::ZERO,
            "an expired batch buys no read time at all"
        );
        assert_eq!(
            crate::command::status_probe::with_io_deadline_bounded(Duration::ZERO, || 1_u8),
            Err(()),
            "and the I/O helper refuses a zero budget instead of running the job"
        );
    }

    /// §B.4.1: the POSITIVE half of budget independence, with a real object
    /// store behind it.
    ///
    /// `object_budget_independent_of_worktree_budget` proves the negative
    /// direction — an exhausted budget refuses — but it runs without a
    /// repository, so its "object side still works" half could only inspect
    /// counters, never perform a read. That cannot distinguish "the object
    /// budget is independent" from "the object budget is broken in the same
    /// way". Here a real repository exists, the WORKTREE budget is spent to
    /// nothing, and an object read is then required to succeed on content it
    /// actually returns. The worktree fixture uses a Unix symlink.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn object_read_still_succeeds_after_the_worktree_budget_is_spent() {
        use std::time::Duration;

        use crate::utils::test::{ChangeDirGuard, ScopedEnvVar, setup_with_new_libra_in};

        let rt = tokio::runtime::Runtime::new().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        rt.block_on(setup_with_new_libra_in(repo.path()));
        let _cwd = ChangeDirGuard::new(repo.path());

        // A real blob in the real object store.
        let payload = b"content the object budget must still be able to read\n";
        let blob = git_internal::internal::object::blob::Blob::from_content_bytes(payload.to_vec());
        let oid = blob.id;
        {
            use std::io::Write as _;
            let hex = oid.to_string();
            let dir = repo.path().join(".libra/objects").join(&hex[..2]);
            std::fs::create_dir_all(&dir).unwrap();
            let mut raw = format!("blob {}\0", payload.len()).into_bytes();
            raw.extend_from_slice(payload);
            let mut enc =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(&raw).unwrap();
            std::fs::write(dir.join(&hex[2..]), enc.finish().unwrap()).unwrap();
        }

        // Spend the WORKTREE budget down to nothing on a real read.
        let link = repo.path().join("spent.link");
        std::os::unix::fs::symlink("t", &link).unwrap();
        let mut worktree = WorktreeReadBudget::new(1024, 1024, 1, Duration::from_secs(30));
        assert!(matches!(
            worktree.read_worktree_blob(&link),
            ContentOutcome::Content(_)
        ));
        assert!(
            matches!(
                worktree.read_worktree_blob(&link),
                ContentOutcome::Skipped(SkipReason::WorktreeBudgetExceeded)
            ),
            "the worktree budget is now genuinely exhausted"
        );

        // The OBJECT budget must still perform a real read and hand back the
        // real bytes — this is what a counter check cannot show.
        let mut objects = ObjectReadBudget::new(1 << 20, 1 << 20, 8, Duration::from_secs(30));
        match objects.read_blob(&oid) {
            ContentOutcome::Content(bytes) => assert_eq!(
                bytes.as_slice(),
                payload,
                "the object read returns the stored content"
            ),
            ContentOutcome::Skipped(reason) => {
                panic!("object read must succeed after worktree exhaustion, got {reason:?}")
            }
        }
    }

    /// §B.3.4: the worktree total is settled by the bytes the read ACTUALLY
    /// consumed, not by the pre-read `stat`. A file that grows between the
    /// stat and the hash used to be billed at the stale, smaller length,
    /// which let a growth race pull unlimited content through a bounded
    /// budget. The seam reports a stale, smaller stat length at exactly that
    /// point — the same accounting branch a growing file exercises, without
    /// the test hook mutating the file.
    #[test]
    #[serial_test::serial]
    fn worktree_total_charges_bytes_read_not_stale_stat() {
        use std::time::Duration;

        use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

        const SHRUNK: u64 = 4;
        const GROWTH: u64 = 4096;
        let dir = tempfile::tempdir().expect("tempdir");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(setup_with_new_libra_in(dir.path()));
        let _cwd = ChangeDirGuard::new(dir.path());
        let path = dir.path().join("grows.txt");
        std::fs::write(&path, vec![b'a'; (SHRUNK + GROWTH) as usize])
            .expect("write the full-size file");

        // SAFETY: single-threaded test body, serialized against other tests
        // that read the process environment. LIBRA_TEST arms the harness
        // gate the seam requires.
        unsafe {
            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            std::env::set_var("LIBRA_TEST_STALE_STAT_PATH", &path);
            std::env::set_var("LIBRA_TEST_STALE_STAT_BYTES", GROWTH.to_string());
        }

        let total = 1_000_000;
        let mut budget = WorktreeReadBudget::new(total, total, 8, Duration::from_secs(30));
        let (_, size, kind) = budget
            .worktree_blob_oid_and_size(&path)
            .expect("the file still hashes");

        unsafe {
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            std::env::remove_var("LIBRA_TEST_STALE_STAT_PATH");
            std::env::remove_var("LIBRA_TEST_STALE_STAT_BYTES");
        }

        let read = SHRUNK + GROWTH;
        assert_eq!(kind, BlobKind::Regular);
        assert_eq!(
            size, read,
            "the reported size is what was read, not the stale 4-byte stat"
        );
        assert_eq!(
            budget.remaining_total,
            total - read,
            "the budget was charged {read} bytes, not the stale 4"
        );
    }

    /// 2026-08-05 R0-1 review: a symlink whose target exceeds the remaining
    /// worktree budget is refused as `WorktreeTooLarge`, but the target
    /// bytes were already pulled from the OS — the refusal must charge
    /// them, like the sibling branch in `read_worktree_blob`, so an
    /// over-cap readlink is never I/O the ledger does not see.
    #[cfg(unix)]
    #[test]
    fn symlink_refusal_charges_the_worktree_budget() {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let link = dir.path().join("over.link");
        std::os::unix::fs::symlink("0123456789", &link).expect("symlink");

        let mut budget = WorktreeReadBudget::new(1024, 4, 8, Duration::from_secs(30));
        let refused = budget.worktree_blob_oid_and_size(&link);
        assert!(
            matches!(refused, Err(SkipReason::WorktreeTooLarge)),
            "a 10-byte target cannot fit a 4-byte remaining budget"
        );
        assert_eq!(
            budget.remaining_total, 0,
            "the readlink bytes are charged (saturating) even on refusal"
        );
    }

    /// 2026-08-05 R0-1 review: the batch deadline bounds the SEQUENCE of
    /// operations inside one read, not each operation from the same stale
    /// entry window. A stat that eats most of the batch leaves the content
    /// read only the remainder; reusing the entry window would let a 5s
    /// batch run to a multiple of itself while every individual operation
    /// looked compliant.
    #[test]
    #[serial_test::serial]
    fn sequential_ops_share_the_batch_deadline() {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("slow.txt");
        std::fs::write(&path, b"content").expect("write fixture");

        // SAFETY: serialized via `serial`; the seam gate mirrors the
        // stale-stat test above.
        unsafe {
            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            std::env::set_var("LIBRA_TEST_SLOW_WORKTREE_STAT_MS", "400");
            std::env::set_var("LIBRA_TEST_SLOW_WORKTREE_READ_MS", "400");
        }
        let mut budget = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_millis(600));
        let outcome = budget.read_worktree_blob(&path);
        unsafe {
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            std::env::remove_var("LIBRA_TEST_SLOW_WORKTREE_STAT_MS");
            std::env::remove_var("LIBRA_TEST_SLOW_WORKTREE_READ_MS");
        }
        assert!(
            matches!(outcome, ContentOutcome::Skipped(SkipReason::IoTimeout)),
            "a 400ms stat must leave the 400ms read too little of the 600ms batch"
        );
    }

    /// 2026-08-05 R0-1 review: LFS classification reads `.gitattributes`,
    /// so it is I/O like any other operation — a blocked attributes lookup
    /// must cost the candidate (`IoTimeout`), not hang the command past
    /// the batch deadline. The seam stands in for a FIFO/hung-mount
    /// attributes file.
    #[test]
    #[serial_test::serial]
    fn lfs_classification_is_bounded_by_the_batch_deadline() {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("classified.txt");
        std::fs::write(&path, b"content").expect("write fixture");

        // SAFETY: serialized via `serial`; same gate as the seams above.
        unsafe {
            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            std::env::set_var("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS", "500");
        }
        let mut budget = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_millis(200));
        let outcome = budget.read_worktree_blob(&path);
        unsafe {
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            std::env::remove_var("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS");
        }
        assert!(
            matches!(outcome, ContentOutcome::Skipped(SkipReason::IoTimeout)),
            "a blocked attributes lookup must be reclaimed by the batch deadline"
        );
    }

    /// Mirror of the pin above for the exact-stage reader: the
    /// `worktree_blob_oid_and_size` path used by status's exact gate must
    /// bound its LFS classification by the batch deadline too.
    #[test]
    #[serial_test::serial]
    fn lfs_classification_is_bounded_on_the_oid_path_too() {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("classified.txt");
        std::fs::write(&path, b"content").expect("write fixture");

        // SAFETY: serialized via `serial`; same gate as the seams above.
        unsafe {
            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            std::env::set_var("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS", "500");
        }
        let mut budget = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_millis(200));
        let outcome = budget.worktree_blob_oid_and_size(&path);
        unsafe {
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            std::env::remove_var("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS");
        }
        assert!(
            matches!(outcome, Err(SkipReason::IoTimeout)),
            "a blocked attributes lookup must be reclaimed on the OID path"
        );
    }

    /// Negative twin of the bounded pins above: the delay seams are inert
    /// without the harness gate. Arming the env vars WITHOUT `LIBRA_TEST`
    /// must not slow any operation, so a short batch still succeeds.
    #[test]
    #[serial_test::serial]
    fn slow_op_seams_are_ignored_without_the_harness_gate() {
        use std::time::Duration;

        use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

        let dir = tempfile::tempdir().expect("tempdir");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(setup_with_new_libra_in(dir.path()));
        let _cwd = ChangeDirGuard::new(dir.path());
        let path = dir.path().join("fast.txt");
        std::fs::write(&path, b"content").expect("write fixture");

        // SAFETY: serialized via `serial`; deliberately NO LIBRA_TEST.
        unsafe {
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            std::env::set_var("LIBRA_TEST_SLOW_WORKTREE_STAT_MS", "5000");
            std::env::set_var("LIBRA_TEST_SLOW_WORKTREE_READ_MS", "5000");
            std::env::set_var("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS", "5000");
        }
        let mut budget = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_millis(1500));
        let outcome = budget.read_worktree_blob(&path);
        unsafe {
            std::env::remove_var("LIBRA_TEST_SLOW_WORKTREE_STAT_MS");
            std::env::remove_var("LIBRA_TEST_SLOW_WORKTREE_READ_MS");
            std::env::remove_var("LIBRA_TEST_SLOW_LFS_ATTRIBUTES_MS");
        }
        assert!(
            matches!(outcome, ContentOutcome::Content(_)),
            "with 5s delays armed but ungated, a 1.5s batch must still succeed"
        );
    }

    /// §B.4.1 designated test: the object-read budget and the worktree-read
    /// budget hold fully independent counters — exhausting one never blocks
    /// the other, and each cap (per-item, total, slot/task) reports through
    /// its own skip reason. Symlink reads keep the worktree side free of any
    /// repository/attribute dependency.
    #[cfg(unix)]
    #[test]
    fn object_budget_independent_of_worktree_budget() {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let small = dir.path().join("small.link");
        let big = dir.path().join("big.link");
        std::os::unix::fs::symlink("abc", &small).expect("small symlink");
        std::os::unix::fs::symlink("0123456789", &big).expect("big symlink");

        // An exhausted OBJECT budget (zero object slots) refuses up front...
        let mut objects = ObjectReadBudget::new(1024, 1024, 0, Duration::from_secs(30));
        assert!(matches!(
            objects.read_blob(&oid_of(b"whatever")),
            ContentOutcome::Skipped(SkipReason::ObjectBudgetExceeded)
        ));

        // ...while a worktree budget still reads through its own counters.
        let mut worktree = WorktreeReadBudget::new(4, 1024, 2, Duration::from_secs(30));
        assert!(matches!(
            worktree.read_worktree_blob(&small),
            ContentOutcome::Content(_)
        ));
        // Worktree per-file cap trips as WorktreeTooLarge (not a budget
        // exhaustion, and never an OBJECT-side reason)...
        assert!(matches!(
            worktree.read_worktree_blob(&big),
            ContentOutcome::Skipped(SkipReason::WorktreeTooLarge)
        ));
        // ...and the task cap trips as WorktreeBudgetExceeded on the third.
        assert!(matches!(
            worktree.read_worktree_blob(&small),
            ContentOutcome::Skipped(SkipReason::WorktreeBudgetExceeded)
        ));

        // The exhausted worktree budget leaves a fresh object budget's
        // pre-checks untouched: only ITS zero-slot/zero-byte/deadline caps
        // refuse, proving there is no shared pool between the two.
        let mut fresh_zero_total = ObjectReadBudget::new(1024, 0, 8, Duration::from_secs(30));
        assert!(matches!(
            fresh_zero_total.read_blob(&oid_of(b"whatever")),
            ContentOutcome::Skipped(SkipReason::ObjectBudgetExceeded)
        ));
        let mut fresh_expired = ObjectReadBudget::new(1024, 1024, 8, Duration::ZERO);
        assert!(matches!(
            fresh_expired.read_blob(&oid_of(b"whatever")),
            ContentOutcome::Skipped(SkipReason::ObjectBudgetExceeded)
        ));

        // Worktree total-byte cap is likewise its own counter: a fresh
        // budget with a tiny total refuses the big symlink, and does so with
        // the WORKTREE-side reason.
        let mut tiny_total = WorktreeReadBudget::new(1024, 4, 8, Duration::from_secs(30));
        assert!(matches!(
            tiny_total.read_worktree_blob(&big),
            ContentOutcome::Skipped(SkipReason::WorktreeTooLarge)
        ));

        // The POSITIVE half of independence, which zero-limit budgets cannot
        // show. Everything above only proves that an ALREADY-EXHAUSTED
        // budget refuses — true even if the two shared one pool.
        //
        // The POSITIVE half, on the two budgets that are LIVE TOGETHER (the
        // zero-limit budgets above only prove an exhausted budget refuses,
        // which would hold even if the two shared one pool): a live object
        // budget's counters must be untouched by worktree reads performed
        // beside it, and vice versa.
        //
        // Asserted on the counters rather than by performing an object read:
        // `read_blob` reaches the global object store, which a unit test
        // running outside a repository does not have.
        let objects_beside = ObjectReadBudget::new(1024, 4096, 8, Duration::from_secs(30));
        let before_objects = objects_beside.remaining();
        let mut worktree_beside = WorktreeReadBudget::new(1024, 4096, 8, Duration::from_secs(30));
        let before_worktree = worktree_beside.remaining();
        assert!(matches!(
            worktree_beside.read_worktree_blob(&small),
            ContentOutcome::Content(_)
        ));
        assert!(matches!(
            worktree_beside.read_worktree_blob(&big),
            ContentOutcome::Content(_)
        ));
        assert_ne!(
            worktree_beside.remaining(),
            before_worktree,
            "the worktree reads really did draw down their own budget"
        );
        assert_eq!(
            objects_beside.remaining(),
            before_objects,
            "…and took nothing from the object budget sitting beside it"
        );

        // Worktree side: with object reads spent, a fresh worktree budget
        // with real limits still returns CONTENT.
        let mut usable_worktree = WorktreeReadBudget::new(1024, 1024, 8, Duration::from_secs(30));
        assert!(matches!(
            usable_worktree.read_worktree_blob(&small),
            ContentOutcome::Content(_)
        ));
    }
}
