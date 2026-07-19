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

use std::path::PathBuf;

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
    /// Resolve the scope of the current process's working directory.
    ///
    /// A linked worktree whose `worktree_id` file is missing or unreadable is
    /// still reported as [`WorktreeScope::Linked`] with the id synthesized from
    /// its canonical path — it must NEVER fall back to [`WorktreeScope::Main`],
    /// which would graft this worktree onto main's HEAD and sequencer rows.
    pub fn current() -> Self {
        match util::current_worktree_id() {
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
                 run `libra worktree repair`"
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
