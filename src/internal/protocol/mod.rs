//! Protocol abstraction for Git transport with shared advertisement parsing and traits implemented by HTTPS, local, and LFS clients.

use std::{cell::RefCell, collections::BTreeSet, io::Error as IoError};

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
    internal::{branch::Branch, object_format},
};

pub mod bundle_client;
pub mod git_client; // to support git server protocol (git://) over TCP
pub mod https_client;
pub mod lfs_client;
pub mod local_client;
pub mod local_push; // issues/480 HP-07: local path push target (Libra)
pub mod local_push_git; // issues/480 HP-08: local path push target (Git)
pub mod mega2_auth; // plan-20260912 MB-04: mega2 write-token resolution (ADR-MB-03)
pub mod mega2_diag; // plan-20261001 MN-01: machine-readable details for mega2 HTTP failures
pub mod mega2_entry; // plan-20260912 MB-04: bounded POST /api/v1/create-entry directory client
pub mod mega2_mutate; // plan-20260912 MB-07: bounded delete-entry / move-entry client
pub mod mega2_tag; // plan-20260912 MB-10: tag_router list/create/get/delete client
pub mod mega2_tree; // plan-20260912: bounded mega2 /api/v1/tree listing client
pub mod repository_arg; // issues/480 HP-06: shared fetch/pull/push repository-arg parsing
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

const MAX_ADVERTISED_SHALLOW_BOUNDARIES: usize = 4096;

/// Protocol negotiation failed before a request could be sent because the
/// remote cannot accept shallow boundaries or a depth request.
#[derive(Debug)]
pub(crate) struct MissingShallowCapability;

impl std::fmt::Display for MissingShallowCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "remote did not advertise the Git 'shallow' capability; cannot request --depth or send existing shallow boundaries; use a server with shallow support",
        )
    }
}

impl std::error::Error for MissingShallowCapability {}

/// The source changed its shallow boundary set after reference discovery.
#[derive(Debug)]
pub(crate) struct ChangedShallowAdvertisement;

impl std::fmt::Display for ChangedShallowAdvertisement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "remote shallow boundaries changed between discovery and fetch; retry the operation",
        )
    }
}

impl std::error::Error for ChangedShallowAdvertisement {}

/// Recognize typed protocol failures even when a transport preserves the
/// original cause behind another I/O or contextual error.
fn has_error_cause<T: std::error::Error + 'static>(error: &IoError) -> bool {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = cause {
        if current.is::<T>() {
            return true;
        }
        cause = current
            .downcast_ref::<IoError>()
            .and_then(IoError::get_ref)
            .map(|source| source as &(dyn std::error::Error + 'static))
            .or_else(|| current.source());
    }
    false
}

pub(crate) fn is_missing_shallow_capability(error: &IoError) -> bool {
    has_error_cause::<MissingShallowCapability>(error)
}

pub(crate) fn is_shallow_advertisement_changed(error: &IoError) -> bool {
    has_error_cause::<ChangedShallowAdvertisement>(error)
}

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
    /// Boundary commits advertised by a shallow upload-pack source.
    pub shallow_boundaries: Vec<String>,
    pub hash_kind: HashKind,
}

/// Resolve wire `HashKind` from discovery capabilities (capability-first).
///
/// Missing `object-format` defaults to SHA-1. Every `object-format=` entry is
/// read; duplicates or conflicting values fail closed.
fn wire_kind_from_capabilities(capabilities: &[String]) -> Result<HashKind, GitError> {
    let mut formats = Vec::new();
    for cap in capabilities {
        let Some(raw) = cap.strip_prefix("object-format=") else {
            continue;
        };
        let kind = object_format::parse_config_value(raw).map_err(|_| {
            GitError::NetworkError("Unsupported object format capability".to_string())
        })?;
        formats.push(kind);
    }
    match formats.as_slice() {
        [] => Ok(HashKind::Sha1),
        [kind] => Ok(*kind),
        [first, rest @ ..] => {
            if rest.iter().all(|kind| kind == first) {
                // Duplicate identical object-format entries still fail closed.
                return Err(GitError::NetworkError(
                    "Conflicting object-format capabilities".to_string(),
                ));
            }
            Err(GitError::NetworkError(
                "Conflicting object-format capabilities".to_string(),
            ))
        }
    }
}

