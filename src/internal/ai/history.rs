//! AI workflow history persistence backed by an orphan Git branch.
//!
//! Libra records every AI process artefact (Intent, Task, Run, Plan,
//! PatchSet, Evidence, ToolInvocation, Provenance, Decision, ContextFrame,
//! ...) on a parallel branch named [`AI_REF`] (`libra/intent`). The branch
//! is *orphan*: it shares no history with the user's code branches but
//! lives inside the same object database, which means:
//!
//! * The same `git gc` policy keeps both AI history and code history
//!   reachable.
//! * AI artefacts are content-addressed under standard Git rules and can be
//!   transferred via the same protocol as the rest of the repository.
//!
//! Each commit on this ref points to a tree that is partitioned by object
//! type (`intent/`, `task/`, `plan/`, ...), with one blob per object id
//! beneath the type subtree. The flow for `append` is:
//!
//! 1. Read the current head (with retry on a busy SQLite) — see
//!    [`HistoryManager::resolve_history_head`].
//! 2. Load that head's root tree, splice the new entry in beneath its type
//!    subtree, write a fresh root tree, and create a child commit — see
//!    [`HistoryManager::create_append_commit`].
//! 3. Compare-and-swap the ref forward, retrying on a stale head — see
//!    [`HistoryManager::update_ref_if_matches`].
//!
//! Concurrency is handled via two retry loops: a SQLite-busy retry that
//! covers transient lock contention, and a head-conflict retry that re-reads
//! the head and retries the splice when another process advanced the ref.
//! Both loops have bounded iteration counts so misuse cannot deadlock the
//! caller.

use std::{collections::HashSet, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, QueryFilter, QueryResult, Set, SqlErr, Statement, TransactionTrait, Value,
    sea_query::Expr,
};
use tokio::time::sleep;

use crate::{
    internal::{
        ai::observed_agents::RedactedBytes,
        model::reference::{self, ConfigKind},
    },
    utils::{
        object::{read_git_object, write_git_object},
        storage::Storage,
    },
};

/// Default Git reference for the AI history orphan branch.
///
/// All AI process objects (Intent, Task, Run, Plan, PatchSet, Evidence,
/// ToolInvocation, Provenance, Decision) live on this single branch,
/// running in parallel with the normal code branch (`refs/heads/*`).
///
/// By keeping AI objects reachable from this ref, they are protected
/// from `git gc` — the branch acts as a GC root.
///
/// In the database, this is stored with kind='Branch' and name='libra/intent'.
pub const AI_REF: &str = "libra/intent";
/// Maximum attempts to retry a SQLite operation that returns a transient
/// "database is locked" error before propagating the failure.
const SQLITE_BUSY_MAX_RETRIES: usize = 15;
/// Base delay (ms) for the linear backoff applied between SQLite-busy retries.
/// The actual delay is `BASE * attempt`, so the worst-case wait is roughly
/// `BASE * SUM(1..=MAX_RETRIES)` which keeps total time bounded.
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;
/// Maximum attempts to re-read the history head and retry a splice when a
/// concurrent writer advances the ref between read and CAS. The bound is
/// generous because each retry is purely local (no network I/O).
const HISTORY_HEAD_CONFLICT_MAX_RETRIES: usize = 32;

/// Outcome of a compare-and-swap reference update.
///
/// Used by [`HistoryManager::update_ref_if_matches`] to communicate whether
/// the ref moved successfully (`Updated`) or whether the expected head was
/// stale and the caller must restart the splice (`HeadChanged`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefUpdateOutcome {
    /// The ref was atomically advanced to the new commit.
    Updated,
    /// Another writer advanced the ref before our CAS — caller should
    /// re-read the head and rebuild the commit on top of it.
    HeadChanged,
}

/// Detect transient SQLite contention that should trigger a retry.
///
/// Functional scope:
/// - Inspects the error message for the well-known "database is locked" or
///   "database schema is locked" substrings emitted by SQLite under busy
///   contention.
///
/// Boundary conditions:
/// - This is intentionally a string match: the SeaORM error wraps the
///   underlying SQLite text, and there is no stable error-code variant for
///   busy/lock conditions in the wrapping layer.
fn is_sqlite_busy(err: &DbErr) -> bool {
    let message = err.to_string();
    message.contains("database is locked") || message.contains("database schema is locked")
}

