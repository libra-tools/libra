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

use std::io;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::storage::{
    codec::decode_versioned,
    error::{StorageBackendError, StorageResult},
};

/// 4-byte wire magic identifying Libra Capture Event frame format v1.
pub const FRAME_MAGIC: [u8; 4] = *b"LCE1";

/// Fixed byte length of the self-authenticating frame header (`MAGIC` +
/// `FRAME_BODY_LEN` + `HEADER_CRC32C` + `FRAME_CRC32C`).
pub const FRAME_HEADER_LEN: usize = 16;

/// Fixed byte length of the body metadata prefix (`FLAGS: u8` + `DEDUP_KEY_LEN: u16_le`).
pub const FRAME_BODY_FIXED_OVERHEAD: usize = 3;

/// Maximum UTF-8 byte length of a frame's semantic `dedup_key` (`512 B`,
/// `ADR-RB-05` #2, `RB-07g`).
pub const MAX_FRAME_DEDUP_KEY_LEN: usize = 512;

/// Maximum byte length of a single frame's `POSTCARD_ENVELOPE` payload
/// (`4 MiB`, `ADR-RB-05` #2, `RB-07g`).
pub const MAX_FRAME_PAYLOAD_LEN: usize = 4 * 1024 * 1024;

/// Minimum legal `FRAME_BODY_LEN` (`3` bytes: `FLAGS` + `DEDUP_KEY_LEN`,
/// `ADR-RB-05` #2, `RB-07g`).
pub const MIN_FRAME_BODY_LEN: u32 = FRAME_BODY_FIXED_OVERHEAD as u32;

/// Maximum legal `FRAME_BODY_LEN` (`3 + 512 + 4 MiB`, `ADR-RB-05` #2, `RB-07g`).
pub const MAX_FRAME_BODY_LEN: u32 =
    (FRAME_BODY_FIXED_OVERHEAD + MAX_FRAME_DEDUP_KEY_LEN + MAX_FRAME_PAYLOAD_LEN) as u32;

const _: () = assert!(
    MAX_FRAME_BODY_LEN as usize == 3 + 512 + 4 * 1024 * 1024,
    "MAX_FRAME_BODY_LEN must equal 3 + 512 + 4 MiB"
);

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
    #[error("capture queue frame dedup_key length ({len} B) exceeds maximum ({max} B)")]
    DedupKeyTooLong { len: usize, max: usize },
    #[error("capture queue frame payload length ({len} B) exceeds maximum ({max} B)")]
    PayloadTooLarge { len: usize, max: usize },
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
    #[error("I/O error reading capture queue frame stream ({kind:?})")]
    Io { kind: io::ErrorKind },
}

/// Verified 16-byte capture queue frame header parsed strictly on the stack
/// before any body bytes are read or allocated (`ADR-RB-05` #2, `RB-07c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedFrameHeader {
    pub body_len: usize,
    pub header_crc: u32,
    pub frame_crc: u32,
}

/// Parse and cryptographically/structurally authenticate a fixed 16-byte frame
/// header *before* allocating any body buffer or reading body bytes (`RB-07c`):
///
/// 1. Verifies `HEADER_CRC32C` over bytes `0..8` (`[MAGIC || FRAME_BODY_LEN]`).
/// 2. Verifies `MAGIC == b"LCE1"`.
/// 3. Verifies `MIN_FRAME_BODY_LEN (3) <= FRAME_BODY_LEN <= MAX_FRAME_BODY_LEN`.
///
/// Any single-byte flip in `MAGIC`, `FRAME_BODY_LEN` (whether in-range or
/// oversized), or `HEADER_CRC32C` fails closed immediately with
/// [`FrameDecodeError::CorruptHeader`] and zero heap allocations.
pub fn parse_and_verify_header(
    header: &[u8; FRAME_HEADER_LEN],
) -> Result<ParsedFrameHeader, FrameDecodeError> {
    let stored_header_crc = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let computed_header_crc = crc32c_checksum(&header[0..8]);
    if stored_header_crc != computed_header_crc {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "header crc32c mismatch",
        });
    }

    if header[0..4] != FRAME_MAGIC {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "magic mismatch",
        });
    }

    let body_len_u32 = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let body_len = validate_frame_body_len(body_len_u32)?;
    let frame_crc = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);

    Ok(ParsedFrameHeader {
        body_len,
        header_crc: stored_header_crc,
        frame_crc,
    })
}

/// Item returned by [`FrameStreamDecoder::next_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameStreamItem {
    /// A complete, dual-CRC32C verified frame starting at byte `offset`.
    Frame {
        offset: u64,
        total_len: usize,
        frame: DecodedFrame,
    },
    /// Stream reached exact clean EOF at byte `offset` with zero leftover bytes.
    CleanEof { offset: u64 },
}

/// Sequential capture queue frame stream decoder over any [`io::Read`] source
/// with a mandatory trusted end boundary (`ADR-RB-05` #2, `RB-07c`, `RB-07f`).
///
/// Enforces two strict pre-allocation invariants on *every* decode step:
/// 1. Authenticates the fixed 16-byte header via [`parse_and_verify_header`] on
///    the stack before reading or allocating any body bytes, preventing forged
///    or bit-flipped `FRAME_BODY_LEN` headers from triggering OOM or being
///    misclassified as `TruncatedTail`.
/// 2. Confirms `end_offset - (frame_start + 16) >= FRAME_BODY_LEN` *before*
///    allocating any body buffer, guaranteeing zero heap allocation even for a
///    maximum-length (`~4 MiB`) frame missing only its final byte.
pub struct FrameStreamDecoder<R> {
    reader: R,
    offset: u64,
    end_offset: u64,
    terminal_error: Option<FrameDecodeError>,
    #[cfg(test)]
    body_allocations: usize,
    #[cfg(test)]
    max_body_capacity: usize,
}

impl<R: io::Read> FrameStreamDecoder<R> {
    /// Create a new sequential decoder starting at byte offset `0` with trusted
    /// stream length `total_len` (e.g. file length or slice length).
    #[must_use]
    pub const fn new(reader: R, total_len: u64) -> Self {
        Self::with_known_len(reader, 0, total_len)
    }

    /// Create a new sequential decoder starting at `offset` with an explicit
    /// trusted end offset `end_offset` (e.g. file metadata length or committed
    /// prefix boundary `committed_len`).
    ///
    /// Reading never crosses `end_offset`. Frames whose validated
    /// `FRAME_BODY_LEN` exceeds `end_offset - (frame_start + 16)` fail closed
    /// immediately with [`FrameDecodeError::TruncatedTail`] before any body
    /// read or heap allocation occurs (`ADR-RB-05` #2). If the underlying
    /// reader hits EOF before `end_offset` is reached, the decoder also fails
    /// closed with [`FrameDecodeError::TruncatedTail`] rather than returning
    /// [`FrameStreamItem::CleanEof`].
    #[must_use]
    pub const fn with_known_len(reader: R, offset: u64, end_offset: u64) -> Self {
        Self {
            reader,
            offset,
            end_offset,
            terminal_error: None,
            #[cfg(test)]
            body_allocations: 0,
            #[cfg(test)]
            max_body_capacity: 0,
        }
    }

    /// Current verified byte offset within the underlying stream.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Trusted stream end offset enforced by this decoder.
    #[must_use]
    pub const fn end_offset(&self) -> u64 {
        self.end_offset
    }

    /// Sticky terminal decode error if the decoder has transitioned into a
    /// fail-closed poisoned state.
    #[must_use]
    pub const fn terminal_error(&self) -> Option<&FrameDecodeError> {
        self.terminal_error.as_ref()
    }

    /// Borrow the underlying reader.
    #[must_use]
    pub const fn get_ref(&self) -> &R {
        &self.reader
    }

    /// Consume the decoder and return the underlying reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.reader
    }

    #[cfg(test)]
    pub(crate) const fn body_allocations(&self) -> usize {
        self.body_allocations
    }

    #[cfg(test)]
    pub(crate) const fn max_body_capacity(&self) -> usize {
        self.max_body_capacity
    }

    /// Read and decode the next frame from the stream.
    ///
    /// Fail-closed sticky error invariant: once any decode or I/O error occurs
    /// (which may have partially consumed stream bytes), the decoder is
    /// permanently poisoned with that error so subsequent `next_frame()` calls
    /// return the same error without reading further bytes or misclassifying
    /// leftover bytes as `CleanEof` or a misaligned frame.
    pub fn next_frame(&mut self) -> Result<FrameStreamItem, FrameDecodeError> {
        if let Some(err) = &self.terminal_error {
            return Err(err.clone());
        }
        match self.next_frame_inner() {
            Ok(item) => Ok(item),
            Err(err) => {
                self.terminal_error = Some(err.clone());
                Err(err)
            }
        }
    }

    fn next_frame_inner(&mut self) -> Result<FrameStreamItem, FrameDecodeError> {
        let frame_start = self.offset;
        if frame_start > self.end_offset {
            return Err(FrameDecodeError::CorruptHeader {
                detail: "stream offset exceeds known length",
            });
        }
        let remaining = self.end_offset - frame_start;
        if remaining == 0 {
            return Ok(FrameStreamItem::CleanEof {
                offset: frame_start,
            });
        }

        let hdr_limit =
            usize::try_from(remaining.min(FRAME_HEADER_LEN as u64)).unwrap_or(FRAME_HEADER_LEN);
        let mut header = [0u8; FRAME_HEADER_LEN];
        let hdr_read = read_up_to(&mut self.reader, &mut header[..hdr_limit])?;
        if hdr_read == 0 {
            // `remaining > 0` yet underlying reader yielded 0 bytes: the stream
            // was prematurely truncated short of its trusted `end_offset`!
            // Never misreport a prematurely exhausted stream as `CleanEof`.
            let needed = usize::try_from(remaining).unwrap_or(usize::MAX);
            return Err(FrameDecodeError::TruncatedTail {
                available: 0,
                needed,
            });
        }
        if hdr_read < FRAME_HEADER_LEN {
            return Err(FrameDecodeError::TruncatedTail {
                available: hdr_read,
                needed: FRAME_HEADER_LEN,
            });
        }

        // Authenticate header completely on stack BEFORE allocating or reading any body bytes.
        let parsed = parse_and_verify_header(&header)?;
        let total_len = FRAME_HEADER_LEN.checked_add(parsed.body_len).ok_or(
            FrameDecodeError::CorruptHeader {
                detail: "frame total length overflow",
            },
        )?;

        // ADR-RB-05 #2 mandatory pre-allocation remaining-length check:
        // confirm remaining_bytes >= FRAME_BODY_LEN strictly BEFORE allocating
        // any body buffer or reading body bytes.
        let body_start = frame_start.checked_add(FRAME_HEADER_LEN as u64).ok_or(
            FrameDecodeError::CorruptHeader {
                detail: "stream offset overflow",
            },
        )?;
        let remaining_body_bytes = self.end_offset.saturating_sub(body_start);
        if remaining_body_bytes < parsed.body_len as u64 {
            let avail_body = usize::try_from(remaining_body_bytes).unwrap_or(usize::MAX);
            return Err(FrameDecodeError::TruncatedTail {
                available: FRAME_HEADER_LEN.saturating_add(avail_body),
                needed: total_len,
            });
        }

        // Read body in bounded chunks (<= 8 KiB initial ceiling) so even if the
        // underlying file shrinks concurrently below `end_offset`, memory usage
        // stays bounded and early EOF fails closed as `TruncatedTail`.
        #[cfg(test)]
        let mut peak_cap = 0usize;
        let body = read_body_bounded(
            &mut self.reader,
            parsed.body_len,
            total_len,
            #[cfg(test)]
            &mut peak_cap,
        );
        #[cfg(test)]
        {
            if peak_cap > 0 {
                self.body_allocations += 1;
                if peak_cap > self.max_body_capacity {
                    self.max_body_capacity = peak_cap;
                }
            }
        }
        let body = body?;
        let frame = verify_and_decode_body(&header, &body, parsed.frame_crc)?;

        let total_u64 = u64::try_from(total_len).map_err(|_| FrameDecodeError::CorruptHeader {
            detail: "frame total length exceeds u64",
        })?;
        self.offset =
            self.offset
                .checked_add(total_u64)
                .ok_or(FrameDecodeError::CorruptHeader {
                    detail: "stream offset overflow",
                })?;

        Ok(FrameStreamItem::Frame {
            offset: frame_start,
            total_len,
            frame,
        })
    }
}

