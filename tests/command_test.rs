//! Main integration test entry point that re-exports the command test modules.
//!
//! Cargo treats every `tests/*.rs` file as its own crate. By declaring a single `mod
//! command` here, the per-command integration tests in `tests/command/*.rs` compile
//! into one shared binary. This avoids paying the build-time cost of one binary per
//! command while still letting each command live in its own file.

mod command;
