//! Archive locator and pack closure checks for explicit traces fetch.
//!
//! Validation is pure. Publishing a ref is the caller's job, and only after
//! [`validate_archive`] succeeds.

use sha1::Sha1;
use sha2::Sha256;

use super::mega2_traces::HashKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveObject {
    pub id: String,
    pub kind: String,
    pub payload: Vec<u8>,
    pub links: Vec<String>,
    pub scope: Option<String>,
}

pub fn archive_tracking_ref(
    remote: &str,
    stream_id: &str,
    hash_kind: HashKind,
) -> Result<String, String> {
    if remote.is_empty() || remote.contains('/') {
        return Err("archive remote name must be a single path segment".to_string());
    }
    uuid::Uuid::parse_str(stream_id)
        .map_err(|_| format!("stream id {stream_id:?} is not a UUID"))?;
    let name = format!(
        "refs/remotes/{remote}/traces/{stream_id}/{}",
        hash_kind.as_str()
    );
    if name == "refs/heads/traces" || name == "refs/libra/traces" || name.starts_with("refs/heads/")
    {
        return Err("archive ref must not replace local traces or a branch".to_string());
    }
    Ok(name)
}

pub fn server_fingerprint(server_id: &str, authority: &str) -> String {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(b"libra-traces/server/v1\0");
    hasher.update(server_id.as_bytes());
    hasher.update(authority.as_bytes());
    hex_encode(hasher.finalize().as_slice())
}

/// Authority is `scheme://host:port` plus the service-root path. Userinfo is omitted.
pub fn transport_authority(remote_url: &str) -> Result<String, String> {
    let url = url::Url::parse(remote_url)
        .map_err(|_| format!("remote URL {remote_url:?} is not a traces transport URL"))?;
    if !matches!(url.scheme(), "http" | "https" | "ssh") {
        return Err(format!(
            "remote URL {remote_url:?} is not an HTTP(S) or SSH Mega2 endpoint"
        ));
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| format!("remote URL {remote_url:?} has no host"))?;
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = url.path().trim_end_matches('/');
    Ok(format!("{}://{host}{port}{path}", url.scheme()))
}

/// SSH discovery URL for one stream. The code remote path is replaced, not rewritten in config.
pub fn ssh_stream_locator(remote_url: &str, stream_id: &str) -> Result<String, String> {
    let derived = super::mega2_traces::derive_scoped_ssh_uri(remote_url)?;
    let path = super::mega2_traces::by_scope_git_path(stream_id)?;
    let rest = derived
        .strip_prefix("ssh://")
        .ok_or_else(|| format!("derived SSH URL {derived:?} has no ssh scheme"))?;
    let host = rest.split_once('/').map(|(host, _)| host).unwrap_or(rest);
    if host.is_empty() {
        return Err(format!(
            "cannot derive a traces SSH host from {remote_url:?}"
        ));
    }
    Ok(format!("ssh://{host}{path}"))
}

/// Read `libra-traces-server` from an authenticated advertisement and bind it to this stream.
pub fn bind_authenticated_discovery(
    advertisement: &str,
    stream_id: &str,
) -> Result<String, String> {
    super::mega2_traces::by_scope_git_path(stream_id)?;
    if advertisement.trim().is_empty() {
        return Err("anonymous traces discovery is refused".to_string());
    }
    let server_id = super::mega2_traces::advertised_server_id(advertisement)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "authenticated discovery has no libra-traces-server id".to_string())?;
    if let Some(scope) = super::mega2_traces::advertised_scope(advertisement)
        && scope != stream_id
    {
        return Err(format!(
            "discovery scope {scope} does not match requested stream {stream_id}"
        ));
    }
    Ok(server_id)
}

pub fn fingerprint_conflict(stored: Option<&str>, next: &str) -> Result<(), String> {
    match stored.map(str::trim).filter(|value| !value.is_empty()) {
        Some(stored) if stored != next => Err(format!(
            "remote already records traces server fingerprint {stored}, which does not match {next}; the archive ref was not changed"
        )),
        _ => Ok(()),
    }
}

pub fn reject_hash_conversion(archive: HashKind, repository: HashKind) -> Result<(), String> {
    if archive == repository {
        Ok(())
    } else {
        Err(format!(
            "archive hash kind {} does not match repository object format {}; hash formats are not converted",
            archive.as_str(),
            repository.as_str()
        ))
    }
}

pub fn git_object_id(hash_kind: HashKind, kind: &str, payload: &[u8]) -> Result<String, String> {
    if !matches!(kind, "blob" | "tree" | "commit" | "tag") {
        return Err(format!("object kind {kind:?} is not a git object type"));
    }
    let mut bytes = format!("{kind} {}\0", payload.len()).into_bytes();
    bytes.extend_from_slice(payload);
    Ok(content_id(hash_kind, &bytes))
}

pub fn content_id(hash_kind: HashKind, payload: &[u8]) -> String {
    match hash_kind {
        HashKind::Sha1 => {
            use sha1::Digest;
            hex_encode(Sha1::digest(payload).as_slice())
        }
        HashKind::Sha256 => {
            use sha2::Digest;
            hex_encode(Sha256::digest(payload).as_slice())
        }
    }
}

