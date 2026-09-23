//! Protocol abstraction for Git transport with shared advertisement parsing and traits implemented by HTTPS, local, and LFS clients.

use std::cell::RefCell;

use bytes::{Bytes, BytesMut};
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
};
use url::Url;

use crate::{
    git_protocol::{
        PKT_LINE_PROTOCOL_ERROR_PREFIX, ServiceType, add_pkt_line_string, read_pkt_line,
    },
    internal::branch::Branch,
};

pub mod bundle_client;
pub mod git_client; // to support git server protocol (git://) over TCP
pub mod https_client;
pub mod lfs_client;
pub mod local_client;
pub mod mega2_auth; // plan-20260912 MB-04: mega2 write-token resolution (ADR-MB-03)
pub mod mega2_entry; // plan-20260912 MB-04: bounded POST /api/v1/create-entry directory client
pub mod mega2_mutate; // plan-20260912 MB-07: bounded delete-entry / move-entry client
pub mod mega2_tag; // plan-20260912 MB-10: tag_router list/create/get/delete client
pub mod mega2_tree; // plan-20260912: bounded mega2 /api/v1/tree listing client
pub mod ssh_client; // to support SSH transport (ssh:// and git@host:path)

pub trait ProtocolClient {
    /// create client from url
    fn from_url(url: &Url) -> Self;
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredReference {
    pub(crate) _hash: String,
    pub(crate) _ref: String,
}

impl DiscoveredReference {
    pub fn hash(&self) -> &str {
        &self._hash
    }

    pub fn name(&self) -> &str {
        &self._ref
    }
}

pub type DiscRef = DiscoveredReference;

pub type FetchStream = futures_util::stream::BoxStream<'static, Result<Bytes, std::io::Error>>;

thread_local! {
    static WIRE_HASH_KIND: RefCell<HashKind> = RefCell::new(HashKind::default());
}

pub fn set_wire_hash_kind(kind: HashKind) {
    WIRE_HASH_KIND.with(|k| {
        *k.borrow_mut() = kind;
    });
}

pub fn get_wire_hash_kind() -> HashKind {
    WIRE_HASH_KIND.with(|k| *k.borrow())
}

/// Result of reference discovery containing refs, capabilities, and hash kind.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub refs: Vec<DiscRef>,
    pub capabilities: Vec<String>,
    pub hash_kind: HashKind,
}

/// Parse discovered references from Git protocol advertisement response.
pub fn parse_discovered_references(
    mut response_content: Bytes,
    service: ServiceType,
) -> Result<DiscoveryResult, GitError> {
    if response_content.is_empty() {
        return Err(GitError::NetworkError(format!(
            "{PKT_LINE_PROTOCOL_ERROR_PREFIX}empty discovery response"
        )));
    }
    let mut ref_list = Vec::new(); // refs
    let mut capabilities = Vec::new(); // capabilities
    let mut saw_header = false; // header seen or not
    let mut processed_first_ref = false;
    let mut hash_kind = HashKind::Sha1;
    // Closure to parse hash kind based on length
    let parse_hash_kind = |hash: &str| match hash.len() {
        40 => Ok(HashKind::Sha1),
        64 => Ok(HashKind::Sha256),
        _ => Err(GitError::NetworkError(format!(
            "Invalid hash length {}, expected 40 or 64",
            hash.len()
        ))),
    };

    loop {
        let (bytes_take, pkt_line) = read_pkt_line(&mut response_content)
            .map_err(|error| GitError::NetworkError(error.to_string()))?;
        if bytes_take == 0 {
            if response_content.is_empty() {
                break;
            } else {
                continue;
            }
        }

        if !saw_header && pkt_line.starts_with(b"# service=") {
            let header = String::from_utf8(pkt_line.to_vec()).map_err(|e| {
                GitError::NetworkError(format!("Invalid UTF-8 in response header: {}", e))
            })?;
            tracing::debug!("discovery header: {header:?}");
            saw_header = true;
            continue;
        }
        saw_header = true;

        let pkt_line = String::from_utf8(pkt_line.to_vec())
            .map_err(|e| GitError::NetworkError(format!("Invalid UTF-8 in response: {}", e)))?;
        let (hash, rest) = pkt_line.split_once(' ').ok_or_else(|| {
            GitError::NetworkError("Invalid reference format, missing object id".to_string())
        })?;
        let detected_kind = parse_hash_kind(hash)?;
        if !processed_first_ref {
            hash_kind = detected_kind;
        } else if detected_kind != hash_kind {
            return Err(GitError::NetworkError(format!(
                "Hash kind mismatch: expected {hash_kind}, got length {}",
                hash.len()
            )));
        }

        let rest = rest.trim();

        if !processed_first_ref {
            let (reference, caps) = match rest.split_once('\0') {
                Some((r, c)) => (r, c),
                None => (rest, ""),
            };
            if !caps.is_empty() {
                capabilities = caps
                    .split(' ')
                    .filter(|cap| !cap.is_empty())
                    .map(|cap| cap.to_string())
                    .collect();
                if let Some(format_cap) = capabilities
                    .iter()
                    .find(|cap| cap.starts_with("object-format="))
                {
                    let format_kind = match format_cap.as_str() {
                        "object-format=sha1" => HashKind::Sha1,
                        "object-format=sha256" => HashKind::Sha256,
                        "object-format=blake3" => HashKind::Blake3,
                        _ => {
                            return Err(GitError::NetworkError(
                                "Unsupported object format capability".to_string(),
                            ));
                        }
                    };
                    if format_kind != detected_kind {
                        return Err(GitError::NetworkError(format!(
                            "Object format mismatch: advertised {format_kind}, got hash length {}",
                            hash.len()
                        )));
                    }
                    hash_kind = format_kind;
                }
            }

            if hash == ObjectHash::zero_str(hash_kind) {
                tracing::debug!(
                    "discovery for {:?} returned zero hash, treating as empty repository",
                    service
                );
                // Empty refs end semantic discovery, not validation of the response framing.
                while !response_content.is_empty() {
                    read_pkt_line(&mut response_content)
                        .map_err(|error| GitError::NetworkError(error.to_string()))?;
                }
                break;
            }

            if reference != "capabilities^{}" {
                ref_list.push(DiscoveredReference {
                    _hash: hash.to_string(),
                    _ref: reference.to_string(),
                });
            }
            if !caps.is_empty() {
                let caps = caps.split(' ').collect::<Vec<&str>>();
                tracing::debug!("capability declarations: {:?}", caps);
            }
            processed_first_ref = true;
        } else {
            ref_list.push(DiscoveredReference {
                _hash: hash.to_string(),
                _ref: rest.to_string(),
            });
        }
    }

    Ok(DiscoveryResult {
        refs: ref_list,
        capabilities,
        hash_kind,
    })
}

