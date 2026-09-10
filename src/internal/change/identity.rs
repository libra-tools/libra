//! Opaque, sidecar-only logical Change IDs.

use std::{fmt, str::FromStr};

use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

const CHANGE_ID_BYTES: usize = 16;
const SYNTHETIC_DOMAIN: &[u8] = b"libra-change-id-v1\0";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChangeId([u8; CHANGE_ID_BYTES]);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChangeIdError {
    #[error("change id must contain exactly 32 hexadecimal characters")]
    InvalidHex,
    #[error("secure random generator failed")]
    Random,
}

impl ChangeId {
    pub fn generate() -> Result<Self, ChangeIdError> {
        let mut bytes = [0; CHANGE_ID_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| ChangeIdError::Random)?;
        Ok(Self(bytes))
    }

    /// Deterministic identity used only when importing a legacy commit that
    /// has no Libra sidecar identity.  The object format is domain-separated
    /// so the same bytes cannot alias across SHA-1 and SHA-256 repositories.
    pub fn synthetic_for_commit(object_format: &str, commit_oid: &[u8]) -> Self {
        let mut input =
            Vec::with_capacity(SYNTHETIC_DOMAIN.len() + object_format.len() + commit_oid.len());
        input.extend_from_slice(SYNTHETIC_DOMAIN);
        input.extend_from_slice(object_format.as_bytes());
        input.extend_from_slice(commit_oid);
        let digest = digest::digest(&digest::SHA256, &input);
        let mut bytes = [0; CHANGE_ID_BYTES];
        bytes.copy_from_slice(&digest.as_ref()[..CHANGE_ID_BYTES]);
        Self(bytes)
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub fn short_prefix(self, length: usize) -> String {
        self.to_hex().chars().take(length).collect()
    }

    pub fn as_bytes(self) -> [u8; CHANGE_ID_BYTES] {
        self.0
    }
}

impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for ChangeId {
    type Err = ChangeIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != CHANGE_ID_BYTES * 2 {
            return Err(ChangeIdError::InvalidHex);
        }
        let mut bytes = [0; CHANGE_ID_BYTES];
        for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let high = (pair[0] as char).to_digit(16);
            let low = (pair[1] as char).to_digit(16);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(ChangeIdError::InvalidHex);
            };
            bytes[index] = ((high << 4) | low) as u8;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for ChangeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ChangeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_opaque_and_random() {
        let first = ChangeId::generate().expect("random id");
        let second = ChangeId::generate().expect("random id");
        assert_ne!(first, second);
        assert_eq!(first.to_hex().len(), 32);
        assert_eq!(first.short_prefix(8).len(), 8);
    }

    #[test]
    fn synthetic_ids_are_stable_and_format_separated() {
        let oid = [0x42; 20];
        assert_eq!(
            ChangeId::synthetic_for_commit("sha1", &oid),
            ChangeId::synthetic_for_commit("sha1", &oid)
        );
        assert_ne!(
            ChangeId::synthetic_for_commit("sha1", &oid),
            ChangeId::synthetic_for_commit("sha256", &oid)
        );
    }
}
