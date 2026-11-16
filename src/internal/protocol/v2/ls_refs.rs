//! Protocol v2 `ls-refs` command: request construction and response parsing.
//!
//! A v2 discovery response only advertises capabilities; references arrive from
//! a `ls-refs` request posted to `git-upload-pack`. The response lines are
//! `<oid> <ref>` optionally followed by `symref-target:<ref>` and
//! `peeled:<oid>` attributes.
//!
//! The parser synthesizes the two v0-shaped artifacts the rest of Libra expects
//! from a reference advertisement — the `symref=HEAD:<target>` capability and
//! the `refs/...^{}` peeled-tag entries — so `ls-remote`, default-branch
//! resolution and tag persistence keep their v0 behavior.

use bytes::{Bytes, BytesMut};
use git_internal::{errors::GitError, hash::HashKind};

use super::{super::validate_discovery_oid, Advertisement};
use crate::{
    git_protocol::{PktFrame, add_delim_pkt, add_flush_pkt, add_pkt_line_string, read_pkt_frame},
    internal::{object_format, protocol::DiscoveredReference},
};

/// Server capability advertised when `ls-refs` accepts `unborn` arguments.
const UNBORN_FEATURE: &str = "unborn";

/// Arguments for one `ls-refs` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LsRefsOptions {
    /// Ask the server to include `peeled:<oid>` for annotated tags.
    pub peel: bool,
    /// Ask the server to include `symref-target:<ref>` for symbolic refs.
    pub symrefs: bool,
    /// Restrict the answer to these ref prefixes (`ref-prefix <prefix>`).
    pub ref_prefixes: Vec<String>,
}

impl LsRefsOptions {
    /// Options covering every advertised ref, with symrefs and peeling.
    pub fn complete() -> Self {
        Self {
            peel: true,
            symrefs: true,
            ref_prefixes: Vec::new(),
        }
    }
}

/// Parsed `ls-refs` response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LsRefsResult {
    /// Advertised references, including synthesized `refs/...^{}` entries.
    pub refs: Vec<DiscoveredReference>,
    /// Target of the `HEAD` symref, when the server reported one.
    pub head_symref: Option<String>,
}

impl LsRefsResult {
    /// Capability lines derived from the response, v0-shaped.
    ///
    /// `symref=HEAD:<target>` is what [`crate::command::fetch::resolve_remote_default_branch`]
    /// and `ls-remote --symref` consume, so an `ls-refs` answer feeds the
    /// existing reference machinery without a second transport concept.
    pub fn derived_capabilities(&self) -> Vec<String> {
        self.head_symref
            .as_ref()
            .map(|target| vec![format!("symref=HEAD:{target}")])
            .unwrap_or_default()
    }
}

/// Build the pkt-line body of a `ls-refs` request.
///
/// The body is `command=ls-refs`, the client capability block, a delim-pkt,
/// the arguments and a flush-pkt. `object-format` is only sent for a non-SHA-1
/// repository, and only when the server advertised the capability.
pub fn build_request(advertisement: &Advertisement, options: &LsRefsOptions) -> Bytes {
    let mut body = BytesMut::new();
    add_pkt_line_string(&mut body, "command=ls-refs\n".to_string());
    add_pkt_line_string(
        &mut body,
        format!("agent=libra/{}\n", env!("CARGO_PKG_VERSION")),
    );
    if let Some(capability) = object_format_capability(advertisement, advertisement.hash_kind) {
        add_pkt_line_string(&mut body, capability);
    }
    if advertisement.features("ls-refs").contains(&UNBORN_FEATURE) {
        add_pkt_line_string(&mut body, format!("{UNBORN_FEATURE}\n"));
    }
    add_delim_pkt(&mut body);
    if options.peel {
        add_pkt_line_string(&mut body, "peel\n".to_string());
    }
    if options.symrefs {
        add_pkt_line_string(&mut body, "symrefs\n".to_string());
    }
    for prefix in &options.ref_prefixes {
        add_pkt_line_string(&mut body, format!("ref-prefix {prefix}\n"));
    }
    add_flush_pkt(&mut body);
    body.freeze()
}

