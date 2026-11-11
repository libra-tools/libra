//! Attribute-directed conversion from worktree bytes to stored blob bytes.
//!
//! Text/eol rules are shared by writers and content comparisons. Index
//! objects are never converted, and worktree files are never rewritten here.

use std::{
    collections::BTreeMap,
    io::{self, Read, Seek},
    path::Path,
};

use super::{
    attributes::{self, AttributeState},
    beneath::{self, RawLstat},
    lfs, util,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanPolicy {
    Preserve,
    Text,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanMode {
    Normal,
    Renormalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolvedPolicy {
    Preserve,
    Text,
}

/// Git's convert.c statistics, carried across chunks including a pending CR.
#[derive(Default)]
struct TextStats {
    crlf: u64,
    lone_cr: u64,
    printable: u64,
    nonprintable: u64,
    nul: bool,
    pending_cr: bool,
    final_sub: bool,
}

impl TextStats {
    fn consume(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.pending_cr {
                if byte == b'\n' {
                    self.crlf += 1;
                } else {
                    self.lone_cr += 1;
                }
            }
            self.pending_cr = byte == b'\r';
            self.final_sub = byte == 0x1a;
            match byte {
                b'\r' | b'\n' => {}
                0 => {
                    self.nul = true;
                    self.nonprintable += 1;
                }
                8 | 9 | 12 | 27 => self.printable += 1,
                1..=31 | 127 => self.nonprintable += 1,
                _ => self.printable += 1,
            }
        }
    }

    fn is_binary(&self) -> bool {
        self.nul
            || self.lone_cr != 0
            || self.pending_cr
            || (self.printable >> 7) < self.nonprintable - u64::from(self.final_sub)
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut stats = Self::default();
        stats.consume(bytes);
        stats
    }
}

/// Index objects are inspected, never cleaned. Non-blob objects are rejected
/// by the typed reader before reaching this predicate.
pub(crate) fn has_text_crlf(bytes: &[u8]) -> bool {
    let stats = TextStats::from_bytes(bytes);
    stats.crlf != 0 && !stats.is_binary()
}

fn resolve_policy(
    policy: CleanPolicy,
    stats: &TextStats,
    mode: CleanMode,
    index_has_text_crlf: impl FnOnce() -> io::Result<bool>,
) -> io::Result<ResolvedPolicy> {
    Ok(match policy {
        CleanPolicy::Preserve => ResolvedPolicy::Preserve,
        CleanPolicy::Text => ResolvedPolicy::Text,
        CleanPolicy::Auto => {
            if stats.crlf == 0
                || stats.is_binary()
                || (mode == CleanMode::Normal && index_has_text_crlf()?)
            {
                ResolvedPolicy::Preserve
            } else {
                ResolvedPolicy::Text
            }
        }
    })
}

/// `-text` wins over `eol`; `text=auto` retains binary and index protection
/// even with an accompanying eol value.
pub(crate) fn explicit_policy(attributes: &BTreeMap<String, AttributeState>) -> CleanPolicy {
    match attributes.get("text") {
        Some(AttributeState::Unset) => return CleanPolicy::Preserve,
        Some(AttributeState::Value(value)) if value == "auto" => return CleanPolicy::Auto,
        Some(AttributeState::Set) => return CleanPolicy::Text,
        _ => {}
    }
    match attributes.get("eol") {
        Some(AttributeState::Value(value)) if matches!(value.as_str(), "lf" | "crlf") => {
            CleanPolicy::Text
        }
        _ => CleanPolicy::Preserve,
    }
}

/// Compact in place so converting an add input does not allocate a second copy.
/// A lone CR and the final-newline state remain unchanged.
fn clean_bytes(mut bytes: Vec<u8>, policy: ResolvedPolicy) -> Vec<u8> {
    if policy == ResolvedPolicy::Preserve {
        return bytes;
    }
    let mut read = 0;
    let mut written = 0;
    while read < bytes.len() {
        if bytes[read] == b'\r' && bytes.get(read + 1) == Some(&b'\n') {
            read += 1;
        }
        bytes[written] = bytes[read];
        written += 1;
        read += 1;
    }
    bytes.truncate(written);
    bytes
}

/// Read the representation to stage, retaining LFS and symlink semantics.
/// Regular content is read from one no-follow descriptor; the initial length
/// bounds the read, and a changed file is rejected before an index can be saved.
pub(crate) fn read_clean_blob_bytes(
    path: impl AsRef<Path>,
    mode: CleanMode,
    index_has_text_crlf: impl FnOnce() -> io::Result<bool>,
) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    read_clean_blob_bytes_inner(path, mode, index_has_text_crlf).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "read worktree blob '{}' for line-ending conversion: {error}; check the file and its attribute sources",
                path.display()
            ),
        )
    })
}

