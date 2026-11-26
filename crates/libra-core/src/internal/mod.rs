//! Core internal modules hosted in `libra-core`.

pub mod account;
pub mod alternates;
pub mod auth;
pub mod branch;
#[doc(hidden)]
pub mod bridge_test_seams;
pub mod change;
pub mod config;
pub mod config_file;
pub mod config_ownership;
#[doc(hidden)]
pub mod config_scope;
pub mod db;
pub mod deps;
pub mod dirty;
pub mod head;
#[doc(hidden)]
pub mod index_pack;
pub mod layer;
#[doc(hidden)]
pub mod lazy_fetch;
pub mod log;
pub mod maintenance_lock;
pub mod merge_base;
pub mod metadata;
#[doc(hidden)]
pub mod metadata_types;
pub use libra_model as model;
pub mod mutable_state_ownership;
pub mod notes;
pub mod object_format;
#[doc(hidden)]
pub mod object_io;
pub mod obliteration;
pub mod operation;
pub mod pack_writer;
pub mod patch_mode;
pub mod process_terminate;
pub mod pseudo_ref;
#[doc(hidden)]
pub mod pseudo_ref_state;
pub mod reflog;
#[doc(hidden)]
pub mod replace;
pub mod revision_ordinal;
pub mod sequencer;
pub mod shallow;
#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod source_scan;
pub mod sparse;
pub mod status_observation;
pub mod tag;
pub mod tree_plumbing;
pub mod upgrade;
pub mod vault;
pub mod workspace;
pub mod worktree_io;
#[doc(hidden)]
pub mod worktree_registry;
pub mod worktree_scope;
