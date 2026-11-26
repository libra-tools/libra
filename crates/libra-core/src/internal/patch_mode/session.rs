//! `add -p` session: auto-advance, `--no-auto-advance`, `s` split, and `e` edit.

use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

use super::{
    edit::{
        CANNOT_EDIT, EDIT_RETRY_PROMPT, EditedHunk, edited_hunk_applies, format_edit_buffer,
        is_editable, parse_edited_buffer, run_editor,
    },
    model::{FileDiff, HunkUse},
};

/// Prompt verb for the current file kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagePrompt {
    Hunk,
    Deletion,
    Addition,
    ModeChange,
}

/// Which patch-mode verb the session uses (`add -p` vs `reset -p`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PatchSessionKind {
    #[default]
    Stage,
    Unstage,
    ApplyToIndex,
}

impl StagePrompt {
    pub fn as_str(self, kind: PatchSessionKind) -> &'static str {
        match (kind, self) {
            (PatchSessionKind::Stage, Self::Hunk) => "Stage this hunk",
            (PatchSessionKind::Stage, Self::Deletion) => "Stage deletion",
            (PatchSessionKind::Stage, Self::Addition) => "Stage addition",
            (PatchSessionKind::Stage, Self::ModeChange) => "Stage mode change",
            (PatchSessionKind::Unstage, Self::Hunk) => "Unstage this hunk",
            (PatchSessionKind::Unstage, Self::Deletion) => "Unstage deletion",
            (PatchSessionKind::Unstage, Self::Addition) => "Unstage addition",
            (PatchSessionKind::Unstage, Self::ModeChange) => "Unstage mode change",
            (PatchSessionKind::ApplyToIndex, Self::Hunk) => "Apply this hunk to index",
            (PatchSessionKind::ApplyToIndex, Self::Deletion) => "Apply deletion to index",
            (PatchSessionKind::ApplyToIndex, Self::Addition) => "Apply addition to index",
            (PatchSessionKind::ApplyToIndex, Self::ModeChange) => "Apply mode change to index",
        }
    }

    pub fn for_file(file: &FileDiff) -> Self {
        if file.deleted {
            Self::Deletion
        } else if file.mode_change && file.hunks.iter().all(synthetic_hunk) {
            Self::ModeChange
        } else if file.added {
            Self::Addition
        } else {
            Self::Hunk
        }
    }
}

/// Session knobs for auto-advance, the hunk editor, and apply-check blobs.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub auto_advance: bool,
    /// `$GIT_EDITOR` / `core.editor` command, already resolved by the caller.
    pub editor: Option<String>,
    /// Gitdir path for `ADD_EDIT.patch`.
    pub edit_path: Option<PathBuf>,
    /// Index blob bytes aligned with the session `files` slice.
    pub index_blobs: Vec<Vec<u8>>,
    /// Prompt / help verb (`Stage` / `Unstage` / `Apply … to index`).
    pub kind: PatchSessionKind,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            auto_advance: true,
            editor: None,
            edit_path: None,
            index_blobs: Vec::new(),
            kind: PatchSessionKind::Stage,
        }
    }
}