fn read_clean_blob_bytes_inner(
    path: &Path,
    mode: CleanMode,
    index_has_text_crlf: impl FnOnce() -> io::Result<bool>,
) -> io::Result<Vec<u8>> {
    let workdir = util::try_working_dir()?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    let relative = absolute.strip_prefix(&workdir).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "blob path is outside the worktree",
        )
    })?;
    let root = beneath::open_root(&workdir)?;
    let stat = beneath::lstat_beneath(&root, relative)?;
    if stat.is_symlink {
        return beneath::read_symlink_beneath(&root, relative);
    }
    let attributes = attributes::try_all_attribute_states_beneath(&workdir, &root, relative)?;
    let is_lfs = matches!(
        attributes.get("filter"),
        Some(AttributeState::Value(value)) if value == "lfs"
    );
    let mut file = beneath::open_file_beneath(&root, relative)?;
    let before = RawLstat::from_metadata(&file.metadata()?)?;
    let mut bytes = Vec::new();
    let mut lfs_hash = is_lfs.then(|| ring::digest::Context::new(&ring::digest::SHA256));
    if !is_lfs {
        let length = usize::try_from(before.len)
            .map_err(|_| io::Error::other("file is too large to stage on this platform"))?;
        bytes.try_reserve_exact(length).map_err(|error| {
            io::Error::other(format!("cannot allocate staged content: {error}"))
        })?;
    }
    let mut remaining = before.len;
    let mut buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let requested = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worktree file shrank while it was being read; retry staging",
            ));
        }
        remaining -= read as u64;
        if let Some(hash) = &mut lfs_hash {
            hash.update(&buffer[..read]);
        } else {
            bytes.extend_from_slice(&buffer[..read]);
        }
    }
    let after = RawLstat::from_metadata(&file.metadata()?)?;
    if !same_file_snapshot(&before, &after) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree file changed while it was being read; retry staging",
        ));
    }
    if let Some(hash) = lfs_hash {
        let oid = hex::encode(hash.finish().as_ref());
        return Ok(lfs::format_pointer_string(&oid, before.len).into_bytes());
    }
    let policy = explicit_policy(&attributes);
    let stats = if policy == CleanPolicy::Auto {
        TextStats::from_bytes(&bytes)
    } else {
        TextStats::default()
    };
    let resolved = resolve_policy(policy, &stats, mode, index_has_text_crlf)?;
    Ok(clean_bytes(bytes, resolved))
}

pub(crate) fn same_file_snapshot(left: &RawLstat, right: &RawLstat) -> bool {
    (
        left.len,
        left.mode,
        left.ctime_sec,
        left.ctime_nsec,
        left.mtime_sec,
        left.mtime_nsec,
    ) == (
        right.len,
        right.mode,
        right.ctime_sec,
        right.ctime_nsec,
        right.mtime_sec,
        right.mtime_nsec,
    )
}