/// Detect unique-constraint violations on the `reference` table.
///
/// Functional scope:
/// - Used by the optimistic CAS path: when two writers race to insert the
///   same ref name, one will see a unique-constraint violation; we treat
///   that as a `HeadChanged` outcome rather than a hard error.
fn is_sqlite_unique_violation(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

/// Manages object history using an orphan branch and Git Tree structure.
///
/// The default branch (`libra/intent`) stores **all** AI workflow objects,
/// running in parallel with the normal code history (`refs/heads/*`).
/// This is initialised during `libra init` so both branches exist from the start.
///
/// Structure (Commit -> Tree):
///   ├── intent/
///   │   └── <intent_id>
///   ├── task/
///   │   └── <task_id>
///   ├── run/
///   │   └── <run_id>
///   ├── plan/
///   │   └── <plan_id>
///   └── …
///
/// The manager is cheap to clone (all state lives behind `Arc` or owned
/// `String`/`PathBuf`) and is safe to share across async tasks. Concurrent
/// `append` calls on the same manager are serialised via the SQLite-side
/// CAS in [`Self::update_ref_if_matches`].
pub struct HistoryManager {
    #[allow(dead_code)]
    storage: Arc<dyn Storage + Send + Sync>,
    repo_path: PathBuf,
    db_conn: Arc<DatabaseConnection>,
    /// The reference name this manager writes to (e.g. "libra/intent").
    ref_name: String,
}

impl HistoryManager {
    /// Build a manager bound to the canonical [`AI_REF`].
    ///
    /// Functional scope:
    /// - Convenience constructor that delegates to [`Self::new_with_ref`]
    ///   with the standard `libra/intent` branch.
    pub fn new(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
    ) -> Self {
        Self::new_with_ref(storage, repo_path, db_conn, AI_REF)
    }

    /// Build a manager bound to an arbitrary ref name.
    ///
    /// Functional scope:
    /// - Used by tests and tooling that need to write a parallel AI history
    ///   under a custom ref (e.g. for staging, comparison, or namespace
    ///   isolation).
    ///
    /// Boundary conditions:
    /// - The ref name is not validated here; callers must ensure it is a
    ///   legal Git ref. The CAS path will fail loudly if the database
    ///   constraint rejects it.
    pub fn new_with_ref(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
        ref_name: impl Into<String>,
    ) -> Self {
        Self {
            storage,
            repo_path,
            db_conn,
            ref_name: ref_name.into(),
        }
    }

    /// Hand back a clone of the underlying SeaORM connection.
    ///
    /// Functional scope:
    /// - Convenience accessor for callers that need to issue auxiliary
    ///   queries against the same database (e.g. listing references for the
    ///   TUI) without having to thread a separate `Arc` around.
    pub fn database_connection(&self) -> DatabaseConnection {
        self.db_conn.as_ref().clone()
    }

    /// Initialise the AI orphan branch with an empty tree commit.
    ///
    /// This should be called once during `libra init` so that the AI ref
    /// exists from the start (parallel to `refs/heads/<branch>`).
    /// If the ref already exists this is a no-op.
    ///
    /// Functional scope:
    /// - Writes a single empty-tree commit and points the ref at it. The
    ///   commit has no parents (it is the root of the orphan branch) and
    ///   uses the canonical `Libra <ai@libra>` signatures so authorship is
    ///   traceable.
    ///
    /// Boundary conditions:
    /// - Returns early if the ref already exists; this makes the call
    ///   idempotent and safe to invoke from `libra init` regardless of
    ///   whether previous initialisations completed.
    /// - Surfaces errors from object serialisation, blob writing, or the
    ///   ref CAS so the caller can present an actionable message.
    pub async fn init_branch(&self) -> Result<()> {
        // Already initialised — nothing to do.
        if self.resolve_history_head().await?.is_some() {
            return Ok(());
        }

        // Write an empty tree.
        let empty_tree_hash = self.write_tree(&[])?;

        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "ai@libra".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "ai@libra".to_string(),
        );

        let commit = Commit::new(
            author,
            committer,
            empty_tree_hash,
            vec![],
            "Initialize AI history branch",
        );

        let commit_data = commit
            .to_data()
            .context("Failed to serialize AI history init commit")?;
        let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
        self.update_ref(&self.ref_name, commit_hash).await?;

        Ok(())
    }

    /// Return the ref name this manager writes to.
    ///
    /// Functional scope:
    /// - Useful for diagnostics, log messages, and TUI labels that need to
    ///   present the active AI history branch to the user.
    pub fn ref_name(&self) -> &str {
        &self.ref_name
    }

    /// Append an object to the history log.
    /// This operation is synchronous (commits immediately) for the MVP.
    ///
    /// Functional scope:
    /// - Implements the read-merge-CAS loop:
    ///   1. Read the current head.
    ///   2. Write a new commit that adds `<object_type>/<object_id>`
    ///      (replacing any prior entry under that path).
    ///   3. CAS the ref forward.
    /// - Reuses [`Self::create_append_commit`] for splice logic and
    ///   [`Self::update_ref_if_matches`] for the optimistic ref update.
    ///
    /// Boundary conditions:
    /// - Retries up to [`HISTORY_HEAD_CONFLICT_MAX_RETRIES`] times when a
    ///   concurrent writer advances the ref between read and CAS. After the
    ///   bound is exhausted the call fails with a contextual error so the
    ///   caller can decide whether to back off and retry.
    /// - The intermediate commit objects from failed CAS attempts remain in
    ///   the object database as garbage; they are unreachable and will be
    ///   collected by the next `libra gc` cycle.
    ///
    /// See: `tests::test_history_append_simple` and
    /// `tests::test_update_ref_if_matches_rejects_stale_history_head`.
    pub async fn append(
        &self,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<()> {
        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            // Phase 1: snapshot the head we are racing against.
            let parent_commit_id = self.resolve_history_head().await?;
            // Phase 2: build the new commit on top of the snapshot.
            let commit_hash =
                self.create_append_commit(parent_commit_id, object_type, object_id, blob_hash)?;

            // Phase 3: atomically advance the ref iff its current value still
            // equals the snapshot. On `HeadChanged`, restart from phase 1.
            match self
                .update_ref_if_matches(&self.ref_name, parent_commit_id, commit_hash)
                .await?
            {
                RefUpdateOutcome::Updated => return Ok(()),
                RefUpdateOutcome::HeadChanged if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES => {
                    continue;
                }
                RefUpdateOutcome::HeadChanged => {
                    return Err(anyhow!(
                        "history head changed repeatedly while appending {}/{}",
                        object_type,
                        object_id
                    ));
                }
            }
        }

        unreachable!("head conflict retry loop must return on success or terminal error")
    }

    /// Retrieve the object hash for a given type and ID from the current history.
    ///
    /// Functional scope:
    /// - Resolves the head commit, walks `<root_tree>/<object_type>/<object_id>`,
    ///   and returns the leaf blob hash if it exists.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the ref is not initialised, when no
    ///   subtree exists for `object_type`, or when the `object_id` entry is
    ///   missing under that subtree.
    /// - Surfaces `Err` only for object-store / parse failures.
    pub async fn get_object_hash(
        &self,
        object_type: &str,
        object_id: &str,
    ) -> Result<Option<ObjectHash>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            if let Some(type_entry) = root_items.iter().find(|item| item.name == object_type) {
                let type_items = self.load_tree(&type_entry.id)?;
                if let Some(item) = type_items.iter().find(|item| item.name == object_id) {
                    return Ok(Some(item.id));
                }
            }
        }
        Ok(None)
    }

    /// Find an object by ID across all types in the history.
    /// Returns (hash, type).
    ///
    /// Functional scope:
    /// - Convenience wrapper around [`Self::find_object_hashes`] that
    ///   returns only the first match.
    ///
    /// Boundary conditions:
    /// - When the same object id exists under multiple type subtrees the
    ///   caller has no control over which is chosen; use
    ///   [`Self::find_object_hashes`] when a deterministic tie-break is
    ///   required.
    pub async fn find_object_hash(&self, object_id: &str) -> Result<Option<(ObjectHash, String)>> {
        Ok(self.find_object_hashes(object_id).await?.into_iter().next())
    }

    /// Find all objects that share the same object ID across history types.
    ///
    /// Functional scope:
    /// - Walks every type subtree under the head root tree and collects
    ///   `(blob_hash, type_name)` tuples for every subtree containing
    ///   `object_id`.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised or the id
    ///   does not appear under any type.
    ///
    /// See: `tests::test_find_object_hashes_returns_all_matching_types`.
    pub async fn find_object_hashes(&self, object_id: &str) -> Result<Vec<(ObjectHash, String)>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            let mut matches = Vec::new();
            for type_entry in root_items {
                let type_items = self.load_tree(&type_entry.id)?;
                if let Some(item) = type_items.iter().find(|item| item.name == object_id) {
                    matches.push((item.id, type_entry.name.clone()));
                }
            }
            return Ok(matches);
        }
        Ok(Vec::new())
    }

    /// List all objects of a specific type from the current history.
    /// Returns a list of (object_id, object_hash).
    ///
    /// Functional scope:
    /// - Loads the head commit's `<object_type>` subtree and yields its
    ///   contents as `(name, blob_hash)` pairs in tree-order.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised or no
    ///   subtree exists for `object_type`.
    pub async fn list_objects(&self, object_type: &str) -> Result<Vec<(String, ObjectHash)>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            if let Some(type_entry) = root_items.iter().find(|item| item.name == object_type) {
                let type_items = self.load_tree(&type_entry.id)?;
                return Ok(type_items
                    .into_iter()
                    .map(|item| (item.name, item.id))
                    .collect());
            }
        }
        Ok(Vec::new())
    }

    /// List all object types present at the current history head.
    ///
    /// Functional scope:
    /// - Returns the names of every top-level subtree under the head root,
    ///   sorted lexicographically for stable output.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised. The empty
    ///   tree case (initialised ref with no objects) likewise yields an
    ///   empty vector.
    ///
    /// See: `tests::test_list_object_types_returns_sorted_types`.
    pub async fn list_object_types(&self) -> Result<Vec<String>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let mut root_items = self.load_commit_tree(&parent_id)?;
            root_items.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(root_items.into_iter().map(|item| item.name).collect());
        }
        Ok(Vec::new())
    }

    /// Resolve the current head commit of the AI history ref.
    ///
    /// Functional scope:
    /// - Queries the `reference` table for the row that matches
    ///   `(name=ref_name, kind=Branch)` and parses its `commit` column into
    ///   an [`ObjectHash`].
    /// - Tolerates transient SQLite-busy errors with a bounded linear
    ///   backoff governed by [`SQLITE_BUSY_MAX_RETRIES`] /
    ///   [`SQLITE_BUSY_RETRY_BASE_MS`].
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the ref row is missing or its `commit`
    ///   column is `NULL` (the ref exists but points nowhere yet).
    /// - Returns `Err` if the stored commit string is not a valid object
    ///   hash — this indicates database corruption and the caller should
    ///   surface it rather than silently treating it as missing.
    pub async fn resolve_history_head(&self) -> Result<Option<ObjectHash>> {
        let mut attempt = 0;
        let ref_model = loop {
            match reference::Entity::find()
                .filter(reference::Column::Name.eq(&self.ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&*self.db_conn)
                .await
            {
                Ok(found) => break found,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    attempt += 1;
                    // Linear backoff (BASE * attempt) — see SQLITE_BUSY_* constants.
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * attempt as u64,
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to query history head"),
            }
        };

        match ref_model {
            Some(model) => match model.commit {
                Some(commit_hash) => ObjectHash::from_str(&commit_hash)
                    .map(Some)
                    .map_err(|e| anyhow!("Invalid commit hash in DB: {}", e)),
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Load the root tree of a commit by parsing its `tree <hash>` header
    /// line.
    ///
    /// Functional scope:
    /// - Reads the commit blob, scans its text lines for the leading
    ///   `tree ` header, parses the referenced tree, and returns its items.
    ///
    /// Boundary conditions:
    /// - Returns an error when the commit blob is missing the `tree`
    ///   header. That should never happen for objects we wrote ourselves
    ///   but we guard against repository corruption.
    fn load_commit_tree(&self, commit_id: &ObjectHash) -> Result<Vec<TreeItem>> {
        let data = read_git_object(&self.repo_path, commit_id)?;
        // Commit format: tree <hash>\nparent...
        let content = String::from_utf8_lossy(&data);
        for line in content.lines() {
            if let Some(hash_str) = line.strip_prefix("tree ") {
                let tree_hash = ObjectHash::from_str(hash_str)
                    .map_err(|e| anyhow!("Invalid tree hash in commit: {}", e))?;
                return self.load_tree(&tree_hash);
            }
        }
        Err(anyhow!("Commit has no tree"))
    }

    /// Load and parse a tree object's items.
    ///
    /// Functional scope:
    /// - Thin wrapper around `Tree::from_bytes` for the AI-history call
    ///   sites; centralised so all tree reads go through the same error
    ///   path.
    fn load_tree(&self, tree_id: &ObjectHash) -> Result<Vec<TreeItem>> {
        let data = read_git_object(&self.repo_path, tree_id)?;

        let tree = Tree::from_bytes(&data, *tree_id)?;
        Ok(tree.tree_items)
    }

    /// Serialise tree items into Git's binary tree format and persist as
    /// an object.
    ///
    /// Functional scope:
    /// - Encodes each item as `<mode> <name>\0<binary_hash>` per the Git
    ///   tree spec, concatenates them in caller-provided order, and writes
    ///   the bytes to the object database under type `tree`.
    ///
    /// Boundary conditions:
    /// - Items must already be sorted by the caller (`append`/the splice
    ///   helpers do this). Unsorted items would still parse but would
    ///   produce a different tree hash than canonical Git.
    /// - Rejects hashes whose binary length is not 20 (SHA-1) or 32
    ///   (SHA-256) — protection against malformed inputs that would
    ///   otherwise corrupt the object store.
    fn write_tree(&self, tree_items: &[TreeItem]) -> Result<ObjectHash> {
        Ok(self.write_tree_with_size(tree_items)?.0)
    }

    /// Encode `tree_items` as a Git tree, write the object, and return
    /// `(hash, encoded_size)`. The size is the *content* length (no Git
    /// header) — same convention as `object_index.o_size`.
    ///
    /// Used by the agent capture path (Phase 3.5c) which needs the byte
    /// count to pair with [`crate::utils::client_storage::enqueue_agent_blob_object_index_update`].
    /// All other callers go through [`Self::write_tree`] and discard the
    /// size.
    fn write_tree_with_size(&self, tree_items: &[TreeItem]) -> Result<(ObjectHash, usize)> {
        let mut data = Vec::new();
        for item in tree_items {
            let mode_str = match item.mode {
                TreeItemMode::Tree => "40000",
                TreeItemMode::Blob => "100644",
                TreeItemMode::BlobExecutable => "100755",
                TreeItemMode::Link => "120000",
                TreeItemMode::Commit => "160000",
            };
            data.extend_from_slice(mode_str.as_bytes());
            data.push(b' ');
            data.extend_from_slice(item.name.as_bytes());
            data.push(0);
            let hash_hex = item.id.to_string();
            let hash_bytes =
                hex::decode(&hash_hex).map_err(|e| anyhow!("Invalid hash hex: {}", e))?;
            // 20 bytes for SHA-1, 32 for SHA-256. Anything else is a
            // signal that we are about to corrupt the object database.
            if hash_bytes.len() != 20 && hash_bytes.len() != 32 {
                return Err(anyhow!("Invalid object hash length: {}", hash_bytes.len()));
            }
            data.extend_from_slice(&hash_bytes);
        }

        let size = data.len();
        let hash = write_git_object(&self.repo_path, "tree", &data)?;
        Ok((hash, size))
    }

    /// Write a tree object and stamp it into `object_index` with the
    /// given `o_type`. Used by the agent capture path so cloud sync
    /// uploads the trees that compose `refs/libra/traces`.
    fn write_tree_indexed(&self, tree_items: &[TreeItem], o_type: &str) -> Result<ObjectHash> {
        let (hash, size) = self.write_tree_with_size(tree_items)?;
        crate::utils::client_storage::enqueue_agent_blob_object_index_update(
            &self.repo_path,
            &hash.to_string(),
            o_type,
            size as i64,
        );
        Ok(hash)
    }

    fn create_append_commit(
        &self,
        parent_commit_id: Option<ObjectHash>,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<ObjectHash> {
        let mut root_items = if let Some(parent_id) = parent_commit_id {
            self.load_commit_tree(&parent_id)?
        } else {
            Vec::new()
        };

        let type_tree_entry = root_items
            .iter()
            .find(|item| item.name == object_type)
            .cloned();

        let mut type_items = if let Some(entry) = type_tree_entry {
            self.load_tree(&entry.id)?
        } else {
            Vec::new()
        };

        let new_item = TreeItem::new(TreeItemMode::Blob, blob_hash, object_id.to_string());
        type_items.retain(|item| item.name != object_id);
        type_items.push(new_item);
        type_items.sort_by(|a, b| a.name.cmp(&b.name));

        let type_tree_hash = self.write_tree(&type_items)?;

        let new_root_item =
            TreeItem::new(TreeItemMode::Tree, type_tree_hash, object_type.to_string());
        root_items.retain(|item| item.name != object_type);
        root_items.push(new_root_item);
        root_items.sort_by(|a, b| a.name.cmp(&b.name));

        let root_tree_hash = self.write_tree(&root_items)?;

        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "history@libra".to_string(),
        );

        let signature = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "history@libra".to_string(),
        );

        let message = format!("Update {}/{}", object_type, object_id);
        let parents = parent_commit_id.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(author, signature, root_tree_hash, parents, &message);
        let commit_data = commit
            .to_data()
            .context("Failed to serialize AI history commit")?;
        write_git_object(&self.repo_path, "commit", &commit_data)
            .context("Failed to write AI history commit")
    }

    async fn update_ref(&self, ref_name: &str, hash: ObjectHash) -> Result<()> {
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction = match self.db_conn.begin().await {
                Ok(txn) => txn,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to begin transaction"),
            };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query reference"),
            };

            let had_existing = existing.is_some();
            let write_result = if let Some(model) = existing {
                let mut active: reference::ActiveModel = model.into();
                active.commit = Set(Some(hash.to_string()));
                active.update(&txn).await.map(|_| ())
            } else {
                let new_ref = reference::ActiveModel {
                    name: Set(Some(ref_name.to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some(hash.to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                new_ref.insert(&txn).await.map(|_| ())
            };

            match write_result {
                Ok(()) => {}
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => {
                    let context = if had_existing {
                        "Failed to update reference"
                    } else {
                        "Failed to insert reference"
                    };
                    return Err(err).context(context);
                }
            }

            match txn.commit().await {
                Ok(()) => return Ok(()),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to commit transaction"),
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    async fn update_ref_if_matches(
        &self,
        ref_name: &str,
        expected_head: Option<ObjectHash>,
        new_hash: ObjectHash,
    ) -> Result<RefUpdateOutcome> {
        self.update_ref_if_matches_with_extra(ref_name, expected_head, new_hash, None)
            .await
    }

    /// Conditional ref update with optional transactional companion writes
    /// (plan-20260713 ADR-DR-10). When `extra` is provided, its SQL runs in
    /// the SAME transaction as the ref write — after the CAS row update
    /// succeeds, before COMMIT — so catalog/claim/revision state can never
    /// diverge from the ref. An `extra` error rolls the whole transaction
    /// back (the ref does not move) and propagates as a hard error, not a
    /// `HeadChanged` retry.
    async fn update_ref_if_matches_with_extra(
        &self,
        ref_name: &str,
        expected_head: Option<ObjectHash>,
        new_hash: ObjectHash,
        extra: Option<(&dyn TracesTxnExtra, &TracesCommitCtx)>,
    ) -> Result<RefUpdateOutcome> {
        let expected_commit = expected_head.map(|hash| hash.to_string());
        let new_commit = new_hash.to_string();

        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction = match self.db_conn.begin().await {
                Ok(txn) => txn,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to begin transaction"),
            };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query reference"),
            };

            let write_result = match existing {
                Some(model) if model.commit != expected_commit => {
                    let _ = txn.rollback().await;
                    return Ok(RefUpdateOutcome::HeadChanged);
                }
                Some(model) => {
                    let mut update = reference::Entity::update_many()
                        .filter(reference::Column::Id.eq(model.id))
                        .filter(reference::Column::Name.eq(ref_name))
                        .filter(reference::Column::Kind.eq(ConfigKind::Branch));
                    update = match expected_commit.as_ref() {
                        Some(commit) => update.filter(reference::Column::Commit.eq(commit.clone())),
                        None => update.filter(reference::Column::Commit.is_null()),
                    };

                    update
                        .col_expr(
                            reference::Column::Commit,
                            Expr::value(Some(new_commit.clone())),
                        )
                        .exec(&txn)
                        .await
                        .map(Some)
                }
                None if expected_commit.is_some() => {
                    let _ = txn.rollback().await;
                    return Ok(RefUpdateOutcome::HeadChanged);
                }
                None => {
                    let new_ref = reference::ActiveModel {
                        name: Set(Some(ref_name.to_string())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(Some(new_commit.clone())),
                        remote: Set(None),
                        ..Default::default()
                    };
                    match new_ref.insert(&txn).await {
                        Ok(_) => Ok(None),
                        Err(err) if is_sqlite_unique_violation(&err) => {
                            let _ = txn.rollback().await;
                            return Ok(RefUpdateOutcome::HeadChanged);
                        }
                        Err(err) => Err(err),
                    }
                }
            };

            let rows_affected = match write_result {
                Ok(rows_affected) => rows_affected,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to compare-and-swap history head"),
            };

            if rows_affected.is_some_and(|result| result.rows_affected != 1) {
                let _ = txn.rollback().await;
                return Ok(RefUpdateOutcome::HeadChanged);
            }

            // ADR-DR-10: companion writes ride the ref transaction. A
            // failure here must NOT move the ref — roll back and fail
            // closed (no HeadChanged retry: the failure is a gate/fence
            // violation or DB fault, not a CAS race).
            if let Some((extra, ctx)) = extra
                && let Err(err) = extra.apply(&txn, ctx).await
            {
                let _ = txn.rollback().await;
                return Err(
                    err.context("transactional companion writes failed; ref update rolled back")
                );
            }

            match txn.commit().await {
                Ok(()) => return Ok(RefUpdateOutcome::Updated),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to commit transaction"),
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    /// Append a checkpoint commit to this manager's ref.
    ///
    /// AG-20 (E4-libra layout). Builds the layered tree
    ///
    /// ```text
    /// checkpoint/<id[:2]>/<id[2:]>/
    ///   metadata.json
    ///   manifest.json
    ///   events/lifecycle.jsonl
    ///   transcript/<agent_kind>.jsonl        (or `.jsonl.001…` chunks, E5)
    ///   redaction_report.json
    ///   content_hash.txt
    /// ```
    ///
    /// and merges it into the parent commit's tree so successive checkpoints
    /// accumulate (rather than overwrite). The resulting commit message
    /// carries `Libra-*` trailers per the design spec (see
    /// `docs/development/commands/_general.md` §3.3). Pre-AG-20 checkpoints
    /// (metadata.json + `transcript/<provider>` only) remain readable as
    /// legacy-v1; this writer never emits that layout again.
    ///
    /// Returns the freshly-written commit hash plus the OIDs callers need to
    /// stamp onto `agent_checkpoint` (root tree OID and metadata blob OID),
    /// along with span bookkeeping (`cas_retries`, `object_count`).
    pub async fn append_checkpoint_commit(
        &self,
        params: CheckpointCommitParams<'_>,
    ) -> Result<CheckpointCommit> {
        // Phase 1: write content blobs once. They are content-addressed, so
        // re-running a CAS retry loop never duplicates them.
        //
        // CEX-EntireIO §14.3 phase-3 item 3: every agent blob is tagged in
        // `object_index` so `libra cloud sync` uploads it to R2. Only the
        // transcript blob(s) carry the distinguished o_type
        // ("agent_transcript"); the JSON sidecars use the standard "blob"
        // tag because cloud sync doesn't filter by o_type — the custom tag
        // exists for downstream tooling that enumerates captured
        // transcripts.
        let mut object_count: u64 = 0;
        let mut write_indexed_blob =
            |bytes: &[u8], o_type: &str, what: &str| -> Result<ObjectHash> {
                let oid = write_git_object(&self.repo_path, "blob", bytes)
                    .with_context(|| format!("failed to write checkpoint {what} blob"))?;
                crate::utils::client_storage::enqueue_agent_blob_object_index_update(
                    &self.repo_path,
                    &oid.to_string(),
                    o_type,
                    bytes.len() as i64,
                );
                object_count += 1;
                Ok(oid)
            };

        let metadata_blob_oid =
            write_indexed_blob(params.metadata_json.bytes(), "blob", "metadata.json")?;
        let events_blob_oid = write_indexed_blob(
            params.lifecycle_events_jsonl.bytes(),
            "blob",
            "events/lifecycle.jsonl",
        )?;
        let report_blob_oid = write_indexed_blob(
            params.redaction_report_json.bytes(),
            "blob",
            "redaction_report.json",
        )?;

        // Transcript: E5 line-boundary-safe chunking above the threshold.
        // Small transcripts stay a single `transcript/<agent_kind>.jsonl`
        // file; larger ones split into `.jsonl.001`, `.jsonl.002`, … parts
        // declared (in order) by the manifest's `transcript` role.
        let transcript_bytes = params.transcript_redacted.bytes();
        let threshold = transcript_chunk_threshold();
        let transcript_file_name = format!("{}.jsonl", params.agent_kind);
        let chunks: Vec<&[u8]> = if transcript_bytes.len() > threshold {
            chunk_transcript_line_safe(transcript_bytes, threshold)?
        } else {
            vec![transcript_bytes]
        };
        let chunked = chunks.len() > 1;
        let mut transcript_parts: Vec<TranscriptPartRef> = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            let name = if chunked {
                format!("{}.{:03}", transcript_file_name, index + 1)
            } else {
                transcript_file_name.clone()
            };
            let oid = write_indexed_blob(chunk, "agent_transcript", "transcript")?;
            transcript_parts.push(TranscriptPartRef {
                name,
                oid,
                byte_len: chunk.len(),
            });
        }

        // content_hash.txt: `sha256:<64-lowercase-hex>` (no trailing
        // newline) over the concatenated bytes of the coverage roles in
        // [`CHECKPOINT_CONTENT_HASH_COVERAGE`] order. The transcript
        // contributes its logical (pre-chunking) byte stream, so the hash
        // is invariant under re-chunking. See the E4-libra section of
        // `docs/development/tracing/agent.md`.
        let content_hash = checkpoint_content_hash(&[
            params.metadata_json.bytes(),
            params.lifecycle_events_jsonl.bytes(),
            transcript_bytes,
            params.redaction_report_json.bytes(),
        ]);
        let content_hash_blob_oid =
            write_indexed_blob(content_hash.as_bytes(), "blob", "content_hash.txt")?;

        // manifest.json is written LAST among the blobs: it declares every
        // other entry's OID/length (including content_hash.txt), so nothing
        // can hash or list the manifest itself without circularity.
        let manifest_bytes = build_checkpoint_manifest_json(
            params.checkpoint_id,
            &transcript_file_name,
            ManifestBlobRef::new(metadata_blob_oid, params.metadata_json.len()),
            ManifestBlobRef::new(events_blob_oid, params.lifecycle_events_jsonl.len()),
            &transcript_parts,
            transcript_bytes.len(),
            ManifestBlobRef::new(report_blob_oid, params.redaction_report_json.len()),
            ManifestBlobRef::new(content_hash_blob_oid, content_hash.len()),
        )?;
        let manifest_blob_oid = write_indexed_blob(&manifest_bytes, "blob", "manifest.json")?;

        // Phase 2: build the leaf trees (transcript/, events/).
        // All trees written under the agent capture path go through
        // `write_tree_indexed` so they reach `object_index` and the
        // standard cloud sync path; otherwise the orphan ref's commits
        // would dereference to missing trees on a fresh `cloud restore`.
        let mut transcript_items: Vec<TreeItem> = transcript_parts
            .iter()
            .map(|part| TreeItem::new(TreeItemMode::Blob, part.oid, part.name.clone()))
            .collect();
        transcript_items.sort_by(|a, b| a.name.cmp(&b.name));
        let transcript_subtree = self.write_tree_indexed(&transcript_items, "tree")?;
        let events_subtree = self.write_tree_indexed(
            &[TreeItem::new(
                TreeItemMode::Blob,
                events_blob_oid,
                CHECKPOINT_LIFECYCLE_EVENTS_FILE.to_string(),
            )],
            "tree",
        )?;
        object_count += 2;

        let mut inner_items = vec![
            TreeItem::new(
                TreeItemMode::Blob,
                metadata_blob_oid,
                "metadata.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                manifest_blob_oid,
                "manifest.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                report_blob_oid,
                "redaction_report.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                content_hash_blob_oid,
                "content_hash.txt".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Tree,
                transcript_subtree,
                "transcript".to_string(),
            ),
            TreeItem::new(TreeItemMode::Tree, events_subtree, "events".to_string()),
        ];
        inner_items.sort_by(|a, b| a.name.cmp(&b.name));
        let inner_tree = self.write_tree_indexed(&inner_items, "tree")?;
        object_count += 1;

        // Phase 3: CAS loop. Read parent, splice
        // `checkpoint/<prefix>/<rest>` into its tree, write the new commit,
        // and update the ref atomically. Retries on head conflict, mirroring
        // the existing `append` flow.
        let prefix = params
            .checkpoint_id
            .get(..2)
            .ok_or_else(|| anyhow!("checkpoint_id must be at least 2 characters"))?
            .to_string();
        let rest = params.checkpoint_id[2..].to_string();
        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            let parent = self.resolve_history_head().await?;
            let new_root = self.splice_checkpoint_tree(parent, &prefix, &rest, inner_tree)?;
            // splice_checkpoint_tree writes exactly three trees
            // (rest→prefix→checkpoint→root splice) per attempt; +1 commit.
            object_count += 4;

            let trailer = format_libra_trailers(&params);
            let message = format!(
                "traces: {} checkpoint {}\n\n{trailer}",
                params.scope.as_str(),
                params.checkpoint_id,
            );
            let author = Signature::new(
                SignatureType::Author,
                "Libra".to_string(),
                "traces@libra".to_string(),
            );
            let committer = Signature::new(
                SignatureType::Committer,
                "Libra".to_string(),
                "traces@libra".to_string(),
            );
            let parents = parent.into_iter().collect::<Vec<_>>();
            let commit = Commit::new(author, committer, new_root, parents, &message);
            let commit_data = commit
                .to_data()
                .context("failed to serialize checkpoint commit")?;
            let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
            // Phase 3.5c: agent capture commits must reach R2 too.
            // Tagging at every CAS retry is idempotent because
            // `update_object_index_once` does an existence check before
            // inserting, and the same `commit_data` produces the same
            // OID across retries.
            crate::utils::client_storage::enqueue_agent_blob_object_index_update(
                &self.repo_path,
                &commit_hash.to_string(),
                "commit",
                commit_data.len() as i64,
            );

            // Per-attempt ctx: commit hash and root tree change on every CAS
            // rebuild, so the companion writes get the values of THIS attempt.
            let commit_ctx = TracesCommitCtx {
                commit_hash: commit_hash.to_string(),
                tree_oid: new_root.to_string(),
                metadata_blob_oid: metadata_blob_oid.to_string(),
            };
            match self
                .update_ref_if_matches_with_extra(
                    &self.ref_name,
                    parent,
                    commit_hash,
                    params.txn_extra.map(|extra| (extra, &commit_ctx)),
                )
                .await?
            {
                RefUpdateOutcome::Updated => {
                    return Ok(CheckpointCommit {
                        commit_hash,
                        tree_oid: new_root,
                        metadata_blob_oid,
                        cas_retries: attempt as u64,
                        object_count,
                    });
                }
                RefUpdateOutcome::HeadChanged if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES => {
                    continue;
                }
                RefUpdateOutcome::HeadChanged => {
                    return Err(anyhow!(
                        "history head changed repeatedly while appending checkpoint {}",
                        params.checkpoint_id
                    ));
                }
            }
        }
        unreachable!("CAS retry loop must return on success or terminal error")
    }

    /// Remove checkpoint commits from this manager's ref and delete their
    /// `agent_checkpoint` rows.
    ///
    /// This is the `libra agent clean` counterpart to
    /// [`Self::append_checkpoint_commit`]. It rewrites the orphan
    /// `refs/libra/traces` chain from the checkpoint catalog, omitting
    /// the supplied checkpoint IDs. Rewriting is necessary because later
    /// committed checkpoints may descend from temporary checkpoints; simply
    /// moving the ref to an ancestor would either keep those temporary commits
    /// reachable or discard later retained checkpoints.
    ///
    /// Repositories that only have catalog rows and an empty traces ref
    /// (older fixtures, partial migrations, or pre-Phase-2 data) still get the
    /// catalog deletion without a ref rewrite.
    pub async fn prune_checkpoint_commits(
        &self,
        checkpoint_ids_to_remove: &[String],
    ) -> Result<CheckpointPruneOutcome> {
        // AG-20 observability (`agent.md` §6): one `agent.clean.prune` span
        // per prune. Required fields: deleted_objects, deleted_sessions,
        // window_guard, duration_ms. No raw filesystem path is ever
        // recorded (forbidden: raw path outside repo).
        let prune_span = tracing::info_span!(
            "agent.clean.prune",
            deleted_objects = tracing::field::Empty,
            deleted_sessions = tracing::field::Empty,
            window_guard = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
        );
        let started = std::time::Instant::now();
        let finish_span = |guard: &'static str, deleted_objects: u64| {
            prune_span.record("deleted_objects", deleted_objects);
            // The prune never deletes `agent_session` rows (sessions are
            // retained for history; only checkpoint rows are dropped).
            prune_span.record("deleted_sessions", 0_u64);
            prune_span.record("window_guard", guard);
            prune_span.record("duration_ms", started.elapsed().as_millis() as u64);
        };

        let remove_set: HashSet<&str> = checkpoint_ids_to_remove
            .iter()
            .map(String::as_str)
            .collect();
        if remove_set.is_empty() {
            finish_span("noop", 0);
            return Ok(CheckpointPruneOutcome {
                removed_checkpoints: 0,
                rewritten_checkpoints: 0,
                ref_rewritten: false,
                window_guard: "noop",
                deleted_object_index_rows: 0,
            });
        }

        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            let expected_head = self.resolve_history_head().await?;
            let rows = self.load_checkpoint_history_rows().await?;
            let existing_remove_ids = rows
                .iter()
                .filter(|row| remove_set.contains(row.checkpoint_id.as_str()))
                .map(|row| row.checkpoint_id.clone())
                .collect::<HashSet<_>>();

            if existing_remove_ids.is_empty() {
                finish_span("noop", 0);
                return Ok(CheckpointPruneOutcome {
                    removed_checkpoints: 0,
                    rewritten_checkpoints: 0,
                    ref_rewritten: false,
                    window_guard: "noop",
                    deleted_object_index_rows: 0,
                });
            }

            // AG-20 window A/B guards — both must pass before any rewrite.
            if let Err(guard_err) = self.enforce_prune_window_guards(expected_head, &rows).await {
                let guard_label = match guard_err.downcast_ref::<CheckpointPruneGuardError>() {
                    Some(CheckpointPruneGuardError::LiveWriterMarker { .. }) => {
                        "live_marker_blocked"
                    }
                    Some(CheckpointPruneGuardError::RefCatalogOrphans { .. }) => {
                        "catalog_orphans_blocked"
                    }
                    // A guard that cannot complete (unreadable chain,
                    // marker-listing failure) still fails the prune closed.
                    None => "guard_check_failed",
                };
                finish_span(guard_label, 0);
                return Err(guard_err);
            }

            let (retained_rows, removed_rows): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|row| !existing_remove_ids.contains(&row.checkpoint_id));

            let (new_head, rewritten) = match expected_head {
                Some(head) => self.rebuild_checkpoint_history(head, &retained_rows)?,
                None => (None, Vec::new()),
            };

            let unreachable_oids =
                collect_exclusive_unreachable_oids(&removed_rows, &retained_rows, &rewritten);

            match self
                .commit_checkpoint_prune(
                    expected_head,
                    new_head,
                    &rewritten,
                    &existing_remove_ids,
                    &unreachable_oids,
                )
                .await?
            {
                (RefUpdateOutcome::Updated, removed_checkpoints, deleted_object_index_rows) => {
                    finish_span("markers_and_catalog_verified", deleted_object_index_rows);
                    return Ok(CheckpointPruneOutcome {
                        removed_checkpoints,
                        rewritten_checkpoints: rewritten.len(),
                        ref_rewritten: expected_head != new_head,
                        window_guard: "markers_and_catalog_verified",
                        deleted_object_index_rows,
                    });
                }
                (RefUpdateOutcome::HeadChanged, _, _)
                    if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES =>
                {
                    continue;
                }
                (RefUpdateOutcome::HeadChanged, _, _) => {
                    return Err(anyhow!(
                        "traces head changed repeatedly while pruning checkpoints"
                    ));
                }
            }
        }

        unreachable!("checkpoint prune retry loop must return on success or terminal error")
    }

    /// AG-24a local erasure for one session (plan.md Task A8.5): make the
    /// three local faces consistent — rewrite `refs/libra/traces` to drop
    /// the session's checkpoints, delete its `agent_checkpoint` and
    /// `agent_session` rows, and clean the now-unreachable `object_index`
    /// rows. The append-only `agent_audit_log` is a separate table and is
    /// never touched.
    ///
    /// Order matters: checkpoints are pruned FIRST (while the catalog rows
    /// still exist, so the ref rewrite can enumerate what to keep), then
    /// the `agent_session` row is deleted. Deleting the session first would
    /// cascade its checkpoint rows away (FK `ON DELETE CASCADE`) and leave
    /// `refs/libra/traces` pointing at orphan commits.
    ///
    /// D1/R2 cloud-mirror deletion propagation is explicitly out of scope
    /// (documented deferral): this covers local consistency only.
    pub async fn erase_session_local(&self, session_id: &str) -> Result<SessionEraseOutcome> {
        use sea_orm::{Statement, Value};
        let backend = self.db_conn.get_database_backend();

        // Enumerate the session's checkpoints from the catalog.
        let rows = self
            .db_conn
            .query_all(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id FROM agent_checkpoint WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("list checkpoints for session erasure")?;
        let checkpoint_ids: Vec<String> = rows
            .into_iter()
            .map(|row| row.try_get_by::<String, _>("checkpoint_id"))
            .collect::<std::result::Result<_, _>>()
            .context("decode checkpoint_id for session erasure")?;

        // Prune the checkpoints (ref rewrite + row + object_index) BEFORE
        // deleting the session row.
        let prune = self.prune_checkpoint_commits(&checkpoint_ids).await?;

        // Delete the session row (cascades any residual checkpoint rows).
        let deleted = self
            .db_conn
            .execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_session WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("delete agent_session row for erasure")?;

        Ok(SessionEraseOutcome {
            session_deleted: deleted.rows_affected() > 0,
            removed_checkpoints: prune.removed_checkpoints,
            ref_rewritten: prune.ref_rewritten,
            deleted_object_index_rows: prune.deleted_object_index_rows,
        })
    }

    /// AG-20 window A/B prune guards (`agent.md` write-sequence matrix,
    /// rows 727-732). Both refusals are deterministic and fail-closed:
    ///
    /// - **Window A/B (live writer)**: any live in-flight marker — for ANY
    ///   session, not just the ones being pruned — blocks the prune. The
    ///   prune is a whole-chain rewrite of the shared `refs/libra/traces`
    ///   ref plus the catalog, so a concurrent writer between stages
    ///   (a)–(d) could otherwise lose loose objects (window A) or a
    ///   ref-reachable-but-uncataloged commit (window B). Markers carry a
    ///   TTL ([`AGENT_TRACES_INFLIGHT_TTL_MS`]), so a crashed writer only
    ///   defers pruning temporarily.
    /// - **Window B residue (ref-vs-catalog)**: walks the first-parent
    ///   chain of the current traces head and refuses when any reachable
    ///   commit has no `agent_checkpoint.traces_commit` row. The rebuild is
    ///   catalog-driven and would silently drop such commits; backfilling
    ///   the catalog is `libra agent doctor --repair`'s job.
    async fn enforce_prune_window_guards(
        &self,
        expected_head: Option<ObjectHash>,
        rows: &[CheckpointHistoryRow],
    ) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        // A marker-listing failure cannot prove the absence of a live
        // writer — propagate it, which aborts (fails closed) the prune.
        let live_markers = list_live_traces_inflight_markers(self.db_conn.as_ref(), now_ms)
            .await
            .context("failed to verify traces in-flight markers (prune fails closed)")?;
        if let Some(marker) = live_markers.first() {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: marker.session_id.clone(),
                attempt_id: marker.attempt_id.clone(),
                ttl_ms: marker.ttl_ms,
            }
            .into());
        }

        let Some(head) = expected_head else {
            return Ok(());
        };
        let cataloged: HashSet<&str> = rows
            .iter()
            .filter_map(|row| row.traces_commit.as_deref())
            .collect();
        // An unreadable chain means the catalog cannot be verified —
        // propagate the walk error (fail closed) rather than pruning blind.
        let mut orphans: Vec<String> = Vec::new();
        for commit_hash in self.first_parent_commit_hashes(head)? {
            if !cataloged.contains(commit_hash.as_str()) {
                orphans.push(commit_hash);
            }
        }
        if let Some(first_commit) = orphans.first().cloned() {
            return Err(CheckpointPruneGuardError::RefCatalogOrphans {
                orphan_count: orphans.len(),
                first_commit,
            }
            .into());
        }
        Ok(())
    }

    /// First-parent commit hashes reachable from `head` (head first),
    /// with a visited-set cycle guard.
    fn first_parent_commit_hashes(&self, head: ObjectHash) -> Result<Vec<String>> {
        let mut hashes = Vec::new();
        let mut visited: HashSet<ObjectHash> = HashSet::new();
        let mut next = Some(head);
        while let Some(oid) = next {
            if !visited.insert(oid) {
                break;
            }
            let data = read_git_object(&self.repo_path, &oid).with_context(|| {
                format!("failed to read traces commit {oid} while walking refs/libra/traces")
            })?;
            let commit = Commit::from_bytes(&data, oid)
                .map_err(|err| anyhow!("failed to parse traces commit {oid}: {err}"))?;
            hashes.push(oid.to_string());
            next = commit.parent_commit_ids.first().copied();
        }
        Ok(hashes)
    }

    async fn load_checkpoint_history_rows(&self) -> Result<Vec<CheckpointHistoryRow>> {
        let backend = self.db_conn.get_database_backend();
        let rows = self
            .db_conn
            .query_all(Statement::from_string(
                backend,
                "SELECT cp.checkpoint_id, cp.session_id, cp.scope, cp.parent_commit, \
                        cp.traces_commit, cp.tree_oid, cp.metadata_blob_oid, cp.created_at, \
                        COALESCE(s.agent_kind, 'unknown') AS agent_kind \
                 FROM agent_checkpoint cp \
                 LEFT JOIN agent_session s ON s.session_id = cp.session_id \
                 ORDER BY cp.created_at ASC, cp.checkpoint_id ASC"
                    .to_string(),
            ))
            .await
            .context("failed to load agent_checkpoint rows for traces rewrite")?;

        rows.into_iter()
            .map(CheckpointHistoryRow::from_query_result)
            .collect()
    }

    fn rebuild_checkpoint_history(
        &self,
        current_head: ObjectHash,
        retained_rows: &[CheckpointHistoryRow],
    ) -> Result<(Option<ObjectHash>, Vec<RewrittenCheckpoint>)> {
        if retained_rows.is_empty() {
            return Ok((None, Vec::new()));
        }

        let current_root = self.load_commit_tree(&current_head)?;
        let mut parent = None;
        let mut rewritten = Vec::with_capacity(retained_rows.len());

        for row in retained_rows {
            let inner_tree = self
                .checkpoint_inner_tree_from_root(&current_root, &row.checkpoint_id)?
                .ok_or_else(|| {
                    anyhow!(
                        "traces tree is missing retained checkpoint {}",
                        row.checkpoint_id
                    )
                })?;
            let (prefix, rest) = checkpoint_tree_path(&row.checkpoint_id)?;
            let root_tree = self.splice_checkpoint_tree(parent, &prefix, &rest, inner_tree)?;
            let commit_hash = self.write_rewritten_checkpoint_commit(parent, root_tree, row)?;
            rewritten.push(RewrittenCheckpoint {
                checkpoint_id: row.checkpoint_id.clone(),
                traces_commit: commit_hash,
                tree_oid: root_tree,
            });
            parent = Some(commit_hash);
        }

        Ok((parent, rewritten))
    }

    fn checkpoint_inner_tree_from_root(
        &self,
        root_items: &[TreeItem],
        checkpoint_id: &str,
    ) -> Result<Option<ObjectHash>> {
        let (prefix, rest) = checkpoint_tree_path(checkpoint_id)?;
        let Some(checkpoint_entry) = root_items.iter().find(|item| item.name == "checkpoint")
        else {
            return Ok(None);
        };
        if checkpoint_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint' entry expected to be a tree, got mode {:?}",
                checkpoint_entry.mode
            ));
        }

        let checkpoint_items = self.load_tree(&checkpoint_entry.id)?;
        let Some(prefix_entry) = checkpoint_items.iter().find(|item| item.name == prefix) else {
            return Ok(None);
        };
        if prefix_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a tree, got mode {:?}",
                prefix_entry.mode
            ));
        }

        let prefix_items = self.load_tree(&prefix_entry.id)?;
        let Some(rest_entry) = prefix_items.iter().find(|item| item.name == rest) else {
            return Ok(None);
        };
        if rest_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint/{prefix}/{rest}' entry expected to be a tree, got mode {:?}",
                rest_entry.mode
            ));
        }
        Ok(Some(rest_entry.id))
    }

    fn write_rewritten_checkpoint_commit(
        &self,
        parent: Option<ObjectHash>,
        root_tree: ObjectHash,
        row: &CheckpointHistoryRow,
    ) -> Result<ObjectHash> {
        let message = format!(
            "traces: {} checkpoint {}\n\n{}",
            row.scope,
            row.checkpoint_id,
            format_rewritten_checkpoint_trailers(row)
        );
        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "traces@libra".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "traces@libra".to_string(),
        );
        let parents = parent.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(author, committer, root_tree, parents, &message);
        let commit_data = commit
            .to_data()
            .context("failed to serialize rewritten checkpoint commit")?;
        let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
        crate::utils::client_storage::enqueue_agent_blob_object_index_update(
            &self.repo_path,
            &commit_hash.to_string(),
            "commit",
            commit_data.len() as i64,
        );
        Ok(commit_hash)
    }

    /// Transactionally CAS the traces ref, update rewritten rows, delete
    /// pruned rows, and drop `object_index` rows for
    /// `unreachable_oids` (the conservative exclusively-removed set from
    /// [`collect_exclusive_unreachable_oids`]). The `object_index` deletion
    /// is idempotent — re-running deletes nothing — and rides in the same
    /// transaction so a crash cannot leave the catalog and the index
    /// disagreeing about the pruned checkpoints.
    ///
    /// Returns `(outcome, removed_rows, deleted_object_index_rows)`.
    async fn commit_checkpoint_prune(
        &self,
        expected_head: Option<ObjectHash>,
        new_head: Option<ObjectHash>,
        rewritten: &[RewrittenCheckpoint],
        remove_ids: &HashSet<String>,
        unreachable_oids: &[String],
    ) -> Result<(RefUpdateOutcome, u64, u64)> {
        let expected_commit = expected_head.map(|hash| hash.to_string());
        let new_commit = new_head.map(|hash| hash.to_string());

        'retry_sqlite: for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction = match self.db_conn.begin().await {
                Ok(txn) => txn,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => {
                    return Err(err).context("Failed to begin checkpoint prune transaction");
                }
            };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(&self.ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query checkpoint prune ref"),
            };

            let write_ref = match existing {
                Some(model) if model.commit != expected_commit => {
                    let _ = txn.rollback().await;
                    return Ok((RefUpdateOutcome::HeadChanged, 0, 0));
                }
                Some(model) => {
                    let mut active: reference::ActiveModel = model.into();
                    active.commit = Set(new_commit.clone());
                    active.update(&txn).await.map(|_| ())
                }
                None if expected_commit.is_some() => {
                    let _ = txn.rollback().await;
                    return Ok((RefUpdateOutcome::HeadChanged, 0, 0));
                }
                None => {
                    let new_ref = reference::ActiveModel {
                        name: Set(Some(self.ref_name.clone())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(new_commit.clone()),
                        remote: Set(None),
                        ..Default::default()
                    };
                    new_ref.insert(&txn).await.map(|_| ())
                }
            };

            if let Err(err) = write_ref {
                if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                return Err(err).context("Failed to update checkpoint prune ref");
            }

            let backend = txn.get_database_backend();
            for item in rewritten {
                if let Err(err) = txn
                    .execute(Statement::from_sql_and_values(
                        backend,
                        "UPDATE agent_checkpoint SET traces_commit = ?, tree_oid = ? \
                         WHERE checkpoint_id = ?",
                        vec![
                            Value::from(item.traces_commit.to_string()),
                            Value::from(item.tree_oid.to_string()),
                            Value::from(item.checkpoint_id.clone()),
                        ],
                    ))
                    .await
                {
                    if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    return Err(err).context("Failed to update rewritten checkpoint row");
                }
            }

            let mut removed = 0;
            for id in remove_ids {
                match txn
                    .execute(Statement::from_sql_and_values(
                        backend,
                        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
                        [Value::from(id.clone())],
                    ))
                    .await
                {
                    Ok(result) => removed += result.rows_affected(),
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    Err(err) => return Err(err).context("Failed to delete pruned checkpoint row"),
                }
            }

            // AG-20: drop `object_index` rows for OIDs this prune made
            // unreachable so cloud sync stops advertising them. Rides in
            // the same transaction; idempotent (missing rows delete 0).
            let deleted_object_index_rows =
                match crate::utils::client_storage::remove_object_index_rows_with_conn(
                    &txn,
                    unreachable_oids,
                )
                .await
                {
                    Ok(count) => count,
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    Err(err) => {
                        return Err(err).context("Failed to delete pruned object_index rows");
                    }
                };

            match txn.commit().await {
                Ok(()) => {
                    return Ok((
                        RefUpdateOutcome::Updated,
                        removed,
                        deleted_object_index_rows,
                    ));
                }
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => {
                    return Err(err).context("Failed to commit checkpoint prune transaction");
                }
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    /// Splice `inner_tree` into `parent`'s tree at the path
    /// `checkpoint/<prefix>/<rest>`, preserving any existing entries in the
    /// surrounding subtrees. Phase 2.1 helper for [`append_checkpoint_commit`].
    fn splice_checkpoint_tree(
        &self,
        parent: Option<ObjectHash>,
        prefix: &str,
        rest: &str,
        inner_tree: ObjectHash,
    ) -> Result<ObjectHash> {
        let mut root_items = match parent {
            Some(parent_id) => self.load_commit_tree(&parent_id)?,
            None => Vec::new(),
        };
        let checkpoint_entry = root_items
            .iter()
            .find(|item| item.name == "checkpoint")
            .cloned();
        let mut checkpoint_items = match checkpoint_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => self.load_tree(&entry.id)?,
            Some(entry) => {
                return Err(anyhow!(
                    "traces tree corruption: 'checkpoint' entry expected to be a tree, \
                     got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                ));
            }
            None => Vec::new(),
        };

        let prefix_entry = checkpoint_items
            .iter()
            .find(|item| item.name == prefix)
            .cloned();
        let mut prefix_items = match prefix_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => self.load_tree(&entry.id)?,
            Some(entry) => {
                return Err(anyhow!(
                    "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a \
                     tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                ));
            }
            None => Vec::new(),
        };

        prefix_items.retain(|item| item.name != rest);
        prefix_items.push(TreeItem::new(
            TreeItemMode::Tree,
            inner_tree,
            rest.to_string(),
        ));
        prefix_items.sort_by(|a, b| a.name.cmp(&b.name));
        // Phase 3.5c: tag every tree spliced into the agent capture
        // history so cloud sync uploads the full reachability set.
        let prefix_tree = self.write_tree_indexed(&prefix_items, "tree")?;

        checkpoint_items.retain(|item| item.name != prefix);
        checkpoint_items.push(TreeItem::new(
            TreeItemMode::Tree,
            prefix_tree,
            prefix.to_string(),
        ));
        checkpoint_items.sort_by(|a, b| a.name.cmp(&b.name));
        let checkpoint_tree = self.write_tree_indexed(&checkpoint_items, "tree")?;

        root_items.retain(|item| item.name != "checkpoint");
        root_items.push(TreeItem::new(
            TreeItemMode::Tree,
            checkpoint_tree,
            "checkpoint".to_string(),
        ));
        root_items.sort_by(|a, b| a.name.cmp(&b.name));
        self.write_tree_indexed(&root_items, "tree")
    }

    #[cfg(test)]
    pub fn get_storage(&self) -> Arc<dyn Storage + Send + Sync> {
        self.storage.clone()
    }
}

/// Inputs to [`HistoryManager::append_checkpoint_commit`].
///
/// All byte slices live for the duration of the call; the function does not
/// retain references after returning.
#[derive(Debug)]
pub struct CheckpointCommitParams<'a> {
    /// UUIDv4 of the checkpoint, used both as the row primary key and as
    /// the leaf path under `checkpoint/<id[:2]>/<id[2:]>/...`.
    pub checkpoint_id: &'a str,
    /// `agent_session.session_id` this checkpoint belongs to.
    pub session_id: &'a str,
    /// `agent_session.agent_kind` (snake_case form, e.g. `claude_code`).
    /// Also the file-name stem of `transcript/<agent_kind>.jsonl` — E4-libra
    /// pins the snake_case db tag here, never the CLI slug (`claude-code`).
    pub agent_kind: &'a str,
    /// User-branch HEAD oid at the moment the checkpoint was taken.
    pub parent_commit: Option<&'a str>,
    /// Scope category: temporary, committed, or subagent.
    pub scope: CheckpointScope,
    /// Optional tool-use id when the checkpoint was triggered by a tool call.
    pub tool_use_id: Option<&'a str>,
    /// Pre-serialised metadata JSON to land at `metadata.json`. Typed as
    /// [`RedactedBytes`] (AG-19 / G4) so the traces write path can only ever
    /// receive bytes that passed through the redaction type.
    pub metadata_json: &'a RedactedBytes,
    /// Already-redacted transcript bytes. Typed as [`RedactedBytes`]
    /// (not `&[u8]`) so the traces write path can only ever receive
    /// bytes that passed through the redaction type — entire.md §8.1 /
    /// §13 P0: every transcript blob written to `traces` must go
    /// through `RedactedBytes`.
    pub transcript_redacted: &'a RedactedBytes,
    /// E3-canonical lifecycle JSONL bytes to land at
    /// `events/lifecycle.jsonl` — one already-redacted canonical event per
    /// line (see `hooks::lifecycle::lifecycle_events_to_canonical_jsonl`).
    /// Today the runtime passes the single triggering event; multi-event
    /// batches are just additional lines. Typed as [`RedactedBytes`]
    /// (AG-19 / G4) so no `&[u8]` can reach the checkpoint sink.
    pub lifecycle_events_jsonl: &'a RedactedBytes,
    /// The aggregated redaction-report JSON (same document that lands in
    /// `agent_session.redaction_report` / metadata.json) to land at
    /// `redaction_report.json`. Rule-hit statistics only — never raw text.
    /// Typed as [`RedactedBytes`] (AG-19 / G4) to keep the whole checkpoint
    /// tree behind the redaction type.
    pub redaction_report_json: &'a RedactedBytes,
    /// plan-20260713 DR-05c-0 (ADR-DR-10): extra SQL applied INSIDE the
    /// winning ref-CAS transaction — catalog row, coverage revision inserts
    /// and claim advances commit atomically with the ref update, or the
    /// whole transaction (ref included) rolls back. `None` keeps the legacy
    /// behavior (catalog inserted separately after the CAS).
    pub txn_extra: Option<&'a dyn TracesTxnExtra>,
}

/// Per-attempt commit identifiers handed to [`TracesTxnExtra::apply`] — the
/// commit hash and root tree change on every CAS rebuild, so the extra must
/// receive them at apply time rather than capture them up front.
#[derive(Debug, Clone)]
pub struct TracesCommitCtx {
    pub commit_hash: String,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
}

/// Transactional companion writes for a traces ref update (ADR-DR-10).
///
/// `apply` runs inside the SAME SQLite transaction as the successful ref
/// CAS, after the ref row write and before COMMIT. Returning an error rolls
/// the entire transaction back — the ref does not move, and the caller's
/// checkpoint write fails closed.
#[async_trait::async_trait]
pub trait TracesTxnExtra: Send + Sync {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()>;
}

impl std::fmt::Debug for dyn TracesTxnExtra + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TracesTxnExtra")
    }
}

