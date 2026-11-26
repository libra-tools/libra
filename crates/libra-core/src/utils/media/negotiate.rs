//! Remote capability negotiation → transfer decision (lore.md §6.4).
//!
//! This is the safety core: a PURE function that, given the probe outcome, the
//! repo policy, and whether a local complete fallback object exists, decides
//! whether a transfer may use chunked LFS or must fall back to standard LFS —
//! and, crucially, BLOCKS (never silently produces a chunk-only artifact) when
//! the remote cannot serve a standard fallback and no local fallback exists
//! (§6.4:438, "never half-write"). The default for a fully-compatible remote is
//! Chunked; every doubt degrades to standard LFS.

use super::{capability::Capabilities, chunker, manifest};

/// Outcome of probing the remote's media-capability endpoint. Distinguishes a
/// missing endpoint (404 / connection refused) from a server error that
/// survived §0.2 backoff, because the two map to different fallback reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The endpoint answered with a capability document.
    Ok(Capabilities),
    /// No capability endpoint (404 / connection refused / DNS) — plain remote.
    NoEndpoint,
    /// The endpoint returned 429/5xx and did not recover after §0.2 retries.
    ServerErrorAfterBackoff,
}

/// The decided transfer mode for a media object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferDecision {
    /// Use Libra chunked LFS with the given algorithm.
    Chunked { algorithm: String },
    /// Fall back to standard Git LFS (safe default), with the reason.
    StandardLfs { reason: FallbackReason },
    /// Refuse the operation: chunked would be the only option but there is no
    /// standard fallback safety net (server refuses fallback AND no local
    /// complete object). Never silently produce a chunk-only artifact.
    Block { reason: BlockReason },
}

/// Why a transfer fell back to standard LFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    NoCapabilityEndpoint,
    ServerErrorAfterBackoff,
    UnknownHigherVersion,
    ChunkedDisabledByServer,
    IncompatibleAlgorithm,
    /// The server advertises chunked LFS but cannot accept our frozen max chunk
    /// size or lacks a required API (batch existence), so chunked transfer is not
    /// viable.
    InsufficientServerCapability,
    DisabledByRepoPolicy,
}

/// Why a transfer was blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    /// The server does not keep a standard LFS fallback object AND the client
    /// has no local complete fallback object — a chunk-only upload would leave
    /// no interoperable object, so the operation is refused.
    NoFallbackAndServerRefuses,
    /// Explicit range export must not fall back to a whole-object transfer.
    RangeExportRefused,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::NoCapabilityEndpoint => "no-capability-endpoint",
            FallbackReason::ServerErrorAfterBackoff => "server-error-after-backoff",
            FallbackReason::UnknownHigherVersion => "unknown-higher-version",
            FallbackReason::ChunkedDisabledByServer => "chunked-disabled-by-server",
            FallbackReason::IncompatibleAlgorithm => "incompatible-algorithm",
            FallbackReason::InsufficientServerCapability => "insufficient-server-capability",
            FallbackReason::DisabledByRepoPolicy => "disabled-by-repo-policy",
        }
    }
}

impl BlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockReason::NoFallbackAndServerRefuses => "no-fallback-and-server-refuses",
            BlockReason::RangeExportRefused => "range-export-refused",
        }
    }
}

/// Highest media-protocol major version this client understands.
const SUPPORTED_MAJOR: u64 = 1;