/// The `object-format=<kind>` capability line for a non-SHA-1 wire.
///
/// SHA-1 is the protocol default, so it is never announced; a server that did
/// not advertise `object-format` never receives the line either.
fn object_format_capability(advertisement: &Advertisement, hash_kind: HashKind) -> Option<String> {
    match hash_kind {
        HashKind::Sha1 => None,
        _ => advertisement
            .supports("object-format")
            .then(|| format!("object-format={}\n", object_format::as_str(hash_kind))),
    }
}

/// Parse a `ls-refs` response body.
///
/// Lines are `<oid> <ref> [symref-target:<ref>] [peeled:<oid>]`. `unborn` lines
/// carry no object id and are skipped (an empty repository advertises no refs,
/// matching the v0 empty-advertisement behavior). Object ids are validated
/// against `hash_kind`, and a tagged (`sha256:<hex>`) id is rejected.
pub fn parse_response(mut body: Bytes, hash_kind: HashKind) -> Result<LsRefsResult, GitError> {
    let mut result = LsRefsResult::default();
    let mut peeled_tags: Vec<DiscoveredReference> = Vec::new();
    while !body.is_empty() {
        match read_pkt_frame(&mut body)
            .map_err(|error| GitError::NetworkError(error.to_string()))?
        {
            PktFrame::Data(payload) => {
                let text = std::str::from_utf8(payload.as_ref()).map_err(|error| {
                    GitError::NetworkError(format!("invalid UTF-8 in ls-refs response: {error}"))
                })?;
                let line = text.trim_end_matches('\n');
                if line.is_empty() {
                    continue;
                }
                if line.starts_with("unborn ") || line == "unborn" {
                    if let Some(target) = line
                        .split(' ')
                        .find_map(|field| field.strip_prefix("symref-target:"))
                    {
                        result.head_symref = Some(target.to_string());
                    }
                    continue;
                }
                parse_ref_line(line, hash_kind, &mut result, &mut peeled_tags)?;
            }
            PktFrame::Flush | PktFrame::Delim | PktFrame::ResponseEnd => break,
        }
    }
    result.refs.extend(peeled_tags);
    Ok(result)
}

/// Parse one `<oid> <ref> [attributes...]` line.
fn parse_ref_line(
    line: &str,
    hash_kind: HashKind,
    result: &mut LsRefsResult,
    peeled_tags: &mut Vec<DiscoveredReference>,
) -> Result<(), GitError> {
    let mut fields = line.split(' ');
    let raw_oid = fields
        .next()
        .filter(|field| !field.is_empty())
        .ok_or_else(|| {
            GitError::NetworkError("missing object id in ls-refs response".to_string())
        })?;
    let refname = fields
        .next()
        .filter(|field| !field.is_empty())
        .ok_or_else(|| {
            GitError::NetworkError("missing ref name in ls-refs response".to_string())
        })?;
    let hash = validate_discovery_oid(hash_kind, raw_oid)?;
    result.refs.push(DiscoveredReference {
        _hash: hash,
        _ref: refname.to_string(),
    });
    for attribute in fields {
        let Some(peeled) = attribute.strip_prefix("peeled:") else {
            if let Some(target) = attribute.strip_prefix("symref-target:")
                && refname == "HEAD"
            {
                result.head_symref = Some(target.to_string());
            }
            continue;
        };
        let peeled = validate_discovery_oid(hash_kind, peeled)?;
        peeled_tags.push(DiscoveredReference {
            _hash: peeled,
            _ref: format!("{refname}^{{}}"),
        });
    }
    Ok(())
}

/// Whether `name` is a well-formed ref name for a v2 advertisement.
///
/// v2 responses are remote input; a ref name must at least name a ref or be
/// `HEAD`, so a malformed answer fails closed instead of creating local refs.
pub(crate) fn is_plausible_ref_name(name: &str) -> bool {
    name == "HEAD" || name.starts_with("refs/")
}