pub fn validate_archive(
    objects: &[ArchiveObject],
    tip: &str,
    hash_kind: HashKind,
    stream_id: &str,
) -> Result<(), String> {
    if objects.is_empty() {
        return Err("traces archive contains no objects".to_string());
    }
    let mut ids = std::collections::BTreeMap::new();
    for object in objects {
        if let Some(scope) = object.scope.as_deref()
            && scope != stream_id
        {
            return Err(format!(
                "object {} is outside stream {stream_id}",
                object.id
            ));
        }
        let actual = git_object_id(hash_kind, &object.kind, &object.payload)?;
        if actual != object.id {
            return Err(format!(
                "object {} does not match its {hash_kind:?} {} payload",
                object.id, object.kind
            ));
        }
        if ids.insert(object.id.clone(), object).is_some() {
            return Err(format!("duplicate object {}", object.id));
        }
    }
    for object in objects {
        for link in &object.links {
            let Some(target) = ids.get(link) else {
                return Err(format!("object {} links to missing {link}", object.id));
            };
            if let Some(scope) = target.scope.as_deref()
                && scope != stream_id
            {
                return Err(format!(
                    "object {} uses delta base {link} outside stream {stream_id}",
                    object.id
                ));
            }
        }
    }
    if !ids.contains_key(tip) {
        return Err(format!(
            "archive tip {tip} is not in the verified object set"
        ));
    }
    Ok(())
}

pub fn forbids_local_traces_ref(name: &str) -> bool {
    name == "refs/heads/traces"
        || name == "refs/libra/traces"
        || name.starts_with("refs/heads/traces/")
        || name.starts_with("refs/libra/traces/")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_locator_binding_matrix() {
        let stream = "33333333-3333-4333-8333-333333333333";
        let name = archive_tracking_ref("origin", stream, HashKind::Sha1).unwrap();
        assert_eq!(
            name,
            "refs/remotes/origin/traces/33333333-3333-4333-8333-333333333333/sha1"
        );
        assert!(forbids_local_traces_ref("refs/heads/traces"));
        assert!(forbids_local_traces_ref("refs/libra/traces"));
        assert!(!forbids_local_traces_ref(&name));
        assert!(archive_tracking_ref("origin/extra", stream, HashKind::Sha1).is_err());
        assert!(archive_tracking_ref("origin", "not-a-uuid", HashKind::Sha1).is_err());
        let first = server_fingerprint("server-a", "https://mega.example");
        let second = server_fingerprint("server-b", "https://mega.example");
        assert_ne!(first, second);
        assert_eq!(
            first,
            server_fingerprint("server-a", "https://mega.example")
        );
        assert_ne!(stream, first);
        assert_eq!(
            transport_authority("https://git@mega.example:8443/project/foo.git").unwrap(),
            "https://mega.example:8443/project/foo.git"
        );
        let ssh =
            ssh_stream_locator("ssh://git@example.test:2222/project/foo.git", stream).unwrap();
        assert_eq!(
            ssh,
            format!("ssh://traces@example.test:2222/.traces/by-scope/{stream}.git")
        );
        assert!(ssh_stream_locator("https://example.test/project/foo.git", stream).is_err());
        assert!(bind_authenticated_discovery("", stream).is_err());
        assert!(bind_authenticated_discovery("agent report-status", stream).is_err());
        assert_eq!(
            bind_authenticated_discovery(
                &format!("libra-traces-server=server-a libra-traces-scope={stream}"),
                stream
            )
            .unwrap(),
            "server-a"
        );
        assert!(
            bind_authenticated_discovery(
                "libra-traces-server=server-a libra-traces-scope=other-stream",
                stream
            )
            .is_err()
        );
        assert!(fingerprint_conflict(Some(&first), &second).is_err());
        assert!(fingerprint_conflict(Some(&first), &first).is_ok());
        assert!(reject_hash_conversion(HashKind::Sha256, HashKind::Sha1).is_err());
        assert!(reject_hash_conversion(HashKind::Sha1, HashKind::Sha1).is_ok());
    }

    #[test]
    fn archive_pack_integrity_matrix() {
        let stream = "33333333-3333-4333-8333-333333333333";
        let payload = b"evidence";
        let id = git_object_id(HashKind::Sha1, "blob", payload).unwrap();
        let parent_payload = b"parent";
        let parent = git_object_id(HashKind::Sha1, "blob", parent_payload).unwrap();
        let sha256 = git_object_id(HashKind::Sha256, "blob", payload).unwrap();
        assert_ne!(id, sha256);
        let objects = vec![
            ArchiveObject {
                id: parent.clone(),
                kind: "blob".to_string(),
                payload: parent_payload.to_vec(),
                links: Vec::new(),
                scope: Some(stream.to_string()),
            },
            ArchiveObject {
                id: id.clone(),
                kind: "blob".to_string(),
                payload: payload.to_vec(),
                links: vec![parent.clone()],
                scope: None,
            },
        ];
        assert!(validate_archive(&objects, &id, HashKind::Sha1, stream).is_ok());
        assert!(validate_archive(&objects, &id, HashKind::Sha256, stream).is_err());
        assert!(validate_archive(&objects, "missing", HashKind::Sha1, stream).is_err());
        let mut broken = objects.clone();
        broken[1].payload = b"tampered".to_vec();
        assert!(validate_archive(&broken, &id, HashKind::Sha1, stream).is_err());
        let dangling = vec![ArchiveObject {
            id: id.clone(),
            kind: "blob".to_string(),
            payload: payload.to_vec(),
            links: vec!["absent".to_string()],
            scope: None,
        }];
        assert!(validate_archive(&dangling, &id, HashKind::Sha1, stream).is_err());
        assert!(validate_archive(&[], &id, HashKind::Sha1, stream).is_err());
        let foreign = vec![ArchiveObject {
            id: id.clone(),
            kind: "blob".to_string(),
            payload: payload.to_vec(),
            links: Vec::new(),
            scope: Some("other-stream".to_string()),
        }];
        assert!(validate_archive(&foreign, &id, HashKind::Sha1, stream).is_err());
        let mut cross_base = objects.clone();
        cross_base[0].scope = Some("other-stream".to_string());
        assert!(validate_archive(&cross_base, &id, HashKind::Sha1, stream).is_err());
    }
}
