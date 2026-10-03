//! Provider-neutral capture foundation.
//!
//! The modules in this tree own the shared capture seams used by live hooks
//! and historical import. They intentionally depend on the canonical hook
//! contracts and observed-agent source contracts, but never on provider
//! implementations. Pure ingress/reducer modules have no durable-store
//! dependency; the catalog module is the narrow, typed exception that owns
//! scope-fenced `agent_session` mutations without knowing ref topology.

pub(crate) mod catalog;
pub(crate) mod checkpoint;
pub(crate) mod coordinator;
pub(crate) mod extraction;
pub(crate) mod finalizer;
pub mod ingress;
pub(crate) mod key;
pub(crate) mod live;
pub(crate) mod live_checkpoint;
// The checkpoint object-I/O helper the OpenCode leg borrows is Unix-only, and
// the repository capture key behind every checkpoint commitment fails closed
// elsewhere, so the oracle is Unix-only.
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
pub(crate) mod state;
#[doc(hidden)]
pub mod test_support;
pub(crate) mod worker;
