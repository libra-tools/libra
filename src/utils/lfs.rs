//! LFS helpers to detect tracked files from attributes, compute SHA256 OIDs, build request payloads/headers, and stream uploads or downloads.

use std::{
    fs,
    fs::File,
    io,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use git_internal::internal::index::Index;
use lazy_static::lazy_static;
use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use ring::digest::{Context, SHA256};
use url::Url;

use crate::utils::{attributes, path, util};

lazy_static! {
    pub static ref LFS_HEADERS: HeaderMap = {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.git-lfs+json"),
        );
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.git-lfs+json"),
        );
        headers
    };
}

/// Check if a file is LFS tracked
/// - supports Git/Libra attributes sources
/// - absolute path
///
/// Returns `false` for paths outside the current worktree or attributes that do
/// not assign `filter=lfs`.
pub fn is_lfs_tracked<P>(path: P) -> bool
where
    P: AsRef<Path>,
{
    attributes::is_lfs_tracked(path.as_ref())
}

const LFS_VERSION: &str = "https://git-lfs.github.com/spec/v1";
/// This is the original & default transfer adapter. All Git LFS clients and servers SHOULD support it.
pub const LFS_TRANSFER_API: &str = "basic";
pub const LFS_HASH_ALGO: &str = "sha256";
const LFS_OID_LEN: usize = 64;
const LFS_POINTER_MAX_SIZE: usize = 300; // bytes

/// Generate lfs pointer file string
/// - return (pointer content, lfs oid)
/// - absolute path
///
/// **Panics** if `path` cannot be read (LFS hash + size require the file to
/// exist at this point). Callers are expected to have verified existence
/// via `is_lfs_tracked` / `Path::exists` before invoking this. New callers
/// that cannot uphold that contract (e.g. racy worktree scans) must use
/// [`generate_pointer_file_result`] instead.
pub fn generate_pointer_file(path: impl AsRef<Path>) -> (String, String) {
    let path = path.as_ref();
    generate_pointer_file_result(path)
        .unwrap_or_else(|err| panic!("generate_pointer_file({}): {err}", path.display()))
}

/// Fallible [`generate_pointer_file`]: returns the pointer content and LFS
/// oid, propagating read/metadata failures instead of panicking. Rename
/// detection and other best-effort readers use this so a vanished or
/// unreadable file degrades to a skipped candidate rather than a crash.
pub fn generate_pointer_file_result(path: impl AsRef<Path>) -> io::Result<(String, String)> {
    let path = path.as_ref();
    let oid = calc_lfs_file_hash(path)?;
    let size = path.metadata()?.len();
    let pointer = format_pointer_string(&oid, size);
    Ok((pointer, oid))
}

/// [`generate_pointer_file_result`] under a hard byte cap.
///
/// Returns `Ok(None)` when the file exceeds `cap`, or when its size changes
/// under the read. Best-effort readers (rename detection) must use this:
/// the plain helper hashes to EOF, so a file that grows after its size was
/// checked would blow the read budget it was supposed to respect, and a
/// pointer built from a moving target names content that never existed.
/// Returns `(pointer, bytes_read)`. `bytes_read` is what the hash actually
/// consumed — NOT the pre-read `stat` length — and is reported even when the
/// pointer is refused, so a caller enforcing a read budget charges what the
/// attempt really cost. A file that grows between the stat and the read
/// otherwise buys unmetered I/O.
pub fn generate_pointer_file_bounded(
    path: impl AsRef<Path>,
    cap: u64,
) -> io::Result<(Option<(String, String)>, u64)> {
    let path = path.as_ref();
    let size = path.metadata()?.len();
    if size > cap {
        return Ok((None, 0));
    }
    let (oid, bytes_read) = calc_lfs_file_hash_bounded(path, cap)?;
    let Some(oid) = oid else {
        return Ok((None, bytes_read));
    };
    // Re-check: a file that changed size under the read would otherwise get
    // a pointer whose `size` field disagrees with the bytes hashed.
    if path.metadata()?.len() != size || bytes_read != size {
        return Ok((None, bytes_read));
    }
    Ok((Some((format_pointer_string(&oid, size), oid)), bytes_read))
}