/// Decide the transfer mode (§6.4 matrix). First match wins; the all-green case
/// (a fully-compatible remote with a fallback safety net) defaults to Chunked.
///
/// - `probe`: the capability-probe outcome.
/// - `repo_policy_chunked_enabled`: whether this repo allows chunked LFS.
/// - `local_fallback_present`: whether a complete standard LFS media object is
///   available locally as a fallback safety net.
pub fn negotiate(
    probe: &ProbeOutcome,
    repo_policy_chunked_enabled: bool,
    local_fallback_present: bool,
) -> TransferDecision {
    let caps = match probe {
        ProbeOutcome::NoEndpoint => {
            return TransferDecision::StandardLfs {
                reason: FallbackReason::NoCapabilityEndpoint,
            };
        }
        ProbeOutcome::ServerErrorAfterBackoff => {
            return TransferDecision::StandardLfs {
                reason: FallbackReason::ServerErrorAfterBackoff,
            };
        }
        ProbeOutcome::Ok(caps) => caps,
    };

    // Safe default on an unrecognized higher major version.
    if parse_major(&caps.version).is_none_or(|major| major > SUPPORTED_MAJOR) {
        return TransferDecision::StandardLfs {
            reason: FallbackReason::UnknownHigherVersion,
        };
    }
    if !caps.chunked_lfs {
        return TransferDecision::StandardLfs {
            reason: FallbackReason::ChunkedDisabledByServer,
        };
    }
    let algo_ok = caps
        .chunk_algorithms
        .iter()
        .any(|a| a == chunker::ALGORITHM);
    let hash_ok = caps.hash_algorithms.iter().any(|h| h == "sha256");
    if !algo_ok || !hash_ok {
        return TransferDecision::StandardLfs {
            reason: FallbackReason::IncompatibleAlgorithm,
        };
    }
    // The server must accept our frozen maximum chunk size, the batch-exists
    // diff, and the v1 paging envelope. `range_read=false` does not block
    // ordinary chunked transfer or a later covering-chunk export.
    let paging_ok = caps.manifest_paging == manifest::MANIFEST_PAGING
        && caps.supports_manifest_id_read
        && caps.max_page_entries >= manifest::MAX_PAGE_ENTRIES as u64
        && caps.max_page_bytes >= manifest::MAX_ENVELOPE_SIZE as u64
        && caps.max_manifest_size >= manifest::MAX_ENVELOPE_SIZE as u64;
    if caps.max_chunk_size < chunker::MAX_SIZE as u64 || !caps.batch_exists_enabled() || !paging_ok
    {
        return TransferDecision::StandardLfs {
            reason: FallbackReason::InsufficientServerCapability,
        };
    }
    if !repo_policy_chunked_enabled {
        return TransferDecision::StandardLfs {
            reason: FallbackReason::DisabledByRepoPolicy,
        };
    }
    // Chunked is viable. NEVER half-write: if the server keeps no standard
    // fallback object AND we have no local complete object, refuse rather than
    // create a chunk-only artifact nothing else can read.
    if !caps.keeps_standard_fallback() && !local_fallback_present {
        return TransferDecision::Block {
            reason: BlockReason::NoFallbackAndServerRefuses,
        };
    }
    TransferDecision::Chunked {
        algorithm: chunker::ALGORITHM.to_string(),
    }
}

/// Explicit range export. Any condition that would use whole-object LFS for an
/// ordinary transfer is a hard refusal here (C-03). `range_read=false` still
/// allows a covering-chunk export when the rest of the paging contract holds.
pub fn negotiate_range_export(
    probe: &ProbeOutcome,
    repo_policy_chunked_enabled: bool,
) -> TransferDecision {
    match negotiate(probe, repo_policy_chunked_enabled, true) {
        TransferDecision::Chunked { algorithm } => TransferDecision::Chunked { algorithm },
        TransferDecision::StandardLfs { .. } | TransferDecision::Block { .. } => {
            TransferDecision::Block {
                reason: BlockReason::RangeExportRefused,
            }
        }
    }
}

/// Ordinary LFS may fall back only before a media transfer starts. After
/// prepare, authentication, hash, and protocol failures stay fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolPhase {
    BeforeMedia,
    InMedia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    StandardLfsFallback,
    FailClosed,
}

pub fn failure_disposition(phase: ProtocolPhase) -> FailureDisposition {
    match phase {
        ProtocolPhase::BeforeMedia => FailureDisposition::StandardLfsFallback,
        ProtocolPhase::InMedia => FailureDisposition::FailClosed,
    }
}

