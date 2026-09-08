//! Local protocol client using filesystem paths to run upload-pack/receive-pack locally and stream pack data over async pipes.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    env, fs,
    future::Future,
    io::Error as IoError,
    path::{Path, PathBuf},
    str::FromStr,
    sync::OnceLock,
};

use bytes::Bytes;
use futures_util::stream;
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, get_hash_kind, set_hash_kind},
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            tag::Tag,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
        pack::{encode::PackEncoder, entry::Entry},
    },
};
use tokio::sync::Mutex;
use url::Url;

use super::{DiscoveryResult, FetchStream, ProtocolClient};
use crate::{
    command::{load_object, log::get_reachable_commits},
    git_protocol::ServiceType,
    internal::{
        ai::linear_ref::{OwnedRefSpec, OwnedRefTransportPolicy},
        branch::Branch,
        config::ConfigKv,
        db::get_db_conn_instance_for_path,
        head::Head,
        protocol::DiscRef,
        reflog, tag,
    },
    utils::{
        client_storage::ClientStorage,
        object_ext::TreeExt,
        util::{DATABASE, cur_dir},
    },
};

#[derive(Debug, Clone)]
enum RepoType {
    GitRepo,
    LibraRepo,
}

#[derive(Debug, Clone)]
pub struct LocalClient {
    repo_path: PathBuf,
    source_type: RepoType,
}

static LOCAL_PROTOCOL_CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn local_protocol_cwd_lock() -> &'static Mutex<()> {
    LOCAL_PROTOCOL_CWD_LOCK.get_or_init(|| Mutex::new(()))
}

/// RAII guard for temporarily switching the process current directory.
///
/// This supports an explicit `restore()` so callers can surface restore
/// failures on the success path, while `Drop` still restores the directory if
/// the surrounding future is cancelled or aborted.
struct RepoCurrentDirGuard {
    original_dir: PathBuf,
    restored: bool,
    restore_failure_logged: bool,
    #[cfg(test)]
    _cwd_lock: crate::utils::test::CwdLockGuard,
}

impl RepoCurrentDirGuard {
    fn change_to(new_dir: &Path) -> Result<Self, IoError> {
        #[cfg(test)]
        let cwd_lock = crate::utils::test::cwd_lock_guard();
        let original_dir = env::current_dir()?;
        env::set_current_dir(new_dir)?;
        Ok(Self {
            original_dir,
            restored: false,
            restore_failure_logged: false,
            #[cfg(test)]
            _cwd_lock: cwd_lock,
        })
    }

    fn restore(&mut self) -> Result<(), IoError> {
        env::set_current_dir(&self.original_dir)?;
        self.restored = true;
        Ok(())
    }

    fn mark_restore_failure_logged(&mut self) {
        self.restore_failure_logged = true;
    }
}

impl Drop for RepoCurrentDirGuard {
    fn drop(&mut self) {
        if self.restored {
            return;
        }

        if let Err(error) = env::set_current_dir(&self.original_dir) {
            if self.restore_failure_logged {
                return;
            }

            self.restore_failure_logged = true;
            tracing::error!(
                restore_dir = %self.original_dir.display(),
                error = %error,
                "failed to restore working directory after local protocol operation"
            );
        }
    }
}

struct HashKindRestoreGuard {
    previous: HashKind,
}

impl HashKindRestoreGuard {
    fn switch_to(hash_kind: HashKind) -> Self {
        let previous = get_hash_kind();
        set_hash_kind(hash_kind);
        Self { previous }
    }
}

impl Drop for HashKindRestoreGuard {
    fn drop(&mut self) {
        set_hash_kind(self.previous);
    }
}

impl ProtocolClient for LocalClient {
    fn from_url(url: &Url) -> Self {
        let path = url
            .to_file_path()
            .unwrap_or_else(|_| PathBuf::from(url.path()));
        Self {
            repo_path: path.clone(),
            source_type: {
                if path.join("libra.db").try_exists().unwrap_or(false)
                    || path.join(".libra/libra.db").try_exists().unwrap_or(false)
                {
                    RepoType::LibraRepo
                } else {
                    RepoType::GitRepo
                }
            },
        }
    }
}

impl LocalClient {
    /// Whether the source is a Libra repository (vs a plain Git repo). Object
    /// alternates auto-registration (lore.md 2.11) is gated on this — a Git
    /// source's `git gc` does not consult Libra's borrowers file.
    pub fn is_libra_source(&self) -> bool {
        matches!(self.source_type, RepoType::LibraRepo)
    }

    async fn with_repo_current_dir<T, E, F, Fut>(&self, operation: F) -> Result<T, E>
    where
        E: From<IoError>,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        // Local protocol operations mutate the process cwd, so serialize them
        // to avoid cross-task races while the repo-scoped cwd is active.
        let _cwd_lock = local_protocol_cwd_lock().lock().await;
        let mut guard = RepoCurrentDirGuard::change_to(&self.repo_path).map_err(E::from)?;
        let result = operation().await;

