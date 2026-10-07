//! plan-20260926 MEM-01/02 deterministic Episode projection.
//!
//! `memory/` implements the repository-scoped, zero-authority projection of
//! durable agent-development facts into `memory_episode` / evidence / path
//! rows (GC-DM-01). It holds no authoritative state and can always be
//! re-derived by `libra memory rebuild`; a stale index never answers
//! (freshness is validated on read).
//!
//! This module deliberately does NOT depend on the removed anchor-confidence
//! / provider / prompt / operation-wrapper surfaces (ER-06 / GC ban; verified
//! by DM-02's `rg` guard).

mod derive_commit;
mod episode;
mod error;
mod projection;

pub use derive_commit::derive_commit_episodes;
pub use episode::{
    Episode, EvidenceEdge, MEMORY_EPISODE_NAMESPACE_V1, MemoryConfidence, Outcome, PathRow,
    SourceKind, content_digest, episode_id,
};
pub use error::MemoryError;
pub use projection::project_episodes;
