//! Protocol v2 `fetch` command: request construction, sectioned response
//! parsing, and adaptation to the v0-shaped stream the fetch pipeline consumes.
//!
//! A v2 fetch response is a sequence of sections — `acknowledgments`,
//! `shallow-info`, `wanted-refs`, `packfile` — separated by delim-pkt (`0001`)
//! and terminated by a flush-pkt (`0000`) or response-end-pkt (`0002`). The
//! `packfile` section always carries sideband-encoded (`%x01-03`) data.
//!
//! [`adapt_response_stream`] turns that framing into the v0 framing that
//! the [`crate::command::fetch`] stream reader already understands:
//! `shallow`/`unshallow` lines become pkt-lines and the packfile sideband frames
//! pass through unchanged. The downstream consumer keeps its single, tested
//! implementation of sideband demultiplexing, pack completion tracking and
//! shallow response validation.

use std::io;

use bytes::{BufMut, Bytes, BytesMut};
use futures_util::{StreamExt, stream};
use git_internal::hash::HashKind;

use super::{super::MissingShallowCapability, Advertisement};
use crate::{
    git_protocol::{PktFrame, add_pkt_line_string, take_pkt_frame},
    internal::protocol::FetchStream,
};

/// Server `fetch` capability feature required by `deepen` / `shallow` arguments.
const SHALLOW_FEATURE: &str = "shallow";

/// Arguments for one v2 `fetch` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchRequest {
    /// Object ids to fetch (`want <oid>`).
    pub wants: Vec<String>,
    /// Object ids already present locally (`have <oid>`).
    pub haves: Vec<String>,
    /// Existing local shallow boundaries (`shallow <oid>`).
    pub shallow: Vec<String>,
    /// Requested history depth (`deepen <n>`).
    pub depth: Option<usize>,
}

/// One parsed `wanted-refs` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedRef {
    /// Object id the server resolved the wanted ref to.
    pub oid: String,
    /// Ref name the client asked for.
    pub name: String,
}

/// A fully parsed v2 fetch response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchResponse {
    /// Lines of the `acknowledgments` section (`ACK ...`, `NAK`, `ready`).
    pub acknowledgments: Vec<String>,
    /// Shallow boundaries the server reports after the fetch.
    pub shallow: Vec<String>,
    /// Boundaries the server no longer considers shallow (`unshallow`).
    pub unshallow: Vec<String>,
    /// Resolved `wanted-refs` entries.
    pub wanted_refs: Vec<WantedRef>,
    /// Sideband payloads of the `packfile` section, in arrival order.
    pub packfile: Vec<Bytes>,
}

/// Build the pkt-line body of a v2 `fetch` request.
///
/// `depth` and `shallow` require the server's `fetch=shallow` feature; without
/// it the request fails typed ([`MissingShallowCapability`]) before any bytes
/// are sent, mirroring the v0 capability check.
pub fn build_request(
    advertisement: &Advertisement,
    request: &FetchRequest,
) -> Result<Bytes, io::Error> {
    let shallow_supported = advertisement.features("fetch").contains(&SHALLOW_FEATURE);
    if !shallow_supported && (request.depth.is_some() || !request.shallow.is_empty()) {
        return Err(io::Error::other(MissingShallowCapability));
    }
    let mut body = BytesMut::new();
    add_pkt_line_string(&mut body, "command=fetch\n".to_string());
    add_pkt_line_string(
        &mut body,
        format!("agent=libra/{}\n", env!("CARGO_PKG_VERSION")),
    );
    if advertisement.hash_kind != HashKind::Sha1 && advertisement.supports("object-format") {
        add_pkt_line_string(
            &mut body,
            format!(
                "object-format={}\n",
                crate::internal::object_format::as_str(advertisement.hash_kind)
            ),
        );
    }
    body.extend_from_slice(crate::git_protocol::PKT_LINE_DELIM_MARKER);
    // `ofs-delta` is safe for the self-contained pack decoder; `thin-pack` is
    // deliberately not requested because a thin pack deltas against objects
    // outside the pack.
    add_pkt_line_string(&mut body, "ofs-delta\n".to_string());
    add_pkt_line_string(&mut body, "include-tag\n".to_string());
    for want in &request.wants {
        add_pkt_line_string(&mut body, format!("want {want}\n"));
    }
    for boundary in &request.shallow {
        add_pkt_line_string(&mut body, format!("shallow {boundary}\n"));
    }
    if let Some(depth) = request.depth {
        add_pkt_line_string(&mut body, format!("deepen {depth}\n"));
    }
    for have in &request.haves {
        add_pkt_line_string(&mut body, format!("have {have}\n"));
    }
    add_pkt_line_string(&mut body, "done\n".to_string());
    body.extend_from_slice(crate::git_protocol::PKT_LINE_END_MARKER);
    Ok(body.freeze())
}

