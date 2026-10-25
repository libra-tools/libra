//! Workspace strategy selection for sub-agent isolated workspaces
//! (CEX-S2-11).
//!
//! This module owns the **pure policy** that picks which materialization
//! strategy a sub-agent should use for its isolated workspace, given the
//! source repository's size. It deliberately carries no I/O: the actual
//! materialization (Libra/Git worktree reservation, sparse checkout, or
//! full copy) lands in a later CEX-S2-11 slice that wires
//! `orchestrator/workspace.rs` into the sub-agent dispatcher.
//!
//! The thresholds come from `docs/development/tracing/agent.md` (Step 2 workspace
//! materialization table):
//!
//! | condition                                   | strategy   |
//! |---------------------------------------------|------------|
//! | `.git` < 1 GiB **and** files < 100K         | `Worktree` |
//! | files ≥ 100K **or** `.git` ≥ 1 GiB          | `Sparse`   |
//! | preferred strategy unavailable **and** user | `FullCopy` |
//! | set `agent.allow_full_copy = true`          |            |
//!
//! [`WorkspaceStrategy::Blocked`] is not produced here — it is a *runtime*
//! decision raised when a sub-agent write escapes the materialized scope,
//! not a selection-time outcome.

use super::event::{WorkspaceMaterialized, WorkspaceStrategy};

/// `.git` size (bytes) at or above which sparse materialization is
/// preferred over a full worktree. 1 GiB, per the agent.md workspace
/// materialization table.
pub const SPARSE_REPO_SIZE_THRESHOLD_BYTES: u64 = 1 << 30;

/// Worktree file count at or above which sparse materialization is
/// preferred. 100K files, per the agent.md workspace materialization
/// table.
pub const SPARSE_FILE_COUNT_THRESHOLD: u64 = 100_000;

/// Source-repository measurements used to pick the preferred workspace
/// strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceSizing {
    /// Total `.git` directory size in bytes.
    pub repo_size_bytes: u64,
    /// Number of files in the source worktree.
    pub worktree_file_count: u64,
}

impl WorkspaceSizing {
    /// `true` when either dimension reaches its sparse threshold — i.e.
    /// `.git` ≥ 1 GiB OR file count ≥ 100K. Sparse materialization is
    /// preferred in this case so a sub-agent never has to copy a huge
    /// history or file tree.
    pub fn requires_sparse(&self) -> bool {
        self.repo_size_bytes >= SPARSE_REPO_SIZE_THRESHOLD_BYTES
            || self.worktree_file_count >= SPARSE_FILE_COUNT_THRESHOLD
    }
}

/// Pick the preferred workspace strategy from repository sizing alone.
///
/// Returns [`WorkspaceStrategy::Sparse`] when either dimension reaches
/// its threshold (`.git` ≥ [`SPARSE_REPO_SIZE_THRESHOLD_BYTES`] OR file
/// count ≥ [`SPARSE_FILE_COUNT_THRESHOLD`]); otherwise
/// [`WorkspaceStrategy::Worktree`].
///
/// Never returns [`WorkspaceStrategy::FullCopy`] (an explicit opt-in
/// fallback — see [`resolve_full_copy_fallback`]) or
/// [`WorkspaceStrategy::Blocked`] (a runtime scope-violation outcome).
pub fn select_preferred_strategy(sizing: WorkspaceSizing) -> WorkspaceStrategy {
    if sizing.requires_sparse() {
        WorkspaceStrategy::Sparse
    } else {
        WorkspaceStrategy::Worktree
    }
}

/// Resolve the fallback strategy when the preferred strategy
/// ([`WorkspaceStrategy::Worktree`] / [`WorkspaceStrategy::Sparse`])
/// could not be materialized.
///
/// Per CEX-S2-11 (2), full copy is only permitted when the user has
/// explicitly opted in via `agent.allow_full_copy = true`, and callers
/// MUST log a warning when this returns `Some(FullCopy)` (full copy is
/// for debug / small fixtures / emergency compatibility only).
///
/// Returns `None` when full copy is not permitted, signalling that the
/// caller should surface the underlying materialization error instead
/// of silently copying the whole repository.
pub fn resolve_full_copy_fallback(allow_full_copy: bool) -> Option<WorkspaceStrategy> {
    allow_full_copy.then_some(WorkspaceStrategy::FullCopy)
}

