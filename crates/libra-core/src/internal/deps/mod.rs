//! File dependency graph (lore.md 3.1) — a typed, VERSIONED per-file
//! dependency-edge subsystem.
//!
//! An edge `(from_path -> to_path, kind)` declares that one file (asset)
//! depends on another. The graph is the reusable substrate for 3.2
//! (dependency-filtered clone/sync) and 3.3 (hydrating VFS): both call
//! [`DependencyStore::transitive_closure`] to expand a root file set into the
//! full set of files that must be materialized.
//!
//! ## Storage — versioned, single-owner (§3.6)
//!
//! Edges are VERSIONED repo metadata, keyed by commit: the authoritative source
//! is one per-commit adjacency document under the reserved notes ref
//! [`REVISION_DEPS_NOTES_REF`] (`refs/notes/deps`), mirroring the landed typed
//! metadata subsystem's `refs/notes/metadata` pattern. `DependencyStore` is the
//! SOLE reader/writer of that ref — no command touches `internal::notes`
//! directly. Because the adjacency doc for a commit is a self-contained,
//! size-bounded unit, every query loads it and computes in memory (cycle-safe
//! BFS); no SQLite projection/cache is introduced (honoring §3.6's "no
//! per-metadata-kind table" red line — a new table would be the forbidden
//! shape, and a cache would add a coherency window this design avoids).
//!
//! ## Wire travel (lore.md 3.2)
//!
//! A Libra deps note is a loose blob (the JSON adjacency doc) plus a row in the
//! SQLite `notes` table — NOT a Git notes-tree-commit, and `refs/notes/deps` is
//! not a real reference-table ref, so it cannot ride the pack/ref want-set. 3.2
//! moves edges cross-machine over a dedicated local-protocol SIDE-CHANNEL:
//! [`DependencyStore::import_notes`] union-merges `(commit, doc)` pairs exported
//! by a source repo's `LocalClient::export_deps_notes`, gated behind
//! `fetch`/`pull --notes` (default OFF, Git parity). It is opt-in and, in v1,
//! LibraRepo↔LibraRepo over the local protocol only (network / foreign-Git /
//! push-side travel are deferred — see `_compatibility.md` D17). A fresh clone
//! reads an empty graph until the notes are fetched with `--notes` (or via
//! `clone --deps-of`, which implies it).

use std::collections::{HashSet, VecDeque};

use serde::{Deserialize, Serialize};

/// The reserved notes ref holding the per-commit dependency adjacency document.
pub const REVISION_DEPS_NOTES_REF: &str = "refs/notes/deps";

/// Document schema version (bumped on an incompatible on-disk change).
const DEPS_DOC_VERSION: u32 = 1;

/// Whole-document byte bound (mirrors metadata's `MAX_VALUE_LEN`), so a commit's
/// dependency note cannot grow into a multi-MiB blob.
const MAX_DOC_LEN: usize = 1 << 20; // 1 MiB

/// Traversal direction for the graph queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow `from -> to` (the deps OF a file).
    Forward,
    /// Follow `to -> from` (the dependents of a file).
    Reverse,
}

/// A single declared dependency edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "generic".to_string()
}

/// The per-commit adjacency document persisted under [`REVISION_DEPS_NOTES_REF`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DepsDoc {
    version: u32,
    edges: Vec<Edge>,
}

/// Result of a transitive-closure query — the reusable seam for 3.2/3.3.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosureResult {
    /// Every path reachable from the roots (roots included), sorted + deduped.
    pub reachable: Vec<String>,
    /// Whether a cycle was encountered during traversal (informational — the
    /// BFS terminates regardless).
    pub cycles_detected: bool,
    /// Whether the depth limit truncated the traversal.
    pub truncated: bool,
}

