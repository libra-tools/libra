//! Object alternates (lore.md 2.3) — borrow objects from a shared/parent
//! object store instead of copying them.
//!
//! This module is the SOLE reader/writer of two git-standard on-disk files
//! under a repo's `objects/info/` dir (§3.6 single-owner; a plain file, so no
//! lazily-created SQLite table, and portable to plain `git` and old Libra
//! binaries):
//!
//! - `alternates` — newline-separated OBJECT-DIRECTORY paths this store borrows
//!   FROM (absolute, or relative to this `objects/` dir; `#` comments / blanks
//!   skipped). The read-resolver consults these on a local miss.
//! - `borrowers` — newline-separated object-dir paths that borrow FROM this
//!   store (a Libra extension git does not have). This is the KEYSTONE of
//!   deletion safety: while this file names any live borrower, `gc` /
//!   `cache evict` REFUSE to prune loose objects, so a shared base can never
//!   delete an object a borrower still needs (the row's 绝不删 requirement,
//!   airtight — see [`has_live_borrowers`]).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// Transitive-alternates recursion backstop.
const MAX_DEPTH: usize = 5;

fn alternates_file(objects_dir: &Path) -> PathBuf {
    objects_dir.join("info").join("alternates")
}

fn borrowers_file(objects_dir: &Path) -> PathBuf {
    objects_dir.join("info").join("borrowers")
}

/// Parse one on-disk list file into resolved absolute object-dir paths (a
/// relative entry is joined to `objects_dir`). Missing file → empty. Comment
/// (`#`) and blank lines are skipped.
fn read_list(path: &Path, objects_dir: &Path) -> Vec<PathBuf> {
    read_list_result(path, objects_dir).unwrap_or_default()
}

/// [`read_list`], but distinguishing "no such file" (an empty list — the
/// normal case) from "cannot be read" (unknown contents).
///
/// The difference is the whole deletion gate: an unreadable `borrowers` file
/// collapsed into "no borrowers", which is permission to delete objects a
/// borrowing repository may still need. Callers that DELETE use this form and
/// fail closed; decorative readers keep the lenient one.
fn read_list_result(path: &Path, objects_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(parse_list(&text, objects_dir))
}

fn parse_list(text: &str, objects_dir: &Path) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let p = PathBuf::from(line);
            if p.is_absolute() {
                p
            } else {
                objects_dir.join(p)
            }
        })
        .collect()
}

fn write_list(path: &Path, entries: &[PathBuf]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    for entry in entries {
        body.push_str(&entry.to_string_lossy());
        body.push('\n');
    }
    crate::utils::atomic_write::write_atomic(path, body.as_bytes(), true)
        .map_err(std::io::Error::other)
}

/// A bounded-retry `O_EXCL` lockfile guard serializing the read-modify-write of
/// `alternates` / `borrowers` (Codex P1: concurrent adds must not drop an
/// entry). Released on drop.
struct FileLock(PathBuf);

impl FileLock {
    fn acquire(target: &Path) -> std::io::Result<Self> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = target.with_extension("lock");
        for _ in 0..200 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => return Ok(FileLock(lock)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "timed out acquiring the alternates lock",
        ))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Serialized read-modify-write: acquire the lock on `list_file`, re-read it,
/// apply `mutate`, and write it back atomically.
fn update_list(
    list_file: &Path,
    base_dir: &Path,
    mutate: impl FnOnce(&mut Vec<PathBuf>),
) -> std::io::Result<bool> {
    let _lock = FileLock::acquire(list_file)?;
    // STRICT re-read. The lenient form turns an unreadable list into an empty
    // one, and this is a read-modify-WRITE: an empty read here would rewrite
    // the file as empty — deleting registrations nobody could see — and hand
    // a deletion gate "no borrowers" for a list it never actually read.
    let mut entries = read_list_result(list_file, base_dir)?;
    let before = entries.len();
    let before_snapshot = entries.clone();
    mutate(&mut entries);
    if entries == before_snapshot {
        return Ok(false);
    }
    let _ = before;
    write_list(list_file, &entries)?;
    Ok(true)
}

