//! File / hunk model, unified-diff parse, split, and reassemble.

use std::fmt;

/// Whether a hunk will be applied when the session writes the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkUse {
    Undecided,
    Skip,
    Use,
}

/// One line inside a hunk, without the leading diff marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Delete(String),
    Insert(String),
}

impl HunkLine {
    pub(crate) fn marker(&self) -> char {
        match self {
            Self::Context(_) => ' ',
            Self::Delete(_) => '-',
            Self::Insert(_) => '+',
        }
    }

    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Context(text) | Self::Delete(text) | Self::Insert(text) => text,
        }
    }

    fn is_change(&self) -> bool {
        matches!(self, Self::Delete(_) | Self::Insert(_))
    }
}

/// One `@@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// Raw `@@ -a,b +c,d @@` line, including any function-context suffix.
    pub header: String,
    pub lines: Vec<HunkLine>,
    /// `\\ No newline at end of file` after a deleted/context preimage line.
    pub no_newline_old: bool,
    /// `\\ No newline at end of file` after an inserted/context postimage line.
    pub no_newline_new: bool,
    pub use_decision: HunkUse,
}

impl Hunk {
    /// Git `splittable_into`: number of change islands separated by context.
    pub fn splittable_into(&self) -> usize {
        change_islands(&self.lines).len().max(1)
    }

    pub fn is_splittable(&self) -> bool {
        self.splittable_into() > 1
    }
}