fn validate_discovery_oid(hash_kind: HashKind, hash: &str) -> Result<String, GitError> {
    if hash.contains(':') {
        return Err(GitError::NetworkError(format!(
            "Tagged object id '{hash}' is not allowed on the pkt-line wire"
        )));
    }
    ObjectHash::from_hex_for_kind(hash_kind, hash)
        .map(|oid| oid.to_string())
        .map_err(|_| {
            GitError::NetworkError(format!(
                "Invalid object id '{hash}' for advertised object-format {}",
                object_format::as_str(hash_kind)
            ))
        })
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
    let mut shallow_boundaries = BTreeSet::new();
    let mut saw_header = false; // header seen or not
    let mut processed_first_ref = false;
    let mut hash_kind = HashKind::Sha1;

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
        if let Some(oid) = pkt_line.strip_prefix("shallow ") {
            let oid = oid.trim_end_matches('\n');
            if !processed_first_ref {
                return Err(GitError::NetworkError(
                    "Unexpected shallow boundary in reference advertisement".to_string(),
                ));
            }
            let canonical_oid = validate_discovery_oid(hash_kind, oid).map_err(|_| {
                GitError::NetworkError(format!(
                    "Invalid shallow boundary object id '{oid}' in reference advertisement"
                ))
            })?;
            shallow_boundaries.insert(canonical_oid);
            if shallow_boundaries.len() > MAX_ADVERTISED_SHALLOW_BOUNDARIES {
                return Err(GitError::NetworkError(format!(
                    "remote advertised more than {MAX_ADVERTISED_SHALLOW_BOUNDARIES} distinct shallow boundaries; deepen the source repository before retrying"
                )));
            }
            continue;
        }
        let (hash, rest) = pkt_line.split_once(' ').ok_or_else(|| {
            GitError::NetworkError("Invalid reference format, missing object id".to_string())
        })?;
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
            }
            // Capability-first: wire kind comes only from object-format (default sha1).
            hash_kind = wire_kind_from_capabilities(&capabilities)?;
            let canonical_hash = validate_discovery_oid(hash_kind, hash)?;

            if !caps.is_empty() {
                let caps = caps.split(' ').collect::<Vec<&str>>();
                tracing::debug!("capability declarations: {:?}", caps);
            }

            if canonical_hash == ObjectHash::zero_str(hash_kind) {
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
                    _hash: canonical_hash,
                    _ref: reference.to_string(),
                });
            }
            processed_first_ref = true;
        } else {
            let canonical_hash = validate_discovery_oid(hash_kind, hash)?;
            ref_list.push(DiscoveredReference {
                _hash: canonical_hash,
                _ref: rest.to_string(),
            });
        }
    }

    Ok(DiscoveryResult {
        refs: ref_list,
        capabilities,
        shallow_boundaries: shallow_boundaries.into_iter().collect(),
        hash_kind,
    })
}

pub fn generate_upload_pack_content(
    have: &[String],
    want: &[String],
    shallow: &[String],
    depth: Option<usize>,
) -> Bytes {
    generate_upload_pack_content_inner(have, want, shallow, depth, false)
}

/// Build a v0/v1 request using the server's advertised shallow capability.
pub fn generate_upload_pack_content_with_capabilities(
    have: &[String],
    want: &[String],
    shallow: &[String],
    depth: Option<usize>,
    advertised_capabilities: &[String],
) -> Result<Bytes, IoError> {
    let supports_shallow = advertised_capabilities.iter().any(|cap| cap == "shallow");
    if !supports_shallow && (depth.is_some() || !shallow.is_empty()) {
        return Err(IoError::other(MissingShallowCapability));
    }
    Ok(generate_upload_pack_content_inner(
        have,
        want,
        shallow,
        depth,
        supports_shallow,
    ))
}