/// The alternate object dirs this store borrows FROM (direct, unresolved).
pub fn list(objects_dir: &Path) -> Vec<PathBuf> {
    read_list(&alternates_file(objects_dir), objects_dir)
}

/// The FLATTENED, transitive alternate chain for `objects_dir` (git alternates
/// are transitive). Cycle-safe (canonicalized visited set, with a raw-path
/// fallback when a dir cannot be canonicalized) and depth-capped. Non-existent
/// alternate dirs are skipped (a dangling alternate is a warning, surfaced by
/// fsck — never a hard read failure here).
pub fn resolve_chain(objects_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    visited.insert(canon(objects_dir));

    let mut frontier: Vec<(PathBuf, usize)> =
        list(objects_dir).into_iter().map(|p| (p, 1usize)).collect();
    while let Some((dir, depth)) = frontier.pop() {
        if depth > MAX_DEPTH {
            tracing::warn!(dir = %dir.display(), "alternates chain exceeds max depth; truncating");
            continue;
        }
        let key = canon(&dir);
        if !visited.insert(key) {
            continue; // cycle / already seen
        }
        if !dir.is_dir() {
            tracing::warn!(dir = %dir.display(), "alternate object dir does not exist; skipping");
            continue;
        }
        out.push(dir.clone());
        for next in list(&dir) {
            frontier.push((next, depth + 1));
        }
    }
    out
}

/// Register `alternate_objects_dir` as an alternate of `objects_dir` (append if
/// absent) AND register `objects_dir` as a BORROWER of the alternate (so the
/// base's gc/evict can protect the borrowed objects). Idempotent.
pub fn add(objects_dir: &Path, alternate_objects_dir: &Path) -> std::io::Result<()> {
    let alternate = std::fs::canonicalize(alternate_objects_dir)
        .unwrap_or_else(|_| alternate_objects_dir.to_path_buf());
    let me = std::fs::canonicalize(objects_dir).unwrap_or_else(|_| objects_dir.to_path_buf());
    // §C.4.3 writer-vs-deleter, from the OTHER side: registering a borrower
    // is a publication into the BASE repository — it is the fact that makes
    // the base's `gc`, `repack -d` and `cache evict` refuse. Registering it
    // without the base's maintenance lock lets it land after that refusal has
    // already been evaluated and before the base unlinks, so the new borrower
    // starts out depending on objects that are about to disappear.
    //
    // The base's `.libra` is the alternate's `objects/` parent.
    let _base_publication = alternate
        .parent()
        .map(crate::internal::maintenance_lock::MaintenanceLock::shared)
        .transpose()
        .map_err(|error| {
            std::io::Error::other(format!(
                "failed to take the base repository's maintenance lock before registering as a                  borrower: {error}"
            ))
        })?;
    // 1) register THIS store as a BORROWER of the alternate FIRST (Codex P1):
    // if step 2 then fails, an extra borrower pin (base over-protected) is
    // safer than an unprotected borrow (base could prune what we read).
    update_list(&borrowers_file(&alternate), &alternate, |borrowers| {
        if !borrowers
            .iter()
            .any(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == me)
        {
            borrowers.push(me.clone());
        }
    })?;
    // 2) record the alternate in this store.
    update_list(&alternates_file(objects_dir), objects_dir, |alts| {
        if !alts
            .iter()
            .any(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == alternate)
        {
            alts.push(alternate.clone());
        }
    })?;
    Ok(())
}