/// Hash a bounded stream through the exact byte policy used by add. Text
/// needs its converted length for the blob header, so both passes share one
/// descriptor and the same snapshot; no second pathname open is allowed.
pub(crate) fn hash_clean_file_handle(
    mut file: std::fs::File,
    policy: CleanPolicy,
    index_has_text_crlf: impl FnOnce() -> io::Result<bool>,
) -> io::Result<git_internal::hash::ObjectHash> {
    let before = RawLstat::from_metadata(&file.metadata()?)?;
    let mut stats = TextStats::default();
    if policy != CleanPolicy::Preserve {
        stream_clean(&mut file, before.len, ResolvedPolicy::Preserve, |bytes| {
            stats.consume(bytes)
        })?;
        file.rewind()?;
    }
    let policy = resolve_policy(policy, &stats, CleanMode::Normal, index_has_text_crlf)?;
    let length = before.len
        - if policy == ResolvedPolicy::Text {
            stats.crlf
        } else {
            0
        };
    let kind = git_internal::hash::get_hash_kind();
    let mut hash = git_internal::utils::HashAlgorithm::new_for_kind(kind);
    hash.update(format!("blob {length}\0").as_bytes());
    let mut actual = 0u64;
    stream_clean(&mut file, before.len, policy, |bytes| {
        actual += bytes.len() as u64;
        hash.update(bytes);
    })?;
    let after = RawLstat::from_metadata(&file.metadata()?)?;
    if actual != length || !same_file_snapshot(&before, &after) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree file changed during line-ending comparison; retry the command",
        ));
    }
    git_internal::hash::ObjectHash::from_bytes_for_kind(kind, &hash.finalize())
        .map_err(io::Error::other)
}

fn stream_clean(
    file: &mut std::fs::File,
    length: u64,
    policy: ResolvedPolicy,
    mut consume: impl FnMut(&[u8]),
) -> io::Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut buffer = Vec::with_capacity(CHUNK + 1);
    let mut remaining = length;
    let mut pending_cr = false;
    while remaining != 0 {
        buffer.clear();
        if pending_cr {
            buffer.push(b'\r');
        }
        let prefix = buffer.len();
        let requested = remaining.min(CHUNK as u64) as usize;
        buffer.resize(prefix + requested, 0);
        let read = file.read(&mut buffer[prefix..])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "worktree file shrank during line-ending comparison; retry the command",
            ));
        }
        remaining -= read as u64;
        buffer.truncate(prefix + read);
        pending_cr = policy == ResolvedPolicy::Text && buffer.last() == Some(&b'\r');
        if pending_cr {
            buffer.pop();
        }
        buffer = clean_bytes(buffer, policy);
        consume(&buffer);
    }
    if pending_cr {
        consume(b"\r");
    }
    Ok(())
}

pub(crate) fn hash_clean_blob_beneath(
    workdir: &Path,
    root: &std::fs::File,
    relative: &Path,
    context: &attributes::ConversionAttributeContext,
    session: Option<u64>,
    index_has_text_crlf: impl FnOnce() -> io::Result<bool>,
) -> io::Result<git_internal::hash::ObjectHash> {
    let stat = beneath::lstat_beneath(root, relative).map_err(|error| {
        if error.kind() == io::ErrorKind::NotADirectory {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot inspect '{}': {error}; each parent must be a directory, not a symbolic link",
                    relative.display()
                ),
            )
        } else {
            error
        }
    })?;
    if stat.is_symlink {
        return Ok(
            git_internal::internal::object::blob::Blob::from_content_bytes(
                beneath::read_symlink_beneath(root, relative)?,
            )
            .id,
        );
    }
    #[cfg(debug_assertions)]
    if std::env::var_os("LIBRA_TEST").is_some_and(|value| value == "1")
        && std::env::var("LIBRA_TEST_CONVERSION_HASH_FAIL")
            .is_ok_and(|path| path == "*" || relative == Path::new(&path))
    {
        return Err(io::Error::other(
            "test-injected conversion content read failure",
        ));
    }
    let states = attributes::attribute_states_beneath_with_context(
        workdir, root, relative, context, session, true,
    )?;
    let mut file = beneath::open_file_beneath(root, relative)?;
    if matches!(states.get("filter"), Some(AttributeState::Value(value)) if value == "lfs") {
        let before = RawLstat::from_metadata(&file.metadata()?)?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        stream_clean(&mut file, before.len, ResolvedPolicy::Preserve, |bytes| {
            hash.update(bytes)
        })?;
        if !same_file_snapshot(&before, &RawLstat::from_metadata(&file.metadata()?)?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LFS file changed during comparison; retry the command",
            ));
        }
        let pointer = lfs::format_pointer_string(&hex::encode(hash.finish().as_ref()), before.len);
        return Ok(git_internal::internal::object::blob::Blob::from_content(&pointer).id);
    }
    hash_clean_file_handle(file, explicit_policy(&states), index_has_text_crlf)
}

