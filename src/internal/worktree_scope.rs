//! `WorktreeScope` — the single internal value object for "which worktree is
//! this operation acting on" (plan-20260714 Part C §C.4.1).
//!
//! Before this type, each module re-interpreted a bare `Option<String>`
//! worktree id (or a bool from `is_linked_worktree()`) with its own convention
//! for what `None` meant. Two storage layers disagree on how the main worktree
//! is spelled, and getting it wrong silently aliases a linked worktree onto
//! main's rows:
//!
//! - the `reference` table (HEAD) uses a NULLABLE `worktree_id`, where main is
//!   `NULL` — see [`WorktreeScope::worktree_id`];
//! - the sequencer/advisory tables use `worktree_id TEXT NOT NULL`, where main
//!   is the empty string (SQLite allows many NULLs, so a nullable unique key
//!   could not express "at most one row per scope") — see
//!   [`WorktreeScope::storage_key`].
//!
//! Resolve the scope ONCE per request and pass it down; do not re-read the
//! process cwd at each layer (the cwd is not a reliable scope carrier — RAII
//! `set_current_dir` guards make it a moving target).

use std::{
    path::PathBuf,
    sync::{RwLock, RwLockReadGuard},
};

/// The scope AND working directory resolved once at command entry — see
/// [`WorktreeScope::pin_request_scope`]. `None` outside a CLI invocation.
static REQUEST_SCOPE: RwLock<Option<RequestScope>> = RwLock::new(None);

/// What an invocation resolved about itself, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestScope {
    pub scope: WorktreeScope,
    /// The directory the command was invoked from. Held with the scope so a
    /// consumer that needs a PATH uses the same observation the scope came
    /// from, rather than re-reading a cwd that has since moved.
    pub workdir: PathBuf,
    /// The gitdir RESOLVED from `workdir` at pin time.
    ///
    /// Resolved once and cached because deriving it later from `workdir` is
    /// wrong in a case that actually happens: a command invoked from a
    /// SUBDIRECTORY pins that subdirectory, and `<workdir>/.libra` is then a
    /// path inside the worktree that no gitdir lives at — which an atomic
    /// write would happily create. Holding the resolved value means a later
    /// discovery failure cannot invent one.
    pub gitdir: PathBuf,
    /// The COMMON storage (`db`/`objects`/legacy sequencer sidecars) resolved
    /// from `workdir` at pin time. Shared between a main worktree and its
    /// linked ones — but shared between WORKTREES, not between repositories,
    /// which is what re-reading the cwd mid-command could cross.
    pub storage: PathBuf,
    /// The working-tree ROOT resolved from `workdir` at pin time — `workdir`
    /// itself is wherever the user stood, which is often a subdirectory.
    pub worktree_root: PathBuf,
}

impl RequestScope {
    /// Resolve every path this invocation will need, ONCE, from the workdir it
    /// was invoked in (§C.4.2).
    ///
    /// One `try_get_paths_full` walk answers all three — gitdir, common
    /// storage and working-tree root — so they cannot disagree with each other
    /// the way three separate later lookups from a moved cwd could.
    ///
    /// `None` when the workdir is not inside a repository. A PARTIALLY
    /// resolved pin is deliberately not representable: a pin that carried
    /// "this is worktree A, but I do not know where A is" would be installed
    /// by a repository that vanished between a caller's probe and this call,
    /// and every reader of it could only panic. Not installing it leaves the
    /// invocation ambient, which is what it was before any of this existed —
    /// the repository is gone either way, and the ambient path reports that
    /// with the diagnostics it always had.
    pub(crate) fn resolve(workdir: PathBuf) -> Option<Self> {
        let (storage, worktree_root, gitdir) =
            util::try_get_request_paths(Some(workdir.clone())).ok()?;
        // The SCOPE comes from the gitdir this same walk produced, never from
        // a second observation: resolving it from the cwd while the paths come
        // from `workdir` is two reads, and a `set_current_dir` between them
        // pairs worktree A's scope key with worktree B's gitdir and storage —
        // every scoped row then written under A's key into B's repository.
        let scope = match util::worktree_id_for_gitdir(&gitdir) {
            Some(id) => WorktreeScope::Linked(id),
            None => WorktreeScope::Main,
        };
        Some(Self {
            scope,
            workdir,
            gitdir,
            storage,
            worktree_root,
        })
    }
}

