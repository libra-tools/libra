//! Attribute-directed conversion from worktree bytes to stored blob bytes.
//!
//! Explicit text/eol rules are shared by writers and content comparisons. Index
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
}

/// `-text` wins over `eol`. Automatic conversion is a separate policy and is
/// deliberately not enabled by an accompanying eol value at this stage.
pub(crate) fn explicit_policy(attributes: &BTreeMap<String, AttributeState>) -> CleanPolicy {
    match attributes.get("text") {
        Some(AttributeState::Unset) => return CleanPolicy::Preserve,
        Some(AttributeState::Value(value)) if value == "auto" => return CleanPolicy::Preserve,
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
pub(crate) fn clean_bytes(mut bytes: Vec<u8>, policy: CleanPolicy) -> Vec<u8> {
    if policy == CleanPolicy::Preserve {
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
pub(crate) fn read_clean_blob_bytes(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    read_clean_blob_bytes_inner(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "read worktree blob '{}' for line-ending conversion: {error}; check the file and its attribute sources",
                path.display()
            ),
        )
    })
}

fn read_clean_blob_bytes_inner(path: &Path) -> io::Result<Vec<u8>> {
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
    Ok(clean_bytes(bytes, explicit_policy(&attributes)))
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
) -> io::Result<git_internal::hash::ObjectHash> {
    let before = RawLstat::from_metadata(&file.metadata()?)?;
    let length = if policy == CleanPolicy::Text {
        let mut length = 0u64;
        stream_clean(&mut file, before.len, policy, |bytes| {
            length += bytes.len() as u64;
        })?;
        file.rewind()?;
        length
    } else {
        before.len
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
    policy: CleanPolicy,
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
        pending_cr = policy == CleanPolicy::Text && buffer.last() == Some(&b'\r');
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
        stream_clean(&mut file, before.len, CleanPolicy::Preserve, |bytes| {
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
    hash_clean_file_handle(file, explicit_policy(&states))
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
    fn explicit_policy_matrix_preserves_auto_and_unset() {
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
                CleanPolicy::Preserve,
            ),
            (
                Some(AttributeState::Value("auto".into())),
                Some("crlf"),
                CleanPolicy::Preserve,
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
            assert_eq!(clean_bytes(input.to_vec(), CleanPolicy::Text), *expected);
            assert_eq!(clean_bytes(input.to_vec(), CleanPolicy::Preserve), *input);
        }
    }

    #[test]
    fn explicit_conversion_spans_read_chunk_boundaries() {
        let mut bytes = vec![b'x'; 64 * 1024 - 1];
        bytes.extend_from_slice(b"\r\nlast\r");
        let converted = clean_bytes(bytes, CleanPolicy::Text);
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
        let converted = clean_bytes(bytes, CleanPolicy::Text);
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
                    let hash = hash_clean_file_handle(file.try_clone().unwrap(), policy).unwrap();
                    let expected = git_internal::internal::object::blob::Blob::from_content_bytes(
                        clean_bytes(bytes.clone(), policy),
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
        let hash = hash_clean_file_handle(file, CleanPolicy::Text).unwrap();
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
            let hash = hash_clean_blob_beneath(&workdir, &root, Path::new(path), &context, Some(1))
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
            let hash =
                hash_clean_blob_beneath(&workdir, &root, Path::new(path), &context, Some(42))
                    .unwrap();
            assert_eq!(
                hash,
                git_internal::internal::object::blob::Blob::from_content_bytes(expected.to_vec())
                    .id,
                "{path}"
            );
        }
        std::fs::write(&global, "*.cmd -text\n").unwrap();
        let hash =
            hash_clean_blob_beneath(&workdir, &root, Path::new("global.cmd"), &context, Some(42))
                .unwrap();
        assert_eq!(
            hash,
            git_internal::internal::object::blob::Blob::from_content_bytes(raw.to_vec()).id
        );
    }
}
