//! Shared index-entry replacement helpers (plan issues/490, ADR-SW-03).
//!
//! git-internal stores the skip-worktree / intent-to-add bits in the index v3
//! extended flags word. Replacing an entry through [`Index::update`] with a
//! freshly built [`IndexEntry`] silently drops those bits, so every
//! "replace an existing entry" write point must go through
//! [`update_preserving`] instead. "Rebuild the index from a tree" write points
//! (for example `read-tree` without `-m`) deliberately keep the plain
//! [`Index::update`] / [`Index::add`], because Git clears the bits there.

use git_internal::internal::index::{Flags, Index, IndexEntry};

/// Which extended flags a replacement inherits from the entry it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagPreservation {
    /// Inherit both `skip_worktree` and `intent_to_add` (the default for
    /// staging operations: the caller is only refreshing content/stat).
    All,
    /// Inherit `skip_worktree`, but keep the caller's `intent_to_add` value:
    /// `add <path>` writing real content clears the intent-to-add bit even
    /// though the entry is otherwise a replacement.
    ExceptIntentToAdd,
}

/// Replace the entry at the same `(path, stage)`, applying `policy` to the
/// extended flags inherited from the entry being replaced.
pub fn update_preserving(index: &mut Index, mut entry: IndexEntry, policy: FlagPreservation) {
    if let Some(existing) = index.get(&entry.name, entry.flags.stage) {
        entry.flags.skip_worktree = existing.flags.skip_worktree;
        if policy == FlagPreservation::All {
            entry.flags.intent_to_add = existing.flags.intent_to_add;
        }
    }
    index.update(entry);
}

/// [`update_preserving`] with [`FlagPreservation::All`].
pub fn update_preserving_flags(index: &mut Index, entry: IndexEntry) {
    update_preserving(index, entry, FlagPreservation::All);
}

/// [`update_preserving`] for a worktree-staged entry under `core.fileMode=false`
/// (ADR-FM-04): the caller's worktree-derived mode is ignored — an existing
/// entry keeps its recorded mode and a new path becomes a plain `100644`.
pub fn update_preserving_file_mode(index: &mut Index, mut entry: IndexEntry, file_mode: bool) {
    if !file_mode {
        entry.mode = index
            .get(&entry.name, entry.flags.stage)
            .map(|existing| existing.mode)
            .unwrap_or(0o100644);
    }
    update_preserving(index, entry, FlagPreservation::All);
}

/// [`update_preserving_file_mode`] for a caller that is writing the path's real
/// content (WT-05, ADR-SW-03): the intent-to-add bit is cleared — once a path
/// has tree content it is no longer intent-to-add — while skip-worktree (and
/// the `core.fileMode=false` mode rule) keep their behavior.
pub fn update_preserving_file_mode_except_intent(
    index: &mut Index,
    mut entry: IndexEntry,
    file_mode: bool,
) {
    if !file_mode {
        entry.mode = index
            .get(&entry.name, entry.flags.stage)
            .map(|existing| existing.mode)
            .unwrap_or(0o100644);
    }
    update_preserving(index, entry, FlagPreservation::ExceptIntentToAdd);
}

/// Set or clear the `skip_worktree` bit on the stage-0 entry at `path`
/// (ADR-SW-06, `update-index --skip-worktree`/`--no-skip-worktree`). This is
/// an in-place flag mutation, not an entry replacement: content, mode and
/// `intent_to_add` are left untouched, and the working tree is never read.
/// Returns `false` when no stage-0 entry exists, so the caller can surface
/// git's `fatal: Unable to mark file <path>` (exit 128).
pub fn set_skip_worktree(index: &mut Index, path: &str, marked: bool) -> bool {
    let Some(existing) = index.get(path, 0) else {
        return false;
    };
    // `IndexEntry` is not `Clone`, so mirror every field explicitly. All stat
    // data and the other flags (including `intent_to_add`) stay untouched.
    let mut entry = IndexEntry {
        ctime: existing.ctime.clone(),
        mtime: existing.mtime.clone(),
        dev: existing.dev,
        ino: existing.ino,
        mode: existing.mode,
        uid: existing.uid,
        gid: existing.gid,
        size: existing.size,
        hash: existing.hash,
        flags: Flags {
            assume_valid: existing.flags.assume_valid,
            extended: existing.flags.extended,
            stage: existing.flags.stage,
            name_length: existing.flags.name_length,
            skip_worktree: marked,
            intent_to_add: existing.flags.intent_to_add,
        },
        name: existing.name.clone(),
    };
    entry.flags.extended = entry.flags.extended_word().is_some();
    index.update(entry);
    true
}

/// Clear a stale assume-unchanged (`CE_VALID`) bit on the entry at
/// `(path, stage)` (issues/637). Libra has no assume-unchanged semantics
/// (DEFER-02), and git-internal <= 0.10.3 wrote the bit on every new entry, so
/// any `CE_VALID` in a Libra index is a leftover that would make Git skip the
/// worktree stat. Everything else on the entry is left untouched. Returns
/// whether the bit was set.
pub fn clear_assume_valid(index: &mut Index, path: &str, stage: u8) -> bool {
    if !index
        .get(path, stage)
        .is_some_and(|entry| entry.flags.assume_valid)
    {
        return false;
    }
    let Some(mut entry) = index.remove(path, stage) else {
        return false;
    };
    entry.flags.assume_valid = false;
    index.update(entry);
    true
}

