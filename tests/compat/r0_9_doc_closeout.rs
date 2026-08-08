//! plan-20260714 R0-9 documentation close-out guard.
//!
//! R0-9 is a docs-only card whose acceptance is "the warning code/source
//! table, the io_blocked JSON schema, and the renameUntracked /
//! renameLimit / quotePath configuration keys are all visible in the
//! user docs, EN and zh stay parallel, and COMPATIBILITY + CHANGELOG
//! carry the same facts". The generic examples-section guard cannot see
//! any of that, so this target pins it directly.

use std::{fs, path::Path};

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// Parse the `| `code` | `source` | …` rows of a documented warning table
/// into a code→source map. Only rows whose first two cells are both
/// backticked identifiers count, so surrounding prose and other tables on
/// the page are ignored.
fn documented_warning_table(doc: &str) -> std::collections::HashMap<String, String> {
    let cell = |raw: &str| -> Option<String> {
        let trimmed = raw.trim();
        trimmed
            .strip_prefix('`')
            .and_then(|rest| rest.strip_suffix('`'))
            .filter(|value| {
                !value.is_empty()
                    && value
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            })
            .map(str::to_string)
    };
    doc.lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim_matches('|').split('|').collect();
            match (
                cells.first().and_then(|c| cell(c)),
                cells.get(1).and_then(|c| cell(c)),
            ) {
                (Some(code), Some(source)) => Some((code, source)),
                _ => None,
            }
        })
        .collect()
}

/// The published warning table matches the implementation ROW BY ROW: every
/// frozen code appears on both pages with the exact source the code is
/// emitted under. Both sides are derived — the codes from
/// `StatusWarningCode::ALL`, the sources from `StatusWarningCode::source()`
/// — so a new code, or a code moved to a different subsystem, fails here
/// instead of shipping a table that quietly lies about which subsystem to
/// investigate.
#[test]
fn status_docs_carry_the_warning_code_and_source_table() {
    let en = read("docs/commands/status.md");
    let zh = read("docs/commands/zh-CN/status.md");
    let en_table = documented_warning_table(&en);
    let zh_table = documented_warning_table(&zh);

    for code in libra::command::status::StatusWarningCode::ALL {
        let code_name = serde_json::to_value(code)
            .expect("serialize code")
            .as_str()
            .expect("code is a string")
            .to_string();
        let source_name = serde_json::to_value(code.source())
            .expect("serialize source")
            .as_str()
            .expect("source is a string")
            .to_string();
        for (label, table) in [("EN", &en_table), ("zh", &zh_table)] {
            let documented = table.get(&code_name).unwrap_or_else(|| {
                panic!("{label} status doc has no warning-table row for `{code_name}`")
            });
            assert_eq!(
                documented, &source_name,
                "{label} status doc maps `{code_name}` to `{documented}`, \
                 but it is emitted with source `{source_name}`"
            );
        }
    }

    // No stale rows either: a code removed from the schema must lose its row.
    let implemented: std::collections::HashSet<String> =
        libra::command::status::StatusWarningCode::ALL
            .iter()
            .map(|code| {
                serde_json::to_value(code)
                    .expect("serialize code")
                    .as_str()
                    .expect("code is a string")
                    .to_string()
            })
            .collect();
    for (label, table) in [("EN", &en_table), ("zh", &zh_table)] {
        for documented in table.keys() {
            assert!(
                implemented.contains(documented),
                "{label} status doc documents `{documented}`, which the schema no longer defines"
            );
        }
    }

    // The frozen SOURCE enum is larger than the set currently in use
    // (`config` is reserved), so it is asserted separately on both pages.
    for source in [
        "config",
        "probe",
        "rename_detect",
        "worktree",
        "metadata",
        "cache",
    ] {
        assert!(
            en.contains(&format!("`{source}`")),
            "EN status doc must name the `{source}` source"
        );
        assert!(
            zh.contains(&format!("`{source}`")),
            "zh status doc must name the `{source}` source"
        );
    }
}

