use std::{fs, path::PathBuf};

pub const COMPATIBILITY_MD: &str = "COMPATIBILITY.md";
pub const GOVERNANCE_MD: &str = "docs/development/commands/_compatibility.md";
pub const SUBFACE_HEADING: &str = "## Sub-face compatibility grading";
pub const GOVERNANCE_HEADING: &str = "## 子面兼容分级（CG-01）";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn read_repo_file(path: &str) -> String {
    let full = repo_root().join(path);
    fs::read_to_string(&full).unwrap_or_else(|error| panic!("read {}: {error}", full.display()))
}

pub fn section<'a>(body: &'a str, heading: &str) -> &'a str {
    let start = body
        .find(heading)
        .unwrap_or_else(|| panic!("missing heading `{heading}`"));
    let rest = &body[start..];
    let end = rest[heading.len()..]
        .find("\n## ")
        .map(|i| i + heading.len())
        .unwrap_or(rest.len());
    &rest[..end]
}

fn is_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
}

pub fn table_rows(section: &str, header_first_cell: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut in_table = false;
    for raw in section.lines() {
        let line = raw.trim();
        if !line.starts_with('|') {
            if in_table {
                break;
            }
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if is_separator(&cells) {
            continue;
        }
        let first = cells.first().cloned().unwrap_or_default();
        if !in_table {
            if first == header_first_cell {
                in_table = true;
            }
            continue;
        }
        rows.push(cells);
    }
    rows
}

pub fn is_empty_cell(cell: &str) -> bool {
    cell.is_empty() || cell.chars().all(|c| matches!(c, '—' | '–' | '-'))
}

pub fn backtick_tokens(line: &str) -> Vec<String> {
    line.split('`')
        .enumerate()
        .filter(|(i, _part)| i % 2 == 1)
        .map(|(_i, part)| part.to_string())
        .collect()
}

pub fn assert_table_header(section: &str, expected: &[&str]) {
    for raw in section.lines() {
        let line = raw.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if is_separator(&cells) {
            continue;
        }
        if cells.first().map(String::as_str) == Some(expected[0]) {
            let want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                cells, want,
                "table header drifted from the expected column order/labels"
            );
            return;
        }
    }
    panic!("no header row starting with `{}` found", expected[0]);
}