/// Sections a v2 fetch response may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Acknowledgments,
    ShallowInfo,
    WantedRefs,
    Packfile,
}

impl Section {
    fn from_header(line: &[u8]) -> Option<Self> {
        match line {
            b"acknowledgments" => Some(Self::Acknowledgments),
            b"shallow-info" => Some(Self::ShallowInfo),
            b"wanted-refs" => Some(Self::WantedRefs),
            b"packfile" => Some(Self::Packfile),
            _ => None,
        }
    }
}

/// Incremental v2 response section reader.
///
/// Frames are fed in arrival order; frames that the v0 consumer understands are
/// re-encoded into `emit` (the adapted stream), while structured fields record
/// the semantic content for tests and callers that parse a complete body.
#[derive(Default)]
struct SectionReader {
    section: Option<Section>,
    emit: Vec<Bytes>,
    response: FetchResponse,
}

impl SectionReader {
    fn on_frame(&mut self, frame: PktFrame) {
        match frame {
            PktFrame::Delim => {
                // Sections are separated by a delim-pkt; the next data frame is
                // a section header. Inside the packfile section everything is
                // sideband data, so a delimiter there is not a section change.
                if self.section != Some(Section::Packfile) {
                    self.section = None;
                }
            }
            PktFrame::Flush | PktFrame::ResponseEnd => {
                if self.section != Some(Section::Packfile) {
                    self.section = None;
                }
            }
            PktFrame::Data(payload) => self.on_data(payload),
        }
    }

    fn on_data(&mut self, payload: Bytes) {
        if payload.starts_with(b"ERR ") || payload.starts_with(b"FATAL ") {
            // Remote failures are reported wherever they appear; the v0 consumer
            // turns an `ERR ` frame into a typed remote error.
            self.emit.push(encode_frame(&payload));
            return;
        }
        let Some(section) = self.section else {
            if let Some(header) = Section::from_header(trim_newline(&payload)) {
                self.section = Some(header);
            }
            return;
        };
        match section {
            Section::Packfile => {
                self.response.packfile.push(payload.clone());
                self.emit.push(encode_frame(&payload));
            }
            Section::ShallowInfo => {
                let line = trim_newline(&payload);
                if let Some(oid) = line.strip_prefix(b"shallow ") {
                    let oid = String::from_utf8_lossy(oid).trim().to_string();
                    self.response.shallow.push(oid.clone());
                    self.emit
                        .push(encode_frame(format!("shallow {oid}\n").as_bytes()));
                } else if let Some(oid) = line.strip_prefix(b"unshallow ") {
                    let oid = String::from_utf8_lossy(oid).trim().to_string();
                    self.response.unshallow.push(oid.clone());
                    self.emit
                        .push(encode_frame(format!("unshallow {oid}\n").as_bytes()));
                }
            }
            Section::Acknowledgments => {
                self.response
                    .acknowledgments
                    .push(String::from_utf8_lossy(&payload).trim().to_string());
            }
            Section::WantedRefs => {
                let line = trim_newline(&payload);
                let text = String::from_utf8_lossy(line).to_string();
                if let Some((oid, name)) = text.split_once(' ') {
                    self.response.wanted_refs.push(WantedRef {
                        oid: oid.to_string(),
                        name: name.to_string(),
                    });
                }
            }
        }
    }
}

/// Encode `payload` as a pkt-line data frame without a trailing newline.
fn encode_frame(payload: &[u8]) -> Bytes {
    let mut buffer = BytesMut::with_capacity(payload.len() + 4);
    buffer.put_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    buffer.put_slice(payload);
    buffer.freeze()
}

/// Trim a trailing `\n` (and `\r\n`) from a raw pkt-line payload.
fn trim_newline(payload: &[u8]) -> &[u8] {
    let payload = payload.strip_suffix(b"\n").unwrap_or(payload);
    payload.strip_suffix(b"\r").unwrap_or(payload)
}

