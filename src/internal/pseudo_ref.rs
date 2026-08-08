//! `WorktreePseudoRefs` — the ONE place that answers "what does `MERGE_HEAD`
//! mean in this worktree" (plan-20260714 §C.5, W2).
//!
//! Git records an in-progress operation's commits as files in `GIT_DIR`
//! (`MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `REBASE_HEAD`,
//! `ORIG_HEAD`). Libra keeps that state in scoped database rows and
//! per-worktree sidecars instead, and §C.5 is explicit that materializing the
//! Git files is a NON-goal for this wave. What is required is that every
//! consumer — `status`, the `--continue`/`--skip`/`--abort` paths, and any
//! future `rev-parse` support — gets the SAME answer for the SAME worktree,
//! derived from the state that already exists.
//!
//! So this module owns no state. Each pseudo-ref is projected, on demand, from
//! the single source of truth that already holds it:
//!
//! | pseudo-ref         | source                                              |
//! |--------------------|-----------------------------------------------------|
//! | `ORIG_HEAD`        | `sequence_state.head_orig` / `rebase_state.orig_head` / `bisect_state.orig_head` / the merge and revert sidecars |
//! | `MERGE_HEAD`       | `merge-state.json` `target` (this worktree's gitdir) |
//! | `CHERRY_PICK_HEAD` | `sequence_state.current_oid` when the sequence is a cherry-pick |
//! |                    | (projected whenever the sequence row EXISTS — i.e. on any stop, |
//! |                    | conflict or hard error — matching Git, which keeps            |
//! |                    | `CHERRY_PICK_HEAD` while a pick is in progress for any reason; |
//! |                    | §C.5's "当前冲突 commit" is read as "the commit the stopped     |
//! |                    | pick was applying", which both stop reasons satisfy)           |
//! | `REVERT_HEAD`      | `revert-state.json` `reverted_commit`, or `sequence_state.current_oid` for a revert sequence |
//! | `REBASE_HEAD`      | `rebase_state.stopped_sha`                          |
//! | `FETCH_HEAD`       | `<local gitdir>/FETCH_HEAD` (the one pseudo-ref that IS a file, because it holds many rows) |
//!
//! Duplicating any of these into a second store is exactly what §C.5 forbids:
//! two worktrees would then have two ways to disagree about the same sequence.
//!
//! §C.5 also fixes two deliberate differences from Git, both declared in
//! `COMPATIBILITY.md` rather than left implicit:
//!
//! 1. `ORIG_HEAD` here means "the commit this worktree's sequence/bisect
//!    started from". Git also writes it from `reset`, `am` and others.
//! 2. None of these names is resolvable by `rev-parse`. `rev-parse` recognises
//!    `HEAD`/`@` only, and this wave does not widen it — a script that asks for
//!    `rev-parse MERGE_HEAD` must get a clear refusal, not a silent miss.

use crate::internal::worktree_scope::WorktreeScope;

/// The pseudo-ref names §C.5 defines. Ordered as the table above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoRef {
    OrigHead,
    MergeHead,
    CherryPickHead,
    RevertHead,
    RebaseHead,
    FetchHead,
}

impl PseudoRef {
    /// The Git-facing spelling, which is also what `COMPATIBILITY.md` names.
    pub fn name(self) -> &'static str {
        match self {
            Self::OrigHead => "ORIG_HEAD",
            Self::MergeHead => "MERGE_HEAD",
            Self::CherryPickHead => "CHERRY_PICK_HEAD",
            Self::RevertHead => "REVERT_HEAD",
            Self::RebaseHead => "REBASE_HEAD",
            Self::FetchHead => "FETCH_HEAD",
        }
    }

    /// Every name this service answers for — the contract `rev-parse`'s
    /// refusal and the compatibility row are both derived from.
    pub const ALL: [PseudoRef; 6] = [
        PseudoRef::OrigHead,
        PseudoRef::MergeHead,
        PseudoRef::CherryPickHead,
        PseudoRef::RevertHead,
        PseudoRef::RebaseHead,
        PseudoRef::FetchHead,
    ];

    /// Resolve a Git-spelled name, case-sensitively as Git does.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.name() == name)
    }
}

/// One resolved pseudo-ref: the name, the commit it points at, and WHERE that
/// came from — the provenance is part of the answer, because "no merge in
/// progress" and "a merge whose state could not be read" are different facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPseudoRef {
    pub name: &'static str,
    pub oid: String,
    /// The store the value was projected from, for diagnostics.
    pub source: &'static str,
}

/// Read-side service over one worktree's pseudo-refs (§C.5).
///
/// Every method takes the scope EXPLICITLY. §C.4.2: the scope is resolved once
/// per request and passed down; a projection that re-read the cwd could answer
/// worktree A's `MERGE_HEAD` for a command acting on worktree B.
pub struct WorktreePseudoRefs {
    scope: WorktreeScope,
    /// The gitdir of `scope`, resolved ONCE when the service is bound.
    ///
    /// The sidecars and `FETCH_HEAD` live in files, and the scope alone does
    /// not locate them: reading them through the REQUEST pin would answer with
    /// the invoking worktree's merge/revert state while the database half
    /// answered for `scope` — the two halves disagreeing is precisely what
    /// §C.4.2/§C.5 forbid. `None` when this scope has no resolvable gitdir,
    /// which the file-backed projections report rather than paper over.
    gitdir: Result<std::path::PathBuf, String>,
}

