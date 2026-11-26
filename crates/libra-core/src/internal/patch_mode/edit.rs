//! Manual hunk edit buffer (`e`) for patch mode (ADR-HF-18 §2).
//!
//! The buffer text matches Git 2.54 `add-patch.c` (`edit_hunk_manually`): a
//! commented header, the current hunk, then the commented footer. Comment
//! lines start with `# ` and are stripped on read-back. An empty remaining
//! hunk aborts the edit and leaves the original hunk unchanged.

use std::path::Path;

use super::{
    apply::{PatchApplyError, PatchApplyMode, apply_selected_hunks_to_blob},
    model::{FileDiff, Hunk, HunkLine, HunkUse, ParseError},
};

/// First line of Git's hunk-edit buffer (including the `# ` prefix).
pub const MANUAL_HUNK_HEADER: &str = "# Manual hunk edit mode -- see bottom for a quick guide.";

/// Prompt shown when the edited hunk does not apply (Git wording, trailing space).
pub const EDIT_RETRY_PROMPT: &str =
    "Your edited hunk does not apply. Edit again (saying \"no\" discards!) [y/n]? ";

pub const CANNOT_EDIT: &str = "Sorry, cannot edit this hunk";

const FOOTER: &str = "\
# ---
# To remove '-' lines, make them ' ' lines (context).
# To remove '+' lines, delete them.
# Lines starting with # will be removed.
# If the patch applies cleanly, the edited hunk will immediately be marked for staging.
# If it does not apply cleanly, you will be given an opportunity to
# edit again. If all lines of the hunk are removed, then the edit is
# aborted and the hunk is left unchanged.
";

/// Outcome of reading the editor buffer back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditedHunk {
    /// Every non-comment line was removed — Git leaves the hunk unchanged.
    Abandoned,
    Hunk(Hunk),
}

/// Deletion files and synthetic mode-only hunks cannot be edited (Git
/// `ALLOW_EDIT`: `!deleted` and `hunk_index + 1 > mode_change`).
pub fn is_editable(file: &FileDiff, hunk: &Hunk) -> bool {
    if file.deleted {
        return false;
    }
    if file.mode_change && hunk.header.is_empty() && hunk.lines.is_empty() {
        return false;
    }
    !hunk.header.is_empty() || !hunk.lines.is_empty()
}

/// Git 2.54 `edit_hunk_manually` buffer for `hunk`.
pub fn format_edit_buffer(hunk: &Hunk) -> String {
    let mut out = String::from(MANUAL_HUNK_HEADER);
    out.push('\n');
    if !hunk.header.is_empty() {
        out.push_str(&hunk.header);
        if !hunk.header.ends_with('\n') {
            out.push('\n');
        }
    }
    for line in &hunk.lines {
        out.push(line.marker());
        out.push_str(line.text());
        out.push('\n');
    }
    if hunk.no_newline_old || hunk.no_newline_new {
        out.push_str("\\ No newline at end of file\n");
    }
    out.push_str(FOOTER);
    out
}

