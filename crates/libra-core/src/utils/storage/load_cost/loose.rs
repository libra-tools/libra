use std::{fs, io::Read, path::Path};

use flate2::read::ZlibDecoder;
use git_internal::{errors::GitError, internal::object::types::ObjectType};

const MAX_HEADER_BYTES: usize = 64;

/// Validate a loose object in constant memory and return the bytes that its
/// payload would require if loaded.
pub(super) fn load_cost(path: &Path) -> Result<u64, GitError> {
    let (mut decoder, _, declared, storage_len) = open_and_read_header(path)?;
    if declared > crate::utils::preview_object::MAX_OBJECT_BYTES {
        return Err(preview_limit_error(path, declared));
    }
    consume_exact_payload(&mut decoder, declared)?;
    validate_stream_end(&mut decoder, storage_len, path)?;
    Ok(declared)
}

/// Validate a loose object in constant memory and return its decoded payload
/// length. Objects whose declared length exceeds [`MAX_OBJECT_BYTES`] return
/// `Ok(None)` before decoding so bounded batch probes neither spin on oversized
/// streams nor discard valid sizes in the rest of the batch.
pub(super) fn decoded_size(path: &Path) -> Result<Option<u64>, GitError> {
    let (mut decoder, _, declared, storage_len) = open_and_read_header(path)?;
    if declared > crate::utils::preview_object::MAX_OBJECT_BYTES {
        return Ok(None);
    }
    consume_exact_payload(&mut decoder, declared)?;
    validate_stream_end(&mut decoder, storage_len, path)?;
    Ok(Some(declared))
}

/// Read one loose object without first buffering its compressed representation.
/// When `max_payload` is present, reject the declared size before allocating.
pub(super) fn read(
    path: &Path,
    max_payload: Option<u64>,
) -> Result<(Vec<u8>, ObjectType), GitError> {
    let (mut decoder, object_type, declared, storage_len) = open_and_read_header(path)?;
    if let Some(limit) = max_payload
        && declared > limit
    {
        return Err(invalid(format!(
            "loose object payload of {declared} bytes at {} exceeds preview limit of {limit} bytes",
            path.display()
        )));
    }

    let declared_usize = usize::try_from(declared).map_err(|error| {
        invalid(format!(
            "loose object payload at {} is too large for this platform: {error}",
            path.display()
        ))
    })?;
    let mut payload = Vec::new();
    payload.try_reserve_exact(declared_usize).map_err(|error| {
        invalid(format!(
            "cannot reserve {declared} bytes for loose object at {}: {error}",
            path.display()
        ))
    })?;
    payload.resize(declared_usize, 0);
    decoder.read_exact(&mut payload).map_err(|error| {
        invalid(format!(
            "loose object at {} ended before its declared {declared}-byte payload: {error}",
            path.display()
        ))
    })?;
    validate_stream_end(&mut decoder, storage_len, path)?;
    Ok((payload, object_type))
}

fn open_and_read_header(
    path: &Path,
) -> Result<(ZlibDecoder<fs::File>, ObjectType, u64, u64), GitError> {
    let file = open_regular_loose_object(path)?;
    let storage_len = file.metadata()?.len();
    let mut decoder = ZlibDecoder::new(file);
    let mut header = Vec::with_capacity(MAX_HEADER_BYTES);
    loop {
        if header.len() == MAX_HEADER_BYTES {
            return Err(invalid(format!(
                "loose object header at {} exceeds {MAX_HEADER_BYTES} bytes",
                path.display()
            )));
        }
        let mut byte = [0u8; 1];
        let read = decoder.read(&mut byte).map_err(|error| {
            invalid(format!(
                "cannot decompress loose object header at {}: {error}",
                path.display()
            ))
        })?;
        if read == 0 {
            return Err(invalid(format!(
                "loose object at {} has no header terminator",
                path.display()
            )));
        }
        if byte[0] == 0 {
            break;
        }
        header.push(byte[0]);
    }

    let header = std::str::from_utf8(&header).map_err(|error| {
        invalid(format!(
            "loose object at {} has a non-UTF-8 header: {error}",
            path.display()
        ))
    })?;
    let (kind, size) = header.split_once(' ').ok_or_else(|| {
        invalid(format!(
            "loose object at {} has an invalid header",
            path.display()
        ))
    })?;
    if kind.is_empty() || size.is_empty() || size.contains(' ') {
        return Err(invalid(format!(
            "loose object at {} has an invalid header",
            path.display()
        )));
    }
    let object_type = ObjectType::from_string(kind)?;
    let declared = size.parse::<u64>().map_err(|error| {
        invalid(format!(
            "loose object at {} has invalid payload size '{size}': {error}",
            path.display()
        ))
    })?;
    Ok((decoder, object_type, declared, storage_len))
}