/// The preferred workspace strategy could not be materialized and
/// full-copy fallback is disabled (`agent.allow_full_copy = false`).
/// Per CEX-S2-11 (2) the caller must surface this — telling the operator
/// how to unblock — rather than silently copying the whole repository.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "could not materialize the preferred sub-agent workspace ({reason}); \
     full-copy fallback is disabled — set `agent.allow_full_copy = true` to permit \
     copying the whole repository, or resolve the underlying materialization failure"
)]
pub struct MaterializationUnavailable {
    /// Human-readable reason the preferred strategy failed to materialize.
    pub reason: String,
}

/// Resolve the final workspace strategy after attempting to materialize
/// the preferred one (CEX-S2-11 (2) fallback flow).
///
/// `attempt` is the preferred-strategy materialization result: `Ok(())`
/// when it materialized, `Err(reason)` when it failed.
///
/// - `Ok(())` → `(preferred, None)`: no fallback; the size-selected
///   strategy was used.
/// - `Err(reason)` + `allow_full_copy` → `(FullCopy, Some(reason))`: the
///   caller must now materialize via full copy and SHOULD log a warning
///   (full copy is for debug / small fixtures / emergency compatibility
///   only). The `reason` becomes the audit event's `fallback_reason`.
/// - `Err(reason)` + `!allow_full_copy` → [`MaterializationUnavailable`].
///
/// Never returns [`WorkspaceStrategy::Blocked`] (a runtime
/// scope-violation outcome, not a materialization choice).
pub fn resolve_after_preferred_attempt(
    preferred: WorkspaceStrategy,
    attempt: Result<(), String>,
    allow_full_copy: bool,
) -> Result<(WorkspaceStrategy, Option<String>), MaterializationUnavailable> {
    // INVARIANT: `preferred` is a size-selected strategy. `select_preferred_strategy`
    // only ever yields Worktree or Sparse; FullCopy is a fallback this function
    // PRODUCES (never accepts as input), and Blocked is a runtime scope-violation
    // outcome — neither is a valid `preferred`. Guard in debug builds so misuse is
    // caught in tests rather than silently returning an `Ok((FullCopy, None))` /
    // `Ok((Blocked, None))` pair that contradicts the contract.
    debug_assert!(
        matches!(
            preferred,
            WorkspaceStrategy::Worktree | WorkspaceStrategy::Sparse
        ),
        "resolve_after_preferred_attempt requires a size-selected preferred strategy \
         (Worktree | Sparse), got {preferred:?}",
    );
    match attempt {
        Ok(()) => Ok((preferred, None)),
        Err(reason) => match resolve_full_copy_fallback(allow_full_copy) {
            Some(fallback) => Ok((fallback, Some(reason))),
            None => Err(MaterializationUnavailable { reason }),
        },
    }
}

/// Build the [`WorkspaceMaterialized`] event payload (CEX-S2-11 (3))
/// emitted once per sub-agent workspace creation.
///
/// `source_repo_size` is pulled from the same [`WorkspaceSizing`] the
/// caller used to pick `strategy`, so the size reported in the audit
/// event can never drift from the size that drove the selection
/// decision. `materialized_file_count` and `elapsed_ms` are measured by
/// the materialization step and passed through verbatim.
///
/// `fallback_reason` is normalized: `None` (no fallback) maps to the
/// empty string the `WorkspaceMaterialized` schema expects, and
/// `Some(reason)` carries the human-readable explanation for using a
/// less-preferred strategy (e.g. "worktree reservation failed: `<err>`").
pub fn record_materialization(
    strategy: WorkspaceStrategy,
    sizing: WorkspaceSizing,
    materialized_file_count: u64,
    elapsed_ms: u64,
    fallback_reason: Option<String>,
) -> WorkspaceMaterialized {
    WorkspaceMaterialized {
        strategy,
        elapsed_ms,
        materialized_file_count,
        source_repo_size: sizing.repo_size_bytes,
        fallback_reason: fallback_reason.unwrap_or_default(),
    }
}