        match guard.restore() {
            Ok(()) => result,
            Err(restore_error) => match result {
                Ok(_) => Err(E::from(restore_error)),
                Err(error) => {
                    guard.mark_restore_failure_logged();
                    tracing::error!(
                        repo_path = %self.repo_path.display(),
                        restore_dir = %guard.original_dir.display(),
                        error = %restore_error,
                        "failed to restore working directory after local protocol operation"
                    );
                    Err(error)
                }
            },
        }
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, IoError> {
        let path = path.as_ref();
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cur_dir().join(path)
        };
        if !absolute.try_exists().unwrap_or(false) {
            return Err(IoError::other(format!(
                "Local repository path does not exist: {}",
                absolute.display()
            )));
        }
        if absolute.join("objects").try_exists().unwrap_or(false) {
            let is_libra_repo = absolute.join("libra.db").try_exists().unwrap_or(false);
            let is_git_repo = absolute.join("HEAD").try_exists().unwrap_or(false);
            match (is_libra_repo, is_git_repo) {
                (true, false) => Ok(Self {
                    repo_path: absolute,
                    source_type: RepoType::LibraRepo,
                }),
                (false, true) => Ok(Self {
                    repo_path: absolute,
                    source_type: RepoType::GitRepo,
                }),
                _ => Err(IoError::other(format!(
                    "No valid Git directory structure found at: {}",
                    absolute.display()
                ))),
            }
        } else if absolute.join(".git/HEAD").try_exists().unwrap_or(false) {
            Ok(Self {
                repo_path: absolute.join(".git"),
                source_type: RepoType::GitRepo,
            })
        } else if absolute
            .join(".libra/libra.db")
            .try_exists()
            .unwrap_or(false)
        {
            Ok(Self {
                repo_path: absolute.join(".libra"),
                source_type: RepoType::LibraRepo,
            })
        } else {
            Err(IoError::other(format!(
                "No valid Git directory structure found at: {}",
                absolute.display()
            )))
        }
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Export this repo's dependency notes (lore.md 3.2) for cross-machine
    /// travel. Returns `(entries, warnings)` where each entry is
    /// `(annotated commit oid, adjacency document text)`. Because a Libra deps
    /// note is a loose blob + a SQLite `notes` row (not a commit-reachable
    /// object), it cannot ride the packfile; this reads the source's notes table
    /// and object store directly over the local protocol. Per-note
    /// fault-tolerant: a note whose blob is missing or not valid UTF-8 is
    /// warn-and-skipped, never fatal — a completed fetch must not abort after its
    /// refs are updated. A plain Git source has no Libra deps notes; it returns
    /// an empty set plus an honest "deferred" warning (network / foreign-Git
    /// notes travel is `_compatibility.md` D17).
    pub async fn export_deps_notes(
        &self,
    ) -> Result<(Vec<(String, String)>, Vec<String>), GitError> {
        match self.source_type {
            RepoType::GitRepo => Ok((
                Vec::new(),
                vec![
                    "dependency-note travel from a non-Libra (Git) source is not supported yet; \
                     the dependency graph was not fetched"
                        .to_string(),
                ],
            )),
            RepoType::LibraRepo => {
                self.with_repo_current_dir(|| async {
                    let repo_hash_kind =
                        self.repo_hash_kind().await.map_err(GitError::CustomError)?;
                    let _hash_guard = HashKindRestoreGuard::switch_to(repo_hash_kind);

                    // Enumerate every `refs/notes/deps` row on the source. A read
                    // failure (e.g. absent table) yields nothing to export rather
                    // than aborting the fetch.
                    let notes = match crate::internal::notes::list(
                        crate::internal::deps::REVISION_DEPS_NOTES_REF,
                        None,
                    )
                    .await
                    {
                        Ok(notes) => notes,
                        Err(e) => {
                            return Ok((
                                Vec::new(),
                                vec![format!(
                                    "could not enumerate dependency notes on the source: {e}"
                                )],
                            ));
                        }
                    };

                    let storage = ClientStorage::init(crate::utils::path::objects());
                    let mut entries: Vec<(String, String)> = Vec::new();
                    let mut warnings: Vec<String> = Vec::new();
                    for note in notes {
                        let Some(blob_hash) = note.note_hash else {
                            continue;
                        };
                        let blob_oid = match ObjectHash::from_str(&blob_hash) {
                            Ok(oid) => oid,
                            Err(e) => {
                                warnings.push(format!(
                                    "skipped source dependency note for {}: invalid blob id \
                                     {blob_hash}: {e}",
                                    note.annotated_object
                                ));
                                continue;
                            }
                        };
                        let bytes = match storage.get(&blob_oid) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                warnings.push(format!(
                                    "skipped source dependency note for {}: {e}",
                                    note.annotated_object
                                ));
                                continue;
                            }
                        };
                        match String::from_utf8(bytes) {
                            Ok(text) => entries.push((note.annotated_object, text)),
                            Err(_) => warnings.push(format!(
                                "skipped source dependency note for {}: content is not valid UTF-8",
                                note.annotated_object
                            )),
                        }
                    }
                    Ok((entries, warnings))
                })
                .await
            }
        }
    }

    async fn repo_hash_kind(&self) -> Result<HashKind, String> {
        let db_path = self.repo_path.join(DATABASE);
        let db_conn = get_db_conn_instance_for_path(&db_path)
            .await
            .map_err(|error| {
                format!(
                    "failed to open local repository database '{}': {error}",
                    db_path.display()
                )
            })?;
        let object_format = ConfigKv::get_with_conn(&db_conn, "core.objectformat")
            .await
            .map_err(|error| {
                format!(
                    "failed to read core.objectformat from local repository '{}': {error}",
                    db_path.display()
                )
            })?
            .map(|entry| entry.value)
            .unwrap_or_else(|| "sha1".to_string());

        match object_format.as_str() {
            "sha1" => Ok(HashKind::Sha1),
            "sha256" => Ok(HashKind::Sha256),
            _ => Err(format!(
                "unsupported object format '{object_format}' in local repository '{}'",
                db_path.display()
            )),
        }
    }

    pub async fn discovery_reference(
        &self,
        service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        if service != ServiceType::UploadPack {
            return Err(GitError::NetworkError(
                "Unsupported service type for local protocol".to_string(),
            ));
        }
        match self.source_type {
            RepoType::GitRepo => {
                // In-process discovery: read the foreign Git repository's refs
                // directly instead of spawning `git-upload-pack --advertise-refs`.
                let hash_kind = git_repo_hash_kind(&self.repo_path);
                let _hash_guard = HashKindRestoreGuard::switch_to(hash_kind);
                let refs = read_git_repo_refs(&self.repo_path).map_err(|error| {
                    GitError::NetworkError(format!(
                        "failed to read references from local repository '{}': {error}",
                        self.repo_path.display()
                    ))
                })?;
                // Advertise `symref=HEAD:<target>` like `git-upload-pack` so
                // `ls-remote --symref` still prints HEAD's target for Git repos.
                let capabilities = git_repo_head_symref(&self.repo_path)
                    .into_iter()
                    .collect::<Vec<_>>();
                Ok(DiscoveryResult {
                    refs,
                    capabilities,
                    hash_kind,
                })
            }
            RepoType::LibraRepo => {
                self.with_repo_current_dir(|| async {
                    let repo_hash_kind =
                        self.repo_hash_kind().await.map_err(GitError::CustomError)?;
                    let _hash_guard = HashKindRestoreGuard::switch_to(repo_hash_kind);

                    let local_branches = Branch::list_branches_result(None)
                        .await
                        .map_err(|error| GitError::CustomError(error.to_string()))?
                        .into_iter()
                        .filter(|branch| ordinary_transport_branch(&branch.name))
                        .collect::<Vec<_>>();

                    let remote_configs = ConfigKv::all_remote_configs()
                        .await
                        .map_err(|error| GitError::CustomError(error.to_string()))?;
                    let mut remote_branches: Vec<_> = vec![];
                    for remote in remote_configs {
                        remote_branches.extend(
                            Branch::list_branches_result(Some(&remote.name))
                                .await
                                .map_err(|error| GitError::CustomError(error.to_string()))?
                                .into_iter()
                                .filter(|branch| ordinary_transport_branch(&branch.name)),
                        );
                    }
                    let head_commit = Head::current_commit_result()
                        .await
                        .map_err(|error| GitError::CustomError(error.to_string()))?;
                    let tags = tag::list()
                        .await
                        .map_err(|error| GitError::CustomError(error.to_string()))?;
                    let mut tag_references = Vec::new();
                    for tag in tags {
                        tag_references.extend(tag_refs(tag).await?);
                    }
                    Ok(DiscoveryResult {
                        refs: local_branches
                            .into_iter()
                            .chain(remote_branches)
                            .map(Into::into)
                            .chain(tag_references)
                            .chain(head_commit.map(|x| x.to_string()).map(|hash| DiscRef {
                                _hash: hash,
                                _ref: reflog::HEAD.to_string(),
                            }))
                            .collect::<Vec<_>>(),
                        capabilities: vec![],
                        hash_kind: repo_hash_kind,
                    })
                })
                .await
            }
        }
    }

    pub async fn fetch_objects(
        &self,
        have: &[String],
        want: &[String],
        shallow: &[String],
        depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        match self.source_type {
            RepoType::GitRepo => {
                // In-process fetch: assemble the pack from the foreign Git
                // repository's own object store instead of spawning
                // `git-upload-pack --stateless-rpc`.
                let _ = shallow; // requested-shallow negotiation is not honoured (matches LibraRepo)
                let hash_kind = git_repo_hash_kind(&self.repo_path);
                // A strictly local store — never route a foreign repo's reads
                // through cloud storage or write objects back into it.
                let storage = ClientStorage::init_local(self.repo_path.join("objects"));
                // Collect synchronously with the foreign hash kind active, then
                // drop the (thread-local) guard before the async encode so it is
                // never held across an `.await`.
                let (entries, shallow) = {
                    let _hash_guard = HashKindRestoreGuard::switch_to(hash_kind);
                    collect_git_repo_entries(&storage, &self.repo_path, want, have, depth).map_err(
                        |error| {
                            IoError::other(format!(
                                "failed to assemble pack for '{}': {error}",
                                self.repo_path.display()
                            ))
                        },
                    )?
                };
                encode_entries_to_fetch_response(entries, shallow, hash_kind).await
            }
            RepoType::LibraRepo => {
                self.with_repo_current_dir(|| async {
                    let repo_hash_kind = self.repo_hash_kind().await.map_err(IoError::other)?;
                    let _hash_guard = HashKindRestoreGuard::switch_to(repo_hash_kind);

                    let mut seen = HashSet::new();
                    have.iter().for_each(|hash| {
                        seen.insert(hash.clone());
                    });

                    // Classify each want. An annotated-tag object contributes the
                    // tag object(s) to the pack and resolves to a target commit;
                    // every other want is treated as a commit (lightweight tags
                    // and branch tips already point at commits). This lets a
                    // libra-native server honour `fetch --tags` of annotated tags.
                    // Requires git-internal >= 0.7.6 so a tag's id is the canonical
                    // hash of its `to_data()` (otherwise the receiver re-hashes to a
                    // different OID and the fetched `refs/tags/*` dangles).
                    let mut tag_entries: Vec<Entry> = Vec::new();
                    let mut commit_targets: Vec<String> = Vec::new();
                    for want_hash in want {
                        let Ok(oid) = git_internal::hash::ObjectHash::from_str(want_hash) else {
                            commit_targets.push(want_hash.clone());
                            continue;
                        };
                        match tag::load_object_trait(&oid).await {
                            Ok(tag::TagObject::Tag(tag_obj)) => {
                                let mut current = tag_obj;
                                for _ in 0..32 {
                                    let target = current.object_hash;
                                    if seen.insert(current.id.to_string()) {
                                        tag_entries.push(Entry::from(current));
                                    }
                                    match tag::load_object_trait(&target).await {
                                        Ok(tag::TagObject::Tag(inner)) => current = inner,
                                        Ok(tag::TagObject::Commit(commit)) => {
                                            commit_targets.push(commit.id.to_string());
                                            break;
                                        }
                                        // Tag of a tree/blob, or a missing target.
                                        _ => break,
                                    }
                                }
                            }
                            _ => commit_targets.push(want_hash.clone()),
                        }
                    }

                    let mut reachable_commits = Vec::new();
                    for branch_hash in &commit_targets {
                        let commits = get_reachable_commits(branch_hash.to_string(), depth)
                            .await
                            .map_err(|error| {
                                IoError::other(format!(
                                    "failed to walk reachable commits for '{branch_hash}': \
                                         {error}"
                                ))
                            })?;
                        reachable_commits.extend(commits);
                    }

                    let commits = reachable_commits
                        .into_iter()
                        .filter(|c| seen.insert(c.id.to_string()))
                        .collect::<Vec<_>>();

                    let (tree_hash, blob_hash): (Vec<_>, Vec<_>) = commits
                        .iter()
                        .map(|commit| &commit.tree_id)
                        .map(load_object::<Tree>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|giterror| match giterror {
                            GitError::IOError(io_error) => io_error,
                            _ => IoError::other(format!("{}", giterror)),
                        })?
                        .into_iter()
                        .flat_map(|t| {
                            t.get_items_with_mode()
                                .into_iter()
                                .map(|(_, hash, mode)| (hash, mode))
                        })
                        .filter(|(hash, _)| seen.insert(hash.to_string()))
                        .partition(|(_, mode)| *mode == TreeItemMode::Tree);

                    let trees = tree_hash
                        .into_iter()
                        .map(|(hash, _)| load_object::<Tree>(&hash))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|giterror| match giterror {
                            GitError::IOError(io_error) => io_error,
                            _ => IoError::other(format!("{}", giterror)),
                        })?;

                    let blobs = blob_hash
                        .into_iter()
                        .map(|(hash, _)| load_object::<Blob>(&hash))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|giterror| match giterror {
                            GitError::IOError(io_error) => io_error,
                            _ => IoError::other(format!("{}", giterror)),
                        })?;

                    let commit_entries: Vec<Entry> = commits.into_iter().map(Entry::from).collect();

                    let tree_entries: Vec<Entry> = trees.into_iter().map(Entry::from).collect();

                    let blob_entries: Vec<Entry> = blobs.into_iter().map(Entry::from).collect();

                    let mut all_entries = Vec::new();
                    all_entries.extend(commit_entries);
                    all_entries.extend(tree_entries);
                    all_entries.extend(blob_entries);
                    all_entries.extend(tag_entries);

                    // The Libra-native fetch path does not negotiate shallow
                    // boundaries, so no `shallow` lines are emitted.
                    encode_entries_to_fetch_response(all_entries, Vec::new(), repo_hash_kind).await
                })
                .await
            }
        }
    }
}