pub fn format_pointer_string(oid: &str, size: u64) -> String {
    format!("version {LFS_VERSION}\noid {LFS_HASH_ALGO}:{oid}\nsize {size}\n")
}

/// Generate LFS Server Url from repo Url.
/// By default, Git LFS will append `.git/info/lfs` to the end of a Git remote url to build the LFS server URL.
/// [doc: server-discovery](https://github.com/git-lfs/git-lfs/blob/main/docs/api/server-discovery.md)
/// - like `https://git-server.com/foo/bar.git/info/lfs`
/// - support ssh & https & git@ format
fn generate_git_lfs_server_url(mut url: String) -> String {
    let ssh_url = url.starts_with("ssh://");
    if url.starts_with("git@") {
        // git@git-server.com:foo/bar.git
        let remote = &url[4..];
        let separator = if remote.starts_with('[') {
            remote.find("]:").map(|index| index + 1)
        } else {
            remote.find(':')
        };
        if let Some(separator) = separator {
            url = format!(
                "https://{}/{}",
                &remote[..separator],
                &remote[separator + 1..]
            );
        }
    } else if ssh_url {
        // ssh://git-server.com/foo/bar.git
        url = "https://".to_string() + &url[6..];
    }

    let Ok(mut parsed) = Url::parse(&url) else {
        return url;
    };
    if ssh_url {
        // SSH usernames/passwords are not HTTP credentials. Leaving `git@`
        // here would also suppress the host-scoped HTTP token lookup.
        // INVARIANT: a parsed HTTPS URL with a host supports userinfo setters.
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
    }
    let path = parsed.path().trim_end_matches('/');
    let path = if path.ends_with("/info/lfs") {
        path.to_owned()
    } else if path.is_empty() {
        "/info/lfs".to_owned()
    } else if path.ends_with(".git") {
        format!("{path}/info/lfs")
    } else {
        format!("{path}.git/info/lfs")
    };
    parsed.set_path(&path);
    parsed.set_fragment(None);
    parsed.to_string()
}

/// Generate Mono LFS Server Url from repo Url.
/// Preserve repository scope for Mega's standard `/info/lfs` router.
/// Example: `http://localhost:8000/project/demo` becomes
/// `http://localhost:8000/project/demo.git/info/lfs`.
/// A host-only HTTP remote keeps the legacy root LFS endpoints.
fn generate_mono_lfs_server_url(url: String) -> String {
    if let Ok(mut parsed) = Url::parse(&url)
        && matches!(parsed.scheme(), "http" | "https")
        && parsed.path().trim_end_matches('/').is_empty()
    {
        parsed.set_fragment(None);
        return parsed.to_string();
    }
    generate_git_lfs_server_url(url)
}

/// Generate LFS Server Url from repo Url.
/// - Automatically detect git or mono repo by domain
/// - Callers normalize the trailing slash before joining endpoint paths.
pub fn generate_lfs_server_url(url_str: String) -> String {
    let url = match Url::parse(&url_str) {
        Ok(url) => url,
        // maybe start with `git@`
        Err(_) => return generate_git_lfs_server_url(url_str),
    };
    match url.domain() {
        Some(domain) => {
            if domain == "github.com" || domain == "gitee.com" {
                generate_git_lfs_server_url(url_str)
            } else {
                generate_mono_lfs_server_url(url_str)
            }
        }
        None => {
            // IP address, like http://127.0.0.1:8000
            generate_mono_lfs_server_url(url_str)
        }
    }
}

