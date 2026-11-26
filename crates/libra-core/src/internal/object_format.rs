//! Single source of truth for `core.objectformat` ↔ [`HashKind`] mapping,
//! digests, pack-index version selection, and repository OID parsing
//! (plan-20260907 GC-B3-01).
//!
//! Call sites that still carry local `"sha1" | "sha256"` string arms are
//! transitional: B3-00 introduces this module and migrates the hook runtime;
//! later cards (B3-01 / B3-07 / …) converge the remaining arms onto these
//! helpers.

use anyhow::{Result, bail};
use git_internal::{
    hash::{HashKind, ObjectHash},
    utils::HashAlgorithm,
};

/// Parse a `core.objectformat` config value.
///
/// Accepts only the exact lowercase spellings `sha1`, `sha256`, and `blake3`.
/// Mixed-case or unknown values fail closed.
pub fn parse_config_value(raw: &str) -> Result<HashKind> {
    match raw {
        "sha1" => Ok(HashKind::Sha1),
        "sha256" => Ok(HashKind::Sha256),
        "blake3" => Ok(HashKind::Blake3),
        _ => bail!("unsupported object format '{raw}' (expected sha1|sha256|blake3)"),
    }
}

/// Lowercase wire / config spelling for `kind` (`sha1` / `sha256` / `blake3`).
pub fn as_str(kind: HashKind) -> &'static str {
    kind.as_str()
}

/// Digest `bytes` with `kind` via git-internal's [`HashAlgorithm`]
/// (never call the `blake3` crate directly — GC-B3-03).
pub fn digest(kind: HashKind, bytes: &[u8]) -> ObjectHash {
    let mut hasher = HashAlgorithm::new_for_kind(kind);
    hasher.update(bytes);
    hasher.finalize_object_hash()
}

/// Whether pack indexes for `kind` must be v2 (non-SHA-1 → v2 only).
pub fn pack_index_is_v2(kind: HashKind) -> bool {
    !matches!(kind, HashKind::Sha1)
}

/// Parse a raw hex repository OID under the process-wide hash kind
/// (`set_hash_kind` / CLI preflight). Prefer [`parse_hex_for_kind`] when the
/// repository kind is known explicitly.
pub fn parse_repo_oid(hex: &str) -> Result<ObjectHash> {
    parse_hex_for_kind(git_internal::hash::get_hash_kind(), hex)
}

/// Parse a raw hex OID for an explicit repository `kind`.
pub fn parse_hex_for_kind(kind: HashKind, hex: &str) -> Result<ObjectHash> {
    ObjectHash::from_hex_for_kind(kind, hex)
        .map_err(|error| anyhow::anyhow!("invalid object id for {}: {error}", kind.as_str()))
}

#[cfg(test)]
mod tests {
    use git_internal::hash::set_hash_kind_for_test;
    use serial_test::serial;

    use super::*;

    #[test]
    fn parse_config_value_accepts_exact_lowercase() {
        assert_eq!(parse_config_value("sha1").unwrap(), HashKind::Sha1);
        assert_eq!(parse_config_value("sha256").unwrap(), HashKind::Sha256);
        assert_eq!(parse_config_value("blake3").unwrap(), HashKind::Blake3);
    }

    #[test]
    fn parse_config_value_rejects_mixed_case_and_unknown() {
        assert!(parse_config_value("SHA1").is_err());
        assert!(parse_config_value("Blake3").is_err());
        assert!(parse_config_value("sha512").is_err());
        assert!(parse_config_value("").is_err());
    }

    #[test]
    fn as_str_and_pack_index_mirror_hash_kind() {
        assert_eq!(as_str(HashKind::Sha1), "sha1");
        assert_eq!(as_str(HashKind::Sha256), "sha256");
        assert_eq!(as_str(HashKind::Blake3), "blake3");
        assert!(!pack_index_is_v2(HashKind::Sha1));
        assert!(pack_index_is_v2(HashKind::Sha256));
        assert!(pack_index_is_v2(HashKind::Blake3));
    }

    #[test]
    #[serial(hash_kind)]
    fn digest_and_parse_round_trip_per_kind() {
        for kind in [HashKind::Sha1, HashKind::Sha256, HashKind::Blake3] {
            let _guard = set_hash_kind_for_test(kind);
            let hashed = digest(kind, b"object-format-fixture");
            assert_eq!(hashed.kind(), kind);
            let hex = hashed.to_string();
            let parsed = parse_hex_for_kind(kind, &hex).expect("hex parse");
            assert_eq!(parsed, hashed);
            let via_repo = parse_repo_oid(&hex).expect("repo oid");
            assert_eq!(via_repo, hashed);
        }
    }
}
