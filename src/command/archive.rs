//! Command-line surface for creating archives from committed tree snapshots.

use std::{
    fs::File,
    io::{BufWriter, Cursor, Seek, Write},
    path::{Component, Path, PathBuf},
};

use bzip2::write::BzEncoder;
use clap::Parser;
use flate2::{Compression, write::GzEncoder};
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        blob::Blob,
        commit::Commit,
        tree::{Tree, TreeItemMode},
    },
};

use crate::{
    command::load_object,
    internal::log::date_parser::parse_date,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        tree_attributes::{self, ExportIgnoreMatcher, TreeAttributeSource},
        util,
    },
};

pub const ARCHIVE_EXAMPLES: &str = "\
EXAMPLES:
    libra archive -o project.tar HEAD
    libra archive --format=tar.gz --prefix=project-v1/ -o project-v1.tar.gz v1.0
    libra archive --format=zip -o feature.zip feature-branch
    libra archive -v -o project.tar HEAD   List archived paths on stderr
    libra archive --add-file=NOTES.txt -o release.tar HEAD   Include an untracked file
    libra archive --format=tar.gz --compression-level=9 -o max.tgz HEAD   Max compression
    libra archive --mtime='2026-01-01 00:00:00 +0000' -o stamped.tar HEAD   Set entry mtime
    libra archive --list";

/// Supported archive output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    /// Uncompressed tarball.
    Tar,
    /// Gzip-compressed tarball.
    TarGz,
    /// Bzip2-compressed tarball.
    TarBz2,
    /// ZIP archive.
    Zip,
}

impl ArchiveFormat {
    /// All supported format name strings, listed in preferred order.
    const ALL: &[&str] = &["tar", "tar.gz", "tar.bz2", "zip"];

    /// Parse a format string strictly, returning an error for unknown formats.
    fn parse_strict(value: &str) -> Result<Self, String> {
        match value {
            "tar" => Ok(Self::Tar),
            "tar.gz" | "tgz" => Ok(Self::TarGz),
            "tar.bz2" | "tbz2" | "tbz" => Ok(Self::TarBz2),
            "zip" => Ok(Self::Zip),
            other => Err(format!(
                "unknown archive format: '{other}'. Supported formats: {}",
                Self::ALL.join(", ")
            )),
        }
    }

    fn list_supported() -> &'static str {
        "tar\ntar.gz\ntar.bz2\nzip\n"
    }
}

/// Create an archive of files from a named tree.
#[derive(Parser, Debug)]
#[command(after_help = ARCHIVE_EXAMPLES)]
pub struct ArchiveArgs {
    /// List supported archive formats and exit.
    #[arg(short = 'l', long = "list")]
    pub list: bool,

    /// Commit, branch, tag, or abbreviated commit hash to archive. Defaults to HEAD.
    #[arg(value_name = "TREEISH")]
    pub treeish: Option<String>,

    /// Limit the archive to matching paths or directories inside TREEISH.
    #[arg(value_name = "PATH", num_args = 0.., trailing_var_arg = true)]
    pub paths: Vec<String>,

    /// Archive format: tar, tar.gz, tar.bz2, or zip.
    #[arg(short = 'f', long, default_value = "tar", value_name = "FMT")]
    pub format: String,

    /// Write archive bytes to a file instead of stdout.
    #[arg(short = 'o', long, value_name = "FILE")]
    pub output: Option<String>,

    /// Prepend a relative directory prefix to each archived path.
    #[arg(long, value_name = "PREFIX")]
    pub prefix: Option<String>,

    /// Report each archived path to stderr as progress.
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Add an untracked file (read from the working tree) to the archive at its
    /// basename, under the optional prefix, like `git archive --add-file=<file>`.
    /// Repeatable; added files are not subject to the `<path>` pathspec filter.
    #[arg(long = "add-file", value_name = "FILE", action = clap::ArgAction::Append)]
    pub add_file: Vec<String>,

    /// Compression level 0-9 for the compressed formats (`tar.gz`, `tar.bz2`,
    /// `zip`); ignored for plain `tar`. Git exposes this as `-0`..`-9`, which
    /// clap cannot model as bare numeric flags, so Libra uses this long form.
    /// (bzip2 has no level 0, so 0 is treated as 1 there.)
    #[arg(long = "compression-level", value_name = "LEVEL", value_parser = clap::value_parser!(u32).range(0..=9))]
    pub compression_level: Option<u32>,