/// Parse the leading integer of a `"<major>"` / `"<major>.<minor>"` version.
fn parse_major(version: &str) -> Option<u64> {
    version.split('.').next()?.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::media::{chunker, manifest};

    fn good_caps() -> Capabilities {
        Capabilities {
            version: "1".to_string(),
            chunked_lfs: true,
            chunk_algorithms: vec!["fastcdc-v2020-32k".to_string()],
            hash_algorithms: vec!["sha256".to_string()],
            max_chunk_size: chunker::MAX_SIZE as u64,
            max_manifest_size: manifest::MAX_ENVELOPE_SIZE as u64,
            supports_batch_exists: true,
            supports_range_read: false,
            supports_standard_lfs_fallback: true,
            batch_exists: true,
            range_read: false,
            standard_lfs_fallback: true,
            supports_manifest_id_read: true,
            manifest_paging: manifest::MANIFEST_PAGING.to_string(),
            max_page_entries: manifest::MAX_PAGE_ENTRIES as u64,
            max_page_bytes: manifest::MAX_ENVELOPE_SIZE as u64,
        }
    }

    #[test]
    fn all_green_defaults_to_chunked() {
        // The positive happy path — a buggy negotiate() returning StandardLfs or
        // Block here must FAIL this test.
        let d = negotiate(&ProbeOutcome::Ok(good_caps()), true, true);
        assert_eq!(
            d,
            TransferDecision::Chunked {
                algorithm: "fastcdc-v2020-32k".to_string()
            }
        );
        // Chunked is still chosen when the server keeps a fallback even without a
        // local fallback object.
        assert!(matches!(
            negotiate(&ProbeOutcome::Ok(good_caps()), true, false),
            TransferDecision::Chunked { .. }
        ));
    }

    #[test]
    fn every_fallback_row() {
        use FallbackReason::*;
        let sl = |c, reason| {
            assert_eq!(
                negotiate(&ProbeOutcome::Ok(c), true, true),
                TransferDecision::StandardLfs { reason }
            );
        };
        assert_eq!(
            negotiate(&ProbeOutcome::NoEndpoint, true, true),
            TransferDecision::StandardLfs {
                reason: NoCapabilityEndpoint
            }
        );
        assert_eq!(
            negotiate(&ProbeOutcome::ServerErrorAfterBackoff, true, true),
            TransferDecision::StandardLfs {
                reason: ServerErrorAfterBackoff
            }
        );
        let mut c = good_caps();
        c.version = "2".to_string();
        sl(c, UnknownHigherVersion);
        let mut c = good_caps();
        c.version = "not-a-number".to_string();
        sl(c, UnknownHigherVersion);
        let mut c = good_caps();
        c.chunked_lfs = false;
        sl(c, ChunkedDisabledByServer);
        let mut c = good_caps();
        c.chunk_algorithms = vec!["fastcdc-v9".to_string()];
        sl(c, IncompatibleAlgorithm);
        let mut c = good_caps();
        c.hash_algorithms = vec!["blake3".to_string()];
        sl(c, IncompatibleAlgorithm);
        let mut c = good_caps();
        c.max_chunk_size = 1024; // smaller than our frozen MAX_SIZE
        sl(c, InsufficientServerCapability);
        let mut c = good_caps();
        c.supports_batch_exists = false;
        c.batch_exists = false;
        sl(c, InsufficientServerCapability);
        let mut c = good_caps();
        c.chunk_algorithms = vec!["fastcdc-v1".to_string()];
        sl(c, IncompatibleAlgorithm);
        let mut c = good_caps();
        c.manifest_paging.clear();
        sl(c, InsufficientServerCapability);
        let mut c = good_caps();
        c.supports_manifest_id_read = false;
        sl(c, InsufficientServerCapability);
        let mut c = good_caps();
        c.max_page_entries = 4095;
        sl(c, InsufficientServerCapability);
        let mut c = good_caps();
        c.max_page_bytes = manifest::MAX_ENVELOPE_SIZE as u64 - 1;
        sl(c, InsufficientServerCapability);
        let mut c = good_caps();
        c.max_manifest_size = 32;
        sl(c, InsufficientServerCapability);
        // range_read=false is already the green fixture and still selects Chunked.
        // repo policy disabled → fallback (checked with an otherwise-green caps)
        assert_eq!(
            negotiate(&ProbeOutcome::Ok(good_caps()), false, true),
            TransferDecision::StandardLfs {
                reason: DisabledByRepoPolicy
            }
        );
    }

    #[test]
    fn block_when_no_fallback_and_server_refuses() {
        let mut c = good_caps();
        c.supports_standard_lfs_fallback = false;
        c.standard_lfs_fallback = false;
        assert_eq!(
            negotiate(&ProbeOutcome::Ok(c.clone()), true, false),
            TransferDecision::Block {
                reason: BlockReason::NoFallbackAndServerRefuses
            }
        );
        // …but a local fallback object rescues it → chunked.
        assert!(matches!(
            negotiate(&ProbeOutcome::Ok(c), true, true),
            TransferDecision::Chunked { .. }
        ));
    }

    #[test]
    fn range_export_never_falls_back_to_a_whole_object() {
        assert_eq!(
            negotiate_range_export(&ProbeOutcome::NoEndpoint, true),
            TransferDecision::Block {
                reason: BlockReason::RangeExportRefused
            }
        );
        let mut missing_id = good_caps();
        missing_id.supports_manifest_id_read = false;
        assert_eq!(
            negotiate_range_export(&ProbeOutcome::Ok(missing_id), true),
            TransferDecision::Block {
                reason: BlockReason::RangeExportRefused
            }
        );
        assert!(matches!(
            negotiate_range_export(&ProbeOutcome::Ok(good_caps()), true),
            TransferDecision::Chunked { .. }
        ));
        assert_eq!(
            failure_disposition(ProtocolPhase::BeforeMedia),
            FailureDisposition::StandardLfsFallback
        );
        assert_eq!(
            failure_disposition(ProtocolPhase::InMedia),
            FailureDisposition::FailClosed
        );
    }
}