pub fn generate_upload_pack_content(
    have: &[String],
    want: &[String],
    shallow: &[String],
    depth: Option<usize>,
) -> Bytes {
    let mut buf = BytesMut::new();
    let mut write_first_line = false;

    // `include-tag` asks the server to also send annotated tag objects that
    // point at objects in the returned pack — this powers Git's default tag
    // auto-follow on `fetch`. Servers that don't support it ignore it.
    // `ofs-delta` lets the server delta-compress objects against earlier objects
    // in the SAME pack by offset (smaller transfers). git-internal's pack decoder
    // resolves OffsetDelta objects, so it is safe to advertise. `thin-pack` is
    // deliberately NOT advertised: a thin pack deltas against objects OUTSIDE the
    // pack, which the self-contained decoder cannot complete. `report-status` is a
    // push (receive-pack) capability and has no place on an upload-pack want line.
    let mut requested_caps = vec![
        "side-band-64k",
        "multi_ack_detailed",
        "ofs-delta",
        "include-tag",
    ];
    if get_wire_hash_kind() == HashKind::Sha256 {
        requested_caps.push("object-format=sha256");
    }
    let requested_caps = requested_caps.join(" ");
    for w in want {
        if !write_first_line {
            add_pkt_line_string(
                &mut buf,
                format!(
                    "want {w} {requested_caps} agent=libra/{}\n",
                    env!("CARGO_PKG_VERSION")
                )
                .to_string(),
            );
            write_first_line = true;
        } else {
            add_pkt_line_string(&mut buf, format!("want {w}\n").to_string());
        }
    }

    for oid in shallow {
        add_pkt_line_string(&mut buf, format!("shallow {oid}\n"));
    }

    // Add deepen line if depth is specified
    if let Some(d) = depth {
        add_pkt_line_string(&mut buf, format!("deepen {d}\n").to_string());
    }

    buf.extend(b"0000");
    for h in have {
        add_pkt_line_string(&mut buf, format!("have {h}\n").to_string());
    }

    add_pkt_line_string(&mut buf, "done\n".to_string());

    buf.freeze()
}

