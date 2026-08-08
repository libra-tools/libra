//! `libra fast-export` — emit the commits reachable from a revision as a
//! `git fast-import` stream, a focused subset of `git fast-export`.
//!
//! Read-only: it never writes objects or refs. Each commit is emitted with a
//! full `deleteall` + `M` file list reconstructed from its tree (rather than a
//! parent diff) — larger than Git's diff-based output but byte-for-byte correct.
//! Multiple refs share one mark table, ranges leave excluded parents as literal
//! prerequisites, annotated tags use `tag` records, and Libra notes mappings use
//! valid fast-import `N` records.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{self, Write},
    str::FromStr,
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        blob::Blob,
        commit::Commit,
        tag::Tag,
        tree::{Tree, TreeItemMode},
        types::ObjectType,
    },
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Statement};

use crate::{
    command::load_object,
    common_utils::parse_commit_msg,
    internal::{branch::Branch, db::get_db_conn_instance, head::Head, model::reference},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
        util,
    },
};

pub const FAST_EXPORT_EXAMPLES: &str = "\
EXAMPLES:
    libra fast-export                 Export the current branch as a fast-import stream
    libra fast-export main topic      Export multiple refs with one shared mark table
    libra fast-export base..main      Export an incremental range
    libra fast-export --all           Export all local branches, tags, and notes
    libra fast-export HEAD > repo.fi  Save the stream to a file";

/// Emit history reachable from one or more revisions as a fast-import stream.
#[derive(Parser, Debug)]
#[command(after_help = FAST_EXPORT_EXAMPLES)]
pub struct FastExportArgs {
    /// Revisions to export (default `HEAD`). `A..B` exports commits reachable
    /// from B but not A; `^A` excludes A's reachable history.
    #[clap(value_name = "REV")]
    pub revs: Vec<String>,

    /// Export every local branch and tag, plus every Libra notes mapping.
    #[clap(long)]
    pub all: bool,
}

#[derive(Clone)]
struct ExportRef {
    name: String,
    tip: ObjectHash,
}

#[derive(Clone)]
struct ExportTag {
    ref_name: String,
    raw_target: ObjectHash,
    peeled_commit: ObjectHash,
    annotated: Option<Tag>,
}

struct ExportNote {
    notes_ref: String,
    object: ObjectHash,
    blob: ObjectHash,
}

struct ExportPlan {
    refs: Vec<ExportRef>,
    tags: HashMap<String, ExportTag>,
    excludes: HashSet<ObjectHash>,
    notes: Vec<ExportNote>,
}

