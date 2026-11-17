//! Provider-neutral capture foundation.
//!
//! The modules in this tree own the shared capture seams used by live hooks
//! and historical import. They intentionally depend on the canonical hook
//! contracts and observed-agent source contracts, but never on provider
//! implementations. Pure ingress/reducer modules have no durable-store
//! dependency; the catalog module is the narrow, typed exception that owns
//! scope-fenced `agent_session` mutations without knowing ref topology.
//! The diagnostics owner separately writes a best-effort, content-free hook
//! observation journal; it never writes capture facts or recovery receipts.

pub(crate) mod catalog;
pub(crate) mod checkpoint;
pub(crate) mod coordinator;
pub mod detach;
pub(crate) mod diagnostics;
pub(crate) mod drain;
pub(crate) mod extraction;
pub use extraction::deadline_provider_extraction_summary;
pub(crate) mod finalizer;
pub mod ingress;
pub(crate) mod key;
pub(crate) mod live;
pub(crate) mod live_checkpoint;
// This oracle uses Unix shell fixtures. Native Windows primitives have their
// own tests and the OpenCode CLI workflow is validated locally on Windows.
#[cfg(all(test, unix))]
mod live_oracle_tests;
/// Non-Unix stand-in so the oracle's verification gate reports `skipped`
/// instead of silently matching no test.
#[cfg(all(test, not(unix)))]
mod live_oracle_tests {
    #[test]
    fn live_checkpoint_metadata_shape_is_stable() {
        eprintln!("skipped (the live checkpoint oracle requires a Unix host)");
    }
}
pub(crate) mod live_pipeline;
pub(crate) mod pending;
pub(crate) mod pending_identity;
pub(crate) mod pending_payload;
pub(crate) mod recovery;
pub(crate) mod runtime_scope;
pub(crate) mod scope_binding;
pub mod snapshot;
pub(crate) mod spool;
pub(crate) mod state;
pub mod store;
pub(crate) mod summary;
#[doc(hidden)]
pub mod test_support;
pub(crate) mod worker;