#[cfg(unix)]
fn open_regular_loose_object(path: &Path) -> Result<fs::File, GitError> {
    use std::os::unix::fs::OpenOptionsExt;

    // O_NONBLOCK prevents a corrupt loose-object FIFO/device from parking a
    // Tokio blocking worker forever before the traversal deadline can fire;
    // O_NOFOLLOW rejects a symlink at the content-addressed final component.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid(format!(
            "loose object at {} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_loose_object(path: &Path) -> Result<fs::File, GitError> {
    let file = fs::File::open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(invalid(format!(
            "loose object at {} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

fn consume_exact_payload(
    decoder: &mut ZlibDecoder<fs::File>,
    declared: u64,
) -> Result<(), GitError> {
    let mut remaining = declared;
    let mut buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|error| invalid(format!("loose object read size is invalid: {error}")))?;
        let read = decoder
            .read(&mut buffer[..wanted])
            .map_err(|error| invalid(format!("cannot decompress loose object payload: {error}")))?;
        if read == 0 {
            return Err(invalid(format!(
                "loose object ended before its declared {declared}-byte payload"
            )));
        }
        remaining -= read as u64;
    }
    Ok(())
}

fn validate_stream_end(
    decoder: &mut ZlibDecoder<fs::File>,
    storage_len: u64,
    path: &Path,
) -> Result<(), GitError> {
    let mut extra = [0u8; 1];
    let read = decoder.read(&mut extra).map_err(|error| {
        invalid(format!(
            "cannot finish decompressing loose object at {}: {error}",
            path.display()
        ))
    })?;
    if read != 0 {
        return Err(invalid(format!(
            "loose object at {} contains data beyond its declared payload",
            path.display()
        )));
    }
    if decoder.total_in() != storage_len {
        return Err(invalid(format!(
            "loose object at {} contains {} trailing compressed bytes",
            path.display(),
            storage_len.saturating_sub(decoder.total_in())
        )));
    }
    Ok(())
}

fn invalid(message: String) -> GitError {
    GitError::InvalidObjectInfo(message)
}

fn preview_limit_error(path: &Path, declared: u64) -> GitError {
    invalid(format!(
        "loose object payload of {declared} bytes at {} exceeds preview limit of {} bytes",
        path.display(),
        crate::utils::preview_object::MAX_OBJECT_BYTES
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::ZlibEncoder};

    use super::*;

    fn write_loose(path: &Path, bytes: &[u8]) {
        let file = std::fs::File::create(path).expect("create loose object");
        let mut encoder = ZlibEncoder::new(file, Compression::default());
        encoder.write_all(bytes).expect("compress loose object");
        encoder.finish().expect("finish loose object");
    }

    #[test]
    fn loose_probe_rejects_declared_size_mismatch() {
        let temp = tempfile::tempdir().expect("create loose fixture");
        let path = temp.path().join("object");
        write_loose(&path, b"blob 1\0payload-that-is-longer");

        assert!(load_cost(&path).is_err());
    }

    #[test]
    fn loose_probe_rejects_trailing_compressed_storage_bytes() {
        let temp = tempfile::tempdir().expect("create loose fixture");
        let path = temp.path().join("object");
        write_loose(&path, b"blob 4\0data");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open loose object for corruption");
        file.write_all(b"trailing-storage-bytes")
            .expect("append trailing storage bytes");

        assert!(load_cost(&path).is_err());
    }

    #[test]
    fn loose_probe_rejects_oversized_declaration_before_decoding_payload() {
        let temp = tempfile::tempdir().expect("create loose fixture");
        let path = temp.path().join("object");
        let declared = crate::utils::preview_object::MAX_OBJECT_BYTES + 1;
        write_loose(&path, format!("blob {declared}\0").as_bytes());

        let error = load_cost(&path).expect_err("oversized preview object must fail closed");
        assert!(
            error.to_string().contains("exceeds preview limit"),
            "the declaration must be rejected before the missing payload is decoded: {error}"
        );
    }

    #[test]
    fn loose_decoded_size_returns_payload_length_and_skips_oversized_declarations() {
        let temp = tempfile::tempdir().expect("create loose fixture");
        let valid_path = temp.path().join("valid");
        write_loose(&valid_path, b"blob 11\0hello world");
        assert_eq!(
            decoded_size(&valid_path).expect("decoded size of valid loose blob"),
            Some(11)
        );

        let oversized_path = temp.path().join("oversized");
        let declared = crate::utils::preview_object::MAX_OBJECT_BYTES + 1;
        write_loose(&oversized_path, format!("blob {declared}\0").as_bytes());
        assert_eq!(
            decoded_size(&oversized_path).expect("oversized loose blob should report None"),
            None
        );

        let corrupt_path = temp.path().join("corrupt");
        write_loose(&corrupt_path, b"blob 1\0payload-that-is-longer");
        assert!(decoded_size(&corrupt_path).is_err());
    }
}
