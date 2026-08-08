//! `libra replace` — substitute one object for another whenever an object is
//! read, a focused subset of `git replace`.
//!
//! A replacement is stored as a loose ref under `.libra/refs/replace/<oid>`
//! whose content is the replacement oid (Git's `refs/replace/` namespace). The
//! peel happens in [`crate::command::load_object`] via [`resolve`], so every
//! reader that goes through `load_object` (`log`, `show`, `rev-parse` peeling,
//! …) transparently sees the replacement — not just one call site.
//!
//! Integrating these loose refs into the SQLite reference table (so `show-ref` /
//! `for-each-ref` list them) and `--graft` / `--edit` / `--convert-graft-file`
//! are documented follow-ups.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use git_internal::hash::ObjectHash;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::OutputConfig,
    util,
};

const REPLACE_REF_DIR: &str = "refs/replace";
/// Bound on how many `refs/replace` hops are followed, so a cycle or a long
/// chain can never spin forever inside the hot object-load path.
const MAX_REPLACE_DEPTH: usize = 8;

/// W0 §C.4.1.1 process-cache rules, applied by NOT caching: this was a
/// single process-global `OnceLock<HashMap>` loaded from whichever
/// repository the process touched first — a long-lived host process
/// (service, code runtime) then resolved repo B's objects through repo A's
/// replacements, and any mutation (its own or another process's) stayed
/// invisible until restart. [`replace_map`] now reads `refs/replace` fresh
/// on every call; see its doc for why that is affordable.
type ReplaceMap = HashMap<ObjectHash, ObjectHash>;

/// The empty map returned whenever no replacements exist — shared so the
/// common case allocates nothing.
static EMPTY_REPLACE_MAP: std::sync::LazyLock<std::sync::Arc<ReplaceMap>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(HashMap::new()));

/// This repository's CURRENT replacement map, read fresh from
/// `refs/replace` on every call (§C.4.1.1 process-cache rules).
///
/// No snapshot is retained at all: every staleness class — another
/// process's create/delete, a same-name `-f` retarget with unchanged
/// metadata, this process's own mutations — is structurally impossible.
/// The cost model that makes this acceptable on the hot object-load path:
/// the overwhelmingly common case (no replacements, directory absent) is a
/// single failed `read_dir` returning the shared empty map; when
/// replacements DO exist they number a handful, and reading a handful of
/// 41-byte ref files per resolve is bounded and far below the object read
/// the caller is about to perform.
fn replace_map() -> std::sync::Arc<ReplaceMap> {
    let Some(dir) = replace_dir() else {
        // Outside a repository nothing can be replaced.
        return EMPTY_REPLACE_MAP.clone();
    };
    let map = load_replace_map(&dir);
    if map.is_empty() {
        return EMPTY_REPLACE_MAP.clone();
    }
    std::sync::Arc::new(map)
}

tokio::task_local! {
    /// A PINNED replacement snapshot for callers that must hold ONE
    /// immutable map across a signature stamp AND an object walk (the
    /// revision-ordinal builder): with fresh per-call reads, a concurrent
    /// retarget between the stamp and the walk could otherwise make the
    /// stamped signature describe a different map than the chain actually
    /// indexed.
    static PINNED_SNAPSHOT: std::sync::Arc<ReplaceMap>;
}

/// One immutable snapshot of the CURRENT replacement map.
pub(crate) fn snapshot() -> std::sync::Arc<ReplaceMap> {
    replace_map()
}

/// Run `f` with every [`resolve`] (and [`effective_replace_signature`]) in
/// this task answering from `map` instead of re-reading `refs/replace`.
pub(crate) fn with_pinned_snapshot_sync<R>(
    map: std::sync::Arc<ReplaceMap>,
    f: impl FnOnce() -> R,
) -> R {
    PINNED_SNAPSHOT.sync_scope(map, f)
}