/// Scope tag stamped on each checkpoint, mirroring the
/// `agent_checkpoint.scope` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointScope {
    Temporary,
    Committed,
    Subagent,
}

impl CheckpointScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Committed => "committed",
            Self::Subagent => "subagent",
        }
    }
}

/// Output from [`HistoryManager::append_checkpoint_commit`]; what the caller
/// stores in `agent_checkpoint`.
///
/// Naming discipline (AG-20): `commit_hash` is the freshly-written commit on
/// `refs/libra/traces`; the DB column it lands in is
/// `agent_checkpoint.traces_commit`. Keep the two names distinct — the Rust
/// side never calls it `traces_commit` and the SQL side never `commit_hash`.
#[derive(Debug, Clone)]
pub struct CheckpointCommit {
    pub commit_hash: ObjectHash,
    pub tree_oid: ObjectHash,
    pub metadata_blob_oid: ObjectHash,
    /// Number of head-conflict retries the ref CAS loop needed (0 = first
    /// attempt won). Recorded on the `agent.checkpoint.write` span.
    pub cas_retries: u64,
    /// Objects written/enqueued for this checkpoint (blobs + trees +
    /// commit, counted across CAS attempts). Recorded on the
    /// `agent.checkpoint.write` span.
    pub object_count: u64,
}

