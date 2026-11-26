//! Pure `refs/replace/*` resolution, snapshot pinning, and signature helpers
//! sunk from `command::replace`.

use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    path::{Path, PathBuf},
};

use git_internal::hash::ObjectHash;

use crate::utils::util;

#[doc(hidden)]
pub const REPLACE_REF_DIR: &str = "refs/replace";
const MAX_REPLACE_DEPTH: usize = 8;

#[doc(hidden)]
pub type ReplaceMap = HashMap<ObjectHash, ObjectHash>;

static EMPTY_REPLACE_MAP: std::sync::LazyLock<std::sync::Arc<ReplaceMap>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(HashMap::new()));

fn replace_map() -> std::sync::Arc<ReplaceMap> {
    let Some(dir) = replace_dir() else {
        return EMPTY_REPLACE_MAP.clone();
    };
    let map = load_replace_map(&dir);
    if map.is_empty() {
        return EMPTY_REPLACE_MAP.clone();
    }
    std::sync::Arc::new(map)
}

tokio::task_local! {
    static PINNED_SNAPSHOT: std::sync::Arc<ReplaceMap>;
}

tokio::task_local! {
    static SUPPRESS_PEEL: bool;
}

/// One immutable snapshot of the current replacement map.
#[doc(hidden)]
pub fn snapshot() -> std::sync::Arc<ReplaceMap> {
    replace_map()
}

/// Run `f` with every [`resolve`] and [`effective_replace_signature`] in this
/// task answering from `map` instead of re-reading `refs/replace`.
#[doc(hidden)]
pub fn with_pinned_snapshot_sync<R>(map: std::sync::Arc<ReplaceMap>, f: impl FnOnce() -> R) -> R {
    PINNED_SNAPSHOT.sync_scope(map, f)
}

/// Run an async future with `refs/replace` peeling suppressed on the current task.
#[doc(hidden)]
pub async fn with_suppressed_peel<F, R>(fut: F) -> R
where
    F: Future<Output = R>,
{
    SUPPRESS_PEEL.scope(true, fut).await
}

/// Run a synchronous closure with `refs/replace` peeling suppressed on the current task.
#[doc(hidden)]
pub fn with_suppressed_peel_sync<R>(f: impl FnOnce() -> R) -> R {
    SUPPRESS_PEEL.sync_scope(true, f)
}

fn peel_suppressed() -> bool {
    SUPPRESS_PEEL
        .try_with(|suppressed| *suppressed)
        .unwrap_or(false)
}

/// Signature of the effective replacement map.
#[doc(hidden)]
pub fn effective_replace_signature() -> String {
    if let Ok(signature) = PINNED_SNAPSHOT.try_with(|map| signature_of(map)) {
        return signature;
    }
    signature_of(&replace_map())
}

/// The signature of one specific replacement snapshot.
#[doc(hidden)]
pub fn signature_of(map: &ReplaceMap) -> String {
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

/// Resolve an object id through `refs/replace`, following a cycle-bounded chain.
#[doc(hidden)]
pub fn resolve(hash: ObjectHash) -> ObjectHash {
    if peel_suppressed() {
        return hash;
    }
    if let Ok(pinned) = PINNED_SNAPSHOT.try_with(|map| map.clone()) {
        return resolve_via(&pinned, hash);
    }
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

fn resolve_via(map: &ReplaceMap, hash: ObjectHash) -> ObjectHash {
    if map.is_empty() {
        return hash;
    }
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

fn read_one_replace_ref(dir: &Path, hash: &ObjectHash) -> Option<ObjectHash> {
    let content = fs::read_to_string(dir.join(hash.to_string())).ok()?;
    crate::internal::object_format::parse_repo_oid(content.trim()).ok()
}

#[doc(hidden)]
pub fn load_replace_map(dir: &Path) -> HashMap<ObjectHash, ObjectHash> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return map;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(src) = crate::internal::object_format::parse_repo_oid(name) else {
            continue;
        };
        let Ok(content) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(dst) = crate::internal::object_format::parse_repo_oid(content.trim()) else {
            continue;
        };
        map.insert(src, dst);
    }
    map
}

#[doc(hidden)]
pub fn replace_dir() -> Option<PathBuf> {
    if let Some(pinned) = crate::internal::worktree_scope::WorktreeScope::request_scope() {
        return Some(pinned.storage.join(REPLACE_REF_DIR));
    }
    util::try_get_storage_path(None)
        .ok()
        .map(|storage| storage.join(REPLACE_REF_DIR))
}