fn ordinary_transport_branch(name: &str) -> bool {
    OwnedRefSpec::for_transport_ref(name)
        .is_none_or(|spec| spec.transport_policy() != OwnedRefTransportPolicy::LocalOnly)
}

/// Read `objectformat` from a foreign Git repository's `config`, defaulting to
/// SHA-1 (the overwhelmingly common case for local Git remotes).
fn git_repo_hash_kind(repo_path: &Path) -> HashKind {
    if let Ok(text) = fs::read_to_string(repo_path.join("config")) {
        for line in text.lines() {
            let lower = line.to_ascii_lowercase();
            if lower.contains("objectformat") && lower.contains("sha256") {
                return HashKind::Sha256;
            }
        }
    }
    HashKind::Sha1
}

/// Read every ref a foreign Git repository advertises: loose `refs/**`,
/// `packed-refs`, and the symbolic/detached `HEAD`. Loose refs win over packed
/// entries of the same name. The returned list mirrors what
/// `git-upload-pack --advertise-refs` would produce, in-process.
fn read_git_repo_refs(repo_path: &Path) -> std::io::Result<Vec<DiscRef>> {
    // BTreeMap keeps the advertisement deterministic.
    let mut refs: BTreeMap<String, String> = BTreeMap::new();

    let refs_root = repo_path.join("refs");
    collect_loose_refs(&refs_root, &refs_root, &mut refs)?;

    if let Ok(text) = fs::read_to_string(repo_path.join("packed-refs")) {
        for line in text.lines() {
            let line = line.trim();
            // `^<oid>` peel lines are ignored — the peel is always derived from
            // the *advertised* tag object below, so a packed peel can never be
            // applied to a loose tag that shadows the packed entry.
            if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            if let Some((oid, name)) = line.split_once(' ') {
                refs.entry(name.trim().to_string())
                    .or_insert_with(|| oid.trim().to_string());
            }
        }
    }

    let mut out: Vec<DiscRef> = refs
        .into_iter()
        .map(|(name, oid)| DiscRef {
            _hash: oid,
            _ref: name,
        })
        .collect();

    // Advertise the peeled commit of every annotated tag as `<ref>^{}`, like
    // `git-upload-pack`. Always dereference the *advertised* tag object through
    // the (strictly local) object store, so loose/packed shadowing is correct.
    let storage = ClientStorage::init_local(repo_path.join("objects"));
    let tag_refs: Vec<(String, String)> = out
        .iter()
        .filter(|r| r._ref.starts_with("refs/tags/"))
        .map(|r| (r._ref.clone(), r._hash.clone()))
        .collect();
    for (name, oid) in tag_refs {
        if let Some(target) = peel_tag(&storage, &oid)
            && target != oid
        {
            out.push(DiscRef {
                _hash: target,
                _ref: format!("{name}^{{}}"),
            });
        }
    }

    // HEAD: a `ref: <target>` symref resolves to the target's oid; a bare oid is
    // a detached HEAD.
    if let Ok(head) = fs::read_to_string(repo_path.join("HEAD")) {
        let head = head.trim();
        if let Some(target) = head.strip_prefix("ref: ") {
            if let Some(hash) = out
                .iter()
                .find(|r| r._ref == target.trim())
                .map(|r| r._hash.clone())
            {
                out.push(DiscRef {
                    _hash: hash,
                    _ref: reflog::HEAD.to_string(),
                });
            }
        } else if !head.is_empty() {
            out.push(DiscRef {
                _hash: head.to_string(),
                _ref: reflog::HEAD.to_string(),
            });
        }
    }

    Ok(out)
}

