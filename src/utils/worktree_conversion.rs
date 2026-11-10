//! Attribute-directed conversion from worktree bytes to stored blob bytes.
//!
//! Explicit text/eol rules are shared by writers and content comparisons. Index
//! objects are never converted, and worktree files are never rewritten here.

use std::{
    collections::BTreeMap,
    io::{self, Read},
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
}