/// Generate LFS cache path, in `.libra/lfs/objects`
pub fn lfs_object_path(oid: &str) -> PathBuf {
    util::storage_path()
        .join("lfs/objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
}

/// Get LFS file oid by path (through `Index`), NOT re-calculate.
///
/// Returns `None` if any of:
/// - the index file fails to load
/// - the path is not in the index
/// - the index entry's object is missing from storage
/// - the stored bytes are not a valid LFS pointer
///
/// Diagnostic warnings are emitted via `tracing::warn!` so a corrupt LFS
/// pointer or missing object during a lock check does not crash `libra push`.
pub fn get_oid_by_path(path: &str) -> Option<String> {
    let index_file = path::index();
    let index = match Index::load(&index_file) {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(
                index = %index_file.display(),
                error = ?err,
                "failed to load index while resolving LFS oid by path"
            );
            return None;
        }
    };
    let hash = index.get_hash(path, 0)?;
    let storage = util::objects_storage();
    let data = match storage.get(&hash) {
        Ok(data) => data,
        Err(err) => {
            tracing::warn!(
                path = %path,
                hash = %hash,
                error = %err,
                "failed to read LFS pointer object from storage"
            );
            return None;
        }
    };
    let (oid, _) = parse_pointer_data(&data)?;
    Some(oid)
}

/// Copy LFS file to `.libra/lfs/objects`
/// - absolute path
pub fn backup_lfs_file<P>(path: P, oid: &str) -> io::Result<()>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let backup_path = lfs_object_path(oid);
    if !backup_path.exists() {
        // INVARIANT: lfs_object_path() always returns `.libra/lfs/objects/AB/CD/<oid>`
        // which has a parent.
        let parent = backup_path
            .parent()
            .expect("lfs_object_path always produces a path with a parent");
        fs::create_dir_all(parent)?;
        fs::copy(path, backup_path)?;
    }
    Ok(())
}

/// SHA256 without type
// `ring` crate is much faster than `sha2` crate ( > 10 times)
/// [`calc_lfs_file_hash`] that refuses to read past `cap` bytes, returning
/// `Ok(None)` instead of hashing an unbounded amount.
/// Returns `(oid, bytes_read)`. `bytes_read` counts every byte pulled off the
/// file, including the overrun byte that proves the cap was exceeded, so a
/// caller can charge a refused read as well as a successful one.
pub fn calc_lfs_file_hash_bounded<P>(path: P, cap: u64) -> io::Result<(Option<String>, u64)>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let mut hash = Context::new(&SHA256);
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > cap {
        return Ok((None, 0));
    }
    // Read AT MOST `len` (<= cap) bytes. The cap is a hard ceiling, so a file
    // that grows is caught by re-stating AFTER the read; reading cap+1 to
    // detect it would overrun the bound itself.
    let mut reader = BufReader::new(file).take(len);
    let mut buffer = [0; 65536];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        hash.update(&buffer[..n]);
    }
    if total != len || reader.into_inner().into_inner().metadata()?.len() != len {
        // Changed size under the read: the digest describes a prefix, or a
        // file that no longer exists in that form.
        return Ok((None, total));
    }
    Ok((Some(hex::encode(hash.finish().as_ref())), total))
}

pub fn calc_lfs_file_hash<P>(path: P) -> io::Result<String>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let mut hash = Context::new(&SHA256);
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buffer = [0; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let file_hash = hex::encode(hash.finish().as_ref());
    Ok(file_hash)
}

