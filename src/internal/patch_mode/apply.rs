//! Apply selected hunks to an index blob. Never writes the worktree.

use super::model::{FileDiff, Hunk, HunkLine, HunkUse};

/// How selected hunks are interpreted against the index blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchApplyMode {
    /// `add -p`: apply worktree-vs-index hunks onto the current index blob.
    Stage,
    /// `reset -p` against HEAD: unstage by reversing the cached hunks.
    ResetHead,
    /// `reset -p <tree-ish>`: apply the reverse-diff hunks onto the index.
    ResetNotHead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedIndexBlob {
    /// `None` means the path is removed from the index.
    pub bytes: Option<Vec<u8>>,
    /// Index mode to write (`100644` / `100755` / `120000`). `None` keeps the
    /// caller's current mode.
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchApplyError {
    BinaryNotSelectable,
    HunkDoesNotApply { path: String, detail: String },
}

impl std::fmt::Display for PatchApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryNotSelectable => {
                write!(f, "binary files cannot be selected in patch mode")
            }
            Self::HunkDoesNotApply { path, detail } => {
                write!(f, "hunk does not apply to '{path}': {detail}")
            }
        }
    }
}

impl std::error::Error for PatchApplyError {}

/// Apply every hunk marked [`HunkUse::Use`] (and a selected mode change) to
/// `old_bytes`, which is the current index blob for [`PatchApplyMode::Stage`].
///
/// `ResetHead` / `ResetNotHead` take the same selected hunks of a reverse
/// patch and apply them forward onto `old_bytes`.
pub fn apply_selected_hunks_to_blob(
    old_bytes: &[u8],
    file: &FileDiff,
    mode: PatchApplyMode,
) -> Result<AppliedIndexBlob, PatchApplyError> {
    if file.binary {
        return Err(PatchApplyError::BinaryNotSelectable);
    }

    let use_mode = file.mode_change
        && file
            .hunks
            .iter()
            .all(|hunk| hunk.use_decision != HunkUse::Skip)
        && (file.hunks.is_empty()
            || file
                .hunks
                .iter()
                .any(|hunk| hunk.use_decision == HunkUse::Use));

    let result_mode = match mode {
        PatchApplyMode::ResetHead => file.old_mode.or(file.new_mode),
        PatchApplyMode::Stage | PatchApplyMode::ResetNotHead => file.new_mode.or(file.old_mode),
    };

    if file.mode_change && file.hunks.is_empty() {
        return Ok(AppliedIndexBlob {
            bytes: Some(old_bytes.to_vec()),
            mode: if use_mode { result_mode } else { file.old_mode },
        });
    }

    let remove_from_index = match mode {
        PatchApplyMode::ResetHead => file.added,
        PatchApplyMode::Stage | PatchApplyMode::ResetNotHead => file.deleted,
    } && file
        .hunks
        .iter()
        .any(|hunk| hunk.use_decision == HunkUse::Use);
    if remove_from_index {
        return Ok(AppliedIndexBlob {
            bytes: None,
            mode: None,
        });
    }

    let selected: Vec<Hunk> = file
        .hunks
        .iter()
        .filter(|hunk| hunk.use_decision == HunkUse::Use)
        .map(|hunk| match mode {
            PatchApplyMode::ResetHead => reverse_hunk(hunk),
            PatchApplyMode::Stage | PatchApplyMode::ResetNotHead => hunk.clone(),
        })
        .collect();
    if selected.is_empty() {
        return Ok(AppliedIndexBlob {
            bytes: Some(old_bytes.to_vec()),
            mode: if use_mode { result_mode } else { file.old_mode },
        });
    }

    let (mut lines, ends_with_nl, crlf) = split_file(old_bytes);
    for hunk in &selected {
        apply_one_hunk(&mut lines, hunk).map_err(|detail| PatchApplyError::HunkDoesNotApply {
            path: file.path.clone(),
            detail,
        })?;
    }
    let mut bytes = join_file(&lines, ends_with_nl, crlf, last_no_newline(&file.hunks));
    if file.added && old_bytes.is_empty() {
        bytes = join_file(
            &lines,
            !last_no_newline(&file.hunks),
            crlf,
            last_no_newline(&file.hunks),
        );
    }
    Ok(AppliedIndexBlob {
        bytes: Some(bytes),
        mode: if use_mode || file.mode_change {
            result_mode
        } else {
            file.new_mode.or(file.old_mode)
        },
    })
}