/// One `diff --git` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// Every header line before the first hunk (`diff --git` … `+++`), raw.
    pub header: String,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub added: bool,
    pub deleted: bool,
    pub mode_change: bool,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    MissingGitHeader,
    BadHunkHeader(String),
    BadHunkLine(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty patch"),
            Self::MissingGitHeader => write!(f, "patch is missing a diff --git header"),
            Self::BadHunkHeader(line) => write!(f, "unrecognized hunk header: {line}"),
            Self::BadHunkLine(line) => write!(f, "unrecognized hunk line: {line}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse one or more `diff --git` file sections. File order is the patch order
/// (Git emits path byte order for `add -p` inputs; callers sort if needed).
pub fn parse_unified_diff(text: &str) -> Result<Vec<FileDiff>, ParseError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut current: Option<FileBuilder> = None;
    let mut pending_no_nl_for_insert = false;

    for raw in split_keep_lines(text) {
        let line = raw.trim_end_matches('\n');
        if line.starts_with("diff --git ") {
            if let Some(builder) = current.take() {
                files.push(builder.finish());
            }
            current = Some(FileBuilder::new(line));
            pending_no_nl_for_insert = false;
            continue;
        }
        let Some(builder) = current.as_mut() else {
            if line.is_empty() {
                continue;
            }
            return Err(ParseError::MissingGitHeader);
        };
        if line.starts_with("@@ ") {
            let hunk = parse_hunk_header(line)?;
            builder.hunks.push(hunk);
            pending_no_nl_for_insert = false;
            continue;
        }
        if line == "\\ No newline at end of file" {
            if let Some(hunk) = builder.hunks.last_mut() {
                if pending_no_nl_for_insert {
                    hunk.no_newline_new = true;
                } else {
                    hunk.no_newline_old = true;
                }
            }
            continue;
        }
        if builder.hunks.is_empty() {
            builder.push_header_line(line);
            continue;
        }
        if line.starts_with("diff --git ") {
            unreachable!("handled above");
        }
        let Some(hunk) = builder.hunks.last_mut() else {
            continue;
        };
        match line.as_bytes().first() {
            Some(b' ') => {
                hunk.lines.push(HunkLine::Context(line[1..].to_string()));
                pending_no_nl_for_insert = false;
            }
            Some(b'-') => {
                hunk.lines.push(HunkLine::Delete(line[1..].to_string()));
                pending_no_nl_for_insert = false;
            }
            Some(b'+') => {
                hunk.lines.push(HunkLine::Insert(line[1..].to_string()));
                pending_no_nl_for_insert = true;
            }
            _ if line.is_empty() => {
                // A completely empty body line is an empty context line
                // whose leading space was stripped by a sloppy producer.
                hunk.lines.push(HunkLine::Context(String::new()));
                pending_no_nl_for_insert = false;
            }
            _ => return Err(ParseError::BadHunkLine(line.to_string())),
        }
    }
    if let Some(builder) = current {
        files.push(builder.finish());
    }
    Ok(files)
}

/// Split `hunk` into one hunk per change island. Returns `None` when Git
/// would print `Sorry, cannot split this hunk`.
pub fn split_hunk(hunk: &Hunk) -> Option<Vec<Hunk>> {
    let islands = change_islands(&hunk.lines);
    if islands.len() <= 1 {
        return None;
    }
    let mut pieces = Vec::with_capacity(islands.len());
    let mut old_cursor = hunk.old_start;
    let mut new_cursor = hunk.new_start;
    let mut consumed_old = 0usize;
    let mut consumed_new = 0usize;
    for (island_old_before, island_new_before, range) in islands_with_offsets(&hunk.lines) {
        while consumed_old < island_old_before {
            consumed_old += 1;
            old_cursor += 1;
        }
        while consumed_new < island_new_before {
            consumed_new += 1;
            new_cursor += 1;
        }
        let start = leading_context_start(&hunk.lines, range.start);
        let end = trailing_context_end(&hunk.lines, range.end);
        let slice = &hunk.lines[start..end];
        let (old_count, new_count) = hunk_line_counts(slice);
        // Context lines before the island belong to this piece but have
        // already been walked by earlier pieces' trailing context — rewind
        // the displayed start so the header matches Git (overlapping context).
        let lead_old = old_lines_in(&hunk.lines[start..range.start]);
        let lead_new = new_lines_in(&hunk.lines[start..range.start]);
        let old_start = old_cursor.saturating_sub(lead_old);
        let new_start = new_cursor.saturating_sub(lead_new);
        let header = crate::utils::diff_format::format_hunk_header(
            old_start, old_count, new_start, new_count,
        );
        let island_old = old_lines_in(&hunk.lines[range.clone()]);
        let island_new = new_lines_in(&hunk.lines[range.clone()]);
        consumed_old += island_old;
        consumed_new += island_new;
        old_cursor += island_old;
        new_cursor += island_new;
        pieces.push(Hunk {
            old_start,
            old_lines: old_count,
            new_start,
            new_lines: new_count,
            header,
            lines: slice.to_vec(),
            no_newline_old: hunk.no_newline_old && end == hunk.lines.len(),
            no_newline_new: hunk.no_newline_new && end == hunk.lines.len(),
            use_decision: HunkUse::Undecided,
        });
    }
    Some(pieces)
}

/// Rebuild a file patch from `file.header` plus every hunk marked [`HunkUse::Use`].
/// Original `@@` headers are kept so `git apply` can locate them on the preimage.
pub fn reassemble_file_patch(file: &FileDiff) -> String {
    let mut out = file.header.clone();
    if !out.ends_with('\n') && !file.hunks.is_empty() {
        out.push('\n');
    }
    for hunk in file
        .hunks
        .iter()
        .filter(|hunk| hunk.use_decision == HunkUse::Use)
    {
        out.push_str(&hunk.header);
        if !hunk.header.ends_with('\n') {
            out.push('\n');
        }
        for line in &hunk.lines {
            out.push(line.marker());
            out.push_str(line.text());
            out.push('\n');
        }
        if hunk.no_newline_old {
            out.push_str("\\ No newline at end of file\n");
        }
        if hunk.no_newline_new {
            out.push_str("\\ No newline at end of file\n");
        }
    }
    out
}

struct FileBuilder {
    path: String,
    header: String,
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    added: bool,
    deleted: bool,
    binary: bool,
    hunks: Vec<Hunk>,
}

impl FileBuilder {
    fn new(git_line: &str) -> Self {
        let path = path_from_git_header(git_line);
        let mut header = git_line.to_string();
        header.push('\n');
        Self {
            path,
            header,
            old_mode: None,
            new_mode: None,
            added: false,
            deleted: false,
            binary: false,
            hunks: Vec::new(),
        }
    }

    fn push_header_line(&mut self, line: &str) {
        self.header.push_str(line);
        self.header.push('\n');
        if let Some(mode) = line.strip_prefix("old mode ") {
            self.old_mode = parse_mode(mode);
        } else if let Some(mode) = line.strip_prefix("new mode ") {
            self.new_mode = parse_mode(mode);
        } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
            self.deleted = true;
            self.old_mode = parse_mode(mode);
        } else if let Some(mode) = line.strip_prefix("new file mode ") {
            self.added = true;
            self.new_mode = parse_mode(mode);
        } else if let Some(rest) = line.strip_prefix("index ") {
            if let Some(mode) = rest.rsplit(' ').next()
                && rest.contains('.')
                && self.old_mode.is_none()
                && self.new_mode.is_none()
            {
                self.old_mode = parse_mode(mode);
                self.new_mode = parse_mode(mode);
            }
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            self.binary = true;
        } else if line == "+++ /dev/null" {
            self.deleted = true;
        } else if line == "--- /dev/null" {
            self.added = true;
        }
    }

    fn finish(self) -> FileDiff {
        let mode_change = match (self.old_mode, self.new_mode) {
            (Some(old), Some(new)) => old != new && !self.added && !self.deleted,
            _ => false,
        };
        let added = self.added
            || self
                .hunks
                .iter()
                .any(|hunk| hunk.old_start == 0 && hunk.old_lines == 0);
        FileDiff {
            path: self.path,
            header: self.header,
            old_mode: self.old_mode,
            new_mode: self.new_mode,
            added,
            deleted: self.deleted,
            mode_change,
            binary: self.binary,
            hunks: self.hunks,
        }
    }
}

