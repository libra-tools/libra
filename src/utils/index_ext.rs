//! Shared index-entry replacement helpers (plan issues/490, ADR-SW-03).
//!
//! git-internal stores the skip-worktree / intent-to-add bits in the index v3
//! extended flags word. Replacing an entry through [`Index::update`] with a
//! freshly built [`IndexEntry`] silently drops those bits, so every
//! "replace an existing entry" write point must go through
//! [`update_preserving`] instead. "Rebuild the index from a tree" write points
//! (for example `read-tree` without `-m`) deliberately keep the plain
//! [`Index::update`] / [`Index::add`], because Git clears the bits there.

use git_internal::internal::index::{Index, IndexEntry};

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
            ObjectHash::from_bytes(&[byte; 20]).expect("oid"),
            3,
        )
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
}
