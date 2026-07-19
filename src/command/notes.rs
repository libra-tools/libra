//! CLI handler for `libra notes` — add, show, list, or remove notes attached to commits.

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::notes::{self, DEFAULT_NOTES_REF},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        text::short_display_hash,
    },
};

pub const NOTES_EXAMPLES: &str = "\
EXAMPLES:
    libra notes add -m \"Reviewed-by: Alice\"         Add a note to HEAD
    libra notes append -m \"Deployed-by: CI\"         Append a line to HEAD's note
    libra notes copy <from> <to>                      Copy a note to another object
    libra notes edit -m \"Replaces existing\"          Set (replace) HEAD's note
    libra notes edit                                  Edit HEAD's note in $EDITOR (pre-filled)
    libra notes show                                  Show the note on HEAD
    libra notes list                                  List all notes
    libra notes remove abc1234                        Remove a note
    libra notes add -f -m \"Updated\" HEAD            Force-overwrite a note
    libra notes merge -s theirs refs/notes/other      Merge another notes ref (theirs on conflict)
    libra notes prune -v                              Drop notes for objects that no longer exist
    libra notes get-ref                               Print the active notes ref";

#[derive(Parser, Debug)]
#[command(about = "Add, show, list, or remove notes attached to commits")]
#[command(after_help = NOTES_EXAMPLES)]
pub struct NotesArgs {
    #[command(subcommand)]
    pub subcommand: Option<NotesSubcommand>,

    /// Operate on a specific notes ref (default: refs/notes/commits)
    #[clap(long, default_value = DEFAULT_NOTES_REF)]
    pub ref_: String,
}