/// Outcome of [`HistoryManager::erase_session_local`] — the three-face
/// local erasure result for one session (AG-24a).
#[derive(Debug, Clone)]
pub struct SessionEraseOutcome {
    /// Whether an `agent_session` row was deleted.
    pub session_deleted: bool,
    /// Checkpoints removed from the catalog + ref.
    pub removed_checkpoints: u64,
    /// Whether `refs/libra/traces` was rewritten.
    pub ref_rewritten: bool,
    /// `object_index` rows dropped for now-unreachable OIDs.
    pub deleted_object_index_rows: u64,
}

/// Result of pruning checkpoint commits from `refs/libra/traces`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPruneOutcome {
    pub removed_checkpoints: u64,
    pub rewritten_checkpoints: usize,
    pub ref_rewritten: bool,
    /// Which AG-20 window-guard path the prune took. `"noop"` when there
    /// was nothing to prune (guards skipped),
    /// `"markers_and_catalog_verified"` when both the live in-flight
    /// marker check and the ref-vs-catalog comparison ran and passed.
    /// Recorded on the `agent.clean.prune` span.
    pub window_guard: &'static str,
    /// `object_index` rows deleted for OIDs the prune made unreachable
    /// (conservative: only OIDs exclusively referenced by the removed
    /// checkpoints). Recorded on the `agent.clean.prune` span as
    /// `deleted_objects`.
    pub deleted_object_index_rows: u64,
}

