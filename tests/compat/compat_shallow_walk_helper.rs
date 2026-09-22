//! Guards ADR-CL-05: history walks in log / rev-list / merge-base / status /
//! blame / describe / shortlog must go through `ShallowSet::parents_for_walk`.
//! Display of recorded parents is allowed when the line (or the next line)
//! carries `SHALLOW-DISPLAY`.

use std::{fs, path::PathBuf};

const WALK_FILES: &[&str] = &[
    "src/command/log.rs",
    "src/command/rev_list.rs",
    "src/command/rev_list_spec.rs",
    "src/command/rev_list_filter.rs",
    "src/command/rev_list_children.rs",
    "src/command/rev_list_cherry.rs",
    "src/internal/merge_base.rs",
    "src/command/status.rs",
    "src/command/blame.rs",
    "src/command/describe.rs",
    "src/command/shortlog.rs",
];

#[test]
fn log_and_rev_list_walks_use_shallow_helper() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for rel in WALK_FILES {
        let path = root.join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read '{}': {error}", path.display()));
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("parent_commit_ids") {
                continue;
            }
            if line.contains("parents_for_walk") || line.contains("SHALLOW-DISPLAY") {
                continue;
            }
            let next = lines.get(index + 1).copied().unwrap_or("");
            if next.contains("SHALLOW-DISPLAY") || next.contains("parents_for_walk") {
                continue;
            }
            offenders.push(format!(
                "{}:{} walks parent_commit_ids without ShallowSet::parents_for_walk",
                rel,
                index + 1
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "new history walks must use internal::shallow::ShallowSet::parents_for_walk \
         (add // SHALLOW-DISPLAY when the site prints recorded parents):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn helper_module_is_the_parser() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/internal/shallow.rs");
    assert!(path.is_file(), "src/internal/shallow.rs must exist");
    let text = fs::read_to_string(&path).expect("read shallow.rs");
    assert!(
        text.contains("parents_for_walk"),
        "ShallowSet must expose parents_for_walk"
    );
}