impl WorktreePseudoRefs {
    /// Bind the service to an explicitly resolved scope.
    pub fn new(scope: WorktreeScope) -> Self {
        let gitdir = crate::command::worktree::local_gitdir_for_scope(&scope);
        Self { scope, gitdir }
    }

    /// The gitdir of the bound scope, or the reason it could not be resolved.
    fn gitdir(&self) -> Result<&std::path::Path, String> {
        match &self.gitdir {
            Ok(path) => Ok(path.as_path()),
            Err(error) => Err(format!(
                "cannot locate the worktree this pseudo-ref belongs to: {error}"
            )),
        }
    }

    /// Bind to the scope THIS INVOCATION is acting on.
    ///
    /// The gitdir comes from the REQUEST PIN, not from
    /// `local_gitdir_for_scope`: that helper re-resolves the common storage
    /// and registry from the ambient cwd, so an in-process cwd move could
    /// pair repository A's database projections with repository B's
    /// merge/revert/`FETCH_HEAD` files — exactly the split fact §C.4.2
    /// forbids. The pin resolved all of its paths at request entry, so both
    /// halves come from one worktree or the request never pinned (in which
    /// case the ambient fallback is at least self-consistent with the
    /// scope's own ambient resolution).
    pub fn for_request() -> Self {
        let scope = WorktreeScope::for_request();
        let gitdir =
            crate::utils::util::request_worktree_gitdir().map_err(|error| error.to_string());
        Self { scope, gitdir }
    }

    /// The scope this service answers for.
    pub fn scope(&self) -> &WorktreeScope {
        &self.scope
    }

    /// Resolve one pseudo-ref, or `None` when nothing in this worktree defines
    /// it (no sequence in progress, no fetch recorded).
    ///
    /// A store that exists but cannot be READ is an error, never `None`: a
    /// `--continue` that treats an unreadable sequence as "nothing in
    /// progress" would start a second one over the first.
    pub async fn resolve(&self, which: PseudoRef) -> Result<Option<ResolvedPseudoRef>, String> {
        match which {
            PseudoRef::OrigHead => self.orig_head().await,
            PseudoRef::MergeHead => Ok(self.merge_head()?),
            PseudoRef::CherryPickHead => self.sequence_current(SequenceFace::CherryPick).await,
            PseudoRef::RevertHead => self.revert_head().await,
            PseudoRef::RebaseHead => self.rebase_stopped().await,
            PseudoRef::FetchHead => Ok(self.fetch_head()?),
        }
    }

    /// Every pseudo-ref this worktree currently defines, in [`PseudoRef::ALL`]
    /// order. What `status` and a diagnostic surface want: one consistent
    /// snapshot rather than six independent lookups a cwd change could split.
    pub async fn resolve_all(&self) -> Result<Vec<ResolvedPseudoRef>, String> {
        let mut resolved = Vec::new();
        for which in PseudoRef::ALL {
            if let Some(value) = self.resolve(which).await? {
                resolved.push(value);
            }
        }
        Ok(resolved)
    }

    /// `ORIG_HEAD` — the commit this worktree's sequence/bisect started from.
    ///
    /// The sources are checked in the order a worktree can hold them; at most
    /// one sequence is active per scope (that is what the sequencer mutex
    /// enforces), so the first hit IS the answer.
    async fn orig_head(&self) -> Result<Option<ResolvedPseudoRef>, String> {
        // EVERY source is read, not the first that answers. At most one
        // operation may be active per scope — that is what the sequencer mutex
        // enforces — so two sources answering is not a priority question, it
        // is evidence that this worktree holds state from an interrupted or
        // mixed-version process. Choosing one silently would hand a caller a
        // rebase's ORIG_HEAD while a merge sidecar sits beside it; reporting
        // it is the only answer that cannot mislead a `--continue`.
        let mut found: Vec<ResolvedPseudoRef> = Vec::new();
        if let Some(state) = crate::internal::sequencer::load_for_scope(&self.scope).await? {
            found.push(ResolvedPseudoRef {
                name: PseudoRef::OrigHead.name(),
                oid: state.head_orig,
                source: "sequence_state",
            });
        }
        if let Some(oid) = crate::command::rebase::orig_head_for_scope(&self.scope).await? {
            found.push(ResolvedPseudoRef {
                name: PseudoRef::OrigHead.name(),
                oid,
                source: "rebase_state",
            });
        }
        if let Some(oid) = crate::command::bisect::orig_head_for_scope(&self.scope).await? {
            found.push(ResolvedPseudoRef {
                name: PseudoRef::OrigHead.name(),
                oid,
                source: "bisect_state",
            });
        }
        if let Some(state) = crate::command::merge::merge_state_for_pseudo_refs(self.gitdir()?)? {
            found.push(ResolvedPseudoRef {
                name: PseudoRef::OrigHead.name(),
                oid: state.orig_head,
                source: "merge-state.json",
            });
        }
        if let Some((orig_head, _)) =
            crate::command::revert::revert_state_for_pseudo_refs(self.gitdir()?)?
        {
            found.push(ResolvedPseudoRef {
                name: PseudoRef::OrigHead.name(),
                oid: orig_head,
                source: "revert-state.json",
            });
        }
        one_of(PseudoRef::OrigHead, found, self.scope.storage_key())
    }