/// Fail-closed refusals raised by [`HistoryManager::prune_checkpoint_commits`]
/// before it rewrites `refs/libra/traces` (AG-20 window A/B closure,
/// `agent.md` write-sequence matrix).
///
/// Callers can `downcast_ref` through the `anyhow` chain to distinguish a
/// deterministic guard refusal (retry later / run doctor) from a real
/// storage failure.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointPruneGuardError {
    /// Window A/B: a writer's in-flight marker is still live. The prune is
    /// a whole-chain rewrite of the shared ref and catalog, so ANY live
    /// marker — regardless of which session it belongs to — blocks the
    /// prune (safest granularity: a concurrent writer between stages
    /// (a)–(d) may hold objects/commits that neither the ref nor the
    /// catalog reaches yet, and its parent head may be about to be
    /// rewritten away). Markers expire after
    /// [`AGENT_TRACES_INFLIGHT_TTL_MS`], so the refusal is temporary.
    #[error(
        "refusing to prune traces checkpoints: a checkpoint write is in flight \
         (session '{session_id}', attempt '{attempt_id}'); in-flight markers \
         expire {ttl_ms} ms after the write starts — retry once the writer \
         finishes or the marker expires"
    )]
    LiveWriterMarker {
        session_id: String,
        attempt_id: String,
        ttl_ms: i64,
    },
    /// Window B residue: `refs/libra/traces` reaches commits that have no
    /// `agent_checkpoint` catalog row. The prune rebuild is catalog-driven,
    /// so rewriting now would silently drop those legal checkpoints;
    /// repairing the catalog is doctor's job.
    #[error(
        "refusing to prune traces checkpoints: refs/libra/traces reaches \
         {orphan_count} commit(s) with no agent_checkpoint catalog row \
         (first: {first_commit}); run `libra agent doctor --repair` to \
         backfill the catalog, then retry"
    )]
    RefCatalogOrphans {
        orphan_count: usize,
        first_commit: String,
    },
}