/// Outcome of [`DependencyStore::import_notes`] — the cross-machine deps-note
/// import (lore.md 3.2). Per-note fault-tolerant: a malformed document or a note
/// whose annotated commit is absent locally is warn-and-skipped, never fatal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    /// Notes union-merged into the local graph (new edges may be zero if the
    /// incoming set was already a subset).
    pub imported: usize,
    /// Notes skipped (malformed, or annotated commit not present locally).
    pub skipped: usize,
    /// Human-readable reasons for each skip (surfaced as fetch warnings).
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DepsError {
    #[error("invalid dependency path '{0}': {1}")]
    InvalidPath(String, String),
    #[error("revision '{0}' could not be resolved: {1}")]
    RevisionNotFound(String, String),
    #[error("a file cannot depend on itself: '{0}'")]
    SelfEdge(String),
    #[error("dependency graph storage error: {0}")]
    Storage(String),
}

/// Normalize a repo-relative path for use as an edge endpoint: strip a leading
/// `./`, convert backslashes to `/`, collapse a trailing slash, and REJECT an
/// absolute path, a `..` escape, or an empty string.
pub fn normalize_edge_path(raw: &str) -> Result<String, DepsError> {
    let unified = raw.replace('\\', "/");
    let trimmed = unified.trim();
    let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
    let stripped = stripped.strip_suffix('/').unwrap_or(stripped);
    if stripped.is_empty() {
        return Err(DepsError::InvalidPath(raw.to_string(), "empty path".into()));
    }
    if stripped.starts_with('/') {
        return Err(DepsError::InvalidPath(
            raw.to_string(),
            "absolute paths are not allowed".into(),
        ));
    }
    if stripped.split('/').any(|c| c == "..") {
        return Err(DepsError::InvalidPath(
            raw.to_string(),
            "'..' path escapes are not allowed".into(),
        ));
    }
    Ok(stripped.to_string())
}

/// Owner module for the file dependency graph. All reads/writes funnel through
/// this type; no command touches the notes ref directly.
pub struct DependencyStore;

impl DependencyStore {
    /// Resolve a revision string (HEAD, a branch, a commit) to a canonical
    /// commit OID hex string, validating that it exists.
    async fn resolve_revision(revision: &str) -> Result<String, DepsError> {
        crate::internal::object_io::get_target_commit(revision)
            .await
            .map(|oid| oid.to_string())
            .map_err(|e| DepsError::RevisionNotFound(revision.to_string(), e.to_string()))
    }

    /// Load the adjacency doc for a commit OID. Absence-tolerant: no note → an
    /// empty edge set (never an error).
    async fn load_doc(oid: &str) -> Result<Vec<Edge>, DepsError> {
        match crate::internal::notes::show(REVISION_DEPS_NOTES_REF, Some(oid)).await {
            Ok((_object, _blob, text)) => {
                if text.len() > MAX_DOC_LEN {
                    return Err(DepsError::Storage(format!(
                        "dependency note for {oid} is {} bytes, over the {MAX_DOC_LEN} bound",
                        text.len()
                    )));
                }
                let doc: DepsDoc = serde_json::from_str(&text).map_err(|e| {
                    DepsError::Storage(format!("dependency note for {oid} is corrupt: {e}"))
                })?;
                if doc.version != DEPS_DOC_VERSION {
                    return Err(DepsError::Storage(format!(
                        "dependency note for {oid} has unsupported version {} (this binary \
                         supports {DEPS_DOC_VERSION})",
                        doc.version
                    )));
                }
                // Codex P1: NEVER trust persisted endpoints — a hand-edited or
                // (once 3.2 lands) fetched note could inject `../x` / `/abs` /
                // backslash escapes that would then flow to 3.2/3.3 consumers.
                // Re-normalize + validate every edge before any query sees it.
                let mut edges = doc.edges;
                for e in &mut edges {
                    e.from = normalize_edge_path(&e.from).map_err(|err| {
                        DepsError::Storage(format!("corrupt edge in note for {oid}: {err}"))
                    })?;
                    e.to = normalize_edge_path(&e.to).map_err(|err| {
                        DepsError::Storage(format!("corrupt edge in note for {oid}: {err}"))
                    })?;
                    if e.from == e.to {
                        return Err(DepsError::Storage(format!(
                            "corrupt self-edge in note for {oid}: '{}'",
                            e.from
                        )));
                    }
                }
                Ok(edges)
            }
            Err(crate::internal::notes::NotesError::NotFound { .. }) => Ok(Vec::new()),
            Err(e) => Err(DepsError::Storage(format!(
                "failed to read the dependency note for {oid}: {e}"
            ))),
        }
    }