    /// `MERGE_HEAD` — the commit being merged IN, defined only while a merge
    /// is in progress in this worktree.
    fn merge_head(&self) -> Result<Option<ResolvedPseudoRef>, String> {
        Ok(
            crate::command::merge::merge_state_for_pseudo_refs(self.gitdir()?)?.map(|state| {
                ResolvedPseudoRef {
                    name: PseudoRef::MergeHead.name(),
                    oid: state.target,
                    source: "merge-state.json",
                }
            }),
        )
    }

    /// `REVERT_HEAD` — the commit being reverted. A conflicted `revert` writes
    /// the sidecar; a multi-commit `revert A..B` runs as a SEQUENCE, so both
    /// stores can define it and both must give the same worktree's answer.
    async fn revert_head(&self) -> Result<Option<ResolvedPseudoRef>, String> {
        let mut found: Vec<ResolvedPseudoRef> = Vec::new();
        if let Some(resolved) = self.sequence_current(SequenceFace::Revert).await? {
            found.push(resolved);
        }
        if let Some((_, reverted)) =
            crate::command::revert::revert_state_for_pseudo_refs(self.gitdir()?)?
        {
            found.push(ResolvedPseudoRef {
                name: PseudoRef::RevertHead.name(),
                oid: reverted,
                source: "revert-state.json",
            });
        }
        one_of(PseudoRef::RevertHead, found, self.scope.storage_key())
    }

    /// The conflicted commit of a cherry-pick / revert SEQUENCE.
    async fn sequence_current(
        &self,
        face: SequenceFace,
    ) -> Result<Option<ResolvedPseudoRef>, String> {
        let Some(state) = crate::internal::sequencer::load_for_scope(&self.scope).await? else {
            return Ok(None);
        };
        let matches = match face {
            SequenceFace::CherryPick => {
                state.kind == crate::internal::sequencer::SequenceKind::CherryPick
            }
            SequenceFace::Revert => state.kind == crate::internal::sequencer::SequenceKind::Revert,
        };
        if !matches {
            return Ok(None);
        }
        // §C.5: `CHERRY_PICK_HEAD`/`REVERT_HEAD` name the commit of a
        // CONFLICT stop. The row alone cannot prove which stop this is —
        // `resume_picks` persists position before every attempt, so a hard
        // error mid-resume leaves the same row. The writer stamps a durable
        // conflict-phase flag into the payload on every conflict stop and
        // strips it on resume; a row without the flag (including rows from
        // older binaries) does not define the pseudo-ref. `ORIG_HEAD` stays
        // defined for ANY sequence — its contract is "where the sequence
        // started", not "why it stopped".
        let stopped_on_conflict = serde_json::from_str::<serde_json::Value>(&state.payload)
            .ok()
            .and_then(|opts| opts.get("stopped_on_conflict").cloned())
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false);
        if !stopped_on_conflict {
            return Ok(None);
        }
        Ok(Some(ResolvedPseudoRef {
            name: face.pseudo_ref().name(),
            oid: state.current_oid,
            source: "sequence_state",
        }))
    }

    /// `REBASE_HEAD` — the commit a stopped rebase is sitting on, if any.
    async fn rebase_stopped(&self) -> Result<Option<ResolvedPseudoRef>, String> {
        Ok(crate::command::rebase::stopped_sha_for_scope(&self.scope)
            .await?
            .map(|oid| ResolvedPseudoRef {
                name: PseudoRef::RebaseHead.name(),
                oid,
                source: "rebase_state",
            }))
    }

    /// `FETCH_HEAD` — the last fetch's candidate in THIS worktree.
    ///
    /// The only pseudo-ref backed by a file, because it records many rows
    /// (§C.5, which defines it as "advertised heads/merge candidates").
    ///
    /// Git resolves the name to the first line whose not-for-merge field is
    /// empty. Libra's fetch marks EVERY line `not-for-merge` on purpose —
    /// `fetch` never designates a merge target here, `pull` does
    /// (`command/fetch.rs::format_fetch_head`) — so that rule alone would make
    /// this projection answer `None` for every file this repository can
    /// produce. It therefore prefers a mergeable line when one exists (a
    /// future writer, or a file written by another tool) and otherwise falls
    /// back to the first ADVERTISED head, which is the other half of §C.5's
    /// definition and the only useful answer for Libra's own files.
    fn fetch_head(&self) -> Result<Option<ResolvedPseudoRef>, String> {
        let path = self.gitdir()?.join("FETCH_HEAD");
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!("cannot read '{}': {error}", path.display()));
            }
        };
        let mut first_advertised: Option<String> = None;
        for line in contents.lines() {
            let mut fields = line.split('\t');
            let Some(oid) = fields.next().map(str::trim).filter(|oid| !oid.is_empty()) else {
                continue;
            };
            // `<oid>\t<not-for-merge>\t<description>`: an empty second field is
            // the merge candidate, exactly as Git spells it.
            let mergeable = fields.next().is_none_or(|flag| flag.trim().is_empty());
            if mergeable {
                return Ok(Some(ResolvedPseudoRef {
                    name: PseudoRef::FetchHead.name(),
                    oid: oid.to_string(),
                    source: "FETCH_HEAD",
                }));
            }
            first_advertised.get_or_insert_with(|| oid.to_string());
        }
        Ok(first_advertised.map(|oid| ResolvedPseudoRef {
            name: PseudoRef::FetchHead.name(),
            oid,
            source: "FETCH_HEAD (advertised head; libra fetch designates no merge target)",
        }))
    }
}