/// Parse a complete v2 fetch response body.
///
/// Used by unit tests and by callers that buffer the whole response; the
/// streaming path shares [`SectionReader`] with [`adapt_response_stream`].
pub fn parse_response(mut body: Bytes) -> Result<FetchResponse, io::Error> {
    let mut reader = SectionReader::default();
    while !body.is_empty() {
        let frame =
            take_pkt_frame(&mut body).map_err(|error| io::Error::other(error.to_string()))?;
        let Some(frame) = frame else {
            return Err(io::Error::other(
                "protocol v2 fetch response ended inside a pkt-line frame",
            ));
        };
        reader.on_frame(frame);
    }
    Ok(reader.response)
}

/// Adapt a v2 fetch response stream into v0-shaped pkt-line frames.
///
/// Emitted frames are exactly what the v0 consumer already parses: synthesized
/// `shallow`/`unshallow` pkt-lines for the `shallow-info` section and the
/// sideband frames of the `packfile` section passed through verbatim. Section
/// headers, delim-pkt and response-end-pkt frames are dropped.
pub fn adapt_response_stream(stream: FetchStream) -> FetchStream {
    stream::unfold(AdaptState::new(stream), |mut state| async move {
        loop {
            match state.next_output() {
                Ok(Some(bytes)) => return Some((Ok(bytes), state)),
                Ok(None) => {}
                Err(error) => return Some((Err(error), state)),
            }
            if state.finished {
                return None;
            }
            match state.inner.next().await {
                Some(Ok(chunk)) => state.push_chunk(chunk),
                Some(Err(error)) => {
                    state.finished = true;
                    return Some((Err(error), state));
                }
                None => state.finished = true,
            }
        }
    })
    .boxed()
}

struct AdaptState {
    inner: FetchStream,
    /// Bytes buffered from the transport that do not yet form a whole frame.
    /// Kept as `Bytes` so consuming one frame is an O(1) offset advance.
    buffer: Bytes,
    reader: SectionReader,
    pending: std::collections::VecDeque<Bytes>,
    finished: bool,
}

impl AdaptState {
    fn new(inner: FetchStream) -> Self {
        Self {
            inner,
            buffer: Bytes::new(),
            reader: SectionReader::default(),
            pending: std::collections::VecDeque::new(),
            finished: false,
        }
    }

    /// Append a transport chunk to the unparsed tail.
    fn push_chunk(&mut self, chunk: Bytes) {
        if self.buffer.is_empty() {
            self.buffer = chunk;
            return;
        }
        let mut merged = BytesMut::with_capacity(self.buffer.len() + chunk.len());
        merged.extend_from_slice(&self.buffer);
        merged.extend_from_slice(&chunk);
        self.buffer = merged.freeze();
    }

