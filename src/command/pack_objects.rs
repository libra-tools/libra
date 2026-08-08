//! `libra pack-objects` — hidden plumbing: read a list of object ids from stdin
//! and encode them into a single pack via the shared
//! [`crate::internal::pack_writer`].
//!
//! This command is intentionally hidden from the public command surface (it is
//! for internal / integration use, not part of Libra's Git-compatibility
//! promise). It reads one object id per line from stdin and either writes the
//! pack into `objects/pack` (printing its checksum name) or streams the raw pack
//! bytes to stdout with `--stdout` (for piping into `libra index-pack`).

use std::io::{Read, Write};

use clap::Parser;
use git_internal::hash::{ObjectHash, get_hash_kind};

use crate::{
    command::maintenance::parse_object_hash,
    internal::pack_writer,
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        path, util,
    },
};

pub const PACK_OBJECTS_EXAMPLES: &str = "\
EXAMPLES:
    libra rev-list --objects HEAD | libra pack-objects        Pack the listed objects
    libra pack-objects --stdout < ids.txt > out.pack          Stream the pack to stdout";

/// Build a pack from object ids read on stdin (hidden plumbing command).
#[derive(Parser, Debug)]
#[command(after_help = PACK_OBJECTS_EXAMPLES)]
pub struct PackObjectsArgs {
    /// Write the raw pack bytes to stdout instead of into `objects/pack`.
    #[arg(long)]
    pub stdout: bool,
}

pub async fn execute(args: PackObjectsArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: PackObjectsArgs, output: &OutputConfig) -> CliResult<()> {
    // Confirm we are inside a repository before touching storage.
    util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
    let storage = ClientStorage::init(path::objects());
    let hash_kind = get_hash_kind();

    // Read object ids (one per line, whitespace-tolerant) from stdin.
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| CliError::fatal(format!("failed to read object ids from stdin: {e}")))?;

    let mut hashes: Vec<ObjectHash> = Vec::new();
    for token in input.split_whitespace() {
        // `rev-list --objects` prints `<id> <path>`; take the leading id and
        // ignore anything that does not parse as an object id.
        if let Some(hash) = parse_object_hash(token) {
            hashes.push(hash);
        }
    }

    if hashes.is_empty() {
        return Err(CliError::command_usage("no object ids provided on stdin")
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    if args.stdout {
        let pack_bytes = pack_writer::encode_hashes_to_pack(&storage, &hashes, hash_kind)
            .await
            .map_err(|e| CliError::fatal(format!("failed to encode pack: {e}")))?
            .ok_or_else(|| CliError::fatal("no objects to pack"))?;
        std::io::stdout()
            .write_all(&pack_bytes)
            .map_err(|e| CliError::fatal(format!("failed to write pack to stdout: {e}")))?;
        return Ok(());
    }

    let pack_dir = path::objects().join("pack");
    let publication = pack_writer::write_pack_with_index(&storage, &hashes, &pack_dir, hash_kind)
        .await
        .map_err(|e| CliError::fatal(format!("failed to write pack: {e}")))?
        .ok_or_else(|| CliError::fatal("no objects to pack"))?;

    // Git's `pack-objects` prints the packed checksum to stdout; mirror that by
    // printing the `pack-<checksum>` stem so callers can locate the new pack.
    if let Some(stem) = publication.path.file_stem().and_then(|s| s.to_str())
        && !output.quiet
    {
        println!("{stem}");
    }
    Ok(())
}
