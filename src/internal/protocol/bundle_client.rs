//! Bundle-file transport: treat a Git v2 bundle as a fetch source.
//!
//! Recognition order (git `builtin/clone.c:97-142`): a local repository
//! directory wins; then `<path>.bundle`; then `<path>` as a bundle file.

use std::{
    io::Error as IoError,
    path::{Path, PathBuf},
    str::FromStr,
};

use bytes::Bytes;
use futures_util::stream;
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash},
};

use super::{DiscRef, DiscoveryResult, FetchStream};
use crate::{
    command::bundle::{
        BundleHeader, parse_header, read_bundle_bounded, validate_bundle_pack, verify_prerequisites,
    },
    git_protocol::ServiceType,
    utils::util::cur_dir,
};

/// A parsed, bounded Git v2 bundle that can advertise heads and emit its pack.
#[derive(Debug, Clone)]
pub struct BundleClient {
    path: PathBuf,
    bytes: Vec<u8>,
    header: BundleHeader,
}

/// After a path fails as a repository directory, try `<path>.bundle` then `<path>`.
pub fn resolve_bundle_file(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cur_dir().join(path)
    };
    let mut with_suffix = absolute.clone().into_os_string();
    with_suffix.push(".bundle");
    let with_suffix = PathBuf::from(with_suffix);
    if with_suffix.is_file() {
        return Some(with_suffix);
    }
    if absolute.is_file() {
        return Some(absolute);
    }
    None
}

impl BundleClient {
    /// Open `path` as a bundle, or the Git-style `<path>.bundle` fallback.
    pub fn open_resolved(path: impl AsRef<Path>) -> Result<Self, String> {
        let file = resolve_bundle_file(path.as_ref())
            .ok_or_else(|| format!("bundle file does not exist: {}", path.as_ref().display()))?;
        Self::open(file)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cur_dir().join(path)
        };
        let bytes = read_bundle_bounded(&absolute, 128).map_err(|error| error.to_string())?;
        let header = parse_header(&bytes, 128).map_err(|error| error.to_string())?;
        Ok(Self {
            path: absolute,
            bytes,
            header,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn discovery_reference(
        &self,
        _service: ServiceType,
    ) -> Result<DiscoveryResult, GitError> {
        let hash_kind = hash_kind_from_heads(&self.header.heads)?;
        let format = match hash_kind {
            HashKind::Sha1 => "sha1",
            HashKind::Sha256 => "sha256",
            HashKind::Blake3 => "blake3",
        };
        let refs = self
            .header
            .heads
            .iter()
            .map(|(oid, name)| DiscRef {
                _hash: oid.clone(),
                _ref: name.clone(),
            })
            .collect();
        Ok(DiscoveryResult {
            refs,
            capabilities: vec![format!("object-format={format}")],
            hash_kind,
        })
    }

    pub async fn fetch_objects(
        &self,
        _have: &[String],
        _want: &[String],
        _shallow: &[String],
        _depth: Option<usize>,
    ) -> Result<FetchStream, IoError> {
        verify_prerequisites(&self.header, 128)
            .map_err(|error| IoError::other(error.to_string()))?;
        let pack = &self.bytes[self.header.pack_offset..];
        validate_bundle_pack(pack, 128).map_err(|error| IoError::other(error.to_string()))?;
        Ok(pack_bytes_to_fetch_stream(pack.to_vec()))
    }
}

fn hash_kind_from_heads(heads: &[(String, String)]) -> Result<HashKind, GitError> {
    let Some((oid, _)) = heads.first() else {
        return Err(GitError::NetworkError(
            "bundle advertises no heads".to_string(),
        ));
    };
    match ObjectHash::from_str(oid) {
        Ok(_) if oid.len() == 40 => Ok(HashKind::Sha1),
        Ok(_) if oid.len() == 64 => Ok(HashKind::Sha256),
        Ok(_) => Err(GitError::NetworkError(format!(
            "unsupported bundle object-id length {}",
            oid.len()
        ))),
        Err(error) => Err(GitError::NetworkError(format!(
            "bundle head has an invalid object id '{oid}': {error}"
        ))),
    }
}

fn pack_bytes_to_fetch_stream(pack_data: Vec<u8>) -> FetchStream {
    let mut response_data = Vec::new();
    let nak_line = "NAK\n";
    let nak_len_hex = format!("{:04x}", nak_line.len() + 4);
    response_data.extend_from_slice(nak_len_hex.as_bytes());
    response_data.extend_from_slice(nak_line.as_bytes());

    let chunk_size = 65500;
    for chunk in pack_data.chunks(chunk_size) {
        let mut sideband_data = Vec::with_capacity(1 + chunk.len());
        sideband_data.push(1);
        sideband_data.extend_from_slice(chunk);
        let len_hex = format!("{:04x}", sideband_data.len() + 4);
        response_data.extend_from_slice(len_hex.as_bytes());
        response_data.extend_from_slice(&sideband_data);
    }
    response_data.extend_from_slice(b"0000");
    Box::pin(stream::iter(vec![Ok(Bytes::from(response_data))]))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn resolve_bundle_file_prefers_suffix_then_exact_file() {
        let dir = tempdir().unwrap();
        let named = dir.path().join("b3");
        let suffixed = dir.path().join("b3.bundle");
        std::fs::write(&suffixed, b"# v2 git bundle\n").unwrap();
        assert_eq!(resolve_bundle_file(&named), Some(suffixed.clone()));
        assert_eq!(resolve_bundle_file(&suffixed), Some(suffixed));
        assert_eq!(resolve_bundle_file(&dir.path().join("missing")), None);
    }

    #[test]
    fn resolve_bundle_file_accepts_an_exact_bundle_path() {
        let dir = tempdir().unwrap();
        let bundle = dir.path().join("repo.bundle");
        std::fs::write(&bundle, b"# v2 git bundle\n").unwrap();
        assert_eq!(resolve_bundle_file(&bundle), Some(bundle));
    }
}
