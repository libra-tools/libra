//! Dual-lane CRC32C capture frame queue (`ADR-RB-05`).
//!
//! Houses the self-authenticating 16-byte header + dual Castagnoli CRC32C
//! frame wire codec ([`frame`]) and, in subsequent Phase 2 cards (`RB-07b`..
//! `RB-09l`), dual-lane directory security posture, `LTM2` dual-replica segment
//! logs, durable quarantine publishing, and single-writer priority-first
//! consumption.

pub mod frame;

pub use frame::{
    DecodedFrame, FRAME_BODY_FIXED_OVERHEAD, FRAME_FLAG_NONE, FRAME_FLAG_PRIORITY,
    FRAME_HEADER_LEN, FRAME_MAGIC, FrameDecodeError, FrameEncodeError, FrameStreamDecoder,
    FrameStreamItem, MAX_FRAME_BODY_LEN, MAX_FRAME_DEDUP_KEY_LEN, MAX_FRAME_PAYLOAD_LEN,
    MIN_FRAME_BODY_LEN, ParsedFrameHeader, crc32c_append, crc32c_checksum, decode_frame,
    decode_postcard_payload, encode_frame, encode_postcard_frame, parse_and_verify_header,
    validate_frame_body_len,
};