    /// Persist the adjacency doc for a commit OID (the SINGLE write path). An
    /// empty edge set removes the note so an empty graph reads identically to a
    /// never-written one.
    async fn store_doc(oid: &str, mut edges: Vec<Edge>) -> Result<(), DepsError> {
        // Deterministic order: dedup + sort so the note blob is stable.
        edges.sort_by(|a, b| (&a.from, &a.to, &a.kind).cmp(&(&b.from, &b.to, &b.kind)));
        edges.dedup();
        if edges.is_empty() {
            match crate::internal::notes::remove(REVISION_DEPS_NOTES_REF, &[oid.to_string()]).await
            {
                Ok(_) | Err(crate::internal::notes::NotesError::NotFound { .. }) => return Ok(()),
                Err(e) => {
                    return Err(DepsError::Storage(format!(
                        "failed to clear the dependency note for {oid}: {e}"
                    )));
                }
            }
        }
        let doc = DepsDoc {
            version: DEPS_DOC_VERSION,
            edges,
        };
        let text = serde_json::to_string_pretty(&doc)
            .map_err(|e| DepsError::Storage(format!("failed to serialize the deps doc: {e}")))?;
        if text.len() > MAX_DOC_LEN {
            return Err(DepsError::Storage(format!(
                "the dependency document for {oid} would exceed {MAX_DOC_LEN} bytes ({} after \
                 this change); remove edges first",
                text.len()
            )));
        }
        crate::internal::notes::add(REVISION_DEPS_NOTES_REF, oid, &text, true)
            .await
            .map(|_| ())
            .map_err(|e| DepsError::Storage(format!("failed to write the dependency note: {e}")))
    }

    /// Add a forward edge `from -> to` (kind `generic` if unspecified) at
    /// `revision`. Idempotent — a duplicate edge is a no-op.
    pub async fn add_edge(
        revision: &str,
        from: &str,
        to: &str,
        kind: &str,
    ) -> Result<(), DepsError> {
        let from = normalize_edge_path(from)?;
        let to = normalize_edge_path(to)?;
        if from == to {
            return Err(DepsError::SelfEdge(from));
        }
        let oid = Self::resolve_revision(revision).await?;
        let mut edges = Self::load_doc(&oid).await?;
        let kind = if kind.is_empty() { "generic" } else { kind };
        if !edges
            .iter()
            .any(|e| e.from == from && e.to == to && e.kind == kind)
        {
            edges.push(Edge {
                from,
                to,
                kind: kind.to_string(),
            });
        }
        Self::store_doc(&oid, edges).await
    }

    /// Remove an edge `from -> to` (of `kind`, or all kinds when `kind` is
    /// empty) at `revision`. A no-op if absent.
    pub async fn remove_edge(
        revision: &str,
        from: &str,
        to: &str,
        kind: &str,
    ) -> Result<bool, DepsError> {
        let from = normalize_edge_path(from)?;
        let to = normalize_edge_path(to)?;
        let oid = Self::resolve_revision(revision).await?;
        let mut edges = Self::load_doc(&oid).await?;
        let before = edges.len();
        edges.retain(|e| !(e.from == from && e.to == to && (kind.is_empty() || e.kind == kind)));
        let removed = edges.len() != before;
        if removed {
            Self::store_doc(&oid, edges).await?;
        }
        Ok(removed)
    }