/// The repository database of the worktree THIS INVOCATION is acting on.
///
/// §C.4.2 pairs a scope with its storage: pinning the scope key while opening
/// the database from the process cwd is only half a fix, because a nested or
/// in-process operation that moves the cwd into another repository would write
/// worktree A's rows into repository B — where A's later reads never find them,
/// so a second sequence can start and strand the first one's recovery state.
///
/// Falls back to the ambient connection only for a caller that never pinned.
pub async fn request_db() -> std::io::Result<sea_orm::DatabaseConnection> {
    // The lock guard is RELEASED before the await: holding a std `MutexGuard`
    // across one blocks every other scope reader for the length of a database
    // open, and would deadlock a reader on the same thread.
    // The storage was resolved AT PIN TIME and is used as-is: re-resolving it
    // from the pinned workdir here would re-walk the filesystem, so a
    // repository replaced or removed mid-command could answer with a
    // different storage than the gitdir/root this same pin already handed to
    // the file layers — the two halves disagreeing is the accident §C.4.2
    // exists to prevent.
    let storage = pinned().as_ref().map(|pinned| pinned.storage.clone());
    match storage {
        Some(storage) => {
            crate::internal::db::get_db_conn_instance_for_path(&storage.join(util::DATABASE)).await
        }
        None => Ok(crate::internal::db::get_db_conn_instance().await),
    }
}

/// Restores the enclosing request scope when dropped — see
/// [`WorktreeScope::override_scope`].
#[must_use = "the override ends when this guard is dropped"]
pub struct ScopeOverrideGuard {
    previous: Option<RequestScope>,
}

impl Drop for ScopeOverrideGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        match REQUEST_SCOPE.write() {
            Ok(mut slot) => *slot = previous,
            Err(poison) => *poison.into_inner() = previous,
        }
    }
}

/// Read the pinned scope, tolerating a poisoned lock: the guarded value is a
/// plain `Option` swap, so a panicked writer cannot leave it half-written, and
/// crashing every scope lookup would be far worse than reading it.
fn pinned() -> RwLockReadGuard<'static, Option<RequestScope>> {
    REQUEST_SCOPE
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
}

use crate::utils::{
    error::{CliError, CliResult},
    util,
};

/// Which worktree an operation is scoped to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeScope {
    /// The main worktree: `reference.worktree_id IS NULL`, sequencer key `""`.
    Main,
    /// A linked worktree, identified by its stable `worktree_id`.
    Linked(String),
}

impl WorktreeScope {
    /// The scope of the CURRENT DIRECTORY.
    ///
    /// This is the right answer for a caller asking "which worktree am I
    /// standing in", including one that has deliberately stepped into another
    /// repository — a local-path `fetch` reading the remote's storage,
    /// `worktree add` seeding the new worktree's HEAD. A command reading or
    /// writing ITS OWN sequencer/rebase/bisect state wants
    /// [`WorktreeScope::for_request`] instead.
    ///
    /// A linked worktree whose `worktree_id` file is missing or unreadable is
    /// still reported as [`WorktreeScope::Linked`] with the id synthesized from
    /// its canonical path — it must NEVER fall back to [`WorktreeScope::Main`],
    /// which would graft this worktree onto main's HEAD and sequencer rows.
    pub fn current() -> Self {
        Self::resolve_from_cwd()
    }

    /// The scope THIS INVOCATION is acting on: the pinned request scope when
    /// dispatch resolved one, the cwd otherwise.
    ///
    /// Use this — not [`Self::current`] — wherever a command reads or writes
    /// its own worktree's sequencer, rebase or bisect state. Those are the
    /// paths where a cwd that moves mid-command (`ChangeDirGuard`, any library
    /// calling `set_current_dir`) would send a save into a different
    /// worktree's row.
    ///
    /// It is deliberately NOT what `current()` returns. A process legitimately
    /// touches other repositories — a local-path `fetch` reads the remote's
    /// storage, `worktree add` seeds the new worktree's HEAD, `repair
    /// --migrate-layout` seeds a migrated one — and answering those with the
    /// invoking worktree's scope looks for rows that were never there.
    pub fn for_request() -> Self {
        match pinned().as_ref() {
            Some(pinned) => pinned.scope.clone(),
            None => Self::resolve_from_cwd(),
        }
    }

    /// This invocation's resolved scope AND workdir, if it pinned one.
    pub fn request_scope() -> Option<RequestScope> {
        pinned().clone()
    }