/// Letters Git would offer. `s` appears only when the current hunk is
/// splittable; `e` appears when the hunk is editable; `>`/`<` appear only
/// when auto-advance is off and more than one file exists.
pub fn available_letters(
    hunk_index: usize,
    hunk_count: usize,
    has_later_undecided: bool,
    has_earlier_undecided: bool,
) -> String {
    available_letters_ex(
        hunk_index,
        hunk_count,
        has_later_undecided,
        has_earlier_undecided,
        true,
        1,
        false,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn available_letters_ex(
    hunk_index: usize,
    hunk_count: usize,
    has_later_undecided: bool,
    has_earlier_undecided: bool,
    auto_advance: bool,
    file_count: usize,
    splittable: bool,
    editable: bool,
) -> String {
    let mut letters = String::from("y,n,q,a,d");
    let push = |out: &mut String, letter: &str| {
        out.push(',');
        out.push_str(letter);
    };
    if hunk_index > 0 && has_earlier_undecided {
        push(&mut letters, "k");
    }
    if hunk_index > 0 {
        push(&mut letters, "K");
    }
    if has_later_undecided {
        push(&mut letters, "j");
    }
    if hunk_index + 1 < hunk_count {
        push(&mut letters, "J");
    }
    if hunk_count > 1 {
        push(&mut letters, "g");
        push(&mut letters, "/");
    }
    if splittable {
        push(&mut letters, "s");
    }
    if editable {
        push(&mut letters, "e");
    }
    if !auto_advance && file_count > 1 {
        push(&mut letters, ">");
        push(&mut letters, "<");
    }
    push(&mut letters, "p");
    push(&mut letters, "P");
    push(&mut letters, "?");
    letters
}

pub fn format_prompt(
    hunk_index: usize,
    hunk_count: usize,
    prompt: StagePrompt,
    letters: &str,
    was: Option<HunkUse>,
    kind: PatchSessionKind,
) -> String {
    let was = match was {
        Some(HunkUse::Use) => " (was: y)",
        Some(HunkUse::Skip) => " (was: n)",
        _ => "",
    };
    format!(
        "({}/{}) {}{} [{}]? ",
        hunk_index + 1,
        hunk_count.max(1),
        prompt.as_str(kind),
        was,
        letters
    )
}

pub fn help_text(letters: &str) -> String {
    help_text_for(letters, PatchSessionKind::Stage)
}

pub fn help_text_for(letters: &str, kind: PatchSessionKind) -> String {
    let mut out = String::new();
    let line = |out: &mut String, letter: char, text: &str| {
        if letters.split(',').any(|item| item == letter.to_string()) {
            out.push_str(&format!("{letter} - {text}\n"));
        }
    };
    let (y, n, q, a, d) = match kind {
        PatchSessionKind::Stage => (
            "stage this hunk",
            "do not stage this hunk",
            "quit; do not stage this hunk or any of the remaining ones",
            "stage this hunk and all later hunks in the file",
            "do not stage this hunk or any of the later hunks in the file",
        ),
        PatchSessionKind::Unstage => (
            "unstage this hunk",
            "do not unstage this hunk",
            "quit; do not unstage this hunk or any of the remaining ones",
            "unstage this hunk and all later hunks in the file",
            "do not unstage this hunk or any of the later hunks in the file",
        ),
        PatchSessionKind::ApplyToIndex => (
            "apply this hunk to index",
            "do not apply this hunk to index",
            "quit; do not apply this hunk or any of the remaining ones",
            "apply this hunk and all later hunks in the file",
            "do not apply this hunk or any of the later hunks in the file",
        ),
    };
    line(&mut out, 'y', y);
    line(&mut out, 'n', n);
    line(&mut out, 'q', q);
    line(&mut out, 'a', a);
    line(&mut out, 'd', d);
    line(
        &mut out,
        'j',
        "leave this hunk undecided, see next undecided hunk",
    );
    line(&mut out, 'J', "leave this hunk undecided, see next hunk");
    line(
        &mut out,
        'k',
        "leave this hunk undecided, see previous undecided hunk",
    );
    line(
        &mut out,
        'K',
        "leave this hunk undecided, see previous hunk",
    );
    line(&mut out, 'g', "select a hunk to go to");
    line(&mut out, '/', "search for a hunk matching the given regex");
    line(&mut out, 's', "split the current hunk into smaller hunks");
    line(&mut out, 'e', "manually edit the current hunk");
    line(&mut out, '>', "select the next file");
    line(&mut out, '<', "select the previous file");
    line(&mut out, 'p', "print the current hunk");
    line(&mut out, 'P', "print the current hunk using the pager");
    line(&mut out, '?', "print help");
    out
}

pub fn hunks_summary(file: &FileDiff) -> Option<String> {
    if file
        .hunks
        .iter()
        .any(|hunk| hunk.use_decision == HunkUse::Undecided)
    {
        return None;
    }
    let use_n = file
        .hunks
        .iter()
        .filter(|hunk| hunk.use_decision == HunkUse::Use)
        .count();
    let skip_n = file
        .hunks
        .iter()
        .filter(|hunk| hunk.use_decision == HunkUse::Skip)
        .count();
    Some(format!(
        "HUNKS SUMMARY - Hunks: {}, USE: {}, SKIP: {}",
        file.hunks.len(),
        use_n,
        skip_n
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Continue,
    Quit,
}

/// Drive one patch-mode session. Decisions stay on `files`; the caller writes
/// the index once after the function returns.
pub fn run_session<R, W>(
    files: &mut [FileDiff],
    input: &mut R,
    output: &mut W,
) -> std::io::Result<SessionAction>
where
    R: BufRead,
    W: Write,
{
    run_session_with(files, input, output, SessionOptions::default())
}

pub fn run_session_with<R, W>(
    files: &mut [FileDiff],
    input: &mut R,
    output: &mut W,
    options: SessionOptions,
) -> std::io::Result<SessionAction>
where
    R: BufRead,
    W: Write,
{
    if files.is_empty() {
        writeln!(output, "No changes.")?;
        return Ok(SessionAction::Quit);
    }
    if files.iter().all(|file| file.binary) {
        writeln!(output, "Only binary files changed.")?;
        return Ok(SessionAction::Quit);
    }

    let selectable: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.binary)
        .map(|(i, _)| i)
        .collect();
    if selectable.is_empty() {
        writeln!(output, "Only binary files changed.")?;
        return Ok(SessionAction::Quit);
    }

    let mut sel = 0usize;
    let mut cursors = vec![0usize; files.len()];
    let mut last_rendered_file: Option<usize> = None;
    loop {
        let file_i = selectable[sel];
        ensure_hunk_slot(&mut files[file_i]);
        let hunk_count = files[file_i].hunks.len().max(1);
        if cursors[file_i] >= hunk_count {
            cursors[file_i] = hunk_count - 1;
        }
        let hunk_idx = cursors[file_i];
        let print_header = last_rendered_file != Some(file_i);
        render_current(files, file_i, hunk_idx, print_header, output)?;
        last_rendered_file = Some(file_i);
        let letters = letters_for(files, &selectable, sel, hunk_idx, &options);
        let prompt = StagePrompt::for_file(&files[file_i]);
        let was = files[file_i]
            .hunks
            .get(hunk_idx)
            .map(|hunk| hunk.use_decision)
            .filter(|decision| *decision != HunkUse::Undecided);
        write!(
            output,
            "{}",
            format_prompt(hunk_idx, hunk_count, prompt, &letters, was, options.kind)
        )?;
        output.flush()?;

        let mut line = String::new();
        let n = input.read_line(&mut line)?;
        if n == 0 {
            return Ok(SessionAction::Quit);
        }
        let answer = line.trim_end_matches(['\n', '\r']);
        match dispatch(
            answer,
            files,
            &selectable,
            &mut sel,
            &mut cursors,
            &options,
            input,
            output,
        )? {
            SessionAction::Quit => return Ok(SessionAction::Quit),
            SessionAction::Continue => {
                if sel >= selectable.len() {
                    return Ok(SessionAction::Continue);
                }
            }
        }
    }
}

fn ensure_hunk_slot(file: &mut FileDiff) {
    if file.hunks.is_empty() && (file.mode_change || file.deleted || file.added) {
        file.hunks.push(super::model::Hunk {
            old_start: 0,
            old_lines: 0,
            new_start: 0,
            new_lines: 0,
            header: String::new(),
            lines: Vec::new(),
            no_newline_old: false,
            no_newline_new: false,
            use_decision: HunkUse::Undecided,
        });
    }
}

fn letters_for(
    files: &[FileDiff],
    selectable: &[usize],
    sel: usize,
    hunk_idx: usize,
    options: &SessionOptions,
) -> String {
    let file = &files[selectable[sel]];
    let hunk_count = file.hunks.len().max(1);
    let has_later = (hunk_idx + 1..hunk_count).any(|i| {
        file.hunks
            .get(i)
            .is_some_and(|h| h.use_decision == HunkUse::Undecided)
    });
    let has_earlier = (0..hunk_idx).any(|i| {
        file.hunks
            .get(i)
            .is_some_and(|h| h.use_decision == HunkUse::Undecided)
    });
    let hunk = file.hunks.get(hunk_idx);
    let splittable = hunk.is_some_and(|hunk| hunk.is_splittable());
    let editable = hunk.is_some_and(|hunk| is_editable(file, hunk));
    available_letters_ex(
        hunk_idx,
        hunk_count,
        has_later,
        has_earlier,
        options.auto_advance,
        selectable.len(),
        splittable,
        editable,
    )
}

fn render_current<W: Write>(
    files: &[FileDiff],
    file_i: usize,
    hunk_idx: usize,
    print_header: bool,
    output: &mut W,
) -> std::io::Result<()> {
    let file = &files[file_i];
    if print_header {
        write!(output, "{}", file.header)?;
    }
    if let Some(hunk) = file.hunks.get(hunk_idx)
        && !hunk.header.is_empty()
    {
        writeln!(output, "{}", hunk.header)?;
        for line in &hunk.lines {
            let marker = match line {
                super::model::HunkLine::Context(_) => ' ',
                super::model::HunkLine::Delete(_) => '-',
                super::model::HunkLine::Insert(_) => '+',
            };
            let text = match line {
                super::model::HunkLine::Context(t)
                | super::model::HunkLine::Delete(t)
                | super::model::HunkLine::Insert(t) => t,
            };
            writeln!(output, "{marker}{text}")?;
        }
    }
    Ok(())
}

fn synthetic_hunk(hunk: &super::model::Hunk) -> bool {
    hunk.header.is_empty() && hunk.lines.is_empty()
}

#[allow(clippy::too_many_arguments)]
fn dispatch<R, W>(
    answer: &str,
    files: &mut [FileDiff],
    selectable: &[usize],
    sel: &mut usize,
    cursors: &mut [usize],
    options: &SessionOptions,
    input: &mut R,
    output: &mut W,
) -> std::io::Result<SessionAction>
where
    R: BufRead,
    W: Write,
{
    if answer.len() != 1 && !matches!(answer.chars().next(), Some('g' | '/')) {
        if answer.is_empty() {
            writeln!(output, "Only one letter is expected, got ''")?;
        } else {
            writeln!(output, "Only one letter is expected, got '{answer}'")?;
        }
        return Ok(SessionAction::Continue);
    }
    let file_i = selectable[*sel];
    ensure_hunk_slot(&mut files[file_i]);
    let hunk_count = files[file_i].hunks.len().max(1);
    let hunk_idx = cursors[file_i];
    match answer {
        "y" => {
            set_current(files, file_i, hunk_idx, HunkUse::Use);
            advance(files, selectable, sel, cursors, options.auto_advance)
        }
        "n" => {
            set_current(files, file_i, hunk_idx, HunkUse::Skip);
            advance(files, selectable, sel, cursors, options.auto_advance)
        }
        "q" => Ok(SessionAction::Quit),
        "a" => {
            for hunk in files[file_i].hunks.iter_mut().skip(hunk_idx) {
                hunk.use_decision = HunkUse::Use;
            }
            if options.auto_advance {
                *sel += 1;
            }
            Ok(SessionAction::Continue)
        }
        "d" => {
            for hunk in files[file_i].hunks.iter_mut().skip(hunk_idx) {
                hunk.use_decision = HunkUse::Skip;
            }
            if options.auto_advance {
                *sel += 1;
            }
            Ok(SessionAction::Continue)
        }
        "j" => move_undecided(files, file_i, &mut cursors[file_i], 1, output),
        "J" => {
            if hunk_idx + 1 < hunk_count {
                cursors[file_i] += 1;
            } else {
                writeln!(output, "No other hunk")?;
            }
            Ok(SessionAction::Continue)
        }
        "s" => split_current(files, file_i, cursors, output),
        "e" => {
            if edit_current(files, file_i, cursors[file_i], options, input, output)? {
                advance(files, selectable, sel, cursors, options.auto_advance)
            } else {
                Ok(SessionAction::Continue)
            }
        }
        "k" => move_undecided(files, file_i, &mut cursors[file_i], -1, output),
        "K" => {
            if hunk_idx > 0 {
                cursors[file_i] -= 1;
            } else {
                writeln!(output, "No other hunk")?;
            }
            Ok(SessionAction::Continue)
        }
        ">" if !options.auto_advance => {
            if selectable.len() <= 1 {
                writeln!(output, "No next file")?;
            } else {
                *sel = (*sel + 1) % selectable.len();
            }
            Ok(SessionAction::Continue)
        }
        "<" if !options.auto_advance => {
            if selectable.len() <= 1 {
                writeln!(output, "No previous file")?;
            } else {
                *sel = (*sel + selectable.len() - 1) % selectable.len();
            }
            Ok(SessionAction::Continue)
        }
        "p" | "P" => Ok(SessionAction::Continue),
        "?" => {
            let letters = letters_for(files, selectable, *sel, hunk_idx, options);
            write!(output, "{}", help_text_for(&letters, options.kind))?;
            if let Some(summary) = hunks_summary(&files[file_i]) {
                writeln!(output, "{summary}")?;
            }
            Ok(SessionAction::Continue)
        }
        other if other.starts_with('g') => {
            goto_hunk(other, files, file_i, &mut cursors[file_i], input, output)
        }
        other if other.starts_with('/') => {
            search_hunk(other, files, file_i, &mut cursors[file_i], input, output)
        }
        other => {
            writeln!(output, "Unknown command '{other}' (use '?' for help)")?;
            Ok(SessionAction::Continue)
        }
    }
}

fn set_current(files: &mut [FileDiff], file_i: usize, hunk_idx: usize, decision: HunkUse) {
    if let Some(hunk) = files[file_i].hunks.get_mut(hunk_idx) {
        hunk.use_decision = decision;
    }
}

fn split_current<W: Write>(
    files: &mut [FileDiff],
    file_i: usize,
    cursors: &mut [usize],
    output: &mut W,
) -> std::io::Result<SessionAction> {
    let hunk_idx = cursors[file_i];
    let Some(hunk) = files[file_i].hunks.get(hunk_idx).cloned() else {
        writeln!(output, "Sorry, cannot split this hunk")?;
        return Ok(SessionAction::Continue);
    };
    match crate::internal::patch_mode::split_hunk(&hunk) {
        Some(pieces) if pieces.len() > 1 => {
            let n = pieces.len();
            files[file_i].hunks.splice(hunk_idx..=hunk_idx, pieces);
            writeln!(output, "Split into {n} hunks.")?;
        }
        _ => writeln!(output, "Sorry, cannot split this hunk")?,
    }
    Ok(SessionAction::Continue)
}

fn edit_current<R, W>(
    files: &mut [FileDiff],
    file_i: usize,
    hunk_idx: usize,
    options: &SessionOptions,
    input: &mut R,
    output: &mut W,
) -> std::io::Result<bool>
where
    R: BufRead,
    W: Write,
{
    let Some(hunk) = files[file_i].hunks.get(hunk_idx).cloned() else {
        writeln!(output, "{CANNOT_EDIT}")?;
        return Ok(false);
    };
    if !is_editable(&files[file_i], &hunk) {
        writeln!(output, "{CANNOT_EDIT}")?;
        return Ok(false);
    }
    let Some(editor) = options.editor.as_deref() else {
        writeln!(output, "{CANNOT_EDIT}")?;
        return Ok(false);
    };
    let path = options
        .edit_path
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("ADD_EDIT.patch"));
    loop {
        let initial = format_edit_buffer(&hunk);
        let edited_text = match run_editor(editor, &path, &initial) {
            Ok(text) => text,
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                writeln!(output, "{CANNOT_EDIT}")?;
                return Ok(false);
            }
        };
        match parse_edited_buffer(&edited_text, &hunk) {
            Ok(EditedHunk::Abandoned) => {
                let _ = std::fs::remove_file(&path);
                return Ok(false);
            }
            Ok(EditedHunk::Hunk(edited)) => {
                let applies = match options.index_blobs.get(file_i) {
                    Some(bytes) => {
                        edited_hunk_applies(&files[file_i], hunk_idx, &edited, bytes).is_ok()
                    }
                    None => true,
                };
                if applies {
                    if let Some(slot) = files[file_i].hunks.get_mut(hunk_idx) {
                        *slot = edited;
                        slot.use_decision = HunkUse::Use;
                    }
                    let _ = std::fs::remove_file(&path);
                    return Ok(true);
                }
            }
            Err(_) => {}
        }
        loop {
            write!(output, "{EDIT_RETRY_PROMPT}")?;
            output.flush()?;
            let mut line = String::new();
            let n = input.read_line(&mut line)?;
            if n == 0 || matches!(line.chars().next(), Some('n' | 'N')) {
                let _ = std::fs::remove_file(&path);
                return Ok(false);
            }
            if matches!(line.chars().next(), Some('y' | 'Y')) {
                break;
            }
        }
    }
}

fn advance(
    files: &[FileDiff],
    selectable: &[usize],
    sel: &mut usize,
    cursors: &mut [usize],
    auto: bool,
) -> std::io::Result<SessionAction> {
    if !auto {
        return Ok(SessionAction::Continue);
    }
    let file_i = selectable[*sel];
    let hunk_count = files[file_i].hunks.len().max(1);
    if cursors[file_i] + 1 < hunk_count {
        cursors[file_i] += 1;
        return Ok(SessionAction::Continue);
    }
    *sel += 1;
    Ok(SessionAction::Continue)
}

fn move_undecided<W: Write>(
    files: &[FileDiff],
    file_i: usize,
    hunk_idx: &mut usize,
    step: isize,
    output: &mut W,
) -> std::io::Result<SessionAction> {
    let hunks = &files[file_i].hunks;
    let mut i = *hunk_idx as isize + step;
    while i >= 0 && (i as usize) < hunks.len() {
        if hunks[i as usize].use_decision == HunkUse::Undecided {
            *hunk_idx = i as usize;
            return Ok(SessionAction::Continue);
        }
        i += step;
    }
    writeln!(output, "No other hunk")?;
    Ok(SessionAction::Continue)
}

fn read_followup<R: BufRead>(
    input: &mut R,
    output: &mut impl Write,
    prompt: &str,
) -> std::io::Result<Option<String>> {
    write!(output, "{prompt}")?;
    output.flush()?;
    let mut line = String::new();
    let n = input.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
}

fn goto_hunk<R, W>(
    answer: &str,
    files: &[FileDiff],
    file_i: usize,
    hunk_idx: &mut usize,
    input: &mut R,
    output: &mut W,
) -> std::io::Result<SessionAction>
where
    R: BufRead,
    W: Write,
{
    if files[file_i].hunks.len() <= 1 {
        writeln!(output, "No other hunks to goto")?;
        return Ok(SessionAction::Continue);
    }
    let rest = answer[1..].trim();
    let rest = if rest.is_empty() {
        match read_followup(input, output, "go to which hunk? ")? {
            Some(value) => value,
            None => return Ok(SessionAction::Quit),
        }
    } else {
        rest.to_string()
    };
    let Ok(n) = rest.parse::<usize>() else {
        writeln!(output, "No other hunks to goto")?;
        return Ok(SessionAction::Continue);
    };
    if n == 0 || n > files[file_i].hunks.len() {
        writeln!(output, "No other hunks to goto")?;
        return Ok(SessionAction::Continue);
    }
    *hunk_idx = n - 1;
    Ok(SessionAction::Continue)
}

fn search_hunk<R, W>(
    answer: &str,
    files: &[FileDiff],
    file_i: usize,
    hunk_idx: &mut usize,
    input: &mut R,
    output: &mut W,
) -> std::io::Result<SessionAction>
where
    R: BufRead,
    W: Write,
{
    if files[file_i].hunks.len() <= 1 {
        writeln!(output, "No other hunks to search")?;
        return Ok(SessionAction::Continue);
    }
    let needle = answer[1..].trim();
    let needle = if needle.is_empty() {
        match read_followup(input, output, "search for regex? ")? {
            Some(value) => value,
            None => return Ok(SessionAction::Quit),
        }
    } else {
        needle.to_string()
    };
    if needle.is_empty() {
        writeln!(output, "No other hunks to search")?;
        return Ok(SessionAction::Continue);
    }
    let Ok(re) = regex::Regex::new(&needle) else {
        writeln!(output, "No other hunks to search")?;
        return Ok(SessionAction::Continue);
    };
    let hunks = &files[file_i].hunks;
    let start = *hunk_idx + 1;
    for offset in 0..hunks.len() {
        let i = (start + offset) % hunks.len();
        if i == *hunk_idx {
            continue;
        }
        let blob = format!("{}{:?}", hunks[i].header, hunks[i].lines);
        if re.is_match(&blob) {
            *hunk_idx = i;
            return Ok(SessionAction::Continue);
        }
    }
    writeln!(output, "No other hunks to search")?;
    Ok(SessionAction::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_letters_omit_unimplemented_commands() {
        let letters = available_letters(0, 2, true, false);
        assert!(letters.contains("y,n,q,a,d"));
        assert!(letters.contains("j"));
        assert!(letters.contains("J"));
        assert!(letters.contains("g"));
        assert!(letters.contains("/"));
        assert!(!letters.split(',').any(|l| l == "s"));
        assert!(letters.split(',').any(|l| l == "e"));
        assert!(!letters.split(',').any(|l| l == ">"));
        assert!(!letters.split(',').any(|l| l == "<"));
    }

    #[test]
    fn first_hunk_omits_k_and_capital_k() {
        let letters = available_letters(0, 1, false, false);
        assert!(!letters.split(',').any(|l| l == "k"));
        assert!(!letters.split(',').any(|l| l == "K"));
        assert!(!letters.split(',').any(|l| l == "j"));
        assert!(!letters.split(',').any(|l| l == "J"));
    }

    #[test]
    fn help_filters_to_available_letters() {
        let help = help_text("y,n,q,?");
        assert!(help.contains("y - stage this hunk"));
        assert!(!help.contains("j -"));
    }

    #[test]
    fn prompt_letters_follow_add_patch_availability() {
        let mid = available_letters(1, 3, true, true);
        assert_eq!(mid, "y,n,q,a,d,k,K,j,J,g,/,e,p,P,?");
        let last_no_undecided = available_letters(2, 3, false, false);
        assert_eq!(last_no_undecided, "y,n,q,a,d,K,g,/,e,p,P,?");
        let first_of_three = available_letters(0, 3, true, false);
        assert_eq!(first_of_three, "y,n,q,a,d,j,J,g,/,e,p,P,?");
    }

    fn dummy_file(path: &str, hunks: usize) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            header: format!("diff --git a/{path} b/{path}\n"),
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            added: false,
            deleted: false,
            mode_change: false,
            binary: false,
            hunks: (0..hunks)
                .map(|i| super::super::model::Hunk {
                    old_start: i * 10 + 1,
                    old_lines: 1,
                    new_start: i * 10 + 1,
                    new_lines: 1,
                    header: format!("@@ -{},1 +{},1 @@", i * 10 + 1, i * 10 + 1),
                    lines: vec![
                        super::super::model::HunkLine::Delete(format!("old{i}")),
                        super::super::model::HunkLine::Insert(format!("new{i}")),
                    ],
                    no_newline_old: false,
                    no_newline_new: false,
                    use_decision: HunkUse::Undecided,
                })
                .collect(),
        }
    }

    #[test]
    fn session_y_n_q_auto_advances_and_keeps_prior_yes() {
        let mut files = vec![dummy_file("a.txt", 2), dummy_file("b.txt", 1)];
        let input = b"?\nzz\nw\ny\nn\nq\n";
        let mut output = Vec::new();
        let action = run_session(&mut files, &mut input.as_slice(), &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(action, SessionAction::Quit);
        assert!(text.contains("y - stage this hunk"));
        assert!(text.contains("Only one letter is expected, got 'zz'"));
        assert!(text.contains("Unknown command 'w' (use '?' for help)"));
        assert_eq!(files[0].hunks[0].use_decision, HunkUse::Use);
        assert_eq!(files[0].hunks[1].use_decision, HunkUse::Skip);
        assert_eq!(files[1].hunks[0].use_decision, HunkUse::Undecided);
    }

    #[test]
    fn session_a_d_and_goto_search() {
        let mut files = vec![dummy_file("a.txt", 3), dummy_file("b.txt", 1)];
        let input = b"g3\n/\nold1\na\nd\n";
        let mut output = Vec::new();
        run_session(&mut files, &mut input.as_slice(), &mut output).unwrap();
        assert_eq!(files[0].hunks[0].use_decision, HunkUse::Undecided);
        assert_eq!(files[0].hunks[1].use_decision, HunkUse::Use);
        assert_eq!(files[0].hunks[2].use_decision, HunkUse::Use);
        assert_eq!(files[1].hunks[0].use_decision, HunkUse::Skip);
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("(3/3) Stage this hunk"));
    }

    #[test]
    fn session_empty_and_binary_messages() {
        let mut none: Vec<FileDiff> = Vec::new();
        let mut out = Vec::new();
        run_session(&mut none, &mut (&b""[..]), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "No changes.\n");

        let mut files = vec![FileDiff {
            path: "bin".into(),
            header: String::new(),
            old_mode: None,
            new_mode: None,
            added: false,
            deleted: false,
            mode_change: false,
            binary: true,
            hunks: Vec::new(),
        }];
        let mut out = Vec::new();
        run_session(&mut files, &mut (&b""[..]), &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Only binary files changed.\n"
        );
    }

    #[test]
    fn no_auto_advance_stays_and_cycles_files() {
        let mut files = vec![dummy_file("first-file", 1), dummy_file("second-file", 1)];
        let input = b"n\n>\n<\ny\nq\n";
        let mut output = Vec::new();
        run_session_with(
            &mut files,
            &mut input.as_slice(),
            &mut output,
            SessionOptions {
                auto_advance: false,
                ..SessionOptions::default()
            },
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("(was: n)"));
        assert!(
            text.contains("diff --git a/second-file b/second-file") || text.contains("second-file")
        );
        assert_eq!(files[0].hunks[0].use_decision, HunkUse::Use);
        assert_eq!(files[1].hunks[0].use_decision, HunkUse::Undecided);
    }

    #[test]
    fn no_auto_advance_single_file_rejects_file_nav() {
        let mut files = vec![dummy_file("only", 3)];
        let input = b"a\nn\n?\n>\n<\nq\n";
        let mut output = Vec::new();
        run_session_with(
            &mut files,
            &mut input.as_slice(),
            &mut output,
            SessionOptions {
                auto_advance: false,
                ..SessionOptions::default()
            },
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("No next file"));
        assert!(text.contains("No previous file"));
        assert!(text.contains("HUNKS SUMMARY - Hunks: 3, USE: 2, SKIP: 1"));
        assert!(
            !available_letters_ex(0, 3, true, false, false, 1, false, true)
                .split(',')
                .any(|l| l == ">" || l == "<")
        );
    }

    #[test]
    fn splittable_hunk_offers_s_and_e() {
        let letters = available_letters_ex(0, 1, false, false, true, 1, true, true);
        assert!(letters.split(',').any(|l| l == "s"));
        assert!(letters.split(',').any(|l| l == "e"));
        assert!(help_text(&letters).contains("s - split the current hunk into smaller hunks"));
        assert!(help_text(&letters).contains("e - manually edit the current hunk"));
    }

    fn splittable_file(path: &str) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            header: format!("diff --git a/{path} b/{path}\n"),
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            added: false,
            deleted: false,
            mode_change: false,
            binary: false,
            hunks: vec![super::super::model::Hunk {
                old_start: 1,
                old_lines: 5,
                new_start: 1,
                new_lines: 5,
                header: "@@ -1,5 +1,5 @@".into(),
                lines: vec![
                    super::super::model::HunkLine::Context("keep".into()),
                    super::super::model::HunkLine::Delete("old1".into()),
                    super::super::model::HunkLine::Insert("new1".into()),
                    super::super::model::HunkLine::Context("mid".into()),
                    super::super::model::HunkLine::Delete("old2".into()),
                    super::super::model::HunkLine::Insert("new2".into()),
                ],
                no_newline_old: false,
                no_newline_new: false,
                use_decision: HunkUse::Undecided,
            }],
        }
    }

    #[test]
    fn session_s_splits_and_unsplittable_is_sorry() {
        let mut files = vec![splittable_file("a.txt"), dummy_file("b.txt", 1)];
        let input = b"s\ny\nn\ns\nq\n";
        let mut output = Vec::new();
        run_session(&mut files, &mut input.as_slice(), &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Split into 2 hunks."), "{text}");
        assert!(text.contains("Sorry, cannot split this hunk"), "{text}");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].use_decision, HunkUse::Use);
        assert_eq!(files[0].hunks[1].use_decision, HunkUse::Skip);
    }

    #[test]
    fn session_e_uses_hunk_and_sorry_on_deletion() {
        let path =
            std::env::temp_dir().join(format!("libra-hf19-add-edit-{}.patch", std::process::id()));
        let mut files = vec![dummy_file("a.txt", 1)];
        let input = b"e\n";
        let mut output = Vec::new();
        run_session_with(
            &mut files,
            &mut input.as_slice(),
            &mut output,
            SessionOptions {
                editor: Some("true".into()),
                edit_path: Some(path.clone()),
                ..SessionOptions::default()
            },
        )
        .unwrap();
        assert_eq!(files[0].hunks[0].use_decision, HunkUse::Use);

        let mut gone = dummy_file("gone.txt", 1);
        gone.deleted = true;
        let input = b"e\nq\n";
        let mut output = Vec::new();
        run_session_with(
            &mut [gone],
            &mut input.as_slice(),
            &mut output,
            SessionOptions {
                editor: Some("true".into()),
                edit_path: Some(path),
                ..SessionOptions::default()
            },
        )
        .unwrap();
        assert!(
            String::from_utf8(output).unwrap().contains(CANNOT_EDIT),
            "deletion should refuse e"
        );
    }
}