impl From<Branch> for DiscoveredReference {
    fn from(branch: Branch) -> Self {
        let _ref = if branch.name.starts_with("refs/") {
            branch.name.clone()
        } else {
            match branch.remote {
                Some(remote) => format!("refs/remotes/{}/{}", remote, branch.name),
                None => format!("refs/heads/{}", branch.name),
            }
        };
        DiscoveredReference {
            _hash: branch.commit.to_string(),
            _ref,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::git_protocol::PktLineError;

    fn discovery_error(input: &[u8]) -> String {
        match parse_discovered_references(Bytes::copy_from_slice(input), ServiceType::UploadPack) {
            Err(GitError::NetworkError(detail)) => detail,
            other => panic!("expected a network error, received {other:?}"),
        }
    }

    fn advertisement(hash: &str, caps: &str) -> Bytes {
        let mut bytes = BytesMut::new();
        add_pkt_line_string(&mut bytes, "# service=git-upload-pack\n".to_string());
        bytes.extend_from_slice(b"0000");
        add_pkt_line_string(&mut bytes, format!("{hash} refs/heads/main\0{caps}\n"));
        bytes.extend_from_slice(b"0000");
        bytes.freeze()
    }

    #[test]
    fn pkt_line_discovery_propagates_pkt_line_error() {
        assert_eq!(
            discovery_error(b"0008abc"),
            PktLineError::TruncatedPayload.to_string()
        );
    }

    #[test]
    fn pkt_line_discovery_marker_prefix_contract() {
        let detail = discovery_error(b"0001");
        assert!(detail.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
        assert_eq!(detail.matches(PKT_LINE_PROTOCOL_ERROR_PREFIX).count(), 1);
        assert_eq!(
            detail,
            "pkt-line protocol error: frame length is smaller than the four-byte header"
        );
    }

    #[test]
    fn pkt_line_discovery_marker_detection_semantics_starts_with() {
        let detail = discovery_error(b"0001");
        assert!(detail.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
        for wrapped in [
            format!("network error: {detail}"),
            format!(" {detail}"),
            detail.to_uppercase(),
        ] {
            assert!(!wrapped.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
        }
    }

    #[test]
    fn pkt_line_discovery_capability_echo_fixed_phrase() {
        let bytes = advertisement(&"1".repeat(40), "object-format=unsupported");
        assert_eq!(
            discovery_error(&bytes),
            "Unsupported object format capability"
        );
    }

    #[test]
    fn pkt_line_discovery_capability_echo_sentinel() {
        let sentinel = "PRIVATE_CAPABILITY_SENTINEL\x1b[31m";
        let bytes = advertisement(&"1".repeat(40), &format!("object-format={sentinel}"));
        let detail = discovery_error(&bytes);
        assert_eq!(detail, "Unsupported object format capability");
        assert!(!detail.contains(sentinel));
        assert!(!detail.contains('\x1b'));
    }

    #[test]
    fn pkt_line_discovery_rejects_empty_response() {
        assert_eq!(
            discovery_error(b""),
            format!("{PKT_LINE_PROTOCOL_ERROR_PREFIX}empty discovery response")
        );
        for service in [ServiceType::UploadPack, ServiceType::ReceivePack] {
            let result = parse_discovered_references(Bytes::from_static(b"0000"), service)
                .expect("valid flush");
            assert!(result.refs.is_empty());
        }
    }

    #[test]
    fn pkt_line_discovery_rejects_malformed_no_panic() {
        for malformed in [
            b"0".as_slice(),
            b"00",
            b"000",
            b"\xff000",
            b"zzzz",
            b"+004",
            b"0001",
            b"0002",
            b"0003",
            b"0008abc",
        ] {
            // A panic fails this test directly; also exercise a later discovery frame.
            assert!(discovery_error(malformed).starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
            let mut response = BytesMut::new();
            add_pkt_line_string(&mut response, "# service=git-upload-pack\n".to_string());
            response.extend_from_slice(b"0000");
            response.extend_from_slice(malformed);
            assert!(discovery_error(&response).starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX));
        }
    }

    #[test]
    fn pkt_line_discovery_valid_response_regression() {
        for (kind, width, cap) in [
            (HashKind::Sha1, 40, "object-format=sha1"),
            (HashKind::Sha256, 64, "object-format=sha256"),
        ] {
            for service in [ServiceType::UploadPack, ServiceType::ReceivePack] {
                let hash = "1".repeat(width);
                let result = parse_discovered_references(advertisement(&hash, cap), service)
                    .expect("valid advertisement");
                assert_eq!(result.hash_kind, kind);
                assert_eq!(result.capabilities, vec![cap]);
                assert_eq!(
                    result.refs,
                    vec![DiscoveredReference {
                        _hash: hash,
                        _ref: "refs/heads/main".to_string()
                    }]
                );
                let empty =
                    parse_discovered_references(advertisement(&"0".repeat(width), cap), service)
                        .expect("valid empty repository");
                assert!(empty.refs.is_empty());
                assert_eq!(empty.hash_kind, kind);
            }
        }
    }

    fn empty_advertisement_with_tail(width: usize, cap: &str, tail: &[u8]) -> Bytes {
        let mut bytes = BytesMut::new();
        add_pkt_line_string(&mut bytes, "# service=git-upload-pack\n".to_string());
        bytes.extend_from_slice(b"0000");
        add_pkt_line_string(
            &mut bytes,
            format!("{} capabilities^{{}}\0{cap}\n", "0".repeat(width)),
        );
        bytes.extend_from_slice(tail);
        bytes.freeze()
    }

    #[test]
    fn pkt_line_empty_discovery_rejects_malformed_tail() {
        use crate::git_protocol::PktFrameError;

        let cases: &[(&[u8], PktLineError)] = &[
            (b"0", PktLineError::TruncatedHeader),
            (b"00", PktLineError::TruncatedHeader),
            (b"000", PktLineError::TruncatedHeader),
            (b"\xff000", PktLineError::InvalidHeaderEncoding),
            (b"zzzz", PktLineError::InvalidHexHeader),
            (b"+004", PktLineError::InvalidHexHeader),
            (b" 004", PktLineError::InvalidHexHeader),
            (
                b"0001",
                PktLineError::InvalidFrameLength(PktFrameError::LengthBelowHeader),
            ),
            (
                b"0002",
                PktLineError::InvalidFrameLength(PktFrameError::LengthBelowHeader),
            ),
            (
                b"0003",
                PktLineError::InvalidFrameLength(PktFrameError::LengthBelowHeader),
            ),
            (b"0008abc", PktLineError::TruncatedPayload),
            (
                b"ffffREMOTE_EMPTY_TAIL_SECRET",
                PktLineError::TruncatedPayload,
            ),
        ];
        for (width, cap) in [(40, "object-format=sha1"), (64, "object-format=sha256")] {
            for service in [ServiceType::UploadPack, ServiceType::ReceivePack] {
                for (tail, expected) in cases {
                    // Also reject a corrupt later frame after accepted empty frames.
                    for prefix in [b"".as_slice(), b"0004", b"00000004"] {
                        let suffix = [prefix, tail].concat();
                        let error = parse_discovered_references(
                            empty_advertisement_with_tail(width, cap, &suffix),
                            service,
                        )
                        .expect_err("zero object ID must not hide malformed framing");
                        let GitError::NetworkError(detail) = error else {
                            panic!("expected network error, got {error:?}");
                        };
                        assert_eq!(
                            detail,
                            expected.to_string(),
                            "{width}/{service:?}/{suffix:?}"
                        );
                        assert_eq!(detail.matches(PKT_LINE_PROTOCOL_ERROR_PREFIX).count(), 1);
                        assert!(!detail.contains("REMOTE_EMPTY_TAIL_SECRET"));
                    }
                }
            }
        }
    }

    #[test]
    fn pkt_line_empty_discovery_preserves_valid_tail() {
        let maximum = [b"ffff".as_slice(), &vec![b'x'; 0xffff - 4], b"0000"].concat();
        for (kind, width, cap) in [
            (HashKind::Sha1, 40, "object-format=sha1"),
            (HashKind::Sha256, 64, "object-format=sha256"),
        ] {
            for service in [ServiceType::UploadPack, ServiceType::ReceivePack] {
                // Missing flush and semantically unused payloads retain the existing
                // parser behavior; this fix validates framing only, not new grammar.
                for tail in [
                    b"".as_slice(),
                    b"0000",
                    b"0004",
                    b"0004000000040000",
                    maximum.as_slice(),
                ] {
                    let caps = format!("multi_ack {cap}");
                    let result = parse_discovered_references(
                        empty_advertisement_with_tail(width, &caps, tail),
                        service,
                    )
                    .expect("existing valid frame semantics stay compatible");
                    assert!(result.refs.is_empty());
                    assert_eq!(result.hash_kind, kind);
                    assert_eq!(result.capabilities, ["multi_ack", cap]);
                }
            }
        }
    }

    #[test]
    fn upload_pack_want_line_advertises_expected_capabilities() {
        let have: Vec<String> = Vec::new();
        let want = vec!["1".repeat(40)];
        let body = generate_upload_pack_content(&have, &want, &[], None);
        let text = String::from_utf8_lossy(&body);

        // The first `want` line carries the capability list + agent string.
        for cap in [
            "side-band-64k",
            "multi_ack_detailed",
            "ofs-delta",
            "include-tag",
        ] {
            assert!(text.contains(cap), "want line must advertise {cap}: {text}");
        }
        assert!(
            text.contains("agent=libra/"),
            "want line must send an agent string: {text}"
        );
        // Intentionally NOT advertised: a thin pack would delta against objects
        // outside the pack, which the self-contained decoder cannot complete.
        assert!(
            !text.contains("thin-pack"),
            "thin-pack must not be advertised: {text}"
        );
    }
}
