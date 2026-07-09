//! CG-01 sub-face compatibility grading guard (`compat_subface_labels`).
//!
//! `COMPATIBILITY.md`'s single `Tier` column is deliberately coarse: a command
//! marked `partial`/`supported` can still hide a broken conflict view, an
//! unparseable `--porcelain` stream, an ignored config default, or a non-Git
//! plumbing output. CG-01 splits every P0/P1-touched command into a fixed
//! enumeration of sub-faces and records every unsupported face with governance.

#[path = "subface_labels/mod.rs"]
mod support;

use std::collections::{BTreeMap, BTreeSet};

use support::{
    doc::{
        COMPATIBILITY_MD, GOVERNANCE_HEADING, GOVERNANCE_MD, SUBFACE_HEADING, assert_table_header,
        read_repo_file, section, table_rows,
    },
    model::{FIXED_SUBFACES, GRADED_COMMANDS, parse_grading_rows},
    plan::{cli_commands, plan_p0_p1_touched_commands, valid_governing_numbers},
};

#[test]
fn subface_grading_matrix_is_well_formed_and_pinned() {
    let fixed: BTreeSet<&str> = FIXED_SUBFACES.into_iter().collect();

    let compat = read_repo_file(COMPATIBILITY_MD);
    assert_table_header(
        section(&compat, SUBFACE_HEADING),
        &[
            "Command",
            "Supported faces",
            "Partial faces",
            "Unsupported faces",
            "Intentionally-different faces",
        ],
    );

    let rows = parse_grading_rows();
    let graded: BTreeSet<String> = rows.iter().map(|r| r.command.clone()).collect();
    let pinned: BTreeSet<String> = GRADED_COMMANDS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        graded.len(),
        rows.len(),
        "{COMPATIBILITY_MD}: duplicate command rows in the sub-face grading table"
    );
    assert_eq!(
        graded,
        pinned,
        "{COMPATIBILITY_MD} sub-face grading command set drifted from the pinned P0/P1 surface \
         (`GRADED_COMMANDS`).\nonly in doc: {:?}\nonly in pin: {:?}",
        graded.difference(&pinned).collect::<Vec<_>>(),
        pinned.difference(&graded).collect::<Vec<_>>(),
    );

    let cli = cli_commands();
    let not_in_cli: Vec<&String> = pinned.iter().filter(|c| !cli.contains(*c)).collect();
    assert!(
        not_in_cli.is_empty(),
        "graded commands absent from src/cli.rs::Commands: {not_in_cli:?}"
    );

    let touched = plan_p0_p1_touched_commands(&cli);
    let ungraded: Vec<&String> = touched.difference(&graded).collect();
    assert!(
        ungraded.is_empty(),
        "plan P0/P1 task scopes name commands missing from the sub-face grading matrix: \
         {ungraded:?}"
    );

    for row in &rows {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (bucket_idx, bucket) in row.buckets.iter().enumerate() {
            for (face, _gov) in bucket {
                assert!(
                    fixed.contains(face.as_str()),
                    "{COMPATIBILITY_MD}: `{}` grades unknown sub-face `{face}` (not in the \
                     fixed enumeration {FIXED_SUBFACES:?})",
                    row.command
                );
                if let Some(prev) = seen.insert(face.clone(), bucket_idx) {
                    assert_eq!(
                        prev, bucket_idx,
                        "{COMPATIBILITY_MD}: `{}` grades `{face}` into two tiers",
                        row.command
                    );
                }
            }
        }
    }

    for row in &rows {
        for (bucket_idx, bucket) in row.buckets.iter().enumerate() {
            if bucket_idx == 2 {
                continue;
            }
            for (face, gov) in bucket {
                assert!(
                    gov.is_none(),
                    "{COMPATIBILITY_MD}: `{}` face `{face}` carries a governing number outside \
                     the unsupported column",
                    row.command
                );
            }
        }
    }
}