/// Remove `alternate_objects_dir` from `objects_dir`'s alternates and
/// unregister `objects_dir` as a borrower of it. Returns whether a link existed.
pub fn remove(objects_dir: &Path, alternate_objects_dir: &Path) -> std::io::Result<bool> {
    let alternate = std::fs::canonicalize(alternate_objects_dir)
        .unwrap_or_else(|_| alternate_objects_dir.to_path_buf());
    let me = std::fs::canonicalize(objects_dir).unwrap_or_else(|_| objects_dir.to_path_buf());
    let removed = update_list(&alternates_file(objects_dir), objects_dir, |alts| {
        alts.retain(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) != alternate);
    })?;
    // Unregister the borrower on the base (locked).
    update_list(&borrowers_file(&alternate), &alternate, |borrowers| {
        borrowers.retain(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) != me);
    })?;
    Ok(removed)
}

/// The LIVE borrowers of `objects_dir` — object dirs that borrow FROM it and
/// still exist. Dead borrower entries (a borrower repo that was deleted) are
/// PRUNED from the file (self-healing), so a stale registration never pins a
/// base forever.
pub fn live_borrowers(objects_dir: &Path) -> Vec<PathBuf> {
    // Unknown contents: report nothing. The DELETION gate uses the fallible
    // form and fails closed; this lenient form exists for decoration, where
    // an empty answer costs a less precise message and never a decision.
    live_borrowers_result(objects_dir).unwrap_or_default()
}

/// Is this borrower registration PROVABLY dead — as opposed to merely
/// unreadable right now?
///
/// The distinction is the whole gate. `is_dir()` was the rule, and it maps
/// EACCES and a stale mount to "not a directory", so a borrower this process
/// merely could not stat was deleted from the registration and the next
/// deletion gate saw no borrower at all. Only an entry that is gone, or that
/// is definitely something other than a directory (an object directory never
/// is), counts as dead.
fn borrower_is_provably_dead(entry: &Path) -> bool {
    // ABSENCE IS NOT PROOF, and this is the whole judgement.
    //
    // An automounted or temporarily unavailable borrower answers ENOENT for a
    // path that exists again the moment it is mounted, and there is no way to
    // tell that apart from a deleted repository by looking — the mount point
    // and the deleted directory are both "not there". Pruning on ENOENT would
    // therefore let the next `gc` delete objects a borrower still needs, for
    // the sake of tidying a text file.
    //
    // So the automatic rule removes only what CANNOT be an object directory:
    // a path that exists and is a regular file. Everything else — absent,
    // unreadable, EIO — is treated as live, and the way to retire it is
    // `libra alternates prune`, where the USER asserts the borrower is gone.
    match std::fs::metadata(entry) {
        Ok(meta) => !meta.is_dir(),
        Err(_) => false,
    }
}

/// Registrations that are ABSENT (as opposed to unreadable), for the explicit
/// `libra alternates prune`.
///
/// Absence is not proof of death for the automatic gate — see
/// [`borrower_is_provably_dead`] — but under an explicit prune the user is
/// the proof: they are asserting these borrowers are gone for good. The
/// command reports each path before removing anything.
pub fn absent_borrowers(objects_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let registered = read_list_result(&borrowers_file(objects_dir), objects_dir)?;
    Ok(registered
        .into_iter()
        .filter(|entry| {
            matches!(
                std::fs::metadata(entry),
                Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
            )
        })
        .collect())
}