impl<R: io::Read + io::Seek> FrameStreamDecoder<R> {
    /// Construct a sequential decoder from a seekable source (`File`, `Cursor`,
    /// etc.), probing its current byte offset and total length so every
    /// `TruncatedTail` check executes strictly *before* any body buffer
    /// allocation (`ADR-RB-05` #2).
    pub fn from_seekable(mut reader: R) -> Result<Self, FrameDecodeError> {
        let pos = reader
            .stream_position()
            .map_err(|err| FrameDecodeError::Io { kind: err.kind() })?;
        let end = reader
            .seek(io::SeekFrom::End(0))
            .map_err(|err| FrameDecodeError::Io { kind: err.kind() })?;
        reader
            .seek(io::SeekFrom::Start(pos))
            .map_err(|err| FrameDecodeError::Io { kind: err.kind() })?;
        if pos > end {
            return Err(FrameDecodeError::Io {
                kind: io::ErrorKind::UnexpectedEof,
            });
        }
        Ok(Self::with_known_len(reader, pos, end))
    }
}

fn read_up_to<R: io::Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, FrameDecodeError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let remaining = buf.len() - filled;
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) if n > remaining => {
                return Err(FrameDecodeError::Io {
                    kind: io::ErrorKind::InvalidData,
                });
            }
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(FrameDecodeError::Io { kind: err.kind() }),
        }
    }
    Ok(filled)
}