/// Strip `#` comment lines and rebuild a hunk. Empty remainder is [`EditedHunk::Abandoned`].
pub fn parse_edited_buffer(text: &str, original: &Hunk) -> Result<EditedHunk, ParseError> {
    let kept: Vec<&str> = text
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.starts_with('#'))
        .collect();
    if kept.iter().all(|line| line.trim().is_empty()) {
        return Ok(EditedHunk::Abandoned);
    }

    let (header_line, body) = if kept
        .first()
        .is_some_and(|line| line.starts_with("@@ ") || *line == "@@")
    {
        (kept[0], &kept[1..])
    } else {
        (original.header.as_str(), kept.as_slice())
    };

    let mut parsed = if header_line.starts_with("@@") {
        parse_header_line(header_line)?
    } else {
        original.clone()
    };
    parsed.lines.clear();
    parsed.no_newline_old = false;
    parsed.no_newline_new = false;
    parsed.use_decision = HunkUse::Undecided;

    let mut pending_no_nl_for_insert = false;
    for line in body {
        if *line == "\\ No newline at end of file" {
            if pending_no_nl_for_insert || matches!(parsed.lines.last(), Some(HunkLine::Insert(_)))
            {
                parsed.no_newline_new = true;
            } else {
                parsed.no_newline_old = true;
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let (marker, rest) = line.split_at(1);
        match marker {
            " " => {
                pending_no_nl_for_insert = false;
                parsed.lines.push(HunkLine::Context(rest.to_string()));
            }
            "-" => {
                pending_no_nl_for_insert = false;
                parsed.lines.push(HunkLine::Delete(rest.to_string()));
            }
            "+" => {
                pending_no_nl_for_insert = true;
                parsed.lines.push(HunkLine::Insert(rest.to_string()));
            }
            _ => return Err(ParseError::BadHunkLine((*line).to_string())),
        }
    }

    let (old_lines, new_lines) = line_counts(&parsed.lines);
    parsed.old_lines = old_lines;
    parsed.new_lines = new_lines;
    parsed.header = crate::utils::diff_format::format_hunk_header(
        parsed.old_start,
        parsed.old_lines,
        parsed.new_start,
        parsed.new_lines,
    );
    if let Some(suffix) = original
        .header
        .splitn(3, " @@")
        .nth(2)
        .filter(|text| !text.is_empty())
    {
        parsed.header.push_str(suffix);
    }
    Ok(EditedHunk::Hunk(parsed))
}

/// Apply-check the file with `edited` substituting the hunk at `hunk_idx`.
pub fn edited_hunk_applies(
    file: &FileDiff,
    hunk_idx: usize,
    edited: &Hunk,
    old_bytes: &[u8],
) -> Result<(), PatchApplyError> {
    let mut trial = file.clone();
    if let Some(slot) = trial.hunks.get_mut(hunk_idx) {
        *slot = edited.clone();
        slot.use_decision = HunkUse::Use;
    }
    apply_selected_hunks_to_blob(old_bytes, &trial, PatchApplyMode::Stage).map(|_| ())
}

/// Write `initial`, run `editor` on `path`, and return the file contents.
pub fn run_editor(editor: &str, path: &Path, initial: &str) -> Result<String, std::io::Error> {
    std::fs::write(path, initial)?;
    let quoted = shell_single_quote(&path.display().to_string());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} {quoted}"))
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "editor '{editor}' exited abnormally"
        )));
    }
    std::fs::read_to_string(path)
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn parse_header_line(line: &str) -> Result<Hunk, ParseError> {
    let body = line
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split(" @@").next())
        .ok_or_else(|| ParseError::BadHunkHeader(line.to_string()))?;
    let mut parts = body.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| ParseError::BadHunkHeader(line.to_string()))?;
    let new = parts
        .next()
        .ok_or_else(|| ParseError::BadHunkHeader(line.to_string()))?;
    let (old_start, old_lines) = parse_span(old, '-')?;
    let (new_start, new_lines) = parse_span(new, '+')?;
    Ok(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        header: line.to_string(),
        lines: Vec::new(),
        no_newline_old: false,
        no_newline_new: false,
        use_decision: HunkUse::Undecided,
    })
}

fn parse_span(spec: &str, sign: char) -> Result<(usize, usize), ParseError> {
    let spec = spec
        .strip_prefix(sign)
        .ok_or_else(|| ParseError::BadHunkHeader(spec.to_string()))?;
    if let Some((start, count)) = spec.split_once(',') {
        let start = start
            .parse()
            .map_err(|_| ParseError::BadHunkHeader(spec.to_string()))?;
        let count = count
            .parse()
            .map_err(|_| ParseError::BadHunkHeader(spec.to_string()))?;
        Ok((start, count))
    } else {
        let start = spec
            .parse()
            .map_err(|_| ParseError::BadHunkHeader(spec.to_string()))?;
        Ok((start, 1))
    }
}