/// Dereference an annotated tag (following nested tags) to its final non-tag
/// object id, reading from a strictly local store. Returns `None` on any read
/// failure so a malformed tag never breaks the whole advertisement.
fn peel_tag(storage: &ClientStorage, oid: &str) -> Option<String> {
    let mut current = ObjectHash::from_str(oid).ok()?;
    for _ in 0..32 {
        match storage.get_object_type(&current) {
            Ok(ObjectType::Tag) => {
                let tag = Tag::from_bytes(&storage.get(&current).ok()?, current).ok()?;
                current = tag.object_hash;
            }
            Ok(_) => return Some(current.to_string()),
            Err(_) => return None,
        }
    }
    None
}

/// If `HEAD` is a symbolic ref, return the `symref=HEAD:<target>` capability
/// string `git-upload-pack` would advertise (so `ls-remote --symref` keeps
/// printing HEAD's target). A detached HEAD has no symref capability.
fn git_repo_head_symref(repo_path: &Path) -> Option<String> {
    let head = fs::read_to_string(repo_path.join("HEAD")).ok()?;
    let target = head.trim().strip_prefix("ref: ")?.trim().to_string();
    Some(format!("symref=HEAD:{target}"))
}

/// Recursively collect loose refs under `dir` into `out`, keyed by their full
/// `refs/…` name.
fn collect_loose_refs(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, String>,
) -> std::io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect_loose_refs(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            let oid = fs::read_to_string(&path)?.trim().to_string();
            if oid.is_empty() {
                continue;
            }
            let name = format!("refs/{}", rel.to_string_lossy().replace('\\', "/"));
            out.insert(name, oid);
        }
    }
    Ok(())
}

