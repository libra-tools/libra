//! `libra fast-import` — import a `git fast-import` stream, a focused subset of
//! `git fast-import`.
//!
//! Supported directives: `blob`, `commit <ref>` (`mark`, `author`, `committer`,
//! `data`, `from`, `merge`, `M`, `D`, `C`, `R`, `N`, `deleteall`), annotated
//! `tag`, `reset <ref>` (`from`), `checkpoint`, `done`, and the lenient preamble
//! `feature` / `option` / `progress` (ignored). `M ... inline` and `N inline`
//! consume their following data block without materialising the whole stream.
//!
//! Safety / resource bounds:
//! - Total input is capped (default 1 GiB, `fastimport.maxInputSize`).
//! - The number of top-level blobs, commits, and tags created is capped
//!   (default 1_000_000, raise with `--max-count`); trees are derived and
//!   written through the shared `write-tree` path and are not separately counted.
//! - Refs must be `refs/…`, valid, and never point outside the repository.
//! - Object ids must match the repository hash length; duplicate marks are
//!   rejected.
//!
//! Transaction model: objects are written immediately, but branch/tag/note
//! updates are buffered and only applied at a `checkpoint`, at `done`, or at a clean EOF.
//! A stream truncated mid-object fails before that flush, so refs are never
//! left half-updated; the orphaned objects are reclaimed by a later
//! `libra gc` (recover with `libra fsck` + `libra gc`).

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    str::FromStr,
};

use clap::Parser;
use git_internal::{
    hash::{ObjectHash, get_hash_kind},
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tag::Tag,
        tree::{TreeItem, TreeItemMode},
        types::ObjectType,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, Set, Statement,
};

use crate::{
    command::{load_object, save_object},
    common_utils::format_commit_msg,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        model::reference,
        tree_plumbing,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        util,
    },
};

const DEFAULT_MAX_INPUT_BYTES: u64 = 1 << 30; // 1 GiB
const DEFAULT_MAX_OBJECTS: u64 = 1_000_000;
const MAX_COMMAND_LINE_BYTES: u64 = 1 << 20; // paths/identities/refs, not data payloads

pub const FAST_IMPORT_EXAMPLES: &str = "\
EXAMPLES:
    libra fast-export main | libra fast-import   Round-trip history through a stream
    libra fast-import < repo.fastimport          Import a saved stream
    libra fast-import --input repo.fastimport    Import from a file";

/// Import a `git fast-import` stream into the repository.
#[derive(Parser, Debug)]
#[command(after_help = FAST_IMPORT_EXAMPLES)]
pub struct FastImportArgs {
    /// Read the stream from a file instead of stdin.
    #[clap(long, value_name = "FILE")]
    pub input: Option<PathBuf>,

    /// Set the maximum number of top-level blobs, commits, and tags to create.
    #[clap(long, value_name = "N")]
    pub max_count: Option<u64>,

    /// Suppress the final summary.
    #[clap(long)]
    pub quiet: bool,
}

