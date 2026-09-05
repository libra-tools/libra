//! Version 2 operation-log primitives.

pub mod facet;
pub mod store;
pub mod view;

// OL-15 removes this compatibility service. Re-exporting it keeps existing
// command integrations source-compatible while all new code uses v2 types.
pub use facet::{
    FacetCapture, FacetCaptureCtx, FacetDiff, FacetError, FacetName, FacetRegistry,
    FacetRestoreCtx, RestorePolicy,
};
pub use store::{
    JournalEntry, JournalPhase, OpHeadsView, OperationKind, OperationMetaV2, OperationStatusV2,
    OperationStoreV2, OperationV2, StoreError,
};
pub use view::{
    CapturePolicy, Completeness, HeadState, RepoViewV2, WorkspaceId, WorkspaceSnapshotV2,
};

pub use crate::internal::legacy_operation::*;