/// Retire borrower registrations under the borrowers-file LOCK, re-checking
/// each one at the moment of removal. Returns the paths actually removed.
///
/// The re-check is the point. Deciding from a snapshot taken earlier lets a
/// borrower that came back — and successfully re-registered in the meantime —
/// have its FRESH registration deleted, after which the base is free to prune
/// objects it is now borrowing. Under the lock, `alternates add` cannot
/// interleave, and an entry that exists again is left alone.
///
/// `named` retires one specific registration whatever the filesystem says
/// about it (the user is asserting it is gone); `None` retires every
/// registration whose path is ABSENT.
pub fn prune_borrowers(objects_dir: &Path, named: Option<&Path>) -> std::io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    update_list(&borrowers_file(objects_dir), objects_dir, |all| {
        all.retain(|entry| {
            let retire = match named {
                // The TARGET is used exactly as given — never canonicalized.
                // Resolving it first lets `prune /alias`, where `/alias` is a
                // symlink to a live borrower, retire that borrower's
                // registration even though `/alias` was never registered.
                // A registration may still be canonicalized to meet the raw
                // target, which is the direction that cannot invent a match.
                Some(target) => {
                    entry == target
                        || std::fs::canonicalize(entry).is_ok_and(|resolved| resolved == target)
                }
                None => matches!(
                    std::fs::metadata(entry),
                    Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
                ),
            };
            if retire {
                removed.push(entry.clone());
            }
            !retire
        });
    })?;
    Ok(removed)
}

/// Whether `objects_dir` is a SHARED BASE that some live borrower depends on.
/// gc / cache-evict consult this and refuse to prune loose objects when true.
pub fn has_live_borrowers(objects_dir: &Path) -> bool {
    !live_borrowers(objects_dir).is_empty()
}

/// [`has_live_borrowers`] for DELETION gates: an unreadable `borrowers` file
/// is an error, not "no borrowers".
///
/// A missing file legitimately means nobody borrows from this store. A file
/// that exists and cannot be read means the answer is UNKNOWN, and the
/// difference decides whether objects a borrowing repository still needs get
/// unlinked. Only an absent file may be read as empty.
fn live_borrowers_result(objects_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let bfile = borrowers_file(objects_dir);
    // Read strictly first: an unreadable registration is a fault the caller
    // must see, and it must not be rewritten.
    let registered = read_list_result(&bfile, objects_dir)?;
    if registered.is_empty() {
        return Ok(Vec::new());
    }

    // The answer is decided INSIDE the lock, from the list as it stands
    // there — and it is the same pass that prunes. Deciding first and
    // pruning after leaves a window in which a registration re-appears: it
    // survives the prune (correctly) but is missing from the answer, and the
    // caller then deletes objects it is borrowing.
    let mut live = Vec::new();
    let locked = update_list(&bfile, objects_dir, |all| {
        all.retain(|entry| {
            if borrower_is_provably_dead(entry) {
                false
            } else {
                live.push(entry.clone());
                true
            }
        });
    });
    match locked {
        Ok(_) => Ok(live),
        // The prune could not take the list lock (or could not write). That
        // must not turn into "no borrowers": fall back to the strict read,
        // which over-reports rather than under-reports.
        Err(_) => Ok(registered
            .into_iter()
            .filter(|entry| !borrower_is_provably_dead(entry))
            .collect()),
    }
}

/// plan-20260714 Part C W0 (§C.11 release gate): THE deletion-safety gate.
///
/// Every entry point that physically removes an object payload from this
/// store must pass through here — `gc`, `repack`, `prune`, `cache evict`
/// (direct and via scheduled maintenance), `file obliterate` and its crash
/// recovery. This store's reachability set does not include a borrower's
/// refs, so deleting while a borrower is live can leave that borrower
/// referencing bytes that no longer exist.
///
/// It exists as ONE function because the alternative — the same two-line
/// check copied into each caller — is how `maintenance run --task
/// cache-evict` and `file obliterate --recover` came to skip it entirely.
/// `action` names the refused operation in the message; `code` lets a caller
/// keep the stable error code its surface already documents.
pub fn ensure_no_live_borrowers(
    action: &str,
    code: crate::utils::error::StableErrorCode,
) -> crate::utils::error::CliResult<()> {
    let objects = crate::utils::path::objects();
    let live = live_borrowers_result(&objects).map_err(|error| {
        crate::utils::error::CliError::fatal(format!(
            "cannot {action}: this object store's borrower registration \
             ('objects/info/borrowers') cannot be read, so whether another repository depends \
             on these objects is unknown: {error}"
        ))
        // Deliberately NOT `code`. A live borrower is a known state a caller
        // may legitimately report as "skipped"; an unreadable registration is
        // a FAULT, and scheduled maintenance must not fold it into a
        // successful run. The code is what lets callers tell them apart.
        .with_stable_code(crate::utils::error::StableErrorCode::IoReadFailed)
        .with_hint("repair or remove that file once you know which repositories borrow from here")
    })?;
    if live.is_empty() {
        return Ok(());
    }
    Err(crate::utils::error::CliError::fatal(format!(
        "cannot {action}: this object store is shared (other repositories borrow from it via \
         alternates), and deleting an object one of them still needs would corrupt it"
    ))
    .with_stable_code(code)
    .with_hint(
        "have the borrowers run 'libra alternates remove' (or dissociate) first; if a borrower \
         repository is gone for good, run 'libra alternates prune' here to retire its \
         registration",
    ))
}

