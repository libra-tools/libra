use crate::support::doc::{
    COMPATIBILITY_MD, SUBFACE_HEADING, is_empty_cell, read_repo_file, section, table_rows,
};

pub const FIXED_SUBFACES: [&str; 5] = [
    "common-user-flow",
    "porcelain-machine",
    "conflict-aware",
    "config-aware",
    "plumbing-compatible",
];

pub const GRADED_COMMANDS: [&str; 44] = [
    "add",
    "archive",
    "branch",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "checkout",
    "cherry-pick",
    "clean",
    "clone",
    "cloud",
    "commit",
    "commit-tree",
    "diff",
    "fast-export",
    "fast-import",
    "fetch",
    "for-each-ref",
    "grep",
    "hooks",
    "init",
    "lfs",
    "log",
    "ls-files",
    "ls-remote",
    "media",
    "merge",
    "pull",
    "push",
    "rebase",
    "remote",
    "reset",
    "restore",
    "rev-parse",
    "revert",
    "rm",
    "show",
    "show-ref",
    "shortlog",
    "status",
    "switch",
    "tag",
    "write-tree",
];

pub struct GradingRow {
    pub command: String,
    pub buckets: [Vec<(String, Option<String>)>; 4],
}

fn split_face_and_gov(token: &str) -> (String, Option<String>) {
    let token = token.trim();
    if let Some(open) = token.find('(') {
        let face = token[..open].trim().to_string();
        let gov = token[open + 1..].trim_end_matches(')').trim().to_string();
        (face, Some(gov))
    } else {
        (token.to_string(), None)
    }
}

fn parse_cell_faces(cell: &str) -> Vec<(String, Option<String>)> {
    if is_empty_cell(cell) {
        return Vec::new();
    }
    cell.split(',').map(split_face_and_gov).collect()
}

pub fn parse_grading_rows() -> Vec<GradingRow> {
    let compat = read_repo_file(COMPATIBILITY_MD);
    let sec = section(&compat, SUBFACE_HEADING).to_string();
    let rows = table_rows(&sec, "Command");
    assert!(
        !rows.is_empty(),
        "{COMPATIBILITY_MD}: sub-face grading table has no rows"
    );
    rows.into_iter()
        .map(|cells| {
            assert_eq!(
                cells.len(),
                5,
                "{COMPATIBILITY_MD}: grading row must have 5 columns: {cells:?}"
            );
            GradingRow {
                command: cells[0].clone(),
                buckets: [
                    parse_cell_faces(&cells[1]),
                    parse_cell_faces(&cells[2]),
                    parse_cell_faces(&cells[3]),
                    parse_cell_faces(&cells[4]),
                ],
            }
        })
        .collect()
}