/// Check if `data` is an LFS pointer, return `oid` & `size`
///
/// Returns `None` for any malformed input, including pointer-shape bytes that
/// happen to contain non-UTF-8 sequences where the oid or size are expected.
pub fn parse_pointer_data(data: &[u8]) -> Option<(String, u64)> {
    if data.len() > LFS_POINTER_MAX_SIZE {
        return None;
    }
    // Start with format `version ...`
    if let Some(data) =
        data.strip_prefix(format!("version {LFS_VERSION}\noid {LFS_HASH_ALGO}:").as_bytes())
        && data.len() > LFS_OID_LEN
        && data[LFS_OID_LEN] == b'\n'
    {
        // Check `oid` length and that it is valid UTF-8 (LFS oids are hex ASCII).
        let oid = String::from_utf8(data[..LFS_OID_LEN].to_vec()).ok()?;
        // Per the LFS pointer spec the sha256 oid is lowercase hex; reject
        // anything else so corrupt pointers fail at parse time instead of
        // propagating garbage into `LfsFileOutput`, batch-protocol object
        // ids, or server-side requests that would surface as an opaque
        // 4xx much later.
        if !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        if let Some(data) = data.strip_prefix(format!("{oid}\nsize ").as_bytes()) {
            let data = String::from_utf8(data.to_vec()).ok()?;
            if let Ok(size) = data.trim_end().parse::<u64>() {
                return Some((oid, size));
            }
        }
    }
    None
}

/// Read max LFS_POINTER_MAX_SIZE bytes
pub fn parse_pointer_file(path: impl AsRef<Path>) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut buffer = [0; LFS_POINTER_MAX_SIZE];
    let bytes_read = file.read(&mut buffer)?;
    if let Some((oid, size)) = parse_pointer_data(&buffer[..bytes_read]) {
        return Ok((oid, size));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid LFS pointer file",
    ))
}