    /// Produce the next adapted frame, if one is available yet.
    fn next_output(&mut self) -> Result<Option<Bytes>, io::Error> {
        loop {
            if let Some(bytes) = self.pending.pop_front() {
                return Ok(Some(bytes));
            }
            let mut view = self.buffer.clone();
            let frame =
                take_pkt_frame(&mut view).map_err(|error| io::Error::other(error.to_string()))?;
            let Some(frame) = frame else {
                return Ok(None);
            };
            self.buffer = view;
            self.reader.on_frame(frame);
            for bytes in self.reader.emit.drain(..) {
                self.pending.push_back(bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};
    use futures_util::{StreamExt, stream};
    use git_internal::hash::HashKind;

    use super::*;
    use crate::git_protocol::{
        add_delim_pkt, add_flush_pkt, add_pkt_line_string, add_response_end_pkt,
    };

    fn advertisement(caps: &[&str], hash_kind: HashKind) -> Advertisement {
        Advertisement {
            capabilities: caps.iter().map(|cap| cap.to_string()).collect(),
            hash_kind,
        }
    }

    fn fetch_sha1() -> Advertisement {
        advertisement(
            &["fetch=shallow wait-for-done", "object-format=sha1"],
            HashKind::Sha1,
        )
    }

    #[test]
    fn request_has_command_capabilities_delim_arguments_and_flush() {
        let advertisement = fetch_sha1();
        let request = FetchRequest {
            wants: vec!["a".repeat(40)],
            haves: vec!["b".repeat(40)],
            shallow: vec![],
            depth: None,
        };
        let body = build_request(&advertisement, &request).expect("valid request");
        let text = String::from_utf8_lossy(&body).to_string();
        assert!(text.starts_with("0012command=fetch\n"), "text={text:?}");
        assert!(text.contains("ofs-delta\n"));
        assert!(text.contains("include-tag\n"));
        assert!(
            !text.contains("thin-pack"),
            "self-contained decoder: {text}"
        );
        let delimiter = text.find("0001").expect("delim-pkt");
        let argument = text.find("ofs-delta\n").expect("argument section");
        assert!(delimiter < argument, "capabilities precede the delimiter");
        assert!(text.contains(&format!("want {}", "a".repeat(40))));
        assert!(
            text.find("want ").unwrap() < text.find("have ").unwrap(),
            "wants precede haves"
        );
        assert!(!text.contains("deepen"));
        assert!(!text.contains("shallow "));
        assert!(
            text.ends_with("0000"),
            "request must end with a flush: {text:?}"
        );
    }

    #[test]
    fn request_encodes_depth_and_shallow_arguments() {
        let advertisement = fetch_sha1();
        let request = FetchRequest {
            wants: vec!["a".repeat(40)],
            haves: vec![],
            shallow: vec!["c".repeat(40)],
            depth: Some(3),
        };
        let text =
            String::from_utf8_lossy(&build_request(&advertisement, &request).unwrap()).to_string();
        assert!(text.contains(&format!("shallow {}", "c".repeat(40))));
        assert!(text.contains("deepen 3\n"));
        let deepen = text.find("deepen 3\n").unwrap();
        let done = text.find("done\n").unwrap();
        assert!(deepen < done, "deepen precedes done");
    }

    #[test]
    fn request_rejects_depth_without_server_shallow_support() {
        let advertisement = advertisement(&["fetch=wait-for-done"], HashKind::Sha1);
        let request = FetchRequest {
            wants: vec!["a".repeat(40)],
            haves: vec![],
            shallow: vec![],
            depth: Some(1),
        };
        let error = build_request(&advertisement, &request).expect_err("depth needs shallow");
        assert!(crate::internal::protocol::is_missing_shallow_capability(
            &error
        ));
        assert_eq!(error.to_string(), MissingShallowCapability.to_string());

        // Without depth or boundaries the same advertisement is fine.
        let request = FetchRequest {
            wants: vec!["a".repeat(40)],
            ..FetchRequest::default()
        };
        build_request(&advertisement, &request).expect("plain fetch needs no shallow support");
    }

    #[test]
    fn request_announces_sha256_only_when_advertised() {
        let sha256 = advertisement(&["fetch=shallow", "object-format=sha256"], HashKind::Sha256);
        let request = FetchRequest {
            wants: vec!["a".repeat(64)],
            ..FetchRequest::default()
        };
        let text = String::from_utf8_lossy(&build_request(&sha256, &request).unwrap()).to_string();
        assert!(text.contains("object-format=sha256"));

        let silent = advertisement(&["fetch=shallow"], HashKind::Sha256);
        let text = String::from_utf8_lossy(&build_request(&silent, &request).unwrap()).to_string();
        assert!(!text.contains("object-format="));
    }

    /// Append a raw pkt-line frame carrying `payload` verbatim.
    fn add_raw_frame(body: &mut BytesMut, payload: &[u8]) {
        body.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
        body.extend_from_slice(payload);
    }

    fn response(frames: impl FnOnce(&mut BytesMut)) -> Bytes {
        let mut body = BytesMut::new();
        frames(&mut body);
        body.freeze()
    }

    #[test]
    fn parses_shallow_info_and_packfile_sections() {
        let pack = b"\x01PACKdata";
        let body = response(|body| {
            add_pkt_line_string(body, "acknowledgments\n".to_string());
            add_pkt_line_string(body, "NAK\n".to_string());
            add_delim_pkt(body);
            add_pkt_line_string(body, "shallow-info\n".to_string());
            add_pkt_line_string(body, format!("shallow {}\n", "a".repeat(40)));
            add_pkt_line_string(body, format!("unshallow {}\n", "b".repeat(40)));
            add_delim_pkt(body);
            add_pkt_line_string(body, "packfile\n".to_string());
            add_raw_frame(body, pack);
            add_response_end_pkt(body);
        });
        let parsed = parse_response(body).expect("valid v2 response");
        assert_eq!(parsed.acknowledgments, vec!["NAK".to_string()]);
        assert_eq!(parsed.shallow, vec!["a".repeat(40)]);
        assert_eq!(parsed.unshallow, vec!["b".repeat(40)]);
        assert_eq!(parsed.packfile.len(), 1);
        assert!(parsed.packfile[0].starts_with(b"\x01PACK"));
    }

    #[test]
    fn parses_wanted_refs_section() {
        let body = response(|body| {
            add_pkt_line_string(body, "wanted-refs\n".to_string());
            add_pkt_line_string(body, format!("{} refs/heads/main\n", "1".repeat(40)));
            add_flush_pkt(body);
        });
        let parsed = parse_response(body).expect("valid v2 response");
        assert_eq!(
            parsed.wanted_refs,
            vec![WantedRef {
                oid: "1".repeat(40),
                name: "refs/heads/main".to_string()
            }]
        );
    }

    #[test]
    fn tolerates_missing_sections_and_flush_terminated_responses() {
        let body = response(|body| {
            add_pkt_line_string(body, "acknowledgments\n".to_string());
            add_pkt_line_string(body, "ACK 123\n".to_string());
            add_flush_pkt(body);
        });
        let parsed = parse_response(body).expect("flush terminates the response");
        assert_eq!(parsed.acknowledgments, vec!["ACK 123".to_string()]);
        assert!(parsed.packfile.is_empty());

        let empty = parse_response(Bytes::new()).expect("empty body has no sections");
        assert_eq!(empty, FetchResponse::default());
    }

    #[test]
    fn rejects_truncated_frames() {
        let mut body = BytesMut::new();
        body.extend_from_slice(b"0012acknowledgments");
        let error = parse_response(body.freeze()).expect_err("truncated frame must fail");
        assert!(error.to_string().contains("ended inside a pkt-line frame"));
    }

    async fn collect(stream: FetchStream) -> Vec<Bytes> {
        stream
            .map(|item| item.expect("adapted stream yields frames"))
            .collect::<Vec<_>>()
            .await
    }

    #[tokio::test]
    async fn adaptation_forwards_shallow_lines_and_packfile_sideband() {
        let body = response(|body| {
            add_pkt_line_string(body, "acknowledgments\n".to_string());
            add_pkt_line_string(body, "NAK\n".to_string());
            add_delim_pkt(body);
            add_pkt_line_string(body, "shallow-info\n".to_string());
            add_pkt_line_string(body, format!("shallow {}\n", "a".repeat(40)));
            add_delim_pkt(body);
            add_pkt_line_string(body, "packfile\n".to_string());
            add_raw_frame(body, b"\x01PACKpayload");
            add_response_end_pkt(body);
        });
        // Feed the body in one-byte chunks to prove the adapter frames across
        // transport chunk boundaries.
        let chunks = body
            .iter()
            .map(|byte| Ok::<Bytes, io::Error>(Bytes::copy_from_slice(&[*byte])))
            .collect::<Vec<_>>();
        let adapted = adapt_response_stream(stream::iter(chunks).boxed());
        let frames = collect(adapted).await;
        assert_eq!(frames.len(), 2, "acknowledgments and headers are dropped");
        let mut shallow_frame = frames[0].clone();
        assert_eq!(
            crate::git_protocol::read_pkt_frame(&mut shallow_frame).expect("valid pkt-line"),
            crate::git_protocol::PktFrame::Data(Bytes::from(format!(
                "shallow {}\n",
                "a".repeat(40)
            )))
        );
        assert!(frames[1].ends_with(b"\x01PACKpayload"));
        assert!(!frames.iter().any(|frame| frame.ends_with(b"packfile\n")));
        assert!(
            !frames
                .iter()
                .any(|frame| frame.ends_with(b"shallow-info\n"))
        );
    }

    #[tokio::test]
    async fn adaptation_forwards_remote_errors() {
        let body = response(|body| {
            add_pkt_line_string(body, "ERR access denied\n".to_string());
            add_response_end_pkt(body);
        });
        let adapted =
            adapt_response_stream(stream::iter(vec![Ok::<Bytes, io::Error>(body)]).boxed());
        let frames = collect(adapted).await;
        assert_eq!(frames.len(), 1);
        assert!(String::from_utf8_lossy(&frames[0]).contains("ERR access denied"));
    }

    #[tokio::test]
    async fn adaptation_propagates_transport_errors() {
        let failed = adapt_response_stream(
            stream::iter(vec![Err::<Bytes, io::Error>(io::Error::other("link down"))]).boxed(),
        );
        let mut failed = failed;
        let item = failed.next().await.expect("an error frame");
        assert!(
            item.expect_err("transport error is preserved")
                .to_string()
                .contains("link down")
        );
    }

    #[tokio::test]
    async fn adaptation_ends_at_eof_without_a_complete_pack() {
        let body = response(|body| {
            add_pkt_line_string(body, "packfile\n".to_string());
            add_raw_frame(body, b"\x01PACK");
        });
        let adapted =
            adapt_response_stream(stream::iter(vec![Ok::<Bytes, io::Error>(body)]).boxed());
        let frames = collect(adapted).await;
        assert_eq!(frames.len(), 1, "only the sideband frame is emitted");
    }
}