pub(crate) fn verify_shallow_advertisement_unchanged(
    expected: &[String],
    actual: &[String],
) -> Result<(), IoError> {
    let expected: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    let actual: BTreeSet<&str> = actual.iter().map(String::as_str).collect();
    if expected == actual {
        Ok(())
    } else {
        Err(IoError::other(ChangedShallowAdvertisement))
    }
}

fn generate_upload_pack_content_inner(
    have: &[String],
    want: &[String],
    shallow: &[String],
    depth: Option<usize>,
    supports_shallow: bool,
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
    if supports_shallow {
        requested_caps.push("shallow");
    }
    match get_wire_hash_kind() {
        HashKind::Sha1 => {}
        HashKind::Sha256 => requested_caps.push("object-format=sha256"),
        HashKind::Blake3 => requested_caps.push("object-format=blake3"),
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
            (HashKind::Blake3, 64, "object-format=blake3"),
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

    #[test]
    fn parse_discovery_does_not_infer_sha256_from_64_hex() {
        // Capability-first: missing object-format ⇒ sha1; a 64-hex OID must not
        // be inferred as sha256 (or blake3) from width alone.
        let hash = "1".repeat(64);
        let err = discovery_error(&advertisement(&hash, "multi_ack"));
        assert!(
            err.contains("Invalid object id") && err.contains("sha1"),
            "expected sha1 width rejection, got {err}"
        );
        // Explicit sha256 capability still accepts 64-hex.
        let ok = parse_discovered_references(
            advertisement(&hash, "object-format=sha256"),
            ServiceType::UploadPack,
        )
        .expect("sha256 capability accepts 64-hex");
        assert_eq!(ok.hash_kind, HashKind::Sha256);
        // Blake3 capability also accepts 64-hex (not inferred from length).
        let blake3 = parse_discovered_references(
            advertisement(&hash, "object-format=blake3"),
            ServiceType::UploadPack,
        )
        .expect("blake3 capability accepts 64-hex");
        assert_eq!(blake3.hash_kind, HashKind::Blake3);
        // No capability + 40-hex remains sha1.
        let sha1 = parse_discovered_references(
            advertisement(&"a".repeat(40), "multi_ack"),
            ServiceType::UploadPack,
        )
        .expect("default sha1");
        assert_eq!(sha1.hash_kind, HashKind::Sha1);
    }

    #[test]
    fn protocol_rejects_tagged_id() {
        for tagged in [
            format!("sha256:{}", "1".repeat(64)),
            format!("blake3:{}", "1".repeat(64)),
            format!("sha1:{}", "1".repeat(40)),
        ] {
            let err = discovery_error(&advertisement(&tagged, "object-format=sha256"));
            assert!(
                err.contains("Tagged object id"),
                "tagged wire id must fail closed: {err}"
            );
        }
    }

    #[test]
    fn protocol_rejects_conflicting_object_format() {
        let hash = "1".repeat(64);
        for caps in [
            "object-format=sha256 object-format=blake3",
            "object-format=blake3 object-format=sha256",
            "object-format=sha256 object-format=sha256",
            "multi_ack object-format=sha1 object-format=sha256",
        ] {
            let err = discovery_error(&advertisement(&hash, caps));
            assert_eq!(
                err, "Conflicting object-format capabilities",
                "caps={caps:?}"
            );
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
        // sha1 default: do not advertise object-format.
        assert!(
            !text.contains("object-format="),
            "sha1 want line must omit object-format: {text}"
        );

        set_wire_hash_kind(HashKind::Sha256);
        let sha256_body = generate_upload_pack_content(&[], &["1".repeat(64)], &[], None);
        let sha256_text = String::from_utf8_lossy(&sha256_body);
        assert!(
            sha256_text.contains("object-format=sha256"),
            "sha256 want line must advertise object-format=sha256: {sha256_text}"
        );
        assert!(
            !sha256_text.contains("object-format=blake3"),
            "sha256 want must not advertise blake3: {sha256_text}"
        );

        set_wire_hash_kind(HashKind::Blake3);
        let blake3_body = generate_upload_pack_content(&[], &["1".repeat(64)], &[], None);
        let blake3_text = String::from_utf8_lossy(&blake3_body);
        assert!(
            blake3_text.contains("object-format=blake3"),
            "blake3 want line must advertise object-format=blake3: {blake3_text}"
        );
        assert!(
            !blake3_text.contains("object-format=sha256"),
            "blake3 want must not advertise sha256: {blake3_text}"
        );
        set_wire_hash_kind(HashKind::Sha1);
    }

    #[test]
    fn missing_shallow_capability_is_typed_and_keeps_actionable_display() {
        let want = vec!["1".repeat(40)];
        for (boundaries, depth) in [(Vec::new(), Some(1)), (vec!["2".repeat(40)], None)] {
            let error =
                generate_upload_pack_content_with_capabilities(&[], &want, &boundaries, depth, &[])
                    .expect_err("depth and existing boundaries require server shallow support");
            assert!(is_missing_shallow_capability(&error));
            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert_eq!(
                error.to_string(),
                "remote did not advertise the Git 'shallow' capability; cannot request --depth or send existing shallow boundaries; use a server with shallow support"
            );

            let contextual = IoError::other(error);
            assert!(is_missing_shallow_capability(&contextual));
        }

        let request = generate_upload_pack_content_with_capabilities(&[], &want, &[], None, &[])
            .expect("full fetch does not require shallow support");
        assert!(!request.is_empty());
        assert!(!is_missing_shallow_capability(&IoError::other(
            "remote did not advertise the Git 'shallow' capability"
        )));
    }

    #[test]
    fn second_upload_pack_shallow_advertisement_must_match_first() {
        let boundary = "1".repeat(40);
        let other = "2".repeat(40);
        assert!(
            verify_shallow_advertisement_unchanged(
                std::slice::from_ref(&boundary),
                std::slice::from_ref(&boundary),
            )
            .is_ok()
        );
        let error = verify_shallow_advertisement_unchanged(&[other], &[])
            .expect_err("changed boundary set must fail before want lines are sent");
        assert!(is_shallow_advertisement_changed(&error));
        assert!(!is_missing_shallow_capability(&error));
        assert!(
            error
                .to_string()
                .contains("changed between discovery and fetch")
        );
        assert!(is_shallow_advertisement_changed(&IoError::other(error)));
        assert!(!is_shallow_advertisement_changed(&IoError::other(
            "remote shallow boundaries changed between discovery and fetch"
        )));
    }

    #[test]
    fn upload_pack_discovery_bounds_distinct_shallow_advertisements() {
        let oid = "1".repeat(40);
        let mut wire = BytesMut::new();
        add_pkt_line_string(&mut wire, format!("{oid} HEAD\0shallow\n"));
        for number in 0..=MAX_ADVERTISED_SHALLOW_BOUNDARIES {
            add_pkt_line_string(&mut wire, format!("shallow {number:040x}\n"));
        }
        wire.extend_from_slice(b"0000");
        let error = parse_discovered_references(wire.freeze(), ServiceType::UploadPack)
            .expect_err("unbounded source shallows must not trigger unbounded storage probes");
        assert!(error.to_string().contains("more than 4096"), "{error}");
    }

    #[test]
    fn upload_pack_discovery_canonicalizes_shallow_oid_case() {
        let reference = "1".repeat(40);
        let upper = "A".repeat(40);
        let lower = "a".repeat(40);
        let mut wire = BytesMut::new();
        add_pkt_line_string(&mut wire, format!("{reference} HEAD\0shallow\n"));
        add_pkt_line_string(&mut wire, format!("shallow {upper}\n"));
        add_pkt_line_string(&mut wire, format!("shallow {lower}\n"));
        wire.extend_from_slice(b"0000");

        let discovery = parse_discovered_references(wire.freeze(), ServiceType::UploadPack)
            .expect("mixed-case forms of one valid boundary must parse once");
        assert_eq!(discovery.shallow_boundaries, vec![lower]);
        verify_shallow_advertisement_unchanged(&discovery.shallow_boundaries, &["a".repeat(40)])
            .expect("canonical boundary must compare equal across advertisements");
    }
}
