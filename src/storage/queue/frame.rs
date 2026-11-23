//! Self-authenticating 16-byte header + dual Castagnoli CRC32C frame wire
//! protocol for the capture dual-lane queue (`ADR-RB-05` #2, `RB-07`).
//!
//! ## Wire Layout
//!
//! ```text
//! Offset  Size   Field
//! 0       4      MAGIC             = b"LCE1"
//! 4       4      FRAME_BODY_LEN    = u32_le (1 + 2 + dedup_len + payload_len)
//! 8       4      HEADER_CRC32C     = crc32c(bytes[0..8])
//! 12      4      FRAME_CRC32C      = crc32c(bytes[0..12] || bytes[16..16+body_len])
//! 16      1      FLAGS             = u8
//! 17      2      DEDUP_KEY_LEN     = u16_le
//! 19      K      DEDUP_KEY_UTF8    = full UTF-8 semantic dedup_key (K bytes)
//! 19+K    P      POSTCARD_ENVELOPE = versioned postcard payload (P bytes)
//! ```

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::storage::{
    codec::{decode_versioned, encode_versioned},
    error::{StorageBackendError, StorageResult},
};

/// 4-byte wire magic identifying Libra Capture Event frame format v1.
pub const FRAME_MAGIC: [u8; 4] = *b"LCE1";

/// Fixed byte length of the self-authenticating frame header (`MAGIC` +
/// `FRAME_BODY_LEN` + `HEADER_CRC32C` + `FRAME_CRC32C`).
pub const FRAME_HEADER_LEN: usize = 16;

/// Fixed byte length of the body metadata prefix (`FLAGS: u8` + `DEDUP_KEY_LEN: u16_le`).
pub const FRAME_BODY_FIXED_OVERHEAD: usize = 3;

/// Default frame flags (ordinary `normal` lane event).
pub const FRAME_FLAG_NONE: u8 = 0;

/// Frame flag marking a priority-lane terminal event (e.g. `SessionEnd`).
pub const FRAME_FLAG_PRIORITY: u8 = 1 << 0;

/// Compute the Castagnoli CRC32C checksum (RFC 3720 polynomial) of `data`.
///
/// Satisfies the standard RFC 3720 check vector:
/// `crc32c_checksum(b"123456789") == 0xe306_9283`.
#[must_use]
pub fn crc32c_checksum(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}

/// Incrementally extend an existing Castagnoli CRC32C checksum with `data`.
#[must_use]
pub fn crc32c_append(previous_crc: u32, data: &[u8]) -> u32 {
    crc32c::crc32c_append(previous_crc, data)
}

/// Decoded capture queue frame carrying `flags`, full UTF-8 semantic
/// `dedup_key`, and raw `postcard` payload envelope bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub flags: u8,
    pub dedup_key: String,
    pub payload: Vec<u8>,
}

/// Errors returned when encoding a capture queue frame.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameEncodeError {
    #[error("capture queue frame dedup_key must not be empty")]
    EmptyDedupKey,
    #[error("capture queue frame dedup_key must not contain NUL bytes")]
    DedupKeyContainsNul,
    #[error("capture queue frame dedup_key length ({len} B) exceeds wire maximum ({max} B)")]
    DedupKeyTooLong { len: usize, max: usize },
    #[error("capture queue frame body length overflows u32 wire limit")]
    BodyLengthOverflow,
    #[error("capture queue frame postcard serialization failed: {detail}")]
    Codec { detail: String },
}

impl FrameEncodeError {
    /// Convert into a sanitized [`StorageBackendError`] (`LBR-IO-001`).
    #[must_use]
    pub fn into_storage_error(self) -> StorageBackendError {
        StorageBackendError::BackendInternal {
            detail: self.to_string(),
        }
    }
}

