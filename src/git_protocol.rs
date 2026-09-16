//! Packet-line helpers and service type parsing for the Git smart protocol, covering both
//! `upload-pack` (fetch/clone) and `receive-pack` (push) flows.
//!
//! The Git smart protocol frames every payload as a sequence of `pkt-line` records: a
//! 4-byte ASCII hex length header followed by `length - 4` bytes of payload. A length of
//! `0000` is a flush marker. This module exposes the minimum primitives required to read
//! and write these frames and to identify which side of the protocol a request targets.

use core::fmt;
use std::str::FromStr;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use git_internal::errors::GitError;

/// Identifies the direction of a smart-protocol exchange.
///
/// Used by HTTP routers and SSH dispatchers to pick the correct backend handler.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ServiceType {
    /// Server-to-client transfer: clone, fetch, ls-remote.
    UploadPack,
    /// Client-to-server transfer: push.
    ReceivePack,
}

impl fmt::Display for ServiceType {
    /// Render the variant as the on-the-wire service name expected by Git clients
    /// (e.g. the `service=` query parameter in `info/refs`).
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ServiceType::UploadPack => write!(f, "git-upload-pack"),
            ServiceType::ReceivePack => write!(f, "git-receive-pack"),
        }
    }
}

impl FromStr for ServiceType {
    type Err = GitError;

    /// Parse a wire-format service name back into a `ServiceType`.
    ///
    /// Boundary conditions:
    /// - Comparison is case-sensitive — `"git-upload-pack"` and `"git-receive-pack"` are
    ///   the only accepted strings.
    /// - Any other input returns `GitError::InvalidArgument` with the offending value
    ///   embedded for debugging.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "git-upload-pack" => Ok(ServiceType::UploadPack),
            "git-receive-pack" => Ok(ServiceType::ReceivePack),
            _ => Err(GitError::InvalidArgument(format!(
                "Invalid service name: {}",
                s
            ))),
        }
    }
}

/// Flush packet (`0000`). Marks the end of a logical group of pkt-lines.
pub const PKT_LINE_END_MARKER: &[u8; 4] = b"0000";

/// String-boundary marker for typed protocol errors carried by external error types.
pub(crate) const PKT_LINE_PROTOCOL_ERROR_PREFIX: &str = "pkt-line protocol error: ";

/// A declared pkt-line length that cannot represent a supported frame.
///
/// Reasons are fixed and never include bytes received from a remote peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PktFrameError {
    /// A non-flush length does not include the complete four-byte header.
    LengthBelowHeader,
    /// A length cannot be encoded in the four hexadecimal header digits.
    LengthAboveMaximum,
}

impl fmt::Display for PktFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::LengthBelowHeader => "frame length is smaller than the four-byte header",
            Self::LengthAboveMaximum => "frame length exceeds the four-digit header limit",
        };
        write!(f, "{PKT_LINE_PROTOCOL_ERROR_PREFIX}{reason}")
    }
}

impl std::error::Error for PktFrameError {}

/// Validate a declared pkt-line length and return the number of payload bytes.
///
/// The length includes the four-byte header. Zero is a flush marker, and four is
/// an empty data frame; both return zero payload bytes. Callers must retain the
/// declared length when they need to distinguish those two cases.
///
/// This helper neither allocates nor consumes input. It does not check whether a
/// payload is actually present; readers must separately reject truncated frames.
///
/// # Errors
///
/// Returns [`PktFrameError::LengthBelowHeader`] for lengths one through three,
/// or [`PktFrameError::LengthAboveMaximum`] for lengths greater than `0xffff`.
pub fn pkt_frame_payload_len(declared_len: u32) -> Result<usize, PktFrameError> {
    match declared_len {
        0 => Ok(0),
        1..=3 => Err(PktFrameError::LengthBelowHeader),
        4..=0xffff => Ok((declared_len - 4) as usize),
        _ => Err(PktFrameError::LengthAboveMaximum),
    }
}

/// A malformed or incomplete pkt-line frame.
///
/// Error messages contain fixed reasons, never remote header or payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PktLineError {
    /// A required header ends before all four bytes arrive.
    TruncatedHeader,
    /// The four-byte header is not valid UTF-8.
    InvalidHeaderEncoding,
    /// The header contains a character other than an ASCII hexadecimal digit.
    InvalidHexHeader,
    /// The declared length cannot represent a supported pkt-line frame.
    InvalidFrameLength(PktFrameError),
    /// The buffer ends before the complete declared payload.
    TruncatedPayload,
}

impl fmt::Display for PktLineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::TruncatedHeader => "incomplete four-byte header",
            Self::InvalidHeaderEncoding => "header is not valid UTF-8",
            Self::InvalidHexHeader => "header must contain four ASCII hexadecimal digits",
            Self::InvalidFrameLength(error) => return error.fmt(f),
            Self::TruncatedPayload => "payload is shorter than the declared frame length",
        };
        write!(f, "{PKT_LINE_PROTOCOL_ERROR_PREFIX}{reason}")
    }
}