    /// Set the modification time of all archive entries, like `git archive
    /// --mtime`. Accepts the same date formats as `--since`/`--until`
    /// (`YYYY-MM-DD`, RFC 3339, `"N days ago"`, a Unix timestamp). Without it,
    /// the archived commit's committer time is used.
    #[arg(long = "mtime", value_name = "TIME")]
    pub mtime: Option<String>,
}

/// Where an archive entry's bytes come from.
enum ArchiveSource {
    /// A tracked blob in the object store, read on demand by hash.
    Blob(ObjectHash),
    /// Inline bytes for an untracked file added via `--add-file`.
    Inline(Vec<u8>),
}

/// Collected metadata about a single entry for archiving.
struct ArchiveEntry {
    /// The logical path within the archive before the optional prefix is applied.
    path: PathBuf,
    /// Where the entry's content comes from.
    source: ArchiveSource,
    /// The file mode (regular or executable) for the archive header.
    mode: TreeItemMode,
}

/// Validate one raw tree item name before joining it into an archive path.
fn validate_tree_entry_name(name: &str) -> Result<&Path, CliError> {
    let path = Path::new(name);
    let mut components = path.components();
    let is_safe_single_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();

    if !is_safe_single_component {
        return Err(CliError::fatal(format!(
            "unsafe archive tree entry name '{name}': expected one relative path component"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt));
    }

    Ok(path)
}

/// Recursively collect archiveable file entries from a tree.
fn collect_tree_entries(
    tree: &Tree,
    base: &Path,
    entries: &mut Vec<ArchiveEntry>,
) -> Result<(), CliError> {
    for item in &tree.tree_items {
        let path = base.join(validate_tree_entry_name(&item.name)?);
        match item.mode {
            TreeItemMode::Tree => {
                let sub_tree: Tree = load_object(&item.id).map_err(|error| {
                    CliError::fatal(format!(
                        "failed to load subtree '{}' at '{}': {error}",
                        item.id,
                        path.display()
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
                collect_tree_entries(&sub_tree, &path, entries)?;
            }
            TreeItemMode::Commit => {
                // Gitlink/submodule entries point at commits that Libra does not
                // materialize as files.
            }
            _ => entries.push(ArchiveEntry {
                path,
                source: ArchiveSource::Blob(item.id),
                mode: item.mode,
            }),
        }
    }

    Ok(())
}

fn entry_has_archive_metadata(entry: &ArchiveEntry) -> bool {
    !entry.path.as_os_str().is_empty()
        && !matches!(entry.mode, TreeItemMode::Tree | TreeItemMode::Commit)
}

/// Read an entry's bytes — from its blob (tracked) or its inline buffer (an
/// `--add-file` untracked file).
fn load_entry_content(entry: &ArchiveEntry) -> Result<Vec<u8>, CliError> {
    match &entry.source {
        ArchiveSource::Blob(hash) => load_blob_content(hash),
        ArchiveSource::Inline(data) => Ok(data.clone()),
    }
}

fn collect_tree_attribute_sources(
    entries: &[ArchiveEntry],
) -> Result<Vec<TreeAttributeSource>, CliError> {
    let mut sources = Vec::new();
    for entry in entries {
        if !tree_attributes::is_tree_attribute_file(&entry.path) {
            continue;
        }
        sources.push(TreeAttributeSource {
            path: entry.path.clone(),
            contents: load_entry_content(entry)?,
        });
    }
    Ok(sources)
}

fn filter_export_ignored_entries(
    entries: Vec<ArchiveEntry>,
    matcher: &ExportIgnoreMatcher,
) -> Vec<ArchiveEntry> {
    entries
        .into_iter()
        .filter(|entry| !matcher.is_ignored(&entry.path))
        .collect()
}

/// Map a working-tree file's metadata to the archive header mode: executable
/// when any execute bit is set (Unix), otherwise a regular file.
#[cfg(unix)]
fn add_file_mode(metadata: &std::fs::Metadata) -> TreeItemMode {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        TreeItemMode::BlobExecutable
    } else {
        TreeItemMode::Blob
    }
}

/// On non-Unix platforms there is no execute bit to honor; added files are
/// archived as regular files.
#[cfg(not(unix))]
fn add_file_mode(_metadata: &std::fs::Metadata) -> TreeItemMode {
    TreeItemMode::Blob
}

/// Build an archive entry for an untracked `--add-file=<file>`: read the file
/// from the working tree and place it at its basename. The optional `--prefix`
/// is applied later by the writers, matching `git archive --add-file`.
fn build_add_file_entry(spec: &str) -> Result<ArchiveEntry, CliError> {
    let src = Path::new(spec);
    let file_name = src.file_name().ok_or_else(|| {
        CliError::command_usage(format!(
            "invalid --add-file path '{spec}': it has no file name"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    let metadata = std::fs::metadata(src).map_err(|error| {
        CliError::fatal(format!("could not read --add-file '{spec}': {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if !metadata.is_file() {
        return Err(
            CliError::command_usage(format!("--add-file '{spec}' is not a regular file"))
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    let data = std::fs::read(src).map_err(|error| {
        CliError::fatal(format!("could not read --add-file '{spec}': {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    Ok(ArchiveEntry {
        path: PathBuf::from(file_name),
        source: ArchiveSource::Inline(data),
        mode: add_file_mode(&metadata),
    })
}

fn validate_pathspec(pathspec: &str) -> Result<PathBuf, CliError> {
    if pathspec.is_empty() {
        return Err(
            CliError::command_usage("archive pathspec must not be empty")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    let path = Path::new(pathspec);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CliError::command_usage(format!(
            "invalid archive pathspec '{pathspec}': use a relative path without '..'"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    Ok(path.to_path_buf())
}

fn entry_matches_pathspec(entry: &ArchiveEntry, pathspec: &Path) -> bool {
    entry.path == pathspec || entry.path.starts_with(pathspec)
}

fn filter_entries_by_pathspecs(
    entries: Vec<ArchiveEntry>,
    pathspecs: &[String],
) -> Result<Vec<ArchiveEntry>, CliError> {
    if pathspecs.is_empty() {
        return Ok(entries);
    }

    let normalized = pathspecs
        .iter()
        .map(|pathspec| validate_pathspec(pathspec))
        .collect::<Result<Vec<_>, _>>()?;
    let filtered = entries
        .into_iter()
        .filter(|entry| {
            normalized
                .iter()
                .any(|pathspec| entry_matches_pathspec(entry, pathspec))
        })
        .collect::<Vec<_>>();

    if filtered.is_empty() {
        return Err(CliError::fatal(format!(
            "pathspec '{}' did not match any files in the archive tree",
            pathspecs.join(", ")
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }

    Ok(filtered)
}

/// Resolve a tree-ish string to the archiveable entries from that commit tree.
/// Resolve a tree-ish to its archive entries and the committer timestamp of the
/// commit it names (used as the default archive-entry mtime, matching Git).
async fn resolve_entries(treeish: &str) -> Result<(Vec<ArchiveEntry>, i64), CliError> {
    let commit_hash = util::get_commit_base(treeish).await.map_err(|error| {
        CliError::fatal(format!("failed to resolve '{treeish}': {error}"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;

    let commit = load_object::<Commit>(&commit_hash).map_err(|error| {
        CliError::fatal(format!("failed to load commit {commit_hash}: {error}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let committer_time = commit.committer.timestamp as i64;

    let tree: Tree = load_object(&commit.tree_id).map_err(|error| {
        CliError::fatal(format!(
            "failed to load tree {} for commit {commit_hash}: {error}",
            commit.tree_id
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;

    let mut entries = Vec::new();
    collect_tree_entries(&tree, Path::new(""), &mut entries)?;
    Ok((entries, committer_time))
}

/// Validate a user-supplied archive prefix before it is joined with file paths.
fn validate_prefix(prefix: Option<&str>) -> Result<Option<PathBuf>, CliError> {
    let Some(prefix) = prefix else {
        return Ok(None);
    };

    if prefix.is_empty() {
        return Ok(Some(PathBuf::new()));
    }

    let path = Path::new(prefix);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CliError::command_usage(format!(
            "invalid archive prefix '{prefix}': use a relative path without '..'"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    Ok(Some(path.to_path_buf()))
}

/// Apply the user-requested prefix to a path within the archive.
fn apply_prefix(prefix: Option<&Path>, path: &Path) -> PathBuf {
    match prefix {
        Some(prefix) => prefix.join(path),
        None => path.to_path_buf(),
    }
}

/// Load blob content for a given hash.
fn load_blob_content(hash: &ObjectHash) -> Result<Vec<u8>, CliError> {
    let blob: Blob = load_object(hash).map_err(|error| {
        CliError::fatal(format!("failed to load blob {hash}: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    Ok(blob.data)
}

/// Determine the UNIX mode bits for a tree entry stored in a tar header.
fn tar_entry_mode(mode: &TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o644,
        TreeItemMode::BlobExecutable => 0o755,
        TreeItemMode::Link => 0o644,
        TreeItemMode::Tree => 0o755,
        TreeItemMode::Commit => 0o644,
    }
}

/// Determine the tar entry type for a tree item.
fn tar_entry_type(mode: &TreeItemMode) -> tar::EntryType {
    match mode {
        TreeItemMode::Link => tar::EntryType::Symlink,
        _ => tar::EntryType::Regular,
    }
}

/// Apply symlink-specific tar header fields and finalize their checksum.
fn configure_tar_symlink_header(
    header: &mut tar::Header,
    archive_path: &Path,
    data: Vec<u8>,
) -> Result<(), CliError> {
    header.set_link_name_literal(&data).map_err(|error| {
        CliError::fatal(format!(
            "invalid symlink target for '{}': {error}",
            archive_path.display()
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    header.set_size(0);
    header.set_cksum();
    Ok(())
}

/// Write a tar archive of the given entries to `writer`. All entries share the
/// `mtime` (Unix seconds); a negative value (a pre-1970 `--mtime`) clamps to 0,
/// the earliest a tar header can represent.
fn write_tar_archive<W: Write>(
    entries: &[ArchiveEntry],
    prefix: Option<&Path>,
    writer: W,
    mtime: i64,
) -> Result<(), CliError> {
    let mut builder = tar::Builder::new(writer);

    for entry in entries {
        let archive_path = apply_prefix(prefix, &entry.path);
        let data = load_entry_content(entry)?;
        let mode = tar_entry_mode(&entry.mode);
        let entry_type = tar_entry_type(&entry.mode);

        let mut header = tar::Header::new_gnu();
        header.set_path(&archive_path).map_err(|error| {
            CliError::fatal(format!(
                "invalid archive path '{}': {error}",
                archive_path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_mtime(mtime.max(0) as u64);
        header.set_entry_type(entry_type);
        header.set_cksum();

        if entry_type == tar::EntryType::Symlink {
            configure_tar_symlink_header(&mut header, &archive_path, data)?;
            builder.append(&header, std::io::empty()).map_err(|error| {
                CliError::fatal(format!(
                    "failed to write symlink '{}': {error}",
                    archive_path.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            continue;
        }

        builder.append(&header, data.as_slice()).map_err(|error| {
            CliError::fatal(format!(
                "failed to write entry '{}': {error}",
                archive_path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }

    builder.finish().map_err(|error| {
        CliError::fatal(format!("failed to finalize archive: {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    Ok(())
}

/// Write a gzip-compressed tar archive.
fn write_tar_gz_archive<W: Write>(
    entries: &[ArchiveEntry],
    prefix: Option<&Path>,
    writer: W,
    level: Option<u32>,
    mtime: i64,
) -> Result<(), CliError> {
    // flate2 accepts levels 0-9 (0 = no compression); default is 6.
    let compression = level.map(Compression::new).unwrap_or_default();
    let gz = GzEncoder::new(writer, compression);
    write_tar_archive(entries, prefix, gz, mtime)
}

/// Write a bzip2-compressed tar archive.
fn write_tar_bz2_archive<W: Write>(
    entries: &[ArchiveEntry],
    prefix: Option<&Path>,
    writer: W,
    level: Option<u32>,
    mtime: i64,
) -> Result<(), CliError> {
    // bzip2 levels are 1-9 (no "store" level), so a requested 0 maps to 1.
    let compression = level
        .map(|l| bzip2::Compression::new(l.clamp(1, 9)))
        .unwrap_or_default();
    let bz = BzEncoder::new(writer, compression);
    write_tar_archive(entries, prefix, bz, mtime)
}

/// Determine external Unix attributes for a zip entry.
fn zip_unix_mode(mode: &TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        _ => 0o100644,
    }
}

/// Convert a Unix timestamp to a zip [`DateTime`]. The MS-DOS-based zip time
/// format only spans 1980-2107, so an out-of-range value (e.g. a pre-1980
/// `--mtime`) falls back to the zip epoch (1980-01-01).
fn zip_datetime(mtime: i64) -> zip::DateTime {
    use chrono::{DateTime, Datelike, Timelike, Utc};
    let dt = DateTime::<Utc>::from_timestamp(mtime, 0).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    zip::DateTime::from_date_and_time(
        dt.year() as u16,
        dt.month() as u8,
        dt.day() as u8,
        dt.hour() as u8,
        dt.minute() as u8,
        dt.second() as u8,
    )
    .unwrap_or_default()
}

/// Write a zip archive of the given entries to `writer`.
fn write_zip_archive<W: Write + Seek>(
    entries: &[ArchiveEntry],
    prefix: Option<&Path>,
    writer: W,
    level: Option<u32>,
    mtime: i64,
) -> Result<(), CliError> {
    let mut archive = zip::ZipWriter::new(writer);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(level.map(|l| l as i32))
        .last_modified_time(zip_datetime(mtime));

    for entry in entries {
        let archive_path = apply_prefix(prefix, &entry.path);
        let path = archive_path
            .to_str()
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "archive path '{}' is not valid UTF-8",
                    archive_path.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?
            .to_string();
        let data = load_entry_content(entry)?;

        if entry.mode == TreeItemMode::Link {
            let symlink_options = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .unix_permissions(0o120777)
                .last_modified_time(zip_datetime(mtime));
            archive.start_file(path, symlink_options).map_err(|error| {
                CliError::fatal(format!(
                    "failed to add symlink '{}': {error}",
                    archive_path.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            archive.write_all(&data).map_err(|error| {
                CliError::fatal(format!(
                    "failed to write symlink target '{}': {error}",
                    archive_path.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            continue;
        }

        archive
            .start_file(path, options.unix_permissions(zip_unix_mode(&entry.mode)))
            .map_err(|error| {
                CliError::fatal(format!(
                    "failed to create zip entry '{}': {error}",
                    archive_path.display()
                ))
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
        archive.write_all(&data).map_err(|error| {
            CliError::fatal(format!(
                "failed to write zip entry '{}': {error}",
                archive_path.display()
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }

    archive.finish().map_err(|error| {
        CliError::fatal(format!("failed to finalize zip archive: {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    Ok(())
}

/// Create a buffered output file.
fn create_output_file(path: &str) -> Result<BufWriter<File>, CliError> {
    let file = File::create(path).map_err(|error| {
        CliError::fatal(format!("failed to create output file '{path}': {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    Ok(BufWriter::new(file))
}

/// Open the output destination: either a file path or stdout.
fn open_output(path: Option<&str>) -> Result<Box<dyn Write>, CliError> {
    match path {
        Some(path) => Ok(Box::new(create_output_file(path)?)),
        None => Ok(Box::new(std::io::stdout())),
    }
}

/// Write zip output to a seekable file when available, buffering only for stdout.
fn write_zip_output(
    entries: &[ArchiveEntry],
    prefix: Option<&Path>,
    output: Option<&str>,
    level: Option<u32>,
    mtime: i64,
) -> Result<(), CliError> {
    if let Some(path) = output {
        return write_zip_archive(entries, prefix, create_output_file(path)?, level, mtime);
    }

    let mut buffer = Cursor::new(Vec::new());
    write_zip_archive(entries, prefix, &mut buffer, level, mtime)?;

    let mut stdout = std::io::stdout();
    stdout.write_all(&buffer.into_inner()).map_err(|error| {
        CliError::fatal(format!("failed to write zip output: {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;
    stdout.flush().map_err(|error| {
        CliError::fatal(format!("failed to flush zip output: {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })
}

/// Create an archive from tree entries, dispatching to the correct writer.
fn create_archive(
    format: ArchiveFormat,
    entries: &[ArchiveEntry],
    prefix: Option<&Path>,
    output: Option<&str>,
    level: Option<u32>,
    mtime: i64,
) -> Result<(), CliError> {
    match format {
        ArchiveFormat::Tar => {
            // Plain tar is uncompressed; the level is not applicable.
            let writer = open_output(output)?;
            write_tar_archive(entries, prefix, writer, mtime)
        }
        ArchiveFormat::TarGz => {
            let writer = open_output(output)?;
            write_tar_gz_archive(entries, prefix, writer, level, mtime)
        }
        ArchiveFormat::TarBz2 => {
            let writer = open_output(output)?;
            write_tar_bz2_archive(entries, prefix, writer, level, mtime)
        }
        ArchiveFormat::Zip => write_zip_output(entries, prefix, output, level, mtime),
    }
}

/// # Side Effects
///
/// Reads commit, tree, and blob objects from the local object store. Writes
/// archive bytes to stdout or the requested output file.
///
/// # Errors
///
/// Returns `CliInvalidArguments` for unsupported formats or unsafe prefixes.
/// Returns `CliInvalidTarget` when the tree-ish cannot be resolved.
/// Returns `RepoCorrupt` when referenced commit or tree objects cannot be read.
pub async fn execute_safe(args: ArchiveArgs, _output: &OutputConfig) -> CliResult<()> {
    if args.list {
        print!("{}", ArchiveFormat::list_supported());
        return Ok(());
    }

    let format = ArchiveFormat::parse_strict(&args.format).map_err(|message| {
        CliError::command_usage(message).with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    let prefix = validate_prefix(args.prefix.as_deref())?;
    let treeish = args.treeish.as_deref().unwrap_or("HEAD");
    let (resolved_entries, committer_time) = resolve_entries(treeish).await?;
    let attribute_sources = collect_tree_attribute_sources(&resolved_entries)?;
    let export_ignore = ExportIgnoreMatcher::from_sources(&attribute_sources);
    let entries = filter_entries_by_pathspecs(resolved_entries, &args.paths)?;
    let mut entries = filter_export_ignored_entries(entries, &export_ignore);

    // Archive-entry modification time: `--mtime` when given (same date formats as
    // `--since`/`--until`), otherwise the archived commit's committer time
    // (matching Git, which uses epoch 0 only as a last resort, not by default).
    let mtime = match args.mtime.as_deref() {
        Some(spec) => parse_date(spec).map_err(|error| {
            CliError::command_usage(format!("invalid --mtime value '{spec}': {error}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?,
        None => committer_time,
    };

    // `--add-file=<file>` appends untracked working-tree files (not subject to
    // the pathspec filter), so an archive can include them alongside — or even
    // instead of — tracked tree content.
    for spec in &args.add_file {
        entries.push(build_add_file_entry(spec)?);
    }

    if entries.is_empty() {
        return Err(
            CliError::fatal(format!("tree '{}' contains no files to archive", treeish))
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    debug_assert!(entries.iter().all(entry_has_archive_metadata));

    // `-v`/`--verbose` reports each archived path (with the prefix applied) to
    // stderr, mirroring `git archive -v`. The listing follows the archive entry
    // order so it lines up with what the writers emit to the output.
    if args.verbose {
        for entry in &entries {
            eprintln!("{}", apply_prefix(prefix.as_deref(), &entry.path).display());
        }
    }

    create_archive(
        format,
        &entries,
        prefix.as_deref(),
        args.output.as_deref(),
        args.compression_level,
        mtime,
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use git_internal::internal::object::tree::TreeItem;

    use super::*;

    #[test]
    fn archive_format_accepts_supported_names() {
        assert_eq!(
            ArchiveFormat::parse_strict("tar").unwrap(),
            ArchiveFormat::Tar
        );
        assert_eq!(
            ArchiveFormat::parse_strict("tar.gz").unwrap(),
            ArchiveFormat::TarGz
        );
        assert_eq!(
            ArchiveFormat::parse_strict("tgz").unwrap(),
            ArchiveFormat::TarGz
        );
        assert_eq!(
            ArchiveFormat::parse_strict("tar.bz2").unwrap(),
            ArchiveFormat::TarBz2
        );
        assert_eq!(
            ArchiveFormat::parse_strict("tbz2").unwrap(),
            ArchiveFormat::TarBz2
        );
        assert_eq!(
            ArchiveFormat::parse_strict("tbz").unwrap(),
            ArchiveFormat::TarBz2
        );
        assert_eq!(
            ArchiveFormat::parse_strict("zip").unwrap(),
            ArchiveFormat::Zip
        );
    }

    #[test]
    fn archive_format_rejects_unknown_names() {
        let err = ArchiveFormat::parse_strict("rar").unwrap_err();

        assert!(err.contains("unknown archive format"));
        assert!(err.contains("tar.gz"));
        assert!(ArchiveFormat::parse_strict("").is_err());
    }

    #[test]
    fn validate_prefix_accepts_safe_relative_paths() {
        assert_eq!(validate_prefix(None).unwrap(), None);
        assert_eq!(
            validate_prefix(Some("release/")).unwrap(),
            Some(PathBuf::from("release/"))
        );
        assert_eq!(
            validate_prefix(Some("nested/release")).unwrap(),
            Some(PathBuf::from("nested/release"))
        );
        assert_eq!(validate_prefix(Some("")).unwrap(), Some(PathBuf::new()));
    }

    #[test]
    fn validate_prefix_rejects_archive_slip_paths() {
        assert!(validate_prefix(Some("../release")).is_err());
        assert!(validate_prefix(Some("release/../other")).is_err());
        assert!(validate_prefix(Some("/tmp/release")).is_err());
    }

    #[test]
    fn validate_pathspec_accepts_safe_relative_paths() {
        assert_eq!(
            validate_pathspec("README.md").unwrap(),
            PathBuf::from("README.md")
        );
        assert_eq!(validate_pathspec("src/").unwrap(), PathBuf::from("src/"));
    }

    #[test]
    fn validate_pathspec_rejects_archive_slip_paths() {
        assert!(validate_pathspec("").is_err());
        assert!(validate_pathspec("../README.md").is_err());
        assert!(validate_pathspec("/tmp/README.md").is_err());
        assert!(validate_pathspec("src/../README.md").is_err());
    }

    #[test]
    fn apply_prefix_prepends_relative_prefix() {
        assert_eq!(
            apply_prefix(Some(Path::new("release")), Path::new("src/lib.rs")),
            PathBuf::from("release/src/lib.rs")
        );
        assert_eq!(
            apply_prefix(None, Path::new("src/lib.rs")),
            PathBuf::from("src/lib.rs")
        );
    }

    #[test]
    fn validate_tree_entry_name_rejects_unsafe_names() {
        assert!(validate_tree_entry_name("README.md").is_ok());
        assert!(validate_tree_entry_name("你好.txt").is_ok());

        for name in [
            "",
            ".",
            "..",
            "../payload",
            "/tmp/payload",
            "nested/file.txt",
        ] {
            let err = validate_tree_entry_name(name).expect_err("unsafe tree entry should fail");
            assert_eq!(err.stable_code(), StableErrorCode::RepoCorrupt);
        }
    }

    #[test]
    fn collect_tree_entries_keeps_blob_metadata() {
        let hash =
            ObjectHash::from_str("8ab686eafeb1f44702738c8b0f24f2567c36da6d").expect("valid hash");
        let tree = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Blob, hash, "README.md".to_string()),
            TreeItem::new(TreeItemMode::BlobExecutable, hash, "script.sh".to_string()),
        ])
        .expect("valid test tree");
        let mut entries = Vec::new();

        collect_tree_entries(&tree, Path::new("docs"), &mut entries).expect("collect entries");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("docs/README.md"));
        assert!(
            matches!(&entries[0].source, ArchiveSource::Blob(h) if *h == hash),
            "tree entries should carry their blob hash"
        );
        assert_eq!(entries[0].mode, TreeItemMode::Blob);
        assert_eq!(entries[1].path, PathBuf::from("docs/script.sh"));
        assert_eq!(entries[1].mode, TreeItemMode::BlobExecutable);
    }

    #[test]
    fn collect_tree_entries_skips_gitlinks() {
        let hash =
            ObjectHash::from_str("8ab686eafeb1f44702738c8b0f24f2567c36da6d").expect("valid hash");
        let tree = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Commit, hash, "submodule".to_string()),
            TreeItem::new(TreeItemMode::Blob, hash, "README.md".to_string()),
        ])
        .expect("valid test tree");
        let mut entries = Vec::new();

        collect_tree_entries(&tree, Path::new(""), &mut entries).expect("collect entries");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("README.md"));
    }

    #[test]
    fn filter_entries_by_pathspecs_keeps_matching_files_and_dirs() {
        let hash =
            ObjectHash::from_str("8ab686eafeb1f44702738c8b0f24f2567c36da6d").expect("valid hash");
        let entries = vec![
            ArchiveEntry {
                path: PathBuf::from("README.md"),
                source: ArchiveSource::Blob(hash),
                mode: TreeItemMode::Blob,
            },
            ArchiveEntry {
                path: PathBuf::from("src/main.rs"),
                source: ArchiveSource::Blob(hash),
                mode: TreeItemMode::Blob,
            },
            ArchiveEntry {
                path: PathBuf::from("src/lib.rs"),
                source: ArchiveSource::Blob(hash),
                mode: TreeItemMode::Blob,
            },
        ];

        let filtered = filter_entries_by_pathspecs(entries, &["src".to_string()])
            .expect("src pathspec should match");

        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|entry| entry.path.starts_with("src")));
    }

    #[test]
    fn tar_helpers_map_supported_file_modes() {
        assert_eq!(tar_entry_mode(&TreeItemMode::Blob), 0o644);
        assert_eq!(tar_entry_mode(&TreeItemMode::BlobExecutable), 0o755);
        assert_eq!(tar_entry_type(&TreeItemMode::Blob), tar::EntryType::Regular);
        assert_eq!(tar_entry_type(&TreeItemMode::Link), tar::EntryType::Symlink);
    }

    #[test]
    fn write_tar_archive_accepts_empty_entries_for_writer_helper() {
        let mut buf = Vec::new();

        write_tar_archive(&[], None, &mut buf, 0).expect("empty tar should finalize");

        assert!(!buf.is_empty());
    }

    #[test]
    fn configure_tar_symlink_header_recomputes_readable_checksum() {
        let mut header = tar::Header::new_gnu();
        header
            .set_path("readme-link")
            .expect("valid symlink test path");
        header.set_size("README.md".len() as u64);
        header.set_mode(tar_entry_mode(&TreeItemMode::Link));
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_cksum();

        configure_tar_symlink_header(&mut header, Path::new("readme-link"), b"README.md".to_vec())
            .expect("configure symlink header");

        let mut buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut buf);
            builder
                .append(&header, std::io::empty())
                .expect("append symlink header");
            builder.finish().expect("finish symlink tar");
        }

        let mut archive = tar::Archive::new(buf.as_slice());
        let mut entries = archive.entries().expect("read symlink tar entries");
        let entry = entries
            .next()
            .expect("symlink tar should contain one entry")
            .expect("parse symlink tar entry");

        assert_eq!(entry.header().entry_type(), tar::EntryType::Symlink);
        assert_eq!(
            entry
                .link_name_bytes()
                .expect("read symlink target")
                .as_ref(),
            b"README.md"
        );
        assert!(entries.next().is_none());
    }

    #[test]
    fn configure_tar_symlink_header_preserves_non_utf8_target_bytes() {
        let mut header = tar::Header::new_gnu();
        header
            .set_path("link")
            .expect("valid non-utf8 symlink test path");
        header.set_size(2);
        header.set_mode(tar_entry_mode(&TreeItemMode::Link));
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_cksum();

        configure_tar_symlink_header(&mut header, Path::new("link"), vec![0xff, b'x'])
            .expect("configure non-utf8 symlink header");

        assert_eq!(
            header.link_name_bytes().expect("link bytes").as_ref(),
            &[0xff, b'x']
        );
    }

    #[test]
    fn write_tar_gz_archive_accepts_empty_entries() {
        let mut buf = Vec::new();

        write_tar_gz_archive(&[], None, &mut buf, None, 0).expect("empty tar.gz should finalize");

        assert!(buf.starts_with(&[0x1f, 0x8b]));
    }

    #[test]
    fn write_tar_bz2_archive_accepts_empty_entries() {
        let mut buf = Vec::new();

        write_tar_bz2_archive(&[], None, &mut buf, None, 0).expect("empty tar.bz2 should finalize");

        assert!(buf.starts_with(b"BZh"));
    }

    #[test]
    fn zip_unix_mode_maps_supported_file_modes() {
        assert_eq!(zip_unix_mode(&TreeItemMode::Blob), 0o100644);
        assert_eq!(zip_unix_mode(&TreeItemMode::BlobExecutable), 0o100755);
        assert_eq!(zip_unix_mode(&TreeItemMode::Link), 0o120000);
    }

    #[test]
    fn write_zip_archive_accepts_empty_entries() {
        let mut buf = Cursor::new(Vec::new());

        write_zip_archive(&[], None, &mut buf, None, 0).expect("empty zip should finalize");

        assert!(buf.into_inner().starts_with(b"PK"));
    }
}
