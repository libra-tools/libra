//! Git wire protocol v2 for the smart HTTP transport.
//!
//! Protocol v2 replaces the v0/v1 reference advertisement with a capability
//! advertisement plus the `ls-refs` and `fetch` commands, all framed with
//! pkt-lines and the v2 `delim-pkt` (`0001`) / `response-end-pkt` (`0002`)
//! markers. See `Documentation/gitprotocol-v2.txt` in Git.
//!
//! The v0/v1 parser in [`super::parse_discovered_references`] and the v0
//! request builders are untouched: callers select this module only after the
//! discovery response proved to start with `version 2`.

use bytes::Bytes;
use git_internal::{errors::GitError, hash::HashKind};

use super::wire_kind_from_capabilities;
use crate::git_protocol::{PktFrame, read_pkt_frame};

pub mod fetch;
pub mod ls_refs;

/// HTTP request header that negotiates the wire protocol version.
pub const GIT_PROTOCOL_HEADER: &str = "Git-Protocol";

/// Value of [`GIT_PROTOCOL_HEADER`] that requests protocol v2.
pub const GIT_PROTOCOL_VERSION_2: &str = "version=2";

/// Fixed diagnostic for a discovery response that is not a v2 advertisement.
const NOT_A_V2_ADVERTISEMENT: &str = "remote response is not a protocol v2 advertisement";
/// Fixed diagnostic for a v2 advertisement that never declared its version.
const MISSING_VERSION_LINE: &str = "protocol v2 advertisement is missing the 'version 2' line";

/// A parsed protocol v2 capability advertisement.
///
/// `capabilities` keeps the raw advertised lines (for example `fetch=shallow
/// wait-for-done`, `ls-refs=unborn`, `object-format=sha1`, `agent=git/2.43.0`)
/// so the same list can be consulted by both the v2 and the legacy call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// Advertised capability lines, in advertisement order.
    pub capabilities: Vec<String>,
    /// Object hash kind resolved from the `object-format` capability.
    pub hash_kind: HashKind,
}

impl Advertisement {
    /// Values advertised for a multi-valued capability such as `fetch=...`.
    ///
    /// The value is a space-separated feature list; an empty list means the
    /// capability is absent.
    pub fn features(&self, capability: &str) -> Vec<&str> {
        let prefix = format!("{capability}=");
        self.capabilities
            .iter()
            .filter_map(|cap| cap.strip_prefix(&prefix))
            .flat_map(|value| value.split(' '))
            .filter(|feature| !feature.is_empty())
            .collect()
    }

    /// Whether the server advertised `capability` (or `capability=<value>`).
    pub fn supports(&self, capability: &str) -> bool {
        let prefix = format!("{capability}=");
        self.capabilities
            .iter()
            .any(|cap| cap == capability || cap.starts_with(&prefix))
    }
}

/// Cheap, non-failing probe: does this discovery body start a v2 exchange?
///
/// The first meaningful frame of a v2 smart-HTTP advertisement is either the
/// `# service=<name>` header followed by a flush and the `version 2` line, or
/// the `version 2` line directly. A v0/v1 advertisement starts with an object
/// id and therefore returns `false`, keeping the legacy parser in charge.
pub fn is_v2_advertisement(body: &Bytes) -> bool {
    let mut body = body.clone();
    // At most: `# service=` header, a flush, and the version line.
    for _ in 0..3 {
        let Ok(frame) = read_pkt_frame(&mut body) else {
            return false;
        };
        match frame {
            PktFrame::Flush => continue,
            PktFrame::Data(payload) => {
                let line = trim_line(payload.as_ref());
                if line == b"version 2" {
                    return true;
                }
                if !line.starts_with(b"# service=") {
                    return false;
                }
            }
            PktFrame::Delim | PktFrame::ResponseEnd => return false,
        }
    }
    false
}