impl std::error::Error for PktLineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidFrameLength(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PktFrameError> for PktLineError {
    fn from(error: PktFrameError) -> Self {
        Self::InvalidFrameLength(error)
    }
}

/// Consume a single pkt-line frame from the front of `bytes` and return its
/// `(declared_length, payload)`.
///
/// Functional scope:
/// - Reads the 4-byte ASCII hex header, decodes it as the total frame length, then
///   splits off `length - 4` bytes of payload.
/// - Mutates the input buffer in place: after a successful call, `bytes` advances past
///   the consumed frame.
///
/// Boundary conditions:
/// - Returns `Ok((0, Bytes::new()))` when `bytes` is empty so callers can use a
///   zero-length response as a stop condition.
/// - Returns `Ok((0, Bytes::new()))` when the decoded length is zero (the flush marker
///   `0000`); the leading 4 header bytes are still consumed.
/// - A `0004` header produces an empty payload with declared length four.
///
/// # Errors
///
/// Returns [`PktLineError`] for incomplete headers or payloads, non-ASCII-hex
/// headers, or invalid frame lengths. On error, the input buffer is unchanged.
/// Callers that require a response must reject empty input at their own boundary.
pub fn read_pkt_line(bytes: &mut Bytes) -> Result<(usize, Bytes), PktLineError> {
    if bytes.is_empty() {
        return Ok((0, Bytes::new()));
    }
    let header = bytes.get(..4).ok_or(PktLineError::TruncatedHeader)?;
    let header_str =
        core::str::from_utf8(header).map_err(|_| PktLineError::InvalidHeaderEncoding)?;
    if !header.iter().all(u8::is_ascii_hexdigit) {
        return Err(PktLineError::InvalidHexHeader);
    }
    let declared_len =
        u32::from_str_radix(header_str, 16).map_err(|_| PktLineError::InvalidHexHeader)?;
    let payload_len = pkt_frame_payload_len(declared_len)?;
    if bytes.len() - 4 < payload_len {
        return Err(PktLineError::TruncatedPayload);
    }
    // Validate the entire frame before consuming either header or payload.
    bytes.advance(4);
    Ok((declared_len as usize, bytes.copy_to_bytes(payload_len)))
}

/// Preserve ordinary transport errors and classify an incomplete frame read.
///
/// `truncated_frame` identifies the header or payload being read. The returned
/// InvalidData error retains a downcastable PktLineError with a fixed protocol
/// reason; other IO errors keep their original kind, reason and source.
pub(crate) fn pkt_line_read_error(
    error: std::io::Error,
    truncated_frame: PktLineError,
) -> std::io::Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        std::io::Error::new(std::io::ErrorKind::InvalidData, truncated_frame)
    } else {
        error
    }
}