fn reverse_hunk(hunk: &Hunk) -> Hunk {
    Hunk {
        old_start: hunk.new_start,
        old_lines: hunk.new_lines,
        new_start: hunk.old_start,
        new_lines: hunk.old_lines,
        header: crate::utils::diff_format::format_hunk_header(
            hunk.new_start,
            hunk.new_lines,
            hunk.old_start,
            hunk.old_lines,
        ),
        lines: hunk
            .lines
            .iter()
            .map(|line| match line {
                HunkLine::Context(text) => HunkLine::Context(text.clone()),
                HunkLine::Delete(text) => HunkLine::Insert(text.clone()),
                HunkLine::Insert(text) => HunkLine::Delete(text.clone()),
            })
            .collect(),
        no_newline_old: hunk.no_newline_new,
        no_newline_new: hunk.no_newline_old,
        use_decision: hunk.use_decision,
    }
}

fn last_no_newline(hunks: &[Hunk]) -> bool {
    hunks
        .iter()
        .rev()
        .find(|hunk| hunk.use_decision == HunkUse::Use)
        .map(|hunk| hunk.no_newline_new)
        .unwrap_or(false)
}

fn split_file(bytes: &[u8]) -> (Vec<String>, bool, bool) {
    if bytes.is_empty() {
        return (Vec::new(), true, false);
    }
    let crlf = bytes.windows(2).any(|w| w == b"\r\n");
    let text = String::from_utf8_lossy(bytes);
    let ends_with_nl = text.ends_with('\n');
    let lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| {
            let mut line = line.to_string();
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    // Keep `\r` on the line so hunk text (`a\r`) matches.
                }
            }
            line
        })
        .collect();
    if ends_with_nl && text.ends_with('\n') {
        // split_inclusive keeps the last empty only for a trailing extra nl.
        if let Some(last) = lines.last()
            && last.is_empty()
            && text.ends_with("\n\n")
        {
            // keep
        }
    }
    // `split_inclusive` on "a\n" yields ["a\n"] → after pop → ["a"]. Good.
    // On "a\nb" (no final nl) yields ["a\n", "b"] → ["a", "b"]. Good.
    (lines, ends_with_nl, crlf)
}

fn join_file(lines: &[String], ends_with_nl: bool, crlf: bool, force_no_nl: bool) -> Vec<u8> {
    let nl: &str = if crlf { "\r\n" } else { "\n" };
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let body = line.trim_end_matches('\r');
        out.push_str(if crlf { body } else { line.as_str() });
        let last = i + 1 == lines.len();
        if !last {
            out.push_str(if crlf { "\r\n" } else { "\n" });
        } else if !force_no_nl && ends_with_nl {
            out.push_str(nl);
        } else if !force_no_nl && !ends_with_nl {
            // keep no newline
        }
    }
    if lines.is_empty() && ends_with_nl && !force_no_nl {
        // empty file stays empty
    }
    out.into_bytes()
}

fn apply_one_hunk(lines: &mut Vec<String>, hunk: &Hunk) -> Result<(), String> {
    if hunk.old_lines == 0 {
        let insert_at = if hunk.old_start == 0 {
            0
        } else {
            hunk.old_start
        };
        let new_lines = postimage(hunk);
        if insert_at > lines.len() {
            return Err(format!(
                "insert at {} past EOF ({})",
                insert_at,
                lines.len()
            ));
        }
        for (offset, line) in new_lines.into_iter().enumerate() {
            lines.insert(insert_at + offset, line);
        }
        return Ok(());
    }

    let start = hunk.old_start.saturating_sub(1);
    let expected = preimage(hunk);
    if start + expected.len() > lines.len() {
        return Err(format!(
            "hunk -{},{} overruns file ({} lines)",
            hunk.old_start,
            hunk.old_lines,
            lines.len()
        ));
    }
    for (i, want) in expected.iter().enumerate() {
        if normalize_line(&lines[start + i]) != normalize_line(want) {
            return Err(format!(
                "line {} mismatch: expected {want:?}, got {:?}",
                start + i + 1,
                lines[start + i]
            ));
        }
    }
    let new_lines = postimage(hunk);
    lines.splice(start..start + expected.len(), new_lines);
    Ok(())
}

fn preimage(hunk: &Hunk) -> Vec<String> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(text) | HunkLine::Delete(text) => Some(text.clone()),
            HunkLine::Insert(_) => None,
        })
        .collect()
}

fn postimage(hunk: &Hunk) -> Vec<String> {
    hunk.lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(text) | HunkLine::Insert(text) => Some(text.clone()),
            HunkLine::Delete(_) => None,
        })
        .collect()
}

fn normalize_line(line: &str) -> String {
    line.trim_end_matches('\r').to_string()
}

