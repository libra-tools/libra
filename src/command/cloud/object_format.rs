//! Resolve the authoritative repository [`HashKind`] for cloud restore/sync
//! paths (plan-20260907 B3-14).
//!
//! Repository-level `object_format` metadata written by B3-09 is the only
//! authority. OID width is used solely for fail-closed conflict checks and
//! for the legacy "all-40 hex → sha1" compatibility window when metadata is
//! absent.

use git_internal::hash::HashKind;

use super::CloudError;
use crate::utils::d1_client::ObjectIndexRow;

/// Hint attached to every ambiguous / missing-metadata failure so operators
/// know the recovery path is a fresh backup, not guessing.
pub(super) const REBACKUP_HINT: &str = "re-run `libra cloud sync` on the source repository to persist object_format metadata, then restore again";

/// Resolve the repository object-format for a cloud restore/sync catalog.
///
/// Rules (B3-14):
/// - Parsed repository metadata wins when present and legal.
/// - Empty index + legal metadata → that kind.
/// - Empty/missing/illegal metadata + empty index → fail (`LBR-REPO-002`).
/// - No metadata + every `o_id` is 40 hex → `sha1` (legacy).
/// - No metadata + any 64-hex (or mixed 40/64) → fail closed.
/// - Metadata vs observed OID width conflict → fail closed.
pub(super) fn resolve_cloud_repository_kind(
    repository_object_format: Option<&str>,
    indexes: &[ObjectIndexRow],
) -> Result<HashKind, CloudError> {
    let meta = repository_object_format
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let widths = collect_oid_widths(indexes)?;
    if let Some(raw) = meta {
        let kind = crate::internal::object_format::parse_config_value(raw).map_err(|_| {
            ambiguous_format(format!(
                "unsupported object format '{raw}' in cloud repository metadata"
            ))
        })?;
        validate_widths_for_kind(kind, &widths)?;
        validate_row_formats_against_kind(kind, indexes)?;
        return Ok(kind);
    }

    // No repository metadata: only the all-40 legacy window is accepted.
    if indexes.is_empty() {
        return Err(ambiguous_format(
            "cloud repository metadata is missing object_format and the object index is empty"
                .to_string(),
        ));
    }
    if widths.is_empty() {
        return Err(ambiguous_format(
            "cloud object index rows have no usable o_id widths without object_format metadata"
                .to_string(),
        ));
    }
    let all_40 = widths.iter().all(|width| *width == 40);
    if all_40 {
        return Ok(HashKind::Sha1);
    }
    if widths.contains(&64) {
        return Err(ambiguous_format(
            "cloud snapshot has 64-hex object ids without repository object_format metadata; refusing to guess sha256 vs blake3"
                .to_string(),
        ));
    }
    Err(ambiguous_format(format!(
        "cloud object index has unsupported o_id widths {widths:?} without object_format metadata"
    )))
}

fn ambiguous_format(detail: String) -> CloudError {
    CloudError::AmbiguousObjectFormat(detail)
}

fn collect_oid_widths(indexes: &[ObjectIndexRow]) -> Result<Vec<usize>, CloudError> {
    let mut widths = Vec::with_capacity(indexes.len());
    for row in indexes {
        let oid = row.o_id.trim();
        if oid.is_empty() {
            return Err(ambiguous_format(
                "cloud object index contains an empty o_id".to_string(),
            ));
        }
        if !oid.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(ambiguous_format(format!(
                "cloud object index o_id '{oid}' is not hexadecimal"
            )));
        }
        widths.push(oid.len());
    }
    Ok(widths)
}

fn expected_width(kind: HashKind) -> usize {
    match kind {
        HashKind::Sha1 => 40,
        HashKind::Sha256 | HashKind::Blake3 => 64,
    }
}

fn validate_widths_for_kind(kind: HashKind, widths: &[usize]) -> Result<(), CloudError> {
    let expected = expected_width(kind);
    for width in widths {
        if *width != expected {
            return Err(ambiguous_format(format!(
                "cloud object_format '{}' expects {expected}-hex o_id but found {width}-hex",
                crate::internal::object_format::as_str(kind)
            )));
        }
    }
    Ok(())
}