/// Errors returned when decoding a capture queue frame.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FrameDecodeError {
    #[error("corrupt capture queue frame header: {detail}")]
    CorruptHeader { detail: &'static str },
    #[error("corrupt capture queue frame payload: {detail}")]
    CorruptPayload { detail: &'static str },
    #[error("truncated capture queue frame tail (available={available} B, needed={needed} B)")]
    TruncatedTail { available: usize, needed: usize },
}

impl FrameDecodeError {
    /// Convert into a sanitized [`StorageBackendError`] (`LBR-IO-001`).
    #[must_use]
    pub fn into_storage_error(self) -> StorageBackendError {
        StorageBackendError::BackendInternal {
            detail: self.to_string(),
        }
    }
}

/// Encode a capture queue frame into a single contiguous buffer carrying the
/// fixed 16-byte self-authenticating header (`MAGIC`, `FRAME_BODY_LEN`,
/// `HEADER_CRC32C`, `FRAME_CRC32C`) followed by `[FLAGS][DEDUP_KEY_LEN]
/// [DEDUP_KEY_UTF8][POSTCARD_ENVELOPE]`.
pub fn encode_frame(
    flags: u8,
    dedup_key: &str,
    postcard_envelope: &[u8],
) -> Result<Vec<u8>, FrameEncodeError> {
    if dedup_key.is_empty() {
        return Err(FrameEncodeError::EmptyDedupKey);
    }
    if dedup_key.as_bytes().contains(&0) {
        return Err(FrameEncodeError::DedupKeyContainsNul);
    }
    let dedup_bytes = dedup_key.as_bytes();
    let dedup_len_u16 =
        u16::try_from(dedup_bytes.len()).map_err(|_| FrameEncodeError::DedupKeyTooLong {
            len: dedup_bytes.len(),
            max: usize::from(u16::MAX),
        })?;

    let body_len_usize = FRAME_BODY_FIXED_OVERHEAD
        .checked_add(dedup_bytes.len())
        .and_then(|n| n.checked_add(postcard_envelope.len()))
        .ok_or(FrameEncodeError::BodyLengthOverflow)?;
    let body_len_u32 =
        u32::try_from(body_len_usize).map_err(|_| FrameEncodeError::BodyLengthOverflow)?;
    let total_len = FRAME_HEADER_LEN
        .checked_add(body_len_usize)
        .ok_or(FrameEncodeError::BodyLengthOverflow)?;

    let mut buf = Vec::with_capacity(total_len);
    buf.extend_from_slice(&FRAME_MAGIC);
    buf.extend_from_slice(&body_len_u32.to_le_bytes());

    let header_crc = crc32c_checksum(&buf[0..8]);
    buf.extend_from_slice(&header_crc.to_le_bytes());
    // Placeholder for FRAME_CRC32C at bytes 12..16.
    buf.extend_from_slice(&0u32.to_le_bytes());

    buf.push(flags);
    buf.extend_from_slice(&dedup_len_u16.to_le_bytes());
    buf.extend_from_slice(dedup_bytes);
    buf.extend_from_slice(postcard_envelope);

    // FRAME_CRC32C covers all non-FRAME_CRC32C bytes:
    // [MAGIC || FRAME_BODY_LEN || HEADER_CRC32C || body].
    let frame_crc = crc32c_append(crc32c_checksum(&buf[0..12]), &buf[FRAME_HEADER_LEN..]);
    buf[12..16].copy_from_slice(&frame_crc.to_le_bytes());

    Ok(buf)
}

/// Serialize `value` with versioned `postcard` framing (`crate::storage::codec`)
/// and encode it into a dual-CRC32C capture queue frame.
pub fn encode_postcard_frame<T: Serialize>(
    flags: u8,
    dedup_key: &str,
    schema_version: u16,
    value: &T,
) -> Result<Vec<u8>, FrameEncodeError> {
    let envelope =
        encode_versioned(schema_version, value).map_err(|err| FrameEncodeError::Codec {
            detail: err.stable_code_str().to_string(),
        })?;
    encode_frame(flags, dedup_key, &envelope)
}

/// Decode a single dual-CRC32C capture queue frame from the beginning of
/// `bytes`, returning `(DecodedFrame, bytes_consumed)`.
pub fn decode_frame(bytes: &[u8]) -> Result<(DecodedFrame, usize), FrameDecodeError> {
    if bytes.len() < FRAME_HEADER_LEN {
        return Err(FrameDecodeError::TruncatedTail {
            available: bytes.len(),
            needed: FRAME_HEADER_LEN,
        });
    }

    if bytes[0..4] != FRAME_MAGIC {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "magic mismatch",
        });
    }

    let body_len_u32 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let stored_header_crc = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let computed_header_crc = crc32c_checksum(&bytes[0..8]);
    if stored_header_crc != computed_header_crc {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "header crc32c mismatch",
        });
    }

    let body_len = usize::try_from(body_len_u32).map_err(|_| FrameDecodeError::CorruptHeader {
        detail: "frame body length exceeds platform usize",
    })?;
    let total_len =
        FRAME_HEADER_LEN
            .checked_add(body_len)
            .ok_or(FrameDecodeError::CorruptHeader {
                detail: "frame total length overflow",
            })?;
    if bytes.len() < total_len {
        return Err(FrameDecodeError::TruncatedTail {
            available: bytes.len(),
            needed: total_len,
        });
    }

    let stored_frame_crc = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let computed_frame_crc = crc32c_append(
        crc32c_checksum(&bytes[0..12]),
        &bytes[FRAME_HEADER_LEN..total_len],
    );
    if stored_frame_crc != computed_frame_crc {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "frame crc32c mismatch",
        });
    }

    let body = &bytes[FRAME_HEADER_LEN..total_len];
    if body.len() < FRAME_BODY_FIXED_OVERHEAD {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "frame body shorter than fixed metadata prefix",
        });
    }

    let flags = body[0];
    let dedup_len = usize::from(u16::from_le_bytes([body[1], body[2]]));
    let dedup_end = FRAME_BODY_FIXED_OVERHEAD.checked_add(dedup_len).ok_or(
        FrameDecodeError::CorruptPayload {
            detail: "dedup_key length overflow",
        },
    )?;
    if dedup_end > body.len() {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "dedup_key length exceeds frame body length",
        });
    }
    if dedup_len == 0 {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "empty dedup_key in frame body",
        });
    }

    let dedup_raw = &body[FRAME_BODY_FIXED_OVERHEAD..dedup_end];
    if dedup_raw.contains(&0) {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "dedup_key contains NUL byte",
        });
    }
    let dedup_key = std::str::from_utf8(dedup_raw)
        .map_err(|_| FrameDecodeError::CorruptPayload {
            detail: "dedup_key is not valid UTF-8",
        })?
        .to_string();

    let payload = body[dedup_end..].to_vec();
    Ok((
        DecodedFrame {
            flags,
            dedup_key,
            payload,
        },
        total_len,
    ))
}