/// One fixed-size advisory certificate per index directory. Only a complete
/// content validation may publish it. A missing, partial, corrupt or unreadable
/// certificate merely disables the stat shortcut; it never supplies a hash.
pub(crate) fn conversion_stat_certificate(
    workdir: &Path,
    root: &std::fs::File,
    directories: &[std::path::PathBuf],
    context: &attributes::ConversionAttributeContext,
    cache_root: &Path,
    index_key: &str,
    record: Option<&str>,
) -> io::Result<(String, bool)> {
    let sources = attributes::conversion_sources_fingerprint(workdir, root, directories, context)?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(b"clean-stat-v1\0");
    hash.update(index_key.as_bytes());
    hash.update(sources.as_bytes());
    let key = hex::encode(hash.finish().as_ref());
    let cache = Path::new("conversion-stat-v1");
    let Ok(cache_root) = beneath::open_root(cache_root) else {
        return Ok((sources, false));
    };
    let valid = (|| -> io::Result<bool> {
        let stat = beneath::lstat_beneath(&cache_root, cache)?;
        if !stat.is_file || stat.len != key.len() as u64 {
            return Ok(false);
        }
        let file = beneath::open_file_beneath(&cache_root, cache)?;
        let mut bytes = Vec::with_capacity(key.len() + 1);
        file.take(key.len() as u64 + 1).read_to_end(&mut bytes)?;
        Ok(bytes == key.as_bytes())
    })()
    .unwrap_or(false);
    if record == Some(sources.as_str()) && !valid {
        // Readers require the complete digest. A partial/concurrent write is
        // a cache miss; all writers have independently validated this key.
        let _ = beneath::write_regular_file_beneath(&cache_root, cache, key.as_bytes(), true);
    }
    Ok((sources, valid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_policy_matrix_selects_auto_and_preserves_unset() {
        for (text, eol, expected) in [
            (None, None, CleanPolicy::Preserve),
            (Some(AttributeState::Set), None, CleanPolicy::Text),
            (None, Some("lf"), CleanPolicy::Text),
            (None, Some("crlf"), CleanPolicy::Text),
            (None, Some("invalid"), CleanPolicy::Preserve),
            (
                Some(AttributeState::Unset),
                Some("lf"),
                CleanPolicy::Preserve,
            ),
            (
                Some(AttributeState::Value("auto".into())),
                Some("lf"),
                CleanPolicy::Auto,
            ),
            (
                Some(AttributeState::Value("auto".into())),
                Some("crlf"),
                CleanPolicy::Auto,
            ),
            (
                Some(AttributeState::Unspecified),
                Some("lf"),
                CleanPolicy::Text,
            ),
        ] {
            let mut attributes = BTreeMap::new();
            if let Some(text) = text {
                attributes.insert("text".into(), text);
            }
            if let Some(eol) = eol {
                attributes.insert("eol".into(), AttributeState::Value(eol.into()));
            }
            assert_eq!(explicit_policy(&attributes), expected, "{attributes:?}");
        }
    }

    #[test]
    fn explicit_conversion_preserves_non_pairs_and_final_newline() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b""),
            (b"one\r\ntwo\r\n", b"one\ntwo\n"),
            (b"one\r\ntwo\n", b"one\ntwo\n"),
            (b"one\rtwo\r", b"one\rtwo\r"),
            (b"\0one\r\nend", b"\0one\nend"),
            (b"\r\r\n", b"\r\n"),
        ];
        for (input, expected) in cases {
            assert_eq!(clean_bytes(input.to_vec(), ResolvedPolicy::Text), *expected);
            assert_eq!(
                clean_bytes(input.to_vec(), ResolvedPolicy::Preserve),
                *input
            );
        }
    }

    #[test]
    fn explicit_conversion_spans_read_chunk_boundaries() {
        let mut bytes = vec![b'x'; 64 * 1024 - 1];
        bytes.extend_from_slice(b"\r\nlast\r");
        let converted = clean_bytes(bytes, ResolvedPolicy::Text);
        assert_eq!(&converted[64 * 1024 - 1..], b"\nlast\r");
    }

    #[test]
    fn explicit_conversion_large_input_reuses_its_allocation() {
        let mut bytes = vec![b'x'; 64 * 1024 * 1024];
        for pair in bytes.as_chunks_mut::<{ 64 * 1024 }>().0 {
            let end = pair.len();
            pair[end - 2..].copy_from_slice(b"\r\n");
        }
        let allocation = bytes.as_ptr();
        let converted = clean_bytes(bytes, ResolvedPolicy::Text);
        assert_eq!(converted.as_ptr(), allocation);
        assert_eq!(converted.len(), 64 * 1024 * 1024 - 1024);
        assert_eq!(converted.last(), Some(&b'\n'));
    }
    #[test]
    fn i1062_worker_stream_hash_matches_add_bytes_across_chunks_and_formats() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};
        for kind in [HashKind::Sha1, HashKind::Sha256] {
            let _kind = set_hash_kind_for_test(kind);
            for length in [0, 1, 65535, 65536, 65537, 131072] {
                let mut bytes = vec![b'x'; length];
                bytes.extend_from_slice(b"\r\n\r\r\nend\r");
                let mut file = tempfile::tempfile().unwrap();
                std::io::Write::write_all(&mut file, &bytes).unwrap();
                for policy in [CleanPolicy::Preserve, CleanPolicy::Text] {
                    file.rewind().unwrap();
                    let hash =
                        hash_clean_file_handle(file.try_clone().unwrap(), policy, || Ok(false))
                            .unwrap();
                    let expected = git_internal::internal::object::blob::Blob::from_content_bytes(
                        clean_bytes(
                            bytes.clone(),
                            resolve_policy(
                                policy,
                                &TextStats::from_bytes(&bytes),
                                CleanMode::Normal,
                                || Ok(false),
                            )
                            .unwrap(),
                        ),
                    )
                    .id;
                    assert_eq!(hash, expected, "{kind:?} length={length} {policy:?}");
                }
            }
        }
    }

    #[test]
    fn i1062_worker_streams_large_text_and_preserves_lone_cr() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};
        let _kind = set_hash_kind_for_test(HashKind::Sha256);
        let mut file = tempfile::tempfile().unwrap();
        let mut block = vec![b'x'; 65536];
        block[65534..].copy_from_slice(b"\r\n");
        let mut expected = git_internal::utils::HashAlgorithm::new_for_kind(HashKind::Sha256);
        expected.update(format!("blob {}\0", 1024 * 65535).as_bytes());
        for _ in 0..1024 {
            std::io::Write::write_all(&mut file, &block).unwrap();
            expected.update(&block[..65534]);
            expected.update(b"\n");
        }
        file.rewind().unwrap();
        let hash = hash_clean_file_handle(file, CleanPolicy::Text, || Ok(false)).unwrap();
        assert_eq!(hash.to_string(), hex::encode(expected.finalize()));
    }

    #[cfg(unix)]
    #[test]
    fn i1062_worker_conversion_keeps_pinned_root_and_symlink_bytes() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};
        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let temp = tempfile::tempdir().unwrap();
        let workdir = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&workdir).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(workdir.join(".gitattributes"), "* text\n").unwrap();
        std::fs::write(workdir.join("probe"), b"inside\r\n").unwrap();
        std::fs::write(outside.join("probe"), b"outside\r\n").unwrap();
        std::os::unix::fs::symlink("target\r\n", workdir.join("link")).unwrap();
        let root = beneath::open_root(&workdir).unwrap();
        std::fs::rename(&workdir, temp.path().join("retired")).unwrap();
        std::os::unix::fs::symlink(&outside, &workdir).unwrap();
        let context = attributes::ConversionAttributeContext {
            configured: None,
            info: vec![],
        };
        for (path, expected) in [
            ("probe", b"inside\n".as_slice()),
            ("link", b"target\r\n".as_slice()),
        ] {
            let hash = hash_clean_blob_beneath(
                &workdir,
                &root,
                Path::new(path),
                &context,
                Some(1),
                || Ok(false),
            )
            .unwrap();
            assert_eq!(
                hash,
                git_internal::internal::object::blob::Blob::from_content_bytes(expected.to_vec())
                    .id
            );
        }
    }

    #[test]
    fn i1062_worker_certificate_needs_full_proof_and_source_versions() {
        let temp = tempfile::tempdir().unwrap();
        let root = beneath::open_root(temp.path()).unwrap();
        let context = attributes::ConversionAttributeContext {
            configured: None,
            info: vec![],
        };
        let key = "a".repeat(64);
        let dirs = [std::path::PathBuf::new()];
        let probe = |record: Option<&str>| {
            conversion_stat_certificate(
                temp.path(),
                &root,
                &dirs,
                &context,
                temp.path(),
                &key,
                record,
            )
            .unwrap()
        };
        let (first, valid) = probe(None);
        assert!(!valid);
        probe(Some(&first));
        assert!(probe(None).1);
        std::fs::write(temp.path().join(".gitattributes"), "* text\n").unwrap();
        let (edited, valid) = probe(None);
        assert!(!valid);
        assert_ne!(edited, first);
        probe(Some(&first));
        assert!(
            !probe(None).1,
            "stale proof cannot mint a valid certificate"
        );
        probe(Some(&edited));
        assert!(probe(None).1);
        std::fs::remove_file(temp.path().join(".gitattributes")).unwrap();
        assert!(!probe(None).1);
        std::fs::create_dir(temp.path().join(".gitattributes")).unwrap();
        assert!(
            conversion_stat_certificate(
                temp.path(),
                &root,
                &dirs,
                &context,
                temp.path(),
                &key,
                None
            )
            .is_err()
        );
    }
    #[test]
    fn i1062_worker_attribute_context_respects_global_nested_and_info_precedence() {
        use git_internal::hash::{HashKind, set_hash_kind_for_test};
        let _kind = set_hash_kind_for_test(HashKind::Sha256);
        let temp = tempfile::tempdir().unwrap();
        let workdir = temp.path().join("repo");
        std::fs::create_dir_all(workdir.join("nested")).unwrap();
        let global = temp.path().join("global-attributes");
        let info = temp.path().join("info-attributes");
        std::fs::write(&global, "*.cmd text\n*.ps1 text\n").unwrap();
        std::fs::write(workdir.join(".gitattributes"), "*.ps1 -text\n").unwrap();
        std::fs::write(workdir.join(".libra_attributes"), "root.ps1 text\n").unwrap();
        std::fs::write(workdir.join("nested/.gitattributes"), "*.ps1 text eol=lf\n").unwrap();
        std::fs::write(&info, "nested/override.ps1 -text\n").unwrap();
        let context = attributes::ConversionAttributeContext {
            configured: Some(global.clone()),
            info: vec![info],
        };
        let root = beneath::open_root(&workdir).unwrap();
        let raw = b"one\r\ntwo\r\n";
        let clean = b"one\ntwo\n";
        for (path, expected) in [
            ("global.cmd", clean.as_slice()),
            ("raw.ps1", raw.as_slice()),
            ("root.ps1", clean.as_slice()),
            ("nested/clean.ps1", clean.as_slice()),
            ("nested/override.ps1", raw.as_slice()),
        ] {
            std::fs::write(workdir.join(path), raw).unwrap();
            let hash = hash_clean_blob_beneath(
                &workdir,
                &root,
                Path::new(path),
                &context,
                Some(42),
                || Ok(false),
            )
            .unwrap();
            assert_eq!(
                hash,
                git_internal::internal::object::blob::Blob::from_content_bytes(expected.to_vec())
                    .id,
                "{path}"
            );
        }
        std::fs::write(&global, "*.cmd -text\n").unwrap();
        let hash = hash_clean_blob_beneath(
            &workdir,
            &root,
            Path::new("global.cmd"),
            &context,
            Some(42),
            || Ok(false),
        )
        .unwrap();
        assert_eq!(
            hash,
            git_internal::internal::object::blob::Blob::from_content_bytes(raw.to_vec()).id
        );
    }
    #[test]
    fn auto_binary_heuristic_matches_fixed_git_boundaries() {
        for (bytes, binary) in [
            (b"text\r\n".as_slice(), false),
            (b"\0text\r\n", true),
            (b"text\r\nlast\r", true),
            (b"a\rb\r\n", true),
            (b"\x01text\r\n", true),
            (b"\x7ftext\r\n", true),
            (b"\x08\t\x0c\x1b\xff\r\n", false),
            (b"text\r\n\x1a", false),
            (b"text\x1a\r\n", true),
            (b"\x1a\x1a\r\n\x1a", true),
        ] {
            assert_eq!(
                TextStats::from_bytes(bytes).is_binary(),
                binary,
                "{bytes:?}"
            );
        }
        for printable in [127, 128, 255, 256] {
            let mut bytes = vec![b'a'; printable];
            bytes.extend_from_slice(b"\x01\r\n");
            assert_eq!(TextStats::from_bytes(&bytes).is_binary(), printable < 128);
            bytes.extend_from_slice(b"\x02");
            assert_eq!(TextStats::from_bytes(&bytes).is_binary(), printable < 256);
        }
    }

    #[test]
    fn auto_index_guard_is_lazy_and_renormalize_keeps_binary_protection() {
        for mode in [CleanMode::Normal, CleanMode::Renormalize] {
            for (policy, raw, expected, reads) in [
                (
                    CleanPolicy::Preserve,
                    b"text\r\n".as_slice(),
                    ResolvedPolicy::Preserve,
                    0,
                ),
                (CleanPolicy::Text, b"\0text\r\n", ResolvedPolicy::Text, 0),
                (
                    CleanPolicy::Auto,
                    b"\0text\r\n",
                    ResolvedPolicy::Preserve,
                    0,
                ),
                (CleanPolicy::Auto, b"text\n", ResolvedPolicy::Preserve, 0),
                (
                    CleanPolicy::Auto,
                    b"text\r\n",
                    if mode == CleanMode::Normal {
                        ResolvedPolicy::Preserve
                    } else {
                        ResolvedPolicy::Text
                    },
                    usize::from(mode == CleanMode::Normal),
                ),
            ] {
                let mut observed = 0;
                let resolved = resolve_policy(policy, &TextStats::from_bytes(raw), mode, || {
                    observed += 1;
                    Ok(true)
                })
                .unwrap();
                assert_eq!(resolved, expected, "{mode:?}/{policy:?}");
                assert_eq!(observed, reads);
            }
        }
        let error = resolve_policy(
            CleanPolicy::Auto,
            &TextStats::from_bytes(b"text\r\n"),
            CleanMode::Normal,
            || Err(io::Error::other("missing index evidence")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing index evidence"));
        assert!(!has_text_crlf(b"\0text\r\n"));
        assert!(!has_text_crlf(b"text\n"));
        assert!(has_text_crlf(b"text\r\n"));
    }

    #[test]
    fn auto_streaming_cross_chunk_crlf_and_final_sub_in_both_formats() {
        use git_internal::{
            hash::{HashKind, set_hash_kind_for_test},
            internal::object::blob::Blob,
        };
        for kind in [HashKind::Sha1, HashKind::Sha256] {
            let _kind = set_hash_kind_for_test(kind);
            for length in [65534, 65535, 65536, 131071] {
                let mut raw = vec![b'x'; length];
                raw.extend_from_slice(b"\r\nlast\r\n\x1a");
                let mut clean = vec![b'x'; length];
                clean.extend_from_slice(b"\nlast\n\x1a");
                for historical_crlf in [false, true] {
                    let mut file = tempfile::tempfile().unwrap();
                    std::io::Write::write_all(&mut file, &raw).unwrap();
                    file.rewind().unwrap();
                    let actual =
                        hash_clean_file_handle(file, CleanPolicy::Auto, || Ok(historical_crlf))
                            .unwrap();
                    let expected = if historical_crlf { &raw } else { &clean };
                    assert_eq!(
                        actual,
                        Blob::from_content_bytes(expected.clone()).id,
                        "{kind:?}/{length}/{historical_crlf}"
                    );
                }
                raw.push(b'\r');
                let mut file = tempfile::tempfile().unwrap();
                std::io::Write::write_all(&mut file, &raw).unwrap();
                file.rewind().unwrap();
                let actual = hash_clean_file_handle(file, CleanPolicy::Auto, || {
                    panic!("binary must not read index")
                })
                .unwrap();
                assert_eq!(actual, Blob::from_content_bytes(raw).id);
            }
        }
    }
}
