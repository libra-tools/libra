//! Version 2 operation-log primitives.

pub mod facet;
pub mod facets;
pub mod middleware;
pub mod snapshot;
pub mod store;
pub mod view;
pub mod working_copy;

// OL-15 removes this compatibility service. Re-exporting it keeps existing
// command integrations source-compatible while all new code uses v2 types.
pub use facet::{
    FacetCapture, FacetCaptureCtx, FacetDiff, FacetError, FacetName, FacetRegistry,
    FacetRestoreCtx, RestorePolicy, StateFacet,
};
pub use facets::{RawIndexFacet, SequencerFacet, SparseFacet, registry_for_scope};
pub use middleware::{
    ClassificationError, MutationClass, OperationError, OperationFuture, OperationResult,
    OperationTxn, classify_command, run_with_operation,
};
pub use snapshot::{ScanError, ScanResult, SnapshotError, SnapshotOutcome, WorkspaceSnapshotter};
pub use store::{
    JournalEntry, JournalPhase, OpHeadsView, OperationKind, OperationMetaV2, OperationStatusV2,
    OperationStoreV2, OperationV2, StoreError,
};
pub use view::{
    CapturePolicy, Completeness, HeadState, RepoViewV2, WorkspaceId, WorkspaceSnapshotV2,
};
pub use working_copy::{PinnedRequestScope, PointerError, Staleness, WorkspaceStatePointer};

pub use crate::internal::legacy_operation::*;