    /// Read the cwd and resolve, ignoring any pinned request scope.
    pub fn resolve_from_cwd() -> Self {
        match util::current_worktree_id() {
            Some(id) => WorktreeScope::Linked(id),
            None => WorktreeScope::Main,
        }
    }

    /// Pin the scope of this invocation, once, at command entry (§C.4.2
    /// resolve-once).
    ///
    /// The cwd is not a scope carrier: `ChangeDirGuard` and any library that
    /// calls `set_current_dir` move it under the command's feet. A sequencer
    /// control that read its state in worktree A and then saved it after the
    /// cwd moved to B would write B's row — deleting A's sequence and
    /// stranding A's work. Pinning makes every later
    /// [`WorktreeScope::for_request`] in the invocation answer for the same
    /// worktree the command started in.
    ///
    /// Pin for the lifetime of the returned guard, which RESTORES whatever was
    /// pinned before (including nothing).
    ///
    /// Restoring matters for in-process hosts: a process that dispatches
    /// `--version` from main and then calls a library API from a linked
    /// worktree must not be answered with main's scope because a previous
    /// invocation left its pin behind. CLI dispatch holds the guard for the
    /// duration of the command; callers that never pin — the `libra code`
    /// server, library users, unit tests — keep resolving from the cwd.
    #[must_use = "the pin ends when this guard is dropped"]
    pub fn pin_request_scope(workdir: PathBuf) -> ScopeOverrideGuard {
        Self::replace_request_scope(RequestScope::resolve(workdir))
    }

    fn replace_request_scope(next: Option<RequestScope>) -> ScopeOverrideGuard {
        let previous = match REQUEST_SCOPE.write() {
            Ok(mut slot) => std::mem::replace(&mut *slot, next),
            Err(poison) => std::mem::replace(&mut *poison.into_inner(), next),
        };
        ScopeOverrideGuard { previous }
    }

    /// Whether this invocation pinned a scope.
    pub fn request_scope_is_pinned() -> bool {
        pinned().is_some()
    }

    /// Act on a DIFFERENT scope for the duration of the returned guard.
    ///
    /// For a command that deliberately operates on another worktree's rows and
    /// wants [`Self::for_request`] to say so — the previous pin is restored on
    /// drop, so the rest of the command still answers for the worktree the
    /// user invoked it from.
    pub fn override_scope(workdir: PathBuf) -> ScopeOverrideGuard {
        Self::replace_request_scope(RequestScope::resolve(workdir))
    }

    /// Install a pin whose SCOPE the filesystem cannot produce.
    ///
    /// Test-only, and deliberately so: production has no way to pair a scope
    /// key with paths that disagree with it, which is what §C.4.2 is about.
    /// Tests need it to represent a linked worktree's scope in a fixture that
    /// has only one working tree.
    #[cfg(test)]
    #[must_use = "the pin ends when this guard is dropped"]
    pub(crate) fn pin_scope_for_test(scope: WorktreeScope, workdir: PathBuf) -> ScopeOverrideGuard {
        let resolved =
            RequestScope::resolve(workdir).map(|resolved| RequestScope { scope, ..resolved });
        Self::replace_request_scope(resolved)
    }

    /// Run UNPINNED for the duration of the returned guard.
    ///
    /// For a command whose target repository does not exist when the
    /// invocation starts — `clone` and `init` create it and then step into
    /// it. There is no worktree for the invoker to act on, so pinning one
    /// would be a lie in both directions: run from outside a repository the
    /// pin resolves nothing and every pinned reader errors; run from INSIDE
    /// one it names the enclosing repository, and the checkout would build
    /// its index instead of the new repository's.
    ///
    /// Unpinned means every resolver falls back to the ambient cwd, which is
    /// what these commands move deliberately and is the only correct answer
    /// while they are inside a repository they are still building.
    pub fn unpinned() -> ScopeOverrideGuard {
        Self::replace_request_scope(None)
    }

    /// Resolve the scope of an EXPLICIT working directory (W1 §C.4.1.1
    /// scope↔workdir binding). A command that captures its workdir once can
    /// derive the matching scope from that same value — deterministic for a
    /// given path, immune to a concurrent process-cwd switch between the
    /// capture and a later mutation/guard step. The same fail-closed rule as
    /// [`WorktreeScope::current`] applies: a linked worktree with a
    /// missing/unreadable `worktree_id` file still resolves as `Linked`.
    pub fn for_workdir(workdir: &std::path::Path) -> Self {
        match util::worktree_id_for_base(Some(workdir.to_path_buf())) {
            Some(id) => WorktreeScope::Linked(id),
            None => WorktreeScope::Main,
        }
    }

