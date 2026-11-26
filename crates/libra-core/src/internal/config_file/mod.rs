//! Git-style configuration file backend.
//!
//! The reader lives in the `config` command module (it answers `--file` reads);
//! [`write`](mod@write) implements Git-compatible `--file` writes.

pub mod write;