/// Exactly one store may define a pseudo-ref in one scope.
///
/// Two agreeing sources are still ONE answer — a `revert` sequence writes both
/// the row and the sidecar with the same commit, and refusing that would break
/// an ordinary conflicted revert. Two DISAGREEING sources are a real
/// inconsistency: the scope holds state from more than one operation, which
/// the mutex forbids, so it can only come from an interrupted or mixed-version
/// process. Report it with both sources named instead of picking one.
fn one_of(
    which: PseudoRef,
    mut found: Vec<ResolvedPseudoRef>,
    scope_key: &str,
) -> Result<Option<ResolvedPseudoRef>, String> {
    found.dedup_by(|a, b| a.oid == b.oid);
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => {
            let detail = found
                .iter()
                .map(|entry| format!("{} says {}", entry.source, entry.oid))
                .collect::<Vec<_>>()
                .join("; ");
            let scope = if scope_key.is_empty() {
                "the main worktree".to_string()
            } else {
                format!("worktree '{scope_key}'")
            };
            Err(format!(
                "{} is defined inconsistently in {scope}: {detail}. Only one operation may be \
                 in progress per worktree, so this is leftover state from an interrupted or \
                 older process — inspect it with `libra status` and finish or abort the \
                 operation that owns it",
                which.name()
            ))
        }
    }
}

/// Which sequence face a projection is asking about.
#[derive(Debug, Clone, Copy)]
enum SequenceFace {
    CherryPick,
    Revert,
}