/// Carry the `skip_worktree` bit from `previous` onto the entries of `rebuilt`
/// by path (ADR-SW-03's rebuild rule: `intent_to_add` is NOT carried — once a
/// path has tree content it is no longer intent-to-add). Used by the
/// `reset`-family rebuilds, where Git preserves skip-worktree across
/// `unpack_trees(reset)`.
pub fn preserve_skip_worktree_from(previous: &Index, rebuilt: &mut Index) {
    let flagged: Vec<String> = previous
        .tracked_files()
        .into_iter()
        .filter_map(|path| {
            let name = path.to_str()?.to_string();
            previous
                .get(&name, 0)
                .filter(|entry| entry.flags.skip_worktree)
                .map(|_| name)
        })
        .collect();
    for name in flagged {
        let Some((hash, mode, size)) = rebuilt
            .get(&name, 0)
            .map(|entry| (entry.hash, entry.mode, entry.size))
        else {
            continue;
        };
        let mut entry = IndexEntry::new_from_blob(name, hash, size);
        entry.mode = mode;
        entry.flags.skip_worktree = true;
        rebuilt.update(entry);
    }
}

#[cfg(test)]
mod tests {
    use git_internal::{
        hash::{HashKind, ObjectHash, set_hash_kind_for_test},
        internal::index::Index,
    };

    use super::*;

    fn entry(name: &str, byte: u8) -> IndexEntry {
        IndexEntry::new_from_blob(
            name.to_string(),
            ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &[byte; 20])
                .expect("oid"),
            3,
        )
    }

    #[test]
    fn set_skip_worktree_flips_only_that_bit_in_place() {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let mut index = Index::new();
        let mut existing = entry("s.txt", 0x11);
        existing.flags.intent_to_add = true;
        index.add(existing);

        // An unknown path reports failure without panicking.
        assert!(!set_skip_worktree(&mut index, "missing.txt", true));

        assert!(set_skip_worktree(&mut index, "s.txt", true));
        let marked = index.get("s.txt", 0).expect("entry");
        assert!(marked.flags.skip_worktree);
        assert!(marked.flags.intent_to_add, "other flags stay untouched");
        assert!(marked.flags.extended, "extended must track the word");
        assert_eq!(
            marked.hash.to_string(),
            entry("s.txt", 0x11).hash.to_string()
        );

        assert!(set_skip_worktree(&mut index, "s.txt", false));
        let cleared = index.get("s.txt", 0).expect("entry");
        assert!(!cleared.flags.skip_worktree);
        // `extended` tracks the remaining extended word — `intent_to_add` is
        // still set here, so the entry stays extended.
        assert_eq!(
            cleared.flags.extended,
            cleared.flags.extended_word().is_some(),
            "extended must track the extended word"
        );
    }

    #[test]
    fn replacement_preserves_both_extended_flags() {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let mut index = Index::new();
        let mut existing = entry("s.txt", 0x11);
        existing.flags.skip_worktree = true;
        existing.flags.intent_to_add = true;
        index.add(existing);
        let mut fresh = entry("s.txt", 0x22);
        fresh.flags.stage = 0;
        update_preserving_flags(&mut index, fresh);
        let loaded = index.get("s.txt", 0).expect("replaced entry");
        assert!(loaded.flags.skip_worktree, "skip_worktree must survive");
        assert!(loaded.flags.intent_to_add, "intent_to_add must survive");
    }

    #[test]
    fn except_intent_to_add_clears_only_that_bit() {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let mut index = Index::new();
        let mut existing = entry("s.txt", 0x11);
        existing.flags.skip_worktree = true;
        existing.flags.intent_to_add = true;
        index.add(existing);
        let fresh = entry("s.txt", 0x22);
        update_preserving(&mut index, fresh, FlagPreservation::ExceptIntentToAdd);
        let loaded = index.get("s.txt", 0).expect("replaced entry");
        assert!(loaded.flags.skip_worktree, "skip_worktree must survive");
        assert!(!loaded.flags.intent_to_add, "intent_to_add must be cleared");

        // A brand-new path has nothing to inherit.
        update_preserving(&mut index, entry("new.txt", 0x33), FlagPreservation::All);
        let new_entry = index.get("new.txt", 0).expect("new entry");
        assert!(!new_entry.flags.skip_worktree && !new_entry.flags.intent_to_add);
    }

    /// issues/637: git-internal 0.10.4 no longer sets `CE_VALID` on new
    /// entries, and `clear_assume_valid` drops a leftover bit without touching
    /// the rest of the entry.
    #[test]
    fn clear_assume_valid_drops_only_that_bit() {
        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        assert!(
            !entry("fresh.txt", 0x10).flags.assume_valid,
            "new entries must not carry CE_VALID"
        );

        let mut index = Index::new();
        let mut legacy = entry("s.txt", 0x11);
        legacy.flags.assume_valid = true;
        legacy.flags.skip_worktree = true;
        legacy.mode = 0o100755;
        index.add(legacy);

        assert!(!clear_assume_valid(&mut index, "missing.txt", 0));
        assert!(clear_assume_valid(&mut index, "s.txt", 0));
        let cleared = index.get("s.txt", 0).expect("entry");
        assert!(!cleared.flags.assume_valid);
        assert!(cleared.flags.skip_worktree, "other flags stay untouched");
        assert_eq!(cleared.mode, 0o100755);
        assert_eq!(cleared.hash, entry("s.txt", 0x11).hash);
        assert!(
            !clear_assume_valid(&mut index, "s.txt", 0),
            "a clean entry reports nothing to clear"
        );
    }
}