pub async fn execute(args: FastImportArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: FastImportArgs, _output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let max_input = configured_max_input().await?;
    let max_objects = args.max_count.unwrap_or(DEFAULT_MAX_OBJECTS);

    let reader: Box<dyn Read> = match &args.input {
        Some(path) => Box::new(fs::File::open(path).map_err(|error| {
            CliError::fatal(format!("cannot open '{}': {error}", path.display()))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?),
        None => Box::new(std::io::stdin()),
    };

    let mut importer = Importer::new(BufReader::new(reader), max_input, max_objects);
    let stats = importer.run().await?;

    if !args.quiet {
        println!(
            "fast-import: {} objects, {} ref(s) updated",
            stats.objects, stats.refs
        );
    }
    Ok(())
}

/// Read `fastimport.maxInputSize` (bytes), falling back to 1 GiB.
async fn configured_max_input() -> CliResult<u64> {
    let entry = ConfigKv::get("fastimport.maxInputSize")
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to read config 'fastimport.maxInputSize': {error}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    let Some(entry) = entry else {
        return Ok(DEFAULT_MAX_INPUT_BYTES);
    };
    let value = entry.value.trim().parse::<u64>().map_err(|error| {
        CliError::command_usage(format!(
            "invalid fastimport.maxInputSize '{}': {error}",
            entry.value
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
    })?;
    if value == 0 {
        return Err(
            CliError::command_usage("fastimport.maxInputSize must be greater than zero")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }
    Ok(value)
}

struct ImportStats {
    objects: u64,
    refs: u64,
}

/// Streaming importer over a single fast-import stream.
struct Importer<R: BufRead> {
    reader: R,
    bytes_read: u64,
    max_input: u64,
    objects: u64,
    max_objects: u64,
    hash_hex_len: usize,
    /// `:N` mark → object id.
    marks: HashMap<u32, ObjectHash>,
    /// Ref updates staged since the last flush (committed at checkpoint/done/EOF).
    pending_refs: HashMap<String, Option<ObjectHash>>,
    /// (`refs/notes/*`, annotated object) → replacement blob; `None` deletes.
    pending_notes: HashMap<(String, String), Option<ObjectHash>>,
    /// Notes refs whose Git tree-shaped snapshot replaces the stored mapping.
    pending_note_replacements: HashSet<String>,
    /// Total refs actually written.
    refs_written: u64,
    /// One-line push-back buffer for lookahead during a `commit` body.
    pending_line: Option<Vec<u8>>,
}

impl<R: BufRead> Importer<R> {
    fn new(reader: R, max_input: u64, max_objects: u64) -> Self {
        Importer {
            reader,
            bytes_read: 0,
            max_input,
            objects: 0,
            max_objects,
            hash_hex_len: get_hash_kind().hex_len(),
            marks: HashMap::new(),
            pending_refs: HashMap::new(),
            pending_notes: HashMap::new(),
            pending_note_replacements: HashSet::new(),
            refs_written: 0,
            pending_line: None,
        }
    }

    async fn run(&mut self) -> CliResult<ImportStats> {
        while let Some(line) = self.next_line()? {
            let line = trim_newline(&line);
            if line.is_empty() {
                continue;
            }
            let text = std::str::from_utf8(line)
                .map_err(|_| self.fatal("non-UTF-8 command line"))?
                .to_string();
            let (cmd, rest) = split_first(&text);
            match cmd {
                "blob" => self.cmd_blob()?,
                "commit" => self.cmd_commit(rest).await?,
                "tag" => self.cmd_tag(rest)?,
                "reset" => self.cmd_reset(rest)?,
                "checkpoint" => self.flush_refs().await?,
                "done" => {
                    self.flush_refs().await?;
                    return Ok(self.stats());
                }
                // Lenient preamble / no-ops.
                "feature" | "option" => {}
                "progress" => eprintln!("{text}"),
                other => {
                    return Err(self.fatal(&format!("unsupported fast-import command '{other}'")));
                }
            }
        }
        // Clean EOF (the stream ended on a command boundary) commits the rest.
        self.flush_refs().await?;
        Ok(self.stats())
    }

    fn stats(&self) -> ImportStats {
        ImportStats {
            objects: self.objects,
            refs: self.refs_written,
        }
    }

    // ------------------------------------------------------------------
    // blob
    // ------------------------------------------------------------------

    fn cmd_blob(&mut self) -> CliResult<()> {
        let mark = self.read_optional_mark()?;
        let data = self.read_data_line()?;
        let blob = git_internal::internal::object::blob::Blob::from_content_bytes(data);
        self.save(&blob, blob.id)?;
        if let Some(mark) = mark {
            self.set_mark(mark, blob.id)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // commit
    // ------------------------------------------------------------------

    async fn cmd_commit(&mut self, refspec: &str) -> CliResult<()> {
        let refname = self.validate_ref(refspec)?;

        let mark = self.read_optional_mark()?;
        let author_line = self.read_optional_prefixed("author")?;
        let committer_line = self
            .read_optional_prefixed("committer")?
            .ok_or_else(|| self.fatal("commit is missing a committer"))?;
        let message = match self.read_required_data()? {
            Some(data) => data,
            None => return Err(self.fatal("commit is missing its message data")),
        };

        let committer = parse_signature("committer", &committer_line)
            .map_err(|message| self.fatal(&message))?;
        let author = match &author_line {
            Some(line) => {
                parse_signature("author", line).map_err(|message| self.fatal(&message))?
            }
            None => {
                // Default the author to the committer, but as an `author`
                // signature — otherwise it would serialize as a second
                // `committer` line and the commit would not parse back.
                let mut author = committer.clone();
                author.signature_type = SignatureType::Author;
                author
            }
        };

        // Build the commit's tree state from its first parent, then apply ops.
        let mut parents: Vec<ObjectHash> = Vec::new();
        let mut state: HashMap<String, (TreeItemMode, ObjectHash)> = HashMap::new();
        let is_notes_ref = refname.starts_with("refs/notes/");
        let mut saw_note_modify = false;
        let mut saw_tree_note_change = false;

        // `from` (first parent) and `merge` (extra parents).
        while let Some(raw) = self.next_line()? {
            let line = trim_newline(&raw);
            if line.is_empty() {
                continue;
            }
            let text = std::str::from_utf8(line)
                .map_err(|_| self.fatal("non-UTF-8 commit body line"))?
                .to_string();
            let (kw, rest) = split_first(&text);
            match kw {
                "from" => {
                    let parent = self.resolve_commitish(rest)?;
                    self.load_tree_state(&parent, &mut state)?;
                    parents.clear();
                    parents.insert(0, parent);
                }
                "merge" => {
                    let parent = self.resolve_commitish(rest)?;
                    load_object::<Commit>(&parent).map_err(|error| {
                        self.fatal(&format!(
                            "merge parent {parent} is not a readable commit: {error}"
                        ))
                    })?;
                    parents.push(parent);
                }
                _ => {
                    self.pending_line = Some(raw);
                    break;
                }
            }
        }

        // File changes.
        while let Some(raw) = self.next_line()? {
            let line = trim_newline(&raw);
            if line.is_empty() {
                break; // a blank line terminates the commit body
            }
            let text = std::str::from_utf8(line)
                .map_err(|_| self.fatal("non-UTF-8 commit body line"))?
                .to_string();
            let (kw, rest) = split_first(&text);
            match kw {
                "M" => {
                    self.apply_modify(rest, &mut state)?;
                    saw_tree_note_change |= is_notes_ref;
                }
                "D" => {
                    self.apply_delete(rest, &mut state)?;
                    saw_tree_note_change |= is_notes_ref;
                }
                "C" => {
                    self.apply_copy_or_rename(rest, &mut state, false)?;
                    saw_tree_note_change |= is_notes_ref;
                }
                "R" => {
                    self.apply_copy_or_rename(rest, &mut state, true)?;
                    saw_tree_note_change |= is_notes_ref;
                }
                "N" => {
                    if is_notes_ref && parents.is_empty() && !saw_note_modify {
                        self.begin_notes_replacement(&refname);
                    }
                    self.apply_note_modify(&refname, rest)?;
                    saw_note_modify = true;
                }
                "deleteall" => {
                    state.clear();
                    saw_tree_note_change |= is_notes_ref;
                }
                "blob" | "commit" | "reset" | "tag" | "checkpoint" | "done" | "feature"
                | "option" | "progress" => {
                    // Next top-level command — hand it back to the main loop.
                    self.pending_line = Some(raw);
                    break;
                }
                other => {
                    return Err(self.fatal(&format!(
                        "unsupported file-change command '{other}' in commit"
                    )));
                }
            }
        }

        if saw_note_modify && saw_tree_note_change {
            return Err(
                self.fatal("a notes commit cannot mix `N` commands with tree-shaped file changes")
            );
        }
        if saw_tree_note_change {
            self.stage_notes_tree(&refname, &state)?;
        }

        let tree_id = tree_plumbing::write_tree_from_leaves(
            state
                .into_iter()
                .map(|(path, (mode, id))| (PathBuf::from(path), mode, id)),
        )
        .map_err(|error| self.fatal(&format!("failed to write tree: {error}")))?;

        let message = message_string(&message).map_err(|message| self.fatal(&message))?;
        let mut commit = Commit {
            id: ObjectHash::default(),
            author,
            committer,
            tree_id,
            parent_commit_ids: parents,
            message: format_commit_msg(&message, None),
        };
        let commit_data = commit
            .to_data()
            .map_err(|error| self.fatal(&format!("failed to serialize commit: {error}")))?;
        commit.id = ObjectHash::from_type_and_data(ObjectType::Commit, &commit_data);
        self.save(&commit, commit.id)?;
        if let Some(mark) = mark {
            self.set_mark(mark, commit.id)?;
        }
        if !is_notes_ref {
            self.pending_refs.insert(refname, Some(commit.id));
        }
        Ok(())
    }

    fn apply_modify(
        &mut self,
        rest: &str,
        state: &mut HashMap<String, (TreeItemMode, ObjectHash)>,
    ) -> CliResult<()> {
        // `M <mode> <dataref> <path>`
        let mut parts = rest.splitn(3, ' ');
        let mode = parts.next().unwrap_or_default();
        let dataref = parts
            .next()
            .ok_or_else(|| self.fatal("malformed `M` (expected `M <mode> <dataref> <path>`)"))?;
        let path = parts
            .next()
            .ok_or_else(|| self.fatal("malformed `M` (missing path)"))?;
        let mode = TreeItemMode::tree_item_type_from_bytes(mode.as_bytes())
            .map_err(|_| self.fatal(&format!("invalid file mode '{mode}'")))?;
        let id = if dataref == "inline" {
            let data = self.read_data_line()?;
            let blob = git_internal::internal::object::blob::Blob::from_content_bytes(data);
            self.save(&blob, blob.id)?;
            blob.id
        } else {
            self.resolve_dataref(dataref)?
        };
        let expected_type = match mode {
            TreeItemMode::Blob | TreeItemMode::BlobExecutable | TreeItemMode::Link => {
                ObjectType::Blob
            }
            TreeItemMode::Commit => ObjectType::Commit,
            TreeItemMode::Tree => {
                return Err(self.fatal(
                    "directory mode 040000 is not supported by `M`; modify its child paths",
                ));
            }
        };
        let actual_type = util::objects_storage()
            .get_object_type(&id)
            .map_err(|error| {
                self.fatal(&format!("cannot inspect modified object {id}: {error}"))
            })?;
        if actual_type != expected_type {
            return Err(self.fatal(&format!(
                "mode {mode:?} requires a {expected_type}, but {id} is {actual_type}"
            )));
        }
        let path = parse_single_path(path).map_err(|message| self.fatal(&message))?;
        prepare_destination(&path, state);
        state.insert(path, (mode, id));
        Ok(())
    }

    fn apply_delete(
        &self,
        rest: &str,
        state: &mut HashMap<String, (TreeItemMode, ObjectHash)>,
    ) -> CliResult<()> {
        let path = parse_single_path(rest).map_err(|message| self.fatal(&message))?;
        remove_path_and_subtree(&path, state);
        Ok(())
    }

    fn apply_copy_or_rename(
        &self,
        rest: &str,
        state: &mut HashMap<String, (TreeItemMode, ObjectHash)>,
        rename: bool,
    ) -> CliResult<()> {
        let (source, tail) = parse_path_token(rest).map_err(|message| self.fatal(&message))?;
        let (destination, tail) = parse_path_token(tail).map_err(|message| self.fatal(&message))?;
        if !tail.trim().is_empty() {
            return Err(self.fatal("copy/rename has trailing path data"));
        }
        if source == destination
            || destination.starts_with(&format!("{source}/"))
            || source.starts_with(&format!("{destination}/"))
        {
            return Err(self.fatal("copy/rename source and destination overlap"));
        }

        let source_prefix = format!("{source}/");
        let mut copied = state
            .iter()
            .filter_map(|(path, value)| {
                if path == &source {
                    Some((destination.clone(), *value))
                } else {
                    path.strip_prefix(&source_prefix)
                        .map(|suffix| (format!("{destination}/{suffix}"), *value))
                }
            })
            .collect::<Vec<_>>();
        if copied.is_empty() {
            return Err(self.fatal(&format!("copy/rename source '{source}' does not exist")));
        }
        copied.sort_by(|left, right| left.0.cmp(&right.0));
        prepare_destination(&destination, state);
        if rename {
            remove_path_and_subtree(&source, state);
        }
        state.extend(copied);
        Ok(())
    }

    fn apply_note_modify(&mut self, refname: &str, rest: &str) -> CliResult<()> {
        if !refname.starts_with("refs/notes/") {
            return Err(self.fatal("`N` note modify is only valid in a refs/notes/* commit"));
        }
        let (dataref, target) = split_first(rest.trim());
        if dataref.is_empty() || target.trim().is_empty() {
            return Err(self.fatal("malformed `N` (expected `N <dataref> <commit-ish>`)"));
        }
        let object = self.resolve_commitish(target.trim())?;
        let blob = if dataref == "inline" {
            let data = self.read_data_line()?;
            let blob = git_internal::internal::object::blob::Blob::from_content_bytes(data);
            self.save(&blob, blob.id)?;
            Some(blob.id)
        } else if is_zero_object_id(dataref, self.hash_hex_len) {
            None
        } else {
            let blob = self.resolve_dataref(dataref)?;
            let object_type = util::objects_storage()
                .get_object_type(&blob)
                .map_err(|error| {
                    self.fatal(&format!("cannot inspect note blob {blob}: {error}"))
                })?;
            if object_type != ObjectType::Blob {
                return Err(
                    self.fatal(&format!("note data {blob} is {object_type}, expected blob"))
                );
            }
            Some(blob)
        };
        self.pending_notes
            .insert((refname.to_string(), object.to_string()), blob);
        Ok(())
    }

    fn stage_notes_tree(
        &mut self,
        refname: &str,
        state: &HashMap<String, (TreeItemMode, ObjectHash)>,
    ) -> CliResult<()> {
        self.begin_notes_replacement(refname);
        for (path, (mode, blob)) in state {
            if *mode != TreeItemMode::Blob {
                return Err(self.fatal(&format!(
                    "notes tree path '{path}' has mode {mode:?}, expected 100644"
                )));
            }
            let object_text = path.replace('/', "");
            if object_text.len() != self.hash_hex_len
                || !object_text.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(self.fatal(&format!(
                    "notes tree path '{path}' is not a {}-digit object id",
                    self.hash_hex_len
                )));
            }
            let object = ObjectHash::from_str(&object_text).map_err(|error| {
                self.fatal(&format!("invalid notes object path '{path}': {error}"))
            })?;
            let object_type = util::objects_storage()
                .get_object_type(blob)
                .map_err(|error| {
                    self.fatal(&format!("cannot inspect note blob {blob}: {error}"))
                })?;
            if object_type != ObjectType::Blob {
                return Err(self.fatal(&format!(
                    "notes tree entry {blob} is {object_type}, expected blob"
                )));
            }
            self.pending_notes
                .insert((refname.to_string(), object.to_string()), Some(*blob));
        }
        Ok(())
    }

    fn begin_notes_replacement(&mut self, refname: &str) {
        self.pending_note_replacements.insert(refname.to_string());
        self.pending_notes
            .retain(|(pending_ref, _), _| pending_ref != refname);
    }

    // ------------------------------------------------------------------
    // annotated tag
    // ------------------------------------------------------------------

    fn cmd_tag(&mut self, name: &str) -> CliResult<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(self.fatal("tag is missing its name"));
        }
        let refname = if name.starts_with("refs/tags/") {
            name.to_string()
        } else {
            format!("refs/tags/{name}")
        };
        self.validate_ref(&refname)?;

        let mark = self.read_optional_mark()?;
        let from = self
            .read_optional_prefixed("from")?
            .ok_or_else(|| self.fatal("tag is missing `from`"))?;
        let target = self.resolve_dataref(&from)?;
        let tagger_line = self
            .read_optional_prefixed("tagger")?
            .ok_or_else(|| self.fatal("tag is missing a tagger"))?;
        let tagger =
            parse_signature("tagger", &tagger_line).map_err(|message| self.fatal(&message))?;
        let message = self
            .read_required_data()?
            .ok_or_else(|| self.fatal("tag is missing its message data"))?;
        let object_type = util::objects_storage()
            .get_object_type(&target)
            .map_err(|error| self.fatal(&format!("cannot inspect tag target {target}: {error}")))?;
        if !matches!(
            object_type,
            ObjectType::Commit | ObjectType::Tree | ObjectType::Blob | ObjectType::Tag
        ) {
            return Err(self.fatal(&format!(
                "unsupported annotated-tag target type {object_type}"
            )));
        }
        let short_name = refname
            .strip_prefix("refs/tags/")
            .ok_or_else(|| self.fatal("tag ref must start with refs/tags/"))?;
        let message = message_string(&message).map_err(|message| self.fatal(&message))?;
        let mut tag = Tag {
            id: ObjectHash::default(),
            object_hash: target,
            object_type,
            tag_name: short_name.to_string(),
            tagger,
            message,
        };
        let tag_data = tag
            .to_data()
            .map_err(|error| self.fatal(&format!("failed to serialize tag: {error}")))?;
        tag.id = ObjectHash::from_type_and_data(ObjectType::Tag, &tag_data);
        self.save(&tag, tag.id)?;
        if let Some(mark) = mark {
            self.set_mark(mark, tag.id)?;
        }
        self.pending_refs.insert(refname, Some(tag.id));
        Ok(())
    }

    // ------------------------------------------------------------------
    // reset
    // ------------------------------------------------------------------

    fn cmd_reset(&mut self, refspec: &str) -> CliResult<()> {
        let refname = self.validate_ref(refspec)?;
        // An optional `from` sets the target. Without it, reset deletes the ref.
        let mut target = None;
        if let Some(raw) = self.next_line()? {
            let line = trim_newline(&raw);
            let text = std::str::from_utf8(line)
                .map_err(|_| self.fatal("non-UTF-8 reset body"))?
                .to_string();
            let (kw, rest) = split_first(&text);
            if kw == "from" {
                target = Some(self.resolve_dataref(rest)?);
            } else if !line.is_empty() {
                self.pending_line = Some(raw);
            }
        }
        if refname.starts_with("refs/notes/") {
            match target {
                Some(commit_id) => {
                    let mut state = HashMap::new();
                    self.load_tree_state(&commit_id, &mut state)?;
                    self.stage_notes_tree(&refname, &state)?;
                }
                None => self.begin_notes_replacement(&refname),
            }
        } else {
            if let (Some(commit_id), true) = (target, refname.starts_with("refs/heads/")) {
                load_object::<Commit>(&commit_id).map_err(|error| {
                    self.fatal(&format!(
                        "branch reset target {commit_id} is not a readable commit: {error}"
                    ))
                })?;
            } else if let Some(object_id) = target {
                util::objects_storage()
                    .get_object_type(&object_id)
                    .map_err(|error| {
                        self.fatal(&format!(
                            "reset target {object_id} is not a readable object: {error}"
                        ))
                    })?;
            }
            self.pending_refs.insert(refname, target);
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // transaction
    // ------------------------------------------------------------------

    async fn flush_refs(&mut self) -> CliResult<()> {
        if self.pending_refs.is_empty()
            && self.pending_notes.is_empty()
            && self.pending_note_replacements.is_empty()
        {
            return Ok(());
        }
        let pending_refs = self.pending_refs.clone();
        let pending_notes = self.pending_notes.clone();
        let pending_note_replacements = self.pending_note_replacements.clone();
        let storage = util::objects_storage();
        for ((notes_ref, object), blob) in &pending_notes {
            if blob.is_none() {
                continue;
            }
            let object_id = ObjectHash::from_str(object).map_err(|error| {
                self.fatal(&format!(
                    "invalid noted object '{object}' in {notes_ref}: {error}"
                ))
            })?;
            storage.get_object_type(&object_id).map_err(|error| {
                self.fatal(&format!(
                    "noted object {object_id} in {notes_ref} is unavailable: {error}"
                ))
            })?;
        }
        // Part C W0 (§C.11): the batch flush rewrites/deletes shared branch
        // refs. Refuse before the transaction if any target branch is checked
        // out in ANOTHER worktree — its HEAD would dangle or its working tree
        // would silently diverge. `branch_checked_out_elsewhere` excludes the
        // current worktree, so importing into this worktree's own branch stays
        // allowed.
        for refname in pending_refs.keys() {
            let Some(branch) = refname.strip_prefix("refs/heads/") else {
                continue;
            };
            // §C.4.4: fail CLOSED — a probe failure must not read as
            // "no other worktree has it" right before a batch ref rewrite.
            let checked_out =
                crate::internal::head::Head::branch_checked_out_elsewhere_result(branch)
                    .await
                    .map_err(|error| {
                        self.fatal(&format!(
                            "cannot determine whether branch '{branch}' is checked out in \
                             another worktree: {error}"
                        ))
                    })?;
            if let Some(other) = checked_out {
                return Err(self.fatal(&format!(
                    "cannot import into branch '{branch}': it is checked out at worktree '{other}'"
                )));
            }
        }
        let db = get_db_conn_instance().await;
        let written = crate::internal::db::write_transaction(&db, |txn| {
                Box::pin(async move {
                    let mut written = 0u64;
                    for (refname, oid) in &pending_refs {
                        if let Some(branch) = refname.strip_prefix("refs/heads/") {
                            match oid {
                                Some(oid) => {
                                    Branch::update_branch_with_conn(
                                        txn,
                                        branch,
                                        &oid.to_string(),
                                        None,
                                    )
                                    .await?;
                                }
                                None => {
                                    match Branch::delete_branch_result_with_conn(txn, branch, None)
                                        .await
                                    {
                                        Ok(()) | Err(BranchStoreError::NotFound(_)) => {}
                                        Err(error) => return Err(DbErr::Custom(error.to_string())),
                                    }
                                }
                            }
                            written += 1;
                            continue;
                        }
                        if refname.starts_with("refs/tags/") {
                            match oid {
                                Some(oid) => {
                                    let existing = reference::Entity::find()
                                        .filter(reference::Column::Name.eq(refname.clone()))
                                        .filter(
                                            reference::Column::Kind
                                                .eq(reference::ConfigKind::Tag),
                                        )
                                        .one(txn)
                                        .await?;
                                    match existing {
                                        Some(row) => {
                                            let mut active: reference::ActiveModel = row.into();
                                            active.commit = Set(Some(oid.to_string()));
                                            active.update(txn).await?;
                                        }
                                        None => {
                                            reference::ActiveModel {
                                                name: Set(Some(refname.clone())),
                                                kind: Set(reference::ConfigKind::Tag),
                                                commit: Set(Some(oid.to_string())),
                                                ..Default::default()
                                            }
                                            .insert(txn)
                                            .await?;
                                        }
                                    }
                                }
                                None => {
                                    reference::Entity::delete_many()
                                        .filter(reference::Column::Name.eq(refname.clone()))
                                        .filter(
                                            reference::Column::Kind
                                                .eq(reference::ConfigKind::Tag),
                                        )
                                        .exec(txn)
                                        .await?;
                                }
                            }
                            written += 1;
                            continue;
                        }
                        return Err(DbErr::Custom(format!(
                            "fast-import cannot persist ref namespace '{refname}'"
                        )));
                    }

                    let mut note_refs = pending_note_replacements.clone();
                    for notes_ref in &pending_note_replacements {
                        txn.execute_raw(Statement::from_sql_and_values(
                            sea_orm::DatabaseBackend::Sqlite,
                            "DELETE FROM notes WHERE notes_ref = ?",
                            [notes_ref.clone().into()],
                        ))
                        .await?;
                    }
                    for ((notes_ref, object), blob) in &pending_notes {
                        note_refs.insert(notes_ref.clone());
                        match blob {
                            Some(blob) => {
                                txn.execute_raw(Statement::from_sql_and_values(
                                    sea_orm::DatabaseBackend::Sqlite,
                                    "INSERT INTO notes (notes_ref, object, blob) VALUES (?, ?, ?) \
                                     ON CONFLICT(notes_ref, object) DO UPDATE SET blob = excluded.blob",
                                    [
                                        notes_ref.clone().into(),
                                        object.clone().into(),
                                        blob.to_string().into(),
                                    ],
                                ))
                                .await?;
                            }
                            None => {
                                txn.execute_raw(Statement::from_sql_and_values(
                                    sea_orm::DatabaseBackend::Sqlite,
                                    "DELETE FROM notes WHERE notes_ref = ? AND object = ?",
                                    [notes_ref.clone().into(), object.clone().into()],
                                ))
                                .await?;
                            }
                        }
                    }
                    written += note_refs.len() as u64;
                    Ok::<u64, DbErr>(written)
                })
        })
        .await
        .map_err(|error| self.fatal(&format!("failed to publish refs atomically: {error}")))?;
        self.pending_refs.clear();
        self.pending_notes.clear();
        self.pending_note_replacements.clear();
        self.refs_written = self.refs_written.saturating_add(written);
        Ok(())
    }

    // ------------------------------------------------------------------
    // primitives
    // ------------------------------------------------------------------

    fn save<T: git_internal::internal::object::ObjectTrait>(
        &mut self,
        object: &T,
        id: ObjectHash,
    ) -> CliResult<()> {
        self.objects += 1;
        if self.objects > self.max_objects {
            return Err(self.fatal(&format!(
                "object limit exceeded ({}); raise with --max-count",
                self.max_objects
            )));
        }
        save_object(object, &id)
            .map_err(|error| self.fatal(&format!("failed to write object: {error}")))
    }

    fn set_mark(&mut self, mark: u32, id: ObjectHash) -> CliResult<()> {
        if self.marks.insert(mark, id).is_some() {
            return Err(self.fatal(&format!("duplicate mark :{mark}")));
        }
        Ok(())
    }

    /// Resolve a `from`/`merge` target: a `:mark` or literal object id.
    fn resolve_commitish(&mut self, spec: &str) -> CliResult<ObjectHash> {
        self.resolve_dataref(spec)
    }

    /// Resolve a `:mark` or literal object id.
    fn resolve_dataref(&mut self, spec: &str) -> CliResult<ObjectHash> {
        let spec = spec.trim();
        if let Some(mark) = spec.strip_prefix(':') {
            let mark: u32 = mark
                .parse()
                .map_err(|_| self.fatal(&format!("invalid mark reference '{spec}'")))?;
            return self
                .marks
                .get(&mark)
                .copied()
                .ok_or_else(|| self.fatal(&format!("undefined mark :{mark}")));
        }
        // Literal object id — must match the repository hash length.
        if spec.len() != self.hash_hex_len || !spec.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.fatal(&format!(
                "object id '{spec}' does not match the repository hash format"
            )));
        }
        ObjectHash::from_str(spec).map_err(|_| self.fatal(&format!("invalid object id '{spec}'")))
    }

    /// Load a commit's tree into the flat path → (mode, id) state map.
    fn load_tree_state(
        &mut self,
        commit_id: &ObjectHash,
        state: &mut HashMap<String, (TreeItemMode, ObjectHash)>,
    ) -> CliResult<()> {
        state.clear();
        let commit: Commit = load_object(commit_id)
            .map_err(|error| self.fatal(&format!("cannot read commit {commit_id}: {error}")))?;
        flatten_tree(&commit.tree_id, "", state).map_err(|error| self.fatal(&error))
    }

    /// Validate a ref target: must be `refs/…`, well-formed, and never escape.
    fn validate_ref(&self, refspec: &str) -> CliResult<String> {
        let refname = refspec.trim();
        if refname.is_empty() {
            return Err(self.fatal("missing ref name"));
        }
        if !refname.starts_with("refs/") {
            return Err(self.fatal(&format!(
                "ref '{refname}' is outside the repository (must be refs/…)"
            )));
        }
        if !util::is_valid_refname(refname) {
            return Err(self.fatal(&format!("invalid ref name '{refname}'")));
        }
        Ok(refname.to_string())
    }

    // ------------------------------------------------------------------
    // line / data reading (resource-bounded)
    // ------------------------------------------------------------------

    fn next_line(&mut self) -> CliResult<Option<Vec<u8>>> {
        if let Some(line) = self.pending_line.take() {
            return Ok(Some(line));
        }
        let mut buf = Vec::new();
        let mut limited = (&mut self.reader).take(MAX_COMMAND_LINE_BYTES + 1);
        let n = limited
            .read_until(b'\n', &mut buf)
            .map_err(|error| self.fatal(&format!("read error: {error}")))?;
        if n == 0 {
            return Ok(None);
        }
        self.account(n as u64)?;
        if n as u64 > MAX_COMMAND_LINE_BYTES {
            return Err(self.fatal(&format!(
                "command line exceeds the {MAX_COMMAND_LINE_BYTES}-byte safety limit"
            )));
        }
        Ok(Some(buf))
    }

    /// Read a command line that must begin with `data`, returning the payload.
    fn read_data_line(&mut self) -> CliResult<Vec<u8>> {
        match self.read_required_data()? {
            Some(data) => Ok(data),
            None => Err(self.fatal("expected a `data` directive")),
        }
    }

    /// Read the next line, requiring it to be a `data` directive.
    fn read_required_data(&mut self) -> CliResult<Option<Vec<u8>>> {
        let Some(raw) = self.next_line()? else {
            return Ok(None);
        };
        let line = trim_newline(&raw);
        let text = std::str::from_utf8(line)
            .map_err(|_| self.fatal("non-UTF-8 data header"))?
            .to_string();
        let (kw, rest) = split_first(&text);
        if kw != "data" {
            return Err(self.fatal(&format!("expected `data`, found `{kw}`")));
        }
        Ok(Some(self.read_data_payload(rest)?))
    }

    fn read_data_payload(&mut self, spec: &str) -> CliResult<Vec<u8>> {
        let spec = spec.trim();
        if let Some(delim) = spec.strip_prefix("<<") {
            // Delimited (here-doc) form: lines until one equals the delimiter.
            let delim = delim.to_string();
            let mut payload = Vec::new();
            loop {
                let Some(raw) = self.next_line()? else {
                    return Err(self.fatal("unterminated delimited `data`"));
                };
                let line = trim_newline(&raw);
                if line == delim.as_bytes() {
                    break;
                }
                payload.extend_from_slice(line);
                payload.push(b'\n');
            }
            Ok(payload)
        } else {
            let n: usize = spec
                .parse()
                .map_err(|_| self.fatal(&format!("invalid data length '{spec}'")))?;
            self.account(n as u64)?;
            let mut payload = vec![0u8; n];
            self.reader
                .read_exact(&mut payload)
                .map_err(|error| self.fatal(&format!("truncated data payload: {error}")))?;
            // Consume the optional trailing LF so it does not leak as a blank line.
            let next = match self.reader.fill_buf() {
                Ok(buf) => buf.first().copied(),
                Err(error) => return Err(self.fatal(&format!("read error: {error}"))),
            };
            if next == Some(b'\n') {
                self.reader.consume(1);
                self.account(1)?; // count the optional LF against the input cap
            }
            Ok(payload)
        }
    }

    fn read_optional_mark(&mut self) -> CliResult<Option<u32>> {
        match self.read_optional_prefixed("mark")? {
            Some(rest) => {
                let mark = rest.trim();
                let mark = mark.strip_prefix(':').unwrap_or(mark);
                let mark: u32 = mark
                    .parse()
                    .map_err(|_| self.fatal(&format!("invalid mark ':{mark}'")))?;
                Ok(Some(mark))
            }
            None => Ok(None),
        }
    }

    /// If the next line starts with `<keyword> `, consume it and return the
    /// remainder; otherwise push it back and return `None`.
    fn read_optional_prefixed(&mut self, keyword: &str) -> CliResult<Option<String>> {
        let Some(raw) = self.next_line()? else {
            return Ok(None);
        };
        let line = trim_newline(&raw);
        let text = std::str::from_utf8(line)
            .map_err(|_| self.fatal("non-UTF-8 header line"))?
            .to_string();
        let (kw, rest) = split_first(&text);
        if kw == keyword {
            Ok(Some(rest.to_string()))
        } else {
            self.pending_line = Some(raw);
            Ok(None)
        }
    }

    fn account(&mut self, n: u64) -> CliResult<()> {
        self.bytes_read = self.bytes_read.saturating_add(n);
        if self.bytes_read > self.max_input {
            return Err(self.fatal(&format!(
                "input exceeds the {}-byte limit (set fastimport.maxInputSize)",
                self.max_input
            )));
        }
        Ok(())
    }

    fn fatal(&self, message: &str) -> CliError {
        CliError::fatal(format!("fast-import: {message}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    }
}

// ----------------------------------------------------------------------------
// free helpers
// ----------------------------------------------------------------------------

fn trim_newline(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    if end > 0 && line[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && line[end - 1] == b'\r' {
        end -= 1;
    }
    &line[..end]
}

fn split_first(text: &str) -> (&str, &str) {
    match text.split_once(' ') {
        Some((head, rest)) => (head, rest),
        None => (text, ""),
    }
}

fn message_string(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| "commit and tag messages must be valid UTF-8 in this repository".to_string())
}

fn is_zero_object_id(spec: &str, hash_hex_len: usize) -> bool {
    spec.len() == hash_hex_len && spec.bytes().all(|byte| byte == b'0')
}

/// Remove a path and, when it names a directory, every descendant.
fn remove_path_and_subtree(path: &str, state: &mut HashMap<String, (TreeItemMode, ObjectHash)>) {
    let prefix = format!("{path}/");
    state.retain(|key, _| key != path && !key.starts_with(&prefix));
}

/// Make `path` writable as a file by removing the old path/subtree and any
/// ancestor that is currently a file. Sibling paths remain intact.
fn prepare_destination(path: &str, state: &mut HashMap<String, (TreeItemMode, ObjectHash)>) {
    remove_path_and_subtree(path, state);
    for (index, _) in path.match_indices('/') {
        state.remove(&path[..index]);
    }
}

fn parse_single_path(input: &str) -> Result<String, String> {
    let (path, tail) = parse_path_token(input)?;
    if !tail.trim().is_empty() {
        return Err("path has unexpected trailing data".to_string());
    }
    Ok(path)
}

/// Parse one fast-import path token, including Git's C-style quoted form.
fn parse_path_token(input: &str) -> Result<(String, &str), String> {
    let input = input.trim_start();
    if input.is_empty() {
        return Err("missing path".to_string());
    }
    if !input.starts_with('"') {
        let end = input.find(char::is_whitespace).unwrap_or(input.len());
        let path = input[..end].to_string();
        validate_import_path(&path)?;
        return Ok((path, &input[end..]));
    }

    let bytes = input.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 1usize;
    let mut closed = None;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                closed = Some(index + 1);
                break;
            }
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    return Err("quoted path ends with a backslash".to_string());
                }
                match bytes[index] {
                    b'a' => decoded.push(0x07),
                    b'b' => decoded.push(0x08),
                    b'f' => decoded.push(0x0c),
                    b'n' => decoded.push(b'\n'),
                    b'r' => decoded.push(b'\r'),
                    b't' => decoded.push(b'\t'),
                    b'v' => decoded.push(0x0b),
                    b'\\' => decoded.push(b'\\'),
                    b'"' => decoded.push(b'"'),
                    digit @ b'0'..=b'7' => {
                        let mut value = digit - b'0';
                        let mut digits = 1usize;
                        while digits < 3
                            && index + 1 < bytes.len()
                            && matches!(bytes[index + 1], b'0'..=b'7')
                        {
                            index += 1;
                            value = value
                                .checked_mul(8)
                                .and_then(|current| current.checked_add(bytes[index] - b'0'))
                                .ok_or_else(|| "quoted path octal escape overflows".to_string())?;
                            digits += 1;
                        }
                        decoded.push(value);
                    }
                    other => {
                        return Err(format!(
                            "unsupported quoted-path escape '\\{}'",
                            char::from(other)
                        ));
                    }
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    let end = closed.ok_or_else(|| "unterminated quoted path".to_string())?;
    let path = String::from_utf8(decoded)
        .map_err(|_| "quoted path is not valid UTF-8 in this repository".to_string())?;
    validate_import_path(&path)?;
    Ok((path, &input[end..]))
}

fn validate_import_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path must not be empty".to_string());
    }
    if path.starts_with('/') || path.contains('\0') {
        return Err(format!("unsafe repository path '{path}'"));
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("unsafe repository path '{path}'"));
    }
    Ok(())
}

/// Recursively flatten a tree into `path → (mode, id)` leaves (gitlinks kept as
/// `TreeItemMode::Commit`).
fn flatten_tree(
    tree_id: &ObjectHash,
    prefix: &str,
    out: &mut HashMap<String, (TreeItemMode, ObjectHash)>,
) -> Result<(), String> {
    let tree: git_internal::internal::object::tree::Tree =
        load_object(tree_id).map_err(|error| format!("cannot read tree {tree_id}: {error}"))?;
    for TreeItem { mode, id, name } in tree.tree_items {
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if mode == TreeItemMode::Tree {
            flatten_tree(&id, &path, out)?;
        } else {
            out.insert(path, (mode, id));
        }
    }
    Ok(())
}

/// Parse a fast-import identity line (`<name> <email> <when>`) into a signature
/// via the object layer, which already understands the `name <email> ts tz` form.
fn parse_signature(kind: &str, line: &str) -> Result<Signature, String> {
    let signature_type = match kind {
        "author" => SignatureType::Author,
        "committer" => SignatureType::Committer,
        "tagger" => SignatureType::Tagger,
        _ => return Err(format!("invalid signature kind '{kind}'")),
    };
    let email_separator = line
        .rfind(" <")
        .ok_or_else(|| format!("invalid {kind} '{line}': missing ` <email>`"))?;
    let email_start = email_separator + 2;
    let email_end = line[email_start..]
        .find('>')
        .map(|offset| email_start + offset)
        .ok_or_else(|| format!("invalid {kind} '{line}': unterminated email"))?;
    let name = &line[..email_separator];
    let email = &line[email_start..email_end];
    if name
        .chars()
        .any(|character| matches!(character, '\0' | '\n' | '\r' | '<' | '>'))
        || email.is_empty()
        || email
            .chars()
            .any(|character| matches!(character, '\0' | '\n' | '\r' | '<' | '>' | ' '))
    {
        return Err(format!("invalid {kind} '{line}': unsafe name or email"));
    }
    let mut suffix = line[email_end + 1..].split_whitespace();
    let timestamp_text = suffix
        .next()
        .ok_or_else(|| format!("invalid {kind} '{line}': missing timestamp"))?;
    let timezone = suffix
        .next()
        .ok_or_else(|| format!("invalid {kind} '{line}': missing timezone"))?;
    if suffix.next().is_some() {
        return Err(format!("invalid {kind} '{line}': trailing identity data"));
    }
    let timestamp = timestamp_text
        .parse::<usize>()
        .map_err(|error| format!("invalid {kind} timestamp '{timestamp_text}': {error}"))?;
    if timezone.len() != 5
        || !matches!(timezone.as_bytes()[0], b'+' | b'-')
        || !timezone.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(format!("invalid {kind} timezone '{timezone}'"));
    }
    Ok(Signature {
        signature_type,
        name: name.to_string(),
        email: email.to_string(),
        timestamp,
        timezone: timezone.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn trims_crlf_and_lf() {
        assert_eq!(trim_newline(b"abc\r\n"), b"abc");
        assert_eq!(trim_newline(b"abc\n"), b"abc");
        assert_eq!(trim_newline(b"abc"), b"abc");
    }

    #[test]
    fn split_first_separates_command_and_rest() {
        assert_eq!(
            split_first("commit refs/heads/main"),
            ("commit", "refs/heads/main")
        );
        assert_eq!(split_first("done"), ("done", ""));
    }

    #[test]
    fn quoted_paths_decode_git_escapes() {
        assert_eq!(parse_single_path("\"a b\"").unwrap(), "a b");
        assert_eq!(parse_single_path("\"tab\\tname\"").unwrap(), "tab\tname");
        assert_eq!(parse_single_path("\"utf\\303\\251\"").unwrap(), "utfé");
        assert_eq!(parse_single_path("plain").unwrap(), "plain");
        assert!(parse_single_path("\"../escape\"").is_err());
    }

    #[test]
    fn delete_removes_path_and_subtree() {
        let mut state: HashMap<String, (TreeItemMode, ObjectHash)> = HashMap::new();
        let id = ObjectHash::default();
        state.insert("dir/a".into(), (TreeItemMode::Blob, id));
        state.insert("dir/sub/b".into(), (TreeItemMode::Blob, id));
        state.insert("keep".into(), (TreeItemMode::Blob, id));
        remove_path_and_subtree("dir", &mut state);
        assert_eq!(state.len(), 1);
        assert!(state.contains_key("keep"));
    }

    #[test]
    fn preparing_a_nested_destination_removes_file_ancestors_only() {
        let id = ObjectHash::default();
        let mut state = HashMap::from([
            ("dir".into(), (TreeItemMode::Blob, id)),
            ("sibling".into(), (TreeItemMode::Blob, id)),
        ]);
        prepare_destination("dir/child", &mut state);
        assert!(!state.contains_key("dir"));
        assert!(state.contains_key("sibling"));
    }

    #[test]
    fn command_lines_are_bounded_before_unbounded_allocation() {
        let input = vec![b'x'; (MAX_COMMAND_LINE_BYTES + 1) as usize];
        let mut importer = Importer::new(
            Cursor::new(input),
            MAX_COMMAND_LINE_BYTES + 10,
            DEFAULT_MAX_OBJECTS,
        );
        let error = importer.next_line().expect_err("oversized line must fail");
        assert!(error.to_string().contains("command line exceeds"));
    }

    #[test]
    fn signature_parser_rejects_malformed_external_input_without_panicking() {
        assert!(parse_signature("author", "missing delimiters").is_err());
        assert!(parse_signature("committer", "Name <bad email> 1 +0000").is_err());
        assert!(parse_signature("tagger", "Name <a@b> nope +0000").is_err());
        assert!(parse_signature("author", "Name <a@b> 1 UTC").is_err());
        let parsed = parse_signature("author", "A U Thor <a@example.com> 1 -0230")
            .expect("valid external identity");
        assert_eq!(parsed.signature_type, SignatureType::Author);
        assert_eq!(parsed.name, "A U Thor");
        assert_eq!(parsed.timezone, "-0230");
    }

    #[test]
    fn non_utf8_commit_metadata_fails_instead_of_being_lossy() {
        assert!(message_string(b"valid UTF-8").is_ok());
        assert!(message_string(&[0xff, 0xfe]).is_err());
    }
}