/// Walk a foreign Git repository's object store and collect every object the
/// client needs: the wanted commits and their ancestors (minus `have`, bounded
/// by `depth`), every tree and blob they reference, and any annotated tag
/// objects (peeled to their target commit). Reads exclusively from `storage`
/// (the foreign `.git/objects`), never the current Libra repository.
fn collect_git_repo_entries(
    storage: &ClientStorage,
    repo_path: &Path,
    want: &[String],
    have: &[String],
    depth: Option<usize>,
) -> Result<(Vec<Entry>, Vec<String>), GitError> {
    let have_set: HashSet<String> = have.iter().cloned().collect();
    let mut seen: HashSet<String> = have_set.clone();
    let mut entries: Vec<Entry> = Vec::new();
    let mut commit_queue: VecDeque<(ObjectHash, usize)> = VecDeque::new();
    let mut tree_roots: Vec<ObjectHash> = Vec::new();
    // Commits whose parents are cut off by `depth` — the shallow boundary.
    let mut shallow: Vec<String> = Vec::new();

    // Resolve each want; peel annotated tags (emitting each tag object) down to
    // the commit they target.
    for spec in want {
        let Ok(oid) = ObjectHash::from_str(spec) else {
            continue;
        };
        if matches!(storage.get_object_type(&oid), Ok(ObjectType::Tag)) {
            let mut current = oid;
            for _ in 0..32 {
                if !seen.insert(current.to_string()) {
                    break;
                }
                let tag = Tag::from_bytes(&storage.get(&current)?, current)?;
                let target = tag.object_hash;
                entries.push(Entry::from(tag));
                match storage.get_object_type(&target) {
                    Ok(ObjectType::Tag) => current = target,
                    Ok(ObjectType::Commit) => {
                        commit_queue.push_back((target, 0));
                        break;
                    }
                    _ => break,
                }
            }
        } else {
            commit_queue.push_back((oid, 0));
        }
    }

    // Breadth-first over reachable commits.
    while let Some((oid, distance)) = commit_queue.pop_front() {
        if !seen.insert(oid.to_string()) {
            continue;
        }
        let commit = Commit::from_bytes(&storage.get(&oid)?, oid)?;
        tree_roots.push(commit.tree_id);
        let parents = commit.parent_commit_ids.clone();
        entries.push(Entry::from(commit));
        if depth.is_none_or(|max| distance + 1 < max) {
            for parent in parents {
                if !seen.contains(&parent.to_string()) {
                    commit_queue.push_back((parent, distance + 1));
                }
            }
        } else {
            // `depth` stops the walk here, so this commit is a shallow boundary
            // (advertised even for a root commit, matching `git-upload-pack`).
            shallow.push(oid.to_string());
        }
    }

    // Every tree and blob reachable from the collected commits.
    let mut tree_queue: VecDeque<ObjectHash> = tree_roots.into_iter().collect();
    while let Some(tree_oid) = tree_queue.pop_front() {
        if !seen.insert(tree_oid.to_string()) {
            continue;
        }
        let tree = Tree::from_bytes(&storage.get(&tree_oid)?, tree_oid)?;
        for item in &tree.tree_items {
            match item.mode {
                TreeItemMode::Tree => tree_queue.push_back(item.id),
                // A gitlink points at a commit in another repository.
                TreeItemMode::Commit => {}
                _ => {
                    if seen.insert(item.id.to_string()) {
                        let blob = Blob::from_bytes(&storage.get(&item.id)?, item.id)?;
                        entries.push(Entry::from(blob));
                    }
                }
            }
        }
        entries.push(Entry::from(tree));
    }

    // include-tag: like `git-upload-pack`, also send an annotated tag object
    // whose target commit is being sent, so the receiver can create
    // `refs/tags/<name>` for tags it auto-follows.
    include_reachable_tags(storage, repo_path, &have_set, &mut seen, &mut entries)?;

    Ok((entries, shallow))
}

/// Add annotated tag objects whose peeled target is in the just-sent set.
fn include_reachable_tags(
    storage: &ClientStorage,
    repo_path: &Path,
    have_set: &HashSet<String>,
    seen: &mut HashSet<String>,
    entries: &mut Vec<Entry>,
) -> Result<(), GitError> {
    let Ok(refs) = read_git_repo_refs(repo_path) else {
        return Ok(());
    };
    // `refs/tags/<name>` -> peeled target oid, for annotated tags only.
    let peeled: BTreeMap<String, String> = refs
        .iter()
        .filter_map(|r| {
            r._ref
                .strip_suffix("^{}")
                .map(|base| (base.to_string(), r._hash.clone()))
        })
        .collect();

    for r in &refs {
        if r._ref.ends_with("^{}") || !r._ref.starts_with("refs/tags/") {
            continue;
        }
        let Some(target) = peeled.get(&r._ref) else {
            continue; // lightweight tag: no tag object to send
        };
        // Send only when the target was just packed (not merely already had) and
        // the tag object itself has not been sent yet.
        if !seen.contains(target) || have_set.contains(target) || seen.contains(&r._hash) {
            continue;
        }
        if let Ok(oid) = ObjectHash::from_str(&r._hash)
            && matches!(storage.get_object_type(&oid), Ok(ObjectType::Tag))
        {
            let tag = Tag::from_bytes(&storage.get(&oid)?, oid)?;
            seen.insert(r._hash.clone());
            entries.push(Entry::from(tag));
        }
    }
    Ok(())
}

