//! `libra bundle` — create and inspect Git v2 bundle files, a focused subset of
//! `git bundle`.
//!
//! A bundle is a small text header followed by a pack:
//!
//! ```text
//! # v2 git bundle
//! <tip-oid> <ref-name>      (one per included ref)
//!                           (blank line)
//! PACK……                    (a v2 pack of every object reachable from the tips)
//! ```
//!
//! This version writes full (non-thin, no-prerequisite) bundles, expands
//! `--all` / `--branches` / `--tags`, retains annotated tag objects, and can
//! `verify`, `list-heads`, or `unbundle` any bounded v2 bundle. Prerequisite
//! (incremental) bundle creation and rev-range arguments remain deferred.

use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::{Parser, Subcommand};
use git_internal::{
    hash::{ObjectHash, get_hash_kind, set_hash_kind},
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            tag::Tag,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
        pack::{encode::PackEncoder, entry::Entry},
    },
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::mpsc;

use crate::{
    command::{index_pack, load_object},
    internal::{branch::Branch, db::get_db_conn_instance, head::Head, model::reference},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        path, util,
    },
};

const BUNDLE_SIGNATURE_V2: &str = "# v2 git bundle";
const MAX_BUNDLE_BYTES: u64 = 1 << 30;

pub const BUNDLE_EXAMPLES: &str = "\
EXAMPLES:
    libra bundle create repo.bundle main      Bundle everything reachable from main
    libra bundle create all.bundle --all      Bundle all local branches and tags
    libra bundle create tags.bundle --tags    Bundle every tag object and target
    libra bundle verify repo.bundle           Check a bundle's header and pack
    libra bundle list-heads repo.bundle       List the refs a bundle carries
    libra bundle unbundle repo.bundle         Import its pack objects and print heads";