/// Extract the serialized `io_blocked[].reason` strings straight from the
/// implementation so a newly added `IoBlockedReason` variant cannot ship
/// without a matching doc update. Reads the `io_blocked_reason_and_code`
/// match arms rather than a hand-copied list.
fn implemented_io_blocked_reasons() -> Vec<String> {
    let source = read("src/command/status.rs");
    let body = source
        .split_once("fn io_blocked_reason_and_code(")
        .and_then(|(_, rest)| rest.split_once("\n}\n"))
        .map(|(body, _)| body.to_string())
        .expect("src/command/status.rs must define io_blocked_reason_and_code");
    let mut reasons = Vec::new();
    for arm in body.split("IoBlockedReason::").skip(1) {
        // The first string literal after the variant is the wire value.
        if let Some((_, rest)) = arm.split_once('"')
            && let Some((value, _)) = rest.split_once('"')
        {
            reasons.push(value.to_string());
        }
    }
    reasons.sort();
    reasons.dedup();
    assert!(
        reasons.len() >= 3,
        "expected at least the three known io_blocked reasons, parsed {reasons:?}"
    );
    reasons
}

/// The io_blocked partial contract (schema fields + the complete `reason`
/// enum + completeness flags + exit arbitration) is documented on both
/// pages.
#[test]
fn status_docs_carry_the_io_blocked_schema() {
    let en = read("docs/commands/status.md");
    let zh = read("docs/commands/zh-CN/status.md");
    // Scope every field assertion to the io_blocked SECTION: a token that
    // happens to appear elsewhere on the page does not document the schema.
    let en_section = section_after(&en, "### The io_blocked partial contract");
    let zh_section = section_after(&zh, "### io_blocked 部分结果契约");
    for (label, section) in [("EN", en_section), ("zh", zh_section)] {
        // The complete serialized shape (see `build_status_json`'s
        // io_blocked loop): the entry, its two path forms, the staged
        // component, the reason, and the rename pair with its score.
        for field in [
            "io_blocked",
            "display",
            "raw_base64",
            "staged",
            "reason",
            "rename",
            "score",
        ] {
            assert!(
                section.contains(field),
                "{label} io_blocked section must document the `{field}` field"
            );
        }
        assert!(
            section.contains("{from, to, score}") || section.contains("{from,to,score}"),
            "{label} io_blocked section must type `rename` as {{from, to, score}}"
        );
        assert!(
            section.contains("null"),
            "{label} io_blocked section must say which fields can be null"
        );
    }
    // The completeness flags and the text-mode failure code are stated on
    // the page (they bracket the section rather than sit inside it).
    for needle in [
        "base_scan_complete",
        "rename_detection_complete",
        "LBR-IO-001",
    ] {
        assert!(en.contains(needle), "EN status doc must document {needle}");
        assert!(zh.contains(needle), "zh status doc must document {needle}");
    }
    assert!(
        en.contains("`complete`") && zh.contains("`complete`"),
        "both pages must document the combined `complete` flag"
    );
    // Every wire value the implementation can serialize must be named IN
    // THE SCHEMA SECTION — a documented enum that omits a live variant
    // makes JSON consumers reject valid output.
    for reason in implemented_io_blocked_reasons() {
        let quoted = format!("`\"{reason}\"`");
        assert!(
            en_section.contains(&quoted),
            "EN io_blocked section must document the reason {quoted}"
        );
        assert!(
            zh_section.contains(&quoted),
            "zh io_blocked section must document the reason {quoted}"
        );
    }
    // Nested rename pairs serialize as {from, to} objects (see
    // `renamed_to_json`), never as "old -> new" strings.
    for (label, doc) in [("EN", &en), ("zh", &zh)] {
        assert!(
            doc.contains("`{from, to}`"),
            "{label} status doc must type staged.renamed/unstaged.renamed as {{from, to}} objects"
        );
        assert!(
            !doc.contains("`\"old -> new\"`"),
            "{label} status doc still types the nested renamed lists as \"old -> new\" strings"
        );
    }
    // The exit priority is the arbitration contract R0-8 implements.
    assert!(
        en.contains("fatal") && en.contains("on-warning") && en.contains("dirty"),
        "EN status doc must state the exit arbitration priority"
    );
    assert!(
        zh.contains("on-warning") && zh.contains("dirty"),
        "zh status doc must state the exit arbitration priority"
    );
}