pub async fn execute(args: FastExportArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: FastExportArgs, _output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let plan = build_export_plan(&args).await?;
    let by_id = collect_commits(&plan.refs, &plan.excludes)?;
    let tips = plan.refs.iter().map(|entry| entry.tip).collect::<Vec<_>>();
    let order = topological_order(&tips, &by_id);
    preflight_export_objects(&plan, &by_id)?;
    let primary_ref = plan
        .refs
        .first()
        .map(|entry| entry.name.as_str())
        .unwrap_or("refs/heads/exported");

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    // Marks are shared by blobs, commits, and annotated tags; `:1`, `:2`, …
    // in emission order.
    let mut marks: HashMap<ObjectHash, usize> = HashMap::new();
    let mut next_mark = 0usize;

    for oid in &order {
        let commit = &by_id[oid];
        let leaves = flatten_tree(&commit.tree_id)?;

        // Emit each not-yet-seen blob.
        for (_, mode, id) in &leaves {
            if *mode == TreeItemMode::Commit || marks.contains_key(id) {
                continue; // gitlink (no blob) or already emitted
            }
            let blob: Blob = load_object(id).map_err(|error| object_error(id, error))?;
            next_mark += 1;
            marks.insert(*id, next_mark);
            writeln!(out, "blob\nmark :{next_mark}\ndata {}", blob.data.len())
                .map_err(write_err)?;
            out.write_all(&blob.data).map_err(write_err)?;
            out.write_all(b"\n").map_err(write_err)?;
        }

        next_mark += 1;
        let commit_mark = next_mark;
        marks.insert(*oid, commit_mark);
        writeln!(out, "commit {primary_ref}\nmark :{commit_mark}").map_err(write_err)?;
        writeln!(out, "author {}", format_ident(&commit.author)).map_err(write_err)?;
        writeln!(out, "committer {}", format_ident(&commit.committer)).map_err(write_err)?;
        let message = export_commit_message(&commit.message);
        writeln!(out, "data {}", message.len()).map_err(write_err)?;
        out.write_all(message.as_bytes()).map_err(write_err)?;
        // The optional LF after the `data` payload terminates the message so the
        // following directive is recognised even when the message has no
        // trailing newline.
        out.write_all(b"\n").map_err(write_err)?;

        // Link parents. Every parent is in the reachable set and was emitted
        // earlier (topological order); a missing mark means a broken traversal,
        // which is a hard error rather than a silently root-less commit.
        for (index, parent) in commit.parent_commit_ids.iter().enumerate() {
            let keyword = if index == 0 { "from" } else { "merge" };
            if let Some(mark) = marks.get(parent) {
                writeln!(out, "{keyword} :{mark}").map_err(write_err)?;
            } else {
                // A range export intentionally omits excluded ancestors. Their
                // literal IDs become prerequisites for an incremental import.
                writeln!(out, "{keyword} {parent}").map_err(write_err)?;
            }
        }

        // Reconstruct the whole tree (no parent diff needed).
        out.write_all(b"deleteall\n").map_err(write_err)?;
        for (path, mode, id) in &leaves {
            let mode_str = std::str::from_utf8(mode.to_bytes()).map_err(|error| {
                CliError::fatal(format!(
                    "fast-export: invalid tree mode for '{path}': {error}"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            let path = quote_path(path);
            if *mode == TreeItemMode::Commit {
                // Submodule gitlink: reference the commit sha directly.
                writeln!(out, "M {mode_str} {id} {path}").map_err(write_err)?;
            } else {
                let mark = marks.get(id).copied().ok_or_else(|| {
                    CliError::fatal(format!(
                        "fast-export: blob {id} for path {path} has no emitted mark"
                    ))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::InternalInvariant)
                })?;
                writeln!(out, "M {mode_str} :{mark} {path}").map_err(write_err)?;
            }
        }
        out.write_all(b"\n").map_err(write_err)?;
    }

    emit_ref_resets(&mut out, &plan, &marks)?;
    emit_annotated_tags(&mut out, &plan, &mut marks, &mut next_mark)?;
    emit_notes(&mut out, &plan.notes, &mut marks, &mut next_mark)?;
    out.write_all(b"done\n").map_err(write_err)?;

    out.flush().map_err(write_err)?;
    Ok(())
}

async fn build_export_plan(args: &FastExportArgs) -> CliResult<ExportPlan> {
    let mut branches = Branch::list_branches_result(None).await.map_err(|error| {
        CliError::fatal(format!("failed to list branches for fast-export: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    branches.sort_by(|left, right| left.name.cmp(&right.name));
    let export_tags = load_export_tags().await?;
    let mut tag_by_spec = HashMap::new();
    for tag in &export_tags {
        tag_by_spec.insert(tag.ref_name.clone(), tag.clone());
        if let Some(short) = tag.ref_name.strip_prefix("refs/tags/") {
            tag_by_spec.insert(short.to_string(), tag.clone());
        }
    }

    let mut refs = Vec::new();
    let mut selected_tags = HashMap::new();
    let mut seen_refs = HashSet::new();
    let mut add_ref = |entry: ExportRef, tag: Option<ExportTag>| {
        if seen_refs.insert(entry.name.clone()) {
            if let Some(tag) = tag {
                selected_tags.insert(entry.name.clone(), tag);
            }
            refs.push(entry);
        }
    };

    if args.all {
        for branch in &branches {
            add_ref(
                ExportRef {
                    name: full_branch_name(&branch.name),
                    tip: branch.commit,
                },
                None,
            );
        }
        let mut tags = export_tags.clone();
        tags.sort_by(|left, right| left.ref_name.cmp(&right.ref_name));
        for tag in tags {
            add_ref(
                ExportRef {
                    name: tag.ref_name.clone(),
                    tip: tag.peeled_commit,
                },
                Some(tag),
            );
        }
    }

    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    if args.revs.is_empty() && !args.all {
        positives.push("HEAD".to_string());
    }
    for spec in &args.revs {
        if let Some(excluded) = spec.strip_prefix('^') {
            if excluded.is_empty() {
                return Err(invalid_revision(spec, "missing revision after '^'"));
            }
            negatives.push(excluded.to_string());
        } else if let Some((excluded, included)) = split_two_dot_range(spec) {
            if excluded.is_empty() || included.is_empty() {
                return Err(invalid_revision(spec, "both sides of A..B are required"));
            }
            negatives.push(excluded.to_string());
            positives.push(included.to_string());
        } else {
            positives.push(spec.clone());
        }
    }

    for (index, spec) in positives.iter().enumerate() {
        if let Some(tag) = tag_by_spec.get(spec).cloned() {
            add_ref(
                ExportRef {
                    name: tag.ref_name.clone(),
                    tip: tag.peeled_commit,
                },
                Some(tag),
            );
            continue;
        }
        if let Some(branch) = branches
            .iter()
            .find(|branch| branch.name == *spec || full_branch_name(&branch.name) == *spec)
        {
            add_ref(
                ExportRef {
                    name: full_branch_name(&branch.name),
                    tip: branch.commit,
                },
                None,
            );
            continue;
        }
        let tip = resolve_commit(spec).await?;
        let name = if spec == "HEAD" {
            match Head::current().await {
                Head::Branch(name) => full_branch_name(&name),
                Head::Detached(_) => "refs/heads/exported".to_string(),
            }
        } else if spec.starts_with("refs/") {
            spec.clone()
        } else {
            format!("refs/heads/exported-{}", index + 1)
        };
        add_ref(ExportRef { name, tip }, None);
    }

    if refs.is_empty() {
        return Err(invalid_revision(
            "<selection>",
            "no positive revision or --all ref was selected",
        ));
    }

    let mut exclude_tips = Vec::new();
    for spec in negatives {
        exclude_tips.push(resolve_commit(&spec).await?);
    }
    let excludes = collect_commit_ids(&exclude_tips, &HashSet::new())?;
    let notes = if args.all {
        load_export_notes().await?
    } else {
        Vec::new()
    };
    Ok(ExportPlan {
        refs,
        tags: selected_tags,
        excludes,
        notes,
    })
}

fn split_two_dot_range(spec: &str) -> Option<(&str, &str)> {
    if spec.contains("...") {
        return None;
    }
    spec.split_once("..")
}

async fn resolve_commit(spec: &str) -> CliResult<ObjectHash> {
    util::get_commit_base(spec)
        .await
        .map_err(|error| invalid_revision(spec, &error))
}

fn invalid_revision(spec: &str, detail: &str) -> CliError {
    CliError::fatal(format!("not a valid revision '{spec}': {detail}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

fn full_branch_name(name: &str) -> String {
    if name.starts_with("refs/heads/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    }
}

async fn load_export_tags() -> CliResult<Vec<ExportTag>> {
    let db = get_db_conn_instance().await;
    let rows = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
        .all(&db)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to list tag refs for fast-export: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    let storage = util::objects_storage();
    let mut tags = Vec::new();
    for row in rows {
        let ref_name = row.name.ok_or_else(|| {
            CliError::fatal("fast-export: tag reference is missing its name")
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let raw = row.commit.ok_or_else(|| {
            CliError::fatal(format!(
                "fast-export: tag '{ref_name}' is missing its target"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let raw_target = ObjectHash::from_str(&raw).map_err(|error| {
            CliError::fatal(format!(
                "fast-export: tag '{ref_name}' has invalid target '{raw}': {error}"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let annotated = match storage.get_object_type(&raw_target) {
            Ok(ObjectType::Tag) => {
                let tag: Tag =
                    load_object(&raw_target).map_err(|error| object_error(&raw_target, error))?;
                if tag.object_type != ObjectType::Commit {
                    return Err(CliError::fatal(format!(
                        "fast-export: annotated tag '{ref_name}' targets {}, not a commit",
                        tag.object_type
                    ))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::Unsupported));
                }
                Some(tag)
            }
            Ok(ObjectType::Commit) => None,
            Ok(other) => {
                return Err(CliError::fatal(format!(
                    "fast-export: lightweight tag '{ref_name}' targets {other}, not a commit"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::Unsupported));
            }
            Err(error) => {
                return Err(CliError::fatal(format!(
                    "fast-export: cannot inspect tag target {raw_target}: {error}"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt));
            }
        };
        let peeled_commit = annotated
            .as_ref()
            .map(|tag| tag.object_hash)
            .unwrap_or(raw_target);
        tags.push(ExportTag {
            ref_name,
            raw_target,
            peeled_commit,
            annotated,
        });
    }
    Ok(tags)
}

async fn load_export_notes() -> CliResult<Vec<ExportNote>> {
    let db = get_db_conn_instance().await;
    let rows = db
        .query_all_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT notes_ref, object, blob FROM notes ORDER BY notes_ref, object".to_string(),
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to list notes for fast-export: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    let mut notes = Vec::new();
    for row in rows {
        let notes_ref = row.try_get::<String>("", "notes_ref").map_err(|error| {
            CliError::fatal(format!("fast-export: invalid notes ref row: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        if !notes_ref.starts_with("refs/notes/") || !util::is_valid_refname(&notes_ref) {
            return Err(CliError::fatal(format!(
                "fast-export: invalid stored notes ref '{notes_ref}'"
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        let object = row.try_get::<String>("", "object").map_err(|error| {
            CliError::fatal(format!("fast-export: invalid notes object row: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        let blob = row.try_get::<String>("", "blob").map_err(|error| {
            CliError::fatal(format!("fast-export: invalid notes blob row: {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;
        notes.push(ExportNote {
            notes_ref,
            object: ObjectHash::from_str(&object).map_err(|error| {
                CliError::fatal(format!(
                    "fast-export: invalid noted object '{object}': {error}"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?,
            blob: ObjectHash::from_str(&blob).map_err(|error| {
                CliError::fatal(format!("fast-export: invalid note blob '{blob}': {error}"))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?,
        });
    }
    Ok(notes)
}

fn collect_commits(
    refs: &[ExportRef],
    excludes: &HashSet<ObjectHash>,
) -> CliResult<HashMap<ObjectHash, Commit>> {
    let ids = collect_commit_ids(
        &refs.iter().map(|entry| entry.tip).collect::<Vec<_>>(),
        excludes,
    )?;
    ids.into_iter()
        .map(|id| {
            load_object::<Commit>(&id)
                .map(|commit| (id, commit))
                .map_err(|error| object_error(&id, error))
        })
        .collect()
}

fn collect_commit_ids(
    tips: &[ObjectHash],
    excludes: &HashSet<ObjectHash>,
) -> CliResult<HashSet<ObjectHash>> {
    let mut seen = HashSet::new();
    let mut stack = tips.to_vec();
    while let Some(id) = stack.pop() {
        if excludes.contains(&id) || !seen.insert(id) {
            continue;
        }
        let commit: Commit = load_object(&id).map_err(|error| object_error(&id, error))?;
        stack.extend(commit.parent_commit_ids);
    }
    Ok(seen)
}

/// Post-order (parents-before-children) traversal of the reachable set so every
/// `from`/`merge` mark is defined before it is referenced.
fn topological_order(tips: &[ObjectHash], by_id: &HashMap<ObjectHash, Commit>) -> Vec<ObjectHash> {
    let mut order = Vec::with_capacity(by_id.len());
    let mut visited: HashSet<ObjectHash> = HashSet::new();
    // (id, children-expanded?)
    let mut stack: Vec<(ObjectHash, bool)> =
        tips.iter().rev().copied().map(|tip| (tip, false)).collect();
    while let Some((id, expanded)) = stack.pop() {
        if !by_id.contains_key(&id) {
            continue;
        }
        if expanded {
            order.push(id);
            continue;
        }
        if !visited.insert(id) {
            continue;
        }
        stack.push((id, true));
        if let Some(commit) = by_id.get(&id) {
            for parent in &commit.parent_commit_ids {
                if by_id.contains_key(parent) && !visited.contains(parent) {
                    stack.push((*parent, false));
                }
            }
        }
    }
    order
}

fn emitted_ref(oid: &ObjectHash, marks: &HashMap<ObjectHash, usize>) -> String {
    marks
        .get(oid)
        .map(|mark| format!(":{mark}"))
        .unwrap_or_else(|| oid.to_string())
}

fn emit_ref_resets(
    out: &mut impl Write,
    plan: &ExportPlan,
    marks: &HashMap<ObjectHash, usize>,
) -> CliResult<()> {
    for entry in &plan.refs {
        if plan
            .tags
            .get(&entry.name)
            .is_some_and(|tag| tag.annotated.is_some())
        {
            continue;
        }
        writeln!(
            out,
            "reset {}\nfrom {}\n",
            entry.name,
            emitted_ref(&entry.tip, marks)
        )
        .map_err(write_err)?;
    }
    Ok(())
}

fn emit_annotated_tags(
    out: &mut impl Write,
    plan: &ExportPlan,
    marks: &mut HashMap<ObjectHash, usize>,
    next_mark: &mut usize,
) -> CliResult<()> {
    for entry in &plan.refs {
        let Some(export_tag) = plan.tags.get(&entry.name) else {
            continue;
        };
        let Some(tag) = export_tag.annotated.as_ref() else {
            continue;
        };
        let short = export_tag
            .ref_name
            .strip_prefix("refs/tags/")
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "fast-export: invalid tag ref '{}'",
                    export_tag.ref_name
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
        if export_tag.raw_target != tag.id {
            return Err(CliError::fatal(format!(
                "fast-export: tag ref '{}' does not match loaded tag object {}",
                export_tag.ref_name, tag.id
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
        *next_mark += 1;
        marks.insert(tag.id, *next_mark);
        writeln!(out, "tag {short}").map_err(write_err)?;
        writeln!(out, "mark :{}", *next_mark).map_err(write_err)?;
        writeln!(out, "from {}", emitted_ref(&tag.object_hash, marks)).map_err(write_err)?;
        writeln!(out, "tagger {}", format_ident(&tag.tagger)).map_err(write_err)?;
        writeln!(out, "data {}", tag.message.len()).map_err(write_err)?;
        out.write_all(tag.message.as_bytes()).map_err(write_err)?;
        // The LF after the data payload terminates the `tag` command. Unlike a
        // commit/reset body, a tag has no additional blank-line terminator;
        // emitting one is rejected as an empty top-level command by Git.
        out.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

fn emit_notes(
    out: &mut impl Write,
    notes: &[ExportNote],
    marks: &mut HashMap<ObjectHash, usize>,
    next_mark: &mut usize,
) -> CliResult<()> {
    let mut grouped: BTreeMap<&str, Vec<&ExportNote>> = BTreeMap::new();
    for note in notes {
        if !marks.contains_key(&note.object) {
            return Err(CliError::fatal(format!(
                "fast-export: note in '{}' targets object {} outside the exported object graph",
                note.notes_ref, note.object
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::Unsupported));
        }
        grouped.entry(&note.notes_ref).or_default().push(note);
    }
    for (notes_ref, entries) in grouped {
        for note in &entries {
            if marks.contains_key(&note.blob) {
                continue;
            }
            let blob: Blob =
                load_object(&note.blob).map_err(|error| object_error(&note.blob, error))?;
            *next_mark += 1;
            marks.insert(note.blob, *next_mark);
            writeln!(out, "blob\nmark :{}\ndata {}", *next_mark, blob.data.len())
                .map_err(write_err)?;
            out.write_all(&blob.data).map_err(write_err)?;
            out.write_all(b"\n").map_err(write_err)?;
        }

        *next_mark += 1;
        writeln!(out, "commit {notes_ref}\nmark :{}", *next_mark).map_err(write_err)?;
        out.write_all(b"committer Libra fast-export <libra@localhost> 0 +0000\n")
            .map_err(write_err)?;
        out.write_all(b"data 0\n").map_err(write_err)?;
        for note in entries {
            let blob_mark = marks.get(&note.blob).ok_or_else(|| {
                CliError::fatal(format!(
                    "fast-export: note blob {} has no emitted mark",
                    note.blob
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
            })?;
            let object_mark = marks.get(&note.object).ok_or_else(|| {
                CliError::fatal(format!(
                    "fast-export: noted object {} has no emitted mark",
                    note.object
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::InternalInvariant)
            })?;
            writeln!(out, "N :{blob_mark} :{object_mark}").map_err(write_err)?;
        }
        out.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

/// Validate every object that will be read and every note target before stdout
/// receives protocol bytes. A redirected export can still be interrupted by an
/// IO failure, but repository corruption or an unrepresentable note cannot
/// leave a deceptively plausible partial stream.
fn preflight_export_objects(
    plan: &ExportPlan,
    commits: &HashMap<ObjectHash, Commit>,
) -> CliResult<()> {
    for export_ref in &plan.refs {
        if !export_ref.name.starts_with("refs/") || !util::is_valid_refname(&export_ref.name) {
            return Err(CliError::fatal(format!(
                "fast-export: invalid selected ref '{}'",
                export_ref.name
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt));
        }
    }
    let mut markable = commits.keys().copied().collect::<HashSet<_>>();
    for commit in commits.values() {
        validate_export_signature(&commit.author, "author")?;
        validate_export_signature(&commit.committer, "committer")?;
        for (_, mode, object) in flatten_tree(&commit.tree_id)? {
            if mode == TreeItemMode::Commit {
                continue;
            }
            load_object::<Blob>(&object).map_err(|error| object_error(&object, error))?;
            markable.insert(object);
        }
    }
    for tag in plan.tags.values() {
        if let Some(annotated) = &tag.annotated {
            validate_export_signature(&annotated.tagger, "tagger")?;
            let short = tag.ref_name.strip_prefix("refs/tags/").ok_or_else(|| {
                CliError::fatal(format!("fast-export: invalid tag ref '{}'", tag.ref_name))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            if annotated.tag_name != short {
                return Err(CliError::fatal(format!(
                    "fast-export: tag ref '{}' aliases object whose embedded name is '{}'",
                    tag.ref_name, annotated.tag_name
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::Unsupported));
            }
            markable.insert(tag.raw_target);
        }
    }
    for note in &plan.notes {
        if !markable.contains(&note.object) {
            return Err(CliError::fatal(format!(
                "fast-export: note in '{}' targets object {} that cannot be represented by a stream mark",
                note.notes_ref, note.object
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::Unsupported));
        }
        load_object::<Blob>(&note.blob).map_err(|error| object_error(&note.blob, error))?;
    }
    Ok(())
}

fn validate_export_signature(
    signature: &git_internal::internal::object::signature::Signature,
    label: &str,
) -> CliResult<()> {
    let unsafe_name = signature
        .name
        .chars()
        .any(|character| matches!(character, '\0' | '\n' | '\r' | '<' | '>'));
    let unsafe_email = signature.email.is_empty()
        || signature
            .email
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '\0' | '<' | '>'));
    let timezone = signature.timezone.as_bytes();
    let unsafe_timezone = timezone.len() != 5
        || !matches!(timezone[0], b'+' | b'-')
        || !timezone[1..].iter().all(|byte| byte.is_ascii_digit());
    if unsafe_name || unsafe_email || unsafe_timezone {
        return Err(CliError::fatal(format!(
            "fast-export: stored {label} identity cannot be represented safely"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::RepoCorrupt));
    }
    Ok(())
}

/// Quote a path with Git's C-style fast-import spelling when whitespace,
/// controls, quotes, backslashes, or non-ASCII bytes are present.
fn quote_path(path: &str) -> String {
    if path
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
    {
        return path.to_string();
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for byte in path.bytes() {
        match byte {
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            0x20..=0x7e => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

/// Convert Libra's stored commit body to fast-import's message payload.
///
/// Unsigned Git commit objects store one blank separator between the committer
/// header and the user message. `git-internal::Commit` retains that separator
/// as the leading LF in `message`, while fast-import's `data` payload contains
/// only the user message. Signed commit headers cannot be represented by a
/// fast-import commit record, so match Git's export behavior and omit them.
fn export_commit_message(stored: &str) -> &str {
    if let Some(message) = stored.strip_prefix('\n') {
        message
    } else {
        parse_commit_msg(stored).0
    }
}

/// Flatten a tree into `(path, mode, object id)` leaves (recursing into
/// subtrees), in a deterministic order.
fn flatten_tree(tree_id: &ObjectHash) -> CliResult<Vec<(String, TreeItemMode, ObjectHash)>> {
    fn walk(
        tree_id: &ObjectHash,
        prefix: &str,
        out: &mut Vec<(String, TreeItemMode, ObjectHash)>,
    ) -> CliResult<()> {
        let tree: Tree = load_object(tree_id).map_err(|error| object_error(tree_id, error))?;
        for item in &tree.tree_items {
            let path = if prefix.is_empty() {
                item.name.clone()
            } else {
                format!("{prefix}/{}", item.name)
            };
            if item.mode == TreeItemMode::Tree {
                walk(&item.id, &path, out)?;
            } else {
                out.push((path, item.mode, item.id));
            }
        }
        Ok(())
    }
    let mut leaves = Vec::new();
    walk(tree_id, "", &mut leaves)?;
    Ok(leaves)
}

/// fast-import identity line body: `Name <email> <timestamp> <timezone>`.
fn format_ident(sig: &git_internal::internal::object::signature::Signature) -> String {
    format!(
        "{} <{}> {} {}",
        sig.name, sig.email, sig.timestamp, sig.timezone
    )
}

fn object_error(id: &ObjectHash, error: git_internal::errors::GitError) -> CliError {
    CliError::fatal(format!("failed to load object {id}: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::RepoCorrupt)
}

fn write_err(error: io::Error) -> CliError {
    CliError::fatal(format!("failed to write fast-export stream: {error}"))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_quoting_matches_git_c_style() {
        assert_eq!(quote_path("plain.txt"), "plain.txt");
        assert_eq!(quote_path("space name"), "\"space name\"");
        assert_eq!(quote_path("tab\tname"), "\"tab\\tname\"");
        assert_eq!(quote_path("snow 雪"), "\"snow \\351\\233\\252\"");
    }

    #[test]
    fn export_payload_omits_the_stored_commit_separator() {
        assert_eq!(
            export_commit_message("\nsubject\nbody\n"),
            "subject\nbody\n"
        );
        assert_eq!(export_commit_message("\n"), "");
    }

    #[test]
    fn two_dot_ranges_are_distinct_from_symmetric_ranges() {
        assert_eq!(split_two_dot_range("base..tip"), Some(("base", "tip")));
        assert_eq!(split_two_dot_range("base...tip"), None);
    }
}