/// Create and inspect Git v2 bundle files.
#[derive(Parser, Debug)]
#[command(after_help = BUNDLE_EXAMPLES)]
pub struct BundleArgs {
    #[command(subcommand)]
    pub command: BundleSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum BundleSubcommand {
    /// Write a bundle of everything reachable from the given revisions.
    Create {
        /// The bundle file to write.
        #[clap(value_name = "FILE")]
        file: PathBuf,
        /// Include every local branch and tag.
        #[clap(long)]
        all: bool,
        /// Include every local branch.
        #[clap(long)]
        branches: bool,
        /// Include every local tag, preserving annotated tag objects.
        #[clap(long)]
        tags: bool,
        /// Revisions whose reachable history to include (each becomes a head).
        #[clap(value_name = "REV")]
        revs: Vec<String>,
    },
    /// Check that a bundle's header is well-formed and its pack is present.
    Verify {
        #[clap(value_name = "FILE")]
        file: PathBuf,
    },
    /// Print the `<oid> <ref>` head lines a bundle carries.
    ListHeads {
        #[clap(value_name = "FILE")]
        file: PathBuf,
    },
    /// Import a bundle's pack objects, then print its advertised heads.
    Unbundle {
        #[clap(value_name = "FILE")]
        file: PathBuf,
    },
}

pub async fn execute(args: BundleArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: BundleArgs, _output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    match args.command {
        BundleSubcommand::Create {
            file,
            all,
            branches,
            tags,
            revs,
        } => create(&file, &revs, all, branches, tags).await,
        BundleSubcommand::Verify { file } => verify(&file),
        BundleSubcommand::ListHeads { file } => list_heads(&file),
        BundleSubcommand::Unbundle { file } => unbundle(&file),
    }
}

// ----------------------------------------------------------------------------
// create
// ----------------------------------------------------------------------------

#[derive(Clone)]
struct BundleHead {
    oid: ObjectHash,
    name: String,
}

async fn create(
    file: &Path,
    revs: &[String],
    all: bool,
    include_branches: bool,
    include_tags: bool,
) -> CliResult<()> {
    let heads =
        collect_bundle_heads(revs, all || include_branches, all || include_tags, all).await?;

    // Collect every object reachable from the tips (deduplicated).
    let mut seen: HashSet<ObjectHash> = HashSet::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut raw_object_bytes = 0u64;
    for head in &heads {
        collect_reachable_object(&head.oid, &mut seen, &mut entries, &mut raw_object_bytes)?;
    }

    if entries.is_empty() {
        return Err(
            CliError::fatal("bundle would contain no objects".to_string())
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }

    let pack = encode_pack(entries).await?;
    let header_bytes = BUNDLE_SIGNATURE_V2.len()
        + 2
        + heads
            .iter()
            .map(|head| head.oid.to_string().len() + 1 + head.name.len() + 1)
            .sum::<usize>();
    if (header_bytes as u64).saturating_add(pack.len() as u64) > MAX_BUNDLE_BYTES {
        return Err(CliError::fatal(format!(
            "bundle output exceeds the {MAX_BUNDLE_BYTES}-byte safety limit"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }

    // Write header + pack to a temporary file, then rename into place so a
    // failure never leaves a half-written bundle.
    let parent = file
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".bundle-{}.tmp", uuid::Uuid::new_v4()));

    let write_result = (|| -> std::io::Result<()> {
        let output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let mut out = std::io::BufWriter::new(output);
        writeln!(out, "{BUNDLE_SIGNATURE_V2}")?;
        for head in &heads {
            writeln!(out, "{} {}", head.oid, head.name)?;
        }
        out.write_all(b"\n")?;
        out.write_all(&pack)?;
        out.flush()?;
        out.get_ref().sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(write_err(error));
    }
    if let Err(error) = fs::rename(&tmp, file) {
        let _ = fs::remove_file(&tmp);
        return Err(write_err(error));
    }

    println!(
        "Created bundle '{}' with {} ref(s).",
        file.display(),
        heads.len()
    );
    Ok(())
}

async fn collect_bundle_heads(
    revs: &[String],
    include_branches: bool,
    include_tags: bool,
    include_head: bool,
) -> CliResult<Vec<BundleHead>> {
    let mut branches = Branch::list_branches_result(None).await.map_err(|error| {
        CliError::fatal(format!("failed to list branches for bundle: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    branches.sort_by(|left, right| left.name.cmp(&right.name));

    let db = get_db_conn_instance().await;
    let rows = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .all(&db)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to list tags for bundle: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    let mut tags = Vec::new();
    for row in rows {
        let name = row.name.ok_or_else(|| {
            CliError::fatal("bundle: stored tag is missing its ref name")
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let target = row.commit.ok_or_else(|| {
            CliError::fatal(format!("bundle: stored tag '{name}' is missing its target"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let oid = ObjectHash::from_str(&target).map_err(|error| {
            CliError::fatal(format!(
                "bundle: stored tag '{name}' has invalid target '{target}': {error}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        tags.push(BundleHead { oid, name });
    }
    tags.sort_by(|left, right| left.name.cmp(&right.name));

    let mut heads = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |head: BundleHead| {
        if seen.insert(head.name.clone()) {
            heads.push(head);
        }
    };
    if include_head && let Some(head) = current_head_advertisement().await? {
        add(head);
    }
    if include_branches {
        for branch in &branches {
            add(BundleHead {
                oid: branch.commit,
                name: full_branch_name(&branch.name),
            });
        }
    }
    if include_tags {
        for tag in &tags {
            add(tag.clone());
        }
    }

    for (index, rev) in revs.iter().enumerate() {
        if let Some(tag) = tags.iter().find(|tag| {
            tag.name == *rev
                || tag
                    .name
                    .strip_prefix("refs/tags/")
                    .is_some_and(|short| short == rev)
        }) {
            add(tag.clone());
            continue;
        }
        if let Some(branch) = branches
            .iter()
            .find(|branch| branch.name == *rev || full_branch_name(&branch.name) == *rev)
        {
            add(BundleHead {
                oid: branch.commit,
                name: full_branch_name(&branch.name),
            });
            continue;
        }
        let oid = util::get_commit_base(rev).await.map_err(|error| {
            CliError::fatal(format!("not a valid revision '{rev}': {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
        add(BundleHead {
            oid,
            name: advertised_name_for_rev(rev, index),
        });
    }

    if heads.is_empty() {
        return Err(CliError::command_usage(
            "bundle create requires at least one REV or --all/--branches/--tags",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    for head in &heads {
        if head.name != "HEAD"
            && (!head.name.starts_with("refs/") || !util::is_valid_refname(&head.name))
        {
            return Err(
                CliError::fatal(format!("bundle: invalid advertised ref '{}'", head.name))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::RepoCorrupt),
            );
        }
    }
    Ok(heads)
}

fn full_branch_name(name: &str) -> String {
    if name.starts_with("refs/heads/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    }
}

fn collect_reachable_object(
    root: &ObjectHash,
    seen: &mut HashSet<ObjectHash>,
    entries: &mut Vec<Entry>,
    raw_object_bytes: &mut u64,
) -> CliResult<()> {
    let storage = util::objects_storage();
    let mut stack = vec![*root];
    while let Some(oid) = stack.pop() {
        if seen.contains(&oid) {
            continue;
        }
        let object_type = storage
            .get_object_type(&oid)
            .map_err(|error| object_error(&oid, error))?;
        match object_type {
            ObjectType::Commit => {
                let commit: Commit =
                    load_object(&oid).map_err(|error| object_error(&oid, error))?;
                seen.insert(oid);
                stack.extend(commit.parent_commit_ids.iter().copied());
                collect_tree(&commit.tree_id, seen, entries, raw_object_bytes)?;
                let entry = serialize_entry(&commit, ObjectType::Commit, oid)?;
                push_bounded_entry(entry, entries, raw_object_bytes)?;
            }
            ObjectType::Tag => {
                let tag: Tag = load_object(&oid).map_err(|error| object_error(&oid, error))?;
                seen.insert(oid);
                stack.push(tag.object_hash);
                let entry = serialize_entry(&tag, ObjectType::Tag, oid)?;
                push_bounded_entry(entry, entries, raw_object_bytes)?;
            }
            ObjectType::Tree => collect_tree(&oid, seen, entries, raw_object_bytes)?,
            ObjectType::Blob => {
                if seen.insert(oid) {
                    let blob: Blob =
                        load_object(&oid).map_err(|error| object_error(&oid, error))?;
                    let entry = Entry {
                        obj_type: ObjectType::Blob,
                        data: blob.data,
                        hash: oid,
                        chain_len: 0,
                    };
                    push_bounded_entry(entry, entries, raw_object_bytes)?;
                }
            }
            other => {
                return Err(CliError::fatal(format!(
                    "bundle cannot encode object {oid} of type {other}"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt));
            }
        }
    }
    Ok(())
}

/// Recursively add a tree and everything beneath it to the object set.
fn collect_tree(
    tree_id: &ObjectHash,
    seen: &mut HashSet<ObjectHash>,
    entries: &mut Vec<Entry>,
    raw_object_bytes: &mut u64,
) -> CliResult<()> {
    let mut stack = vec![*tree_id];
    while let Some(current_tree_id) = stack.pop() {
        if !seen.insert(current_tree_id) {
            continue;
        }
        let tree: Tree =
            load_object(&current_tree_id).map_err(|error| object_error(&current_tree_id, error))?;
        for item in &tree.tree_items {
            match item.mode {
                TreeItemMode::Tree => stack.push(item.id),
                // A gitlink points at a commit in another repository; not our object.
                TreeItemMode::Commit => {}
                _ => {
                    if seen.insert(item.id) {
                        let blob: Blob =
                            load_object(&item.id).map_err(|error| object_error(&item.id, error))?;
                        let entry = Entry {
                            obj_type: ObjectType::Blob,
                            data: blob.data,
                            hash: item.id,
                            chain_len: 0,
                        };
                        push_bounded_entry(entry, entries, raw_object_bytes)?;
                    }
                }
            }
        }
        let entry = serialize_entry(&tree, ObjectType::Tree, current_tree_id)?;
        push_bounded_entry(entry, entries, raw_object_bytes)?;
    }
    Ok(())
}

fn serialize_entry<T: ObjectTrait>(
    object: &T,
    object_type: ObjectType,
    hash: ObjectHash,
) -> CliResult<Entry> {
    let data = object.to_data().map_err(|error| {
        CliError::fatal(format!(
            "failed to serialize {object_type} object {hash}: {error}"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    Ok(Entry {
        obj_type: object_type,
        data,
        hash,
        chain_len: 0,
    })
}

fn push_bounded_entry(
    entry: Entry,
    entries: &mut Vec<Entry>,
    raw_object_bytes: &mut u64,
) -> CliResult<()> {
    *raw_object_bytes = raw_object_bytes.saturating_add(entry.data.len() as u64);
    if *raw_object_bytes > MAX_BUNDLE_BYTES {
        return Err(CliError::fatal(format!(
            "bundle raw object data exceeds the {MAX_BUNDLE_BYTES}-byte safety limit"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    entries.push(entry);
    Ok(())
}

/// Encode objects into a v2 pack, hashing with the repository's hash kind.
async fn encode_pack(entries: Vec<Entry>) -> CliResult<Vec<u8>> {
    let count = entries.len();
    let (pack_tx, mut pack_rx) = mpsc::channel::<Vec<u8>>(128);
    let (entry_tx, entry_rx) = mpsc::channel::<MetaAttached<Entry, EntryMeta>>(128);
    let mut encoder = PackEncoder::new(count, 0, pack_tx);
    let kind = get_hash_kind();
    let encoder_handle = tokio::spawn(async move {
        set_hash_kind(kind);
        encoder.encode(entry_rx).await
    });
    let producer_handle = tokio::spawn(async move {
        for entry in entries {
            entry_tx
                .send(MetaAttached {
                    inner: entry,
                    meta: EntryMeta::new(),
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok::<(), String>(())
    });

    let mut bytes = Vec::new();
    while let Some(chunk) = pack_rx.recv().await {
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > MAX_BUNDLE_BYTES {
            producer_handle.abort();
            encoder_handle.abort();
            return Err(CliError::fatal(format!(
                "bundle pack exceeds the {MAX_BUNDLE_BYTES}-byte safety limit"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidTarget));
        }
        bytes.extend_from_slice(&chunk);
    }
    let producer_result = producer_handle.await;
    let encoder_result = encoder_handle.await;
    encoder_result
        .map_err(|error| {
            CliError::fatal(format!("pack encoder task failed: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
        })?
        .map_err(|error| {
            CliError::fatal(format!("pack encoding failed: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
        })?;
    producer_result
        .map_err(|error| {
            CliError::fatal(format!("pack producer task failed: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
        })?
        .map_err(|error| {
            CliError::fatal(format!("failed to feed pack encoder: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
        })?;
    Ok(bytes)
}

// ----------------------------------------------------------------------------
// verify / list-heads
// ----------------------------------------------------------------------------

/// The parsed text header of a bundle.
#[derive(Debug, Clone)]
pub(crate) struct BundleHeader {
    pub(crate) prerequisites: Vec<(String, String)>,
    pub(crate) heads: Vec<(String, String)>,
    /// Byte offset where the pack begins (just after the blank line).
    pub(crate) pack_offset: usize,
}

fn verify(file: &Path) -> CliResult<()> {
    let bytes = read_bundle_bounded(file, 1)?;
    let header = parse_header(&bytes, 1)?;
    verify_prerequisites(&header, 1)?;
    let pack = &bytes[header.pack_offset..];
    validate_bundle_pack(pack, 1)?;

    println!("{} is okay", file.display());
    for (oid, name) in &header.heads {
        println!("{oid} {name}");
    }
    Ok(())
}

fn list_heads(file: &Path) -> CliResult<()> {
    let bytes = read_bundle_bounded(file, 1)?;
    let header = parse_header(&bytes, 1)?;
    for (oid, name) in &header.heads {
        println!("{oid} {name}");
    }
    Ok(())
}

fn unbundle(file: &Path) -> CliResult<()> {
    let bytes = read_bundle_bounded(file, 128)?;
    let header = parse_header(&bytes, 128)?;
    verify_prerequisites(&header, 128)?;
    let pack = &bytes[header.pack_offset..];
    let checksum = validate_bundle_pack(pack, 128)?;

    let pack_dir = path::try_objects()
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to locate object store for unbundle: {error}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?
        .join("pack");
    fs::create_dir_all(&pack_dir).map_err(write_err)?;
    let final_pack = pack_dir.join(format!("pack-{checksum}.pack"));
    let final_index = pack_dir.join(format!("pack-{checksum}.idx"));
    if final_pack.exists() || final_index.exists() {
        if !(final_pack.is_file() && final_index.is_file()) {
            return Err(CliError::fatal(format!(
                "unbundle destination for pack {checksum} is incomplete; run 'libra fsck' before retrying"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        if !file_matches_bytes(&final_pack, pack).map_err(|error| read_err(error, 128))? {
            return Err(CliError::fatal(format!(
                "installed pack {checksum} does not match the validated bundle payload"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        let temp_index = pack_dir.join(format!(".bundle-{}.idx", uuid::Uuid::new_v4()));
        let index_result = build_bundle_index(&final_pack, &temp_index);
        if let Err(error) = index_result {
            let _ = fs::remove_file(&temp_index);
            return Err(error);
        }
        let index_matches =
            files_equal(&temp_index, &final_index).map_err(|error| read_err(error, 128));
        let _ = fs::remove_file(&temp_index);
        if !index_matches? {
            return Err(CliError::fatal(format!(
                "installed index for pack {checksum} is corrupt; run 'libra fsck' before retrying"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
    } else {
        let nonce = uuid::Uuid::new_v4();
        let temp_pack = pack_dir.join(format!(".bundle-{nonce}.pack"));
        let temp_index = pack_dir.join(format!(".bundle-{nonce}.idx"));
        let install_result = (|| -> CliResult<()> {
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_pack)
                .map_err(write_err)?;
            output.write_all(pack).map_err(write_err)?;
            output.sync_all().map_err(write_err)?;
            build_bundle_index(&temp_pack, &temp_index)?;
            fs::rename(&temp_pack, &final_pack).map_err(write_err)?;
            if let Err(error) = fs::rename(&temp_index, &final_index) {
                let _ = fs::remove_file(&final_pack);
                return Err(write_err(error));
            }
            Ok(())
        })();
        if let Err(error) = install_result {
            let _ = fs::remove_file(&temp_pack);
            let _ = fs::remove_file(&temp_index);
            return Err(error);
        }
    }

    let storage = util::objects_storage();
    for (oid, name) in &header.heads {
        let oid = ObjectHash::from_str(oid).map_err(|error| {
            CliError::fatal(format!(
                "bundle head '{name}' has invalid object id: {error}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        storage.get_object_type(&oid).map_err(|error| {
            CliError::fatal(format!(
                "unbundled head '{name}' points to unavailable object {oid}: {error}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
    }

    for (oid, name) in &header.heads {
        println!("{oid} {name}");
    }
    Ok(())
}

fn build_bundle_index(pack: &Path, index: &Path) -> CliResult<()> {
    let pack_name = pack.to_string_lossy().into_owned();
    let index_name = index.to_string_lossy().into_owned();
    match get_hash_kind() {
        git_internal::hash::HashKind::Sha1 => index_pack::build_index_v1(&pack_name, &index_name),
        git_internal::hash::HashKind::Sha256 | git_internal::hash::HashKind::Blake3 => {
            index_pack::build_index_v2(&pack_name, &index_name)
        }
    }
    .map_err(|error| {
        CliError::fatal(format!("failed to index bundle pack: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })
}

fn file_matches_bytes(path: &Path, expected: &[u8]) -> std::io::Result<bool> {
    if fs::metadata(path)?.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut input = fs::File::open(path)?;
    let mut offset = 0usize;
    let mut buffer = [0u8; 64 * 1024];
    while offset < expected.len() {
        let count = input.read(&mut buffer)?;
        let Some(end) = offset.checked_add(count) else {
            return Ok(false);
        };
        let Some(expected_chunk) = expected.get(offset..end) else {
            return Ok(false);
        };
        if count == 0 || buffer[..count] != *expected_chunk {
            return Ok(false);
        }
        offset = end;
    }
    Ok(true)
}

fn files_equal(left: &Path, right: &Path) -> std::io::Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_count = left.read(&mut left_buffer)?;
        let right_count = right.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

pub(crate) fn read_bundle_bounded(file: &Path, exit_code: i32) -> CliResult<Vec<u8>> {
    let input = fs::File::open(file).map_err(|error| read_err(error, exit_code))?;
    let size = input
        .metadata()
        .map_err(|error| read_err(error, exit_code))?
        .len();
    if size > MAX_BUNDLE_BYTES {
        return Err(CliError::fatal(format!(
            "bundle '{}' exceeds the {}-byte safety limit",
            file.display(),
            MAX_BUNDLE_BYTES
        ))
        .with_exit_code(exit_code)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    let mut bytes = Vec::with_capacity(size.min(8 << 20) as usize);
    let mut limited = input.take(MAX_BUNDLE_BYTES + 1);
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| read_err(error, exit_code))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(CliError::fatal(format!(
            "bundle '{}' grew beyond the {}-byte safety limit while reading",
            file.display(),
            MAX_BUNDLE_BYTES
        ))
        .with_exit_code(exit_code)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    Ok(bytes)
}

pub(crate) fn verify_prerequisites(header: &BundleHeader, exit_code: i32) -> CliResult<()> {
    let storage = util::objects_storage();
    let mut missing = Vec::new();
    for (oid, _) in &header.prerequisites {
        match ObjectHash::from_str(oid) {
            Ok(hash) if storage.get(&hash).is_ok() => {}
            _ => missing.push(oid.clone()),
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(CliError::fatal(format!(
        "bundle requires objects this repository does not have:\n  {}",
        missing.join("\n  ")
    ))
    .with_exit_code(exit_code)
    .with_stable_code(StableErrorCode::CliInvalidTarget))
}

pub(crate) fn validate_bundle_pack(pack: &[u8], exit_code: i32) -> CliResult<ObjectHash> {
    let hash_len = get_hash_kind().size();
    if pack.len() < 12 + hash_len || &pack[0..4] != b"PACK" || pack[4..8] != [0, 0, 0, 2] {
        return Err(
            CliError::fatal("bundle pack is missing or not a version-2 pack".to_string())
                .with_exit_code(exit_code)
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    let payload_len = pack.len() - hash_len;
    let expected = ObjectHash::from_bytes(&pack[payload_len..]).map_err(|error| {
        CliError::fatal(format!(
            "bundle pack has an invalid checksum trailer: {error}"
        ))
        .with_exit_code(exit_code)
        .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let actual = ObjectHash::new(&pack[..payload_len]);
    if actual != expected {
        return Err(CliError::fatal(format!(
            "bundle pack checksum mismatch: expected {expected}, computed {actual}"
        ))
        .with_exit_code(exit_code)
        .with_stable_code(StableErrorCode::RepoCorrupt));
    }
    Ok(expected)
}

/// Parse the text header up to the blank line that precedes the pack.
pub(crate) fn parse_header(bytes: &[u8], exit_code: i32) -> CliResult<BundleHeader> {
    // A malformed bundle is a verification failure (exit 1), matching
    // `git bundle verify` — exit 128 is reserved for usage errors.
    let invalid = |message: &str| {
        CliError::fatal(format!("not a valid bundle: {message}"))
            .with_exit_code(exit_code)
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    };

    let mut prerequisites = Vec::new();
    let mut heads = Vec::new();
    let mut head_names = HashSet::new();
    let mut offset = 0usize;
    let mut first_line = true;

    loop {
        let Some(nl) = bytes[offset..].iter().position(|&b| b == b'\n') else {
            return Err(invalid("missing header terminator"));
        };
        let line = &bytes[offset..offset + nl];
        let next = offset + nl + 1;

        if first_line {
            if line != BUNDLE_SIGNATURE_V2.as_bytes() {
                if line.starts_with(b"# v3 git bundle") {
                    return Err(invalid("v3 bundles are not supported"));
                }
                return Err(invalid("missing `# v2 git bundle` signature"));
            }
            first_line = false;
            offset = next;
            continue;
        }

        if line.is_empty() {
            // Blank line: header ends, pack begins.
            if heads.is_empty() {
                return Err(invalid("header advertises no heads"));
            }
            return Ok(BundleHeader {
                prerequisites,
                heads,
                pack_offset: next,
            });
        }

        let text = std::str::from_utf8(line).map_err(|_| invalid("non-UTF-8 header line"))?;
        if let Some(rest) = text.strip_prefix('-') {
            // Prerequisite: `-<oid> [comment]`.
            let (oid, comment) = split_oid_rest(rest);
            ObjectHash::from_str(&oid)
                .map_err(|_| invalid(&format!("invalid prerequisite object id '{oid}'")))?;
            prerequisites.push((oid, comment));
        } else {
            let (oid, name) = split_oid_rest(text);
            ObjectHash::from_str(&oid)
                .map_err(|_| invalid(&format!("invalid head object id '{oid}'")))?;
            if name != "HEAD" && (!name.starts_with("refs/") || !util::is_valid_refname(&name)) {
                return Err(invalid(&format!("invalid advertised ref '{name}'")));
            }
            if !head_names.insert(name.clone()) {
                return Err(invalid(&format!("duplicate advertised ref '{name}'")));
            }
            heads.push((oid, name));
        }
        offset = next;
    }
}

/// Split a header line into its leading oid and the remaining label/comment.
fn split_oid_rest(line: &str) -> (String, String) {
    match line.split_once(' ') {
        Some((oid, rest)) => (oid.to_string(), rest.to_string()),
        None => (line.to_string(), String::new()),
    }
}

// ----------------------------------------------------------------------------
// shared helpers
// ----------------------------------------------------------------------------

/// Advertised header name for an explicit revision after branch/tag lookup.
/// `HEAD` stays `HEAD` (Git bundle create), including when attached to a branch.
fn advertised_name_for_rev(rev: &str, index: usize) -> String {
    if rev == "HEAD" {
        "HEAD".to_string()
    } else if rev.starts_with("refs/") {
        rev.to_string()
    } else {
        format!("refs/heads/bundle-export-{}", index + 1)
    }
}

/// Current repository HEAD as a bundle advertisement, or `None` when unborn.
async fn current_head_advertisement() -> CliResult<Option<BundleHead>> {
    let oid = Head::current_commit_result().await.map_err(|error| {
        CliError::fatal(format!("failed to resolve HEAD for bundle: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    Ok(oid.map(|oid| BundleHead {
        oid,
        name: "HEAD".to_string(),
    }))
}

fn object_error(id: &ObjectHash, error: git_internal::errors::GitError) -> CliError {
    CliError::fatal(format!("failed to load object {id}: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::RepoCorrupt)
}

/// A bundle that cannot be read is a verification failure (exit 1), matching
/// `git bundle verify` — only usage errors use 128.
fn read_err(error: std::io::Error, exit_code: i32) -> CliError {
    CliError::fatal(format!("failed to read bundle: {error}"))
        .with_exit_code(exit_code)
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

fn write_err(error: std::io::Error) -> CliError {
    CliError::fatal(format!("failed to write bundle: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_name_for_explicit_head_is_head() {
        assert_eq!(advertised_name_for_rev("HEAD", 0), "HEAD");
        assert_eq!(
            advertised_name_for_rev("refs/heads/main", 0),
            "refs/heads/main"
        );
        assert_eq!(
            advertised_name_for_rev("orphan-oid", 2),
            "refs/heads/bundle-export-3"
        );
    }

    #[test]
    fn parses_a_v2_header_with_heads() {
        let oid = "a".repeat(get_hash_kind().hex_len());
        let bytes = format!("# v2 git bundle\n{oid} refs/heads/main\n\nPACK");
        let header = parse_header(bytes.as_bytes(), 1).unwrap();
        assert_eq!(header.heads, vec![(oid, "refs/heads/main".into())]);
        assert_eq!(&bytes.as_bytes()[header.pack_offset..], b"PACK");
    }

    #[test]
    fn parses_prerequisites() {
        let prerequisite = "d".repeat(get_hash_kind().hex_len());
        let head = "b".repeat(get_hash_kind().hex_len());
        let bytes =
            format!("# v2 git bundle\n-{prerequisite} comment here\n{head} refs/heads/x\n\nPACK");
        let header = parse_header(bytes.as_bytes(), 1).unwrap();
        assert_eq!(header.prerequisites.len(), 1);
        assert_eq!(header.prerequisites[0].0, prerequisite);
        assert_eq!(header.heads.len(), 1);
    }

    #[test]
    fn rejects_a_missing_signature() {
        assert!(parse_header(b"not a bundle\n\nPACK", 1).is_err());
    }

    #[test]
    fn raw_object_accounting_fails_before_retaining_an_oversized_entry() {
        let mut entries = Vec::new();
        let mut raw_object_bytes = MAX_BUNDLE_BYTES - 1;
        let entry = Entry {
            obj_type: ObjectType::Blob,
            data: vec![0, 1],
            hash: ObjectHash::default(),
            chain_len: 0,
        };

        assert!(push_bounded_entry(entry, &mut entries, &mut raw_object_bytes).is_err());
        assert!(entries.is_empty());
    }

    #[test]
    fn rejects_v3() {
        assert!(parse_header(b"# v3 git bundle\n\nPACK", 1).is_err());
    }
}