fn line_counts(lines: &[HunkLine]) -> (usize, usize) {
    let mut old = 0usize;
    let mut new = 0usize;
    for line in lines {
        match line {
            HunkLine::Context(_) => {
                old += 1;
                new += 1;
            }
            HunkLine::Delete(_) => old += 1,
            HunkLine::Insert(_) => new += 1,
        }
    }
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hunk() -> Hunk {
        Hunk {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            header: "@@ -1,3 +1,3 @@".into(),
            lines: vec![
                HunkLine::Context("keep".into()),
                HunkLine::Delete("old".into()),
                HunkLine::Insert("new".into()),
            ],
            no_newline_old: false,
            no_newline_new: false,
            use_decision: HunkUse::Undecided,
        }
    }

    #[test]
    fn buffer_matches_git_header_and_footer() {
        let text = format_edit_buffer(&sample_hunk());
        assert!(text.starts_with(MANUAL_HUNK_HEADER), "{text}");
        assert!(
            text.contains("@@ -1,3 +1,3 @@\n keep\n-old\n+new\n"),
            "{text}"
        );
        assert!(
            text.contains("# To remove '-' lines, make them ' ' lines (context)."),
            "{text}"
        );
        assert!(
            text.contains("# To remove '+' lines, delete them."),
            "{text}"
        );
        assert!(
            text.contains("# Lines starting with # will be removed."),
            "{text}"
        );
        assert!(
            text.contains(
                "# If the patch applies cleanly, the edited hunk will immediately be marked for staging."
            ),
            "{text}"
        );
        assert!(
            text.contains("# aborted and the hunk is left unchanged."),
            "{text}"
        );
    }

    #[test]
    fn empty_after_comments_is_abandoned() {
        let original = sample_hunk();
        let text = format_edit_buffer(&original)
            .lines()
            .filter(|line| line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            parse_edited_buffer(&text, &original).unwrap(),
            EditedHunk::Abandoned
        );
    }

    #[test]
    fn plus_to_context_recounts_lines() {
        let original = sample_hunk();
        let edited = "@@ -1,3 +1,3 @@\n keep\n-old\n new\n";
        let EditedHunk::Hunk(hunk) = parse_edited_buffer(edited, &original).unwrap() else {
            panic!("expected hunk");
        };
        assert_eq!(hunk.old_lines, 3);
        assert_eq!(hunk.new_lines, 2);
        assert_eq!(
            hunk.lines,
            vec![
                HunkLine::Context("keep".into()),
                HunkLine::Delete("old".into()),
                HunkLine::Context("new".into()),
            ]
        );
    }

    #[test]
    fn garbage_body_is_invalid() {
        let original = sample_hunk();
        assert!(parse_edited_buffer("not a hunk at all\n", &original).is_err());
    }

    #[test]
    fn deletion_and_mode_are_not_editable() {
        let hunk = sample_hunk();
        let deleted = FileDiff {
            path: "gone".into(),
            header: String::new(),
            old_mode: Some(0o100644),
            new_mode: None,
            added: false,
            deleted: true,
            mode_change: false,
            binary: false,
            hunks: vec![hunk.clone()],
        };
        assert!(!is_editable(&deleted, &hunk));
        let mode = FileDiff {
            path: "mode".into(),
            header: String::new(),
            old_mode: Some(0o100644),
            new_mode: Some(0o100755),
            added: false,
            deleted: false,
            mode_change: true,
            binary: false,
            hunks: vec![Hunk {
                old_start: 0,
                old_lines: 0,
                new_start: 0,
                new_lines: 0,
                header: String::new(),
                lines: Vec::new(),
                no_newline_old: false,
                no_newline_new: false,
                use_decision: HunkUse::Undecided,
            }],
        };
        assert!(!is_editable(&mode, &mode.hunks[0]));
        let regular = FileDiff {
            path: "a".into(),
            header: String::new(),
            old_mode: Some(0o100644),
            new_mode: Some(0o100644),
            added: false,
            deleted: false,
            mode_change: false,
            binary: false,
            hunks: vec![hunk.clone()],
        };
        assert!(is_editable(&regular, &hunk));
    }
}