// ---------------------------------------------------------------------------
// AG-20 E4-libra checkpoint layout: chunking, content hash, manifest
// ---------------------------------------------------------------------------

/// E5 transcript chunking threshold: transcripts strictly larger than this
/// split into line-boundary-safe `.jsonl.%03d` parts. Frozen wire value —
/// matches the entire.io archive envelope (`50 * 1024 * 1024`).
pub const TRANSCRIPT_CHUNK_THRESHOLD_BYTES: usize = 50 * 1024 * 1024;

/// File name of the canonical lifecycle event stream inside a checkpoint
/// tree (`events/lifecycle.jsonl`, E4-libra).
pub const CHECKPOINT_LIFECYCLE_EVENTS_FILE: &str = "lifecycle.jsonl";

/// `metadata.json` external schema version written by the AG-20 writer.
///
/// v1 (pre-AG-20): `schema_version`, `checkpoint_id`, `session_id`,
/// `agent_kind`, `scope`, `provider_session_id`, `working_dir`,
/// `redaction_report`, `created_at`.
/// v2 (AG-20): all v1 fields (strictly additive — v1 readers keep working)
/// plus `model` (from the triggering lifecycle event when present, else
/// `"unknown"`, mirroring the E4-entire missing-`model` tolerance).
pub const CHECKPOINT_METADATA_SCHEMA_VERSION: u32 = 2;

/// `manifest.json` external schema version (first version).
pub const CHECKPOINT_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Ordered coverage roles for `content_hash.txt` — the sha256 runs over the
/// concatenation of these manifest entries' bytes in exactly this order.
/// `manifest.json` (written after the hash) and `content_hash.txt` itself
/// are excluded by construction. The transcript role contributes its
/// logical byte stream (chunks concatenated in part order), so the hash is
/// invariant under re-chunking. Mirrored in the manifest's
/// `content_hash.coverage` array so every checkpoint self-describes the
/// definition.
pub const CHECKPOINT_CONTENT_HASH_COVERAGE: [&str; 4] = [
    "metadata",
    "lifecycle_events",
    "transcript",
    "redaction_report",
];

/// Resolve the effective E5 chunking threshold.
///
/// `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD` (bytes, test-only — mirrors the
/// `LIBRA_TEST_*` convention) overrides the frozen 50 MiB constant so tests
/// can exercise the chunking path without allocating 50 MiB. Invalid or
/// zero values fall back to the constant rather than erroring: a stray env
/// var must never turn the writer into a per-byte chunker or a hard error.
pub fn transcript_chunk_threshold() -> usize {
    std::env::var("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&threshold| threshold > 0)
        .unwrap_or(TRANSCRIPT_CHUNK_THRESHOLD_BYTES)
}

/// Split JSONL bytes into chunks of at most `max_size` bytes, cutting only
/// at line boundaries (`\n` stays with the line it terminates). E5 contract:
/// a single line whose bytes (including its terminator) exceed `max_size`
/// is a **hard error** — silently splitting mid-line would corrupt the JSONL
/// framing for every downstream reader.
///
/// Returns borrowed sub-slices (no copy); an empty input yields one empty
/// chunk so callers always have at least one part to name.
pub fn chunk_transcript_line_safe(content: &[u8], max_size: usize) -> Result<Vec<&[u8]>> {
    if max_size == 0 {
        return Err(anyhow!("transcript chunk size must be greater than zero"));
    }
    if content.len() <= max_size {
        return Ok(vec![content]);
    }

    let mut chunks = Vec::new();
    let mut chunk_start = 0usize;
    let mut line_start = 0usize;
    while line_start < content.len() {
        let line_end = match content[line_start..].iter().position(|&b| b == b'\n') {
            Some(offset) => line_start + offset + 1, // keep the terminator
            None => content.len(),                   // final unterminated line
        };
        let line_len = line_end - line_start;
        if line_len > max_size {
            return Err(anyhow!(
                "transcript line of {line_len} bytes exceeds the {max_size}-byte chunk \
                 threshold; refusing to split mid-line (E5). Raise the threshold or fix \
                 the producer emitting the oversized line"
            ));
        }
        if line_end - chunk_start > max_size {
            chunks.push(&content[chunk_start..line_start]);
            chunk_start = line_start;
        }
        line_start = line_end;
    }
    if chunk_start < content.len() {
        chunks.push(&content[chunk_start..]);
    }
    Ok(chunks)
}

/// Reassemble E5 chunks back into the logical transcript byte stream.
/// Inverse of [`chunk_transcript_line_safe`]: parts must be supplied in
/// manifest-declared order.
pub fn reassemble_transcript_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let total = chunks.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for chunk in chunks {
        out.extend_from_slice(chunk);
    }
    out
}