/// Encode `entries` into a v2 pack and wrap it in the upload-pack wire response
/// (`NAK`, then the pack over sideband-64k channel 1 with channel-2 progress,
/// then a flush). Shared by the Libra-native and foreign-Git fetch paths.
/// A valid empty v2 pack: the 12-byte header followed by its trailer hashed in
/// the repository's hash kind. Sent when an up-to-date fetch has nothing to give.
fn empty_pack_bytes(hash_kind: HashKind) -> Vec<u8> {
    let mut pack = Vec::with_capacity(32);
    pack.extend_from_slice(b"PACK");
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&0u32.to_be_bytes());
    match hash_kind {
        HashKind::Sha1 => {
            use sha1::Digest;
            let mut hasher = sha1::Sha1::new();
            hasher.update(&pack);
            pack.extend_from_slice(&hasher.finalize());
        }
        HashKind::Sha256 => {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(&pack);
            pack.extend_from_slice(&hasher.finalize());
        }
    }
    pack
}

/// Encode `entries` (non-empty) into a v2 pack, propagating the repository hash
/// kind into the encoder's spawned task.
async fn encode_pack_bytes(entries: Vec<Entry>, hash_kind: HashKind) -> Result<Vec<u8>, IoError> {
    let (entry_tx, entry_rx) = tokio::sync::mpsc::channel::<MetaAttached<Entry, EntryMeta>>(1_000);
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(1_000);

    let total_objects = entries.len();
    let encode_handle = tokio::spawn(async move {
        // Set the hash kind BEFORE constructing the encoder: `PackEncoder::new`
        // initializes the pack-trailer hasher from the thread-local, so it must
        // see the repository's kind (not whatever this worker thread last had).
        set_hash_kind(hash_kind);
        let mut encoder = PackEncoder::new(total_objects, 0, stream_tx);
        encoder.encode(entry_rx).await
    });

    for entry in entries {
        let meta_entry = MetaAttached {
            inner: entry,
            meta: EntryMeta::default(),
        };
        if let Err(e) = entry_tx.send(meta_entry).await {
            return Err(IoError::other(format!("Failed to send entry: {}", e)));
        }
    }
    drop(entry_tx);

    let mut pack_data = Vec::new();
    while let Some(chunk) = stream_rx.recv().await {
        pack_data.extend(chunk);
    }
    encode_handle
        .await
        .map_err(|e| IoError::other(format!("Encode task panicked: {}", e)))?
        .map_err(|e| IoError::other(format!("Pack encoding failed: {}", e)))?;
    Ok(pack_data)
}

async fn encode_entries_to_fetch_response(
    entries: Vec<Entry>,
    shallow: Vec<String>,
    hash_kind: HashKind,
) -> Result<FetchStream, IoError> {
    // An up-to-date fetch produces no objects; `PackEncoder` panics on a
    // zero-object pack, so emit a valid empty pack (header + trailer) instead.
    // The hash kind is passed explicitly so we never depend on a thread-local
    // held across this `.await` (Tokio may resume on another worker thread).
    let pack_data = if entries.is_empty() {
        empty_pack_bytes(hash_kind)
    } else {
        encode_pack_bytes(entries, hash_kind).await?
    };

    if pack_data.len() < 12 || &pack_data[0..4] != b"PACK" {
        return Err(IoError::other("Invalid pack signature"));
    }

    let mut response_data = Vec::new();

    // Shallow boundary section: `shallow <oid>` pkt-lines, then a flush, before
    // the NAK — so the receiver can persist the shallow metadata.
    if !shallow.is_empty() {
        for oid in &shallow {
            let line = format!("shallow {oid}\n");
            let len_hex = format!("{:04x}", line.len() + 4);
            response_data.extend_from_slice(len_hex.as_bytes());
            response_data.extend_from_slice(line.as_bytes());
        }
        response_data.extend_from_slice(b"0000");
    }

    let nak_line = "NAK\n";
    let nak_len_hex = format!("{:04x}", nak_line.len() + 4);
    response_data.extend_from_slice(nak_len_hex.as_bytes());
    response_data.extend_from_slice(nak_line.as_bytes());

    let chunk_size = 65500;
    for chunk in pack_data.chunks(chunk_size) {
        let mut sideband_data = Vec::with_capacity(1 + chunk.len());
        sideband_data.push(1);
        sideband_data.extend_from_slice(chunk);
        let len_hex = format!("{:04x}", sideband_data.len() + 4);
        response_data.extend_from_slice(len_hex.as_bytes());
        response_data.extend_from_slice(&sideband_data);

        const PROGRESS_CHUNK_INTERVAL: usize = 10;
        if response_data.len() % (chunk_size * PROGRESS_CHUNK_INTERVAL) == 0 {
            let progress_msg = format!("Pack {}/{}...\n", response_data.len(), pack_data.len());
            let mut progress_data = Vec::with_capacity(1 + progress_msg.len());
            progress_data.push(2);
            progress_data.extend_from_slice(progress_msg.as_bytes());
            let progress_len_hex = format!("{:04x}", progress_data.len() + 4);
            response_data.extend_from_slice(progress_len_hex.as_bytes());
            response_data.extend_from_slice(&progress_data);
        }
    }
    response_data.extend_from_slice(b"0000");

    let response_stream = stream::iter(vec![Ok(Bytes::from(response_data))]);
    Ok(Box::pin(response_stream) as FetchStream)
}

fn tag_object_hash(object: &tag::TagObject) -> String {
    match object {
        tag::TagObject::Commit(commit) => commit.id.to_string(),
        tag::TagObject::Tag(tag) => tag.id.to_string(),
        tag::TagObject::Tree(tree) => tree.id.to_string(),
        tag::TagObject::Blob(blob) => blob.id.to_string(),
    }
}

async fn tag_refs(tag: tag::Tag) -> Result<Vec<DiscRef>, GitError> {
    let refname = format!("refs/tags/{}", tag.name);
    let mut refs = vec![DiscRef {
        _hash: tag_object_hash(&tag.object),
        _ref: refname.clone(),
    }];

    if let tag::TagObject::Tag(tag_object) = tag.object {
        refs.push(DiscRef {
            _hash: peel_tag_object_hash(tag_object.object_hash, &refname).await?,
            _ref: format!("{refname}^{{}}"),
        });
    }

    Ok(refs)
}