/// Append a raw comment/line to a store's info dir (used by tests / diagnostics).
#[cfg(test)]
pub(crate) fn append_alternate_line(objects_dir: &Path, line: &str) -> std::io::Result<()> {
    let path = alternates_file(objects_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objdir(root: &Path, name: &str) -> PathBuf {
        let d = root.join(name).join("objects");
        std::fs::create_dir_all(d.join("info")).unwrap();
        d
    }

    #[test]
    fn add_list_remove_round_trip_and_borrower_registration() {
        let tmp = tempfile::tempdir().unwrap();
        let a = objdir(tmp.path(), "A"); // base
        let b = objdir(tmp.path(), "B"); // borrower

        assert!(list(&b).is_empty());
        assert!(!has_live_borrowers(&a));

        add(&b, &a).unwrap();
        // B lists A as an alternate; A now has B as a live borrower.
        let alts = list(&b);
        assert_eq!(alts.len(), 1);
        assert!(has_live_borrowers(&a), "A is a shared base");
        assert_eq!(live_borrowers(&a).len(), 1);

        // add is idempotent (no duplicate borrower / alternate).
        add(&b, &a).unwrap();
        assert_eq!(list(&b).len(), 1);
        assert_eq!(live_borrowers(&a).len(), 1);

        // remove unregisters both directions.
        assert!(remove(&b, &a).unwrap());
        assert!(list(&b).is_empty());
        assert!(
            !has_live_borrowers(&a),
            "borrower gone -> A no longer shared"
        );
        assert!(
            !remove(&b, &a).unwrap(),
            "removing a non-alternate returns false"
        );
    }

    #[test]
    fn resolve_chain_is_transitive_cycle_safe_and_skips_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let a = objdir(tmp.path(), "A");
        let b = objdir(tmp.path(), "B");
        let c = objdir(tmp.path(), "C");
        // C -> B -> A (transitive).
        add(&b, &a).unwrap();
        add(&c, &b).unwrap();
        let chain = resolve_chain(&c);
        let canon: HashSet<PathBuf> = chain
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap())
            .collect();
        assert!(canon.contains(&std::fs::canonicalize(&b).unwrap()));
        assert!(
            canon.contains(&std::fs::canonicalize(&a).unwrap()),
            "transitive A reached"
        );

        // A cycle A -> A is broken by the visited set (no infinite loop).
        add(&a, &a).unwrap_or(()); // self-ref may be added by the raw writer
        append_alternate_line(&a, &a.to_string_lossy()).unwrap();
        let _ = resolve_chain(&a); // must terminate

        // A dangling alternate is skipped (not in the chain).
        let missing = tmp.path().join("gone").join("objects");
        append_alternate_line(&c, &missing.to_string_lossy()).unwrap();
        let chain2 = resolve_chain(&c);
        assert!(
            !chain2.iter().any(|p| p == &missing),
            "dangling alternate is skipped"
        );
    }
}
