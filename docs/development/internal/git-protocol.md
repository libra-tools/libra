# Buffered pkt-line parsing

`libra::git_protocol::read_pkt_line` now returns
`Result<(usize, bytes::Bytes), PktLineError>` instead of a tuple. Update callers
to propagate or map the error before destructuring:

```rust
use bytes::Bytes;
use libra::git_protocol::{read_pkt_line, PktLineError};

fn first_payload(input: &mut Bytes) -> Result<Bytes, PktLineError> {
    let (_, payload) = read_pkt_line(input)?;
    Ok(payload)
}
```

## Contract

- Empty input returns `Ok((0, Bytes::new()))` without consuming bytes. A caller
  requiring an advertisement must reject an absent response itself.
- `0000` consumes its four-byte header and returns length zero and an empty payload.
- `0004` consumes its header and returns length four and an empty payload.
- Other successful frames consume the complete frame and leave subsequent bytes
  unchanged. Hexadecimal digits can use either case; signs and whitespace are invalid.
- Malformed or incomplete input returns `PktLineError` and leaves the buffer
  unchanged. The error distinguishes short headers, invalid encoding, invalid
  hexadecimal digits, invalid lengths, and truncated payloads.
- `PktFrameError` converts through `From<PktFrameError>` into the frame-length
  variant. The pure `pkt_frame_payload_len` helper is the source of frame-length
  validation.

Errors implement `std::error::Error` and Display. Their fixed messages begin with
`pkt-line protocol error: ` and contain no received header or payload bytes.
Within Libra, adapters to external string error types preserve this leading
marker once and classify the raw detail using the crate-internal marker constant
before adding context. External Rust callers should match `PktLineError` variants
directly; the marker constant is not part of the public API.
The Rust API signature change requires downstream callers to handle `Result`.
This buffered parser contract does not certify streaming git:// or SSH readers;
those readers have separate validation and error propagation paths.