    /// True when this is a linked (non-main) worktree.
    pub fn is_linked(&self) -> bool {
        matches!(self, WorktreeScope::Linked(_))
    }

    /// Key for NULLABLE `worktree_id` columns (the `reference`/HEAD table):
    /// `None` for main, `Some(id)` for a linked worktree.
    pub fn worktree_id(&self) -> Option<&str> {
        match self {
            WorktreeScope::Main => None,
            WorktreeScope::Linked(id) => Some(id.as_str()),
        }
    }

    /// Key for `worktree_id TEXT NOT NULL` columns (sequencer/advisory tables):
    /// the empty string for main, the id for a linked worktree. Empty string
    /// rather than NULL so a unique key can express "at most one row per scope"
    /// — SQLite treats every NULL as distinct.
    pub fn storage_key(&self) -> &str {
        match self {
            WorktreeScope::Main => "",
            WorktreeScope::Linked(id) => id.as_str(),
        }
    }

    /// This worktree's LOCAL `.libra` gitdir — where its private `index`,
    /// `worktree_id`, and filesystem sidecars live. Equals the common storage
    /// for main; the linked worktree's own `.libra` otherwise.
    pub fn local_gitdir(&self) -> CliResult<PathBuf> {
        util::try_get_worktree_gitdir(None).map_err(|error| {
            CliError::fatal(format!(
                "cannot resolve this worktree's gitdir: {error}; if this is a linked worktree, \
                 run `libra worktree repair --confirm <worktree-path>` from the main worktree"
            ))
            .with_stable_code(crate::utils::error::StableErrorCode::IoReadFailed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_scope_uses_null_reference_key_and_empty_storage_key() {
        let scope = WorktreeScope::Main;
        assert!(!scope.is_linked());
        // The `reference` table spells main as NULL...
        assert_eq!(scope.worktree_id(), None);
        // ...while the NOT NULL sequencer columns spell it as "".
        assert_eq!(scope.storage_key(), "");
    }

    #[test]
    fn linked_scope_carries_its_id_in_both_key_forms() {
        let scope = WorktreeScope::Linked("wt-abc123".to_string());
        assert!(scope.is_linked());
        assert_eq!(scope.worktree_id(), Some("wt-abc123"));
        assert_eq!(scope.storage_key(), "wt-abc123");
    }

    /// §C.4.2 resolve-once: once an invocation has pinned its scope, moving
    /// the process cwd cannot change the answer.
    ///
    /// This is the hazard in one assertion: a sequencer control reads its
    /// state in worktree A, something calls `set_current_dir` — a
    /// `ChangeDirGuard`, a library, a nested helper — and the save that
    /// follows would land in whatever worktree the cwd had become, deleting
    /// A's sequence while A's checkout stays on disk.
    #[test]
    #[serial_test::serial]
    fn a_pinned_scope_survives_a_cwd_change() {
        let original = std::env::current_dir().expect("cwd");
        let elsewhere = std::env::temp_dir();

        let _pin = WorktreeScope::pin_scope_for_test(
            WorktreeScope::Linked("wt-pinned".to_string()),
            original.clone(),
        );
        assert!(WorktreeScope::request_scope_is_pinned());
        assert_eq!(WorktreeScope::for_request().storage_key(), "wt-pinned");

        std::env::set_current_dir(&elsewhere).expect("move the process cwd");
        assert_eq!(
            WorktreeScope::for_request().storage_key(),
            "wt-pinned",
            "the pinned scope answers for the whole invocation"
        );
        // `current()` still answers about the CWD. A process legitimately
        // steps into another repository — a local-path fetch reads the
        // remote's storage — and answering that with the invoking worktree's
        // scope looks for rows that were never there.
        assert_eq!(WorktreeScope::current(), WorktreeScope::Main);
        assert_eq!(WorktreeScope::resolve_from_cwd(), WorktreeScope::Main);

        // A later invocation re-pins rather than inheriting.
        {
            let _inner = WorktreeScope::pin_request_scope(elsewhere.clone());
            assert_eq!(WorktreeScope::for_request(), WorktreeScope::Main);
        }
        // ...and the guard RESTORES the enclosing pin, so an in-process host
        // is not left answering for whatever ran last.
        assert_eq!(WorktreeScope::for_request().storage_key(), "wt-pinned");

        std::env::set_current_dir(&original).expect("restore the cwd");
    }

    /// §C.4.2: the SIDECAR path follows the pinned workdir, not the cwd.
    ///
    /// This is the file half of the same hazard: a command reads its rebase
    /// state from scope A's row, the cwd moves, and the aux sidecar it writes
    /// next lands in scope B's gitdir — pairing one worktree's database state
    /// with another's file state, which is worse than either alone.
    #[test]
    #[serial_test::serial]
    fn a_pinned_workdir_keeps_sidecars_in_their_own_gitdir() {
        let repo = tempfile::tempdir().expect("repo");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        // A real repository, so the resolver has a storage root to find.
        {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(crate::utils::test::setup_with_new_libra_in(repo.path()));
        }
        let original = std::env::current_dir().expect("cwd");

        // Pin a SUBDIRECTORY, which is what a command invoked from one does.
        // Deriving `<workdir>/.libra` would answer `repo/sub/.libra` — a path
        // no gitdir lives at, and one an atomic write would happily create.
        let subdir = repo.path().join("sub");
        std::fs::create_dir_all(&subdir).expect("subdir");
        let _pin = WorktreeScope::pin_request_scope(subdir.clone());
        std::env::set_current_dir(elsewhere.path()).expect("move the cwd");

        let resolved =
            crate::utils::util::request_worktree_gitdir().expect("the pinned workdir resolves");
        // Canonicalize the expectation: the resolver answers canonical paths,
        // and macOS tempdirs live behind the /var -> /private/var alias.
        let repo_root = std::fs::canonicalize(repo.path()).unwrap_or_else(|_| repo.path().into());
        assert_eq!(
            resolved,
            repo_root.join(crate::utils::util::ROOT_DIR),
            "the sidecar gitdir is the WORKTREE's, not one derived from the \
             invocation directory"
        );
        assert_eq!(
            crate::utils::util::request_worktree_gitdir_strict(),
            resolved,
            "and the infallible variant agrees"
        );

        std::env::set_current_dir(&original).expect("restore the cwd");
    }

    /// §C.4.2: the STORAGE and the working-tree ROOT follow the pin too.
    ///
    /// The legacy sequencer sidecars (`rebase-merge`/`rebase-apply`) live in
    /// the common storage and the reset writes files under the working-tree
    /// root. Both were resolved from the cwd, so a rebase that read its plan
    /// from repository A's rows could finish against repository B's sidecar
    /// directory and overwrite B's files. Pinning a SUBDIRECTORY also proves
    /// the root is the resolved worktree root, not the invocation directory.
    #[test]
    #[serial_test::serial]
    fn a_pinned_request_resolves_storage_and_worktree_root_once() {
        let repo = tempfile::tempdir().expect("repo");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(crate::utils::test::setup_with_new_libra_in(repo.path()));
        }
        let original = std::env::current_dir().expect("cwd");
        let canonical_repo =
            std::fs::canonicalize(repo.path()).unwrap_or_else(|_| repo.path().to_path_buf());

        let subdir = repo.path().join("sub");
        std::fs::create_dir_all(&subdir).expect("subdir");
        let _pin = WorktreeScope::pin_request_scope(subdir.clone());
        std::env::set_current_dir(elsewhere.path()).expect("move the cwd");

        let storage = crate::utils::util::request_storage_path();
        let storage = std::fs::canonicalize(&storage).unwrap_or(storage);
        assert_eq!(
            storage,
            canonical_repo.join(crate::utils::util::ROOT_DIR),
            "the legacy sidecar directory is resolved in the PINNED repository"
        );

        let root = crate::utils::util::request_working_dir();
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        assert_eq!(
            root, canonical_repo,
            "and the working-tree root is the worktree's root, not the \
             invocation subdirectory or the cwd"
        );

        std::env::set_current_dir(&original).expect("restore the cwd");
    }

    /// §C.4.2: the scope key and the DATABASE come from the same observation.
    ///
    /// Pinning the key while opening the database from the cwd is only half a
    /// fix — a nested or in-process operation that moves the cwd into another
    /// repository would write this worktree's rows into THAT one, where its own
    /// later reads never find them, so a second sequence could start and strand
    /// the first one's recovery state.
    #[tokio::test]
    #[serial_test::serial]
    async fn the_request_database_follows_the_pin_not_the_cwd() {
        let repo_a = tempfile::tempdir().expect("repo a");
        let repo_b = tempfile::tempdir().expect("repo b");
        let original = std::env::current_dir().expect("cwd");
        for repo in [repo_a.path(), repo_b.path()] {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo);
            crate::utils::test::setup_with_new_libra_in(repo).await;
        }

        let _pin = WorktreeScope::pin_request_scope(repo_a.path().to_path_buf());
        // The cwd is repository B; the pin is repository A.
        std::env::set_current_dir(repo_b.path()).expect("move the cwd");

        let db = request_db()
            .await
            .expect("the pinned repository's database");
        let path = {
            use sea_orm::{ConnectionTrait, Statement};
            let row = db
                .query_one_raw(Statement::from_string(
                    db.get_database_backend(),
                    "PRAGMA database_list".to_string(),
                ))
                .await
                .expect("query the open database")
                .expect("a row");
            row.try_get_by_index::<String>(2).expect("the file path")
        };
        let path = std::fs::canonicalize(&path).unwrap_or_else(|_| std::path::PathBuf::from(&path));
        let expected =
            std::fs::canonicalize(repo_a.path()).unwrap_or_else(|_| repo_a.path().to_path_buf());
        assert!(
            path.starts_with(&expected),
            "the request database is the PINNED repository's ({}), not the cwd's: {}",
            expected.display(),
            path.display()
        );

        std::env::set_current_dir(&original).expect("restore the cwd");
    }

    /// §C.4.2: a workdir that cannot be resolved installs NO pin — and in
    /// particular does not leave the ENCLOSING pin answering for it.
    ///
    /// A partially resolved pin is not representable, so a repository that
    /// disappeared between a caller's probe and the pin cannot produce one
    /// whose readers can only panic. What must not happen is the other
    /// failure: an inner scope that cannot be resolved silently inheriting
    /// the outer one, so worktree A's rows are read for an operation that has
    /// nothing to do with A.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_unresolvable_pin_installs_nothing_and_never_inherits() {
        let outer = tempfile::tempdir().expect("the enclosing repository");
        let ambient = tempfile::tempdir().expect("the repository the cwd is in");
        let nowhere = tempfile::tempdir().expect("not a repository");
        let original = std::env::current_dir().expect("cwd");
        for repo in [outer.path(), ambient.path()] {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo);
            crate::utils::test::setup_with_new_libra_in(repo).await;
        }

        let _outer_pin = WorktreeScope::pin_request_scope(outer.path().to_path_buf());
        std::env::set_current_dir(ambient.path()).expect("move the cwd");
        {
            // An inner pin at a directory that is NOT a repository.
            let _inner = WorktreeScope::pin_request_scope(nowhere.path().to_path_buf());
            assert!(
                !WorktreeScope::request_scope_is_pinned(),
                "an unresolvable workdir installs no pin"
            );

            let db = request_db().await.expect("resolves ambiently");
            let path = open_database_path(&db).await;
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            let expected = std::fs::canonicalize(ambient.path())
                .unwrap_or_else(|_| ambient.path().to_path_buf());
            assert!(
                path.starts_with(&expected),
                "it resolves the CWD's repository ({}), never the enclosing pin's",
                path.display()
            );
        }
        // ...and the enclosing pin is restored when the inner guard drops.
        assert!(WorktreeScope::request_scope_is_pinned());

        std::env::set_current_dir(&original).expect("restore the cwd");
    }

    /// The file backing an open sea-orm SQLite connection.
    async fn open_database_path(db: &sea_orm::DatabaseConnection) -> std::path::PathBuf {
        use sea_orm::{ConnectionTrait, Statement};
        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "PRAGMA database_list".to_string(),
            ))
            .await
            .expect("query the open database")
            .expect("a row");
        std::path::PathBuf::from(row.try_get_by_index::<String>(2).expect("the file path"))
    }

    /// The two key forms must never collide: a linked worktree can never be
    /// mistaken for main in either storage convention.
    #[test]
    fn linked_scope_never_aliases_main() {
        let main = WorktreeScope::Main;
        let linked = WorktreeScope::Linked("wt-abc123".to_string());
        assert_ne!(main.storage_key(), linked.storage_key());
        assert_ne!(main.worktree_id(), linked.worktree_id());
        assert_ne!(main, linked);
    }
}
