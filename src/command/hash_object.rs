//! Implements `hash-object` for computing Git-compatible object IDs for blob,
//! commit, tree, and tag content (with optional `--literally` to skip validation).

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
};

const HASH_OBJECT_EXAMPLES: &str = "\
EXAMPLES:
    libra hash-object README.md                         Compute the blob id (no write)
    libra hash-object -w src/main.rs                    Compute and write the object to .libra/objects/
    libra hash-object -t commit payload                 Hash file content as a commit (validated)
    libra hash-object -t tag --literally payload        Hash as a tag without content validation
    printf 'hello' | libra hash-object --stdin          Hash stdin instead of a file
    printf 'hello' | libra hash-object --stdin --path README.md  Label stdin with a path context
    printf 'hello' | libra hash-object --stdin --json   Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = HASH_OBJECT_EXAMPLES)]
pub struct HashObjectArgs {
    /// Actually write the object into the object database
    #[arg(short = 'w', long)]
    pub write: bool,

    /// Read the object contents from standard input
    #[arg(long, conflicts_with = "paths")]
    pub stdin: bool,

    /// Read file paths from standard input (one per line) and hash each
    #[arg(
        long = "stdin-paths",
        conflicts_with_all = ["stdin", "paths", "filter_path"]
    )]
    pub stdin_paths: bool,

    /// Object type to hash: `blob` (default), `commit`, `tree`, or `tag`.
    #[arg(
        short = 't',
        long = "type",
        default_value = "blob",
        value_name = "TYPE"
    )]
    pub object_type: String,

    /// Hash the bytes as the given type WITHOUT verifying that the content is a
    /// well-formed object of that type (Git's `--literally`).
    #[arg(long)]
    pub literally: bool,

    /// File paths to hash
    #[arg(
        value_name = "PATH",
        required_unless_present_any = ["stdin", "stdin_paths"]
    )]
    pub paths: Vec<PathBuf>,

    /// Path context label for compatibility with Git hash-object
    #[arg(long = "path", value_name = "PATH", conflicts_with = "no_filters")]
    pub filter_path: Option<PathBuf>,

    /// Hash raw bytes without path-based content filters
    #[arg(long = "no-filters", conflicts_with = "filter_path")]
    pub no_filters: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HashObjectOutput {
    object_type: String,
    write: bool,
    objects: Vec<HashObjectEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct HashObjectEntry {
    source: String,
    oid: String,
    size: usize,
    written: bool,
}

pub async fn execute(args: HashObjectArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

/// # Side Effects
///
/// With `-w`/`--write`, stores each computed object (of the requested type) in the
/// current repository object database. Without `--write`, this command only reads
/// input and prints Git-compatible object IDs.
///
/// # Errors
///
/// Returns structured CLI errors for unsupported object types, content that is not a
/// well-formed object of the requested type (without `--literally`), unreadable
/// input, object-write failures, and stdout write failures.
pub async fn execute_safe(args: HashObjectArgs, output: &OutputConfig) -> CliResult<()> {
    let object_type = parse_git_object_type(&args.object_type)?;

    if output.is_json() {
        let result = hash_objects(&args, object_type)?;
        return render_hash_object_output(&result, output);
    }

    hash_objects_streaming(&args, object_type, output)
}

/// Parse the `-t`/`--type` value, restricted to the four Git object types. Other
/// `ObjectType` variants (Libra AI-native objects, pack deltas) are intentionally
/// not accepted by the Git-compatible `hash-object` surface.
fn parse_git_object_type(value: &str) -> CliResult<ObjectType> {
    match value {
        "blob" => Ok(ObjectType::Blob),
        "commit" => Ok(ObjectType::Commit),
        "tree" => Ok(ObjectType::Tree),
        "tag" => Ok(ObjectType::Tag),
        other => Err(
            CliError::fatal(format!("unsupported object type '{other}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("hash-object supports blob, commit, tree, and tag."),
        ),
    }
}

fn hash_objects(args: &HashObjectArgs, object_type: ObjectType) -> CliResult<HashObjectOutput> {
    let objects = if args.stdin {
        vec![hash_one_source(
            stdin_source(args),
            read_stdin()?,
            args.write,
            object_type,
            args.literally,
        )?]
    } else {
        let paths = effective_paths(args)?;
        let mut entries = Vec::with_capacity(paths.len());
        for path in &paths {
            entries.push(hash_one_source(
                path.display().to_string(),
                read_file(path)?,
                args.write,
                object_type,
                args.literally,
            )?);
        }
        entries
    };

    Ok(HashObjectOutput {
        object_type: args.object_type.clone(),
        write: args.write,
        objects,
    })
}

fn hash_objects_streaming(
    args: &HashObjectArgs,
    object_type: ObjectType,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.quiet {
        return hash_objects(args, object_type).map(|_| ());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();

    if args.stdin {
        let entry = hash_one_source(
            stdin_source(args),
            read_stdin()?,
            args.write,
            object_type,
            args.literally,
        )?;
        write_hash_line(&mut writer, &entry.oid)?;
        return Ok(());
    }

    for path in &effective_paths(args)? {
        let entry = hash_one_source(
            path.display().to_string(),
            read_file(path)?,
            args.write,
            object_type,
            args.literally,
        )?;
        write_hash_line(&mut writer, &entry.oid)?;
    }

    Ok(())
}

/// The paths to hash: the positional `paths`, or — with `--stdin-paths` — the
/// newline-separated paths read from standard input, each taken verbatim except
/// for the line terminator (blank records become empty paths that error).
fn effective_paths(args: &HashObjectArgs) -> CliResult<Vec<PathBuf>> {
    if args.stdin_paths {
        read_stdin_paths()
    } else {
        Ok(args.paths.clone())
    }
}

fn read_stdin_paths() -> CliResult<Vec<PathBuf>> {
    let data = read_stdin()?;
    let text = String::from_utf8(data).map_err(|_| {
        CliError::fatal("--stdin-paths input is not valid UTF-8")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("provide newline-separated file paths on standard input.")
    })?;
    // Each record is a pathname; like Git, take it verbatim except for the line
    // terminator. `str::lines()` strips only `\n` (and a trailing `\r`), so
    // trailing spaces are preserved and blank records become empty paths that
    // `read_file` then reports as errors — matching `git hash-object`.
    Ok(text.lines().map(PathBuf::from).collect())
}

fn stdin_source(args: &HashObjectArgs) -> String {
    args.filter_path
        .as_ref()
        .map_or_else(|| "-".to_string(), |path| path.display().to_string())
}

fn hash_one_source(
    source: impl Into<String>,
    data: Vec<u8>,
    write: bool,
    object_type: ObjectType,
    literally: bool,
) -> CliResult<HashObjectEntry> {
    let size = data.len();
    // The object id is SHA over the loose-object header `<type> <size>\0<content>`,
    // computed identically for every type (this matches `Blob::id` for blobs).
    let object_hash = ObjectHash::from_type_and_data(object_type, &data);
    let oid = object_hash.to_string();

    // Without `--literally`, verify the content is a well-formed object of the given
    // type (a blob accepts any bytes), matching Git's pre-hash validation.
    if !literally {
        validate_object_content(object_type, &data, &object_hash)?;
    }

    if write {
        // `ClientStorage::put` writes the loose object from the raw payload + type, so
        // it works even for `--literally` content that no typed object would parse.
        util::objects_storage()
            .put(&object_hash, &data, object_type)
            .map_err(|error| {
                CliError::fatal(format!(
                    "failed to write {object_type} object {oid}: {error}"
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
                .with_hint("check repository object storage permissions and available disk space.")
            })?;
    }

    Ok(HashObjectEntry {
        source: source.into(),
        oid,
        size,
        written: write,
    })
}

/// Verify that `data` parses as a well-formed object of `object_type`. A blob accepts
/// any byte sequence; commit/tree/tag are checked structurally.
///
/// Validation is done with dedicated SAFE byte-level parsers, NOT the git-internal
/// `from_bytes` parsers: those contain `unwrap`s and an
/// `unsafe { String::from_utf8_unchecked(..) }` on the commit message, so feeding
/// arbitrary `hash-object` input through them risks a panic or undefined behavior.
/// The checks here never panic and never invoke that unsafe code. Header bytes are
/// parsed as bytes (not UTF-8) so that, like Git, a non-UTF-8 author/tag is accepted
/// while a NUL byte in the header block is rejected; only the ASCII structural tokens
/// are interpreted.
fn validate_object_content(
    object_type: ObjectType,
    data: &[u8],
    object_hash: &ObjectHash,
) -> CliResult<()> {
    // The embedded hash widths follow the object id's algorithm.
    let (raw_hash_len, hex_hash_len) = match object_hash {
        ObjectHash::Sha1(_) => (20usize, 40usize),
        ObjectHash::Sha256(_) => (32usize, 64usize),
        ObjectHash::Blake3(_) => (32usize, 64usize),
    };

    let well_formed = match object_type {
        ObjectType::Blob => true, // any byte sequence is a valid blob
        ObjectType::Commit => is_well_formed_commit(data, hex_hash_len),
        ObjectType::Tree => is_well_formed_tree(data, raw_hash_len),
        ObjectType::Tag => is_well_formed_tag(data, hex_hash_len),
        // parse_git_object_type only yields the four Git types.
        _ => true,
    };

    if well_formed {
        Ok(())
    } else {
        Err(CliError::fatal(format!("invalid {object_type} object"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("pass --literally to hash malformed content without validation."))
    }
}

/// The header block of a commit/tag: bytes up to (and including) the first blank line,
/// or the whole input if there is none. The trailing message may be arbitrary bytes,
/// so only the header block is parsed as text.
fn object_header_block(data: &[u8]) -> &[u8] {
    match data.windows(2).position(|window| window == b"\n\n") {
        Some(index) => &data[..=index],
        None => data,
    }
}

fn is_hex_of_len(value: &[u8], len: usize) -> bool {
    value.len() == len && value.iter().all(|byte| byte.is_ascii_hexdigit())
}

/// Validate a Git ident line value: `Name <email> <unix-ts> <tz>`, matching the
/// boundary `git fsck` enforces — a non-empty name with a space before the email, an
/// `<email>` without angle brackets, a non-zero-padded numeric timestamp, and a
/// `[+-]NNNN` timezone with no trailing tokens.
fn is_valid_ident(value: &[u8]) -> bool {
    let Some(lt) = value.iter().position(|&byte| byte == b'<') else {
        return false;
    };
    // There must be a space immediately before the email. The name itself may be empty
    // (Git accepts `author  <a@b> …`) or non-UTF-8, but the separating space is required
    // (Git rejects `author <a@b> …`, where no space precedes `<`).
    if lt == 0 || value[lt - 1] != b' ' {
        return false;
    }
    let after_lt = &value[lt + 1..];
    let Some(gt_rel) = after_lt.iter().position(|&byte| byte == b'>') else {
        return false;
    };
    let email = &after_lt[..gt_rel];
    if email.contains(&b'<') || email.contains(&b'>') {
        return false;
    }
    let Some(rest) = after_lt[gt_rel + 1..].strip_prefix(b" ") else {
        return false;
    };
    let mut fields = rest.split(|&byte| byte == b' ');
    let Some(timestamp) = fields.next() else {
        return false;
    };
    // Timestamp: all digits, with no leading zero (except the value "0").
    if timestamp.is_empty()
        || !timestamp.iter().all(|byte| byte.is_ascii_digit())
        || (timestamp.len() > 1 && timestamp[0] == b'0')
    {
        return false;
    }
    // Timezone: a sign followed by exactly four digits, and nothing after it.
    let Some(timezone) = fields.next() else {
        return false;
    };
    if fields.next().is_some() {
        return false;
    }
    timezone.len() == 5
        && matches!(timezone[0], b'+' | b'-')
        && timezone[1..].iter().all(|byte| byte.is_ascii_digit())
}

/// Validate a Git tag name as a refname component path (operating on bytes so a
/// non-UTF-8 name is accepted like Git), matching the cases `git hash-object -t tag`
/// rejects (spaces/control chars, `..`, leading `.`, `.lock` suffix, `@{`, `//`,
/// leading/trailing `/`, …).
fn is_valid_tag_name(name: &[u8]) -> bool {
    let contains = |needle: &[u8]| name.windows(needle.len()).any(|window| window == needle);
    // Note: a bare `@` is a VALID tag-object name (Git accepts `tag @`), even though
    // `@{` sequences are rejected — so there is no `name == b"@"` rejection here.
    if name.is_empty()
        || name.first() == Some(&b'/')
        || name.last() == Some(&b'/')
        || name.last() == Some(&b'.')
        || contains(b"//")
        || contains(b"..")
        || contains(b"@{")
    {
        return false;
    }
    name.split(|&byte| byte == b'/').all(|component| {
        !component.is_empty()
            && component.first() != Some(&b'.')
            && !component.ends_with(b".lock")
            && component.iter().all(|&byte| {
                byte > 0x20
                    && byte != 0x7f
                    && !matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
            })
    })
}

/// Strictly validate a commit, enforcing Git's required header ORDER: `tree <hex>`,
/// then zero or more `parent <hex>`, then `author <ident>`, then `committer <ident>`.
/// The message after the blank line may be arbitrary bytes, so only the header block
/// is parsed. Matches the cases `git hash-object -t commit` rejects without
/// `--literally` (parent-after-author, bad ident, …).
fn is_well_formed_commit(data: &[u8], hex_hash_len: usize) -> bool {
    let headers = object_header_block(data);
    // Git rejects a NUL byte ANYWHERE in a commit object (nulInCommit), including the
    // message — not just the header block. The header block must also be
    // newline-terminated — its final header line (including any trailing header like
    // `encoding`) must end in `\n` (Git's unterminatedHeader). A blank line/message is
    // NOT required.
    if data.contains(&0) || !headers.ends_with(b"\n") {
        return false;
    }
    let mut lines = headers.split(|&byte| byte == b'\n').peekable();

    match lines.next().and_then(|line| line.strip_prefix(b"tree ")) {
        Some(tree) if is_hex_of_len(tree, hex_hash_len) => {}
        _ => return false,
    }
    while let Some(parent) = lines.peek().and_then(|line| line.strip_prefix(b"parent ")) {
        if !is_hex_of_len(parent, hex_hash_len) {
            return false;
        }
        lines.next();
    }
    match lines.next().and_then(|line| line.strip_prefix(b"author ")) {
        Some(ident) if is_valid_ident(ident) => {}
        _ => return false,
    }
    match lines
        .next()
        .and_then(|line| line.strip_prefix(b"committer "))
    {
        Some(ident) if is_valid_ident(ident) => {}
        _ => return false,
    }
    // Termination is enforced by the block-level `ends_with(b"\n")` check above;
    // trailing headers (encoding, gpgsig, …) and the message are not order-checked.
    true
}

/// Strictly validate an annotated tag: `object <hex>`, `type <git type>`, a non-empty
/// `tag <name>`, then `tagger <ident>`, in that order.
fn is_well_formed_tag(data: &[u8], hex_hash_len: usize) -> bool {
    let headers = object_header_block(data);
    // Reject NUL in the header block and require it to be newline-terminated (the
    // tagger/last header line must end in `\n`), matching Git.
    if headers.contains(&0) || !headers.ends_with(b"\n") {
        return false;
    }
    let mut lines = headers.split(|&byte| byte == b'\n');

    match lines.next().and_then(|line| line.strip_prefix(b"object ")) {
        Some(object) if is_hex_of_len(object, hex_hash_len) => {}
        _ => return false,
    }
    match lines.next().and_then(|line| line.strip_prefix(b"type ")) {
        Some(b"blob" | b"commit" | b"tree" | b"tag") => {}
        _ => return false,
    }
    match lines.next().and_then(|line| line.strip_prefix(b"tag ")) {
        Some(name) if is_valid_tag_name(name) => {}
        _ => return false,
    }
    match lines.next().and_then(|line| line.strip_prefix(b"tagger ")) {
        Some(ident) if is_valid_ident(ident) => {}
        _ => return false,
    }
    // Termination is enforced by the block-level `ends_with(b"\n")` check above.
    true
}

/// Git's canonical tree entry modes (trees store `40000`, not `040000`).
const VALID_TREE_MODES: [&[u8]; 5] = [b"40000", b"100644", b"100755", b"120000", b"160000"];

/// Compare two tree entry names using Git's ordering, where a directory (tree) entry
/// sorts as if its name had a trailing `/`.
fn tree_name_cmp(name1: &[u8], is_tree1: bool, name2: &[u8], is_tree2: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let common = name1.len().min(name2.len());
    match name1[..common].cmp(&name2[..common]) {
        Ordering::Equal => {}
        non_equal => return non_equal,
    }
    let next = |name: &[u8], is_tree: bool| -> u8 {
        if name.len() > common {
            name[common]
        } else if is_tree {
            b'/'
        } else {
            0
        }
    };
    next(name1, is_tree1).cmp(&next(name2, is_tree2))
}

/// Strictly validate a tree: a sequence of `<mode> <name>\0<raw hash>` entries that
/// consume the buffer exactly, with Git-valid modes, safe names (non-empty, no `/`,
/// not `.`/`..`), and strict Git ordering (which also forbids duplicates). Empty tree
/// is valid. Matches `git hash-object -t tree` rejections without `--literally`.
fn is_well_formed_tree(data: &[u8], raw_hash_len: usize) -> bool {
    use std::cmp::Ordering;

    let mut index = 0;
    let len = data.len();
    let mut previous: Option<(Vec<u8>, bool)> = None;
    let mut seen_names: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    while index < len {
        // Mode: bytes up to a space, restricted to Git's canonical tree modes.
        let mode_start = index;
        while index < len && data[index] != b' ' {
            index += 1;
        }
        if index >= len {
            return false;
        }
        let mode = &data[mode_start..index];
        if !VALID_TREE_MODES.contains(&mode) {
            return false;
        }
        let is_tree = mode == b"40000";
        index += 1; // space
        // Name: bytes up to NUL; must be non-empty, contain no `/`, and not be `.`/`..`.
        let name_start = index;
        while index < len && data[index] != 0 {
            index += 1;
        }
        if index >= len {
            return false;
        }
        let name = &data[name_start..index];
        // Reject empty names, path separators, `.`/`..`, and `.git` (case-insensitive),
        // matching Git's tree-entry name checks.
        if name.is_empty()
            || name.contains(&b'/')
            || name == b"."
            || name == b".."
            || name.eq_ignore_ascii_case(b".git")
        {
            return false;
        }
        index += 1; // NUL
        // Raw object hash of the algorithm's width.
        if len - index < raw_hash_len {
            return false;
        }
        index += raw_hash_len;

        // Entries must be strictly increasing in Git's tree order...
        if let Some((prev_name, prev_is_tree)) = &previous
            && tree_name_cmp(prev_name, *prev_is_tree, name, is_tree) != Ordering::Less
        {
            return false;
        }
        // ...and no two entries may share a raw name (Git's duplicateEntries), even a
        // blob and a tree with the same name (which Git ordering would otherwise allow).
        if !seen_names.insert(name.to_vec()) {
            return false;
        }
        previous = Some((name.to_vec(), is_tree));
    }
    true
}

fn read_file(path: &Path) -> CliResult<Vec<u8>> {
    fs::read(path).map_err(|error| {
        CliError::fatal(format!(
            "failed to read '{}': {}",
            path.display(),
            format_io_error(&error)
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_hint("verify the path exists and is readable.")
    })
}

fn read_stdin() -> CliResult<Vec<u8>> {
    let mut data = Vec::new();
    io::stdin().read_to_end(&mut data).map_err(|error| {
        CliError::fatal(format!("failed to read standard input: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    Ok(data)
}

fn render_hash_object_output(result: &HashObjectOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("hash-object", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for entry in &result.objects {
        write_hash_line(&mut writer, &entry.oid)?;
    }
    Ok(())
}

fn write_hash_line<W: Write>(writer: &mut W, oid: &str) -> CliResult<()> {
    match writeln!(writer, "{oid}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(
            CliError::fatal(format!("failed to write hash-object output: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        ),
    }
}

fn format_io_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_string(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_one_source_matches_git_empty_blob_hash() {
        let entry = hash_one_source("-", Vec::new(), false, ObjectType::Blob, false)
            .expect("hash empty source");
        assert_eq!(entry.oid, "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(entry.size, 0);
        assert!(!entry.written);
    }

    #[test]
    fn safe_validators_match_git_strictness() {
        const T: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

        // Commit: valid (with a parent in the right place); rejects garbage, binary
        // input, a parent AFTER author, and a malformed ident — matching git fsck.
        let commit = format!(
            "tree {T}\nparent {T}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nmsg\n"
        );
        assert!(is_well_formed_commit(commit.as_bytes(), 40));
        assert!(!is_well_formed_commit(b"not a commit\n", 40));
        assert!(!is_well_formed_commit(&[0xff, 0xfe, 0x00], 40));
        let parent_after = format!(
            "tree {T}\nauthor A <a@b> 0 +0000\nparent {T}\ncommitter A <a@b> 0 +0000\n\nm\n"
        );
        assert!(!is_well_formed_commit(parent_after.as_bytes(), 40));
        let bad_ident = format!("tree {T}\nauthor x\ncommitter A <a@b> 0 +0000\n\nm\n");
        assert!(!is_well_formed_commit(bad_ident.as_bytes(), 40));
        // Ident grammar (matching git fsck): zero-padded timestamp, empty name, and
        // malformed timezones are all rejected. A non-UTF-8 name is accepted.
        assert!(!is_valid_ident(b"A <a@b> 01 +0000"));
        assert!(!is_valid_ident(b"<a@b> 0 +0000"));
        assert!(!is_valid_ident(b"A <a@b> 0 0000"));
        assert!(!is_valid_ident(b"A <a@b> 0 +000"));
        assert!(is_valid_ident(b"A U Thor <a@b> 1700000000 -0800"));
        assert!(is_valid_ident(b"A\xff <a@b> 0 +0000"));
        // The name may be empty as long as a space precedes the email (git accepts
        // `author  <a@b>`), but the separating space is required (`author <a@b>` with
        // no space before `<` is rejected).
        assert!(is_valid_ident(b" <a@b> 0 +0000"));
        assert!(!is_valid_ident(b"<a@b> 0 +0000"));

        // Header terminator (git's unterminatedHeader): the final header line must end
        // in a newline. A blank line/message is NOT required, but a trailing header
        // without a final newline is rejected.
        let no_newline = format!("tree {T}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000");
        assert!(!is_well_formed_commit(no_newline.as_bytes(), 40));
        let trailing_newline =
            format!("tree {T}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n");
        assert!(is_well_formed_commit(trailing_newline.as_bytes(), 40));
        let extra_header_no_lf =
            format!("tree {T}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\nencoding UTF-8");
        assert!(!is_well_formed_commit(extra_header_no_lf.as_bytes(), 40));
        let extra_header_lf = format!(
            "tree {T}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\nencoding UTF-8\n"
        );
        assert!(is_well_formed_commit(extra_header_lf.as_bytes(), 40));
        // A NUL anywhere in a commit (including the message) is rejected (nulInCommit).
        let nul_body =
            format!("tree {T}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nmsg\0more\n");
        assert!(!is_well_formed_commit(nul_body.as_bytes(), 40));

        // Tag: valid; rejects bad object hash, empty tag name, and missing tagger.
        let tag = format!("object {T}\ntype commit\ntag v1\ntagger A <a@b> 0 +0000\n\nmsg\n");
        assert!(is_well_formed_tag(tag.as_bytes(), 40));
        assert!(!is_well_formed_tag(b"object zzz\n", 40));
        let empty_name = format!("object {T}\ntype tree\ntag \ntagger A <a@b> 0 +0000\n\nm\n");
        assert!(!is_well_formed_tag(empty_name.as_bytes(), 40));
        // Tagger line must be newline-terminated (unterminatedHeader).
        let tag_no_newline = format!("object {T}\ntype tree\ntag v1\ntagger A <a@b> 0 +0000");
        assert!(!is_well_formed_tag(tag_no_newline.as_bytes(), 40));
        // Tag name is refname-validated (matching git): spaces, `..`, and `.lock` are
        // rejected; an ordinary (or non-UTF-8) name is accepted.
        assert!(is_valid_tag_name(b"v1.0"));
        assert!(!is_valid_tag_name(b"a b"));
        assert!(!is_valid_tag_name(b".."));
        assert!(!is_valid_tag_name(b"v.lock"));
        assert!(is_valid_tag_name(b"v\xff"));
        // A bare `@` is a valid tag-object name (git accepts it), but `@{` is rejected.
        assert!(is_valid_tag_name(b"@"));
        assert!(!is_valid_tag_name(b"a@{b"));

        // Tree: empty + sorted entries valid; rejects bad mode, `/` in a name, an
        // unsorted pair, and a short hash.
        assert!(is_well_formed_tree(b"", 20));
        let entry = |mode: &[u8], name: &[u8]| {
            let mut e = mode.to_vec();
            e.push(b' ');
            e.extend_from_slice(name);
            e.push(0);
            e.extend_from_slice(&[0x11; 20]);
            e
        };
        let mut sorted = entry(b"100644", b"a");
        sorted.extend(entry(b"100644", b"b"));
        assert!(is_well_formed_tree(&sorted, 20));
        assert!(!is_well_formed_tree(&entry(b"777", b"a"), 20));
        assert!(!is_well_formed_tree(&entry(b"100644", b"a/b"), 20));
        let mut unsorted = entry(b"100644", b"b");
        unsorted.extend(entry(b"100644", b"a"));
        assert!(!is_well_formed_tree(&unsorted, 20));
        assert!(!is_well_formed_tree(b"100644 a\0short", 20));
        // `.git` (case-insensitive) is rejected.
        assert!(!is_well_formed_tree(&entry(b"100644", b".git"), 20));
        assert!(!is_well_formed_tree(&entry(b"40000", b".GIT"), 20));
        // A blob and a tree sharing a raw name is a duplicate (git's duplicateEntries),
        // even though tree ordering would treat `a` < `a/`.
        let mut dup = entry(b"100644", b"a");
        dup.extend(entry(b"40000", b"a"));
        assert!(!is_well_formed_tree(&dup, 20));
    }

    #[test]
    fn write_hash_line_ignores_broken_pipe() {
        struct BrokenPipeWriter;

        impl Write for BrokenPipeWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = BrokenPipeWriter;
        write_hash_line(&mut writer, "abc").expect("broken pipe should be ignored");
    }
}
