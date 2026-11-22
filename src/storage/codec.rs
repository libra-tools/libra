//! Versioned `postcard` binary envelope codec (`[schema_ver: u16_le][postcard_bytes]`).
//!
//! Every `redb` value stored by Libra is prefixed with an explicit little-endian
//! `u16` schema version header (`ADR-RB-04` #2) so struct evolutions are
//! explicitly versioned and corrupt, truncated, or future-version bytes fail
//! closed with a typed [`StorageBackendError`].

use serde::{Serialize, de::DeserializeOwned};

use crate::storage::error::{StorageBackendError, StorageResult};

/// Fixed byte length of the leading `u16_le` schema version header.
pub const VERSION_HEADER_LEN: usize = 2;

/// Encode `value` into `[schema_ver: u16_le][postcard_bytes]`.
pub fn encode_versioned<T: Serialize>(schema_ver: u16, value: &T) -> StorageResult<Vec<u8>> {
    let payload = postcard::to_stdvec(value).map_err(|err| StorageBackendError::CodecEncode {
        context: "postcard serialize",
        detail: err.to_string(),
    })?;
    let mut out = Vec::with_capacity(VERSION_HEADER_LEN + payload.len());
    out.extend_from_slice(&schema_ver.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Inspect the leading 2-byte `u16_le` schema version header without decoding the payload.
pub fn decode_version_header(bytes: &[u8]) -> StorageResult<(u16, &[u8])> {
    if bytes.len() < VERSION_HEADER_LEN {
        return Err(StorageBackendError::TruncatedHeader {
            actual_len: bytes.len(),
        });
    }
    let version = u16::from_le_bytes([bytes[0], bytes[1]]);
    Ok((version, &bytes[VERSION_HEADER_LEN..]))
}

/// Decode raw `postcard` payload bytes for an already-validated `schema_ver`,
/// rejecting truncated inputs, malformed encodings, and trailing garbage bytes.
pub fn decode_postcard_payload<T: DeserializeOwned>(
    schema_ver: u16,
    payload: &[u8],
) -> StorageResult<T> {
    let (value, trailing) = postcard::take_from_bytes::<T>(payload).map_err(|err| match err {
        postcard::Error::DeserializeUnexpectedEnd => StorageBackendError::TruncatedPayload {
            schema_ver,
            actual_payload_len: payload.len(),
            detail: err.to_string(),
        },
        other => StorageBackendError::CorruptPayload {
            schema_ver,
            detail: other.to_string(),
        },
    })?;
    if !trailing.is_empty() {
        return Err(StorageBackendError::CorruptPayload {
            schema_ver,
            detail: format!(
                "unexpected {} trailing byte(s) after postcard payload",
                trailing.len()
            ),
        });
    }
    Ok(value)
}

/// Decode a versioned envelope by first checking `schema_ver <= max_supported_ver`
/// and then dispatching to a version-specific decoder closure.
///
/// Checking the version header before deserialization prevents older records
/// from ever being silently deserialized into a newer struct layout.
pub fn decode_versioned_dispatch<T, F>(
    bytes: &[u8],
    max_supported_ver: u16,
    decode_by_version: F,
) -> StorageResult<T>
where
    F: FnOnce(u16, &[u8]) -> StorageResult<T>,
{
    let (schema_ver, payload) = decode_version_header(bytes)?;
    if schema_ver > max_supported_ver {
        return Err(StorageBackendError::UnsupportedSchemaVersion {
            found: schema_ver,
            max_supported: max_supported_ver,
        });
    }
    decode_by_version(schema_ver, payload)
}

/// Decode a versioned envelope expecting an exact `expected_schema_ver`.
///
/// The version header is checked **before** attempting `postcard` deserialization:
/// - `schema_ver > expected_schema_ver` returns [`StorageBackendError::UnsupportedSchemaVersion`].
/// - `schema_ver < expected_schema_ver` returns [`StorageBackendError::CorruptPayload`]
///   without attempting to decode an older layout as `T`.
pub fn decode_versioned<T: DeserializeOwned>(
    bytes: &[u8],
    expected_schema_ver: u16,
) -> StorageResult<T> {
    decode_versioned_dispatch(bytes, expected_schema_ver, |schema_ver, payload| {
        if schema_ver != expected_schema_ver {
            return Err(StorageBackendError::CorruptPayload {
                schema_ver,
                detail: format!(
                    "unexpected schema version {schema_ver}, expected {expected_schema_ver}; use decode_versioned_dispatch for cross-version decoding"
                ),
            });
        }
        decode_postcard_payload(schema_ver, payload)
    })
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SampleRecordV1 {
        id: u64,
        name: String,
        flag: bool,
        tags: Vec<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SampleRecordV2 {
        name: String,
        id: u64,
        flag: bool,
        tags: Vec<String>,
        note: Option<String>,
    }

    #[test]
    fn versioned_postcard_roundtrip_and_corrupt_reject() {
        let record = SampleRecordV1 {
            id: 42,
            name: "refs/heads/main".to_string(),
            flag: true,
            tags: vec!["alpha".to_string(), "beta".to_string()],
        };

        // AC1: [schema_ver: u16_le][postcard_bytes] lossless round-trip.
        let encoded = encode_versioned(1, &record).expect("encode versioned record");
        assert!(encoded.len() > VERSION_HEADER_LEN);
        assert_eq!(&encoded[..2], &1u16.to_le_bytes());

        let decoded: SampleRecordV1 =
            decode_versioned(&encoded, 1).expect("decode versioned record");
        assert_eq!(decoded, record);

        // Cross-version dispatch explicitly decodes V1 via V1 type and upgrades to V2.
        let upgraded: SampleRecordV2 =
            decode_versioned_dispatch(&encoded, 2, |ver, payload| match ver {
                1 => {
                    let v1: SampleRecordV1 = decode_postcard_payload(ver, payload)?;
                    Ok(SampleRecordV2 {
                        name: v1.name,
                        id: v1.id,
                        flag: v1.flag,
                        tags: v1.tags,
                        note: None,
                    })
                }
                2 => decode_postcard_payload(ver, payload),
                other => Err(StorageBackendError::CorruptPayload {
                    schema_ver: other,
                    detail: format!("unhandled schema version {other}"),
                }),
            })
            .expect("explicit cross-version dispatch must succeed");
        assert_eq!(upgraded.id, 42);
        assert_eq!(upgraded.name, "refs/heads/main");
        assert_eq!(upgraded.note, None);

        // Exact-version decoder rejects older V1 bytes instead of decoding them as V2.
        let older_err = decode_versioned::<SampleRecordV2>(&encoded, 2)
            .expect_err("older schema version must not be decoded directly as V2");
        assert!(
            matches!(
                older_err,
                StorageBackendError::CorruptPayload { schema_ver: 1, .. }
            ),
            "expected CorruptPayload for older schema version, got {older_err:?}"
        );

        // AC2: corrupt payload bytes -> typed StorageBackendError::CorruptPayload.
        let mut corrupt_utf8 = encoded.clone();
        let name_pos = corrupt_utf8
            .windows(b"refs".len())
            .position(|w| w == b"refs")
            .expect("locate utf-8 string bytes");
        corrupt_utf8[name_pos] = 0xFF;
        let err = decode_versioned::<SampleRecordV1>(&corrupt_utf8, 1)
            .expect_err("corrupt utf-8 payload must fail");
        assert!(
            matches!(
                err,
                StorageBackendError::CorruptPayload { schema_ver: 1, .. }
            ),
            "expected CorruptPayload, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-IO-001");

        // Trailing garbage bytes after a valid payload must also fail closed.
        let mut with_trailing = encoded.clone();
        with_trailing.push(0xAA);
        let err = decode_versioned::<SampleRecordV1>(&with_trailing, 1)
            .expect_err("trailing bytes must be rejected");
        assert!(
            matches!(
                err,
                StorageBackendError::CorruptPayload { schema_ver: 1, .. }
            ),
            "expected CorruptPayload for trailing bytes, got {err:?}"
        );

        // AC3: < 2 bytes (missing version header) -> TruncatedHeader.
        for short in [&[][..], &[0x01][..]] {
            let err = decode_versioned::<SampleRecordV1>(short, 1)
                .expect_err("short header input must fail");
            assert!(
                matches!(
                    err,
                    StorageBackendError::TruncatedHeader { actual_len } if actual_len == short.len()
                ),
                "expected TruncatedHeader for len {}, got {err:?}",
                short.len()
            );
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
        }

        // AC4: valid 2-byte header with truncated payload (header-only and mid-payload) -> TruncatedPayload.
        for cut in [VERSION_HEADER_LEN, encoded.len() - 3, encoded.len() - 1] {
            let err = decode_versioned::<SampleRecordV1>(&encoded[..cut], 1)
                .expect_err("truncated payload must fail");
            assert!(
                matches!(
                    err,
                    StorageBackendError::TruncatedPayload {
                        schema_ver: 1,
                        actual_payload_len,
                        ..
                    } if actual_payload_len == cut - VERSION_HEADER_LEN
                ),
                "expected TruncatedPayload for cut {cut}, got {err:?}"
            );
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
        }

        // AC5: unknown higher schema_ver -> UnsupportedSchemaVersion.
        let future_bytes = encode_versioned(99, &record).expect("encode future schema version");
        let err = decode_versioned::<SampleRecordV1>(&future_bytes, 1)
            .expect_err("future schema version must fail");
        assert!(
            matches!(
                err,
                StorageBackendError::UnsupportedSchemaVersion {
                    found: 99,
                    max_supported: 1
                }
            ),
            "expected UnsupportedSchemaVersion, got {err:?}"
        );
        assert_eq!(err.stable_code_str(), "LBR-IO-001");
    }
}