impl SequenceFace {
    fn pseudo_ref(self) -> PseudoRef {
        match self {
            Self::CherryPick => PseudoRef::CherryPickHead,
            Self::Revert => PseudoRef::RevertHead,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Register a linked worktree with its own gitdir, so a scope naming it
    /// resolves to real files (which every file-backed projection needs).
    fn register_linked_worktree(repo: &std::path::Path, id: &str) -> std::path::PathBuf {
        let linked_path = repo.join(format!("wt-{id}"));
        let gitdir = linked_path.join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&gitdir).expect("linked gitdir");
        std::fs::write(gitdir.join("worktree_id"), format!("{id}\n")).expect("id");
        std::fs::write(
            gitdir.join("commondir"),
            repo.join(crate::utils::util::ROOT_DIR)
                .to_string_lossy()
                .as_bytes(),
        )
        .expect("commondir");
        std::fs::write(
            repo.join(crate::utils::util::ROOT_DIR)
                .join("worktrees.json"),
            serde_json::json!({
                "schema_version": 3,
                "linked_history": "existed",
                "epoch_counter": 1,
                "entries": [
                    { "path": repo.to_string_lossy(), "is_main": true, "locked": false },
                    {
                        "path": linked_path.to_string_lossy(),
                        "is_main": false,
                        "locked": false,
                        "worktree_id": id,
                        "state": "active",
                        "epoch": 1
                    }
                ]
            })
            .to_string(),
        )
        .expect("registry");
        linked_path
    }

    /// §C.5 / W2 acceptance: two worktrees get their OWN answers.
    ///
    /// The whole point of the service is that a `CHERRY_PICK_HEAD` asked for
    /// while acting on worktree A is A's conflicted commit — even when
    /// worktree B has its own sequence in the same repository. Reading the
    /// process cwd instead of the passed scope is exactly the bug this pins.
    #[tokio::test]
    #[serial_test::serial]
    async fn each_scope_projects_its_own_sequence() {
        use crate::internal::sequencer::{SequenceKind, SequenceState};

        let repo = tempfile::tempdir().expect("repo");
        let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let main = WorktreeScope::Main;
        let linked = WorktreeScope::Linked("wt-projection".to_string());
        register_linked_worktree(repo.path(), "wt-projection");

        // Main holds a cherry-pick; the linked worktree holds a revert.
        let _main_pin = WorktreeScope::pin_scope_for_test(main.clone(), repo.path().to_path_buf());
        crate::internal::sequencer::save(&SequenceState {
            kind: SequenceKind::CherryPick,
            head_name: "main".to_string(),
            head_orig: "1111111111111111111111111111111111111111".to_string(),
            current_oid: "2222222222222222222222222222222222222222".to_string(),
            todo: Vec::new(),
            payload: r#"{"stopped_on_conflict":true}"#.to_string(),
        })
        .await
        .expect("save main's sequence");
        drop(_main_pin);

        let _linked_pin =
            WorktreeScope::pin_scope_for_test(linked.clone(), repo.path().to_path_buf());
        crate::internal::sequencer::save(&SequenceState {
            kind: SequenceKind::Revert,
            head_name: "topic".to_string(),
            head_orig: "3333333333333333333333333333333333333333".to_string(),
            current_oid: "4444444444444444444444444444444444444444".to_string(),
            todo: Vec::new(),
            payload: r#"{"stopped_on_conflict":true}"#.to_string(),
        })
        .await
        .expect("save the linked worktree's sequence");

        // The pin is the LINKED scope for the rest of this test, so any
        // projection that ignored its argument would answer with the revert.
        let main_refs = WorktreePseudoRefs::new(main.clone());
        let cherry = main_refs
            .resolve(PseudoRef::CherryPickHead)
            .await
            .expect("main's cherry-pick projection")
            .expect("main has a cherry-pick in progress");
        assert_eq!(
            cherry.oid, "2222222222222222222222222222222222222222",
            "main's CHERRY_PICK_HEAD is main's conflicted commit"
        );
        assert!(
            main_refs
                .resolve(PseudoRef::RevertHead)
                .await
                .expect("main's revert projection")
                .is_none(),
            "and main has no REVERT_HEAD — that is the other worktree's sequence"
        );
        assert_eq!(
            main_refs
                .resolve(PseudoRef::OrigHead)
                .await
                .expect("main's ORIG_HEAD")
                .expect("main's sequence defines one")
                .oid,
            "1111111111111111111111111111111111111111"
        );

        let linked_refs = WorktreePseudoRefs::new(linked);
        let revert = linked_refs
            .resolve(PseudoRef::RevertHead)
            .await
            .expect("the linked worktree's revert projection")
            .expect("it has a revert in progress");
        assert_eq!(
            revert.oid, "4444444444444444444444444444444444444444",
            "the linked worktree's REVERT_HEAD is ITS conflicted commit"
        );
        assert!(
            linked_refs
                .resolve(PseudoRef::CherryPickHead)
                .await
                .expect("the linked worktree's cherry-pick projection")
                .is_none(),
            "and it does not see main's cherry-pick"
        );
    }

    /// §C.5: a sequence row WITHOUT the conflict-phase flag — a hard-error
    /// stop, or a row written by an older binary — defines NO
    /// `CHERRY_PICK_HEAD`, while `ORIG_HEAD` (a "where did it start" fact)
    /// stays defined.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_non_conflict_stop_defines_no_cherry_pick_head() {
        use crate::internal::sequencer::{SequenceKind, SequenceState};

        let repo = tempfile::tempdir().expect("repo");
        let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let _pin =
            WorktreeScope::pin_scope_for_test(WorktreeScope::Main, repo.path().to_path_buf());
        crate::internal::sequencer::save(&SequenceState {
            kind: SequenceKind::CherryPick,
            head_name: "main".to_string(),
            head_orig: "1111111111111111111111111111111111111111".to_string(),
            current_oid: "2222222222222222222222222222222222222222".to_string(),
            todo: Vec::new(),
            // The pre-attempt save `resume_picks` writes: position known,
            // conflict NOT proven.
            payload: r#"{"stopped_on_conflict":false}"#.to_string(),
        })
        .await
        .expect("save the hard-error-stopped sequence");

        let service = WorktreePseudoRefs::new(WorktreeScope::Main);
        assert!(
            service
                .resolve(PseudoRef::CherryPickHead)
                .await
                .expect("projection")
                .is_none(),
            "a stop that is not a proven conflict defines no CHERRY_PICK_HEAD"
        );
        assert_eq!(
            service
                .resolve(PseudoRef::OrigHead)
                .await
                .expect("ORIG_HEAD projection")
                .expect("any sequence defines ORIG_HEAD")
                .oid,
            "1111111111111111111111111111111111111111",
            "ORIG_HEAD is about where the sequence STARTED, not why it stopped"
        );
    }