/// The warning DELIVERY MATRIX (§B.5): text modes print to stderr, `--json`
/// carries warnings in the envelope and leaves stderr clean, and repository
/// preflight advisories follow the same split rather than being an
/// exception. Prose that says otherwise sends integrators looking for a
/// stderr channel that no longer exists.
#[test]
fn status_docs_state_the_warning_delivery_matrix() {
    let en = read("docs/commands/status.md");
    let zh = read("docs/commands/zh-CN/status.md");

    // No page may claim a stderr-only bypass exists.
    for (label, doc) in [("EN", &en), ("zh", &zh)] {
        assert!(
            !doc.contains("printed on stderr in every mode")
                && !doc.contains("在**所有**模式下都打印到 stderr"),
            "{label} status doc still claims preflight warnings print on stderr              in every mode; JSON buffers them into the envelope instead"
        );
    }
    // And both must state the JSON side positively.
    assert!(
        en.contains("`data.warnings[]`") && en.contains("stderr stays clean"),
        "EN status doc must state that --json delivers warnings in the envelope          and leaves stderr clean"
    );
    assert!(
        zh.contains("`data.warnings[]`") && zh.contains("stderr 保持干净"),
        "zh status doc must state the same"
    );
    // The exit priority is the other half of the matrix.
    for (label, doc) in [("EN", &en), ("zh", &zh)] {
        assert!(
            doc.contains("9") && doc.contains("on-warning"),
            "{label} status doc must state the exit-9 arbitration rule"
        );
    }
}

/// The three R0 configuration keys are documented on both pages and in
/// the compatibility matrix.
#[test]
fn status_docs_and_compat_carry_the_rename_configuration_keys() {
    let en = read("docs/commands/status.md");
    let zh = read("docs/commands/zh-CN/status.md");
    let compat = read("COMPATIBILITY.md");
    for key in [
        "status.renameUntracked",
        "status.renameLimit",
        "core.quotePath",
    ] {
        assert!(en.contains(key), "EN status doc must document {key}");
        assert!(zh.contains(key), "zh status doc must document {key}");
        assert!(compat.contains(key), "COMPATIBILITY must document {key}");
    }

    // COMPATIBILITY carries its own copy of the frozen warning-source enum;
    // a stale copy there is exactly as misleading as a stale command doc,
    // because the matrix is what integrators read first.
    for source in [
        "config",
        "probe",
        "rename_detect",
        "worktree",
        "metadata",
        "cache",
    ] {
        assert!(
            compat.contains(&format!("`{source}`")),
            "COMPATIBILITY must list the `{source}` warning source"
        );
    }
    let sources_claim = section_after(&compat, "one frozen `{code,message,source}` schema");
    for source in ["config", "probe"] {
        assert!(
            sources_claim.contains(source),
            "COMPATIBILITY's frozen-schema sentence itself must name `{source}`, \
             not merely mention it elsewhere in the file"
        );
    }
}