fn read_body_bounded<R: io::Read>(
    reader: &mut R,
    body_len: usize,
    total_len: usize,
    #[cfg(test)] peak_cap: &mut usize,
) -> Result<Vec<u8>, FrameDecodeError> {
    const INITIAL_CAP_CEILING: usize = 8 * 1024;
    let mut body: Vec<u8> = Vec::new();
    let mut scratch = [0u8; 8192];
    while body.len() < body_len {
        let want = (body_len - body.len()).min(scratch.len());
        match reader.read(&mut scratch[..want]) {
            Ok(0) => {
                return Err(FrameDecodeError::TruncatedTail {
                    available: FRAME_HEADER_LEN.saturating_add(body.len()),
                    needed: total_len,
                });
            }
            Ok(n) if n > want => {
                return Err(FrameDecodeError::Io {
                    kind: io::ErrorKind::InvalidData,
                });
            }
            Ok(n) => {
                let next_len = body.len() + n;
                if body.capacity() == 0 {
                    let initial_cap = body_len.min(INITIAL_CAP_CEILING).max(next_len);
                    body.reserve_exact(initial_cap);
                } else if next_len > body.capacity() {
                    let target_cap = body
                        .capacity()
                        .saturating_mul(2)
                        .max(next_len)
                        .min(body_len);
                    body.reserve_exact(target_cap - body.len());
                }
                #[cfg(test)]
                {
                    *peak_cap = (*peak_cap).max(body.capacity());
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(FrameDecodeError::Io { kind: err.kind() }),
        }
    }
    Ok(body)
}

fn verify_and_decode_body(
    header: &[u8; FRAME_HEADER_LEN],
    body: &[u8],
    stored_frame_crc: u32,
) -> Result<DecodedFrame, FrameDecodeError> {
    if body.len() < FRAME_BODY_FIXED_OVERHEAD {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "frame body shorter than fixed overhead",
        });
    }
    let computed_frame_crc = crc32c_append(crc32c_checksum(&header[0..12]), body);
    if stored_frame_crc != computed_frame_crc {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "frame crc32c mismatch",
        });
    }

    let flags = body[0];
    let dedup_len = usize::from(u16::from_le_bytes([body[1], body[2]]));
    if dedup_len == 0 {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "empty dedup_key in frame body",
        });
    }
    if dedup_len > MAX_FRAME_DEDUP_KEY_LEN {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "dedup_key length exceeds MAX_FRAME_DEDUP_KEY_LEN",
        });
    }
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
    let payload_len = body.len() - dedup_end;
    if payload_len > MAX_FRAME_PAYLOAD_LEN {
        return Err(FrameDecodeError::CorruptPayload {
            detail: "payload length exceeds MAX_FRAME_PAYLOAD_LEN",
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
    Ok(DecodedFrame {
        flags,
        dedup_key,
        payload,
    })
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

fn validate_dedup_key_for_encode(dedup_key: &str) -> Result<u16, FrameEncodeError> {
    if dedup_key.is_empty() {
        return Err(FrameEncodeError::EmptyDedupKey);
    }
    if dedup_key.as_bytes().contains(&0) {
        return Err(FrameEncodeError::DedupKeyContainsNul);
    }
    let dedup_len = dedup_key.len();
    if dedup_len > MAX_FRAME_DEDUP_KEY_LEN {
        return Err(FrameEncodeError::DedupKeyTooLong {
            len: dedup_len,
            max: MAX_FRAME_DEDUP_KEY_LEN,
        });
    }
    u16::try_from(dedup_len).map_err(|_| FrameEncodeError::DedupKeyTooLong {
        len: dedup_len,
        max: MAX_FRAME_DEDUP_KEY_LEN,
    })
}

/// Validate a header's `FRAME_BODY_LEN` against the protocol bounds
/// `MIN_FRAME_BODY_LEN (3) <= body_len <= MAX_FRAME_BODY_LEN (3 + 512 + 4 MiB)`
/// *before* allocating any body buffer or classifying truncation (`ADR-RB-05` #2,
/// `RB-07g`).
pub fn validate_frame_body_len(body_len: u32) -> Result<usize, FrameDecodeError> {
    if body_len < MIN_FRAME_BODY_LEN {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "frame body length below minimum 3 bytes",
        });
    }
    if body_len > MAX_FRAME_BODY_LEN {
        return Err(FrameDecodeError::CorruptHeader {
            detail: "frame body length exceeds MAX_FRAME_BODY_LEN",
        });
    }
    usize::try_from(body_len).map_err(|_| FrameDecodeError::CorruptHeader {
        detail: "frame body length exceeds platform usize",
    })
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
    let dedup_len_u16 = validate_dedup_key_for_encode(dedup_key)?;
    if postcard_envelope.len() > MAX_FRAME_PAYLOAD_LEN {
        return Err(FrameEncodeError::PayloadTooLarge {
            len: postcard_envelope.len(),
            max: MAX_FRAME_PAYLOAD_LEN,
        });
    }
    let dedup_bytes = dedup_key.as_bytes();

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

/// Bounded `std::io::Write` sink for `postcard::to_io` that prepends
/// `[schema_ver: u16_le]` and aborts immediately once the versioned envelope
/// would exceed `MAX_FRAME_PAYLOAD_LEN`, clamping buffer growth strictly at
/// `max_len` so oversized inputs can never exhaust memory (`RB-07g`).
struct BoundedEnvelopeWriter {
    buf: Vec<u8>,
    max_len: usize,
    exceeded_len: Option<usize>,
}

impl BoundedEnvelopeWriter {
    fn new(schema_version: u16, max_len: usize) -> Self {
        let mut buf = Vec::with_capacity(256.min(max_len));
        buf.extend_from_slice(&schema_version.to_le_bytes());
        Self {
            buf,
            max_len,
            exceeded_len: None,
        }
    }
}

impl io::Write for BoundedEnvelopeWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let next_len = self.buf.len().saturating_add(data.len());
        if next_len > self.max_len {
            self.exceeded_len = Some(next_len);
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "postcard envelope exceeds MAX_FRAME_PAYLOAD_LEN",
            ));
        }
        if next_len > self.buf.capacity() {
            let target_cap = self
                .buf
                .capacity()
                .saturating_mul(2)
                .max(next_len)
                .min(self.max_len);
            self.buf.reserve_exact(target_cap - self.buf.len());
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialize `value` with bounded versioned `postcard` framing
/// (`[schema_ver: u16_le][postcard_bytes]`, stopping immediately if the
/// envelope exceeds [`MAX_FRAME_PAYLOAD_LEN`]) and encode it into a dual-CRC32C
/// capture queue frame.
pub fn encode_postcard_frame<T: Serialize>(
    flags: u8,
    dedup_key: &str,
    schema_version: u16,
    value: &T,
) -> Result<Vec<u8>, FrameEncodeError> {
    let _ = validate_dedup_key_for_encode(dedup_key)?;
    let mut writer = BoundedEnvelopeWriter::new(schema_version, MAX_FRAME_PAYLOAD_LEN);
    match postcard::to_io(value, &mut writer) {
        Ok(_) => encode_frame(flags, dedup_key, &writer.buf),
        Err(_) => {
            if let Some(len) = writer.exceeded_len {
                Err(FrameEncodeError::PayloadTooLarge {
                    len,
                    max: MAX_FRAME_PAYLOAD_LEN,
                })
            } else {
                Err(FrameEncodeError::Codec {
                    detail: "LBR-IO-001".to_string(),
                })
            }
        }
    }
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

    let mut header = [0u8; FRAME_HEADER_LEN];
    header.copy_from_slice(&bytes[0..FRAME_HEADER_LEN]);

    // Authenticate 16-byte header strictly on stack BEFORE checking remaining
    // slice length or allocating any body buffer (`RB-07c`).
    let parsed = parse_and_verify_header(&header)?;
    let total_len =
        FRAME_HEADER_LEN
            .checked_add(parsed.body_len)
            .ok_or(FrameDecodeError::CorruptHeader {
                detail: "frame total length overflow",
            })?;
    if bytes.len() < total_len {
        return Err(FrameDecodeError::TruncatedTail {
            available: bytes.len(),
            needed: total_len,
        });
    }

    let body = &bytes[FRAME_HEADER_LEN..total_len];
    let frame = verify_and_decode_body(&header, body, parsed.frame_crc)?;
    Ok((frame, total_len))
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
    use crate::storage::codec::encode_versioned;

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

    /// `RB-07g` AC1–AC4:
    /// - AC1: Payloads exceeding `4 MiB` fail closed during encoding.
    /// - AC2: `dedup_key` exceeding `512 B` fails closed during encoding.
    /// - AC3: `MAX_FRAME_BODY_LEN` equals `3 + 512 + 4 MiB` exactly.
    /// - AC4: Illegal body-length lower bound (`FRAME_BODY_LEN < 3`) fails
    ///   closed in the decode guard as `CorruptHeader` (alongside upper bound
    ///   `> MAX_FRAME_BODY_LEN` and component-level decoded bounds).
    #[test]
    fn frame_encode_limits_and_min_max_body_len_guard() {
        // AC3: Exact protocol constants.
        assert_eq!(MAX_FRAME_DEDUP_KEY_LEN, 512);
        assert_eq!(MAX_FRAME_PAYLOAD_LEN, 4 * 1024 * 1024);
        assert_eq!(MIN_FRAME_BODY_LEN, 3);
        assert_eq!(
            MAX_FRAME_BODY_LEN as usize,
            3 + 512 + 4 * 1024 * 1024,
            "AC3: MAX_FRAME_BODY_LEN must equal exactly 3 + 512 + 4 MiB"
        );

        // Exact maximum legal frame: 512 B dedup_key + 4 MiB payload -> body_len == MAX_FRAME_BODY_LEN.
        let max_key = "k".repeat(MAX_FRAME_DEDUP_KEY_LEN);
        let max_payload = vec![0x5au8; MAX_FRAME_PAYLOAD_LEN];
        let max_frame = encode_frame(FRAME_FLAG_PRIORITY, &max_key, &max_payload)
            .expect("exact 512B dedup_key + 4 MiB payload must encode cleanly");
        assert_eq!(
            max_frame.len(),
            FRAME_HEADER_LEN + MAX_FRAME_BODY_LEN as usize
        );
        assert_eq!(
            u32::from_le_bytes([max_frame[4], max_frame[5], max_frame[6], max_frame[7]]),
            MAX_FRAME_BODY_LEN
        );
        let (max_decoded, max_consumed) =
            decode_frame(&max_frame).expect("exact MAX_FRAME_BODY_LEN frame must decode cleanly");
        assert_eq!(max_consumed, max_frame.len());
        assert_eq!(max_decoded.flags, FRAME_FLAG_PRIORITY);
        assert_eq!(max_decoded.dedup_key.len(), MAX_FRAME_DEDUP_KEY_LEN);
        assert_eq!(max_decoded.payload.len(), MAX_FRAME_PAYLOAD_LEN);

        // AC1: Payload > 4 MiB fails closed at encode stage (both raw and postcard APIs).
        let oversized_payload = vec![0xa5u8; MAX_FRAME_PAYLOAD_LEN + 1];
        let payload_err = encode_frame(FRAME_FLAG_NONE, "ok-key", &oversized_payload)
            .expect_err("AC1: payload > 4 MiB must fail closed in encode_frame");
        assert_eq!(
            payload_err,
            FrameEncodeError::PayloadTooLarge {
                len: MAX_FRAME_PAYLOAD_LEN + 1,
                max: MAX_FRAME_PAYLOAD_LEN,
            }
        );
        assert_eq!(
            payload_err.into_storage_error().stable_code_str(),
            "LBR-IO-001"
        );

        let oversized_envelope = SampleCaptureEnvelope {
            session_id: "sess-oversized".to_string(),
            seq: 1,
            event_kind: "ToolUse".to_string(),
            body: vec![0x11u8; MAX_FRAME_PAYLOAD_LEN],
        };
        let postcard_payload_err =
            encode_postcard_frame(FRAME_FLAG_NONE, "ok-key", 1, &oversized_envelope)
                .expect_err("AC1: serialized postcard envelope > 4 MiB must fail closed");
        assert!(matches!(
            postcard_payload_err,
            FrameEncodeError::PayloadTooLarge {
                len,
                max: MAX_FRAME_PAYLOAD_LEN
            } if len > MAX_FRAME_PAYLOAD_LEN
        ));

        // AC2: dedup_key > 512 B fails closed at encode stage (both raw and postcard APIs, sanitized).
        let oversized_key = format!("secret-prefix-{}", "x".repeat(MAX_FRAME_DEDUP_KEY_LEN));
        assert!(oversized_key.len() > MAX_FRAME_DEDUP_KEY_LEN);
        let key_err = encode_frame(FRAME_FLAG_NONE, &oversized_key, b"p")
            .expect_err("AC2: dedup_key > 512 B must fail closed in encode_frame");
        assert_eq!(
            key_err,
            FrameEncodeError::DedupKeyTooLong {
                len: oversized_key.len(),
                max: MAX_FRAME_DEDUP_KEY_LEN,
            }
        );
        let key_storage_err = key_err.into_storage_error();
        assert_eq!(key_storage_err.stable_code_str(), "LBR-IO-001");
        assert!(
            !key_storage_err.to_string().contains("secret-prefix"),
            "dedup_key length error must not leak raw key content"
        );

        let exact_513_key = "z".repeat(MAX_FRAME_DEDUP_KEY_LEN + 1);
        assert_eq!(
            encode_postcard_frame(FRAME_FLAG_NONE, &exact_513_key, 1, &42u64),
            Err(FrameEncodeError::DedupKeyTooLong {
                len: MAX_FRAME_DEDUP_KEY_LEN + 1,
                max: MAX_FRAME_DEDUP_KEY_LEN,
            })
        );

        // Helper to craft a raw header+body with valid HEADER_CRC32C and FRAME_CRC32C.
        let craft_header_only = |declared_body_len: u32| -> [u8; FRAME_HEADER_LEN] {
            let mut hdr = [0u8; FRAME_HEADER_LEN];
            hdr[0..4].copy_from_slice(&FRAME_MAGIC);
            hdr[4..8].copy_from_slice(&declared_body_len.to_le_bytes());
            let hdr_crc = crc32c_checksum(&hdr[0..8]);
            hdr[8..12].copy_from_slice(&hdr_crc.to_le_bytes());
            hdr
        };
        let craft_full = |body: &[u8]| -> Vec<u8> {
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

        // AC4: Illegal body-length lower bound (`FRAME_BODY_LEN < 3`: 0, 1, 2) rejected
        // fail-closed as `CorruptHeader` by both `validate_frame_body_len` and `decode_frame`
        // (even when no body bytes follow, never misclassified as `TruncatedTail`).
        for bad_len in [0u32, 1u32, 2u32] {
            assert_eq!(
                validate_frame_body_len(bad_len),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "frame body length below minimum 3 bytes",
                }),
                "AC4: validate_frame_body_len({bad_len}) must reject < 3"
            );
            let hdr_only = craft_header_only(bad_len);
            assert_eq!(
                decode_frame(&hdr_only),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "frame body length below minimum 3 bytes",
                }),
                "AC4: header-only frame with body_len={bad_len} must reject as CorruptHeader"
            );
            let full = craft_full(&vec![0u8; bad_len as usize]);
            assert_eq!(
                decode_frame(&full),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "frame body length below minimum 3 bytes",
                }),
                "AC4: full valid-CRC frame with body_len={bad_len} must reject as CorruptHeader"
            );
        }

        // Upper-bound pre-allocation guard: `FRAME_BODY_LEN == MAX_FRAME_BODY_LEN + 1`
        // (and `u32::MAX`) rejected as `CorruptHeader` before inspecting body or allocating.
        for huge_len in [MAX_FRAME_BODY_LEN + 1, u32::MAX] {
            assert_eq!(
                validate_frame_body_len(huge_len),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "frame body length exceeds MAX_FRAME_BODY_LEN",
                })
            );
            let hdr_only = craft_header_only(huge_len);
            assert_eq!(
                decode_frame(&hdr_only),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "frame body length exceeds MAX_FRAME_BODY_LEN",
                })
            );
        }

        // Component-level decoded bounds: valid-CRC frames whose total body_len <= MAX_FRAME_BODY_LEN
        // still cannot smuggle `dedup_key > 512 B` or `payload > 4 MiB`!
        let mut smuggled_key_body = Vec::with_capacity(FRAME_BODY_FIXED_OVERHEAD + 513);
        smuggled_key_body.push(FRAME_FLAG_NONE);
        smuggled_key_body.extend_from_slice(&513u16.to_le_bytes());
        smuggled_key_body.extend_from_slice(&vec![b'a'; 513]);
        assert_eq!(
            decode_frame(&craft_full(&smuggled_key_body)),
            Err(FrameDecodeError::CorruptPayload {
                detail: "dedup_key length exceeds MAX_FRAME_DEDUP_KEY_LEN",
            })
        );

        let mut smuggled_payload_body =
            Vec::with_capacity(FRAME_BODY_FIXED_OVERHEAD + 1 + MAX_FRAME_PAYLOAD_LEN + 1);
        smuggled_payload_body.push(FRAME_FLAG_NONE);
        smuggled_payload_body.extend_from_slice(&1u16.to_le_bytes());
        smuggled_payload_body.push(b'k');
        smuggled_payload_body.resize(
            smuggled_payload_body.len() + MAX_FRAME_PAYLOAD_LEN + 1,
            0x77,
        );
        assert_eq!(
            decode_frame(&craft_full(&smuggled_payload_body)),
            Err(FrameDecodeError::CorruptPayload {
                detail: "payload length exceeds MAX_FRAME_PAYLOAD_LEN",
            })
        );

        // Codex R1 P1 regression: streaming bounded postcard serialization MUST stop
        // immediately once the versioned envelope crosses 4 MiB without materializing
        // a giant virtual input (1 GiB logical payload).
        struct GiantVirtualStream {
            total_chunks: usize,
            emitted_chunks: std::sync::atomic::AtomicUsize,
        }
        impl Serialize for GiantVirtualStream {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut seq = serializer.serialize_seq(Some(self.total_chunks))?;
                let chunk = [0x42u8; 64 * 1024]; // 64 KiB per chunk
                for _ in 0..self.total_chunks {
                    let count = self
                        .emitted_chunks
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;
                    // 4 MiB / 64 KiB = 64 chunks; fail hard if serializer ever kept running past 80 chunks!
                    assert!(
                        count <= 80,
                        "bounded postcard serializer failed to stop early at 4 MiB (emitted {count} chunks)"
                    );
                    seq.serialize_element(&chunk.as_slice())?;
                }
                seq.end()
            }
        }
        let giant = GiantVirtualStream {
            total_chunks: 16_384, // 1 GiB logical payload!
            emitted_chunks: std::sync::atomic::AtomicUsize::new(0),
        };
        let giant_err = encode_postcard_frame(FRAME_FLAG_NONE, "bounded-stream-key", 1, &giant)
            .expect_err("1 GiB virtual stream must fail closed at 4 MiB bound");
        assert!(matches!(
            giant_err,
            FrameEncodeError::PayloadTooLarge {
                len,
                max: MAX_FRAME_PAYLOAD_LEN
            } if len > MAX_FRAME_PAYLOAD_LEN
        ));
        let emitted = giant
            .emitted_chunks
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            (64..=66).contains(&emitted),
            "expected bounded serializer to abort right at 4 MiB (~64-65 chunks), emitted {emitted}"
        );
    }

    /// `RB-07c` AC1–AC4:
    /// Exercises sequential stream decoder (`FrameStreamDecoder`) and single-frame
    /// decoder (`decode_frame`) pre-allocation header authentication:
    /// - AC1: Single-byte flip in any byte of `MAGIC` (`0..4`) rejected by
    ///   `HEADER_CRC32C` with `FrameDecodeError::CorruptHeader` before body allocation.
    /// - AC2: Single-byte flip in `FRAME_BODY_LEN` (`4..8`) remaining within
    ///   remaining stream length rejected by `HEADER_CRC32C` with `CorruptHeader`
    ///   before body allocation.
    /// - AC3: Single-byte flip in `FRAME_BODY_LEN` (`4..8`) producing a huge value
    ///   (both `> remaining_bytes` and `> MAX_FRAME_BODY_LEN`) rejected by
    ///   `HEADER_CRC32C` with `CorruptHeader` (never `TruncatedTail`, zero huge alloc).
    /// - AC4: Single-byte flip in any byte of `HEADER_CRC32C` (`8..12`) rejected
    ///   with `CorruptHeader` before body allocation.
    #[test]
    fn acknowledged_frame_each_header_field_bitflip_rejected_pre_alloc() {
        /// Spy reader that records total bytes read and panics if the decoder
        /// attempts to read past `max_allowed_bytes` (proving zero body reads
        /// after a corrupt 16-byte header).
        struct PreAllocSpyReader<'a> {
            data: &'a [u8],
            pos: usize,
            max_allowed_bytes: usize,
        }
        impl<'a> PreAllocSpyReader<'a> {
            fn new(data: &'a [u8], max_allowed_bytes: usize) -> Self {
                Self {
                    data,
                    pos: 0,
                    max_allowed_bytes,
                }
            }
        }
        impl io::Read for PreAllocSpyReader<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let avail = self.data.len().saturating_sub(self.pos);
                let n = buf.len().min(avail);
                assert!(
                    self.pos + n <= self.max_allowed_bytes,
                    "decoder read past header boundary ({} > {}) on corrupt header!",
                    self.pos + n,
                    self.max_allowed_bytes
                );
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        let env = SampleCaptureEnvelope {
            session_id: "sess-rb07c".to_string(),
            seq: 7,
            event_kind: "TurnComplete".to_string(),
            body: vec![0xab; 128],
        };
        let frame1 = encode_postcard_frame(FRAME_FLAG_NONE, "rb07c:frame-1", 1, &env)
            .expect("encode frame1");
        let frame2 = encode_postcard_frame(FRAME_FLAG_PRIORITY, "rb07c:frame-2", 1, &env)
            .expect("encode frame2");
        // Build a stream with generous trailing bytes (> frame1 length) so we can
        // test in-range `FRAME_BODY_LEN` flips cleanly.
        let mut two_frames = frame1.clone();
        two_frames.extend_from_slice(&frame2);

        let assert_corrupt_header_pre_alloc =
            |corrupted: &[u8], expected_detail: &'static str, label: &str| {
                // Single-frame slice decoder
                assert_eq!(
                    decode_frame(corrupted),
                    Err(FrameDecodeError::CorruptHeader {
                        detail: expected_detail,
                    }),
                    "{label}: decode_frame must return CorruptHeader"
                );
                // Sequential stream decoder with strict 16-byte read ceiling + zero body allocation proof
                let spy = PreAllocSpyReader::new(corrupted, FRAME_HEADER_LEN);
                let mut dec = FrameStreamDecoder::new(spy, corrupted.len() as u64);
                assert_eq!(
                    dec.next_frame(),
                    Err(FrameDecodeError::CorruptHeader {
                        detail: expected_detail,
                    }),
                    "{label}: FrameStreamDecoder must return CorruptHeader"
                );
                assert_eq!(
                    dec.body_allocations(),
                    0,
                    "{label}: zero body buffer allocations permitted on corrupt header"
                );
                assert_eq!(
                    dec.get_ref().pos,
                    FRAME_HEADER_LEN,
                    "{label}: decoder must stop strictly after reading the 16-byte header"
                );
                assert_eq!(
                    dec.offset(),
                    0,
                    "{label}: stream offset must not advance on corrupt header"
                );
                // Codex R1 P1: Sticky poison invariant — retrying `next_frame()` after a
                // corrupt header MUST keep returning the same `CorruptHeader` without
                // reading further bytes or returning `CleanEof`/misaligned frames.
                assert_eq!(
                    dec.next_frame(),
                    Err(FrameDecodeError::CorruptHeader {
                        detail: expected_detail,
                    }),
                    "{label}: poisoned decoder must keep returning CorruptHeader on retry"
                );
                assert_eq!(dec.body_allocations(), 0);
                assert_eq!(dec.get_ref().pos, FRAME_HEADER_LEN);
                assert_eq!(dec.offset(), 0);
            };

        // AC1: Every single byte of MAGIC (`0..4`) flipped -> rejected by HEADER_CRC32C pre-alloc.
        for idx in 0..4usize {
            let mut mutated = two_frames.clone();
            mutated[idx] ^= 0x01;
            assert_corrupt_header_pre_alloc(
                &mutated,
                "header crc32c mismatch",
                &format!("AC1 MAGIC byte {idx}"),
            );
        }
        // Also verify forged wrong MAGIC with recomputed valid HEADER_CRC32C fails pre-alloc.
        {
            let mut forged_magic = two_frames.clone();
            forged_magic[3] = b'2'; // "LCE2"
            let recomputed_hdr_crc = crc32c_checksum(&forged_magic[0..8]);
            forged_magic[8..12].copy_from_slice(&recomputed_hdr_crc.to_le_bytes());
            assert_corrupt_header_pre_alloc(
                &forged_magic,
                "magic mismatch",
                "AC1 forged MAGIC with valid header CRC",
            );
        }

        // AC2: Single-byte flip in FRAME_BODY_LEN that stays strictly WITHIN remaining stream bytes.
        {
            let mut mutated = two_frames.clone();
            let orig_len = u32::from_le_bytes([mutated[4], mutated[5], mutated[6], mutated[7]]);
            mutated[4] ^= 0x08; // +/- 8 bytes, still >= 3 and <= two_frames.len() - 16
            let flipped_len = u32::from_le_bytes([mutated[4], mutated[5], mutated[6], mutated[7]]);
            assert_ne!(flipped_len, orig_len);
            assert!(
                flipped_len >= MIN_FRAME_BODY_LEN
                    && (flipped_len as usize) <= two_frames.len() - FRAME_HEADER_LEN,
                "AC2 setup: flipped_len ({flipped_len}) must remain within remaining stream bytes"
            );
            assert_corrupt_header_pre_alloc(
                &mutated,
                "header crc32c mismatch",
                "AC2 in-range FRAME_BODY_LEN byte 4 bitflip",
            );
        }

        // AC3: Single-byte flip in FRAME_BODY_LEN producing huge values:
        // (3a) > remaining stream bytes while still <= MAX_FRAME_BODY_LEN (never TruncatedTail!)
        {
            let mut mutated = frame1.clone();
            mutated[6] ^= 0x10; // adds +1 MiB (exceeds remaining bytes, <= 4 MiB)
            let flipped_len = u32::from_le_bytes([mutated[4], mutated[5], mutated[6], mutated[7]]);
            assert!((flipped_len as usize) > frame1.len() - FRAME_HEADER_LEN);
            assert!(flipped_len <= MAX_FRAME_BODY_LEN);
            assert_corrupt_header_pre_alloc(
                &mutated,
                "header crc32c mismatch",
                "AC3a >remaining-bytes <=MAX FRAME_BODY_LEN bitflip",
            );
        }
        // (3b) > MAX_FRAME_BODY_LEN (byte 7 MSB flip -> > 2 GiB, zero huge allocation!)
        {
            let mut mutated = frame1.clone();
            mutated[7] ^= 0x80; // > 2 GiB!
            let flipped_len = u32::from_le_bytes([mutated[4], mutated[5], mutated[6], mutated[7]]);
            assert!(flipped_len > MAX_FRAME_BODY_LEN);
            assert_corrupt_header_pre_alloc(
                &mutated,
                "header crc32c mismatch",
                "AC3b >2GiB MSB FRAME_BODY_LEN bitflip",
            );
        }
        // Exhaustively verify all 4 byte positions of FRAME_BODY_LEN (`4..8`).
        for idx in 4..8usize {
            let mut mutated = frame1.clone();
            mutated[idx] ^= 0x40;
            assert_corrupt_header_pre_alloc(
                &mutated,
                "header crc32c mismatch",
                &format!("AC2/AC3 FRAME_BODY_LEN byte {idx}"),
            );
        }

        // AC4: Every single byte of HEADER_CRC32C (`8..12`) flipped -> rejected pre-alloc.
        for idx in 8..12usize {
            let mut mutated = two_frames.clone();
            mutated[idx] ^= 0x01;
            assert_corrupt_header_pre_alloc(
                &mutated,
                "header crc32c mismatch",
                &format!("AC4 HEADER_CRC32C byte {idx}"),
            );
        }

        // Verify mid-stream corruption at frame 2 after frame 1 succeeded cleanly:
        // frame 1 allocates 1 body buffer, frame 2 corrupt header triggers 0 additional allocations
        // and stops strictly at `frame1.len() + 16`.
        {
            let mut second_corrupt = two_frames.clone();
            second_corrupt[frame1.len() + 7] ^= 0x80; // huge FRAME_BODY_LEN on frame 2
            let spy = PreAllocSpyReader::new(&second_corrupt, frame1.len() + FRAME_HEADER_LEN);
            let mut dec = FrameStreamDecoder::new(spy, second_corrupt.len() as u64);
            let item1 = dec.next_frame().expect("frame 1 decodes cleanly");
            assert!(matches!(
                item1,
                FrameStreamItem::Frame { offset: 0, total_len, ref frame }
                    if total_len == frame1.len() && frame.dedup_key == "rb07c:frame-1"
            ));
            assert_eq!(dec.body_allocations(), 1);
            assert_eq!(dec.offset(), frame1.len() as u64);

            let err2 = dec
                .next_frame()
                .expect_err("corrupt frame 2 header must fail closed pre-alloc");
            assert_eq!(
                err2,
                FrameDecodeError::CorruptHeader {
                    detail: "header crc32c mismatch",
                }
            );
            assert_eq!(
                dec.body_allocations(),
                1,
                "corrupt frame 2 must perform zero body allocations"
            );
            assert_eq!(dec.get_ref().pos, frame1.len() + FRAME_HEADER_LEN);
            assert_eq!(dec.offset(), frame1.len() as u64);
            // Retry on poisoned mid-stream decoder also stays fail-closed.
            assert_eq!(
                dec.next_frame(),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "header crc32c mismatch",
                })
            );
            assert_eq!(dec.body_allocations(), 1);
            assert_eq!(dec.get_ref().pos, frame1.len() + FRAME_HEADER_LEN);
        }

        // Exact 16-byte trailing corrupt header (no following body bytes): retry MUST
        // NOT report `CleanEof` even though the underlying reader is now at EOF!
        {
            let mut trailing_bad_hdr = frame1[..FRAME_HEADER_LEN].to_vec();
            trailing_bad_hdr[8] ^= 0x01; // flip HEADER_CRC32C
            let spy = PreAllocSpyReader::new(&trailing_bad_hdr, FRAME_HEADER_LEN);
            let mut dec = FrameStreamDecoder::new(spy, trailing_bad_hdr.len() as u64);
            assert_eq!(
                dec.next_frame(),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "header crc32c mismatch",
                })
            );
            assert_eq!(
                dec.next_frame(),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "header crc32c mismatch",
                }),
                "retrying after exact-16B trailing corrupt header must never return CleanEof"
            );
            assert_eq!(dec.body_allocations(), 0);
            assert_eq!(dec.offset(), 0);
        }
    }

    /// `RB-07f` AC1–AC4:
    /// Exercises sequential frame decoder (`FrameStreamDecoder`) stream-termination
    /// classification across all EOF and truncation boundaries:
    /// - AC1: 0-byte empty stream returns `FrameStreamItem::CleanEof`.
    /// - AC2: Multi-frame valid stream decodes every frame losslessly and returns
    ///   `CleanEof` at exact EOF (including chunked 1-byte reads + `Interrupted`).
    /// - AC3: Valid `HEADER_CRC32C` with remaining bytes `< FRAME_BODY_LEN`
    ///   (0 body bytes, partial body, mid-stream after valid frames, and huge
    ///   4 MiB valid header with 0/10 body bytes) is strictly classified as
    ///   `FrameDecodeError::TruncatedTail` (bounded allocation, sticky fail-closed).
    /// - AC4: Trailing `1..=15` bytes of incomplete header (both valid `LCE1`
    ///   prefix and arbitrary non-magic bytes, at stream start and after valid
    ///   frames) are strictly classified as `FrameDecodeError::TruncatedTail`
    ///   (strictly distinct from `CleanEof` and `CorruptHeader`, sticky fail-closed).
    #[test]
    fn seq_decoder_distinguishes_clean_eof_and_truncated_tail_variants() {
        /// Reader that yields at most `max_chunk` bytes per call and injects
        /// periodic `ErrorKind::Interrupted` retries before delivering data/EOF.
        struct ChunkedInterruptedReader<'a> {
            data: &'a [u8],
            pos: usize,
            max_chunk: usize,
            calls: usize,
        }
        impl<'a> ChunkedInterruptedReader<'a> {
            fn new(data: &'a [u8], max_chunk: usize) -> Self {
                Self {
                    data,
                    pos: 0,
                    max_chunk,
                    calls: 0,
                }
            }
        }
        impl io::Read for ChunkedInterruptedReader<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls % 3 == 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "simulated EINTR",
                    ));
                }
                let avail = self.data.len().saturating_sub(self.pos);
                let n = buf.len().min(avail).min(self.max_chunk);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        /// Reader that serves `prefix` bytes and then returns a deterministic
        /// non-`Interrupted` I/O error (`BrokenPipe`), tracking post-error calls
        /// to prove sticky error poisoning never reads further.
        struct FaultInjectingReader<'a> {
            prefix: &'a [u8],
            pos: usize,
            errored: bool,
            reads_after_error: usize,
        }
        impl<'a> FaultInjectingReader<'a> {
            fn new(prefix: &'a [u8]) -> Self {
                Self {
                    prefix,
                    pos: 0,
                    errored: false,
                    reads_after_error: 0,
                }
            }
        }
        impl io::Read for FaultInjectingReader<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.errored {
                    self.reads_after_error += 1;
                    return Ok(0);
                }
                let avail = self.prefix.len().saturating_sub(self.pos);
                if avail > 0 {
                    let n = buf.len().min(avail);
                    buf[..n].copy_from_slice(&self.prefix[self.pos..self.pos + n]);
                    self.pos += n;
                    return Ok(n);
                }
                self.errored = true;
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "simulated mid-stream I/O failure",
                ))
            }
        }

        // AC1: 0-byte empty stream -> CleanEof (idempotent on retry, preserves custom base offset,
        // handles EINTR cleanly, zero body allocations, no terminal error).
        {
            let mut dec = FrameStreamDecoder::new(io::Cursor::new(&[][..]), 0);
            assert_eq!(
                dec.next_frame(),
                Ok(FrameStreamItem::CleanEof { offset: 0 }),
                "AC1: empty stream must return CleanEof at offset 0"
            );
            assert_eq!(
                dec.next_frame(),
                Ok(FrameStreamItem::CleanEof { offset: 0 }),
                "AC1: repeated next_frame on empty stream must remain CleanEof"
            );
            assert_eq!(dec.offset(), 0);
            assert_eq!(dec.end_offset(), 0);
            assert_eq!(dec.body_allocations(), 0);
            assert_eq!(dec.terminal_error(), None);

            let mut dec_offset = FrameStreamDecoder::with_known_len(
                ChunkedInterruptedReader::new(&[], 1),
                4096,
                4096,
            );
            assert_eq!(
                dec_offset.next_frame(),
                Ok(FrameStreamItem::CleanEof { offset: 4096 }),
                "AC1: empty stream with base offset must return CleanEof at base offset"
            );
            assert_eq!(dec_offset.offset(), 4096);
            assert_eq!(dec_offset.terminal_error(), None);
        }

        // Prepare 4 heterogeneous valid frames (empty payload, small priority, UTF-8 key,
        // and >16 KiB multi-chunk body exercising bounded buffer growth).
        let env1 = SampleCaptureEnvelope {
            session_id: "sess-rb07f-1".to_string(),
            seq: 1,
            event_kind: "PromptSubmit".to_string(),
            body: b"alpha".to_vec(),
        };
        let env2 = SampleCaptureEnvelope {
            session_id: "sess-rb07f-2".to_string(),
            seq: 2,
            event_kind: "SessionEnd".to_string(),
            body: vec![0xde, 0xad, 0xbe, 0xef],
        };
        let env3 = SampleCaptureEnvelope {
            session_id: "sess-rb07f-3".to_string(),
            seq: 3,
            event_kind: "ToolOutput".to_string(),
            body: vec![0x5cu8; 20 * 1024], // > 16 KiB to exercise bounded Vec growth
        };
        let f0 = encode_frame(FRAME_FLAG_NONE, "rb07f:empty-payload", &[])
            .expect("encode f0 empty payload");
        let f1 = encode_postcard_frame(FRAME_FLAG_NONE, "rb07f:frame-1:正常", 1, &env1)
            .expect("encode f1");
        let f2 = encode_postcard_frame(FRAME_FLAG_PRIORITY, "rb07f:frame-2:终态", 1, &env2)
            .expect("encode f2");
        let f3 = encode_postcard_frame(FRAME_FLAG_NONE, "rb07f:frame-3:large", 1, &env3)
            .expect("encode f3");

        let frames = [&f0, &f1, &f2, &f3];
        let mut full_stream = Vec::new();
        for f in &frames {
            full_stream.extend_from_slice(f);
        }

        // AC2: Multi-frame valid stream lossless decode + exact CleanEof (both Cursor and 1-byte EINTR reader).
        for (mode, base_offset) in [("cursor", 0u64), ("chunked-1b-eintr", 512u64)] {
            let end_offset = base_offset + full_stream.len() as u64;
            let mut cursor_dec;
            let mut chunked_dec;
            let decoded_items: Vec<FrameStreamItem>;
            let final_eof: FrameStreamItem;
            let repeated_eof: FrameStreamItem;
            let final_offset: u64;
            let final_term_err: Option<FrameDecodeError>;

            if mode == "cursor" {
                cursor_dec = FrameStreamDecoder::with_known_len(
                    io::Cursor::new(full_stream.as_slice()),
                    base_offset,
                    end_offset,
                );
                let mut items = Vec::new();
                for _ in 0..frames.len() {
                    items.push(cursor_dec.next_frame().expect("AC2 cursor frame"));
                }
                final_eof = cursor_dec.next_frame().expect("AC2 cursor CleanEof");
                repeated_eof = cursor_dec.next_frame().expect("AC2 cursor repeat CleanEof");
                final_offset = cursor_dec.offset();
                final_term_err = cursor_dec.terminal_error().cloned();
                decoded_items = items;
            } else {
                chunked_dec = FrameStreamDecoder::with_known_len(
                    ChunkedInterruptedReader::new(full_stream.as_slice(), 1),
                    base_offset,
                    end_offset,
                );
                let mut items = Vec::new();
                for _ in 0..frames.len() {
                    items.push(chunked_dec.next_frame().expect("AC2 chunked frame"));
                }
                final_eof = chunked_dec.next_frame().expect("AC2 chunked CleanEof");
                repeated_eof = chunked_dec
                    .next_frame()
                    .expect("AC2 chunked repeat CleanEof");
                final_offset = chunked_dec.offset();
                final_term_err = chunked_dec.terminal_error().cloned();
                decoded_items = items;
            }

            let mut expected_off = base_offset;
            for (idx, item) in decoded_items.iter().enumerate() {
                match item {
                    FrameStreamItem::Frame {
                        offset,
                        total_len,
                        frame,
                    } => {
                        assert_eq!(*offset, expected_off, "AC2 ({mode}) frame {idx} offset");
                        assert_eq!(
                            *total_len,
                            frames[idx].len(),
                            "AC2 ({mode}) frame {idx} total_len"
                        );
                        let (single_decoded, single_len) =
                            decode_frame(frames[idx]).expect("single-frame parity");
                        assert_eq!(single_len, *total_len);
                        assert_eq!(frame, &single_decoded, "AC2 ({mode}) frame {idx} lossless");
                        expected_off += *total_len as u64;
                    }
                    FrameStreamItem::CleanEof { .. } => {
                        panic!("AC2 ({mode}) unexpected early CleanEof at frame {idx}");
                    }
                }
            }
            // Spot-check postcard payloads decoded from stream items.
            if let FrameStreamItem::Frame { frame, .. } = &decoded_items[1] {
                assert_eq!(
                    decode_postcard_payload::<SampleCaptureEnvelope>(frame, 1).expect("env1"),
                    env1
                );
            }
            if let FrameStreamItem::Frame { frame, .. } = &decoded_items[2] {
                assert_eq!(frame.flags, FRAME_FLAG_PRIORITY);
                assert_eq!(
                    decode_postcard_payload::<SampleCaptureEnvelope>(frame, 1).expect("env2"),
                    env2
                );
            }
            if let FrameStreamItem::Frame { frame, .. } = &decoded_items[3] {
                assert_eq!(
                    decode_postcard_payload::<SampleCaptureEnvelope>(frame, 1).expect("env3"),
                    env3
                );
            }
            assert_eq!(
                final_eof,
                FrameStreamItem::CleanEof {
                    offset: expected_off
                },
                "AC2 ({mode}): exact EOF must return CleanEof at stream end"
            );
            assert_eq!(
                repeated_eof,
                FrameStreamItem::CleanEof {
                    offset: expected_off
                },
                "AC2 ({mode}): repeated EOF must remain CleanEof"
            );
            assert_eq!(final_offset, expected_off);
            assert_eq!(final_term_err, None);
        }

        // AC3: Valid HEADER_CRC32C with remaining bytes < FRAME_BODY_LEN -> TruncatedTail.
        // (3a) Single frame cut at 0 body bytes, 1 body byte, mid-key, mid-payload, and total_len - 1.
        let body_cuts = [
            FRAME_HEADER_LEN,           // 0 body bytes remaining
            FRAME_HEADER_LEN + 1,       // only FLAGS byte
            FRAME_HEADER_LEN + 3,       // FLAGS + DEDUP_KEY_LEN, 0 key bytes
            FRAME_HEADER_LEN + 8,       // mid dedup_key
            f1.len() / 2,               // mid payload
            f1.len().saturating_sub(1), // 1 byte short of complete frame
        ];
        for cut in body_cuts {
            let truncated = &f1[..cut];
            let expected_err = FrameDecodeError::TruncatedTail {
                available: cut,
                needed: f1.len(),
            };
            assert_eq!(
                decode_frame(truncated),
                Err(expected_err.clone()),
                "AC3a cut={cut}: decode_frame must classify short body as TruncatedTail"
            );

            let mut dec = FrameStreamDecoder::new(
                ChunkedInterruptedReader::new(truncated, 5),
                truncated.len() as u64,
            );
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "AC3a cut={cut}: FrameStreamDecoder must classify short body as TruncatedTail"
            );
            assert_eq!(dec.offset(), 0, "AC3a cut={cut}: offset must stay 0");
            assert_eq!(dec.terminal_error(), Some(&expected_err));
            assert_eq!(
                dec.body_allocations(),
                0,
                "AC3a cut={cut}: zero body heap allocation permitted before confirming remaining >= body_len"
            );
            assert_eq!(dec.max_body_capacity(), 0);
            assert_eq!(
                dec.get_ref().pos,
                FRAME_HEADER_LEN,
                "AC3a cut={cut}: decoder must stop strictly after the 16-byte header"
            );
            // Sticky error poison check: retrying MUST keep returning TruncatedTail, NEVER CleanEof!
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "AC3a cut={cut}: retry on TruncatedTail must stay poisoned, never CleanEof"
            );
            assert_eq!(dec.offset(), 0);
        }

        // (3b) Multi-frame prefix followed by valid header + truncated body (both 0 and partial body).
        for tail_cut in [FRAME_HEADER_LEN, FRAME_HEADER_LEN + 9, f2.len() - 1] {
            let mut stream = f0.clone();
            stream.extend_from_slice(&f1);
            let valid_prefix_end = stream.len() as u64;
            stream.extend_from_slice(&f2[..tail_cut]);

            let mut dec =
                FrameStreamDecoder::new(io::Cursor::new(stream.as_slice()), stream.len() as u64);
            assert!(matches!(
                dec.next_frame(),
                Ok(FrameStreamItem::Frame { offset: 0, total_len, .. }) if total_len == f0.len()
            ));
            assert!(matches!(
                dec.next_frame(),
                Ok(FrameStreamItem::Frame { offset, total_len, .. })
                    if offset == f0.len() as u64 && total_len == f1.len()
            ));
            assert_eq!(dec.offset(), valid_prefix_end);
            assert_eq!(dec.body_allocations(), 2);

            let expected_err = FrameDecodeError::TruncatedTail {
                available: tail_cut,
                needed: f2.len(),
            };
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "AC3b tail_cut={tail_cut}: mid-stream short body must return TruncatedTail"
            );
            assert_eq!(
                dec.body_allocations(),
                2,
                "AC3b tail_cut={tail_cut}: truncated third frame must perform zero body allocation"
            );
            assert_eq!(
                dec.offset(),
                valid_prefix_end,
                "AC3b tail_cut={tail_cut}: offset must stay at last complete frame boundary"
            );
            assert_eq!(
                dec.next_frame(),
                Err(expected_err),
                "AC3b tail_cut={tail_cut}: retry must remain TruncatedTail, never CleanEof"
            );
            assert_eq!(dec.offset(), valid_prefix_end);
        }

        // (3c) Valid 16-byte header declaring MAX_FRAME_BODY_LEN (~4 MiB) across ALL
        // constructors (`new`, `from_seekable`, `with_known_len`) with 0 bytes, 10 bytes,
        // and `MAX_FRAME_BODY_LEN - 1` bytes remaining -> strictly `TruncatedTail` with
        // ZERO body allocations and ZERO body bytes read (`ADR-RB-05` #2)!
        {
            let mut max_hdr = [0u8; FRAME_HEADER_LEN];
            max_hdr[0..4].copy_from_slice(&FRAME_MAGIC);
            max_hdr[4..8].copy_from_slice(&MAX_FRAME_BODY_LEN.to_le_bytes());
            let hdr_crc = crc32c_checksum(&max_hdr[0..8]);
            max_hdr[8..12].copy_from_slice(&hdr_crc.to_le_bytes());
            // FRAME_CRC32C is not checked until full body arrives; keep non-zero sentinel.
            max_hdr[12..16].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            let expected_needed = FRAME_HEADER_LEN + MAX_FRAME_BODY_LEN as usize;

            // 0 body bytes via `new` -> zero body allocation!
            let mut dec0 =
                FrameStreamDecoder::new(io::Cursor::new(&max_hdr[..]), max_hdr.len() as u64);
            let err0 = FrameDecodeError::TruncatedTail {
                available: FRAME_HEADER_LEN,
                needed: expected_needed,
            };
            assert_eq!(dec0.next_frame(), Err(err0.clone()));
            assert_eq!(dec0.body_allocations(), 0);
            assert_eq!(dec0.max_body_capacity(), 0);
            assert_eq!(dec0.next_frame(), Err(err0));

            // 10 body bytes via `new` -> zero body allocation!
            let mut hdr_plus_10 = max_hdr.to_vec();
            hdr_plus_10.extend_from_slice(&[0xaa; 10]);
            let mut dec10 = FrameStreamDecoder::new(
                io::Cursor::new(hdr_plus_10.as_slice()),
                hdr_plus_10.len() as u64,
            );
            let err10 = FrameDecodeError::TruncatedTail {
                available: FRAME_HEADER_LEN + 10,
                needed: expected_needed,
            };
            assert_eq!(dec10.next_frame(), Err(err10.clone()));
            assert_eq!(dec10.body_allocations(), 0);
            assert_eq!(dec10.max_body_capacity(), 0);
            assert_eq!(dec10.get_ref().position(), FRAME_HEADER_LEN as u64);
            assert_eq!(dec10.next_frame(), Err(err10.clone()));
            assert_eq!(
                err10.clone().into_storage_error().stable_code_str(),
                "LBR-IO-001"
            );

            // Maximum-length (~4 MiB) frame missing ONLY its final 1 byte tested across
            // `new`, `from_seekable`, and `with_known_len` -> ZERO body allocation!
            let max_minus_1_len = expected_needed - 1;
            let mut fake_max_minus_1 = vec![0x5au8; max_minus_1_len];
            fake_max_minus_1[..FRAME_HEADER_LEN].copy_from_slice(&max_hdr);
            let err_minus_1 = FrameDecodeError::TruncatedTail {
                available: max_minus_1_len,
                needed: expected_needed,
            };

            let mut dec_new = FrameStreamDecoder::new(
                io::Cursor::new(fake_max_minus_1.as_slice()),
                max_minus_1_len as u64,
            );
            assert_eq!(dec_new.next_frame(), Err(err_minus_1.clone()));
            assert_eq!(dec_new.body_allocations(), 0);
            assert_eq!(dec_new.max_body_capacity(), 0);
            assert_eq!(dec_new.get_ref().position(), FRAME_HEADER_LEN as u64);
            assert_eq!(dec_new.next_frame(), Err(err_minus_1.clone()));

            let mut dec_seek =
                FrameStreamDecoder::from_seekable(io::Cursor::new(fake_max_minus_1.as_slice()))
                    .expect("from_seekable must succeed");
            assert_eq!(dec_seek.end_offset(), max_minus_1_len as u64);
            assert_eq!(
                dec_seek.next_frame(),
                Err(err_minus_1.clone()),
                "from_seekable: 4 MiB frame missing final 1 byte must return TruncatedTail"
            );
            assert_eq!(
                dec_seek.body_allocations(),
                0,
                "from_seekable: zero body buffer allocation allowed when remaining < FRAME_BODY_LEN"
            );
            assert_eq!(dec_seek.max_body_capacity(), 0);
            assert_eq!(
                dec_seek.get_ref().position(),
                FRAME_HEADER_LEN as u64,
                "from_seekable: decoder must stop right after the 16-byte header without reading body"
            );
            assert_eq!(dec_seek.offset(), 0);
            assert_eq!(dec_seek.next_frame(), Err(err_minus_1.clone()));

            let mut dec_known = FrameStreamDecoder::with_known_len(
                ChunkedInterruptedReader::new(fake_max_minus_1.as_slice(), 4),
                0,
                max_minus_1_len as u64,
            );
            assert_eq!(dec_known.next_frame(), Err(err_minus_1.clone()));
            assert_eq!(dec_known.body_allocations(), 0);
            assert_eq!(dec_known.max_body_capacity(), 0);
            assert_eq!(dec_known.get_ref().pos, FRAME_HEADER_LEN);
            assert_eq!(dec_known.next_frame(), Err(err_minus_1));

            // Concurrent-shrink safety net: even if the underlying reader yields fewer bytes
            // than `end_offset` claimed (e.g. concurrent truncation), `read_body_bounded`
            // caps initial allocation at <= 8 KiB and fails closed as `TruncatedTail`.
            let mut dec_shrunk_body = FrameStreamDecoder::with_known_len(
                io::Cursor::new(hdr_plus_10.as_slice()),
                0,
                expected_needed as u64,
            );
            assert_eq!(dec_shrunk_body.next_frame(), Err(err10.clone()));
            assert!(
                dec_shrunk_body.max_body_capacity() <= 8 * 1024,
                "concurrent-shrink safety net must cap body alloc at <= 8 KiB, got {}",
                dec_shrunk_body.max_body_capacity()
            );
            assert_eq!(dec_shrunk_body.next_frame(), Err(err10));

            // Codex R2 P1 #2: Prematurely exhausted underlying reader before reaching
            // `end_offset` MUST fail closed as `TruncatedTail` (NEVER `CleanEof`!) and
            // stay sticky-poisoned on retry (both at offset 0 and after a valid frame).
            let mut dec_empty_short =
                FrameStreamDecoder::with_known_len(io::Cursor::new(&[][..]), 0, 100);
            let err_empty_short = FrameDecodeError::TruncatedTail {
                available: 0,
                needed: 100,
            };
            assert_eq!(
                dec_empty_short.next_frame(),
                Err(err_empty_short.clone()),
                "empty reader with non-zero end_offset must never report CleanEof"
            );
            assert_eq!(dec_empty_short.offset(), 0);
            assert_eq!(dec_empty_short.body_allocations(), 0);
            assert_eq!(
                dec_empty_short.next_frame(),
                Err(err_empty_short),
                "retry on prematurely exhausted stream must remain TruncatedTail"
            );

            let expected_two_len = (f0.len() + f1.len()) as u64;
            let mut dec_mid_short = FrameStreamDecoder::with_known_len(
                io::Cursor::new(f0.as_slice()),
                0,
                expected_two_len,
            );
            assert!(matches!(
                dec_mid_short.next_frame(),
                Ok(FrameStreamItem::Frame { offset: 0, total_len, .. }) if total_len == f0.len()
            ));
            let err_mid_short = FrameDecodeError::TruncatedTail {
                available: 0,
                needed: f1.len(),
            };
            assert_eq!(
                dec_mid_short.next_frame(),
                Err(err_mid_short.clone()),
                "stream prematurely exhausted after f0 must fail closed as TruncatedTail, never CleanEof"
            );
            assert_eq!(dec_mid_short.offset(), f0.len() as u64);
            assert_eq!(dec_mid_short.next_frame(), Err(err_mid_short));

            // Invalid bounds (`offset > end_offset` and seekable cursor past EOF) fail closed.
            let mut dec_bad_bounds =
                FrameStreamDecoder::with_known_len(io::Cursor::new(&[][..]), 50, 10);
            assert_eq!(
                dec_bad_bounds.next_frame(),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "stream offset exceeds known length",
                })
            );
            assert_eq!(
                dec_bad_bounds.next_frame(),
                Err(FrameDecodeError::CorruptHeader {
                    detail: "stream offset exceeds known length",
                })
            );

            let mut past_eof_cursor = io::Cursor::new(f0.as_slice());
            past_eof_cursor.set_position((f0.len() + 10) as u64);
            assert_eq!(
                FrameStreamDecoder::from_seekable(past_eof_cursor).err(),
                Some(FrameDecodeError::Io {
                    kind: io::ErrorKind::UnexpectedEof,
                })
            );

            // Also verify `from_seekable` on empty and full multi-frame streams, and
            // bounded prefix scans (`with_known_len` stopping cleanly at `committed_len`
            // without reading uncommitted bytes past `committed_len`).
            let mut dec_seek_empty = FrameStreamDecoder::from_seekable(io::Cursor::new(&[][..]))
                .expect("from_seekable empty");
            assert_eq!(
                dec_seek_empty.next_frame(),
                Ok(FrameStreamItem::CleanEof { offset: 0 })
            );

            let mut dec_seek_full =
                FrameStreamDecoder::from_seekable(io::Cursor::new(full_stream.as_slice()))
                    .expect("from_seekable full stream");
            for _ in 0..frames.len() {
                assert!(matches!(
                    dec_seek_full.next_frame(),
                    Ok(FrameStreamItem::Frame { .. })
                ));
            }
            assert_eq!(
                dec_seek_full.next_frame(),
                Ok(FrameStreamItem::CleanEof {
                    offset: full_stream.len() as u64
                })
            );

            // Bounded committed_len prefix scan: full_stream has f0+f1+f2+f3, limit to f0+f1.
            let committed_boundary = (f0.len() + f1.len()) as u64;
            let mut dec_bounded = FrameStreamDecoder::with_known_len(
                io::Cursor::new(full_stream.as_slice()),
                0,
                committed_boundary,
            );
            assert!(matches!(
                dec_bounded.next_frame(),
                Ok(FrameStreamItem::Frame { offset: 0, total_len, .. }) if total_len == f0.len()
            ));
            assert!(matches!(
                dec_bounded.next_frame(),
                Ok(FrameStreamItem::Frame { offset, total_len, .. })
                    if offset == f0.len() as u64 && total_len == f1.len()
            ));
            assert_eq!(
                dec_bounded.next_frame(),
                Ok(FrameStreamItem::CleanEof {
                    offset: committed_boundary
                })
            );
            assert_eq!(
                dec_bounded.get_ref().position(),
                committed_boundary,
                "bounded decoder must never read uncommitted bytes beyond end_offset"
            );

            // Bounded limit cutting into f2 (both 5-byte partial header and body-minus-1):
            let cut_hdr = committed_boundary + 5;
            let mut dec_cut_hdr = FrameStreamDecoder::with_known_len(
                io::Cursor::new(full_stream.as_slice()),
                0,
                cut_hdr,
            );
            assert!(dec_cut_hdr.next_frame().is_ok());
            assert!(dec_cut_hdr.next_frame().is_ok());
            assert_eq!(
                dec_cut_hdr.next_frame(),
                Err(FrameDecodeError::TruncatedTail {
                    available: 5,
                    needed: FRAME_HEADER_LEN,
                })
            );
            assert_eq!(dec_cut_hdr.get_ref().position(), cut_hdr);

            let cut_body = committed_boundary + (f2.len() as u64) - 1;
            let mut dec_cut_body = FrameStreamDecoder::with_known_len(
                io::Cursor::new(full_stream.as_slice()),
                0,
                cut_body,
            );
            assert!(dec_cut_body.next_frame().is_ok());
            assert!(dec_cut_body.next_frame().is_ok());
            assert_eq!(dec_cut_body.body_allocations(), 2);
            assert_eq!(
                dec_cut_body.next_frame(),
                Err(FrameDecodeError::TruncatedTail {
                    available: f2.len() - 1,
                    needed: f2.len(),
                })
            );
            assert_eq!(
                dec_cut_body.body_allocations(),
                2,
                "bounded cut inside f2 body must perform zero additional body allocation"
            );
            assert_eq!(
                dec_cut_body.get_ref().position(),
                committed_boundary + FRAME_HEADER_LEN as u64
            );
        }

        // (3d) Sticky I/O error poisoning (both during header read and mid-body read).
        for (label, prefix_slice) in [
            ("mid-header I/O error", &f1[..5]),
            ("mid-body I/O error", &f1[..FRAME_HEADER_LEN + 4]),
        ] {
            let mut dec =
                FrameStreamDecoder::new(FaultInjectingReader::new(prefix_slice), f1.len() as u64);
            let expected_io = FrameDecodeError::Io {
                kind: io::ErrorKind::BrokenPipe,
            };
            assert_eq!(dec.next_frame(), Err(expected_io.clone()), "{label}");
            assert_eq!(dec.offset(), 0, "{label}: offset must stay 0");
            assert_eq!(
                dec.next_frame(),
                Err(expected_io),
                "{label}: retry must remain Io error, never CleanEof"
            );
            assert_eq!(
                dec.get_ref().reads_after_error,
                0,
                "{label}: poisoned decoder must never read underlying reader again"
            );
        }

        // AC4: Exhaustively verify EVERY trailing incomplete header length `1..=15` bytes:
        // must ALWAYS be classified strictly as `TruncatedTail { available: n, needed: 16 }`,
        // NEVER `CleanEof` and NEVER `CorruptHeader` (even when bytes are non-magic garbage!).
        let garbage_tail: [u8; 15] = [
            0xff, 0x00, 0xde, 0xad, 0xbe, 0xef, 0x13, 0x37, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44,
            0x33,
        ];
        for n in 1..FRAME_HEADER_LEN {
            for (kind, sample) in [
                ("valid-LCE1-prefix", &f1[..n]),
                ("non-magic-tail", &garbage_tail[..n]),
            ] {
                let expected_err = FrameDecodeError::TruncatedTail {
                    available: n,
                    needed: FRAME_HEADER_LEN,
                };

                // (4a) At stream start (offset 0)
                assert_eq!(
                    decode_frame(sample),
                    Err(expected_err.clone()),
                    "AC4a ({kind}, n={n}): decode_frame must return TruncatedTail"
                );
                let mut dec_start = FrameStreamDecoder::new(
                    ChunkedInterruptedReader::new(sample, 1),
                    sample.len() as u64,
                );
                let res_start = dec_start.next_frame();
                assert_eq!(
                    res_start,
                    Err(expected_err.clone()),
                    "AC4a ({kind}, n={n}): stream start must return TruncatedTail, not CleanEof or CorruptHeader"
                );
                assert!(!matches!(
                    res_start,
                    Err(FrameDecodeError::CorruptHeader { .. })
                ));
                assert_eq!(dec_start.offset(), 0);
                assert_eq!(dec_start.body_allocations(), 0);
                assert_eq!(
                    dec_start.next_frame(),
                    Err(expected_err.clone()),
                    "AC4a ({kind}, n={n}): retry must remain TruncatedTail, never CleanEof"
                );

                // (4b) After valid multi-frame prefix (f0 + f1 + sample[..n])
                let mut stream = f0.clone();
                stream.extend_from_slice(&f1);
                let valid_prefix_end = stream.len() as u64;
                stream.extend_from_slice(sample);

                let mut dec_mid = FrameStreamDecoder::new(
                    ChunkedInterruptedReader::new(stream.as_slice(), 3),
                    stream.len() as u64,
                );
                assert!(matches!(
                    dec_mid.next_frame(),
                    Ok(FrameStreamItem::Frame { offset: 0, total_len, .. }) if total_len == f0.len()
                ));
                assert!(matches!(
                    dec_mid.next_frame(),
                    Ok(FrameStreamItem::Frame { offset, total_len, .. })
                        if offset == f0.len() as u64 && total_len == f1.len()
                ));
                assert_eq!(dec_mid.offset(), valid_prefix_end);
                assert_eq!(
                    dec_mid.next_frame(),
                    Err(expected_err.clone()),
                    "AC4b ({kind}, n={n}): trailing partial header after valid frames must return TruncatedTail"
                );
                assert_eq!(
                    dec_mid.offset(),
                    valid_prefix_end,
                    "AC4b ({kind}, n={n}): offset must remain at last valid frame end"
                );
                assert_eq!(
                    dec_mid.next_frame(),
                    Err(expected_err),
                    "AC4b ({kind}, n={n}): retry after partial header must remain TruncatedTail"
                );
                assert_eq!(dec_mid.offset(), valid_prefix_end);
            }
        }
    }

    /// `RB-07d` AC1–AC5:
    /// Exercises full-frame validation *after* header verification succeeds across
    /// every single byte of all 5 non-header / body / `FRAME_CRC32C` fields:
    /// - AC1: Any single-byte flip in `FRAME_CRC32C` (`12..16`) rejected with
    ///   `FrameDecodeError::CorruptPayload`.
    /// - AC2: Any single-byte flip in `FLAGS` (`16`) rejected by `FRAME_CRC32C`
    ///   with `FrameDecodeError::CorruptPayload`.
    /// - AC3: Any single-byte flip in `DEDUP_KEY_LEN` (`17..19`) rejected by
    ///   `FRAME_CRC32C` with `FrameDecodeError::CorruptPayload`.
    /// - AC4: Any single-byte flip in `DEDUP_KEY_UTF8` (`19..19+K`) rejected by
    ///   `FRAME_CRC32C` with `FrameDecodeError::CorruptPayload`.
    /// - AC5: Any single-byte flip in `POSTCARD_ENVELOPE` (`19+K..total_len`)
    ///   rejected by `FRAME_CRC32C` with `FrameDecodeError::CorruptPayload`.
    ///
    /// Verified through `parse_and_verify_header` (still succeeds), `decode_frame`,
    /// and `FrameStreamDecoder` (both standalone and mid-stream with trailing frames
    /// + sticky error poisoning + sanitized `LBR-IO-001` conversion).
    #[test]
    fn acknowledged_frame_each_body_and_crc_field_bitflip_rejected() {
        let secret_dedup_key = "rb07d:secret-provider-session-id-xyz:中文去重键";
        let env = SampleCaptureEnvelope {
            session_id: "secret-session-rb07d".to_string(),
            seq: 99,
            event_kind: "SessionEnd".to_string(),
            body: b"secret-payload-content-for-rb07d-verification".to_vec(),
        };
        let frame = encode_postcard_frame(FRAME_FLAG_PRIORITY, secret_dedup_key, 1, &env)
            .expect("encode acknowledged frame");
        let prefix_frame =
            encode_frame(FRAME_FLAG_NONE, "rb07d:prefix", b"prefix-body").expect("encode prefix");
        let suffix_frame =
            encode_frame(FRAME_FLAG_NONE, "rb07d:suffix", b"suffix-body").expect("encode suffix");

        let dedup_len = secret_dedup_key.len();
        let flags_offset = FRAME_HEADER_LEN; // 16
        let dedup_len_start = flags_offset + 1; // 17
        let dedup_len_end = dedup_len_start + 2; // 19
        let dedup_utf8_start = dedup_len_end; // 19
        let dedup_utf8_end = dedup_utf8_start + dedup_len;
        let payload_start = dedup_utf8_end;
        let payload_end = frame.len();
        assert!(
            payload_end > payload_start,
            "envelope payload must be non-empty"
        );

        let expected_err = FrameDecodeError::CorruptPayload {
            detail: "frame crc32c mismatch",
        };

        let assert_single_byte_flip_rejected = |byte_idx: usize, mask: u8, field_label: &str| {
            let mut mutated = frame.clone();
            mutated[byte_idx] ^= mask;

            // Header verification (`MAGIC` + `FRAME_BODY_LEN` + `HEADER_CRC32C`) MUST still pass,
            // proving rejection happens specifically in the post-header full-frame CRC path.
            let mut hdr = [0u8; FRAME_HEADER_LEN];
            hdr.copy_from_slice(&mutated[..FRAME_HEADER_LEN]);
            assert!(
                parse_and_verify_header(&hdr).is_ok(),
                "{field_label} byte {byte_idx}: header CRC must remain valid on non-header flip"
            );

            // Single-frame slice decoder rejects with `CorruptPayload`.
            assert_eq!(
                decode_frame(&mutated),
                Err(expected_err.clone()),
                "{field_label} byte {byte_idx} (mask {mask:#04x}): decode_frame must return CorruptPayload"
            );

            // Standalone sequential decoder (`from_seekable`) rejects with `CorruptPayload`
            // and remains sticky-poisoned on retry (never `CleanEof`, never `CorruptHeader`).
            let mut dec = FrameStreamDecoder::from_seekable(io::Cursor::new(mutated.as_slice()))
                .expect("from_seekable");
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "{field_label} byte {byte_idx}: FrameStreamDecoder must return CorruptPayload"
            );
            assert_eq!(
                dec.offset(),
                0,
                "{field_label} byte {byte_idx}: offset must not advance on CorruptPayload"
            );
            assert_eq!(dec.terminal_error(), Some(&expected_err));
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "{field_label} byte {byte_idx}: retry must remain CorruptPayload, never CleanEof"
            );
            assert_eq!(dec.offset(), 0);
        };

        // AC1: Every single byte of `FRAME_CRC32C` (`12..16`) flipped (multiple bitmasks).
        for idx in 12..FRAME_HEADER_LEN {
            for mask in [0x01u8, 0x80u8] {
                assert_single_byte_flip_rejected(idx, mask, "AC1 FRAME_CRC32C");
            }
        }

        // AC2: `FLAGS` (`16`) flipped (both flipping priority bit and setting unused bits).
        for mask in [0x01u8, 0x02u8, 0x80u8, 0xffu8] {
            assert_single_byte_flip_rejected(flags_offset, mask, "AC2 FLAGS");
        }

        // AC3: Every single byte of `DEDUP_KEY_LEN` (`17..19`) flipped (both low and high bytes).
        for idx in dedup_len_start..dedup_len_end {
            for mask in [0x01u8, 0x80u8] {
                assert_single_byte_flip_rejected(idx, mask, "AC3 DEDUP_KEY_LEN");
            }
        }

        // AC4: Every single byte of `DEDUP_KEY_UTF8` (`19..19+K`) flipped.
        for idx in dedup_utf8_start..dedup_utf8_end {
            assert_single_byte_flip_rejected(idx, 0x01, "AC4 DEDUP_KEY_UTF8");
        }

        // AC5: Every single byte of `POSTCARD_ENVELOPE` (`19+K..total_len`) flipped.
        for idx in payload_start..payload_end {
            assert_single_byte_flip_rejected(idx, 0x01, "AC5 POSTCARD_ENVELOPE");
        }

        // Mid-stream verification across all 5 fields with a valid prefix frame AND a valid
        // trailing frame: prefix frame decodes cleanly, corrupted middle frame fails with
        // `CorruptPayload`, stream offset freezes at `prefix_frame.len()`, and retry never
        // skips ahead to the trailing frame or returns `CleanEof`.
        let field_representatives = [
            ("AC1 FRAME_CRC32C", 12usize),
            ("AC2 FLAGS", flags_offset),
            ("AC3 DEDUP_KEY_LEN", dedup_len_start),
            ("AC4 DEDUP_KEY_UTF8", dedup_utf8_start + dedup_len / 2),
            (
                "AC5 POSTCARD_ENVELOPE",
                payload_start + (payload_end - payload_start) / 2,
            ),
        ];
        for (label, rel_idx) in field_representatives {
            let mut mutated_mid = frame.clone();
            mutated_mid[rel_idx] ^= 0x40;

            let mut stream = prefix_frame.clone();
            let prefix_end = stream.len() as u64;
            stream.extend_from_slice(&mutated_mid);
            stream.extend_from_slice(&suffix_frame);

            let mut dec =
                FrameStreamDecoder::new(io::Cursor::new(stream.as_slice()), stream.len() as u64);
            assert!(matches!(
                dec.next_frame(),
                Ok(FrameStreamItem::Frame { offset: 0, total_len, ref frame })
                    if total_len == prefix_frame.len() && frame.dedup_key == "rb07d:prefix"
            ));
            assert_eq!(dec.offset(), prefix_end);
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "{label}: mid-stream frame 2 bitflip must return CorruptPayload"
            );
            assert_eq!(
                dec.offset(),
                prefix_end,
                "{label}: offset must stay at frame 1 end"
            );
            // Retry MUST stay poisoned as `CorruptPayload` rather than advancing into `suffix_frame`.
            assert_eq!(
                dec.next_frame(),
                Err(expected_err.clone()),
                "{label}: poisoned mid-stream decoder must not skip to trailing frame"
            );
            assert_eq!(dec.offset(), prefix_end);
        }

        // Large multi-chunk (>16 KiB) payload boundary flips (first, middle, final byte of payload)
        // prove incremental chunked body reads feed the full body into `FRAME_CRC32C`.
        {
            let big_env = SampleCaptureEnvelope {
                session_id: "sess-rb07d-large".to_string(),
                seq: 100,
                event_kind: "ToolOutput".to_string(),
                body: vec![0x3cu8; 24 * 1024],
            };
            let big_frame = encode_postcard_frame(FRAME_FLAG_NONE, "rb07d:large-key", 1, &big_env)
                .expect("encode large frame");
            let big_payload_start =
                FRAME_HEADER_LEN + FRAME_BODY_FIXED_OVERHEAD + "rb07d:large-key".len();
            for flip_idx in [
                big_payload_start,
                (big_payload_start + big_frame.len()) / 2,
                big_frame.len() - 1,
            ] {
                let mut mutated = big_frame.clone();
                mutated[flip_idx] ^= 0x01;
                let mut dec =
                    FrameStreamDecoder::from_seekable(io::Cursor::new(mutated.as_slice()))
                        .expect("from_seekable large");
                assert_eq!(dec.next_frame(), Err(expected_err.clone()));
                assert_eq!(dec.next_frame(), Err(expected_err.clone()));
            }
        }

        // Sanitized `into_storage_error()` check: stable code `LBR-IO-001`, zero key/payload leakage.
        let mut flipped_for_err = frame.clone();
        flipped_for_err[payload_start] ^= 0x01;
        let storage_err = decode_frame(&flipped_for_err)
            .expect_err("payload flip must fail")
            .into_storage_error();
        assert_eq!(storage_err.stable_code_str(), "LBR-IO-001");
        let msg = storage_err.to_string();
        assert!(
            !msg.contains("secret-provider-session-id-xyz")
                && !msg.contains("secret-session-rb07d")
                && !msg.contains("secret-payload-content"),
            "CorruptPayload storage error must not leak raw dedup_key or payload: {msg}"
        );
    }
}