    /// §C.4.2: `for_request` binds BOTH halves — scope AND gitdir — to the
    /// request pin, not the ambient cwd.
    ///
    /// Repository A is pinned; the process cwd then moves to repository B.
    /// The service must keep answering with A's file-backed state (the merge
    /// sidecar planted in A's gitdir). A constructor that re-resolved the
    /// gitdir from the cwd would pair A's scope with B's files — the §C.4.2
    /// split-fact this pins against. Reverting the pinned constructor makes
    /// this fail.
    #[tokio::test]
    #[serial_test::serial]
    async fn for_request_keeps_the_pinned_worktree_after_a_cwd_move() {
        let repo_a = tempfile::tempdir().expect("repo A");
        let repo_b = tempfile::tempdir().expect("repo B");
        {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo_a.path());
            crate::utils::test::setup_with_new_libra_in(repo_a.path()).await;
        }
        {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo_b.path());
            crate::utils::test::setup_with_new_libra_in(repo_b.path()).await;
        }

        // A's merge sidecar; B gets a DIFFERENT one so a mis-bound gitdir
        // produces a WRONG answer rather than a missing one.
        for (repo, target) in [
            (repo_a.path(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            (repo_b.path(), "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        ] {
            std::fs::write(
                repo.join(crate::utils::util::ROOT_DIR)
                    .join("merge-state.json"),
                serde_json::json!({
                    "head_name": "main",
                    "orig_head": "cccccccccccccccccccccccccccccccccccccccc",
                    "target": target,
                    "target_ref": "refs/heads/incoming",
                    "conflicted_paths": ["clash.txt"],
                })
                .to_string(),
            )
            .expect("merge sidecar");
        }

        // Pin repository A, then MOVE the cwd to repository B.
        let _pin =
            WorktreeScope::pin_scope_for_test(WorktreeScope::Main, repo_a.path().to_path_buf());
        let _cd = crate::utils::test::ChangeDirGuard::new(repo_b.path());

        let service = WorktreePseudoRefs::for_request();
        let merge_head = service
            .resolve(PseudoRef::MergeHead)
            .await
            .expect("the pinned worktree's merge projection")
            .expect("repository A has a merge in progress");
        assert_eq!(
            merge_head.oid, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "for_request answers with the PINNED repository's sidecar, not the \
             ambient cwd's"
        );
    }

    /// §C.12 named regression `linked_pseudo_refs_resolve_per_worktree`:
    /// `resolve_all` across two scopes — the full-surface composition of the
    /// per-projection tests above.
    ///
    /// Main holds a cherry-pick while the linked worktree holds a merge
    /// sidecar; each scope's `resolve_all` names EXACTLY its own pseudo-refs
    /// and never the other's. This is the isolation the W2 acceptance line
    /// demands of `WorktreePseudoRefs` (the public `rev-parse` surface stays
    /// deferred by §C.5 — `tests/compat/pseudo_ref_surface.rs` pins that).
    #[tokio::test]
    #[serial_test::serial]
    async fn linked_pseudo_refs_resolve_per_worktree() {
        use crate::internal::sequencer::{SequenceKind, SequenceState};

        let repo = tempfile::tempdir().expect("repo");
        let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let main = WorktreeScope::Main;
        let linked = WorktreeScope::Linked("wt-resolve-all".to_string());
        let linked_path = register_linked_worktree(repo.path(), "wt-resolve-all");

        // Main: a cherry-pick sequence (DB row).
        let _main_pin = WorktreeScope::pin_scope_for_test(main.clone(), repo.path().to_path_buf());
        crate::internal::sequencer::save(&SequenceState {
            kind: SequenceKind::CherryPick,
            head_name: "main".to_string(),
            head_orig: "1111111111111111111111111111111111111111".to_string(),
            current_oid: "2222222222222222222222222222222222222222".to_string(),
            todo: Vec::new(),
            payload: r#"{"stopped_on_conflict":true}"#.to_string(),
        })
        .await
        .expect("save main's sequence");
        drop(_main_pin);

        // Linked worktree: an in-progress merge (sidecar in ITS gitdir).
        std::fs::write(
            linked_path
                .join(crate::utils::util::ROOT_DIR)
                .join("merge-state.json"),
            serde_json::json!({
                "head_name": "topic",
                "orig_head": "6666666666666666666666666666666666666666",
                "target": "5555555555555555555555555555555555555555",
                "target_ref": "refs/heads/incoming",
                "conflicted_paths": ["clash.txt"],
            })
            .to_string(),
        )
        .expect("linked merge sidecar");

        let named = |resolved: &[ResolvedPseudoRef]| -> Vec<&'static str> {
            resolved.iter().map(|entry| entry.name).collect()
        };

        let main_all = WorktreePseudoRefs::new(main)
            .resolve_all()
            .await
            .expect("main's resolve_all");
        assert_eq!(
            named(&main_all),
            vec!["ORIG_HEAD", "CHERRY_PICK_HEAD"],
            "main defines exactly its own pseudo-refs: {main_all:?}"
        );
        assert!(
            main_all
                .iter()
                .all(|entry| !entry.oid.starts_with('5') && !entry.oid.starts_with('6')),
            "and none of its answers leak the linked worktree's merge: {main_all:?}"
        );

        let linked_all = WorktreePseudoRefs::new(linked)
            .resolve_all()
            .await
            .expect("the linked worktree's resolve_all");
        assert_eq!(
            named(&linked_all),
            vec!["ORIG_HEAD", "MERGE_HEAD"],
            "the linked worktree defines exactly its own pseudo-refs: {linked_all:?}"
        );
        assert_eq!(
            linked_all
                .iter()
                .find(|entry| entry.name == "MERGE_HEAD")
                .expect("its merge target")
                .oid,
            "5555555555555555555555555555555555555555",
            "its MERGE_HEAD is ITS merge's target"
        );
        assert!(
            linked_all
                .iter()
                .all(|entry| !entry.oid.starts_with('1') && !entry.oid.starts_with('2')),
            "and none of its answers leak main's cherry-pick: {linked_all:?}"
        );
    }

    /// §C.5 / W2 acceptance: `REBASE_HEAD` follows its scope like the rest.
    ///
    /// Two worktrees can each hold a STOPPED rebase; each `WorktreePseudoRefs`
    /// must project its own scope's `stopped_sha`, never the other's — and a
    /// rebase that has not stopped defines no `REBASE_HEAD` at all.
    #[tokio::test]
    #[serial_test::serial]
    async fn each_scope_projects_its_own_rebase_head() {
        use sea_orm::{ConnectionTrait, Statement};

        let repo = tempfile::tempdir().expect("repo");
        let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        register_linked_worktree(repo.path(), "wt-rebase");

        let db = crate::internal::worktree_scope::request_db()
            .await
            .expect("repository db");
        for (scope_key, stopped) in [
            ("", Some("1111111111111111111111111111111111111111")),
            (
                "wt-rebase",
                Some("2222222222222222222222222222222222222222"),
            ),
        ] {
            db.execute_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "INSERT INTO rebase_state (worktree_id, head_name, onto, orig_head, \
                 current_head, todo, done, stopped_sha) VALUES (?, 'main', \
                 '3333333333333333333333333333333333333333', \
                 '4444444444444444444444444444444444444444', \
                 '5555555555555555555555555555555555555555', '', '', ?)",
                [scope_key.into(), stopped.into()],
            ))
            .await
            .expect("seed a stopped rebase");
        }

        // Pin the LINKED scope, then ask MAIN: a projection reading the pin
        // would answer 2222…
        let _pin = WorktreeScope::pin_scope_for_test(
            WorktreeScope::Linked("wt-rebase".to_string()),
            repo.path().to_path_buf(),
        );
        let main_head = WorktreePseudoRefs::new(WorktreeScope::Main)
            .resolve(PseudoRef::RebaseHead)
            .await
            .expect("main's REBASE_HEAD")
            .expect("main's rebase is stopped");
        assert_eq!(
            main_head.oid, "1111111111111111111111111111111111111111",
            "main's REBASE_HEAD is MAIN's stopped commit"
        );
        let linked_head = WorktreePseudoRefs::new(WorktreeScope::Linked("wt-rebase".to_string()))
            .resolve(PseudoRef::RebaseHead)
            .await
            .expect("the linked worktree's REBASE_HEAD")
            .expect("its rebase is stopped");
        assert_eq!(
            linked_head.oid, "2222222222222222222222222222222222222222",
            "and the linked worktree's is its own"
        );

        // A rebase in progress that has NOT stopped defines no REBASE_HEAD.
        db.execute_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "UPDATE rebase_state SET stopped_sha = NULL WHERE worktree_id = ''".to_string(),
        ))
        .await
        .expect("clear main's stop point");
        assert!(
            WorktreePseudoRefs::new(WorktreeScope::Main)
                .resolve(PseudoRef::RebaseHead)
                .await
                .expect("main's REBASE_HEAD after the stop cleared")
                .is_none(),
            "an unstopped rebase defines no REBASE_HEAD"
        );
    }

    /// §C.5: the SIDECAR half follows the scope too.
    ///
    /// The database half was already scoped; the merge/revert sidecars and
    /// `FETCH_HEAD` are FILES, and reading them through the request pin made
    /// `WorktreePseudoRefs::new(Main)` answer with the pinned worktree's merge
    /// state while its `ORIG_HEAD` came from main's rows. This creates a real
    /// linked worktree, gives each a different sidecar, and asserts each scope
    /// reads its own.
    #[tokio::test]
    #[serial_test::serial]
    async fn each_scope_reads_its_own_sidecars() {
        let repo = tempfile::tempdir().expect("repo");
        let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        let linked_path = register_linked_worktree(repo.path(), "wt-sidecars");
        let linked_gitdir = linked_path.join(crate::utils::util::ROOT_DIR);

        // Different merge sidecars in each gitdir.
        let sidecar = |target: &str| {
            serde_json::json!({
                "head_name": "main",
                "orig_head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "target": target,
                "target_ref": "refs/heads/other",
                "allow_unrelated_histories": false,
                "skip_hooks": false,
                "conflicted_paths": []
            })
            .to_string()
        };
        std::fs::write(
            repo.path()
                .join(crate::utils::util::ROOT_DIR)
                .join("merge-state.json"),
            sidecar("1111111111111111111111111111111111111111"),
        )
        .expect("main sidecar");
        std::fs::write(
            linked_gitdir.join("merge-state.json"),
            sidecar("2222222222222222222222222222222222222222"),
        )
        .expect("linked sidecar");

        // Pin the LINKED worktree, then ask MAIN: a projection that read the
        // pin instead of its own scope would answer with 2222…
        let _pin = WorktreeScope::pin_scope_for_test(
            WorktreeScope::Linked("wt-sidecars".to_string()),
            linked_path.clone(),
        );
        let main_merge = WorktreePseudoRefs::new(WorktreeScope::Main)
            .resolve(PseudoRef::MergeHead)
            .await
            .expect("main's MERGE_HEAD")
            .expect("main has a merge sidecar");
        assert_eq!(
            main_merge.oid, "1111111111111111111111111111111111111111",
            "main's MERGE_HEAD comes from MAIN's sidecar, not the pinned worktree's"
        );

        let linked_merge =
            WorktreePseudoRefs::new(WorktreeScope::Linked("wt-sidecars".to_string()))
                .resolve(PseudoRef::MergeHead)
                .await
                .expect("the linked worktree's MERGE_HEAD")
                .expect("it has a merge sidecar");
        assert_eq!(
            linked_merge.oid, "2222222222222222222222222222222222222222",
            "and the linked worktree reads its own"
        );

        // `FETCH_HEAD` is file-backed too, and libra's fetch marks every row
        // `not-for-merge` — the projection must still answer.
        std::fs::write(
            linked_gitdir.join("FETCH_HEAD"),
            "3333333333333333333333333333333333333333\tnot-for-merge\tbranch 'main' of origin\n",
        )
        .expect("linked FETCH_HEAD");
        let fetched = WorktreePseudoRefs::new(WorktreeScope::Linked("wt-sidecars".to_string()))
            .resolve(PseudoRef::FetchHead)
            .await
            .expect("the linked worktree's FETCH_HEAD")
            .expect("a not-for-merge row still defines an advertised head");
        assert_eq!(fetched.oid, "3333333333333333333333333333333333333333");
        assert!(
            WorktreePseudoRefs::new(WorktreeScope::Main)
                .resolve(PseudoRef::FetchHead)
                .await
                .expect("main's FETCH_HEAD")
                .is_none(),
            "and main, which never fetched, has none"
        );
    }

    /// Two stores defining the same pseudo-ref DIFFERENTLY is reported, not
    /// silently resolved by priority — only one operation may be in progress
    /// per worktree, so disagreement is leftover state a caller must see.
    #[test]
    fn disagreeing_sources_are_an_error_and_agreeing_ones_are_not() {
        let same = |oid: &str, source: &'static str| ResolvedPseudoRef {
            name: PseudoRef::OrigHead.name(),
            oid: oid.to_string(),
            source,
        };
        // One source: the answer.
        assert_eq!(
            one_of(
                PseudoRef::OrigHead,
                vec![same("aaaa", "sequence_state")],
                ""
            )
            .expect("one source resolves")
            .map(|resolved| resolved.oid),
            Some("aaaa".to_string())
        );
        // Two sources AGREEING — a conflicted `revert` writes both the row and
        // the sidecar with the same commit; refusing that would break it.
        assert_eq!(
            one_of(
                PseudoRef::OrigHead,
                vec![
                    same("aaaa", "sequence_state"),
                    same("aaaa", "revert-state.json")
                ],
                ""
            )
            .expect("agreeing sources resolve")
            .map(|resolved| resolved.oid),
            Some("aaaa".to_string())
        );
        // Two sources DISAGREEING: reported, with both named.
        let error = one_of(
            PseudoRef::OrigHead,
            vec![
                same("aaaa", "rebase_state"),
                same("bbbb", "merge-state.json"),
            ],
            "wt-1",
        )
        .expect_err("disagreement is not resolvable by priority");
        assert!(error.contains("rebase_state says aaaa"), "{error}");
        assert!(error.contains("merge-state.json says bbbb"), "{error}");
        assert!(error.contains("worktree 'wt-1'"), "{error}");
    }

    #[test]
    fn every_declared_name_round_trips() {
        for which in PseudoRef::ALL {
            assert_eq!(
                PseudoRef::parse(which.name()),
                Some(which),
                "{} must parse back to itself",
                which.name()
            );
        }
    }

    /// §C.5 names exactly these six. The list is the contract `rev-parse`'s
    /// refusal message and the compatibility row are both derived from, so a
    /// silent addition here would leave both stale.
    #[test]
    fn the_declared_set_is_the_c5_table() {
        let names: Vec<&str> = PseudoRef::ALL.iter().map(|which| which.name()).collect();
        assert_eq!(
            names,
            vec![
                "ORIG_HEAD",
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "REBASE_HEAD",
                "FETCH_HEAD"
            ]
        );
    }

    #[test]
    fn a_git_name_libra_does_not_define_is_not_accepted() {
        // Git writes these too; §C.5 does not define them here, and pretending
        // otherwise would make `rev-parse` promise a value nothing produces.
        assert_eq!(PseudoRef::parse("BISECT_HEAD"), None);
        assert_eq!(PseudoRef::parse("AUTO_MERGE"), None);
        // Case-sensitive, as Git is.
        assert_eq!(PseudoRef::parse("merge_head"), None);
    }
}