fn validate_row_formats_against_kind(
    kind: HashKind,
    indexes: &[ObjectIndexRow],
) -> Result<(), CloudError> {
    let expected = crate::internal::object_format::as_str(kind);
    for row in indexes {
        if let Some(raw) = row
            .object_format
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            let row_kind =
                crate::internal::object_format::parse_config_value(raw).map_err(|_| {
                    ambiguous_format(format!(
                        "unsupported object_format '{raw}' on object-index row {}",
                        row.o_id
                    ))
                })?;
            if row_kind != kind {
                return Err(ambiguous_format(format!(
                    "object-index row {} has object_format '{}' which conflicts with repository metadata '{expected}'",
                    row.o_id, raw
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error::StableErrorCode;

    fn row(oid: &str, format: Option<&str>) -> ObjectIndexRow {
        ObjectIndexRow {
            o_id: oid.to_string(),
            o_type: "blob".to_string(),
            o_size: 1,
            repo_id: "repo".to_string(),
            created_at: 1,
            is_synced: 1,
            object_format: format.map(str::to_string),
        }
    }

    fn oid40() -> String {
        "a".repeat(40)
    }

    fn oid64() -> String {
        "b".repeat(64)
    }

    #[test]
    fn cloud_object_format_uses_metadata() {
        let kind = resolve_cloud_repository_kind(Some("blake3"), &[row(&oid64(), None)])
            .expect("metadata wins");
        assert_eq!(kind, HashKind::Blake3);
    }

    #[test]
    fn cloud_object_format_refuses_ambiguous_64() {
        let err = resolve_cloud_repository_kind(None, &[row(&oid64(), None)])
            .expect_err("64 without metadata");
        match &err {
            CloudError::AmbiguousObjectFormat(detail) => {
                assert!(detail.contains("64-hex"));
                assert!(detail.contains("refusing"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        let cli = err.clone().into_cli_error("restore");
        assert_eq!(cli.stable_code(), StableErrorCode::RepoCorrupt);
        assert_eq!(cli.stable_code().as_str(), "LBR-REPO-002");
        assert!(
            cli.hints().iter().any(|hint| {
                let text = hint.as_str();
                text.contains("cloud sync") || text.contains("object_format")
            }),
            "recovery hint missing: {:?}",
            cli.hints()
        );
    }

    #[test]
    fn cloud_object_format_mixed_width_rows_fail_closed() {
        let err = resolve_cloud_repository_kind(None, &[row(&oid40(), None), row(&oid64(), None)])
            .expect_err("mixed widths");
        assert!(matches!(err, CloudError::AmbiguousObjectFormat(_)));
    }

    #[test]
    fn cloud_object_format_legacy_all_40_is_sha1() {
        let kind = resolve_cloud_repository_kind(None, &[row(&oid40(), None), row(&oid40(), None)])
            .expect("legacy sha1");
        assert_eq!(kind, HashKind::Sha1);
    }

    #[test]
    fn cloud_object_format_empty_index_uses_repository_metadata() {
        let kind =
            resolve_cloud_repository_kind(Some("sha256"), &[]).expect("empty index uses metadata");
        assert_eq!(kind, HashKind::Sha256);
    }

    #[test]
    fn cloud_object_format_metadata_row_conflict_fail_closed() {
        let err = resolve_cloud_repository_kind(Some("sha1"), &[row(&oid64(), None)])
            .expect_err("width conflict");
        assert!(matches!(err, CloudError::AmbiguousObjectFormat(_)));

        let err = resolve_cloud_repository_kind(Some("blake3"), &[row(&oid64(), Some("sha256"))])
            .expect_err("row format conflict");
        assert!(matches!(err, CloudError::AmbiguousObjectFormat(_)));
    }

    #[test]
    fn cloud_object_format_missing_metadata_fail_closed() {
        let err = resolve_cloud_repository_kind(None, &[]).expect_err("missing metadata");
        assert!(matches!(err, CloudError::AmbiguousObjectFormat(_)));
        let err = resolve_cloud_repository_kind(Some("   "), &[]).expect_err("blank metadata");
        assert!(matches!(err, CloudError::AmbiguousObjectFormat(_)));
        let err = resolve_cloud_repository_kind(Some("sha512"), &[]).expect_err("illegal metadata");
        assert!(matches!(err, CloudError::AmbiguousObjectFormat(_)));
    }

    #[test]
    fn cloud_width_inference_negative_fixtures() {
        // Semantic stand-ins for `let n = o_id.len(); n == 64` style inference:
        // the resolver must never accept a lone 64-hex catalog without metadata.
        let aliases = [oid64(), format!("{}{}", "c".repeat(32), "d".repeat(32))];
        for oid in aliases {
            assert!(
                resolve_cloud_repository_kind(None, &[row(&oid, None)]).is_err(),
                "width-only inference must fail for {oid}"
            );
        }
        // Helper-indirect: even when every row carries a NULL object_format,
        // repository metadata remains required for non-sha1 widths.
        let with_null_rows = [row(&oid64(), None), row(&oid64(), Some(""))];
        assert!(resolve_cloud_repository_kind(None, &with_null_rows).is_err());
        assert_eq!(
            resolve_cloud_repository_kind(Some("blake3"), &with_null_rows).unwrap(),
            HashKind::Blake3
        );

        // Balanced-bracket function-node allowlist targets must remain
        // parseable in client_storage.rs (B3-14 cloud semantic guard).
        let src = include_str!("../../utils/client_storage.rs");
        for name in [
            "expected_index_repair_oid_len",
            "valid_index_repair_oid",
            "valid_index_repair_type",
        ] {
            assert!(
                extract_balanced_fn(src, name).is_some(),
                "function node '{name}' must be extractable by balanced braces"
            );
        }
    }

    #[test]
    fn cloud_restore_propagates_object_format() {
        let kind = resolve_cloud_repository_kind(Some("sha256"), &[row(&oid64(), None)]).unwrap();
        let propagated = crate::internal::object_format::as_str(kind);
        assert_eq!(propagated, "sha256");
    }

    #[test]
    fn cloud_sync_uses_repository_kind() {
        // Backup writes the process repository kind (from core.objectformat /
        // get_hash_kind), never OID width — covered by sync calling
        // upsert_*_with_format(as_str(get_hash_kind())).
        let src = include_str!("sync.rs");
        assert!(
            src.contains("upsert_repository_with_format")
                && src.contains("upsert_object_index_with_format")
                && src.contains("object_format::as_str"),
            "cloud sync must persist repository kind via with_format helpers"
        );
        assert!(
            !src.contains("o_id.len()"),
            "cloud sync must not inspect o_id width"
        );
    }

    #[test]
    fn cloud_agent_capture_uses_repository_kind() {
        let src = include_str!("agent_capture.rs");
        assert!(
            src.contains("upsert_object_index_with_format")
                && src.contains("object_format::as_str"),
            "agent-capture publish must tag rows with repository kind"
        );
        // Rebuild the forbidden width-inference form without embedding the
        // contiguous token sequence that the B3-14 zero-hit guard scans for.
        let needle = format!("{}{}{}{}", "o_id.", "len()", " == ", "64");
        assert!(
            !src.contains(&needle),
            "agent-capture must not infer kind from OID width"
        );
    }

    #[test]
    fn cloud_restore_ambiguous_64_json_error() {
        let err =
            resolve_cloud_repository_kind(None, &[row(&oid64(), None)]).expect_err("ambiguous 64");
        let cli = err.into_cli_error("restore");
        assert_eq!(cli.stable_code().as_str(), "LBR-REPO-002");
        // Structured envelope surface used by `--json` / `--machine`.
        let envelope = serde_json::to_value(cli.stable_code()).expect("serialize code");
        assert_eq!(envelope, serde_json::json!("LBR-REPO-002"));
        assert_eq!(cli.exit_code(), 128);
    }

    fn extract_balanced_fn(src: &str, name: &str) -> Option<(usize, usize)> {
        let needle = format!("fn {name}");
        let start = src.find(&needle)?;
        let brace = src[start..].find('{')? + start;
        let mut depth = 0i32;
        for (offset, ch) in src[brace..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start, brace + offset));
                    }
                }
                _ => {}
            }
        }
        None
    }
}