/// Append a UTF-8 string as a pkt-line to `pkt_line_stream`.
///
/// Functional scope:
/// - Writes the 4-byte ASCII hex length header (`buf_str.len() + 4`, including the
///   header itself) followed by the raw bytes of `buf_str`.
/// - Does **not** add a trailing newline; callers that need newline-terminated lines
///   (the common case for capability advertisements) must include the `\n` in
///   `buf_str`.
///
/// Boundary conditions:
/// - Maximum frame size is `0xffff` bytes (65,535) per the Git protocol spec; this
///   helper does not enforce that limit and will silently produce malformed frames if
///   given an oversized string. Callers must chunk longer payloads themselves.
pub fn add_pkt_line_string(pkt_line_stream: &mut BytesMut, buf_str: String) {
    let buf_str_length = buf_str.len() + 4;
    pkt_line_stream.put(Bytes::from(format!("{:04x}", buf_str_length)));
    pkt_line_stream.put(buf_str.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::{
        Bytes, PKT_LINE_PROTOCOL_ERROR_PREFIX, PktFrameError, PktLineError, pkt_frame_payload_len,
        read_pkt_line,
    };
    use crate::utils::error::{CliError, StableErrorCode};

    fn assert_rejected(input: &[u8], expected: PktLineError) {
        let mut bytes = Bytes::copy_from_slice(input);
        let original = bytes.clone();
        assert_eq!(read_pkt_line(&mut bytes), Err(expected));
        assert_eq!(bytes, original, "failed parsing must not consume input");
        assert!(
            expected
                .to_string()
                .starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX)
        );
    }

    #[test]
    fn read_pkt_line_rejects_short_header() {
        for input in [b"0".as_slice(), b"00", b"000"] {
            assert_rejected(input, PktLineError::TruncatedHeader);
        }
    }

    #[test]
    fn read_pkt_line_rejects_non_utf8_header() {
        assert_rejected(b"\xff000", PktLineError::InvalidHeaderEncoding);
    }

    #[test]
    fn read_pkt_line_rejects_non_hex_header() {
        for input in [b"zzzz", b"+004", b"-004", b" 004", b"0x04", b"000\n"] {
            assert_rejected(input, PktLineError::InvalidHexHeader);
        }
    }

    #[test]
    fn read_pkt_line_rejects_len_below_four() {
        for input in [b"0001", b"0002", b"0003"] {
            assert_rejected(input, PktFrameError::LengthBelowHeader.into());
        }
    }

    #[test]
    fn read_pkt_line_rejects_truncated_frame() {
        for input in [b"0005".as_slice(), b"0008abc", b"ffffremote-sentinel"] {
            assert_rejected(input, PktLineError::TruncatedPayload);
        }
    }

    #[test]
    fn read_pkt_line_preserves_empty_buffer() {
        let mut bytes = Bytes::new();
        assert_eq!(read_pkt_line(&mut bytes), Ok((0, Bytes::new())));
        assert!(bytes.is_empty());
    }

    #[test]
    fn read_pkt_line_preserves_flush() {
        let mut bytes = Bytes::from_static(b"00000005x");
        assert_eq!(read_pkt_line(&mut bytes), Ok((0, Bytes::new())));
        assert_eq!(bytes, b"0005x".as_slice());
    }

    #[test]
    fn read_pkt_line_preserves_len_4_empty_payload() {
        let mut bytes = Bytes::from_static(b"00040000");
        assert_eq!(read_pkt_line(&mut bytes), Ok((4, Bytes::new())));
        assert_eq!(bytes, b"0000".as_slice());
    }

    #[test]
    fn read_pkt_line_preserves_payload_and_following_frames() {
        let mut bytes = Bytes::from_static(b"000Ahello\n0005x0000");
        assert_eq!(
            read_pkt_line(&mut bytes),
            Ok((10, Bytes::from_static(b"hello\n")))
        );
        assert_eq!(read_pkt_line(&mut bytes), Ok((5, Bytes::from_static(b"x"))));
        assert_eq!(read_pkt_line(&mut bytes), Ok((0, Bytes::new())));
        assert!(bytes.is_empty());
        let payload = vec![0xff; 0xffff - 4];
        for header in [b"ffff", b"FFFF"] {
            let mut frame = header.to_vec();
            frame.extend_from_slice(&payload);
            frame.extend_from_slice(b"0000");
            let mut bytes = Bytes::from(frame);
            assert_eq!(
                read_pkt_line(&mut bytes),
                Ok((0xffff, Bytes::copy_from_slice(&payload)))
            );
            assert_eq!(bytes, b"0000".as_slice());
        }
    }

    #[test]
    fn pkt_line_error_text_matches_protocol_classifier() {
        for (error, reason) in [
            (PktLineError::TruncatedHeader, "incomplete four-byte header"),
            (
                PktLineError::InvalidHeaderEncoding,
                "header is not valid UTF-8",
            ),
            (
                PktLineError::InvalidHexHeader,
                "header must contain four ASCII hexadecimal digits",
            ),
            (
                PktFrameError::LengthBelowHeader.into(),
                "frame length is smaller than the four-byte header",
            ),
            (
                PktFrameError::LengthAboveMaximum.into(),
                "frame length exceeds the four-digit header limit",
            ),
            (
                PktLineError::TruncatedPayload,
                "payload is shorter than the declared frame length",
            ),
        ] {
            let message = error.to_string();
            assert_eq!(message, format!("{PKT_LINE_PROTOCOL_ERROR_PREFIX}{reason}"));
            assert_eq!(
                CliError::fatal(message).stable_code(),
                StableErrorCode::NetworkProtocol
            );
        }
    }

    #[test]
    fn pkt_frame_payload_len_ok_flush_zero() {
        assert_eq!(pkt_frame_payload_len(0), Ok(0));
    }

    #[test]
    fn pkt_frame_payload_len_rejects_len_below_four() {
        for declared_len in 1..=3 {
            assert_eq!(
                pkt_frame_payload_len(declared_len),
                Err(PktFrameError::LengthBelowHeader)
            );
        }
        let message = PktFrameError::LengthBelowHeader.to_string();
        assert_eq!(
            message,
            "pkt-line protocol error: frame length is smaller than the four-byte header"
        );
        assert!(message.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
    }

    #[test]
    fn pkt_frame_payload_len_ok_len_4_empty() {
        assert_eq!(pkt_frame_payload_len(4), Ok(0));
    }

    #[test]
    fn pkt_frame_payload_len_ok_normal_and_upper_bound() {
        for declared_len in 5..=0xffff {
            assert_eq!(
                pkt_frame_payload_len(declared_len),
                Ok((declared_len - 4) as usize)
            );
        }
        assert_eq!(pkt_frame_payload_len(0xffff), Ok(65_531));
    }

    #[test]
    fn pkt_frame_payload_len_rejects_over_0xffff() {
        for declared_len in [0x10000, 0x10001, 0x100000, u32::MAX - 1, u32::MAX] {
            assert_eq!(
                pkt_frame_payload_len(declared_len),
                Err(PktFrameError::LengthAboveMaximum)
            );
        }
        let message = PktFrameError::LengthAboveMaximum.to_string();
        assert_eq!(
            message,
            "pkt-line protocol error: frame length exceeds the four-digit header limit"
        );
        assert!(message.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
    }
}