/// Build the audit-log warning a caller MUST emit when a sub-agent
/// workspace had to fall back to a full repository copy (CEX-S2-11 (2)).
///
/// Full copy is the opt-in-gated, last-resort strategy: it duplicates the
/// entire worktree rather than reusing the object store, so each fallback
/// is worth flagging in the audit log even though it was explicitly
/// permitted via `agent.allow_full_copy = true`. The structured
/// [`WorkspaceMaterialized`] event already records the same
/// `fallback_reason`; this message is the human-facing warning that sits
/// alongside it.
///
/// Returns `None` for every non-`FullCopy` strategy so callers warn only
/// on the expensive fallback and stay silent on the normal
/// [`WorkspaceStrategy::Worktree`] path. `fallback_reason` is the reason
/// the preferred strategy could not be materialized (the same value
/// threaded into [`record_materialization`]); when absent a generic
/// explanation is substituted so the warning is never empty.
pub fn full_copy_fallback_warning(
    strategy: WorkspaceStrategy,
    fallback_reason: Option<&str>,
) -> Option<String> {
    if strategy != WorkspaceStrategy::FullCopy {
        return None;
    }
    let reason = fallback_reason
        .filter(|reason| !reason.is_empty())
        .unwrap_or("preferred workspace strategy unavailable");
    Some(format!(
        "sub-agent workspace fell back to a full repository copy \
         (agent.allow_full_copy = true): {reason}; a full copy duplicates the \
         entire worktree and is intended for debug / small fixtures / emergency \
         compatibility only"
    ))
}

/// A sub-agent attempted a write outside the filesystem scope it was
/// granted in [`AgentContextPack::write_scope`]. Per CEX-S2-11 (4) the
/// surfaced error must be user-friendly and tell the operator exactly
/// how to unblock the task (widen the declared write scope), rather
/// than failing with an opaque permission error.
///
/// [`AgentContextPack::write_scope`]: super::context_pack::AgentContextPack::write_scope
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "write to '{path}' is outside the sub-agent's granted write scope; \
     add a covering path to AgentContextPack.write_scope to allow it, \
     or route the change through the parent agent"
)]
pub struct WriteScopeViolation {
    /// The offending repo-relative write path, as supplied by the caller.
    pub path: String,
}

/// Check whether a repo-relative `write_path` falls within the
/// sub-agent's declared `write_scope` (CEX-S2-11 (4) — the scope-
/// containment half of the `Blocked` workspace outcome).
///
/// Paths are compared **lexically and component-wise** after dropping
/// empty / `.` segments and resolving `..`:
///
/// - A write equal to or nested under any scope entry is in scope
///   (`src` covers `src` and `src/foo.rs`, but not the sibling
///   `srcfoo` — matching is on whole path components, never byte
///   prefixes).
/// - A scope entry that normalizes to the repo root (`.` or the empty
///   string) grants the whole tree.
/// - A `write_path` that escapes the repo root via `..`
///   (e.g. `../outside` or `src/../../etc`) is always a violation — it
///   can never be brought back in scope, so this is the primary
///   traversal defense.
/// - An **absolute** `write_path` (leading `/` or `\`) is always a
///   violation. The scope is repo-relative, so `/etc/passwd` must not
///   be silently rebased onto a relative `etc` scope entry just
///   because the leading separator drops an empty segment — that would
///   let an absolute path escape the sandbox via the relative
///   namespace.
///
/// Returns `Err(WriteScopeViolation)` (carrying the original
/// `write_path` for the error message) when no scope entry covers the
/// write; the caller maps this to [`WorkspaceStrategy::Blocked`].
pub fn check_write_in_scope(
    write_path: &str,
    write_scope: &[String],
) -> Result<(), WriteScopeViolation> {
    let violation = || WriteScopeViolation {
        path: write_path.to_string(),
    };

    // Absolute paths are never repo-relative; reject them before
    // normalization so a leading separator (which `split('/')` would
    // otherwise drop as an empty segment) can't rebase `/etc/passwd`
    // onto a relative `etc` scope entry. Covers Unix-absolute (`/...`)
    // and Windows UNC / drive-root (`\...`) forms.
    if is_absolute_path(write_path) {
        return Err(violation());
    }

    // A write that escapes the repo root can never be covered by a
    // repo-relative scope entry — reject before scope matching.
    let Some(target) = normalize_relative_path(write_path) else {
        return Err(violation());
    };

    let covered = write_scope.iter().any(|entry| {
        normalize_relative_path(entry).is_some_and(|scope| path_is_under(&target, &scope))
    });

    if covered { Ok(()) } else { Err(violation()) }
}