/// Decode the versioned `postcard` payload carried inside a [`DecodedFrame`].
pub fn decode_postcard_payload<T: DeserializeOwned>(
    frame: &DecodedFrame,
    expected_schema_version: u16,
) -> StorageResult<T> {
    decode_versioned(&frame.payload, expected_schema_version)
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct SampleCaptureEnvelope {
        session_id: String,
        seq: u64,
        event_kind: String,
        body: Vec<u8>,
    }

    /// `RB-07` AC1–AC4:
    /// - AC1: `frame::crc32c_checksum` matches RFC 3720 standard vector `b"123456789" -> 0xe3069283`.
    /// - AC2: `encode_frame` / `encode_postcard_frame` produces an exact 16-byte header
    ///   (`MAGIC=b"LCE1"`, `FRAME_BODY_LEN`, `HEADER_CRC32C` over bytes `0..8`,
    ///   `FRAME_CRC32C` over all non-`FRAME_CRC32C` bytes).
    /// - AC3: Single-frame decoder round-trips `flags`, `dedup_key`, and `postcard` payload
    ///   100% losslessly.
    /// - AC4: Full UTF-8 semantic `dedup_key` is preserved verbatim so two long keys
    ///   differing only in their final byte remain completely distinct without collision.
    #[test]
    fn crc32c_rfc3720_vector_and_16b_dual_crc_roundtrip() {
        // AC1: RFC 3720 Castagnoli CRC32C standard check vector (and incremental append parity).
        assert_eq!(
            crc32c_checksum(b"123456789"),
            0xe306_9283,
            "AC1: RFC 3720 CRC32C check vector must equal 0xe3069283"
        );
        assert_eq!(
            crc32c_append(crc32c_checksum(b"1234"), b"56789"),
            0xe306_9283,
            "AC1: incremental crc32c_append must match single-shot RFC 3720 vector"
        );
        assert_eq!(crc32c_checksum(b""), 0);

        // AC2 + AC3: 16-byte header wire verification + lossless postcard roundtrip.
        let envelope = SampleCaptureEnvelope {
            session_id: "sess-rb07-001".to_string(),
            seq: 42,
            event_kind: "SessionEnd".to_string(),
            body: vec![0x00, 0x11, 0x22, 0xfe, 0xff],
        };
        let dedup_key = "codex:sess-rb07-001:turn-42:中文语义键";
        let encoded = encode_postcard_frame(FRAME_FLAG_PRIORITY, dedup_key, 1, &envelope)
            .expect("AC2: encode postcard frame must succeed");

        let expected_envelope = encode_versioned(1, &envelope).expect("encode expected envelope");
        let expected_body_len =
            FRAME_BODY_FIXED_OVERHEAD + dedup_key.len() + expected_envelope.len();
        assert_eq!(
            encoded.len(),
            FRAME_HEADER_LEN + expected_body_len,
            "AC2: total frame size must be 16B header + body_len"
        );
        assert_eq!(
            &encoded[0..4],
            b"LCE1",
            "AC2: bytes 0..4 must carry MAGIC=b\"LCE1\""
        );
        assert_eq!(
            u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]),
            expected_body_len as u32,
            "AC2: bytes 4..8 must carry FRAME_BODY_LEN (u32_le)"
        );
        let expected_header_crc = crc32c_checksum(&encoded[0..8]);
        assert_eq!(
            u32::from_le_bytes([encoded[8], encoded[9], encoded[10], encoded[11]]),
            expected_header_crc,
            "AC2: bytes 8..12 must carry HEADER_CRC32C over bytes 0..8"
        );
        let expected_frame_crc = crc32c_append(
            crc32c_checksum(&encoded[0..12]),
            &encoded[FRAME_HEADER_LEN..],
        );
        assert_eq!(
            u32::from_le_bytes([encoded[12], encoded[13], encoded[14], encoded[15]]),
            expected_frame_crc,
            "AC2: bytes 12..16 must carry FRAME_CRC32C over all non-FRAME_CRC32C bytes"
        );
        assert_eq!(encoded[16], FRAME_FLAG_PRIORITY);
        assert_eq!(
            u16::from_le_bytes([encoded[17], encoded[18]]),
            dedup_key.len() as u16
        );
        assert_eq!(&encoded[19..19 + dedup_key.len()], dedup_key.as_bytes());
        assert_eq!(
            &encoded[19 + dedup_key.len()..],
            expected_envelope.as_slice()
        );

        // AC3: Single-frame lossless decode (including trailing bytes ignored beyond frame end).
        let mut with_trailing = encoded.clone();
        with_trailing.extend_from_slice(b"next-frame-bytes");
        let (decoded, consumed) =
            decode_frame(&with_trailing).expect("AC3: decode_frame must succeed");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.flags, FRAME_FLAG_PRIORITY);
        assert_eq!(decoded.dedup_key, dedup_key);
        assert_eq!(decoded.payload, expected_envelope);
        let roundtripped: SampleCaptureEnvelope =
            decode_postcard_payload(&decoded, 1).expect("AC3: decode_postcard_payload must match");
        assert_eq!(roundtripped, envelope);

        // Also verify empty-payload frame roundtrips losslessly.
        let empty_encoded =
            encode_frame(FRAME_FLAG_NONE, "empty-payload-key", &[]).expect("encode empty payload");
        let (empty_decoded, empty_consumed) =
            decode_frame(&empty_encoded).expect("decode empty payload");
        assert_eq!(empty_consumed, empty_encoded.len());
        assert_eq!(empty_decoded.flags, FRAME_FLAG_NONE);
        assert_eq!(empty_decoded.dedup_key, "empty-payload-key");
        assert!(empty_decoded.payload.is_empty());

        // AC4: Two long UTF-8 semantic dedup keys differing ONLY in the very last byte
        // must encode and decode distinctly with zero truncation or collision.
        let shared_prefix = format!(
            "provider:claude_code:repo:{}:turn:{}",
            "a".repeat(200),
            "段".repeat(60) // 180 UTF-8 bytes
        );
        let key_a = format!("{shared_prefix}:x");
        let key_b = format!("{shared_prefix}:y");
        assert_eq!(key_a.len(), key_b.len());
        assert!(key_a.len() > 400);
        assert_eq!(
            &key_a.as_bytes()[..key_a.len() - 1],
            &key_b.as_bytes()[..key_b.len() - 1],
            "keys must differ strictly in their final byte"
        );
        assert_ne!(
            key_a.as_bytes()[key_a.len() - 1],
            key_b.as_bytes()[key_b.len() - 1]
        );

        let frame_a = encode_postcard_frame(FRAME_FLAG_NONE, &key_a, 1, &envelope)
            .expect("encode long key A");
        let frame_b = encode_postcard_frame(FRAME_FLAG_NONE, &key_b, 1, &envelope)
            .expect("encode long key B");
        assert_ne!(frame_a, frame_b);

        let (dec_a, len_a) = decode_frame(&frame_a).expect("decode long key A");
        let (dec_b, len_b) = decode_frame(&frame_b).expect("decode long key B");
        assert_eq!(len_a, frame_a.len());
        assert_eq!(len_b, frame_b.len());
        assert_eq!(dec_a.dedup_key, key_a);
        assert_eq!(dec_b.dedup_key, key_b);
        assert_ne!(
            dec_a.dedup_key, dec_b.dedup_key,
            "AC4: full semantic dedup_keys differing in the last byte must remain distinct"
        );
        assert_eq!(
            decode_postcard_payload::<SampleCaptureEnvelope>(&dec_a, 1).expect("payload A"),
            envelope
        );
        assert_eq!(
            decode_postcard_payload::<SampleCaptureEnvelope>(&dec_b, 1).expect("payload B"),
            envelope
        );

        // Basic encoder validation sanity checks (empty / NUL dedup_key rejected).
        assert_eq!(
            encode_frame(FRAME_FLAG_NONE, "", b"p"),
            Err(FrameEncodeError::EmptyDedupKey)
        );
        assert_eq!(
            encode_frame(FRAME_FLAG_NONE, "bad\0key", b"p"),
            Err(FrameEncodeError::DedupKeyContainsNul)
        );

        // Sanitized `into_storage_error()` conversion checks (stable code LBR-IO-001, zero key/payload leak).
        let secret_key = "secret-provider-session-token-xyz\0nul";
        let enc_storage_err = encode_frame(FRAME_FLAG_NONE, secret_key, b"secret-payload-999")
            .expect_err("NUL key must fail")
            .into_storage_error();
        assert_eq!(enc_storage_err.stable_code_str(), "LBR-IO-001");
        let enc_msg = enc_storage_err.to_string();
        assert!(
            !enc_msg.contains("secret-provider-session-token-xyz")
                && !enc_msg.contains("secret-payload-999"),
            "encode storage error must not leak raw key or payload: {enc_msg}"
        );

        // Helper to craft a raw frame with freshly computed valid HEADER_CRC32C and FRAME_CRC32C
        // so structural body invariants are exercised independently of CRC checks.
        let craft_valid_crc_frame = |body: &[u8]| -> Vec<u8> {
            let mut raw = Vec::with_capacity(FRAME_HEADER_LEN + body.len());
            raw.extend_from_slice(&FRAME_MAGIC);
            raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
            let hdr_crc = crc32c_checksum(&raw[0..8]);
            raw.extend_from_slice(&hdr_crc.to_le_bytes());
            raw.extend_from_slice(&0u32.to_le_bytes());
            raw.extend_from_slice(body);
            let frm_crc = crc32c_append(crc32c_checksum(&raw[0..12]), &raw[FRAME_HEADER_LEN..]);
            raw[12..16].copy_from_slice(&frm_crc.to_le_bytes());
            raw
        };

        // Structural body rejection 1: DEDUP_KEY_LEN exceeds remaining body bytes.
        let mut oversized_klen_body = vec![FRAME_FLAG_NONE];
        oversized_klen_body.extend_from_slice(&100u16.to_le_bytes());
        oversized_klen_body.extend_from_slice(b"short");
        let err_oversized = decode_frame(&craft_valid_crc_frame(&oversized_klen_body))
            .expect_err("oversized dedup_key_len must fail closed");
        assert_eq!(
            err_oversized,
            FrameDecodeError::CorruptPayload {
                detail: "dedup_key length exceeds frame body length"
            }
        );

        // Structural body rejection 2: DEDUP_KEY_LEN == 0.
        let mut zero_klen_body = vec![FRAME_FLAG_NONE];
        zero_klen_body.extend_from_slice(&0u16.to_le_bytes());
        zero_klen_body.extend_from_slice(b"payload");
        assert_eq!(
            decode_frame(&craft_valid_crc_frame(&zero_klen_body)),
            Err(FrameDecodeError::CorruptPayload {
                detail: "empty dedup_key in frame body"
            })
        );

        // Structural body rejection 3: NUL byte inside DEDUP_KEY_UTF8.
        let mut nul_key_body = vec![FRAME_FLAG_NONE];
        let nul_key = b"secret-nul\0key";
        nul_key_body.extend_from_slice(&(nul_key.len() as u16).to_le_bytes());
        nul_key_body.extend_from_slice(nul_key);
        let err_nul = decode_frame(&craft_valid_crc_frame(&nul_key_body))
            .expect_err("NUL in dedup_key must fail closed");
        assert_eq!(
            err_nul,
            FrameDecodeError::CorruptPayload {
                detail: "dedup_key contains NUL byte"
            }
        );

        // Structural body rejection 4: Invalid UTF-8 inside DEDUP_KEY_UTF8 + sanitized LBR-IO-001 conversion.
        let mut bad_utf8_body = vec![FRAME_FLAG_NONE];
        let bad_utf8 = [b's', b'e', b'c', 0xff, 0xfe];
        bad_utf8_body.extend_from_slice(&(bad_utf8.len() as u16).to_le_bytes());
        bad_utf8_body.extend_from_slice(&bad_utf8);
        bad_utf8_body.extend_from_slice(b"secret-body-888");
        let err_utf8 = decode_frame(&craft_valid_crc_frame(&bad_utf8_body))
            .expect_err("invalid UTF-8 dedup_key must fail closed");
        assert_eq!(
            err_utf8,
            FrameDecodeError::CorruptPayload {
                detail: "dedup_key is not valid UTF-8"
            }
        );
        let dec_storage_err = err_utf8.into_storage_error();
        assert_eq!(dec_storage_err.stable_code_str(), "LBR-IO-001");
        let dec_msg = dec_storage_err.to_string();
        assert!(
            !dec_msg.contains("secret-body-888") && !dec_msg.contains("sec"),
            "decode storage error must not leak raw body or key bytes: {dec_msg}"
        );
    }
}