/// Parse a protocol v2 capability advertisement.
///
/// Accepts both the smart-HTTP shape (`# service=` header, flush, `version 2`)
/// and a bare `version 2` line. Capability lines are collected until the next
/// flush; a delim-pkt or response-end-pkt before the version line is a
/// protocol error.
pub fn parse_advertisement(mut body: Bytes) -> Result<Advertisement, GitError> {
    if body.is_empty() {
        return Err(GitError::NetworkError(
            "empty protocol v2 advertisement".to_string(),
        ));
    }
    let mut saw_version = false;
    let mut capabilities = Vec::new();
    while !body.is_empty() {
        let frame =
            read_pkt_frame(&mut body).map_err(|error| GitError::NetworkError(error.to_string()))?;
        match frame {
            PktFrame::Flush => {
                if saw_version {
                    break;
                }
            }
            PktFrame::Delim | PktFrame::ResponseEnd => {
                return Err(GitError::NetworkError(
                    "unexpected section marker in protocol v2 advertisement".to_string(),
                ));
            }
            PktFrame::Data(payload) => {
                let text = std::str::from_utf8(payload.as_ref()).map_err(|error| {
                    GitError::NetworkError(format!(
                        "invalid UTF-8 in protocol v2 advertisement: {error}"
                    ))
                })?;
                let line = text.trim_end_matches('\n');
                if !saw_version {
                    if line.starts_with("# service=") || line.is_empty() {
                        continue;
                    }
                    if line == "version 2" {
                        saw_version = true;
                        continue;
                    }
                    return Err(GitError::NetworkError(NOT_A_V2_ADVERTISEMENT.to_string()));
                }
                if !line.is_empty() {
                    capabilities.push(line.to_string());
                }
            }
        }
    }
    if !saw_version {
        return Err(GitError::NetworkError(MISSING_VERSION_LINE.to_string()));
    }
    let hash_kind = wire_kind_from_capabilities(&capabilities)?;
    Ok(Advertisement {
        capabilities,
        hash_kind,
    })
}

