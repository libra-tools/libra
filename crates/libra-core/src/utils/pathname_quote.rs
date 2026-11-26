//! Git-compatible pathname quoting primitives sunk from `command::status::output`.

use std::path::Path;

/// Quote a pathname according to Git's `core.quotePath` rules.
#[doc(hidden)]
pub fn quote_pathname(path: &Path, quote_path: bool) -> String {
    let bytes = quote_pathname_bytes(path, quote_path);
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            String::from_utf8(quote_pathname_bytes(path, true)).unwrap_or_else(|error| {
                // INVARIANT: the quote_path=true form octal-escapes every
                // byte >= 0x80, so it is pure ASCII and always valid UTF-8.
                String::from_utf8_lossy(error.as_bytes()).into_owned()
            })
        }
    }
}

/// Byte-faithful variant of [`quote_pathname`] for byte-oriented writers.
#[doc(hidden)]
pub fn quote_pathname_bytes(path: &Path, quote_path: bool) -> Vec<u8> {
    #[cfg(unix)]
    let bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let display = path.display().to_string();
    #[cfg(not(unix))]
    let bytes: &[u8] = display.as_bytes();
    let needs_escape =
        |b: u8| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || (quote_path && b >= 0x80);
    if !bytes.iter().copied().any(needs_escape) {
        return bytes.to_vec();
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 8);
    out.push(b'"');
    for &b in bytes {
        match b {
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ if b < 0x20 || b == 0x7f || (quote_path && b >= 0x80) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
    out
}