tokio::task_local! {
    /// When set on the current task, [`resolve`] is a no-op. The `replace`
    /// command names objects by their *literal* oid, so it suppresses the peel
    /// while resolving its own arguments (otherwise creating a replacement would
    /// change how its own arguments resolve — e.g. `HEAD~1` after `HEAD` was
    /// already replaced). Task-local (not process-global) so a concurrent task
    /// on the multi-thread runtime is never affected.
    static SUPPRESS_PEEL: bool;
}

fn peel_suppressed() -> bool {
    SUPPRESS_PEEL
        .try_with(|suppressed| *suppressed)
        .unwrap_or(false)
}

/// Signature of the EFFECTIVE replacement map. Under a pinned snapshot it
/// describes exactly the map [`resolve`] answers from (the ordinal builder's
/// coherence invariant); otherwise it digests a fresh read, so any replace
/// mutation — this process's or another's — yields a differing signature
/// that triggers an honest rebuild on the next read (lore.md 1.16).
pub(crate) fn effective_replace_signature() -> String {
    if let Ok(signature) = PINNED_SNAPSHOT.try_with(|map| signature_of(map)) {
        return signature;
    }
    signature_of(&replace_map())
}

/// The signature of one SPECIFIC snapshot (see
/// [`effective_replace_signature`] for the ambient form).
pub(crate) fn signature_of(map: &ReplaceMap) -> String {
    if map.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<String> = map
        .iter()
        .map(|(from, to)| format!("{from}={to}"))
        .collect();
    pairs.sort();
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    for pair in &pairs {
        hasher.update(pair.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub const REPLACE_EXAMPLES: &str = "\
EXAMPLES:
    libra replace <object> <replacement>   Replace one object with another on read
    libra replace -f <object> <repl>       Replace even across object types / overwrite
    libra replace -d <object>...           Delete replacement(s)
    libra replace -l [<pattern>]           List replaced object ids (the default)";

/// Create, list, or delete object replacements (`refs/replace/*`).
#[derive(Parser, Debug)]
#[command(after_help = REPLACE_EXAMPLES)]
pub struct ReplaceArgs {
    /// Overwrite an existing replacement and allow a type mismatch.
    #[clap(short = 'f', long)]
    pub force: bool,

    /// Delete the replacement for each given object.
    #[clap(short = 'd', long)]
    pub delete: bool,

    /// List replaced object ids (optionally filtered by a substring).
    #[clap(short = 'l', long)]
    pub list: bool,

    /// Objects / replacement (see EXAMPLES).
    #[clap(value_name = "ARG")]
    pub args: Vec<String>,
}

// ----------------------------------------------------------------------------
// peel hook — called by `command::load_object`
// ----------------------------------------------------------------------------

/// Resolve an object id through `refs/replace`, following a chain (cycle-bounded).
/// The unpinned form reads only the CHAIN's ref files by name — the common
/// no-replacement case is ONE failed open of `refs/replace/<oid>` (plus the
/// same ancestor walk the object read itself already performs) — bounded
/// overhead on the hot object-load path, paid for always-current
/// cross-repository/cross-process correctness.
pub fn resolve(hash: ObjectHash) -> ObjectHash {
    if peel_suppressed() {
        return hash;
    }
    // Under a pinned snapshot (the ordinal builder), answer from the pin.
    if let Ok(pinned) = PINNED_SNAPSHOT.try_with(|map| map.clone()) {
        return resolve_via(&pinned, hash);
    }
    // HOT PATH: read only the CHAIN's ref files by name — O(chain hops,
    // capped) per resolve regardless of how many replacements the
    // repository holds, and the overwhelmingly common case (no replacement
    // for this oid) is a single failed open of `refs/replace/<oid>`. Every
    // read is fresh, so any process's create/delete/retarget is visible
    // immediately.
    let Some(dir) = replace_dir() else {
        return hash;
    };
    let mut current = hash;
    let mut visited = HashSet::new();
    visited.insert(current);
    for _ in 0..MAX_REPLACE_DEPTH {
        match read_one_replace_ref(&dir, &current) {
            Some(next) if visited.insert(next) => current = next,
            _ => break,
        }
    }
    current
}

/// Follow the chain within one immutable map (pinned-snapshot form).
fn resolve_via(map: &ReplaceMap, hash: ObjectHash) -> ObjectHash {
    if map.is_empty() {
        return hash;
    }
    // Cycle-bounded exactly like the per-file walk.
    let mut current = hash;
    let mut visited = HashSet::new();
    visited.insert(current);
    for _ in 0..MAX_REPLACE_DEPTH {
        match map.get(&current) {
            Some(&next) if visited.insert(next) => current = next,
            _ => break,
        }
    }
    current
}

/// One replacement hop read directly by filename (malformed content is
/// skipped, matching the loader's robustness-over-strictness stance).
fn read_one_replace_ref(dir: &Path, hash: &ObjectHash) -> Option<ObjectHash> {
    let content = fs::read_to_string(dir.join(hash.to_string())).ok()?;
    ObjectHash::from_str(content.trim()).ok()
}

/// Scan `.libra/refs/replace/` into a map. Best-effort: a malformed entry is
/// skipped rather than breaking every object read (robustness over strictness).
fn load_replace_map(dir: &Path) -> HashMap<ObjectHash, ObjectHash> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return map; // absent directory ⇒ no replacements
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(src) = ObjectHash::from_str(name) else {
            continue;
        };
        let Ok(content) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(dst) = ObjectHash::from_str(content.trim()) else {
            continue;
        };
        map.insert(src, dst);
    }
    map
}

fn replace_dir() -> Option<PathBuf> {
    // Request-pinned storage first (§C.4.2): a host process serving several
    // worktrees/repositories must key and load the map for the repository
    // THIS invocation acts on, not for wherever the process cwd points.
    if let Some(pinned) = crate::internal::worktree_scope::WorktreeScope::request_scope() {
        return Some(pinned.storage.join(REPLACE_REF_DIR));
    }
    util::try_get_storage_path(None)
        .ok()
        .map(|root| root.join(REPLACE_REF_DIR))
}

// ----------------------------------------------------------------------------
// CLI
// ----------------------------------------------------------------------------

pub async fn execute(args: ReplaceArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: ReplaceArgs, _output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    // The `replace` command names objects by their literal oid, so the peel must
    // not rewrite its own argument resolution. Scope the suppression to this
    // task so concurrent tasks on the runtime are unaffected.
    SUPPRESS_PEEL.scope(true, run(args)).await
}

async fn run(args: ReplaceArgs) -> CliResult<()> {
    if args.delete {
        if args.args.is_empty() {
            return Err(usage("`replace -d` needs at least one object"));
        }
        return delete(&args.args).await;
    }
    if args.list || args.args.len() <= 1 {
        return list(args.args.first().map(String::as_str));
    }
    if args.args.len() == 2 {
        return create(&args.args[0], &args.args[1], args.force).await;
    }
    Err(usage(
        "too many arguments: use `replace <object> <replacement>`, `-d <object>...`, or `-l`",
    ))
}

async fn create(object: &str, replacement: &str, force: bool) -> CliResult<()> {
    let obj = resolve_any(object).await?;
    let repl = resolve_any(replacement).await?;
    if obj == repl {
        return Err(fatal(format!("cannot replace object {obj} with itself")));
    }

    // Git refuses a cross-type replacement unless forced.
    let storage = util::objects_storage();
    let obj_type = storage
        .get_object_type(&obj)
        .map_err(|error| fatal(format!("cannot read object {obj}: {error}")))?;
    let repl_type = storage
        .get_object_type(&repl)
        .map_err(|error| fatal(format!("cannot read object {repl}: {error}")))?;
    if obj_type != repl_type && !force {
        return Err(fatal(format!(
            "object {obj} is a {obj_type} but {repl} is a {repl_type}; pass -f to force"
        )));
    }

    let dir = replace_dir().ok_or_else(CliError::repo_not_found)?;
    let path = dir.join(obj.to_string());
    if path.exists() && !force {
        return Err(fatal(format!(
            "replacement for {obj} already exists; pass -f to overwrite"
        )));
    }
    fs::create_dir_all(&dir).map_err(write_err)?;
    // No cache invalidation is needed: `replace_map` reads `refs/replace`
    // fresh on every call, so this write is visible to the next resolve in
    // EVERY process, including this one.
    fs::write(&path, format!("{repl}\n")).map_err(write_err)?;
    Ok(())
}

async fn delete(objects: &[String]) -> CliResult<()> {
    let dir = replace_dir().ok_or_else(CliError::repo_not_found)?;
    // No cache invalidation is needed: `replace_map` reads `refs/replace`
    // fresh on every call, so removals — even from a partially-failed
    // batch — are visible to the next resolve in every process.
    for spec in objects {
        let obj = resolve_any(spec).await?;
        let path = dir.join(obj.to_string());
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(fatal(format!("no replacement for {obj}")));
            }
            Err(error) => return Err(write_err(error)),
        }
    }
    Ok(())
}

