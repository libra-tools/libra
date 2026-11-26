//! FastCDC LFS media chunking and authenticated Mega transport (lore.md §6).
//!
//! This module is lore.md §6 "LFS FastCDC chunking" (`fastcdc-v2020-32k` recipe): a
//! strictly feature-gated (`fastcdc`, default OFF) **client** layer that
//! content-defines chunks of a media object, builds a versioned manifest,
//! stores chunks in a local content-addressed store, reassembles + verifies,
//! and negotiates a remote's chunked-vs-standard-LFS capability with an airtight
//! safe fallback. [`transfer`] connects the feature-enabled LFS upload/download
//! paths to Mega's opt-in media server. Shared repository ACLs, server-side GC,
//! obliteration, and byte-range hydration remain separate work; see the media
//! command documentation for the current owner/repository isolation boundary.
//!
//! ## Invariants (load-bearing)
//!
//! - **Git object graph untouched (§6.2):** a chunk is NEVER a Git object ID.
//!   Chunks and manifests live in a `.libra/media/` store that is a physical
//!   sibling of `objects/` and is never walked as loose objects; `chunk_hash`
//!   and `media_oid` address RAW bytes, never `blob <size>\0`-wrapped content.
//! - **`media_oid` is always SHA-256 (§6.3):** it reuses [`crate::utils::lfs`]'s
//!   `calc_lfs_file_hash` (ring SHA-256 over the full file), independent of the
//!   repository `core.objectformat`, so a SHA-1 repo still emits a `media_oid`
//!   byte-identical to a standard LFS pointer's `oid sha256:…`.
//! - **Never half-write (§6.4):** the capability negotiation defaults to
//!   standard LFS on any doubt and BLOCKS (never silently produces a chunk-only
//!   artifact) when the remote cannot serve a standard fallback and no local
//!   fallback object exists. Reassembly verifies the full `media_oid` BEFORE the
//!   atomic publish.
//! - **Paging is not identity (P-01):** the canonical manifest id hashes the
//!   chunk sequence, not page boundaries. Local layouts store a bounded summary
//!   plus immutable pages under `.libra/media/fastcdc-v2020-32k/`. The hash/offset
//!   index there is derived data and can be deleted and rebuilt; it is not
//!   repository config. The legacy `.libra/media/{chunks,manifests}` cache is
//!   left in place and is not read, written, or deleted.
//! - **Paged transfer (P-02/P-04):** upload prepares a summary, puts canonical
//!   pages, seals, PUTs only hashes named by the paged missing cursor, and polls
//!   a durable finalize task. Download pins `manifest_id` and checks id, oid,
//!   and size before replacing the destination.

pub mod capability;
pub mod chunk_store;
pub mod chunker;
pub mod manifest;
pub mod negotiate;
pub mod page_cache;
pub mod transfer;

use ring::digest::{Context, SHA256};

/// Lowercase-hex SHA-256 of RAW bytes (no Git object framing). Used for both
/// `chunk_hash` and any in-memory digest; the whole-file `media_oid` streams
/// through [`crate::utils::lfs::calc_lfs_file_hash`] (same primitive).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut ctx = Context::new(&SHA256);
    ctx.update(bytes);
    hex::encode(ctx.finish().as_ref())
}

/// Whether a string is exactly 64 ASCII-lowercase-hex characters (a SHA-256
/// digest in the canonical form used by `media_oid`/`chunk_hash`).
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