    /// All edges declared at `revision` (sorted). Absence-tolerant.
    pub async fn all_edges(revision: &str) -> Result<Vec<Edge>, DepsError> {
        let oid = Self::resolve_revision(revision).await?;
        Self::load_doc(&oid).await
    }

    /// Decode + validate an incoming (fetched) adjacency document TEXT into a
    /// trusted edge set. Mirrors [`load_doc`]'s hardening: enforces the size and
    /// version bounds and re-normalizes + validates every endpoint so a hostile
    /// peer cannot inject `/abs`, `..` escapes, backslash paths, or self-edges.
    /// Returns a human-readable reason on rejection (the caller warn-skips it).
    fn decode_import_doc(text: &str) -> Result<Vec<Edge>, String> {
        if text.len() > MAX_DOC_LEN {
            return Err(format!(
                "document is {} bytes, over the {MAX_DOC_LEN} bound",
                text.len()
            ));
        }
        let doc: DepsDoc =
            serde_json::from_str(text).map_err(|e| format!("document is corrupt: {e}"))?;
        if doc.version != DEPS_DOC_VERSION {
            return Err(format!(
                "unsupported document version {} (this binary supports {DEPS_DOC_VERSION})",
                doc.version
            ));
        }
        let mut edges = doc.edges;
        for e in &mut edges {
            e.from = normalize_edge_path(&e.from).map_err(|err| format!("corrupt edge: {err}"))?;
            e.to = normalize_edge_path(&e.to).map_err(|err| format!("corrupt edge: {err}"))?;
            if e.from == e.to {
                return Err(format!("corrupt self-edge: '{}'", e.from));
            }
        }
        Ok(edges)
    }

    /// Import cross-machine dependency notes (lore.md 3.2). `entries` are
    /// `(annotated commit oid, adjacency document text)` pairs produced by a
    /// source repo's `export_deps_notes`. Each note is UNION-merged into the
    /// local graph for its commit (never a clobbering overwrite — an existing
    /// locally-authored edge on the same commit survives). Per-note
    /// fault-tolerant and absence-tolerant: a malformed document, or a note whose
    /// annotated commit is not present in the local object store (realistic under
    /// `--single-branch`/`--depth`/partial history), is warn-and-skipped so a
    /// completed fetch is never aborted after its refs are already updated.
    pub async fn import_notes(entries: &[(String, String)]) -> ImportOutcome {
        let mut outcome = ImportOutcome::default();
        for (raw_oid, text) in entries {
            let incoming = match Self::decode_import_doc(text) {
                Ok(edges) => edges,
                Err(reason) => {
                    outcome.skipped += 1;
                    outcome
                        .warnings
                        .push(format!("skipped dependency note for {raw_oid}: {reason}"));
                    continue;
                }
            };
            // Confirm the annotated commit is present locally BEFORE touching the
            // note store (load_doc/store_doc resolve the commit and would error on
            // an absent one). A missing commit → warn-skip, not a hard failure.
            let oid = match Self::resolve_revision(raw_oid).await {
                Ok(oid) => oid,
                Err(_) => {
                    outcome.skipped += 1;
                    outcome.warnings.push(format!(
                        "skipped dependency note for {raw_oid}: its commit is not present locally"
                    ));
                    continue;
                }
            };
            let mut merged = match Self::load_doc(&oid).await {
                Ok(existing) => existing,
                Err(e) => {
                    outcome.skipped += 1;
                    outcome
                        .warnings
                        .push(format!("skipped dependency note for {raw_oid}: {e}"));
                    continue;
                }
            };
            // Union: append incoming, let store_doc sort + dedup.
            merged.extend(incoming);
            match Self::store_doc(&oid, merged).await {
                Ok(()) => outcome.imported += 1,
                Err(e) => {
                    outcome.skipped += 1;
                    outcome
                        .warnings
                        .push(format!("skipped dependency note for {raw_oid}: {e}"));
                }
            }
        }
        outcome
    }