fn path_from_git_header(line: &str) -> String {
    // `diff --git a/<path> b/<path>` (add -p) or the reverse
    // `diff --git b/<path> a/<path>` (`reset -p` Apply). Prefer the second
    // side after stripping the `a/` / `b/` prefix so both orders yield the
    // destination path. Renames still resolve to the right-hand name.
    let rest = line.strip_prefix("diff --git ").unwrap_or(line);
    let (left, right) = rest.split_once(' ').unwrap_or((rest, ""));
    let right = right
        .strip_prefix("a/")
        .or_else(|| right.strip_prefix("b/"))
        .unwrap_or(right);
    if !right.is_empty() && right != "/dev/null" {
        return right.to_string();
    }
    left.strip_prefix("a/")
        .or_else(|| left.strip_prefix("b/"))
        .unwrap_or(left)
        .to_string()
}

fn parse_mode(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim(), 8).ok()
}

fn parse_hunk_header(line: &str) -> Result<Hunk, ParseError> {
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
    let (old_start, old_lines) = parse_hunk_span(old, '-')?;
    let (new_start, new_lines) = parse_hunk_span(new, '+')?;
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

fn parse_hunk_span(spec: &str, sign: char) -> Result<(usize, usize), ParseError> {
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

fn split_keep_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            lines.push(text[start..=i].to_string());
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push(text[start..].to_string());
    }
    lines
}

fn change_islands(lines: &[HunkLine]) -> Vec<std::ops::Range<usize>> {
    let mut islands = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].is_change() {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].is_change() {
            i += 1;
        }
        islands.push(start..i);
    }
    islands
}

fn islands_with_offsets(lines: &[HunkLine]) -> Vec<(usize, usize, std::ops::Range<usize>)> {
    let mut old = 0usize;
    let mut new = 0usize;
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].is_change() {
            old += 1;
            new += 1;
            i += 1;
            continue;
        }
        let start = i;
        let old_before = old;
        let new_before = new;
        while i < lines.len() && lines[i].is_change() {
            match &lines[i] {
                HunkLine::Delete(_) => old += 1,
                HunkLine::Insert(_) => new += 1,
                HunkLine::Context(_) => {}
            }
            i += 1;
        }
        out.push((old_before, new_before, start..i));
    }
    out
}

fn leading_context_start(lines: &[HunkLine], island_start: usize) -> usize {
    let mut start = island_start;
    while start > 0 && matches!(lines[start - 1], HunkLine::Context(_)) {
        start -= 1;
    }
    start
}

fn trailing_context_end(lines: &[HunkLine], island_end: usize) -> usize {
    let mut end = island_end;
    while end < lines.len() && matches!(lines[end], HunkLine::Context(_)) {
        end += 1;
    }
    end
}