/// Compute `content_hash.txt`'s value: `sha256:` + 64 lowercase hex over the
/// concatenation of `sections` in the order given (callers pass the
/// [`CHECKPOINT_CONTENT_HASH_COVERAGE`] roles' bytes). No trailing newline —
/// the string IS the file content, mirroring the E4-entire format.
pub fn checkpoint_content_hash(sections: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for section in sections {
        hasher.update(section);
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Parse a `content_hash.txt` payload into its 64-lowercase-hex digest.
///
/// Writer output always carries the `sha256:` prefix; the reader ALSO
/// accepts legacy bare hex (E4-entire compatibility table) and surrounding
/// whitespace/newline slack. Returns `None` for anything else, so callers
/// fail closed on garbage.
pub fn parse_content_hash(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let hex = trimmed.strip_prefix("sha256:").unwrap_or(trimmed);
    let normalized = hex.to_ascii_lowercase();
    (normalized.len() == 64 && normalized.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(normalized)
}

/// One written transcript part (single file or E5 chunk): tree-entry name,
/// blob OID, byte length.
#[derive(Debug, Clone)]
struct TranscriptPartRef {
    name: String,
    oid: ObjectHash,
    byte_len: usize,
}

/// (OID, byte length) pair for one single-blob manifest entry.
#[derive(Debug, Clone, Copy)]
struct ManifestBlobRef {
    oid: ObjectHash,
    byte_len: usize,
}

impl ManifestBlobRef {
    fn new(oid: ObjectHash, byte_len: usize) -> Self {
        Self { oid, byte_len }
    }
}

fn manifest_entry(
    path: &str,
    blob: ManifestBlobRef,
    media_type: &str,
    redaction: &str,
    schema_version: u32,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "oid": blob.oid.to_string(),
        "byte_len": blob.byte_len,
        "media_type": media_type,
        "compression": "none",
        "redaction": redaction,
        "schema_version": schema_version,
    })
}

/// Serialise `manifest.json` for one E4-libra checkpoint: logical role →
/// `{path, oid, byte_len, media_type, compression, redaction,
/// schema_version}`. Paths are manifest-relative (relative to the
/// checkpoint's inner tree). A chunked transcript omits the single-blob
/// `oid` and instead declares ordered `parts` (E5: doctor/export/transcript
/// readers resolve chunks ONLY through this list, never by globbing tree
/// names). `redaction` is `"redacted"` for entries carrying scrubbed
/// content, `"report"` for the rule-hit report, `"none"` for derived
/// artifacts with no user content (content_hash).
#[allow(clippy::too_many_arguments)]
fn build_checkpoint_manifest_json(
    checkpoint_id: &str,
    transcript_file_name: &str,
    metadata: ManifestBlobRef,
    lifecycle_events: ManifestBlobRef,
    transcript_parts: &[TranscriptPartRef],
    transcript_total_len: usize,
    redaction_report: ManifestBlobRef,
    content_hash: ManifestBlobRef,
) -> Result<Vec<u8>> {
    let transcript_logical_path = format!("transcript/{transcript_file_name}");
    let mut transcript_entry = serde_json::json!({
        "path": transcript_logical_path,
        "byte_len": transcript_total_len,
        "media_type": "application/x-ndjson",
        "compression": "none",
        "redaction": "redacted",
        "schema_version": 1,
    });
    // INVARIANT: transcript_entry is constructed as a JSON object above.
    let transcript_obj = transcript_entry
        .as_object_mut()
        .expect("transcript manifest entry is an object");
    if transcript_parts.len() == 1 {
        transcript_obj.insert(
            "oid".to_string(),
            serde_json::json!(transcript_parts[0].oid.to_string()),
        );
    } else {
        transcript_obj.insert("chunked".to_string(), serde_json::json!(true));
        transcript_obj.insert(
            "parts".to_string(),
            serde_json::json!(
                transcript_parts
                    .iter()
                    .map(|part| {
                        serde_json::json!({
                            "path": format!("transcript/{}", part.name),
                            "oid": part.oid.to_string(),
                            "byte_len": part.byte_len,
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }

    let manifest = serde_json::json!({
        "schema_version": CHECKPOINT_MANIFEST_SCHEMA_VERSION,
        "checkpoint_id": checkpoint_id,
        "content_hash": {
            "algorithm": "sha256",
            "path": "content_hash.txt",
            // Self-describing hash definition: sha256 over the
            // concatenation of these roles' bytes in THIS order (the
            // transcript contributes its logical, reassembled stream).
            "coverage": CHECKPOINT_CONTENT_HASH_COVERAGE,
        },
        "entries": {
            "metadata": manifest_entry(
                "metadata.json",
                metadata,
                "application/json",
                "redacted",
                CHECKPOINT_METADATA_SCHEMA_VERSION,
            ),
            "lifecycle_events": manifest_entry(
                "events/lifecycle.jsonl",
                lifecycle_events,
                "application/x-ndjson",
                "redacted",
                1,
            ),
            "transcript": transcript_entry,
            "redaction_report": manifest_entry(
                "redaction_report.json",
                redaction_report,
                "application/json",
                "report",
                1,
            ),
            "content_hash": manifest_entry(
                "content_hash.txt",
                content_hash,
                "text/plain",
                "none",
                1,
            ),
        },
    });
    serde_json::to_vec_pretty(&manifest).context("failed to serialize checkpoint manifest.json")
}

// ---------------------------------------------------------------------------
// AG-20 window A/B closure: traces writer in-flight markers
// ---------------------------------------------------------------------------

/// TTL for traces-writer in-flight markers: markers older than this are
/// considered stale leftovers of a crashed writer and stop protecting
/// their OIDs. Ten minutes comfortably bounds a checkpoint write (which is
/// local-only I/O) while keeping crashed-writer garbage collectable.
pub const AGENT_TRACES_INFLIGHT_TTL_MS: i64 = 10 * 60 * 1000;

/// One in-flight traces-writer marker (window A/B guard, AG-20).
///
/// Stored as JSON in `metadata_kv` under scope
/// [`crate::internal::metadata::MetadataScope::AgentTracesInflight`] with
/// `target` = the Libra agent session id and `key` = the write attempt's
/// checkpoint UUID. The writer creates the marker BEFORE stage (a) (blob
/// writes) and clears it AFTER stage (d) (`agent_checkpoint` INSERT), so a
/// live marker tells the prune side "objects for this attempt may exist
/// that neither the ref nor the catalog reaches yet — do not collect".
///
/// **Guarantee level (honest)**: marker persistence is best-effort. The
/// writer awaits the marker upsert before writing blobs, so on the normal
/// path the marker IS durably in SQLite first; but a marker write/clear
/// failure only logs a warning and never fails the ingest — a checkpoint
/// must not be lost because its advisory guard could not be written. The
/// prune side therefore must keep its ref-vs-catalog fail-closed
/// comparison as the primary window-B defence and treat markers as an
/// additional (window-A) shield.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TracesInflightMarker {
    /// Marker JSON schema version (additive evolution only).
    pub schema_version: u32,
    /// Libra agent session id (`agent_session.session_id`).
    pub session_id: String,
    /// Attempt UUID — the checkpoint id this write will (try to) catalog.
    pub attempt_id: String,
    /// Unix epoch milliseconds when the writer created the marker.
    pub started_at_ms: i64,
    /// Time-to-live in milliseconds; `started_at_ms + ttl_ms <= now` means
    /// expired.
    pub ttl_ms: i64,
    /// Traces commit hash, filled in (best-effort) once the ref CAS
    /// succeeded — lets prune protect the exact commit during window B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// OIDs written for this attempt (tree/metadata-blob level), filled in
    /// best-effort after stage (b) — lets prune protect loose objects
    /// during window A.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oids: Vec<String>,
}

impl TracesInflightMarker {
    /// A fresh marker for a write attempt starting now.
    pub fn new(session_id: &str, attempt_id: &str, started_at_ms: i64) -> Self {
        Self {
            schema_version: 1,
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            started_at_ms,
            ttl_ms: AGENT_TRACES_INFLIGHT_TTL_MS,
            commit: None,
            oids: Vec::new(),
        }
    }

    /// Whether the marker is still live at `now_ms`.
    pub fn is_live(&self, now_ms: i64) -> bool {
        self.started_at_ms.saturating_add(self.ttl_ms) > now_ms
    }
}

/// Upsert an in-flight marker row. Exported (not a stable API) so the
/// writer, the prune side, and integration tests share one implementation.
pub async fn write_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    marker: &TracesInflightMarker,
) -> Result<()> {
    let value =
        serde_json::to_string(marker).context("failed to serialize traces in-flight marker")?;
    crate::internal::metadata::MetadataKv::set_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        &marker.session_id,
        &marker.attempt_id,
        &value,
        crate::internal::metadata::MetadataValueType::Text,
    )
    .await
    .context("failed to persist traces in-flight marker")?;
    Ok(())
}

/// Remove one in-flight marker (stage (d) complete, or prune-side cleanup
/// of an expired marker). Returns whether a row was removed.
pub async fn clear_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    crate::internal::metadata::MetadataKv::unset_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        session_id,
        attempt_id,
    )
    .await
    .context("failed to clear traces in-flight marker")
}

/// List the LIVE (non-expired at `now_ms`) in-flight markers across all
/// sessions — the prune-side entry point: any OID/commit named by a
/// returned marker must be treated as reachable, and (fail-closed) a live
/// marker for a session should defer pruning that session's chain.
///
/// Rows whose JSON does not parse are skipped with a warning: a corrupt
/// marker cannot name OIDs to protect and has no readable TTL, so keeping
/// it would block pruning forever. The prune side's ref-vs-catalog
/// comparison remains the primary defence (see [`TracesInflightMarker`]).
pub async fn list_live_traces_inflight_markers<C: ConnectionTrait>(
    conn: &C,
    now_ms: i64,
) -> Result<Vec<TracesInflightMarker>> {
    let entries = crate::internal::metadata::MetadataKv::list_scope_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
    )
    .await
    .context("failed to list traces in-flight markers")?;
    let mut live = Vec::new();
    for entry in entries {
        match serde_json::from_str::<TracesInflightMarker>(&entry.value) {
            Ok(marker) => {
                if marker.is_live(now_ms) {
                    live.push(marker);
                }
            }
            Err(err) => {
                tracing::warn!(
                    session_id = %entry.target,
                    attempt_id = %entry.key,
                    error = %err,
                    "skipping unparseable traces in-flight marker"
                );
            }
        }
    }
    Ok(live)
}

/// Probe the checkpoint catalog by traces commit hash: returns the
/// `checkpoint_id` of the row whose `traces_commit` equals `commit_hash`,
/// if any. The writer calls this between ref CAS and catalog INSERT so a
/// crash-retry (or a doctor repair that already backfilled the row from
/// the ref) skips the INSERT instead of duplicating the commit's catalog
/// entry; doctor's window-B repair uses the same probe for idempotency.
pub async fn agent_checkpoint_id_for_traces_commit<C: ConnectionTrait>(
    conn: &C,
    commit_hash: &str,
) -> Result<Option<String>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id FROM agent_checkpoint WHERE traces_commit = ? LIMIT 1",
            [Value::from(commit_hash)],
        ))
        .await
        .context("failed to probe agent_checkpoint by traces_commit")?;
    row.map(|row| {
        row.try_get_by("checkpoint_id")
            .context("decode agent_checkpoint.checkpoint_id")
    })
    .transpose()
}

#[derive(Debug, Clone)]
struct CheckpointHistoryRow {
    checkpoint_id: String,
    session_id: String,
    agent_kind: String,
    scope: String,
    parent_commit: Option<String>,
    /// `agent_checkpoint.traces_commit` — the commit this row currently
    /// points at on `refs/libra/traces`. Consumed by the prune-side
    /// ref-vs-catalog window-B guard and the `object_index` cleanup.
    traces_commit: Option<String>,
    /// `agent_checkpoint.tree_oid` (root tree of `traces_commit`).
    tree_oid: Option<String>,
    /// `agent_checkpoint.metadata_blob_oid` (the checkpoint's
    /// `metadata.json` blob).
    metadata_blob_oid: Option<String>,
}

impl CheckpointHistoryRow {
    fn from_query_result(row: QueryResult) -> Result<Self> {
        Ok(Self {
            checkpoint_id: row
                .try_get_by("checkpoint_id")
                .context("decode agent_checkpoint.checkpoint_id")?,
            session_id: row
                .try_get_by("session_id")
                .context("decode agent_checkpoint.session_id")?,
            agent_kind: row
                .try_get_by("agent_kind")
                .context("decode agent_session.agent_kind")?,
            scope: row
                .try_get_by("scope")
                .context("decode agent_checkpoint.scope")?,
            parent_commit: row.try_get_by("parent_commit").ok().flatten(),
            traces_commit: row.try_get_by("traces_commit").ok().flatten(),
            tree_oid: row.try_get_by("tree_oid").ok().flatten(),
            metadata_blob_oid: row.try_get_by("metadata_blob_oid").ok().flatten(),
        })
    }
}

#[derive(Debug, Clone)]
struct RewrittenCheckpoint {
    checkpoint_id: String,
    traces_commit: ObjectHash,
    tree_oid: ObjectHash,
}