/// The diff-side degradation semantics R0-9 requires are in the diff
/// docs AND released through the CHANGELOG.
#[test]
fn diff_docs_and_changelog_carry_the_degradation_semantics() {
    let en = read("docs/commands/diff.md");
    let zh = read("docs/commands/zh-CN/diff.md");
    let changelog = read("CHANGELOG.md");

    // Both budgets must be documented INSIDE the budgets section, not
    // merely mentioned somewhere on the page, and each must state what
    // survives its degradation — that is the whole R0-9 claim.
    let en_budgets = section_after(&en, "### Rename detection budgets");
    let zh_budgets = section_after(&zh, "### 重命名检测预算");
    for (label, section) in [("EN", en_budgets), ("zh", zh_budgets)] {
        for needle in ["diff.renameLimit", "diff.renameComparisonBudget"] {
            assert!(
                section.contains(needle),
                "{label} diff doc must document {needle} in the budgets section"
            );
        }
        assert!(
            section.contains("basename"),
            "{label} budgets section must say unique-basename pairs survive the renameLimit gate"
        );
    }
    // The comparison budget discards the WHOLE exhaustive pass (unlike the
    // limit, which only skips it) — the two are easy to conflate.
    assert!(
        en_budgets.contains("discarded") || en_budgets.contains("discards"),
        "EN budgets section must say the exhaustive pass is DISCARDED when the \
         comparison budget is exhausted: {en_budgets}"
    );
    assert!(
        zh_budgets.contains("丢弃"),
        "zh budgets section must say the exhaustive pass is discarded: {zh_budgets}"
    );
    // And the two degradations must NOT be conflated in the other direction:
    // the limit always keeps every scored unique-basename pair, while the
    // comparison budget only keeps pairs ALREADY scored when it is spent —
    // docs that promise all basename pairs survive the budget describe a
    // behavior the engine does not have.
    assert!(
        en_budgets.contains("already scored") || en_budgets.contains("already-paired"),
        "EN budgets section must say the comparison budget only keeps pairs \
         already scored: {en_budgets}"
    );
    assert!(
        en_budgets.contains("always keeps every scored unique-basename pair"),
        "EN budgets section must distinguish the limit from the budget on \
         unique-basename survival: {en_budgets}"
    );
    assert!(
        zh_budgets.contains("已评分"),
        "zh budgets section must say the comparison budget only keeps pairs \
         already scored: {zh_budgets}"
    );
    assert!(
        zh_budgets.contains("总是保留"),
        "zh budgets section must distinguish the limit from the budget on \
         unique-basename survival: {zh_budgets}"
    );
    // Scope the CHANGELOG assertions to the R0-9 release note itself — the
    // file mentions renameLimit/unique-basename in unrelated releases, so a
    // whole-file `contains` would stay green even if THIS note regressed.
    let note_start = changelog
        .find("Rename-degradation semantics are now spelled out")
        .expect("CHANGELOG must keep the R0-9 rename-degradation release note");
    let note_tail = &changelog[note_start..];
    let note_end = ["\n- **", "\n### ", "\n## "]
        .iter()
        .filter_map(|stop| note_tail.find(stop))
        .min()
        .unwrap_or(note_tail.len());
    // Flatten hard line wraps so phrase pins are independent of reflow.
    let note = note_tail[..note_end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for needle in [
        "diff.renameLimit",
        "diff.renameComparisonBudget",
        "rename_limit_product_skipped",
        "similarity_budget_exceeded",
        // limit degradation: exact + unique-basename survive, exhaustive skipped
        "exact renames AND unique-basename renames are still reported",
        "only the exhaustive inexact stage is skipped",
        // budget degradation: wholesale discard, only already-scored pairs kept
        "discarded wholesale",
        "only pairs already scored",
        "exact plus the unique-basename pairs",
    ] {
        assert!(
            note.contains(needle),
            "the R0-9 CHANGELOG release note must keep the claim {needle:?}: {note}"
        );
    }

    // The degradation semantics must match the engine: the unique-basename
    // stage runs BEFORE the renameLimit gate, so exceeding the limit keeps
    // exact *and* unique-basename pairs. Docs that say "exact only" send
    // readers hunting for a bug that is not there.
    let en_section = section_after(&en, "diff.renameLimit");
    assert!(
        en_section.contains("unique-basename"),
        "EN diff doc must state that unique-basename pairs survive the renameLimit gate"
    );
    let zh_section = section_after(&zh, "diff.renameLimit");
    assert!(
        zh_section.contains("basename"),
        "zh diff doc must state that unique-basename pairs survive the renameLimit gate"
    );

    // Same claim, third location: the compatibility matrix. Each claim is
    // asserted inside ITS OWN table row — the rows are single (very long)
    // lines, so a substring search across the file would let the `diff`
    // row satisfy an assertion about the `status` row.
    let compat = read("COMPATIBILITY.md");
    let diff_row = compat_row(&compat, "`diff.renameLimit` (per-side cap");
    assert!(
        diff_row.contains("unique-basename"),
        "COMPATIBILITY's diff row must state that unique-basename pairs \
         survive the renameLimit gate, not just exact renames"
    );
    let status_row = compat_row(&compat, "the per-side cap comes from `status.renameLimit`");
    assert!(
        status_row.contains("unique-basename"),
        "COMPATIBILITY's status row must state the same degradation semantics"
    );
    assert_ne!(
        diff_row, status_row,
        "the two claims must live in DIFFERENT rows; if they resolve to the \
         same line this guard is not actually checking both surfaces"
    );
}

/// The Meaning cell (third column) of the warning-table row for `code`.
/// Asserting on the whole row would be vacuous for some needles — e.g.
/// "skipped" occurs inside the code `rename_limit_product_skipped` itself —
/// so the semantic pins below must see ONLY the Meaning cell.
fn warning_meaning(doc: &str, code: &str) -> String {
    let marker = format!("| `{code}` |");
    let row = doc
        .lines()
        .find(|line| line.starts_with(marker.as_str()))
        .unwrap_or_else(|| panic!("status doc must keep a warning-table row for {code}"));
    let cells: Vec<&str> = row.trim_matches('|').split('|').collect();
    assert_eq!(
        cells.len(),
        3,
        "warning-table row for {code} must have exactly code/source/meaning cells: {row}"
    );
    cells[2].trim().to_string()
}

/// The Meaning cells of the two degradation warnings carry the survivor
/// semantics (what a degraded pass KEEPS vs discards). The code→source
/// coverage test above cannot see those cells, so pin them per row here —
/// in both languages, since either translation could regress alone.
#[test]
fn status_docs_pin_survivor_semantics_in_warning_meaning_cells() {
    let en = read("docs/commands/status.md");
    let zh = read("docs/commands/zh-CN/status.md");

    // Budget exhaustion discards ONLY the exhaustive pass; exact and
    // already-scored unique-basename pairs survive.
    let en_cell = warning_meaning(&en, "similarity_budget_exceeded");
    for needle in [
        "exhaustive",
        "discarded",
        "exact",
        "already-scored unique-basename",
        "kept",
    ] {
        assert!(
            en_cell.contains(needle),
            "EN similarity_budget_exceeded Meaning cell must keep {needle:?}: {en_cell}"
        );
    }
    let zh_cell = warning_meaning(&zh, "similarity_budget_exceeded");
    for needle in ["穷举", "丢弃", "exact", "已评分的 unique-basename", "保留"] {
        assert!(
            zh_cell.contains(needle),
            "zh similarity_budget_exceeded Meaning cell must keep {needle:?}: {zh_cell}"
        );
    }

    // Limit excess merely SKIPS the exhaustive stage (nothing scored is lost).
    let en_cell = warning_meaning(&en, "rename_limit_product_skipped");
    for needle in ["exhaustive", "skipped"] {
        assert!(
            en_cell.contains(needle),
            "EN rename_limit_product_skipped Meaning cell must keep {needle:?}: {en_cell}"
        );
    }
    let zh_cell = warning_meaning(&zh, "rename_limit_product_skipped");
    for needle in ["exhaustive", "跳过"] {
        assert!(
            zh_cell.contains(needle),
            "zh rename_limit_product_skipped Meaning cell must keep {needle:?}: {zh_cell}"
        );
    }
}

/// The single `COMPATIBILITY.md` table row containing `needle`. Rows are one
/// line each, so this is the correct granularity for a per-command claim.
fn compat_row<'a>(compat: &'a str, needle: &str) -> &'a str {
    compat
        .lines()
        .find(|line| line.starts_with('|') && line.contains(needle))
        .unwrap_or_else(|| panic!("COMPATIBILITY.md must have a table row containing {needle}"))
}

/// The prose block introduced by `needle`, bounded by the next blank line
/// that precedes a heading or fence — enough context to assert a claim is
/// made *about that key*, not merely present somewhere in the file.
fn section_after<'a>(doc: &'a str, needle: &str) -> &'a str {
    let start = doc
        .find(needle)
        .unwrap_or_else(|| panic!("doc must mention {needle}"));
    let rest = &doc[start..];
    let end = rest
        .find("\n### ")
        .or_else(|| rest.find("\n```"))
        .unwrap_or(rest.len());
    &rest[..end]
}