/// Validate every parsed reference names a real ref namespace.
pub(crate) fn validate_ref_names(result: &LsRefsResult) -> Result<(), GitError> {
    for reference in &result.refs {
        if !is_plausible_ref_name(reference.name()) {
            return Err(GitError::NetworkError(
                "ls-refs response contained a ref outside the refs/ namespace".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};

    use super::*;
    use crate::git_protocol::{add_flush_pkt, add_pkt_line_string};

    fn advertisement(caps: &[&str]) -> Advertisement {
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, "version 2\n".to_string());
        for cap in caps {
            add_pkt_line_string(&mut body, format!("{cap}\n"));
        }
        add_flush_pkt(&mut body);
        super::super::parse_advertisement(body.freeze()).expect("valid advertisement")
    }

    fn request_text(advertisement: &Advertisement, options: &LsRefsOptions) -> String {
        let body = build_request(advertisement, options);
        let text = String::from_utf8_lossy(&body).to_string();
        // Keep the raw framing visible so the test asserts section boundaries.
        format!("{text:?}")
    }

    #[test]
    fn request_carries_command_capabilities_delim_and_arguments() {
        let advertisement = advertisement(&["ls-refs=unborn", "fetch=shallow"]);
        let mut options = LsRefsOptions::complete();
        options.ref_prefixes = vec!["refs/heads/".to_string(), "refs/tags/".to_string()];
        let body = build_request(&advertisement, &options);
        let text = String::from_utf8_lossy(&body).to_string();

        assert!(text.starts_with("0014command=ls-refs\n"), "text={text:?}");
        assert!(text.contains(&format!("agent=libra/{}\n", env!("CARGO_PKG_VERSION"))));
        assert!(
            text.contains("000bunborn\n"),
            "unborn feature text={text:?}"
        );
        let delimiter = text.find("0001").expect("delim-pkt separates sections");
        let arguments = text.find("0009peel\n").expect("peel argument");
        assert!(delimiter < arguments, "delim must precede arguments");
        assert!(text.ends_with("0000"));
        assert!(text.contains("000csymrefs\n"));
        assert!(text.contains("ref-prefix refs/heads/\n"));
        assert!(text.contains("ref-prefix refs/tags/\n"));
        // SHA-1 is the protocol default and must not be announced.
        assert!(!text.contains("object-format="));
    }

    #[test]
    fn request_omits_unadvertised_features_and_redundant_arguments() {
        let advertisement = advertisement(&["fetch=shallow"]);
        let options = LsRefsOptions::default();
        let text = request_text(&advertisement, &options);
        assert!(!text.contains("unborn"));
        assert!(!text.contains("peel"));
        assert!(!text.contains("symrefs"));
        assert!(!text.contains("ref-prefix"));
    }

    #[test]
    fn request_announces_sha256_only_when_advertised() {
        let mut advertisement_sha256 = advertisement(&["object-format=sha256", "ls-refs"]);
        advertisement_sha256.hash_kind = HashKind::Sha256;
        let text = request_text(&advertisement_sha256, &LsRefsOptions::complete());
        assert!(text.contains("object-format=sha256"), "{text}");

        // A server that never advertised object-format is not sent the line.
        let silent = Advertisement {
            capabilities: vec!["ls-refs".to_string()],
            hash_kind: HashKind::Sha256,
        };
        let text = request_text(&silent, &LsRefsOptions::complete());
        assert!(!text.contains("object-format="), "{text}");
    }

    fn response(lines: &[&str]) -> Bytes {
        let mut body = BytesMut::new();
        for line in lines {
            add_pkt_line_string(&mut body, format!("{line}\n"));
        }
        add_flush_pkt(&mut body);
        body.freeze()
    }

    #[test]
    fn parses_refs_symrefs_and_peeled_tags() {
        let head = "a".repeat(40);
        let main = "b".repeat(40);
        let tag = "c".repeat(40);
        let peeled = "d".repeat(40);
        let body = response(&[
            &format!("{head} HEAD symref-target:refs/heads/main"),
            &format!("{main} refs/heads/main"),
            &format!("{tag} refs/tags/v1.0 peeled:{peeled}"),
        ]);
        let result = parse_response(body, HashKind::Sha1).expect("valid response");
        assert_eq!(result.head_symref.as_deref(), Some("refs/heads/main"));
        assert_eq!(
            result.derived_capabilities(),
            vec!["symref=HEAD:refs/heads/main".to_string()]
        );
        assert_eq!(
            result.refs,
            vec![
                DiscoveredReference {
                    _hash: head,
                    _ref: "HEAD".to_string()
                },
                DiscoveredReference {
                    _hash: main,
                    _ref: "refs/heads/main".to_string()
                },
                DiscoveredReference {
                    _hash: tag,
                    _ref: "refs/tags/v1.0".to_string()
                },
                DiscoveredReference {
                    _hash: peeled,
                    _ref: "refs/tags/v1.0^{}".to_string()
                },
            ]
        );
    }

    #[test]
    fn empty_response_and_unborn_head_yield_no_refs() {
        let result = parse_response(response(&[]), HashKind::Sha1).expect("empty response");
        assert!(result.refs.is_empty());
        assert!(result.derived_capabilities().is_empty());

        let result = parse_response(
            response(&["unborn HEAD symref-target:refs/heads/main"]),
            HashKind::Sha1,
        )
        .expect("unborn HEAD");
        assert!(result.refs.is_empty());
        assert_eq!(result.head_symref.as_deref(), Some("refs/heads/main"));
    }

    #[test]
    fn rejects_invalid_oids_and_tagged_ids() {
        for line in [
            "1 refs/heads/main".to_string(),
            format!("{} refs/heads/main", "z".repeat(40)),
            format!("sha256:{} refs/heads/main", "1".repeat(64)),
            format!(
                "{} refs/heads/main peeled:{}",
                "1".repeat(40),
                "z".repeat(40)
            ),
        ] {
            let error = parse_response(response(&[&line]), HashKind::Sha1)
                .expect_err("invalid object ids must fail closed");
            let message = error.to_string();
            assert!(
                message.contains("Invalid object id") || message.contains("Tagged object id"),
                "unexpected error for {line:?}: {message}"
            );
        }
        // A sha256 response is accepted when the wire kind says sha256.
        let body = response(&[&format!("{} refs/heads/main", "1".repeat(64))]);
        parse_response(body, HashKind::Sha256).expect("64-hex oid is valid for sha256");
    }

    #[test]
    fn rejects_malformed_lines_without_panicking() {
        for line in ["refs/heads/main", " ", "\t", "1 "] {
            let body = response(&[line]);
            let outcome = parse_response(body, HashKind::Sha1);
            assert!(outcome.is_err(), "line {line:?} must be rejected");
        }
        // A blank line carries no ref and is ignored rather than rejected.
        let blank = parse_response(response(&[""]), HashKind::Sha1).expect("blank line is ignored");
        assert!(blank.refs.is_empty());
        // A truncated frame is a protocol error, not a panic.
        let mut body = BytesMut::new();
        body.extend_from_slice(b"0008ab");
        let error =
            parse_response(body.freeze(), HashKind::Sha1).expect_err("truncated frame must fail");
        assert!(error.to_string().contains("pkt-line"));
    }

    #[test]
    fn response_end_marker_terminates_the_response() {
        let oid = "1".repeat(40);
        let mut body = BytesMut::new();
        add_pkt_line_string(&mut body, format!("{oid} refs/heads/main\n"));
        crate::git_protocol::add_response_end_pkt(&mut body);
        let result = parse_response(body.freeze(), HashKind::Sha1).expect("response-end is valid");
        assert_eq!(result.refs.len(), 1);
    }

    #[test]
    fn ref_name_validation_rejects_outside_namespace() {
        let result = LsRefsResult {
            refs: vec![DiscoveredReference {
                _hash: "1".repeat(40),
                _ref: "../../etc/passwd".to_string(),
            }],
            head_symref: None,
        };
        validate_ref_names(&result).expect_err("refs outside refs/ are rejected");
        let ok = LsRefsResult {
            refs: vec![
                DiscoveredReference {
                    _hash: "1".repeat(40),
                    _ref: "HEAD".to_string(),
                },
                DiscoveredReference {
                    _hash: "1".repeat(40),
                    _ref: "refs/heads/main".to_string(),
                },
            ],
            head_symref: None,
        };
        validate_ref_names(&ok).expect("HEAD and refs/ names are valid");
    }
}