/// `true` when `path` is an absolute filesystem path that must never be
/// treated as repo-relative: a Unix-absolute path (leading `/`), or a
/// Windows UNC / drive-root path (leading `\`). Repo-relative scope
/// entries can only ever cover relative paths, so absolute writes are
/// rejected outright by [`check_write_in_scope`].
fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\')
}

/// Lexically normalize a repo-relative path into its component vector,
/// dropping empty / `.` segments and resolving `..`. Returns `None`
/// when the path escapes the repo root (a `..` with no parent left to
/// pop) — such paths can never be inside a repo-relative scope.
fn normalize_relative_path(path: &str) -> Option<Vec<&str>> {
    let mut components: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            other => components.push(other),
        }
    }
    Some(components)
}

/// `true` when `target` is equal to or nested under `scope`,
/// component-wise. An empty `scope` (the normalized repo root) covers
/// every target.
fn path_is_under(target: &[&str], scope: &[&str]) -> bool {
    scope.len() <= target.len() && target[..scope.len()] == *scope
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both dimensions below their thresholds → `Worktree` (the default
    /// reuse-the-object-store strategy).
    #[test]
    fn select_prefers_worktree_below_both_thresholds() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: SPARSE_REPO_SIZE_THRESHOLD_BYTES - 1,
            worktree_file_count: SPARSE_FILE_COUNT_THRESHOLD - 1,
        };
        assert!(!sizing.requires_sparse());
        assert_eq!(
            select_preferred_strategy(sizing),
            WorkspaceStrategy::Worktree
        );
    }

    /// A tiny repo (the common case) → `Worktree`.
    #[test]
    fn select_prefers_worktree_for_small_repo() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: 4 * 1024 * 1024, // 4 MiB
            worktree_file_count: 1_200,
        };
        assert_eq!(
            select_preferred_strategy(sizing),
            WorkspaceStrategy::Worktree
        );
    }

    /// `.git` size at exactly the 1 GiB threshold → `Sparse` (the
    /// boundary is inclusive: `>=`). Pins the `≥ 1 GiB` half of the
    /// agent.md rule so an off-by-one refactor to `>` trips here.
    #[test]
    fn select_switches_to_sparse_at_repo_size_threshold() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: SPARSE_REPO_SIZE_THRESHOLD_BYTES,
            worktree_file_count: 10,
        };
        assert!(sizing.requires_sparse());
        assert_eq!(select_preferred_strategy(sizing), WorkspaceStrategy::Sparse);
    }

    /// File count at exactly the 100K threshold → `Sparse` (inclusive
    /// boundary). Pins the `≥ 100K` half of the rule.
    #[test]
    fn select_switches_to_sparse_at_file_count_threshold() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: 1024,
            worktree_file_count: SPARSE_FILE_COUNT_THRESHOLD,
        };
        assert!(sizing.requires_sparse());
        assert_eq!(select_preferred_strategy(sizing), WorkspaceStrategy::Sparse);
    }

    /// Either dimension over its threshold independently forces
    /// `Sparse` — the rule is an OR, not an AND. Covers both the
    /// "huge history, few files" and "many files, small history"
    /// shapes.
    #[test]
    fn select_uses_sparse_when_either_dimension_exceeds_threshold() {
        let big_history = WorkspaceSizing {
            repo_size_bytes: 8 * SPARSE_REPO_SIZE_THRESHOLD_BYTES,
            worktree_file_count: 50,
        };
        assert_eq!(
            select_preferred_strategy(big_history),
            WorkspaceStrategy::Sparse
        );

        let many_files = WorkspaceSizing {
            repo_size_bytes: 16 * 1024 * 1024,
            worktree_file_count: 2 * SPARSE_FILE_COUNT_THRESHOLD,
        };
        assert_eq!(
            select_preferred_strategy(many_files),
            WorkspaceStrategy::Sparse
        );
    }

    /// Full copy is gated on the explicit opt-in. `true` →
    /// `Some(FullCopy)`; `false` → `None` (caller must surface the real
    /// materialization error rather than silently full-copying).
    #[test]
    fn full_copy_fallback_requires_explicit_opt_in() {
        assert_eq!(
            resolve_full_copy_fallback(true),
            Some(WorkspaceStrategy::FullCopy)
        );
        assert_eq!(resolve_full_copy_fallback(false), None);
    }

    /// A successful preferred-strategy attempt keeps that strategy with
    /// no fallback reason — for both size-selected strategies.
    #[test]
    fn resolve_after_attempt_keeps_preferred_on_success() {
        for preferred in [WorkspaceStrategy::Worktree, WorkspaceStrategy::Sparse] {
            assert_eq!(
                resolve_after_preferred_attempt(preferred, Ok(()), false),
                Ok((preferred, None)),
                "a successful {preferred:?} attempt must not fall back",
            );
            // allow_full_copy is irrelevant when the attempt succeeds.
            assert_eq!(
                resolve_after_preferred_attempt(preferred, Ok(()), true),
                Ok((preferred, None)),
            );
        }
    }

    /// A failed preferred attempt falls back to `FullCopy` — carrying the
    /// failure reason as the audit `fallback_reason` — only when the user
    /// opted in via `allow_full_copy = true`.
    #[test]
    fn resolve_after_attempt_falls_back_to_full_copy_when_opted_in() {
        let outcome = resolve_after_preferred_attempt(
            WorkspaceStrategy::Sparse,
            Err("sparse checkout unavailable: object store offline".to_string()),
            true,
        );
        assert_eq!(
            outcome,
            Ok((
                WorkspaceStrategy::FullCopy,
                Some("sparse checkout unavailable: object store offline".to_string()),
            )),
        );
    }

    /// A failed preferred attempt WITHOUT the opt-in surfaces
    /// `MaterializationUnavailable` (never silently full-copies), and the
    /// error is actionable — it names the reason and points at
    /// `agent.allow_full_copy`.
    #[test]
    fn resolve_after_attempt_errors_without_opt_in() {
        let err = resolve_after_preferred_attempt(
            WorkspaceStrategy::Worktree,
            Err("worktree reservation failed: lock held".to_string()),
            false,
        )
        .expect_err("must not silently full-copy when opt-in is off");
        assert_eq!(err.reason, "worktree reservation failed: lock held");
        let message = err.to_string();
        assert!(
            message.contains("worktree reservation failed: lock held")
                && message.contains("agent.allow_full_copy"),
            "error must name the reason and the opt-in flag: {message}",
        );
    }

    /// The `preferred` precondition is guarded in debug builds: passing
    /// a non-size-selected strategy (`FullCopy` / `Blocked`) panics the
    /// `debug_assert` rather than returning a nonsensical `Ok` pair.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "size-selected preferred strategy")]
    fn resolve_after_attempt_rejects_non_size_selected_preferred() {
        let _ = resolve_after_preferred_attempt(WorkspaceStrategy::FullCopy, Ok(()), false);
    }

    /// The fallback resolver never produces `Blocked` (a runtime
    /// scope-violation outcome) — sweep success/failure × opt-in.
    #[test]
    fn resolve_after_attempt_never_returns_blocked() {
        for preferred in [WorkspaceStrategy::Worktree, WorkspaceStrategy::Sparse] {
            for allow in [false, true] {
                for attempt in [Ok(()), Err("boom".to_string())] {
                    if let Ok((strategy, _)) =
                        resolve_after_preferred_attempt(preferred, attempt, allow)
                    {
                        assert_ne!(
                            strategy,
                            WorkspaceStrategy::Blocked,
                            "resolver must never select Blocked",
                        );
                    }
                }
            }
        }
    }

    /// `record_materialization` locks `source_repo_size` to the
    /// sizing used for selection and passes timing / file count
    /// through verbatim. The `None` fallback maps to the empty string
    /// the `WorkspaceMaterialized` schema expects.
    #[test]
    fn record_materialization_locks_source_size_and_normalizes_no_fallback() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: 256 * 1024 * 1024,
            worktree_file_count: 4_000,
        };
        let event = record_materialization(WorkspaceStrategy::Worktree, sizing, 4_000, 1_234, None);

        assert_eq!(event.strategy, WorkspaceStrategy::Worktree);
        assert_eq!(event.source_repo_size, sizing.repo_size_bytes);
        assert_eq!(event.materialized_file_count, 4_000);
        assert_eq!(event.elapsed_ms, 1_234);
        assert_eq!(
            event.fallback_reason, "",
            "no fallback must serialize as the empty string, not a sentinel",
        );
    }

    /// A `Some(reason)` fallback is carried verbatim — used when a
    /// less-preferred strategy had to be chosen (e.g. worktree
    /// reservation failed and we fell back to sparse / full copy).
    #[test]
    fn record_materialization_carries_fallback_reason() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: 2 * SPARSE_REPO_SIZE_THRESHOLD_BYTES,
            worktree_file_count: 250_000,
        };
        let event = record_materialization(
            WorkspaceStrategy::FullCopy,
            sizing,
            250_000,
            9_000,
            Some("sparse checkout unavailable: object store offline".to_string()),
        );

        assert_eq!(event.strategy, WorkspaceStrategy::FullCopy);
        assert_eq!(event.source_repo_size, sizing.repo_size_bytes);
        assert_eq!(
            event.fallback_reason,
            "sparse checkout unavailable: object store offline",
        );
    }

    /// The full-copy fallback warning fires only for `FullCopy` and is
    /// silent for every other strategy — a caller wired to this helper
    /// never warns on the normal `Worktree` / `Sparse` paths (CEX-S2-11
    /// (2): warn only on the expensive opt-in fallback).
    #[test]
    fn full_copy_warning_only_fires_for_full_copy() {
        for quiet in [
            WorkspaceStrategy::Worktree,
            WorkspaceStrategy::Sparse,
            WorkspaceStrategy::Blocked,
        ] {
            assert_eq!(
                full_copy_fallback_warning(quiet, Some("anything")),
                None,
                "{quiet:?} must not emit a full-copy fallback warning",
            );
        }
    }

    /// A `FullCopy` warning carries the supplied reason and names the
    /// opt-in flag so the audit log says why a full copy happened and how
    /// it was permitted.
    #[test]
    fn full_copy_warning_carries_reason_and_names_opt_in() {
        let warning = full_copy_fallback_warning(
            WorkspaceStrategy::FullCopy,
            Some("sparse checkout unavailable: object store offline"),
        )
        .expect("FullCopy must produce a warning");
        assert!(
            warning.contains("sparse checkout unavailable: object store offline"),
            "warning must carry the fallback reason: {warning}",
        );
        assert!(
            warning.contains("agent.allow_full_copy = true"),
            "warning must name the opt-in flag: {warning}",
        );
    }

    /// An absent or empty reason still yields a non-empty warning — the
    /// audit log never gets a blank line that hides why a full copy ran.
    #[test]
    fn full_copy_warning_substitutes_generic_reason_when_missing() {
        for missing in [None, Some("")] {
            let warning = full_copy_fallback_warning(WorkspaceStrategy::FullCopy, missing)
                .expect("FullCopy must produce a warning");
            assert!(
                warning.contains("preferred workspace strategy unavailable"),
                "missing reason must fall back to a generic explanation: {warning}",
            );
            assert!(
                warning.contains("agent.allow_full_copy = true"),
                "warning must still name the opt-in flag: {warning}",
            );
        }
    }

    /// `record_materialization` payloads round-trip through serde so
    /// they can be appended to `agents/{run_id}.jsonl` and read back by
    /// projection / audit consumers. Pins the wire shape against the
    /// `WorkspaceMaterialized` schema (`deny_unknown_fields`).
    #[test]
    fn record_materialization_round_trips_through_serde() {
        let sizing = WorkspaceSizing {
            repo_size_bytes: 12 * 1024 * 1024,
            worktree_file_count: 900,
        };
        let event = record_materialization(WorkspaceStrategy::Sparse, sizing, 120, 42, None);
        let json = serde_json::to_string(&event).expect("serialize WorkspaceMaterialized");
        let back: WorkspaceMaterialized =
            serde_json::from_str(&json).expect("deserialize WorkspaceMaterialized");
        assert_eq!(back, event);
    }

    /// A write equal to or nested under a declared scope entry is in
    /// scope; matching is component-wise so `src` covers `src` and
    /// `src/foo.rs` but never the byte-prefix sibling `srcfoo`.
    #[test]
    fn write_in_scope_accepts_paths_under_declared_scope() {
        let scope = vec!["src".to_string(), "docs/commands".to_string()];

        for ok in [
            "src",
            "src/foo.rs",
            "src/internal/ai/mod.rs",
            "docs/commands",
            "docs/commands/clean.md",
            "./src/foo.rs",      // leading "./" normalizes away
            "src/./bar.rs",      // interior "." normalizes away
            "src/sub/../baz.rs", // interior ".." stays under src
        ] {
            assert!(
                check_write_in_scope(ok, &scope).is_ok(),
                "expected `{ok}` to be in scope",
            );
        }
    }

    /// Writes outside every scope entry are violations, including the
    /// byte-prefix lookalike `srcfoo` (must not match `src`).
    #[test]
    fn write_in_scope_rejects_out_of_scope_paths() {
        let scope = vec!["src".to_string()];

        for bad in [
            "lib/x.rs",
            "srcfoo/x.rs",
            "srcfoo",
            "Cargo.toml",
            "tests/t.rs",
        ] {
            let err = check_write_in_scope(bad, &scope)
                .expect_err("expected out-of-scope write to be rejected");
            assert_eq!(err.path, bad, "violation must echo the offending path");
        }
    }

    /// `..` traversal that escapes the repo root is always a violation
    /// and can never be brought back in scope — this is the primary
    /// path-traversal defense. Even a permissive whole-repo scope (".")
    /// must not let a write climb above the root.
    #[test]
    fn write_in_scope_rejects_root_escaping_traversal() {
        let permissive = vec![".".to_string()];
        for escaping in [
            "../outside",
            "../../etc/passwd",
            "src/../../etc/passwd",
            "a/b/../../../c",
        ] {
            assert!(
                check_write_in_scope(escaping, &permissive).is_err(),
                "root-escaping path `{escaping}` must be rejected even under a '.' scope",
            );
        }
    }

    /// Absolute paths must never be treated as repo-relative. A
    /// Unix-absolute `/etc/passwd` must NOT be rebased onto a relative
    /// `etc` scope entry just because `split('/')` drops the leading
    /// empty segment — that was a real escape-via-relative-namespace
    /// bypass. Windows UNC / drive-root (`\...`) forms are rejected
    /// too. This holds even under a permissive `.` scope.
    #[test]
    fn write_in_scope_rejects_absolute_paths() {
        // A scope entry that, post-normalization, equals the tail of an
        // absolute path — the exact shape that bypassed the check before
        // the absolute-path guard.
        let etc_scope = vec!["etc".to_string()];
        for absolute in ["/etc/passwd", "/etc", "//etc/passwd"] {
            let err = check_write_in_scope(absolute, &etc_scope).expect_err(
                "absolute path must be rejected, not rebased onto a relative scope entry",
            );
            assert_eq!(err.path, absolute);
        }

        // Windows-style absolute / UNC forms.
        for absolute in ["\\etc", "\\\\server\\share\\x"] {
            assert!(
                check_write_in_scope(absolute, &etc_scope).is_err(),
                "Windows absolute/UNC path `{absolute}` must be rejected",
            );
        }

        // Even a whole-repo `.` scope must not admit an absolute path.
        let permissive = vec![".".to_string()];
        for absolute in ["/etc/passwd", "/anything", "\\x"] {
            assert!(
                check_write_in_scope(absolute, &permissive).is_err(),
                "absolute path `{absolute}` must be rejected even under a '.' scope",
            );
        }
    }

    /// `..` that resolves to a sibling (without escaping root) is
    /// rejected when the sibling is outside scope: `src/../lib/x`
    /// normalizes to `lib/x`, which is not under `src`.
    #[test]
    fn write_in_scope_rejects_sibling_via_dotdot() {
        let scope = vec!["src".to_string()];
        let err = check_write_in_scope("src/../lib/x.rs", &scope)
            .expect_err("dotdot into a sibling must be rejected");
        assert_eq!(err.path, "src/../lib/x.rs");
    }

    /// An empty `write_scope` grants nothing — every write is blocked.
    /// Pins the fail-closed default so a sub-agent with no declared
    /// write scope can't write anywhere.
    #[test]
    fn write_in_scope_empty_scope_blocks_everything() {
        for any in ["src/foo.rs", "README.md", "."] {
            assert!(
                check_write_in_scope(any, &[]).is_err(),
                "empty write_scope must block `{any}`",
            );
        }
    }

    /// A scope entry that normalizes to the repo root (`.` or the empty
    /// string) grants the whole in-repo tree (but still not
    /// root-escaping paths — covered separately).
    #[test]
    fn write_in_scope_root_scope_entry_grants_whole_tree() {
        for root_entry in [".", "", "./"] {
            let scope = vec![root_entry.to_string()];
            for target in ["src/foo.rs", "Cargo.toml", "a/b/c/d.rs"] {
                assert!(
                    check_write_in_scope(target, &scope).is_ok(),
                    "root scope `{root_entry}` must cover `{target}`",
                );
            }
        }
    }

    /// The violation's `Display` is user-friendly and actionable per
    /// CEX-S2-11 (4): it names the offending path and tells the
    /// operator to extend `AgentContextPack.write_scope`. Pin the
    /// wording so a future refactor can't regress it into an opaque
    /// permission error.
    #[test]
    fn write_scope_violation_message_is_actionable() {
        let err = check_write_in_scope("etc/secret", &["src".to_string()])
            .expect_err("out-of-scope write must error");
        let message = err.to_string();
        assert!(
            message.contains("etc/secret"),
            "message must name the path: {message}"
        );
        assert!(
            message.contains("AgentContextPack.write_scope"),
            "message must point at the write scope to extend: {message}",
        );
    }

    /// The selection function never emits `FullCopy` or `Blocked` —
    /// those are an opt-in fallback and a runtime scope-violation
    /// outcome respectively, not size-driven choices. Sweep a range of
    /// sizings and assert the output is always one of the two
    /// size-selected variants.
    #[test]
    fn select_never_emits_full_copy_or_blocked() {
        for repo_size_bytes in [
            0,
            1024,
            SPARSE_REPO_SIZE_THRESHOLD_BYTES - 1,
            SPARSE_REPO_SIZE_THRESHOLD_BYTES,
            64 * SPARSE_REPO_SIZE_THRESHOLD_BYTES,
        ] {
            for worktree_file_count in [
                0,
                10,
                SPARSE_FILE_COUNT_THRESHOLD - 1,
                SPARSE_FILE_COUNT_THRESHOLD,
                5 * SPARSE_FILE_COUNT_THRESHOLD,
            ] {
                let strategy = select_preferred_strategy(WorkspaceSizing {
                    repo_size_bytes,
                    worktree_file_count,
                });
                assert!(
                    matches!(
                        strategy,
                        WorkspaceStrategy::Worktree | WorkspaceStrategy::Sparse
                    ),
                    "select_preferred_strategy returned {strategy:?} for \
                     repo_size_bytes={repo_size_bytes}, \
                     worktree_file_count={worktree_file_count}",
                );
            }
        }
    }
}