#[cfg(test)]
mod tests {
    use git_internal::internal::object::blob::Blob;

    use super::*;
    use crate::internal::patch_mode::model::{parse_unified_diff, split_hunk};

    fn fixture(path: &str) -> String {
        let root = env!("CARGO_MANIFEST_DIR");
        std::fs::read_to_string(format!("{root}/tests/data/patch-mode/{path}"))
            .unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    fn lines(n: usize) -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=n {
            out.push_str(&format!("line {i}\n"));
        }
        out.into_bytes()
    }

    fn blob_hex(bytes: &[u8]) -> String {
        Blob::from_content_bytes(bytes.to_vec()).id.to_string()
    }

    fn mark(file: &mut FileDiff, decisions: &str) {
        assert_eq!(decisions.len(), file.hunks.len(), "decision length");
        for (hunk, ch) in file.hunks.iter_mut().zip(decisions.chars()) {
            hunk.use_decision = match ch {
                'y' => HunkUse::Use,
                'n' => HunkUse::Skip,
                other => panic!("bad decision {other}"),
            };
        }
    }

    fn index_hash(path: &str, filename: &str) -> String {
        for line in fixture(path).lines() {
            let mut parts = line.split_whitespace();
            let _mode = parts.next();
            let hash = parts.next().expect("hash");
            let _stage = parts.next();
            let name = parts.next().unwrap_or("");
            if name == filename {
                return hash.to_string();
            }
        }
        panic!("{path} has no {filename}");
    }