/// OIDs that a prune provably makes unreachable and that are exclusively
/// referenced by the removed checkpoints — the conservative candidate set
/// for `object_index` cleanup (AG-20; the pre-fix behaviour leaked every
/// row forever).
///
/// Included per removed catalog row: its `traces_commit` (the commit
/// object), `tree_oid` (the commit's root tree), and `metadata_blob_oid`
/// (its `metadata.json` blob). Each is referenced only by that
/// checkpoint's chain entry by construction, and anything still referenced
/// is excluded below.
///
/// Deliberately **excluded** (exclusivity is not cheaply provable from the
/// catalog, so we skip rather than risk deleting a shared OID):
/// - inner checkpoint subtrees and transcript/events/manifest blobs of the
///   removed checkpoints (their OIDs are not recorded in the catalog);
/// - the pre-rewrite commits/root trees of RETAINED checkpoints (they may
///   be byte-identical to their rewritten successors, and the leak is
///   bounded by the retained-row count).
///
/// The exclusion set covers every OID the catalog still references after
/// the prune: retained rows' current OIDs plus the freshly rewritten
/// commits/trees and the new head.
fn collect_exclusive_unreachable_oids(
    removed_rows: &[CheckpointHistoryRow],
    retained_rows: &[CheckpointHistoryRow],
    rewritten: &[RewrittenCheckpoint],
) -> Vec<String> {
    let mut still_referenced: HashSet<String> = HashSet::new();
    for row in retained_rows {
        still_referenced.extend(
            [&row.traces_commit, &row.tree_oid, &row.metadata_blob_oid]
                .into_iter()
                .filter_map(|oid| oid.clone()),
        );
    }
    for item in rewritten {
        still_referenced.insert(item.traces_commit.to_string());
        still_referenced.insert(item.tree_oid.to_string());
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut unreachable = Vec::new();
    for row in removed_rows {
        for oid in [&row.traces_commit, &row.tree_oid, &row.metadata_blob_oid]
            .into_iter()
            .filter_map(|oid| oid.clone())
        {
            if !still_referenced.contains(&oid) && seen.insert(oid.clone()) {
                unreachable.push(oid);
            }
        }
    }
    unreachable
}

fn checkpoint_tree_path(checkpoint_id: &str) -> Result<(String, String)> {
    let prefix = checkpoint_id
        .get(..2)
        .ok_or_else(|| anyhow!("checkpoint_id must be at least 2 characters"))?
        .to_string();
    let rest = checkpoint_id
        .get(2..)
        .ok_or_else(|| anyhow!("checkpoint_id must be valid UTF-8 at byte 2"))?
        .to_string();
    Ok((prefix, rest))
}

fn format_libra_trailers(params: &CheckpointCommitParams<'_>) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("Libra-Session: {}\n", params.session_id));
    buf.push_str(&format!("Libra-Agent: {}\n", params.agent_kind));
    if let Some(commit) = params.parent_commit {
        buf.push_str(&format!("Libra-Parent-Commit: {commit}\n"));
    }
    buf.push_str(&format!("Libra-Checkpoint-ID: {}\n", params.checkpoint_id));
    buf.push_str(&format!("Libra-Scope: {}\n", params.scope.as_str()));
    if let Some(tool) = params.tool_use_id {
        buf.push_str(&format!("Libra-Tool-Use-ID: {tool}\n"));
    }
    buf
}

fn format_rewritten_checkpoint_trailers(row: &CheckpointHistoryRow) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("Libra-Session: {}\n", row.session_id));
    buf.push_str(&format!("Libra-Agent: {}\n", row.agent_kind));
    if let Some(commit) = &row.parent_commit {
        buf.push_str(&format!("Libra-Parent-Commit: {commit}\n"));
    }
    buf.push_str(&format!("Libra-Checkpoint-ID: {}\n", row.checkpoint_id));
    buf.push_str(&format!("Libra-Scope: {}\n", row.scope));
    buf
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, Schema, Statement};
    use tempfile::tempdir;
    use tokio::time::sleep;

    use super::*;
    use crate::{internal::db, utils::storage::local::LocalStorage};

    async fn setup_test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let builder = db.get_database_backend();
        let schema = Schema::new(builder);
        let stmt = schema.create_table_from_entity(reference::Entity);
        db.execute(builder.build(&stmt)).await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_history_append_simple() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        // 1. Append first object
        let blob_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        manager.append("task", "task-1", blob_hash).await.unwrap();

        // Verify ref exists in DB
        let ref_model = reference::Entity::find()
            .filter(reference::Column::Name.eq(AI_REF))
            .filter(reference::Column::Kind.eq(ConfigKind::Branch))
            .one(&*db_conn)
            .await
            .unwrap()
            .expect("Reference should exist");

        let commit_hash_str = ref_model.commit.expect("Commit hash should exist");
        let commit_hash = ObjectHash::from_str(&commit_hash_str).unwrap();

        // Verify we can load commit
        let data = read_git_object(&repo_path, &commit_hash).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(content.contains("tree "));
        assert!(content.contains("Update task/task-1"));

        // 2. Append second object (same type)
        let blob_hash_2 = ObjectHash::from_str("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();
        manager.append("task", "task-2", blob_hash_2).await.unwrap();

        // 3. Append third object (different type)
        manager.append("run", "run-1", blob_hash).await.unwrap();

        // Load Head Commit from DB
        let head = manager.resolve_history_head().await.unwrap().unwrap();

        // Verify we can load commit
        let data = read_git_object(&repo_path, &head).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(content.contains("tree "));
        assert!(content.contains("Update run/run-1"));
    }

    #[tokio::test]
    async fn test_find_object_hashes_returns_all_matching_types() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        let blob_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let other_hash = ObjectHash::from_str("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();

        manager
            .append("patchset", "shared-id", blob_hash)
            .await
            .unwrap();
        manager
            .append("event", "shared-id", other_hash)
            .await
            .unwrap();

        let matches = manager.find_object_hashes("shared-id").await.unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|(_, kind)| kind == "patchset"));
        assert!(matches.iter().any(|(_, kind)| kind == "event"));
    }

    #[tokio::test]
    async fn test_list_object_types_returns_sorted_types() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        let blob_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        manager
            .append("run_event", "run-event-1", blob_hash)
            .await
            .unwrap();
        manager
            .append("patchset", "patchset-1", blob_hash)
            .await
            .unwrap();

        let types = manager.list_object_types().await.unwrap();
        assert_eq!(types, vec!["patchset".to_string(), "run_event".to_string()]);
    }

    #[tokio::test]
    async fn test_update_ref_retries_when_sqlite_is_locked() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir(&objects_dir).unwrap();
        let db_path = repo_path.join("libra.db");

        let db_conn = Arc::new(
            db::create_database(db_path.to_str().unwrap())
                .await
                .expect("failed to create sqlite database"),
        );
        let storage = Arc::new(LocalStorage::new(objects_dir));
        let manager = HistoryManager::new(storage, repo_path.clone(), db_conn.clone());

        let locker = db::establish_connection_with_busy_timeout(
            db_path.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .expect("failed to open lock holder connection");
        let backend = locker.get_database_backend();
        locker
            .execute(Statement::from_string(backend, "BEGIN EXCLUSIVE"))
            .await
            .expect("failed to acquire sqlite exclusive lock");

        let release = {
            let locker = locker.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(250)).await;
                let backend = locker.get_database_backend();
                locker
                    .execute(Statement::from_string(backend, "COMMIT"))
                    .await
                    .expect("failed to release sqlite exclusive lock");
            })
        };

        let hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        manager
            .update_ref(AI_REF, hash)
            .await
            .expect("update_ref should retry through a transient sqlite lock");
        release.await.unwrap();

        let resolved = manager
            .resolve_history_head()
            .await
            .expect("history head should be readable after retry")
            .expect("history head should exist");
        assert_eq!(resolved, hash);
    }

    #[tokio::test]
    async fn test_update_ref_if_matches_rejects_stale_history_head() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage, repo_path, db_conn);

        let task_hash = ObjectHash::from_str("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391").unwrap();
        let plan_hash = ObjectHash::from_str("f4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();
        let frame_hash = ObjectHash::from_str("a4e6d0434b8b29ae775ad8c2e48c5391e69de29b").unwrap();

        manager.append("task", "task-1", task_hash).await.unwrap();
        let stale_head = manager.resolve_history_head().await.unwrap();
        let stale_commit = manager
            .create_append_commit(stale_head, "plan", "plan-1", plan_hash)
            .expect("stale append commit should be created");

        manager
            .append("context_frame", "frame-1", frame_hash)
            .await
            .unwrap();

        let outcome = manager
            .update_ref_if_matches(AI_REF, stale_head, stale_commit)
            .await
            .expect("stale ref update should not error");
        assert_eq!(outcome, RefUpdateOutcome::HeadChanged);

        manager.append("plan", "plan-1", plan_hash).await.unwrap();

        assert!(
            manager
                .get_object_hash("context_frame", "frame-1")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            manager
                .get_object_hash("plan", "plan-1")
                .await
                .unwrap()
                .is_some()
        );
    }

    // -------------------------------------------------------------------
    // AG-20: E5 line-safe chunking
    // -------------------------------------------------------------------

    /// Small inputs (≤ max) come back as one borrowed chunk, unsplit.
    #[test]
    fn chunker_returns_single_chunk_at_or_below_threshold() {
        let content = b"line-1\nline-2\n";
        let chunks = chunk_transcript_line_safe(content, content.len()).unwrap();
        assert_eq!(chunks, vec![&content[..]]);
        // Empty input still yields one (empty) chunk to name.
        let empty = chunk_transcript_line_safe(b"", 16).unwrap();
        assert_eq!(empty, vec![&b""[..]]);
    }

    /// Chunks cut ONLY at line boundaries, each stays within the limit,
    /// and concatenating them reproduces the input byte-for-byte.
    #[test]
    fn chunker_splits_on_line_boundaries_and_roundtrips() {
        let mut content = Vec::new();
        for index in 0..100 {
            content.extend_from_slice(format!("{{\"line\":{index}}}\n").as_bytes());
        }
        let max = 64;
        let chunks = chunk_transcript_line_safe(&content, max).unwrap();
        assert!(chunks.len() > 1, "must actually chunk");
        for chunk in &chunks {
            assert!(chunk.len() <= max, "chunk of {} exceeds {max}", chunk.len());
            assert!(
                chunk.ends_with(b"\n"),
                "every newline-terminated input chunk must end at a line boundary"
            );
        }
        let owned: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        assert_eq!(reassemble_transcript_chunks(&owned), content);
    }

    /// A final unterminated line is preserved verbatim (no invented `\n`).
    #[test]
    fn chunker_preserves_final_unterminated_line() {
        let content = b"aaaa\nbbbb\ncccc-tail";
        let chunks = chunk_transcript_line_safe(content, 10).unwrap();
        let owned: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        assert_eq!(reassemble_transcript_chunks(&owned), content.to_vec());
        assert!(chunks.last().unwrap().ends_with(b"cccc-tail"));
    }

    /// E5 hard error: a single line larger than the threshold refuses to
    /// split mid-line.
    #[test]
    fn chunker_rejects_single_line_over_threshold() {
        let long_line = vec![b'x'; 100];
        let err = chunk_transcript_line_safe(&long_line, 64).unwrap_err();
        assert!(
            err.to_string().contains("exceeds"),
            "error must explain the oversized line: {err}"
        );
        // Terminated variant errors too.
        let mut terminated = long_line.clone();
        terminated.push(b'\n');
        assert!(chunk_transcript_line_safe(&terminated, 64).is_err());
        // Zero max is rejected outright.
        assert!(chunk_transcript_line_safe(b"x", 0).is_err());
    }

    // -------------------------------------------------------------------
    // AG-20: content hash format + reader tolerance
    // -------------------------------------------------------------------

    /// Writer format is `sha256:` + 64 lowercase hex, no trailing newline,
    /// and equals the sha256 of the concatenated sections.
    #[test]
    fn content_hash_has_pinned_format_and_value() {
        let hash = checkpoint_content_hash(&[b"alpha", b"beta"]);
        assert!(hash.starts_with("sha256:"));
        let hex = &hash["sha256:".len()..];
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!hash.ends_with('\n'));
        // Concatenation order matters and is deterministic.
        assert_eq!(hash, checkpoint_content_hash(&[b"alphabeta"]));
        assert_ne!(hash, checkpoint_content_hash(&[b"beta", b"alpha"]));
    }

    /// Reader tolerance (E4-entire table): the prefix form and legacy bare
    /// hex both parse to the same digest; garbage does not parse.
    #[test]
    fn parse_content_hash_accepts_prefix_and_legacy_bare_hex() {
        let digest = "a".repeat(64);
        assert_eq!(
            parse_content_hash(&format!("sha256:{digest}")),
            Some(digest.clone())
        );
        assert_eq!(parse_content_hash(&digest), Some(digest.clone()));
        // Whitespace slack (e.g. a stray trailing newline) is tolerated.
        assert_eq!(
            parse_content_hash(&format!("sha256:{digest}\n")),
            Some(digest.clone())
        );
        // Uppercase hex normalises to lowercase.
        assert_eq!(
            parse_content_hash(&digest.to_uppercase()),
            Some(digest.clone())
        );
        assert_eq!(parse_content_hash("sha256:tooshort"), None);
        assert_eq!(parse_content_hash(&"z".repeat(64)), None);
        assert_eq!(parse_content_hash(""), None);
    }

    // -------------------------------------------------------------------
    // AG-20: in-flight marker liveness math
    // -------------------------------------------------------------------

    #[test]
    fn inflight_marker_liveness_respects_ttl() {
        let marker = TracesInflightMarker::new("session-a", "attempt-1", 1_000);
        assert!(marker.is_live(1_000));
        assert!(marker.is_live(1_000 + AGENT_TRACES_INFLIGHT_TTL_MS - 1));
        assert!(!marker.is_live(1_000 + AGENT_TRACES_INFLIGHT_TTL_MS));
        // Marker JSON round-trips (schema pin for the prune side).
        let json = serde_json::to_string(&marker).unwrap();
        let back: TracesInflightMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "session-a");
        assert_eq!(back.attempt_id, "attempt-1");
        assert_eq!(back.ttl_ms, AGENT_TRACES_INFLIGHT_TTL_MS);
        assert!(back.commit.is_none());
        assert!(back.oids.is_empty());
    }
}