#[derive(Subcommand, Debug)]
pub enum NotesSubcommand {
    /// Add a note to an object (defaults to HEAD)
    Add {
        /// Object to annotate (defaults to HEAD)
        #[clap(required = false)]
        object: Option<String>,

        /// Note message text (repeatable; blank lines separate messages)
        #[clap(short, long)]
        message: Vec<String>,

        /// Read note message from file (- for stdin)
        #[clap(short = 'F', long)]
        file: Vec<String>,

        /// Overwrite an existing note
        #[clap(short, long)]
        force: bool,
    },
    /// Append to an object's note (creating it if absent)
    Append {
        /// Object to annotate (defaults to HEAD)
        #[clap(required = false)]
        object: Option<String>,

        /// Note message text (repeatable; blank lines separate messages)
        #[clap(short, long)]
        message: Vec<String>,

        /// Read note message from file (- for stdin)
        #[clap(short = 'F', long)]
        file: Vec<String>,
    },
    /// Set (replace) an object's note, creating it if absent
    Edit {
        /// Object to annotate (defaults to HEAD)
        #[clap(required = false)]
        object: Option<String>,

        /// Note message text (repeatable; blank lines separate messages)
        #[clap(short, long)]
        message: Vec<String>,

        /// Read note message from file (- for stdin)
        #[clap(short = 'F', long)]
        file: Vec<String>,
    },
    /// Copy the note of one object to another object
    Copy {
        /// Source object to copy the note from
        from_object: String,

        /// Target object to copy the note to
        to_object: String,

        /// Overwrite an existing note on the target object
        #[clap(short, long)]
        force: bool,
    },
    /// List note objects and the commits they annotate
    List {
        /// Object to list notes for (omit to list all)
        #[clap(required = false)]
        object: Option<String>,
    },
    /// Show the note text for an object
    Show {
        /// Object to show the note for (defaults to HEAD)
        #[clap(required = false)]
        object: Option<String>,
    },
    /// Remove notes for one or more objects
    Remove {
        /// Objects to remove notes from (defaults to HEAD)
        #[clap(required = false)]
        objects: Vec<String>,
    },
    /// Merge notes from another notes ref into the current notes ref
    Merge {
        /// The notes ref to merge FROM (e.g. `refs/notes/other`)
        other_ref: String,

        /// Conflict-resolution strategy: `manual` (default; aborts on a
        /// conflicting note), `ours`, `theirs`, `union`, or `cat_sort_uniq`
        #[clap(short = 's', long)]
        strategy: Option<String>,
    },
    /// Remove notes for objects that no longer exist in the object store
    Prune {
        /// Report what would be pruned without deleting anything
        #[clap(short = 'n', long = "dry-run")]
        dry_run: bool,

        /// Print each pruned object id
        #[clap(short = 'v', long)]
        verbose: bool,
    },
    /// Print the notes ref that operations act on (honors `--ref`)
    GetRef,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action")]
pub enum NotesOutput {
    #[serde(rename = "add")]
    Add {
        #[serde(rename = "ref")]
        notes_ref: String,
        object: String,
        note_hash: String,
    },
    #[serde(rename = "append")]
    Append {
        #[serde(rename = "ref")]
        notes_ref: String,
        object: String,
        note_hash: String,
    },
    #[serde(rename = "edit")]
    Edit {
        #[serde(rename = "ref")]
        notes_ref: String,
        object: String,
        note_hash: String,
    },
    #[serde(rename = "copy")]
    Copy {
        #[serde(rename = "ref")]
        notes_ref: String,
        from_object: String,
        to_object: String,
        note_hash: String,
    },
    #[serde(rename = "list")]
    List {
        #[serde(rename = "ref")]
        notes_ref: String,
        notes: Vec<NotesListEntry>,
        #[serde(skip)]
        object_scoped: bool,
    },
    #[serde(rename = "show")]
    Show {
        #[serde(rename = "ref")]
        notes_ref: String,
        object: String,
        note_hash: String,
        text: String,
    },
    #[serde(rename = "remove")]
    Remove {
        #[serde(rename = "ref")]
        notes_ref: String,
        removed: Vec<NotesRemovedEntry>,
    },
    #[serde(rename = "merge")]
    Merge {
        #[serde(rename = "ref")]
        notes_ref: String,
        other_ref: String,
        merged: usize,
        skipped: usize,
        resolved_conflicts: Vec<String>,
    },
    #[serde(rename = "prune")]
    Prune {
        #[serde(rename = "ref")]
        notes_ref: String,
        pruned: Vec<String>,
        dry_run: bool,
        #[serde(skip)]
        verbose: bool,
    },
    #[serde(rename = "get-ref")]
    GetRef {
        #[serde(rename = "ref")]
        notes_ref: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct NotesListEntry {
    pub note_hash: Option<String>,
    pub annotated_object: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotesRemovedEntry {
    pub object: String,
    pub note_hash: String,
}

pub async fn execute(args: NotesArgs) {
    let argv: Vec<String> = std::env::args().collect();
    if let Err(err) = execute_safe(args, &OutputConfig::default(), &argv).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`].
///
/// `argv` is the raw process argument vector used to reconstruct
/// the original `-m`/`-F` occurrence order. Callers that do not spawn
/// from `main()` should provide the same slice they passed to clap.
pub async fn execute_safe(
    args: NotesArgs,
    output: &OutputConfig,
    argv: &[String],
) -> CliResult<()> {
    let notes_ref = &notes::normalize_notes_ref(&args.ref_)
        .map_err(|e| CliError::from(NotesCliError::from(e)))?;

    let subcommand = args
        .subcommand
        .unwrap_or(NotesSubcommand::List { object: None });

    match subcommand {
        NotesSubcommand::Add {
            object,
            message: _,
            file: _,
            force,
        } => {
            let target = object.as_deref().unwrap_or("HEAD");
            // Without `-m`/`-F`, compose the note in an editor. Like Git, the
            // existing note (if any) pre-fills the buffer; if one exists and `-f`
            // was not given, abort early (before opening the editor) just as the
            // `-m` path would.
            let content = if note_content_parts_present(argv) {
                build_note_content(argv)?
            } else {
                let initial = match notes::show(notes_ref, Some(target)).await {
                    Ok((_, object, note)) => {
                        if !force {
                            return Err(NotesCliError::from(notes::NotesError::AlreadyExists {
                                notes_ref: notes_ref.to_string(),
                                object,
                            })
                            .into());
                        }
                        note
                    }
                    Err(notes::NotesError::NotFound { .. }) => String::new(),
                    Err(err) => return Err(NotesCliError::from(err).into()),
                };
                compose_note_via_editor(&initial, target).await?
            };
            let result = notes::add(notes_ref, target, &content, force)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Add {
                notes_ref: result.notes_ref,
                object: result.object,
                note_hash: result.note_hash,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Append {
            object,
            message: _,
            file: _,
        } => {
            let target = object.as_deref().unwrap_or("HEAD");
            // Without `-m`/`-F`, compose the appended text in an editor.
            let new_content = if note_content_parts_present(argv) {
                build_note_content(argv)?
            } else {
                compose_note_via_editor("", target).await?
            };
            // Concatenate after the existing note (separated by a blank line),
            // or create a fresh note when the object has none — matching Git.
            let content = match notes::show(notes_ref, Some(target)).await {
                Ok((_, _, existing)) if !existing.trim().is_empty() => {
                    format!("{}\n\n{}", existing.trim_end_matches('\n'), new_content)
                }
                Ok(_) | Err(notes::NotesError::NotFound { .. }) => new_content,
                Err(err) => return Err(NotesCliError::from(err).into()),
            };
            // `force` overwrites the ref with the concatenated note.
            let result = notes::add(notes_ref, target, &content, true)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Append {
                notes_ref: result.notes_ref,
                object: result.object,
                note_hash: result.note_hash,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Edit {
            object,
            message: _,
            file: _,
        } => {
            // `edit` sets (replaces) the note unconditionally, creating it if
            // absent — distinct from `add`, which fails when one exists. Without
            // `-m`/`-F`, open an editor pre-filled with the existing note.
            let target = object.as_deref().unwrap_or("HEAD");
            let content = if note_content_parts_present(argv) {
                build_note_content(argv)?
            } else {
                let existing = match notes::show(notes_ref, Some(target)).await {
                    Ok((_, _, note)) => note,
                    Err(notes::NotesError::NotFound { .. }) => String::new(),
                    Err(err) => return Err(NotesCliError::from(err).into()),
                };
                compose_note_via_editor(&existing, target).await?
            };
            let result = notes::add(notes_ref, target, &content, true)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Edit {
                notes_ref: result.notes_ref,
                object: result.object,
                note_hash: result.note_hash,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Copy {
            from_object,
            to_object,
            force,
        } => {
            // Read the source note (errors if the source object has none),
            // then write it onto the target (`force` overwrites an existing
            // target note) — matching `git notes copy`.
            let (_, _, text) = notes::show(notes_ref, Some(&from_object))
                .await
                .map_err(NotesCliError::from)?;
            let result = notes::add(notes_ref, &to_object, &text, force)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Copy {
                notes_ref: result.notes_ref,
                from_object,
                to_object: result.object,
                note_hash: result.note_hash,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::List { object } => {
            let entries = notes::list(notes_ref, object.as_deref())
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::List {
                notes_ref: notes_ref.to_string(),
                notes: entries
                    .into_iter()
                    .map(|e| NotesListEntry {
                        note_hash: e.note_hash,
                        annotated_object: e.annotated_object,
                    })
                    .collect(),
                object_scoped: object.is_some(),
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Show { object } => {
            let (obj_hash, note_hash, text) = notes::show(notes_ref, object.as_deref())
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Show {
                notes_ref: notes_ref.to_string(),
                object: obj_hash,
                note_hash,
                text,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Remove { objects } => {
            let to_remove = if objects.is_empty() {
                vec!["HEAD".to_string()]
            } else {
                objects
            };
            let removed = notes::remove(notes_ref, &to_remove)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Remove {
                notes_ref: notes_ref.to_string(),
                removed: removed
                    .into_iter()
                    .map(|(obj, hash)| NotesRemovedEntry {
                        object: obj,
                        note_hash: hash,
                    })
                    .collect(),
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Merge {
            other_ref,
            strategy,
        } => {
            let other_normalized =
                notes::normalize_notes_ref(&other_ref).map_err(NotesCliError::from)?;
            let strat = notes::NoteMergeStrategy::parse(strategy.as_deref())
                .map_err(NotesCliError::from)?;
            let result = notes::merge(notes_ref, &other_normalized, strat)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Merge {
                notes_ref: result.notes_ref,
                other_ref: result.other_ref,
                merged: result.merged,
                skipped: result.skipped,
                resolved_conflicts: result.resolved_conflicts,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::Prune { dry_run, verbose } => {
            let pruned = notes::prune(notes_ref, dry_run)
                .await
                .map_err(NotesCliError::from)?;
            let out = NotesOutput::Prune {
                notes_ref: notes_ref.to_string(),
                pruned,
                dry_run,
                verbose,
            };
            render_output(&out, output)?;
        }
        NotesSubcommand::GetRef => {
            let out = NotesOutput::GetRef {
                notes_ref: notes_ref.to_string(),
            };
            render_output(&out, output)?;
        }
    }

    Ok(())
}

/// A content source with the type and value as it appeared on the command line.
#[derive(Debug)]
enum ContentPart {
    Message(String),
    File(String),
}

/// Walk the raw process arguments to rebuild the original `-m`/`-F` occurrence
/// order.  Clap splits them into separate `Vec`s, but `git notes` semantics
/// require that the paragraph order matches the command-line order (e.g.
/// `-F header -m trailer` must produce `header\n\ntrailer`, not the reverse).
fn ordered_content_parts(argv: &[String]) -> Vec<ContentPart> {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-m" || arg == "--message" {
            i += 1;
            if i < argv.len() {
                parts.push(ContentPart::Message(argv[i].clone()));
            }
        } else if arg == "-F" || arg == "--file" {
            i += 1;
            if i < argv.len() {
                parts.push(ContentPart::File(argv[i].clone()));
            }
        } else if let Some(val) = arg.strip_prefix("-m") {
            if !val.is_empty() {
                parts.push(ContentPart::Message(val.to_string()));
            }
        } else if let Some(val) = arg.strip_prefix("-F") {
            if !val.is_empty() {
                parts.push(ContentPart::File(val.to_string()));
            }
        } else if let Some(val) = arg.strip_prefix("--message=") {
            parts.push(ContentPart::Message(val.to_string()));
        } else if let Some(val) = arg.strip_prefix("--file=") {
            parts.push(ContentPart::File(val.to_string()));
        }
        i += 1;
    }
    parts
}

fn build_note_content(argv: &[String]) -> CliResult<String> {
    let ordered = ordered_content_parts(argv);

    let mut parts: Vec<String> = Vec::new();

    for part in &ordered {
        match part {
            ContentPart::Message(msg) => parts.push(msg.clone()),
            ContentPart::File(file_path) => {
                let data = if file_path == "-" {
                    std::io::read_to_string(std::io::stdin())
                        .map_err(|e| CliError::io(format!("failed to read stdin: {e}")))?
                } else {
                    std::fs::read_to_string(file_path)
                        .map_err(|e| CliError::io(format!("failed to read '{file_path}': {e}")))?
                };
                parts.push(data);
            }
        }
    }

    if ordered.is_empty() {
        return Err(
            CliError::command_usage("provide a message with '-m <msg>' or '-F <file>'.")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    let content = parts.join("\n\n");
    if content.trim().is_empty() {
        return Err(CliError::command_usage(
            "empty note content is not allowed. Provide non-empty text with '-m' or a non-empty file with '-F'.",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    Ok(content)
}

/// Whether the invocation supplied note content via `-m`/`--message` or
/// `-F`/`--file`. When false, `add`/`edit`/`append` fall back to an editor.
fn note_content_parts_present(argv: &[String]) -> bool {
    !ordered_content_parts(argv).is_empty()
}

/// Open an editor to compose a note when no `-m`/`-F` was given (`git notes`
/// editor form). `initial` seeds the buffer (the existing note for `edit`,
/// empty for `add`/`append`). Unlike commit/tag messages, a note may legitimately
/// contain lines starting with `#`, so comment lines are NOT stripped — only
/// `git stripspace` whitespace cleanup is applied. An empty result aborts.
async fn compose_note_via_editor(initial: &str, object: &str) -> CliResult<String> {
    use std::io::IsTerminal;

    // An explicitly configured editor runs even without a TTY (so scripted
    // editors work in tests/automation); `vi` is only assumed on a terminal.
    let editor_cmd = match crate::command::editor::resolve_editor().await {
        Some(cmd) => cmd,
        None if std::io::stdin().is_terminal() => "vi".to_string(),
        None => {
            return Err(CliError::fatal(format!(
                "no editor configured to compose the note for '{object}'"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("set GIT_EDITOR, core.editor, VISUAL, or EDITOR")
            .with_hint("or pass the note directly with -m/--message or -F/--file."));
        }
    };

    // Part C §C.4.3: transient per-worktree editor scratch — on shared storage
    // two worktrees composing a message concurrently would truncate each other's
    // buffer. Identical path for the main worktree (local == common storage).
    let path = crate::utils::util::worktree_gitdir().join("NOTES_EDITMSG");
    let raw = crate::command::editor::edit_message(&path, initial, &editor_cmd, true)
        .await
        .map_err(|e| {
            CliError::io(format!("failed to edit the note: {e}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

    let content = clean_note_message(&raw);
    if content.is_empty() {
        return Err(CliError::command_usage("aborting note: empty note content")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("write some text in the editor, or pass -m/--message."));
    }
    Ok(content)
}

/// Whitespace-only `git stripspace` cleanup for an edited note: trim trailing
/// whitespace per line and collapse blank-line runs (dropping leading/trailing
/// blanks). Comment (`#`) lines are preserved — notes may contain them. The
/// result carries NO trailing newline, matching the `-m`/`-F` content path
/// (`build_note_content`) so editor- and message-created notes store identically
/// (a narrowing of Git's stripspace, which appends a final newline).
fn clean_note_message(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pending_blank = false;
    for line in raw.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            pending_blank = !out.is_empty();
            continue;
        }
        if pending_blank {
            out.push(String::new());
            pending_blank = false;
        }
        out.push(trimmed.to_string());
    }
    out.join("\n")
}

fn render_output(result: &NotesOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("notes", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    match result {
        NotesOutput::Add {
            notes_ref,
            object,
            note_hash: _,
        } => {
            println!(
                "Added note to {} in {}",
                short_display_hash(object),
                notes_ref
            );
        }
        NotesOutput::Append {
            notes_ref,
            object,
            note_hash: _,
        } => {
            println!(
                "Appended to note for {} in {}",
                short_display_hash(object),
                notes_ref
            );
        }
        NotesOutput::Edit {
            notes_ref,
            object,
            note_hash: _,
        } => {
            println!(
                "Set note for {} in {}",
                short_display_hash(object),
                notes_ref
            );
        }
        NotesOutput::Copy {
            notes_ref,
            from_object,
            to_object,
            note_hash: _,
        } => {
            println!(
                "Copied note from {} to {} in {}",
                short_display_hash(from_object),
                short_display_hash(to_object),
                notes_ref
            );
        }
        NotesOutput::List {
            notes_ref: _,
            notes,
            object_scoped,
        } => {
            for entry in notes {
                match &entry.note_hash {
                    Some(hash) if *object_scoped => println!("{}", short_display_hash(hash)),
                    Some(hash) => println!(
                        "{} {}",
                        short_display_hash(hash),
                        short_display_hash(&entry.annotated_object)
                    ),
                    None if *object_scoped => println!("(none)"),
                    None => println!("(none) {}", short_display_hash(&entry.annotated_object)),
                }
            }
        }
        NotesOutput::Show { text, .. } => {
            print!("{text}");
        }
        NotesOutput::Remove { notes_ref, removed } => {
            for entry in removed {
                println!(
                    "Removed note from {} in {}",
                    short_display_hash(&entry.object),
                    notes_ref
                );
            }
        }
        NotesOutput::Merge {
            notes_ref,
            other_ref,
            merged,
            skipped,
            resolved_conflicts,
        } => {
            println!(
                "Merged notes from {other_ref} into {notes_ref}: {merged} merged, {skipped} unchanged",
            );
            if !resolved_conflicts.is_empty() {
                println!(
                    "Resolved {} conflict(s) by strategy:",
                    resolved_conflicts.len()
                );
                for object in resolved_conflicts {
                    println!("  {}", short_display_hash(object));
                }
            }
        }
        NotesOutput::Prune {
            notes_ref: _,
            pruned,
            dry_run,
            verbose,
        } => {
            // Git's `notes prune` is silent unless `-v`/`-n`; then it prints the
            // (full) object id of each pruned note, one per line.
            if *verbose || *dry_run {
                for object in pruned {
                    println!("{object}");
                }
            }
        }
        NotesOutput::GetRef { notes_ref } => {
            println!("{notes_ref}");
        }
    }

    Ok(())
}

// ── Error mapping ──────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
enum NotesCliError {
    #[error("{0}")]
    Notes(#[from] notes::NotesError),
}

impl From<NotesCliError> for CliError {
    fn from(error: NotesCliError) -> Self {
        let message = error.to_string();
        match &error {
            NotesCliError::Notes(inner) => match inner {
                notes::NotesError::InvalidNotesRef(_) => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint(
                        "notes refs must start with 'refs/notes/'; e.g. 'refs/notes/commits'.",
                    ),
                notes::NotesError::AlreadyExists { .. } => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint("use '-f' to overwrite the existing note."),
                notes::NotesError::NotFound { .. } => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra notes list' to see which objects have notes."),
                notes::NotesError::InvalidObject(_, _) => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra log' to find valid commit references."),
                notes::NotesError::HeadUnborn => CliError::fatal(message)
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint("create a commit first before adding notes."),
                notes::NotesError::QueryFailed(_) => {
                    CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
                }
                notes::NotesError::ResolveFailed(_) => {
                    CliError::fatal(message).with_stable_code(StableErrorCode::RepoCorrupt)
                }
                notes::NotesError::StoreBlobFailed(_) => {
                    CliError::fatal(message).with_stable_code(StableErrorCode::IoWriteFailed)
                }
                notes::NotesError::MergeConflict { .. } => CliError::failure(message)
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint(
                        "re-run with --strategy=ours/theirs/union/cat_sort_uniq to resolve the conflicting notes",
                    ),
                notes::NotesError::UnsupportedStrategy(_) => CliError::command_usage(message)
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("valid strategies: manual, ours, theirs, union, cat_sort_uniq"),
                notes::NotesError::MergeRaced { .. } => CliError::failure(message)
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint("another writer changed the notes during the merge; re-run it"),
            },
        }
    }
}
