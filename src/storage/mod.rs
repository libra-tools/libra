//! Storage backend abstraction, dual-store implementations, and capture queue.
//!
//! Track A modules live exclusively under `src/storage/` so storage work stays
//! isolated from business commands until Track B atomic integration.