    #[test]
    fn two_hunks_use_skip_matches_git_apply_cached() {
        let mut files = parse_unified_diff(&fixture("two-hunks/diff-files.patch")).unwrap();
        let old = lines(20);
        assert_eq!(
            blob_hex(&old),
            index_hash("two-hunks/apply-nn.index", "f.txt")
        );

        mark(&mut files[0], "yy");
        let applied = apply_selected_hunks_to_blob(&old, &files[0], PatchApplyMode::Stage).unwrap();
        let bytes = applied.bytes.expect("kept");
        assert_eq!(
            blob_hex(&bytes),
            index_hash("two-hunks/apply-yy.index", "f.txt")
        );

        let mut files = parse_unified_diff(&fixture("two-hunks/diff-files.patch")).unwrap();
        mark(&mut files[0], "yn");
        let applied = apply_selected_hunks_to_blob(&old, &files[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(&applied.bytes.unwrap()),
            index_hash("two-hunks/apply-yn.index", "f.txt")
        );

        let mut files = parse_unified_diff(&fixture("two-hunks/diff-files.patch")).unwrap();
        mark(&mut files[0], "ny");
        let applied = apply_selected_hunks_to_blob(&old, &files[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(&applied.bytes.unwrap()),
            index_hash("two-hunks/apply-ny.index", "f.txt")
        );

        let mut files = parse_unified_diff(&fixture("two-hunks/diff-files.patch")).unwrap();
        mark(&mut files[0], "nn");
        let applied = apply_selected_hunks_to_blob(&old, &files[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(&applied.bytes.unwrap()),
            index_hash("two-hunks/apply-nn.index", "f.txt")
        );
    }

    #[test]
    fn split3_selected_islands_match_git() {
        let files = parse_unified_diff(&fixture("split3/diff-files.patch")).unwrap();
        let pieces = split_hunk(&files[0].hunks[0]).expect("split");
        let old = lines(10);

        let run = |answers: &str, fixture_name: &str| {
            let mut file = files[0].clone();
            file.hunks = pieces.clone();
            mark(&mut file, answers);
            let applied = apply_selected_hunks_to_blob(&old, &file, PatchApplyMode::Stage).unwrap();
            assert_eq!(
                blob_hex(&applied.bytes.unwrap()),
                index_hash(fixture_name, "s.txt"),
                "{answers}"
            );
        };
        run("yny", "split3/apply-syny.index");
        run("nyn", "split3/apply-snyn.index");
        run("yyy", "split3/apply-syyy.index");
        run("nnn", "split3/apply-snnn.index");
    }

    #[test]
    fn three_files_tc0042_matches_git() {
        let files = parse_unified_diff(&fixture("three-files/diff-files.patch")).unwrap();
        let old = lines(8);
        // s y n / s n y / s y y
        let answers = ["yn", "ny", "yy"];
        let names = ["a.txt", "b.txt", "c.txt"];
        for (file, (ans, name)) in files.iter().zip(answers.iter().zip(names)) {
            let pieces = split_hunk(&file.hunks[0]).expect("split");
            let mut staged = file.clone();
            staged.hunks = pieces;
            mark(&mut staged, ans);
            let applied =
                apply_selected_hunks_to_blob(&old, &staged, PatchApplyMode::Stage).unwrap();
            assert_eq!(
                blob_hex(&applied.bytes.unwrap()),
                index_hash("three-files/apply-tc0042.index", name),
                "{name} {ans}"
            );
        }
    }

    #[test]
    fn deletion_use_removes_blob() {
        let mut files = parse_unified_diff(&fixture("deletion/diff-files.patch")).unwrap();
        mark(&mut files[0], "y");
        let old = lines(3);
        let applied = apply_selected_hunks_to_blob(&old, &files[0], PatchApplyMode::Stage).unwrap();
        assert!(applied.bytes.is_none());

        let mut files = parse_unified_diff(&fixture("deletion/diff-files.patch")).unwrap();
        mark(&mut files[0], "n");
        let applied = apply_selected_hunks_to_blob(&old, &files[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(applied.bytes.as_deref(), Some(old.as_slice()));
    }

    #[test]
    fn crlf_and_noeol_and_empty_match_git() {
        let mut crlf = parse_unified_diff(&fixture("crlf/diff-files.patch")).unwrap();
        mark(&mut crlf[0], "y");
        let old = b"a\r\nb\r\nc\r\n";
        let applied = apply_selected_hunks_to_blob(old, &crlf[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(&applied.bytes.unwrap()),
            index_hash("crlf/apply-y.index", "crlf.txt")
        );
        mark(&mut crlf[0], "n");
        let applied = apply_selected_hunks_to_blob(old, &crlf[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(&applied.bytes.unwrap()),
            index_hash("crlf/apply-n.index", "crlf.txt")
        );

        let mut noeol = parse_unified_diff(&fixture("noeol/diff-files.patch")).unwrap();
        mark(&mut noeol[0], "y");
        let old = b"a\nb";
        let applied = apply_selected_hunks_to_blob(old, &noeol[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(&applied.bytes.unwrap()),
            index_hash("noeol/apply-y.index", "noeol.txt")
        );

        let mut empty = parse_unified_diff(&fixture("empty/diff-files.patch")).unwrap();
        mark(&mut empty[0], "y");
        let applied = apply_selected_hunks_to_blob(b"", &empty[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(
            blob_hex(applied.bytes.as_deref().unwrap_or(&[])),
            index_hash("empty/apply-y.index", "e.txt")
        );
    }

    #[test]
    fn mode_only_flips_mode_without_rewriting_bytes() {
        let files = parse_unified_diff(&fixture("mode/diff-files.patch")).unwrap();
        let old = b"#!/bin/sh\n";
        let applied = apply_selected_hunks_to_blob(old, &files[0], PatchApplyMode::Stage).unwrap();
        assert_eq!(applied.bytes.as_deref(), Some(old.as_slice()));
        assert_eq!(applied.mode, Some(0o100755));
        assert_eq!(blob_hex(old), index_hash("mode/apply-y.index", "m.sh"));
    }

    #[test]
    fn reset_head_reverses_cached_hunks_onto_index() {
        let mut files = parse_unified_diff(&fixture("reset-head/diff-cached.patch")).unwrap();
        mark(&mut files[0], "n");
        mark(&mut files[1], "y");
        let index = b"line 1 x\nline 2\nline 3\nline 4\n";
        let bar =
            apply_selected_hunks_to_blob(index, &files[0], PatchApplyMode::ResetHead).unwrap();
        assert_eq!(
            blob_hex(bar.bytes.as_deref().unwrap()),
            index_hash("reset-head/apply-ny.index", "bar")
        );
        let foo =
            apply_selected_hunks_to_blob(index, &files[1], PatchApplyMode::ResetHead).unwrap();
        assert_eq!(
            blob_hex(foo.bytes.as_deref().unwrap()),
            index_hash("reset-head/apply-ny.index", "foo")
        );
    }

    #[test]
    fn reset_nothead_applies_reverse_diff_forward() {
        let mut files = parse_unified_diff(&fixture("reset-nothead/diff-reverse.patch")).unwrap();
        mark(&mut files[0], "y");
        let index = b"line 1\nline 2 x\nline 3\nline 4\n";
        let applied =
            apply_selected_hunks_to_blob(index, &files[0], PatchApplyMode::ResetNotHead).unwrap();
        assert_eq!(
            blob_hex(applied.bytes.as_deref().unwrap()),
            index_hash("reset-nothead/apply-y.index", "bar")
        );
    }
}