#[test]
fn subface_fixed_enumeration_definition_matches_guard() {
    let fixed: BTreeSet<String> = FIXED_SUBFACES.iter().map(|s| s.to_string()).collect();

    let compat = read_repo_file(COMPATIBILITY_MD);
    let sec = section(&compat, SUBFACE_HEADING);
    assert_table_header(sec, &["Sub-face", "What it promises"]);
    let defined: BTreeSet<String> = table_rows(sec, "Sub-face")
        .into_iter()
        .map(|cells| cells[0].trim_matches('`').to_string())
        .collect();
    assert_eq!(
        defined, fixed,
        "{COMPATIBILITY_MD}: the `Fixed sub-face enumeration` table drifted from the guard's set"
    );

    let governance = read_repo_file(GOVERNANCE_MD);
    let gov_sec = section(&governance, GOVERNANCE_HEADING);
    for face in &FIXED_SUBFACES {
        let token = format!("`{face}`");
        assert!(
            gov_sec.contains(&token),
            "{GOVERNANCE_MD}: CG-01 section must name every sub-face; missing {token}"
        );
    }
}

#[test]
fn subface_unsupported_faces_are_registered_with_governing_number() {
    let valid = valid_governing_numbers();
    assert!(
        valid.contains("P0-01") && valid.contains("D5"),
        "sanity: valid governing ids should include known plan/D ids; got {valid:?}"
    );

    let mut from_matrix: BTreeSet<(String, String, String)> = BTreeSet::new();
    for row in parse_grading_rows() {
        for (face, gov) in &row.buckets[2] {
            let gov = gov.clone().unwrap_or_else(|| {
                panic!(
                    "{COMPATIBILITY_MD}: unsupported face `{face}` on `{}` has no governing \
                     number",
                    row.command
                )
            });
            assert!(
                valid.contains(&gov),
                "{COMPATIBILITY_MD}: `{}` unsupported `{face}` cites governing number `{gov}` \
                 that is not a plan task or `D`-decision",
                row.command
            );
            from_matrix.insert((row.command.clone(), face.clone(), gov));
        }
    }

    let governance = read_repo_file(GOVERNANCE_MD);
    let gov_sec = section(&governance, GOVERNANCE_HEADING);
    assert_table_header(gov_sec, &["命令", "unsupported 子面", "治理编号", "说明"]);
    let mut from_registry: BTreeSet<(String, String, String)> = BTreeSet::new();
    for cells in table_rows(gov_sec, "命令") {
        assert!(
            cells.len() >= 3,
            "{GOVERNANCE_MD}: unsupported registry row needs 命令/子面/治理编号: {cells:?}"
        );
        let gov = cells[2].clone();
        assert!(
            valid.contains(&gov),
            "{GOVERNANCE_MD}: registry row for `{}` cites governing number `{gov}` that is not \
             a plan task or `D`-decision",
            cells[0]
        );
        from_registry.insert((cells[0].clone(), cells[1].clone(), gov));
    }

    assert_eq!(
        from_matrix,
        from_registry,
        "unsupported sub-faces must match bidirectionally between {COMPATIBILITY_MD} and \
         {GOVERNANCE_MD}.\nonly in grading matrix: {:?}\nonly in governance registry: {:?}",
        from_matrix.difference(&from_registry).collect::<Vec<_>>(),
        from_registry.difference(&from_matrix).collect::<Vec<_>>(),
    );
}

#[test]
fn subface_sections_cross_link() {
    let compat = read_repo_file(COMPATIBILITY_MD);
    let governance = read_repo_file(GOVERNANCE_MD);

    assert!(
        compat.contains("## Sub-face compatibility grading (P0/P1-touched commands)"),
        "{COMPATIBILITY_MD}: missing the CG-01 sub-face grading heading"
    );
    assert!(
        governance.contains(GOVERNANCE_HEADING),
        "{GOVERNANCE_MD}: missing the CG-01 sub-face grading heading"
    );
    assert!(
        compat.contains("compat_subface_labels"),
        "{COMPATIBILITY_MD}: must name the machine guard `compat_subface_labels`"
    );
    assert!(
        compat.contains("_compatibility.md#子面兼容分级cg-01"),
        "{COMPATIBILITY_MD}: must link to the governance registry anchor"
    );
    assert!(
        governance
            .contains("COMPATIBILITY.md#sub-face-compatibility-grading-p0p1-touched-commands"),
        "{GOVERNANCE_MD}: must link back to the sub-face grading anchor"
    );
}
