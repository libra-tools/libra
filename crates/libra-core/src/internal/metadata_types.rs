//! Shared AI-metadata constants and enums sunk from `internal::ai` so
//! `libra-core::internal::{metadata,operation::restore}` have zero upward
//! dependencies on `libra-ai`.

use anyhow::{Result, bail};

/// Maximum size in bytes for a reasoning artifact manifest.
#[doc(hidden)]
pub const REASONING_ARTIFACT_MANIFEST_MAX_BYTES: usize = 256 * 1024;

/// Libra-owned AI history ref preserved across operation restores.
#[doc(hidden)]
pub const AI_REF: &str = "libra/intent";

/// MAC domain separation purpose for private reasoning identity records.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningIdentityMacPurpose {
    /// Frozen identity-v1 descriptor/turn-slot purpose.
    Descriptor,
    /// Frozen identity-v1 record and whole-map purpose.
    Record,
    DescriptorV2,
    RecordV2,
    SessionMapV2,
}

impl ReasoningIdentityMacPurpose {
    pub fn descriptor_for_schema_version(version: i64) -> Result<Self> {
        match version {
            1 => Ok(Self::Descriptor),
            2 => Ok(Self::DescriptorV2),
            _ => bail!("unsupported private reasoning identity version; run libra agent doctor"),
        }
    }

    pub fn record_for_schema_version(version: i64) -> Result<Self> {
        match version {
            1 => Ok(Self::Record),
            2 => Ok(Self::RecordV2),
            _ => bail!("unsupported private reasoning identity version; run libra agent doctor"),
        }
    }

    pub fn map_for_schema_version(version: i64) -> Result<Self> {
        match version {
            // identity-v1 deliberately used the Record purpose for its map.
            1 => Ok(Self::Record),
            2 => Ok(Self::SessionMapV2),
            _ => bail!("unsupported private reasoning identity version; run libra agent doctor"),
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::Descriptor => "reasoning-identity/hmac-v1/",
            Self::Record => "reasoning-record/hmac-v1/",
            Self::DescriptorV2 => "reasoning-identity/hmac-v2/",
            Self::RecordV2 => "reasoning-record/hmac-v2/",
            Self::SessionMapV2 => "reasoning-map/hmac-v2/",
        }
    }
}