    /// Immediate neighbors of `path` at `revision` in the given direction,
    /// optionally filtered by kind.
    pub async fn direct(
        revision: &str,
        path: &str,
        direction: Direction,
        kind_filter: Option<&str>,
    ) -> Result<Vec<String>, DepsError> {
        let path = normalize_edge_path(path)?;
        let edges = Self::all_edges(revision).await?;
        let mut out: Vec<String> = edges
            .into_iter()
            .filter(|e| kind_filter.is_none_or(|k| e.kind == k))
            .filter_map(|e| match direction {
                Direction::Forward if e.from == path => Some(e.to),
                Direction::Reverse if e.to == path => Some(e.from),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// The transitive closure of `roots` at `revision` — the reusable seam for
    /// 3.2/3.3. Cycle-safe (a `HashSet` visited guard, iterative BFS — no
    /// recursion, so deep/wide graphs never overflow the stack). `depth_limit`
    /// of `None` means unbounded; the roots are included in `reachable`.
    pub async fn transitive_closure(
        revision: &str,
        roots: &[String],
        direction: Direction,
        depth_limit: Option<usize>,
    ) -> Result<ClosureResult, DepsError> {
        let edges = Self::all_edges(revision).await?;
        // Build the adjacency once.
        let mut adjacency: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for e in &edges {
            let (k, v) = match direction {
                Direction::Forward => (e.from.as_str(), e.to.as_str()),
                Direction::Reverse => (e.to.as_str(), e.from.as_str()),
            };
            adjacency.entry(k).or_default().push(v);
        }

        let mut visited: HashSet<String> = HashSet::new();
        let mut result = ClosureResult::default();
        // (path, depth)
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        for root in roots {
            let root = normalize_edge_path(root)?;
            if visited.insert(root.clone()) {
                queue.push_back((root, 0));
            }
        }
        while let Some((node, depth)) = queue.pop_front() {
            if let Some(limit) = depth_limit
                && depth >= limit
            {
                // Do not expand past the limit; note truncation only if this
                // node actually has out-neighbors that we are skipping.
                if adjacency.get(node.as_str()).is_some_and(|n| !n.is_empty()) {
                    result.truncated = true;
                }
                continue;
            }
            if let Some(neighbors) = adjacency.get(node.as_str()) {
                for &next in neighbors {
                    // A revisited node is NOT necessarily a cycle (DAG fan-in
                    // revisits too) — real cycle detection is a separate DFS
                    // (Codex P1). BFS still never re-enqueues → termination.
                    if visited.insert(next.to_string()) {
                        queue.push_back((next.to_string(), depth + 1));
                    }
                }
            }
        }
        // Real cycle detection: an iterative 3-color DFS over the graph rooted
        // at `roots` (a back-edge to a GRAY node is a true cycle). Structural,
        // independent of depth_limit. Iterative → stack-safe on deep graphs.
        result.cycles_detected = detect_cycle(&adjacency, roots);
        result.reachable = visited.into_iter().collect();
        result.reachable.sort();
        Ok(result)
    }

    /// A cycle-safe shortest edge-path from `from` to `to` over forward edges,
    /// explaining WHY `to` is transitively pulled in. `None` = unreachable.
    pub async fn why(
        revision: &str,
        from: &str,
        to: &str,
    ) -> Result<Option<Vec<String>>, DepsError> {
        let from = normalize_edge_path(from)?;
        let to = normalize_edge_path(to)?;
        let edges = Self::all_edges(revision).await?;
        let mut adjacency: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for e in &edges {
            adjacency
                .entry(e.from.as_str())
                .or_default()
                .push(e.to.as_str());
        }
        // BFS tracking predecessors, terminating on the visited set (cycle-safe).
        let mut visited: HashSet<String> = HashSet::from([from.clone()]);
        let mut prev: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut queue: VecDeque<String> = VecDeque::from([from.clone()]);
        while let Some(node) = queue.pop_front() {
            if node == to {
                // Reconstruct the path.
                let mut path = vec![to.clone()];
                let mut cur = to.clone();
                while let Some(p) = prev.get(&cur) {
                    path.push(p.clone());
                    cur = p.clone();
                }
                path.reverse();
                return Ok(Some(path));
            }
            if let Some(neighbors) = adjacency.get(node.as_str()) {
                for &next in neighbors {
                    if visited.insert(next.to_string()) {
                        prev.insert(next.to_string(), node.clone());
                        queue.push_back(next.to_string());
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Iterative 3-color DFS cycle detection over a direction-adjacency (Codex P1).
/// Returns true iff the graph reachable from `roots` contains a back-edge
/// (a true directed cycle) — DAG fan-in does NOT count. Stack-safe (explicit
/// frame stack, no recursion).
fn detect_cycle(adjacency: &std::collections::HashMap<&str, Vec<&str>>, roots: &[String]) -> bool {
    // 0/absent = white (unseen), 1 = gray (on the active DFS path), 2 = black.
    let mut color: std::collections::HashMap<&str, u8> = std::collections::HashMap::new();
    for root in roots {
        let Ok(root) = normalize_edge_path(root) else {
            continue;
        };
        // Resolve `root` to the adjacency's borrowed key (or skip if absent).
        let Some((&root_key, _)) = adjacency.get_key_value(root.as_str()) else {
            continue;
        };
        if color.get(root_key).copied().unwrap_or(0) != 0 {
            continue;
        }
        // Frame stack of (node, next-child index).
        let mut stack: Vec<(&str, usize)> = vec![(root_key, 0)];
        color.insert(root_key, 1);
        while let Some(&(node, idx)) = stack.last() {
            let neighbors = adjacency.get(node).map(|v| v.as_slice()).unwrap_or(&[]);
            if idx < neighbors.len() {
                // INVARIANT: the enclosing `while let Some(..) = stack.last()`
                // proved the stack is non-empty this iteration, and nothing has
                // popped it since, so `last_mut()` is always `Some`.
                stack.last_mut().expect("stack non-empty").1 = idx + 1;
                let next = neighbors[idx];
                match color.get(next).copied().unwrap_or(0) {
                    0 => {
                        color.insert(next, 1);
                        stack.push((next, 0));
                    }
                    1 => return true, // back-edge to a node on the active path
                    _ => {}           // black: fully explored, no cycle via it
                }
            } else {
                color.insert(node, 2);
                stack.pop();
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_unsafe_and_strips() {
        assert_eq!(normalize_edge_path("./a/b.txt").unwrap(), "a/b.txt");
        assert_eq!(normalize_edge_path("a\\b.txt").unwrap(), "a/b.txt");
        assert_eq!(normalize_edge_path("a/b/").unwrap(), "a/b");
        assert!(normalize_edge_path("").is_err());
        assert!(normalize_edge_path("/etc/passwd").is_err());
        assert!(normalize_edge_path("a/../b").is_err());
    }

    // The cross-machine import decoder (lore.md 3.2) must accept a well-formed
    // document and REJECT every hostile/malformed shape so a fetched note cannot
    // inject unsafe endpoints. `import_notes` warn-skips whatever this rejects.
    #[test]
    fn decode_import_doc_accepts_valid_and_rejects_hostile() {
        // Valid: normalized + backslashes unified.
        let ok = r#"{"version":1,"edges":[{"from":"a/x.txt","to":"b\\y.txt","kind":"generic"}]}"#;
        let edges = DependencyStore::decode_import_doc(ok).expect("valid doc");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "a/x.txt");
        assert_eq!(edges[0].to, "b/y.txt");

        // `..` escape rejected.
        assert!(
            DependencyStore::decode_import_doc(
                r#"{"version":1,"edges":[{"from":"a.txt","to":"../evil","kind":"generic"}]}"#
            )
            .is_err()
        );
        // Absolute path rejected.
        assert!(
            DependencyStore::decode_import_doc(
                r#"{"version":1,"edges":[{"from":"a.txt","to":"/etc/passwd","kind":"generic"}]}"#
            )
            .is_err()
        );
        // Self-edge rejected.
        assert!(
            DependencyStore::decode_import_doc(
                r#"{"version":1,"edges":[{"from":"a.txt","to":"a.txt","kind":"generic"}]}"#
            )
            .is_err()
        );
        // Unsupported version rejected.
        assert!(
            DependencyStore::decode_import_doc(
                r#"{"version":999,"edges":[{"from":"a.txt","to":"b.txt","kind":"generic"}]}"#
            )
            .is_err()
        );
        // Not JSON rejected.
        assert!(DependencyStore::decode_import_doc("not a document").is_err());
        // Over the size bound rejected.
        let huge = format!("{}{}", " ".repeat(MAX_DOC_LEN + 1), "x");
        assert!(DependencyStore::decode_import_doc(&huge).is_err());
    }

    // Pure in-memory closure test over a synthetic adjacency (no repo needed):
    // proves cycle-safety and depth limiting of the BFS core.
    fn closure(edges: &[(&str, &str)], roots: &[&str], depth: Option<usize>) -> ClosureResult {
        // Mirror transitive_closure's algorithm on an in-memory edge list.
        let mut adjacency: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for (f, t) in edges {
            adjacency.entry(*f).or_default().push(*t);
        }
        let mut visited: HashSet<String> = HashSet::new();
        let mut result = ClosureResult::default();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        for r in roots {
            if visited.insert(r.to_string()) {
                queue.push_back((r.to_string(), 0));
            }
        }
        while let Some((node, d)) = queue.pop_front() {
            if let Some(limit) = depth
                && d >= limit
            {
                if adjacency.get(node.as_str()).is_some_and(|n| !n.is_empty()) {
                    result.truncated = true;
                }
                continue;
            }
            if let Some(ns) = adjacency.get(node.as_str()) {
                for &next in ns {
                    if visited.insert(next.to_string()) {
                        queue.push_back((next.to_string(), d + 1));
                    }
                }
            }
        }
        let root_strings: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
        result.cycles_detected = super::detect_cycle(&adjacency, &root_strings);
        result.reachable = visited.into_iter().collect();
        result.reachable.sort();
        result
    }

    #[test]
    fn transitive_closure_is_cycle_safe() {
        // A -> B -> C -> A (cycle) + B -> D.
        let r = closure(
            &[("A", "B"), ("B", "C"), ("C", "A"), ("B", "D")],
            &["A"],
            None,
        );
        assert_eq!(r.reachable, vec!["A", "B", "C", "D"]);
        assert!(r.cycles_detected, "the A->B->C->A cycle is detected");
        assert!(!r.truncated);
    }

    #[test]
    fn dag_fan_in_is_not_a_false_cycle() {
        // A->B, A->C, B->D, C->D revisits D but is a DAG (no cycle).
        let r = closure(
            &[("A", "B"), ("A", "C"), ("B", "D"), ("C", "D")],
            &["A"],
            None,
        );
        assert_eq!(r.reachable, vec!["A", "B", "C", "D"]);
        assert!(!r.cycles_detected, "DAG fan-in must NOT report a cycle");
    }

    #[test]
    fn transitive_closure_respects_depth_limit() {
        // A -> B -> C -> D, depth 1 reaches only A,B and marks truncation.
        let r = closure(&[("A", "B"), ("B", "C"), ("C", "D")], &["A"], Some(1));
        assert_eq!(r.reachable, vec!["A", "B"]);
        assert!(r.truncated);
    }
}