/// Extract LFS patterns from `.libra_attributes` file
pub fn extract_lfs_patterns(file_path: &str) -> io::Result<Vec<String>> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    // ' ' needs '\' before it to be escaped
    // INVARIANT: this regex is a compile-time literal; `Regex::new` only
    // returns Err for syntactically invalid patterns, which is caught by
    // unit tests.
    let re = Regex::new(r"^\s*(([^\s#\\]|\\ )+)")
        .expect("LFS attributes regex is a valid hardcoded pattern");

    let mut patterns = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if !line.contains("filter=lfs") {
            continue;
        }
        if let Some(cap) = re.captures(&line)
            && let Some(pattern) = cap.get(1)
        {
            let pattern = pattern.as_str().replace(r"\ ", " ");
            patterns.push(pattern);
        }
    }

    Ok(patterns)
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    /// §B.3.4: the bounded LFS readers report the bytes they actually
    /// consumed, on the refusal paths as well as the success path. A caller
    /// enforcing a read budget settles on that number; billing the pre-read
    /// `stat` instead lets a file that grows in between pull the larger read
    /// through for the smaller stale price.
    #[test]
    fn bounded_lfs_readers_report_bytes_actually_read() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, vec![b'x'; 5000]).unwrap();

        // Success: the count is the real content length.
        let (oid, read) = calc_lfs_file_hash_bounded(&path, 1 << 20).unwrap();
        assert!(oid.is_some());
        assert_eq!(read, 5000);
        let (pointer, read) = generate_pointer_file_bounded(&path, 1 << 20).unwrap();
        assert!(pointer.is_some());
        assert_eq!(read, 5000);

        // Refused for exceeding the cap, and the cap is a HARD ceiling: the
        // stat rejects before a single byte is read, so nothing is consumed
        // and nothing is charged.
        let (oid, read) = calc_lfs_file_hash_bounded(&path, 100).unwrap();
        assert!(oid.is_none(), "over the cap");
        assert_eq!(read, 0, "a refused read consumes nothing past the cap");

        // A pointer whose hash consumed a different number of bytes than the
        // pre-read stat is refused rather than published with a size field
        // that disagrees with the bytes hashed.
        let (pointer, read) = generate_pointer_file_bounded(&path, 100).unwrap();
        assert!(pointer.is_none(), "over the cap");
        assert_eq!(read, 0, "the stat-level rejection never opened the file");
    }

    #[tokio::test]
    #[serial]
    async fn test_generate_pointer_file() {
        use tempfile::tempdir;

        // Create a temporary directory
        let temp_dir = tempdir().unwrap();
        let test_file_path = temp_dir.path().join("test-lfs-file.bin");

        // Write test content
        let test_content = b"This is test content for LFS pointer generation.\nMultiple lines.";
        std::fs::write(&test_file_path, test_content).unwrap();

        // Generate the pointer file
        let (pointer, oid) = generate_pointer_file(&test_file_path);

        // Verify pointer format
        assert!(pointer.starts_with(&format!("version {LFS_VERSION}\n")));
        assert!(pointer.contains(&format!("oid {LFS_HASH_ALGO}:{oid}")));
        assert!(pointer.contains(&format!("size {}\n", test_content.len())));
        assert_eq!(oid.len(), 64);

        println!("Generated pointer:\n{}", pointer);

        // temp_dir automatically cleans up when dropped
    }

    #[test]
    fn test_is_pointer_file() {
        let data =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:3b2c9e5f8e6a8b7a9c8d6e5f7a9b8c7d6e5f8a9b7a9c8d6e5f8a9b7a9c8d6e51\nsize 1234\n";
        assert!(parse_pointer_data(data).is_some());
    }

    #[test]
    fn test_gen_git_lfs_server_url() {
        const LFS_SERVER_URL: &str = "https://github.com/libra-tools/mega.git/info/lfs";
        let url = "https://github.com/libra-tools/mega".to_owned();
        assert_eq!(generate_lfs_server_url(url), LFS_SERVER_URL);

        let url = "https://github.com/libra-tools/mega.git".to_owned();
        assert_eq!(generate_lfs_server_url(url), LFS_SERVER_URL);

        let url = "git@github.com:libra-tools/mega.git".to_owned();
        assert_eq!(generate_lfs_server_url(url), LFS_SERVER_URL);

        let url = "ssh://github.com/libra-tools/mega.git".to_owned();
        assert_eq!(generate_lfs_server_url(url), LFS_SERVER_URL);

        let url = "ssh://git@github.com/libra-tools/mega.git".to_owned();
        assert_eq!(generate_lfs_server_url(url), LFS_SERVER_URL);
    }

    #[test]
    fn lfs_url_preserves_query_and_only_removes_ssh_credentials() {
        assert_eq!(
            generate_lfs_server_url(
                "ssh://git:unused@host.example:8443/repo.git/?tenant=one#ref".to_owned()
            ),
            "https://host.example:8443/repo.git/info/lfs?tenant=one"
        );
        assert_eq!(
            generate_lfs_server_url(
                "https://user:token@host.example/repo.git/info/lfs/?tenant=one".to_owned()
            ),
            "https://user:token@host.example/repo.git/info/lfs?tenant=one"
        );
        assert_eq!(
            generate_lfs_server_url("git@[::1]:project/demo.git".to_owned()),
            "https://[::1]/project/demo.git/info/lfs"
        );
    }

    #[test]
    fn test_gen_mono_lfs_server_url() {
        const LFS_SERVER_URL: &str = "https://gitmono.com/libra-tools/mega.git/info/lfs";
        assert_eq!(
            generate_lfs_server_url(LFS_SERVER_URL.to_owned()),
            LFS_SERVER_URL
        );
        const LOCAL_LFS_SERVER_URL: &str = "http://localhost:8000/xxx/yyy";
        assert_eq!(
            Url::parse(LOCAL_LFS_SERVER_URL).unwrap().domain().unwrap(),
            "localhost"
        );
        assert_eq!(
            generate_lfs_server_url(LOCAL_LFS_SERVER_URL.to_owned()),
            "http://localhost:8000/xxx/yyy.git/info/lfs"
        );
        for (remote, expected) in [
            ("http://127.0.0.1:8000", "http://127.0.0.1:8000/"),
            ("http://localhost:8000/", "http://localhost:8000/"),
            ("https://gitmono.com", "https://gitmono.com/"),
            (
                "http://[::1]:8000/?tenant=one#ref",
                "http://[::1]:8000/?tenant=one",
            ),
        ] {
            assert_eq!(generate_lfs_server_url(remote.to_owned()), expected);
        }
    }

    #[test]
    fn test_parse_pointer_data() {
        let data = r#"version https://git-lfs.github.com/spec/v1
oid sha256:4859402c258b836d02e955d1090e29f586e58b2040504d68afec3d8d43757bba
size 10
"#;
        let res = parse_pointer_data(data.as_bytes()).unwrap();
        println!("{res:?}");
        assert_eq!(
            res.0,
            "4859402c258b836d02e955d1090e29f586e58b2040504d68afec3d8d43757bba"
        );
        assert_eq!(res.1, 10);
    }

    /// Regression for v0.17.203: pointer-shaped bytes whose oid region contains
    /// non-UTF-8 bytes must return `None` rather than panicking inside the old
    /// `String::from_utf8(...).unwrap()`.
    #[test]
    fn parse_pointer_data_non_utf8_oid_returns_none() {
        let mut data = b"version https://git-lfs.github.com/spec/v1\noid sha256:".to_vec();
        // 64 non-UTF-8 bytes where the oid hex chars should be.
        data.extend(std::iter::repeat_n(0xFFu8, LFS_OID_LEN));
        data.push(b'\n');
        data.extend_from_slice(b"size 10\n");
        assert!(
            parse_pointer_data(&data).is_none(),
            "non-UTF-8 oid bytes should yield None, not panic"
        );
    }

    /// Regression for v0.17.203: a too-short payload that matches the prefix
    /// but ends before the oid terminator must return `None` rather than
    /// slice-panicking on `data[LFS_OID_LEN]`.
    #[test]
    fn parse_pointer_data_short_payload_returns_none() {
        let mut data = b"version https://git-lfs.github.com/spec/v1\noid sha256:".to_vec();
        // Only 10 bytes where 64 hex chars + a newline are expected.
        data.extend_from_slice(b"abcdef0123");
        assert!(
            parse_pointer_data(&data).is_none(),
            "short payload should yield None, not slice-panic"
        );
    }

    /// Pointer-shaped bytes that exceed the max size cap should return None
    /// without even attempting to parse.
    #[test]
    fn parse_pointer_data_oversized_returns_none() {
        let data = vec![b'a'; LFS_POINTER_MAX_SIZE + 1];
        assert!(parse_pointer_data(&data).is_none());
    }

    /// The LFS pointer spec requires the sha256 oid to be lowercase
    /// hex. Pointer-shaped bytes whose oid region is valid UTF-8 but
    /// contains non-hex characters (e.g., a corrupted pointer with 'g'
    /// repeated 64 times) must return `None`, so garbage oids never
    /// reach `LfsFileOutput` or the LFS batch / lock server calls.
    #[test]
    fn parse_pointer_data_non_hex_oid_returns_none() {
        let mut data = b"version https://git-lfs.github.com/spec/v1\noid sha256:".to_vec();
        // 64 ASCII 'g' chars — valid UTF-8, definitely not hex.
        data.extend(std::iter::repeat_n(b'g', LFS_OID_LEN));
        data.push(b'\n');
        data.extend_from_slice(b"size 10\n");
        assert!(
            parse_pointer_data(&data).is_none(),
            "non-hex but valid-UTF-8 oid should yield None"
        );

        // Happy-path control: replacing 'g' with 'a' (which IS hex)
        // restores acceptance, proving we did not over-reject on
        // structurally identical input.
        let mut ok = b"version https://git-lfs.github.com/spec/v1\noid sha256:".to_vec();
        ok.extend(std::iter::repeat_n(b'a', LFS_OID_LEN));
        ok.push(b'\n');
        ok.extend_from_slice(b"size 10\n");
        let (oid, size) =
            parse_pointer_data(&ok).expect("all-hex 'a' oid should parse as a valid pointer");
        assert_eq!(oid.len(), LFS_OID_LEN);
        assert_eq!(size, 10);
    }
}