/// Trim an ASCII line ending from a raw pkt-line payload.
fn trim_line(payload: &[u8]) -> &[u8] {
    let len = payload
        .iter()
        .rposition(|byte| *byte != b'\n' && *byte != b'\r')
        .map(|position| position + 1)
        .unwrap_or(0);
    &payload[..len]
}

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};

    use super::*;
    use crate::git_protocol::{add_delim_pkt, add_flush_pkt, add_pkt_line_string};

    fn advertisement_body(version: &str, caps: &[&str]) -> Bytes {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "# service=git-upload-pack\n".to_string());
        add_flush_pkt(&mut body);
        add_pkt_line_string(&mut body, format!("{version}\n"));
        for cap in caps {
            add_pkt_line_string(&mut body, format!("{cap}\n"));
        }
        add_flush_pkt(&mut body);
        body.freeze()
    }

    #[test]
    fn detects_v2_advertisement_with_and_without_service_header() {
        assert!(is_v2_advertisement(&advertisement_body("version 2", &[])));
        let mut bare = BytesMut::new();
        add_pkt_line_string(&mut bare, "version 2\n".to_string());
        add_pkt_line_string(&mut bare, "agent=git/2.43.0\n".to_string());
        add_flush_pkt(&mut bare);
        assert!(is_v2_advertisement(&bare.freeze()));
    }

    #[test]
    fn v0_advertisement_is_not_misdetected() {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "# service=git-upload-pack\n".to_string());
        add_flush_pkt(&mut body);
        add_pkt_line_string(
            &mut body,
            format!(
                "{} HEAD\0side-band-64k symref=HEAD:refs/heads/main\n",
                "1".repeat(40)
            ),
        );
        add_flush_pkt(&mut body);
        assert!(!is_v2_advertisement(&body.freeze()));
    }

    #[test]
    fn detects_neither_truncated_nor_malformed_bodies() {
        for body in [
            Bytes::new(),
            Bytes::from_static(b"0"),
            Bytes::from_static(b"zzzz"),
            Bytes::from_static(b"0001"),
            Bytes::from_static(b"0002000eversion 2\n"),
        ] {
            assert!(!is_v2_advertisement(&body), "{body:?}");
        }
    }

    #[test]
    fn parses_capabilities_and_object_format() {
        let body = advertisement_body(
            "version 2",
            &[
                "agent=git/2.43.0",
                "ls-refs=unborn",
                "fetch=shallow wait-for-done",
                "object-format=sha1",
                "server-option",
            ],
        );
        let advertisement = parse_advertisement(body).expect("valid v2 advertisement");
        assert_eq!(advertisement.hash_kind, HashKind::Sha1);
        assert_eq!(advertisement.capabilities.len(), 5);
        assert_eq!(
            advertisement.features("fetch"),
            vec!["shallow", "wait-for-done"]
        );
        assert_eq!(advertisement.features("ls-refs"), vec!["unborn"]);
        assert!(advertisement.supports("ls-refs"));
        assert!(advertisement.supports("server-option"));
        assert!(!advertisement.supports("fetch-other"));
        assert!(advertisement.features("missing").is_empty());
    }

    #[test]
    fn parses_sha256_object_format() {
        let body = advertisement_body("version 2", &["object-format=sha256"]);
        let advertisement = parse_advertisement(body).expect("valid v2 advertisement");
        assert_eq!(advertisement.hash_kind, HashKind::Sha256);
    }

    #[test]
    fn rejects_conflicting_object_format() {
        let body = advertisement_body(
            "version 2",
            &["object-format=sha256", "object-format=sha256"],
        );
        let error = parse_advertisement(body).expect_err("conflicting formats must fail closed");
        assert_eq!(
            error.to_string(),
            "Network Error: Conflicting object-format capabilities"
        );
    }

    #[test]
    fn rejects_unsupported_object_format_without_echoing_it() {
        let sentinel = "PRIVATE_CAPABILITY_SENTINEL\x1b[31m";
        let body = advertisement_body("version 2", &[&format!("object-format={sentinel}")]);
        let error = parse_advertisement(body).expect_err("unsupported format must fail closed");
        assert_eq!(
            error.to_string(),
            "Network Error: Unsupported object format capability"
        );
        assert!(!error.to_string().contains("SENTINEL"));
    }

    #[test]
    fn rejects_v0_advertisement_and_section_markers() {
        let mut v0 = BytesMut::new();
        add_pkt_line_string(&mut v0, format!("{} HEAD\0side-band-64k\n", "1".repeat(40)));
        add_flush_pkt(&mut v0);
        assert_eq!(
            parse_advertisement(v0.freeze())
                .expect_err("v0 body is not a v2 advertisement")
                .to_string(),
            format!("Network Error: {NOT_A_V2_ADVERTISEMENT}")
        );

        let mut delim = BytesMut::new();
        add_pkt_line_string(&mut delim, "version 2\n".to_string());
        add_delim_pkt(&mut delim);
        let error = parse_advertisement(delim.freeze()).expect_err("delim is not valid here");
        assert!(error.to_string().contains("unexpected section marker"));
    }

    #[test]
    fn rejects_empty_and_versionless_bodies() {
        assert_eq!(
            parse_advertisement(Bytes::new())
                .expect_err("empty body must fail")
                .to_string(),
            "Network Error: empty protocol v2 advertisement"
        );
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "agent=git/2.43.0\n".to_string());
        add_flush_pkt(&mut body);
        assert_eq!(
            parse_advertisement(body.freeze())
                .expect_err("missing version line must fail")
                .to_string(),
            format!("Network Error: {NOT_A_V2_ADVERTISEMENT}")
        );
    }

    #[test]
    fn rejects_non_utf8_capability_line() {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "version 2\n".to_string());
        let mut raw = BytesMut::new();
        raw.extend_from_slice(b"0008");
        raw.extend_from_slice(&[0xff, 0xfe, 0xfd, 0xfc]);
        body.extend_from_slice(&raw);
        add_flush_pkt(&mut body);
        let error = parse_advertisement(body.freeze()).expect_err("non-UTF-8 must fail");
        assert!(error.to_string().contains("invalid UTF-8"));
    }
}