async fn peel_tag_object_hash(
    mut object_hash: git_internal::hash::ObjectHash,
    refname: &str,
) -> Result<String, GitError> {
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(object_hash) {
            return Err(GitError::CustomError(format!(
                "detected cycle while peeling tag '{refname}'"
            )));
        }

        match tag::load_object_trait(&object_hash).await? {
            tag::TagObject::Commit(commit) => return Ok(commit.id.to_string()),
            tag::TagObject::Tree(tree) => return Ok(tree.id.to_string()),
            tag::TagObject::Blob(blob) => return Ok(blob.id.to_string()),
            tag::TagObject::Tag(tag) => object_hash = tag.object_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, future::pending, process::Command as StdCommand};

    use serial_test::serial;
    use tempfile::tempdir;
    use tokio::{
        io::AsyncReadExt,
        sync::{mpsc, oneshot},
        time::{Duration, timeout},
    };
    use tokio_util::io::StreamReader;

    use super::*;
    use crate::{
        git_protocol::ServiceType,
        utils::test::{ChangeDirGuard, setup_with_new_libra_in},
    };

    fn run_git<I, S>(cwd: Option<&Path>, args: I) -> StdCommand
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut cmd = StdCommand::new("git");
        if let Some(path) = cwd {
            cmd.current_dir(path);
        }
        cmd.args(args);
        cmd
    }

    #[tokio::test]
    async fn discovery_reference_empty_repo_returns_refs() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join("empty.git");
        run_git(None, ["init", "--bare", repo_path.to_str().unwrap()])
            .status()
            .unwrap();

        let client = LocalClient::from_path(&repo_path).unwrap();
        let refs = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .unwrap();
        assert!(refs.refs.is_empty());
    }

    #[tokio::test]
    async fn git_repo_discovery_lists_branch_and_head_in_process() {
        // A non-bare Git repo with one commit: in-process discovery must list the
        // branch ref and a matching HEAD without spawning git-upload-pack.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("work");
        assert!(
            run_git(None, ["init", repo.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        for (k, v) in [
            ("user.name", "Local Tester"),
            ("user.email", "local@test"),
            ("commit.gpgsign", "false"),
        ] {
            run_git(Some(&repo), ["config", k, v]).status().unwrap();
        }
        std::fs::write(repo.join("a.txt"), "content").unwrap();
        run_git(Some(&repo), ["add", "a.txt"]).status().unwrap();
        assert!(
            run_git(Some(&repo), ["commit", "-m", "c1"])
                .status()
                .unwrap()
                .success()
        );
        let head = String::from_utf8(
            run_git(Some(&repo), ["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let client = LocalClient::from_path(&repo).unwrap();
        let result = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .unwrap();

        // A branch ref pointing at HEAD's commit, plus a HEAD entry with the same oid.
        assert!(
            result
                .refs
                .iter()
                .any(|r| r._ref.starts_with("refs/heads/") && r._hash == head),
            "discovery should list the branch ref at {head}: {:?}",
            result
                .refs
                .iter()
                .map(|r| r._ref.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            result
                .refs
                .iter()
                .any(|r| r._ref == reflog::HEAD && r._hash == head),
            "discovery should list HEAD at {head}"
        );
        // HEAD's symref target is advertised so `ls-remote --symref` keeps working.
        assert!(
            result
                .capabilities
                .iter()
                .any(|cap| cap.starts_with("symref=HEAD:refs/heads/")),
            "discovery should advertise the HEAD symref: {:?}",
            result.capabilities
        );
    }

    #[tokio::test]
    async fn git_repo_discovery_advertises_peeled_annotated_tag() {
        // An annotated tag must be advertised with its `refs/tags/<n>^{}` peel
        // (the target commit), like `git-upload-pack --advertise-refs`.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("work");
        assert!(
            run_git(None, ["init", repo.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        for (k, v) in [
            ("user.name", "Local Tester"),
            ("user.email", "local@test"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ] {
            run_git(Some(&repo), ["config", k, v]).status().unwrap();
        }
        std::fs::write(repo.join("a.txt"), "content").unwrap();
        run_git(Some(&repo), ["add", "a.txt"]).status().unwrap();
        assert!(
            run_git(Some(&repo), ["commit", "-m", "c1"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_git(Some(&repo), ["tag", "-a", "v1", "-m", "release v1"])
                .status()
                .unwrap()
                .success()
        );
        let commit = String::from_utf8(
            run_git(Some(&repo), ["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let client = LocalClient::from_path(&repo).unwrap();
        let result = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .unwrap();

        assert!(
            result.refs.iter().any(|r| r._ref == "refs/tags/v1"),
            "the annotated tag ref should be advertised: {:?}",
            result
                .refs
                .iter()
                .map(|r| r._ref.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            result
                .refs
                .iter()
                .any(|r| r._ref == "refs/tags/v1^{}" && r._hash == commit),
            "the peeled tag ref should point at the commit {commit}: {:?}",
            result
                .refs
                .iter()
                .map(|r| r._ref.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn fetch_objects_produces_pack_stream() {
        let temp = tempdir().unwrap();
        let remote_path = temp.path().join("remote.git");
        let work_path = temp.path().join("work");

        assert!(
            run_git(None, ["init", "--bare", remote_path.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_git(None, ["init", work_path.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_git(Some(&work_path), ["config", "user.name", "Local Tester"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_git(Some(&work_path), ["config", "user.email", "local@test"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_git(Some(&work_path), ["config", "commit.gpgsign", "false"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(work_path.join("README.md"), "hello world").unwrap();
        assert!(
            run_git(Some(&work_path), ["add", "README.md"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            run_git(Some(&work_path), ["commit", "-m", "initial commit"])
                .status()
                .unwrap()
                .success()
        );

        let branch = String::from_utf8(
            run_git(Some(&work_path), ["rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        assert!(
            run_git(
                Some(&work_path),
                ["remote", "add", "origin", remote_path.to_str().unwrap()],
            )
            .status()
            .unwrap()
            .success()
        );
        assert!(
            run_git(
                Some(&work_path),
                ["push", "origin", &format!("HEAD:refs/heads/{branch}"),],
            )
            .status()
            .unwrap()
            .success()
        );

        let head = String::from_utf8(
            run_git(Some(&work_path), ["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        let client = LocalClient::from_path(&remote_path).unwrap();
        let refs = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .unwrap();
        assert!(!refs.refs.is_empty());

        let want = vec![head];
        let have = Vec::new();
        let stream = client.fetch_objects(&have, &want, &[], None).await.unwrap();
        let mut reader = StreamReader::new(stream);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        assert!(buf.windows(4).any(|w| w == b"PACK"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn fetch_objects_propagates_reachable_commit_walk_errors() {
        let repo_dir = tempdir().unwrap();
        setup_with_new_libra_in(repo_dir.path()).await;

        let client = LocalClient::from_path(repo_dir.path()).unwrap();
        let want = vec!["not-a-valid-hash".to_string()];
        let error = match client.fetch_objects(&[], &want, &[], None).await {
            Ok(_) => panic!("invalid want should fail instead of returning an empty pack"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(
            message.contains("failed to walk reachable commits for 'not-a-valid-hash'"),
            "fetch_objects should preserve the failing want hash, got: {message}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn libra_discovery_hides_exact_local_only_memory_ref() {
        let repo_dir = tempdir().unwrap();
        setup_with_new_libra_in(repo_dir.path()).await;
        let _guard = ChangeDirGuard::new(repo_dir.path());
        let oid = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
        Branch::update_branch("main", oid, None).await.unwrap();
        Branch::update_branch("libra/memory/repo", oid, None)
            .await
            .unwrap();
        Branch::update_branch("libra/memory/repo-user", oid, None)
            .await
            .unwrap();

        let client = LocalClient::from_path(repo_dir.path()).unwrap();
        let result = client
            .discovery_reference(ServiceType::UploadPack)
            .await
            .unwrap();
        let names: HashSet<&str> = result.refs.iter().map(|item| item._ref.as_str()).collect();
        assert!(names.contains("refs/heads/main"));
        assert!(names.contains("refs/heads/libra/memory/repo-user"));
        assert!(!names.contains("refs/heads/libra/memory/repo"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn with_repo_current_dir_restores_current_dir_when_task_is_cancelled() {
        let caller_dir = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();
        let _guard = ChangeDirGuard::new(caller_dir.path());
        setup_with_new_libra_in(repo_dir.path()).await;

        let client = LocalClient::from_path(repo_dir.path()).unwrap();
        let original_dir = env::current_dir().unwrap();
        let repo_storage_dir = client.repo_path().to_path_buf();
        let (entered_tx, entered_rx) = oneshot::channel();

        let handle = tokio::spawn({
            let client = client.clone();
            async move {
                let _ = client
                    .with_repo_current_dir(|| async move {
                        let _ = entered_tx.send(env::current_dir().unwrap());
                        pending::<()>().await;
                        #[allow(unreachable_code)]
                        Ok::<(), IoError>(())
                    })
                    .await;
            }
        });

        let entered_dir = entered_rx.await.unwrap();
        assert_eq!(
            fs::canonicalize(entered_dir).unwrap(),
            fs::canonicalize(repo_storage_dir).unwrap()
        );

        handle.abort();
        let _ = handle.await;

        assert_eq!(
            fs::canonicalize(env::current_dir().unwrap()).unwrap(),
            fs::canonicalize(original_dir).unwrap(),
            "aborted local protocol operation should restore caller cwd",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn with_repo_current_dir_serializes_concurrent_operations() {
        let caller_dir = tempdir().unwrap();
        let repo_a = tempdir().unwrap();
        let repo_b = tempdir().unwrap();
        let _guard = ChangeDirGuard::new(caller_dir.path());
        setup_with_new_libra_in(repo_a.path()).await;
        setup_with_new_libra_in(repo_b.path()).await;

        let client_a = LocalClient::from_path(repo_a.path()).unwrap();
        let client_b = LocalClient::from_path(repo_b.path()).unwrap();
        let repo_a_storage_dir = client_a.repo_path().to_path_buf();
        let repo_b_storage_dir = client_b.repo_path().to_path_buf();
        let original_dir = env::current_dir().unwrap();
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel::<(u8, PathBuf)>();
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let handle_a = tokio::spawn({
            let client = client_a.clone();
            let entered_tx = entered_tx.clone();
            async move {
                client
                    .with_repo_current_dir(|| async move {
                        let _ = entered_tx.send((1, env::current_dir().unwrap()));
                        let _ = release_rx.await;
                        Ok::<(), IoError>(())
                    })
                    .await
                    .unwrap();
            }
        });

        let (first_id, first_dir) = entered_rx.recv().await.unwrap();
        assert_eq!(first_id, 1);
        assert_eq!(
            fs::canonicalize(first_dir).unwrap(),
            fs::canonicalize(repo_a_storage_dir).unwrap()
        );

        let handle_b = tokio::spawn({
            let client = client_b.clone();
            let entered_tx = entered_tx.clone();
            async move {
                client
                    .with_repo_current_dir(|| async move {
                        let _ = entered_tx.send((2, env::current_dir().unwrap()));
                        Ok::<(), IoError>(())
                    })
                    .await
                    .unwrap();
            }
        });

        assert!(
            timeout(Duration::from_millis(100), entered_rx.recv())
                .await
                .is_err(),
            "concurrent local protocol operations should serialize cwd changes",
        );

        release_tx.send(()).unwrap();

        let (second_id, second_dir) = timeout(Duration::from_secs(5), entered_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second_id, 2);
        assert_eq!(
            fs::canonicalize(second_dir).unwrap(),
            fs::canonicalize(repo_b_storage_dir).unwrap()
        );

        handle_a.await.unwrap();
        handle_b.await.unwrap();

        assert_eq!(
            fs::canonicalize(env::current_dir().unwrap()).unwrap(),
            fs::canonicalize(original_dir).unwrap(),
            "serialized local protocol operations should restore caller cwd",
        );
    }
}