fn hunk_line_counts(lines: &[HunkLine]) -> (usize, usize) {
    (old_lines_in(lines), new_lines_in(lines))
}

fn old_lines_in(lines: &[HunkLine]) -> usize {
    lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_) | HunkLine::Delete(_)))
        .count()
}

fn new_lines_in(lines: &[HunkLine]) -> usize {
    lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_) | HunkLine::Insert(_)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str) -> String {
        let root = env!("CARGO_MANIFEST_DIR");
        std::fs::read_to_string(format!("{root}/tests/data/patch-mode/{path}"))
            .unwrap_or_else(|e| panic!("read {path}: {e}"))
    }

    #[test]
    fn parse_two_hunks_preserves_git_headers() {
        let patch = fixture("two-hunks/diff-files.patch");
        let files = parse_unified_diff(&patch).expect("parse");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "f.txt");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].header, "@@ -1,5 +1,5 @@");
        assert_eq!(files[0].hunks[1].header, "@@ -15,6 +15,6 @@ line 14");
        assert!(files[0].header.starts_with("diff --git a/f.txt b/f.txt\n"));
        assert!(files[0].header.contains("index c4352f8..e41e5cb 100644\n"));
    }

    #[test]
    fn parse_reverse_git_header_keeps_path() {
        let files = parse_unified_diff(&fixture("reset-nothead/diff-reverse.patch")).unwrap();
        assert_eq!(files[0].path, "bar");
        assert!(files[0].header.starts_with("diff --git b/bar a/bar\n"));
    }

    #[test]
    fn parse_file_order_matches_three_files_fixture() {
        let files = parse_unified_diff(&fixture("three-files/diff-files.patch")).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "b.txt", "c.txt"]);
        assert!(
            files
                .iter()
                .all(|f| f.hunks.first().is_some_and(|h| h.splittable_into() == 2))
        );
    }

    #[test]
    fn split3_first_hunk_is_three_islands() {
        let files = parse_unified_diff(&fixture("split3/diff-files.patch")).unwrap();
        assert_eq!(files[0].hunks[0].splittable_into(), 3);
        assert_eq!(
            fixture("split3/split-first-hunk.txt").trim(),
            "Split into 3 hunks."
        );
        let pieces = split_hunk(&files[0].hunks[0]).expect("splittable");
        assert_eq!(pieces.len(), 3);
    }

    #[test]
    fn three_files_each_hunk_splits_in_two() {
        let files = parse_unified_diff(&fixture("three-files/diff-files.patch")).unwrap();
        for file in &files {
            assert_eq!(file.hunks[0].splittable_into(), 2);
        }
        assert_eq!(
            fixture("three-files/split-first-hunk.txt").trim(),
            "Split into 2 hunks."
        );
    }

    #[test]
    fn classify_deletion_mode_binary() {
        let deletion = parse_unified_diff(&fixture("deletion/diff-files.patch")).unwrap();
        assert!(deletion[0].deleted);
        assert!(!deletion[0].binary);

        let mode = parse_unified_diff(&fixture("mode/diff-files.patch")).unwrap();
        assert!(mode[0].mode_change);
        assert_eq!(mode[0].old_mode, Some(0o100644));
        assert_eq!(mode[0].new_mode, Some(0o100755));
        assert!(mode[0].hunks.is_empty());

        let binary = parse_unified_diff(&fixture("binary/diff-files.patch")).unwrap();
        assert_eq!(binary.len(), 2);
        assert!(binary[0].binary);
        assert!(!binary[1].binary);
    }

    #[test]
    fn parse_crlf_noeol_empty() {
        let crlf = parse_unified_diff(&fixture("crlf/diff-files.patch")).unwrap();
        assert_eq!(crlf[0].hunks[0].lines[0], HunkLine::Context("a\r".into()));

        let noeol = parse_unified_diff(&fixture("noeol/diff-files.patch")).unwrap();
        assert!(noeol[0].hunks[0].no_newline_old);
        assert!(noeol[0].hunks[0].no_newline_new);

        let empty = parse_unified_diff(&fixture("empty/diff-files.patch")).unwrap();
        assert!(empty[0].added);
        assert_eq!(empty[0].hunks[0].old_start, 0);
        assert_eq!(empty[0].hunks[0].old_lines, 0);
    }
}