fn list(pattern: Option<&str>) -> CliResult<()> {
    let Some(dir) = replace_dir() else {
        return Ok(());
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(read_err(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(read_err)?;
        if let Some(name) = entry.file_name().to_str()
            && ObjectHash::from_str(name).is_ok()
            && pattern.is_none_or(|p| name.contains(p))
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    for name in names {
        println!("{name}");
    }
    Ok(())
}

/// Resolve an argument to an object id: a full object-hash string of any type
/// that exists, otherwise a commit-ish / ref via `get_commit_base`.
async fn resolve_any(spec: &str) -> CliResult<ObjectHash> {
    if let Ok(hash) = ObjectHash::from_str(spec)
        && util::objects_storage().get(&hash).is_ok()
    {
        return Ok(hash);
    }
    util::get_commit_base(spec)
        .await
        .map_err(|error| fatal(format!("not a valid object '{spec}': {error}")))
}

fn usage(message: &str) -> CliError {
    CliError::command_usage(message.to_string())
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidArguments)
}

fn fatal(message: String) -> CliError {
    CliError::fatal(message)
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

fn read_err(error: std::io::Error) -> CliError {
    CliError::fatal(format!("failed to read refs/replace: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoReadFailed)
}

fn write_err(error: std::io::Error) -> CliError {
    CliError::fatal(format!("failed to write refs/replace: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::worktree_scope::WorktreeScope;

    fn oid(byte: u8) -> ObjectHash {
        ObjectHash::from_str(&format!("{byte:02x}").repeat(20)).expect("valid test oid")
    }

    fn repo_fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("tempdir");
        let gitdir = root.path().join(".libra");
        fs::create_dir_all(&gitdir).expect("gitdir");
        fs::write(gitdir.join("libra.db"), b"").expect("db marker");
        root
    }

    fn write_replace_ref(root: &Path, from: &ObjectHash, to: &ObjectHash) {
        let dir = root.join(".libra").join(REPLACE_REF_DIR);
        fs::create_dir_all(&dir).expect("refs/replace");
        fs::write(dir.join(from.to_string()), format!("{to}\n")).expect("replace ref");
    }

    /// W0 §C.4.1.1 process-cache rules: each repository resolves through ITS
    /// OWN replace map. Under the retired process-global `OnceLock`, repo B
    /// inherited whichever map the process loaded first.
    #[test]
    #[serial_test::serial]
    fn replace_map_is_keyed_per_repository() {
        let repo_a = repo_fixture();
        let repo_b = repo_fixture();
        let from = oid(0xaa);
        let to = oid(0xbb);
        write_replace_ref(repo_a.path(), &from, &to);

        {
            let _pin = WorktreeScope::pin_request_scope(repo_a.path().to_path_buf());
            assert_eq!(resolve(from), to, "repo A resolves its own replacement");
        }
        {
            let _pin = WorktreeScope::pin_request_scope(repo_b.path().to_path_buf());
            assert_eq!(
                resolve(from),
                from,
                "repo B must NOT resolve through repo A's map"
            );
        }
    }

    /// §C.4.1.1 freshness: a replace mutation performed by ANOTHER PROCESS
    /// (simulated by direct on-disk writes) is visible to this process's
    /// next resolve — the map is fingerprint-keyed, not frozen per process.
    #[test]
    #[serial_test::serial]
    fn external_replace_mutations_are_visible() {
        let repo = repo_fixture();
        let from = oid(0xcc);
        let to = oid(0xdd);

        let _pin = WorktreeScope::pin_request_scope(repo.path().to_path_buf());
        assert_eq!(resolve(from), from, "no replacements yet");

        // "Another process" creates a replacement: the next resolve here
        // must see it without any in-process invalidation.
        write_replace_ref(repo.path(), &from, &to);
        assert_eq!(
            resolve(from),
            to,
            "an external create must be visible to a long-lived process"
        );

        // "Another process" RETARGETS the same ref file in place — same
        // name, same content length (fixed-size oid), no membership change.
        // This is the staleness class a metadata fingerprint cannot see.
        let retarget = oid(0xde);
        write_replace_ref(repo.path(), &from, &retarget);
        assert_eq!(
            resolve(from),
            retarget,
            "an external same-name retarget must be visible immediately"
        );

        // "Another process" deletes it again.
        fs::remove_file(
            repo.path()
                .join(".libra")
                .join(REPLACE_REF_DIR)
                .join(from.to_string()),
        )
        .expect("remove ref out of band");
        assert_eq!(
            resolve(from),
            from,
            "an external delete must be visible to a long-lived process"
        );
    }

    /// The ordinal builder's invariant (§C.4.1.1): under a pinned snapshot,
    /// a concurrent retarget is invisible to BOTH `resolve` and the
    /// signature — they answer from the same immutable map, so the stamp
    /// can never describe a different map than the walk used; after the pin
    /// exits, the mutation is immediately visible.
    #[test]
    #[serial_test::serial]
    fn pinned_snapshot_keeps_signature_and_resolution_coherent() {
        let repo = repo_fixture();
        let from = oid(0xa1);
        let to = oid(0xa2);
        let retarget = oid(0xa3);
        write_replace_ref(repo.path(), &from, &to);

        let _pin = WorktreeScope::pin_request_scope(repo.path().to_path_buf());
        let snap = snapshot();
        let sig = signature_of(&snap);
        with_pinned_snapshot_sync(snap.clone(), || {
            // A "concurrent" mutation lands mid-build.
            write_replace_ref(repo.path(), &from, &retarget);
            assert_eq!(
                resolve(from),
                to,
                "the pinned walk answers from the snapshot"
            );
            assert_eq!(
                effective_replace_signature(),
                sig,
                "the stamped signature describes the SAME map the walk used"
            );
        });
        assert_eq!(
            resolve(from),
            retarget,
            "after the pin exits the mutation is visible"
        );
        assert_ne!(
            effective_replace_signature(),
            sig,
            "the ambient signature now describes the mutated map"
        );
    }

    /// The REAL `replace -d` path: its removal is visible to the very next
    /// resolve in the same process (no snapshot can outlive the mutation).
    #[tokio::test]
    #[serial_test::serial]
    async fn delete_mutation_path_invalidates_the_cache() {
        let repo = repo_fixture();
        let objects = repo.path().join(".libra").join("objects");
        fs::create_dir_all(&objects).expect("objects dir");
        let from = oid(0xee);
        let to = oid(0xef);
        // `resolve_any` accepts a raw oid only when the object exists in
        // THIS repo's store.
        crate::utils::client_storage::ClientStorage::init(objects)
            .put(
                &from,
                b"payload",
                git_internal::internal::object::types::ObjectType::Blob,
            )
            .expect("store fixture object");
        write_replace_ref(repo.path(), &from, &to);

        let _cd = crate::utils::test::ChangeDirGuard::new(repo.path());
        let _pin = WorktreeScope::pin_request_scope(repo.path().to_path_buf());
        assert_eq!(resolve(from), to, "the mapping caches first");

        delete(&[from.to_string()]).await.expect("replace -d");
        assert_eq!(
            resolve(from),
            from,
            "the next resolve in the same process sees the delete's removal"
        );
    }
}
