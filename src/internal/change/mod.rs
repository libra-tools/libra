//! Stable logical Change IDs and their rewrite genealogy.

mod identity;
mod resolve;
mod store;
pub use identity::{ChangeId, ChangeIdError};
pub use resolve::{ChangeIdResolution, ResolveError, resolve_change_id_prefix};
pub use store::{ChangeRevision, ChangeStore, ChangeStoreError, RevisionVisibility};
