//! Internal layer for the Libra runtime.
//!
//! This module is the seam between the user-facing CLI commands (in `src/command`)
//! and the persistent state held inside `.libra/`:
//!
//! - [`branch`] / [`tag`] / [`head`] / [`reflog`]: high-level wrappers around the SQLite
//!   `reference` and `reflog` tables; they expose Git-shaped concepts (refs, HEAD,
//!   reflog entries) without leaking sea-orm details to callers.
//! - [`config`]: typed accessor for the `config` and `config_kv` tables, used by
//!   `libra config` and by every subsystem that needs to read repo-local settings.
//! - [`db`]: SQLite bootstrap, migration runner, and pooled connection accessor.
//! - [`model`]: raw sea-orm `Entity`/`Model`/`ActiveModel` definitions. Other modules
//!   in this layer wrap these so callers do not depend on sea-orm directly.
//! - [`protocol`]: clients for Git's wire protocols (smart HTTP, ssh, local fs) plus
//!   the LFS client. These are pluggable behind the [`protocol::SmartProtocol`] trait.
//! - [`log`]: rendering of `git log`–style output and date/time parsing helpers.
//! - [`ai`] / [`tui`]: agent runtime and terminal UI used by `libra code`.
//! - [`vault`]: encrypted at-rest storage for credentials and provider secrets.
//!
//! Modules here may depend on `git-internal` and on each other but should *not* depend
//! on `src/command/*` — that direction is the CLI dispatch boundary.

pub mod account;
pub mod ai;
pub mod alternates;
pub mod auth;
pub mod branch;
pub mod config;
pub mod db;
pub mod deps;
pub mod dirty;
pub mod head;
pub mod layer;
pub mod log;
pub mod merge_base;
pub mod metadata;
pub mod model;
pub mod notes;
pub mod obliteration;
pub mod operation;
pub mod operation_wrapper;
pub mod pack_writer;
pub mod protocol;
pub mod publish;
pub mod reflog;
pub mod repo_hooks;
pub mod revision_ordinal;
pub mod sequencer;
pub mod sparse;
pub mod tag;
pub mod tree_plumbing;
pub mod tui;
pub mod vault;
