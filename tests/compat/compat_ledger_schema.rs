//! plan-20260729 CT2-01 — the structural contract of the compatibility
//! evidence ledger, and the mechanism that enforces it.
//!
//! The ledger lives at `tests/compat-ledger/<family>/<stem>.toml`: one TOML
//! per upstream source file, carrying one `[[scenario]]` table per migrated
//! upstream test. ADR-CT-03 in
//! [`docs/development/plan/plan-20260729.md`](../../docs/development/plan/plan-20260729.md)
//! is the single normative definition of the field set; this guard is its
//! executable form and must not invent fields of its own.
//!
//! Why a guard from day one: the ledger's whole purpose is to be believable
//! evidence about Git compatibility. A row that self-reports its command's
//! compatibility tier, or that says `declined` without naming a decision, or
//! that quietly loses `owner`/`review_date`, is worse than no row at all — it
//! looks like evidence. So the tier is RECOMPUTED from `COMPATIBILITY.md`
//! rather than trusted, every field is required to be present and non-blank,
//! and the field set is closed: an unknown key is an error, not a comment.
//!
//! `_example/` holds one valid file (the format's worked example) and
//! `_invalid/` holds one file per rejection this guard promises to make.
//! Both are skipped when walking the real ledger — a directory whose name
//! starts with `_` is fixture space, not evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

// ─────────────────────────────────────────────────────────────────────────────
// The schema, as ADR-CT-03 defines it.
// ─────────────────────────────────────────────────────────────────────────────

const CATEGORIES: &[&str] = &["direct", "adapted", "declined", "blocked"];
const COMMAND_STATUSES: &[&str] = &["supported", "partial", "intentionally-different", "absent"];
const SURFACE_STATUSES: &[&str] = &[
    "git-compatible",
    "intentionally-different",
    "absent",
    "deferred",
];

/// Every field a `[[scenario]]` must carry. `libra_tests` is filled in later
/// (CT3-02), `decision_id`/`blocked_by` are category-conditional, so those
/// three are optional here and checked separately.
const REQUIRED_FIELDS: &[&str] = &[
    "id",
    "category",
    "command_status",
    "surface_compatibility",
    "surface_evidence",
    "reason",
    "owner",
    "review_date",
    "upstream_revision",
    "upstream_file",
    "libra_command",
    "libra_surface",
];

const CONDITIONAL_FIELDS: &[&str] = &["decision_id", "blocked_by", "libra_tests"];

/// ADR-CT-03: `reason` is free text of at most 200 Unicode scalar values.
const REASON_MAX_SCALARS: usize = 200;

/// Absolute paths that would leak a developer's machine into the evidence
/// (ER-11). A ledger row is published material; it names repository-relative
/// paths or nothing.
const PRIVATE_PATH_MARKERS: &[&str] = &["/Users/", "/home/", "/root/", "C:\\Users\\"];

// ─────────────────────────────────────────────────────────────────────────────
// Locations
// ─────────────────────────────────────────────────────────────────────────────

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ledger_root() -> PathBuf {
    repo_root().join("tests/compat-ledger")
}

fn example_file() -> PathBuf {
    ledger_root().join("_example/ledger_example.toml")
}

fn invalid_file(stem: &str) -> PathBuf {
    ledger_root().join(format!("_invalid/{stem}.toml"))
}

// ─────────────────────────────────────────────────────────────────────────────
// A validated scenario row
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Scenario {
    id: String,
    category: String,
    surface_evidence: String,
    libra_tests: Vec<String>,
}

/// Validate one ledger file's text. `Ok` carries its scenarios in file order;
/// `Err` carries a message naming what was wrong, so a rejection fixture can
/// assert it was rejected for its own reason and not by accident.
fn validate_ledger_text(display_path: &str, text: &str) -> Result<Vec<Scenario>, String> {
    validate_ledger_text_with_lock(display_path, text, None)
}

/// The same validation, with the surface lock consulted when one exists.
///
/// The lock is what turns `surface_compatibility` from a claim into a derived
/// value: CT2-02's `SURFACES.gen` produces it from the authoritative registry,
/// and a row may not contradict it. Until a family ships its
/// `SURFACES.lock` (CT3-03) there is nothing to check against, so `None`
/// means "surface adjudication not available here" rather than "anything
/// goes" — a row still cannot claim `direct` over a surface it declares to be
/// anything but `git-compatible` (CT2-01's declared-value half).
fn validate_ledger_text_with_lock(
    display_path: &str,
    text: &str,
    lock: Option<&SurfaceLock>,
) -> Result<Vec<Scenario>, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|error| format!("{display_path}: not valid TOML: {error}"))?;
    let table = doc
        .as_table()
        .ok_or_else(|| format!("{display_path}: top level must be a table"))?;

    for key in table.keys() {
        if key != "scenario" {
            return Err(format!(
                "{display_path}: unknown top-level key '{key}'; a ledger file holds only \
                 [[scenario]] tables"
            ));
        }
    }

    let rows = table
        .get("scenario")
        .ok_or_else(|| format!("{display_path}: no [[scenario]] table"))?
        .as_array()
        .ok_or_else(|| format!("{display_path}: 'scenario' must be an array of tables"))?;
    if rows.is_empty() {
        return Err(format!("{display_path}: [[scenario]] array is empty"));
    }

    let compat = compatibility_tiers();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut scenarios = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let row = row
            .as_table()
            .ok_or_else(|| format!("{display_path}: [[scenario]] #{index} is not a table"))?;
        let where_ = format!("{display_path} scenario #{index}");

        // Closed field set: an unknown key is a typo or a private extension,
        // and either way the guard would stop covering it.
        for key in row.keys() {
            if !REQUIRED_FIELDS.contains(&key.as_str())
                && !CONDITIONAL_FIELDS.contains(&key.as_str())
            {
                return Err(format!("{where_}: unknown field '{key}'"));
            }
        }

        let mut values: BTreeMap<&str, String> = BTreeMap::new();
        for field in REQUIRED_FIELDS {
            let value = row
                .get(*field)
                .ok_or_else(|| format!("{where_}: missing field '{field}'"))?;
            let value = value
                .as_str()
                .ok_or_else(|| format!("{where_}: field '{field}' must be a string"))?;
            if value.trim().is_empty() {
                return Err(format!("{where_}: field '{field}' is blank"));
            }
            values.insert(field, value.to_string());
        }

        for (field, value) in &values {
            for marker in PRIVATE_PATH_MARKERS {
                if value.contains(marker) {
                    return Err(format!(
                        "{where_}: field '{field}' contains the absolute private path \
                         marker '{marker}'"
                    ));
                }
            }
        }

        let id = values["id"].clone();
        let (stem, slug) = id
            .split_once("::")
            .ok_or_else(|| format!("{where_}: id '{id}' is not '<stem>::<slug>'"))?;
        if stem.is_empty() || slug.is_empty() {
            return Err(format!("{where_}: id '{id}' has an empty half"));
        }
        if !seen_ids.insert(id.clone()) {
            return Err(format!("{where_}: duplicate scenario id '{id}'"));
        }

        let category = values["category"].clone();
        if !CATEGORIES.contains(&category.as_str()) {
            return Err(format!(
                "{where_}: category '{category}' is not in the closed set"
            ));
        }
        let command_status = values["command_status"].clone();
        if !COMMAND_STATUSES.contains(&command_status.as_str()) {
            return Err(format!(
                "{where_}: command_status '{command_status}' is not in the closed set"
            ));
        }
        let surface_compatibility = values["surface_compatibility"].clone();
        if !SURFACE_STATUSES.contains(&surface_compatibility.as_str()) {
            return Err(format!(
                "{where_}: surface_compatibility '{surface_compatibility}' is not in the \
                 closed set"
            ));
        }

        if !is_iso_date(&values["review_date"]) {
            return Err(format!(
                "{where_}: review_date '{}' is not an ISO YYYY-MM-DD date",
                values["review_date"]
            ));
        }
        if values["reason"].chars().count() > REASON_MAX_SCALARS {
            return Err(format!(
                "{where_}: reason is {} scalar values, the limit is {REASON_MAX_SCALARS}",
                values["reason"].chars().count()
            ));
        }
        let revision = &values["upstream_revision"];
        if revision.len() != 40 || !revision.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "{where_}: upstream_revision '{revision}' is not a 40-hex pinned grit SHA"
            ));
        }

        // The tier is RECOMPUTED, never trusted. `absent` means the command is
        // not in `COMPATIBILITY.md`'s top-level table at all.
        let libra_command = values["libra_command"].clone();
        let derived = compat
            .get(libra_command.as_str())
            .cloned()
            .unwrap_or_else(|| "absent".to_string());
        if derived != command_status {
            return Err(format!(
                "{where_}: command_status '{command_status}' disagrees with the tier \
                 '{derived}' derived from COMPATIBILITY.md for '{libra_command}'"
            ));
        }
        if matches!(derived.as_str(), "intentionally-different" | "absent")
            && category != "declined"
        {
            return Err(format!(
                "{where_}: '{libra_command}' is {derived}, so the only admissible category \
                 is 'declined', not '{category}'"
            ));
        }

        // ADR-CT-03's `direct` threshold. The command half is recomputed
        // above; the surface half is checked here as declared and will be
        // recomputed from SURFACES.lock by CT2-02.
        if category == "direct" {
            if !matches!(command_status.as_str(), "supported" | "partial") {
                return Err(format!(
                    "{where_}: category 'direct' requires command_status supported|partial, \
                     found '{command_status}'"
                ));
            }
            if surface_compatibility != "git-compatible" {
                return Err(format!(
                    "{where_}: category 'direct' requires surface_compatibility \
                     'git-compatible', found '{surface_compatibility}'"
                ));
            }
        }

        let libra_surface = values["libra_surface"].clone();
        if let Some(lock) = lock {
            let (key, locked) = lock
                .resolve(&libra_command, &libra_surface)
                .ok_or_else(|| {
                    format!(
                        "{where_}: surface '{libra_surface}' of '{libra_command}' is not in \
                     SURFACES.lock, so nothing adjudicates it"
                    )
                })?;
            // For a `direct` row the lock's verdict is the binding one, and the
            // refusal should say so rather than blaming the declared value.
            if category == "direct" && locked != "git-compatible" {
                return Err(format!(
                    "{where_}: category 'direct' requires the lock to record \
                     '{libra_surface}' as git-compatible, but the lock says '{locked}' \
                     (matched key '{key}')"
                ));
            }
            if locked != surface_compatibility {
                return Err(format!(
                    "{where_}: surface_compatibility '{surface_compatibility}' disagrees \
                     with the lock's '{locked}' for '{libra_surface}' (matched key '{key}')"
                ));
            }
        }

        match category.as_str() {
            "declined" => {
                let decision_id = row
                    .get("decision_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("{where_}: category 'declined' requires decision_id"))?;
                if !decision_exists(decision_id) {
                    return Err(format!(
                        "{where_}: decision_id '{decision_id}' does not resolve to a \
                         '### {decision_id}' heading in _compatibility.md"
                    ));
                }
            }
            "blocked" => {
                let blocked_by = row
                    .get("blocked_by")
                    .ok_or_else(|| format!("{where_}: category 'blocked' requires blocked_by"))?;
                let items = blocked_by
                    .as_array()
                    .ok_or_else(|| format!("{where_}: blocked_by must be an array"))?;
                if items.is_empty()
                    || items
                        .iter()
                        .any(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
                {
                    return Err(format!(
                        "{where_}: blocked_by must be a non-empty array of non-blank strings"
                    ));
                }
            }
            _ => {}
        }

        let surface_evidence = values["surface_evidence"].clone();
        check_surface_evidence(&where_, &surface_evidence, &libra_command, &libra_surface)?;

        let libra_tests = match row.get("libra_tests") {
            None => Vec::new(),
            Some(value) => {
                let items = value
                    .as_array()
                    .ok_or_else(|| format!("{where_}: libra_tests must be an array"))?;
                let mut names = Vec::new();
                for item in items {
                    let name = item
                        .as_str()
                        .ok_or_else(|| format!("{where_}: libra_tests entries must be strings"))?;
                    if name.trim().is_empty() {
                        return Err(format!("{where_}: libra_tests contains a blank entry"));
                    }
                    names.push(name.to_string());
                }
                names
            }
        };

        scenarios.push(Scenario {
            id,
            category,
            surface_evidence,
            libra_tests,
        });
    }

    Ok(scenarios)
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !value
        .char_indices()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = value[0..4].parse().unwrap_or(0);
    let month: u32 = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..10].parse().unwrap_or(0);
    // Shape alone would accept 2026-02-30. A review date that never happened
    // is not a review date.
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// The `Command -> Tier` map of `COMPATIBILITY.md`'s top-level table, located
/// by its heading so a second table elsewhere in the file cannot be mistaken
/// for it.
fn compatibility_tiers() -> BTreeMap<String, String> {
    let text = fs::read_to_string(repo_root().join("COMPATIBILITY.md"))
        .expect("COMPATIBILITY.md must be readable");
    let mut tiers = BTreeMap::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("## Top-level commands") {
            inside = true;
            continue;
        }
        if inside && line.starts_with("## ") {
            break;
        }
        if !inside || !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 3 || cells[0] == "Command" || cells[0].starts_with("---") {
            continue;
        }
        tiers.insert(cells[0].to_string(), cells[1].to_string());
    }
    assert!(
        !tiers.is_empty(),
        "no rows parsed out of COMPATIBILITY.md's top-level command table"
    );
    tiers
}

/// Whether `_compatibility.md` carries a `### <id>` heading for this decision.
fn decision_exists(decision_id: &str) -> bool {
    let text = fs::read_to_string(repo_root().join("docs/development/commands/_compatibility.md"))
        .expect("_compatibility.md must be readable");
    text.lines()
        .any(|line| heading_names_decision(line, decision_id))
}

/// Whether a `### …` heading names exactly this decision. A bare
/// `starts_with` would let `D1` match `### D10`, which is how a dangling
/// decision id sneaks past as a valid one.
fn heading_names_decision(line: &str, decision_id: &str) -> bool {
    let Some(rest) = line.strip_prefix("### ") else {
        return false;
    };
    let Some(tail) = rest.strip_prefix(decision_id) else {
        return false;
    };
    tail.chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
}

/// `surface_evidence` must resolve to real text, and that text must mention
/// both the command and the surface. Citing a page that does not actually
/// discuss the flag is the failure mode this closes: it looks like a citation
/// and proves nothing.
fn check_surface_evidence(
    where_: &str,
    evidence: &str,
    command: &str,
    surface: &str,
) -> Result<(), String> {
    let body = resolve_evidence(where_, evidence)?;
    for needle in [command, surface] {
        if !body.contains(needle) {
            return Err(format!(
                "{where_}: surface_evidence '{evidence}' does not mention '{needle}'"
            ));
        }
    }
    Ok(())
}

fn resolve_evidence(where_: &str, evidence: &str) -> Result<String, String> {
    // Form 1: COMPATIBILITY.md:<line>
    if let Some(rest) = evidence.strip_prefix("COMPATIBILITY.md:") {
        let line_no: usize = rest
            .parse()
            .map_err(|_| format!("{where_}: '{evidence}' has a non-numeric line number"))?;
        let text = read_repo_file(where_, Path::new("COMPATIBILITY.md"))?;
        return text
            .lines()
            .nth(line_no.checked_sub(1).ok_or_else(|| {
                format!("{where_}: '{evidence}' uses line 0; line numbers are 1-based")
            })?)
            .map(str::to_string)
            .ok_or_else(|| format!("{where_}: '{evidence}' points past the end of the file"));
    }

    // Form 2: _compatibility.md#D<n> — the heading carries the decision id
    // followed by its title, so it is matched by decision id at a token
    // boundary rather than by slug.
    if let Some(anchor) = evidence.strip_prefix("_compatibility.md#") {
        let text = read_repo_file(
            where_,
            Path::new("docs/development/commands/_compatibility.md"),
        )?;
        return decision_section_text(&text, anchor)
            .ok_or_else(|| format!("{where_}: '{evidence}' names no such decision section"));
    }

    // Form 3: docs/commands/<cmd>.md#<section>
    if let Some((path, anchor)) = evidence.split_once('#')
        && path.starts_with("docs/commands/")
        && path.ends_with(".md")
    {
        let text = read_repo_file(where_, Path::new(path))?;
        return section_text(&text, anchor)
            .ok_or_else(|| format!("{where_}: '{evidence}' names no such section"));
    }

    Err(format!(
        "{where_}: surface_evidence '{evidence}' is not one of the three admissible forms"
    ))
}

fn read_repo_file(where_: &str, relative: &Path) -> Result<String, String> {
    fs::read_to_string(repo_root().join(relative))
        .map_err(|error| format!("{where_}: cannot read {}: {error}", relative.display()))
}

/// The text of the section whose heading slug is EXACTLY `anchor`, up to the
/// next heading at the same or a shallower level. Prefix matching is
/// deliberately not offered: `#options` must not silently bind to
/// `## Option Details`, or a citation could point at a section that never
/// mentions the flag it claims to document.
fn section_text(text: &str, anchor: &str) -> Option<String> {
    collect_section(text, |title| slugify(title) == slugify(anchor))
}

/// The section of `_compatibility.md` whose heading names decision `anchor`.
fn decision_section_text(text: &str, anchor: &str) -> Option<String> {
    collect_section(text, |title| {
        heading_names_decision(&format!("### {title}"), anchor)
    })
}

fn collect_section(text: &str, matches: impl Fn(&str) -> bool) -> Option<String> {
    let mut depth = 0usize;
    let mut body: Option<Vec<&str>> = None;
    for line in text.lines() {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let is_heading = hashes > 0 && line.as_bytes().get(hashes) == Some(&b' ');
        if is_heading {
            let title = line[hashes + 1..].trim();
            if body.is_some() && hashes <= depth {
                break;
            }
            if body.is_none() && matches(title) {
                depth = hashes;
                body = Some(vec![line]);
                continue;
            }
        }
        if let Some(collected) = body.as_mut() {
            collected.push(line);
        }
    }
    body.map(|lines| lines.join("\n"))
}

fn slugify(title: &str) -> String {
    title
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == ' ' || c == '-' || c == '_' {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Walking the real ledger
// ─────────────────────────────────────────────────────────────────────────────

/// One loaded ledger file: its repository-relative path (what a downstream
/// consumer must be able to open), its stem, and its scenarios.
#[derive(Debug, Clone)]
struct LedgerFile {
    relative: String,
    stem: String,
    scenarios: Vec<Scenario>,
}

/// Every real ledger file, sorted by path. Directories whose name starts with
/// `_` are fixture space and are skipped.
fn load_real_ledger() -> Vec<LedgerFile> {
    let root = ledger_root();
    let mut files: Vec<PathBuf> = Vec::new();
    if root.is_dir() {
        for family in read_dir_sorted(&root) {
            let name = family
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if !family.is_dir() || name.starts_with('_') {
                continue;
            }
            for file in read_dir_sorted(&family) {
                if file.extension().is_some_and(|ext| ext == "toml") {
                    files.push(file);
                }
            }
        }
    }

    let mut loaded = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for file in files {
        // A family's committed `SURFACES.lock` adjudicates its rows. None
        // exists yet (CT3-03 delivers the first one); when it lands it enters
        // this path with no further wiring.
        let lock = file
            .parent()
            .map(|dir| dir.join("SURFACES.lock"))
            .filter(|path| path.is_file())
            .map(|path| {
                let text = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                SurfaceLock::parse(&text)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            });
        let display = file
            .strip_prefix(repo_root())
            .unwrap_or(&file)
            .display()
            .to_string();
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {display}: {e}"));
        let scenarios = validate_ledger_text_with_lock(&display, &text, lock.as_ref())
            .unwrap_or_else(|error| panic!("ledger row rejected: {error}"));
        for scenario in &scenarios {
            if let Some(previous) = seen.insert(scenario.id.clone(), display.clone()) {
                panic!(
                    "duplicate scenario id '{}' in {display} and {previous}",
                    scenario.id
                );
            }
        }
        let stem = file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        loaded.push(LedgerFile {
            relative: display,
            stem,
            scenarios,
        });
    }
    loaded
}

fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    entries
}

/// The example, loaded through the same validator the real tree uses.
fn load_example() -> Vec<LedgerFile> {
    let path = example_file();
    let text = fs::read_to_string(&path).expect("the ledger example must exist");
    let scenarios = validate_ledger_text("_example/ledger_example.toml", &text)
        .expect("the ledger example must be valid");
    vec![LedgerFile {
        relative: "tests/compat-ledger/_example/ledger_example.toml".to_string(),
        stem: "ledger_example".to_string(),
        scenarios,
    }]
}

// ─────────────────────────────────────────────────────────────────────────────
// Dumps consumed by later cards (CT3-02 / CT3-03 / CT3-04)
// ─────────────────────────────────────────────────────────────────────────────

fn dump_scenario_ids(tree: &[LedgerFile]) -> Vec<String> {
    tree.iter()
        .flat_map(|file| {
            let stem = file.stem.clone();
            file.scenarios
                .iter()
                .map(move |row| format!("SCENARIO_ID {stem}\t{}", row.id))
        })
        .collect()
}

/// `DIRECT_ID <scenario_id>\t<ledger file path>`. The path is the real
/// repository-relative path of the file the row lives in — a consumer must be
/// able to open it, and `<family>/` is part of that path.
fn dump_direct_ids(tree: &[LedgerFile]) -> Vec<String> {
    tree.iter()
        .flat_map(|file| {
            let relative = file.relative.clone();
            file.scenarios
                .iter()
                .filter(|row| row.category == "direct")
                .map(move |row| format!("DIRECT_ID {}\t{relative}", row.id))
        })
        .collect()
}

fn dump_libra_tests(tree: &[LedgerFile]) -> Vec<String> {
    tree.iter()
        .flat_map(|file| file.scenarios.iter().flat_map(|row| row.libra_tests.iter()))
        .map(|name| format!("LIBRA_TEST {name}"))
        .collect()
}

fn dump_surface_evidence(tree: &[LedgerFile]) -> Vec<String> {
    tree.iter()
        .flat_map(|file| {
            file.scenarios
                .iter()
                .map(|row| format!("EVIDENCE {}\t{}", row.id, row.surface_evidence))
        })
        .collect()
}

fn dump_scenario_tests(tree: &[LedgerFile]) -> Vec<String> {
    tree.iter()
        .flat_map(|file| {
            file.scenarios.iter().flat_map(|row| {
                row.libra_tests
                    .iter()
                    .map(move |name| format!("SCENARIO_TEST {}\t{name}", row.id))
            })
        })
        .collect()
}

/// Print a dump for the consumers that run this target with `--nocapture`.
fn emit(lines: &[String]) {
    for line in lines {
        println!("{line}");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn ledger_example_is_valid() {
    let path = example_file();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let scenarios = validate_ledger_text("_example/ledger_example.toml", &text)
        .unwrap_or_else(|error| panic!("the worked example must validate: {error}"));
    assert!(
        !scenarios.is_empty(),
        "the worked example must carry at least one scenario"
    );
}

/// Every rejection fixture: it must be refused, and the refusal must name the
/// thing the fixture is about. A fixture that is rejected for an unrelated
/// reason (a typo elsewhere) would otherwise look like coverage.
fn assert_rejected(stem: &str, expected_fragment: &str) {
    let path = invalid_file(stem);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    match validate_ledger_text(&format!("_invalid/{stem}.toml"), &text) {
        Ok(_) => panic!("_invalid/{stem}.toml was accepted; it must be rejected"),
        Err(error) => assert!(
            error.contains(expected_fragment),
            "_invalid/{stem}.toml was rejected for the wrong reason.\n  expected to mention: \
             {expected_fragment}\n  got: {error}"
        ),
    }
}

#[test]
fn ledger_rejects_missing_field() {
    assert_rejected("missing_field", "missing field 'owner'");
}

#[test]
fn ledger_rejects_blank_field() {
    assert_rejected("blank_field", "is blank");
}

#[test]
fn ledger_rejects_bad_category() {
    assert_rejected("bad_category", "is not in the closed set");
}

#[test]
fn ledger_rejects_bad_review_date() {
    assert_rejected("bad_review_date", "is not an ISO YYYY-MM-DD date");
}

#[test]
fn ledger_rejects_dangling_decision_id() {
    assert_rejected("dangling_decision_id", "does not resolve to a");
}

#[test]
fn ledger_rejects_empty_blocked_by() {
    assert_rejected("empty_blocked_by", "blocked_by must be a non-empty array");
}

#[test]
fn ledger_rejects_duplicate_id() {
    assert_rejected("duplicate_id", "duplicate scenario id");
}

#[test]
fn ledger_rejects_private_path() {
    assert_rejected("private_path", "absolute private path marker");
}

#[test]
fn ledger_rejects_reason_too_long() {
    assert_rejected("reason_too_long", "the limit is 200");
}

#[test]
fn ledger_rejects_bad_upstream_revision() {
    assert_rejected("bad_upstream_revision", "is not a 40-hex pinned grit SHA");
}

#[test]
fn ledger_rejects_empty_upstream_file() {
    assert_rejected("empty_upstream_file", "field 'upstream_file' is blank");
}

#[test]
fn ledger_rejects_empty_libra_command() {
    assert_rejected("empty_libra_command", "field 'libra_command' is blank");
}

#[test]
fn ledger_rejects_empty_libra_surface() {
    assert_rejected("empty_libra_surface", "field 'libra_surface' is blank");
}

#[test]
fn ledger_rejects_bad_surface_status() {
    assert_rejected("bad_surface_status", "surface_compatibility");
}

#[test]
fn ledger_rejects_direct_with_incompatible_surface() {
    assert_rejected(
        "bad_direct_surface_status",
        "category 'direct' requires surface_compatibility",
    );
}

#[test]
fn ledger_rejects_self_reported_tier_mismatch() {
    assert_rejected("self_reported_tier_mismatch", "disagrees with the tier");
}

#[test]
fn ledger_rejects_unknown_field() {
    assert_rejected("unknown_field", "unknown field");
}

#[test]
fn ledger_empty_tree_passes() {
    // The tree starts empty and must not go red for having no evidence yet.
    // `load_real_ledger` panics on any invalid row, so reaching the end is
    // the assertion.
    let tree = load_real_ledger();
    for file in &tree {
        assert!(
            !file.scenarios.is_empty(),
            "{} carries no scenarios",
            file.relative
        );
    }
}

#[test]
fn ledger_dump_scenario_ids() {
    let example = load_example();
    let lines = dump_scenario_ids(&example);
    assert_eq!(lines.len(), example[0].scenarios.len());
    assert!(
        lines[0].starts_with("SCENARIO_ID ledger_example\t"),
        "dump format: {}",
        lines[0]
    );
    emit(&dump_scenario_ids(&load_real_ledger()));
}

#[test]
fn ledger_dump_direct_ids() {
    let example = load_example();
    let lines = dump_direct_ids(&example);
    assert!(
        !lines.is_empty(),
        "the worked example must contain a direct scenario so this dump is exercised"
    );
    for line in &lines {
        let rest = line
            .strip_prefix("DIRECT_ID ")
            .unwrap_or_else(|| panic!("dump format: {line}"));
        let (_id, path) = rest
            .split_once('\t')
            .unwrap_or_else(|| panic!("DIRECT_ID must carry two tab-separated columns: {line}"));
        assert!(
            path.starts_with("tests/compat-ledger/") && path.ends_with(".toml"),
            "second column is a repository-relative ledger path: {line}"
        );
    }
    emit(&dump_direct_ids(&load_real_ledger()));
}

#[test]
fn ledger_dump_libra_tests() {
    let example = load_example();
    let lines = dump_libra_tests(&example);
    assert!(
        !lines.is_empty(),
        "the worked example must carry libra_tests so this dump is exercised"
    );
    for line in &lines {
        assert!(line.starts_with("LIBRA_TEST "), "dump format: {line}");
    }
    emit(&dump_libra_tests(&load_real_ledger()));
}

#[test]
fn ledger_dump_surface_evidence() {
    let example = load_example();
    let lines = dump_surface_evidence(&example);
    assert_eq!(lines.len(), example[0].scenarios.len());
    for line in &lines {
        let rest = line
            .strip_prefix("EVIDENCE ")
            .unwrap_or_else(|| panic!("dump format: {line}"));
        assert!(
            rest.contains('\t'),
            "EVIDENCE must carry two tab-separated columns: {line}"
        );
    }
    emit(&dump_surface_evidence(&load_real_ledger()));
}

#[test]
fn ledger_dump_scenario_tests() {
    let example = load_example();
    let lines = dump_scenario_tests(&example);
    assert!(
        !lines.is_empty(),
        "the worked example must pair a scenario with a test so this dump is exercised"
    );
    for line in &lines {
        let rest = line
            .strip_prefix("SCENARIO_TEST ")
            .unwrap_or_else(|| panic!("dump format: {line}"));
        assert!(
            rest.contains('\t'),
            "SCENARIO_TEST must carry two tab-separated columns: {line}"
        );
    }
    emit(&dump_scenario_tests(&load_real_ledger()));
}

// ─────────────────────────────────────────────────────────────────────────────
// CT2-01 review round 1: the three matchers below were prefix-based and would
// each have produced a false PASS. Pin the boundaries directly, so a future
// simplification back to `starts_with` fails here instead of in the evidence.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn ledger_decision_id_match_is_bounded() {
    // The real headings are `### D1：…`, `### D10：…`.
    assert!(heading_names_decision("### D1：`submodule` 子命令族", "D1"));
    assert!(
        !heading_names_decision("### D10：sparse-checkout", "D1"),
        "D1 must not match the D10 heading"
    );
    assert!(decision_exists("D1"), "D1 exists in _compatibility.md");
    assert!(
        !decision_exists("D999"),
        "a decision that does not exist must not resolve"
    );
}

#[test]
fn ledger_section_anchor_match_is_exact() {
    let doc = "# Page\n\n## Options\n\nbody-a\n\n## Option Details\n\nbody-b\n";
    let options = section_text(doc, "options").expect("the Options section resolves");
    assert!(options.contains("body-a"), "resolved: {options}");
    assert!(
        !options.contains("body-b"),
        "a section must stop at the next heading of the same level: {options}"
    );
    assert!(
        section_text(doc, "option").is_none(),
        "a prefix of a heading slug must not resolve"
    );
}

#[test]
fn ledger_review_date_rejects_impossible_calendar_days() {
    assert!(is_iso_date("2026-08-08"));
    assert!(is_iso_date("2024-02-29"), "2024 is a leap year");
    assert!(!is_iso_date("2026-02-29"), "2026 is not a leap year");
    assert!(!is_iso_date("2026-04-31"), "April has 30 days");
    assert!(!is_iso_date("2026-13-01"));
    assert!(!is_iso_date("2026-00-10"));
    assert!(!is_iso_date("2026-8-8"), "the format is zero-padded");
}

// ─────────────────────────────────────────────────────────────────────────────
// CT2-02 — the surface lock: generated from the authoritative registry by
// `tests/compat-ledger/SURFACES.gen`, then treated as the adjudicator of every
// row's `surface_compatibility`.
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed `SURFACES.lock`: `(command, surface) -> status`.
#[derive(Debug, Default, Clone)]
struct SurfaceLock {
    rows: Vec<(String, String, String)>,
}

impl SurfaceLock {
    fn parse(text: &str) -> Result<Self, String> {
        let mut rows = Vec::new();
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 4 {
                return Err(format!(
                    "SURFACES.lock line {}: expected 4 tab-separated fields, found {}",
                    number + 1,
                    fields.len()
                ));
            }
            rows.push((
                fields[0].to_string(),
                fields[1].to_string(),
                fields[2].to_string(),
            ));
        }
        Ok(Self { rows })
    }

    /// GC-13's conflict resolution: longest exact prefix match. An exact key
    /// wins; failing that the longest key that is a prefix of the surface
    /// wins, so `--base=auto` is adjudicated by its own row when one exists
    /// and falls back to `--base` when it does not.
    ///
    /// Returns the key that matched together with its status, so a caller can
    /// say WHICH row decided — "specific over generic" is only checkable if
    /// the winner is observable.
    fn resolve(&self, command: &str, surface: &str) -> Option<(&str, &str)> {
        self.rows
            .iter()
            .filter(|(cmd, key, _)| cmd == command && surface.starts_with(key.as_str()))
            .max_by_key(|(_, key, _)| key.len())
            .map(|(_, key, status)| (key.as_str(), status.as_str()))
    }
}

/// Run `SURFACES.gen` over a registry and return the lock it prints.
fn run_surfaces_gen(registry: &Path) -> Result<String, String> {
    let generator = ledger_root().join("SURFACES.gen");
    let output = std::process::Command::new(&generator)
        .env("SURFACE_REGISTRY", registry)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("cannot run {}: {error}", generator.display()))?;
    if !output.status.success() {
        return Err(format!(
            "SURFACES.gen exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn example_registry() -> PathBuf {
    ledger_root().join("_example/registry_example.tsv")
}

fn invalid_registry(stem: &str) -> PathBuf {
    ledger_root().join(format!("_invalid_registry/{stem}.tsv"))
}

/// The lock the example registry generates, used to self-verify the mechanism
/// while the real per-family locks do not exist yet.
fn example_lock() -> SurfaceLock {
    let text = run_surfaces_gen(&example_registry())
        .unwrap_or_else(|error| panic!("the example registry must generate: {error}"));
    SurfaceLock::parse(&text).expect("the generated lock must parse")
}

fn assert_rejected_with_lock(stem: &str, expected_fragment: &str) {
    let path = invalid_file(stem);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let lock = example_lock();
    match validate_ledger_text_with_lock(&format!("_invalid/{stem}.toml"), &text, Some(&lock)) {
        Ok(_) => panic!("_invalid/{stem}.toml was accepted; it must be rejected"),
        Err(error) => assert!(
            error.contains(expected_fragment),
            "_invalid/{stem}.toml was rejected for the wrong reason.\n  expected to mention: \
             {expected_fragment}\n  got: {error}"
        ),
    }
}

#[test]
fn surfaces_gen_is_deterministic() {
    let first = run_surfaces_gen(&example_registry()).expect("first run");
    let second = run_surfaces_gen(&example_registry()).expect("second run");
    assert_eq!(
        first, second,
        "two runs over the same registry must be byte-identical, or the lock cannot be \
         regenerated and diffed"
    );
    assert!(
        !first.is_empty(),
        "the example registry is not empty, so its lock must not be either"
    );
    // Sorted, four columns, LF-terminated.
    let mut previous: Option<(String, String)> = None;
    for line in first.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 4, "lock line is four columns: {line}");
        let key = (fields[0].to_string(), fields[1].to_string());
        if let Some(previous) = previous {
            assert!(
                previous < key,
                "lock is sorted by (command, surface): {line}"
            );
        }
        previous = Some(key);
    }
    assert!(first.ends_with('\n'), "the lock is LF-terminated");
}

#[test]
fn surfaces_gen_empty_registry_yields_empty_lock() {
    // An empty registry is a legitimate starting state; it must not be an
    // error, and it must produce an empty lock rather than a stale one.
    let dir = std::env::temp_dir().join("libra-ct202-empty-registry");
    fs::create_dir_all(&dir).expect("temp dir");
    let registry = dir.join("empty.tsv");
    fs::write(&registry, "# only a comment\n\n").expect("write registry");
    let lock = run_surfaces_gen(&registry).expect("an empty registry is not an error");
    assert!(lock.is_empty(), "expected an empty lock, got: {lock:?}");
    let _ = fs::remove_file(&registry);
    let _ = fs::remove_dir(&dir);
}

fn assert_registry_rejected(stem: &str, expected_fragment: &str) {
    match run_surfaces_gen(&invalid_registry(stem)) {
        Ok(lock) => panic!("_invalid_registry/{stem}.tsv generated a lock: {lock:?}"),
        Err(error) => assert!(
            error.contains(expected_fragment),
            "_invalid_registry/{stem}.tsv failed for the wrong reason.\n  expected: \
             {expected_fragment}\n  got: {error}"
        ),
    }
}

#[test]
fn surfaces_gen_rejects_dangling_anchor() {
    assert_registry_rejected("dangling_anchor", "does not resolve");
}

#[test]
fn surfaces_gen_rejects_text_mismatch() {
    assert_registry_rejected("text_mismatch", "does not mention");
}

#[test]
fn surfaces_gen_rejects_duplicate_key() {
    assert_registry_rejected("duplicate_key", "duplicate key");
}

#[test]
fn surfaces_gen_rejects_missing_registry() {
    // A missing registry must be an error, never an empty lock: an empty lock
    // would let a downstream `diff` pass against evidence that documents
    // nothing.
    let missing = ledger_root().join("_example/there-is-no-such-registry.tsv");
    assert!(!missing.exists());
    match run_surfaces_gen(&missing) {
        Ok(lock) => panic!("a missing registry produced a lock: {lock:?}"),
        Err(error) => assert!(
            error.contains("cannot read the surface registry"),
            "unexpected failure: {error}"
        ),
    }
}

#[test]
fn ledger_longest_prefix_match_resolves_specific_over_generic() {
    let lock = example_lock();
    // Both `--word-diff` and `--word-diff-regex` are keys. A surface spelled
    // with a value must be adjudicated by the SPECIFIC key.
    let (key, _status) = lock
        .resolve("diff", "--word-diff-regex=[a-z]+")
        .expect("the specific key adjudicates");
    assert_eq!(
        key, "--word-diff-regex",
        "the longest matching key must win over the generic '--word-diff'"
    );
    // The generic key still adjudicates its own spellings.
    let (key, _status) = lock
        .resolve("diff", "--word-diff=color")
        .expect("the generic key adjudicates its own spelling");
    assert_eq!(key, "--word-diff");
    // A key of one command must not adjudicate another command's surface.
    assert!(
        lock.resolve("update-ref", "--numstat").is_none(),
        "keys are scoped to their command"
    );
}

#[test]
fn ledger_rejects_dangling_surface_evidence() {
    // The lock is consulted before the citation is, so this fixture only
    // exercises anchor resolution while its surface IS locked. Pin that, or a
    // later edit could quietly turn it into a second copy of
    // `ledger_rejects_surface_not_in_lock`.
    assert!(
        example_lock().resolve("diff", "--numstat").is_some(),
        "the fixture's surface must be in the lock, or it would be refused before its \
         dangling anchor is ever resolved"
    );
    assert_rejected_with_lock("dangling_surface_evidence", "names no such section");
}

#[test]
fn ledger_rejects_surface_evidence_text_mismatch() {
    assert_rejected_with_lock(
        "surface_evidence_text_mismatch",
        "does not mention '--stdin'",
    );
}

#[test]
fn ledger_rejects_surface_not_in_lock() {
    assert_rejected_with_lock("surface_not_in_lock", "is not in SURFACES.lock");
}

#[test]
fn ledger_rejects_direct_with_non_git_compatible_surface() {
    assert_rejected_with_lock("non_git_compatible_direct", "requires the lock to record");
}

/// CT2-02 AC 2: a committed lock must be exactly what the generator produces
/// from the authoritative registry today — otherwise the lock is a snapshot of
/// someone's intent rather than of the registry.
///
/// No family lock exists yet, so the mechanism self-verifies on the example
/// registry: generate, persist, regenerate, compare. When CT3-03 commits
/// `tests/compat-ledger/t4/SURFACES.lock` it enters the loop below with no
/// further wiring.
#[test]
fn ledger_committed_locks_match_the_regenerated_lock() {
    let registry = repo_root().join("docs/development/gap/surface-registry.tsv");
    let mut checked = 0usize;
    let root = ledger_root();
    if root.is_dir() {
        for family in read_dir_sorted(&root) {
            let name = family
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if !family.is_dir() || name.starts_with('_') {
                continue;
            }
            let lock_path = family.join("SURFACES.lock");
            if !lock_path.is_file() {
                continue;
            }
            let committed = fs::read_to_string(&lock_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", lock_path.display()));
            let regenerated = run_surfaces_gen(&registry).unwrap_or_else(|error| {
                panic!("regenerating the lock for family {name} failed: {error}")
            });
            assert_eq!(
                committed, regenerated,
                "{}/SURFACES.lock is not what SURFACES.gen produces from the registry \
                 today; regenerate it",
                name
            );
            checked += 1;
        }
    }

    if checked == 0 {
        // Self-verify the mechanism itself, so this test is never vacuous.
        let generated = run_surfaces_gen(&example_registry()).expect("generate the example lock");
        let dir = std::env::temp_dir().join("libra-ct202-lock-roundtrip");
        fs::create_dir_all(&dir).expect("temp dir");
        let persisted = dir.join("SURFACES.lock");
        fs::write(&persisted, &generated).expect("persist the example lock");
        let again = run_surfaces_gen(&example_registry()).expect("regenerate");
        let committed = fs::read_to_string(&persisted).expect("read back");
        assert_eq!(
            committed, again,
            "a persisted lock must equal a regeneration of the same registry"
        );
        let _ = fs::remove_file(&persisted);
        let _ = fs::remove_dir(&dir);
    }
}

/// CT2-02 review round 1 (P2): evidence-anchor resolution exists twice — in
/// python inside `SURFACES.gen`, which validates the REGISTRY's anchors, and in
/// Rust here, which validates each LEDGER row's citation. They read different
/// inputs for different purposes, so they cannot simply be merged; what they
/// must never do is disagree about whether a given anchor backs a given
/// (command, surface). This drives both over the same table and fails if they
/// ever split.
#[test]
fn ledger_anchor_resolution_agrees_with_the_generator() {
    // (anchor, command, surface, backs_the_row)
    let cases: &[(&str, &str, &str, bool)] = &[
        (
            "docs/commands/diff.md#description",
            "diff",
            "--numstat",
            true,
        ),
        (
            "docs/commands/update-ref.md#comparison-with-git",
            "update-ref",
            "--stdin",
            true,
        ),
        // Resolves, but the cited text never mentions the surface.
        (
            "docs/commands/update-ref.md#synopsis",
            "update-ref",
            "--stdin",
            false,
        ),
        // No such section.
        (
            "docs/commands/diff.md#no-such-section",
            "diff",
            "--numstat",
            false,
        ),
        // A prefix of a real heading slug must not bind.
        ("docs/commands/diff.md#descript", "diff", "--numstat", false),
        // Decision sections, matched by bounded decision id.
        ("_compatibility.md#D999", "diff", "--numstat", false),
        // Line references.
        ("COMPATIBILITY.md:0", "diff", "--numstat", false),
        ("COMPATIBILITY.md:99999999", "diff", "--numstat", false),
        ("COMPATIBILITY.md:notanumber", "diff", "--numstat", false),
        // Not one of the three admissible forms.
        (
            "docs/development/gap/grit-gap.md#anything",
            "diff",
            "--numstat",
            false,
        ),
        ("just-some-text", "diff", "--numstat", false),
    ];

    let dir = std::env::temp_dir().join("libra-ct202-anchor-parity");
    fs::create_dir_all(&dir).expect("temp dir");

    for (anchor, command, surface, expected) in cases {
        // The Rust side: does the citation resolve AND mention both literals?
        let rust_ok = check_surface_evidence("parity", anchor, command, surface).is_ok();

        // The generator side: a one-row registry with the same anchor.
        let registry = dir.join("parity.tsv");
        fs::write(
            &registry,
            format!("{command}\t{surface}\tgit-compatible\t{anchor}\n"),
        )
        .expect("write the parity registry");
        let generator_ok = run_surfaces_gen(&registry).is_ok();

        assert_eq!(
            rust_ok, *expected,
            "the ledger validator disagrees with the expectation for {anchor} \
             ({command} {surface})"
        );
        assert_eq!(
            generator_ok, rust_ok,
            "SURFACES.gen and the ledger validator disagree about {anchor} \
             ({command} {surface}): generator={generator_ok}, validator={rust_ok}"
        );
    }

    let _ = fs::remove_file(dir.join("parity.tsv"));
    let _ = fs::remove_dir(&dir);
}

/// CT2-02 review round 1 (a): the input side of determinism. A registry edited
/// on Windows, or with padded cells, must produce the same lock as the same
/// registry written cleanly — and padding must not let a duplicate key slip
/// through by looking different.
#[test]
fn surfaces_gen_normalises_line_endings_and_padding() {
    let dir = std::env::temp_dir().join("libra-ct202-normalisation");
    fs::create_dir_all(&dir).expect("temp dir");

    let clean = "diff\t--numstat\tgit-compatible\tdocs/commands/diff.md#description\n";
    let messy = "diff\t --numstat \tgit-compatible\t docs/commands/diff.md#description \r\n";

    let clean_path = dir.join("clean.tsv");
    let messy_path = dir.join("messy.tsv");
    fs::write(&clean_path, clean).expect("write clean");
    fs::write(&messy_path, messy).expect("write messy");
    assert_eq!(
        run_surfaces_gen(&clean_path).expect("clean generates"),
        run_surfaces_gen(&messy_path).expect("messy generates"),
        "CRLF and padded cells must normalise to the same lock"
    );

    // And padding must not disguise a duplicate key.
    let padded_dup = dir.join("padded_dup.tsv");
    fs::write(
        &padded_dup,
        format!("{clean} diff \t--numstat\tabsent\tdocs/commands/diff.md#description\n"),
    )
    .expect("write padded duplicate");
    match run_surfaces_gen(&padded_dup) {
        Ok(lock) => panic!("a padded duplicate key generated a lock: {lock:?}"),
        Err(error) => assert!(
            error.contains("duplicate key"),
            "expected a duplicate-key refusal: {error}"
        ),
    }

    for path in [clean_path, messy_path, padded_dup] {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_dir(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
// CT2-03 — the clean-room phrase allowlist, frozen by a sidecar digest.
// ─────────────────────────────────────────────────────────────────────────────

/// The clean-room gate's normalisation, mirrored here so the entry-format rule
/// is checked in the same terms the consumer will use: collapse whitespace,
/// lowercase, then count tokens. Its window is 8 tokens wide, so an entry of
/// any other length can never match and would sit in the file looking like an
/// approved exception while doing nothing.
fn allowlist_token_count(entry: &str) -> usize {
    entry.to_lowercase().split_whitespace().count()
}

#[test]
fn phrase_allowlist_sidecar_matches() {
    use sha2::{Digest, Sha256};

    let allowlist = ledger_root().join("PHRASE_ALLOWLIST.txt");
    let sidecar = ledger_root().join("PHRASE_ALLOWLIST.sha256");
    assert!(allowlist.is_file(), "{} must exist", allowlist.display());
    assert!(sidecar.is_file(), "{} must exist", sidecar.display());

    // The sidecar is a `shasum -a 256` line and nothing else. Anything extra
    // is a place for a second, contradictory claim to hide.
    let sidecar_text = fs::read_to_string(&sidecar).expect("read the sidecar");
    let mut lines = sidecar_text.lines();
    let line = lines.next().unwrap_or_default();
    assert!(
        lines.next().is_none(),
        "the sidecar must hold exactly one line, found:\n{sidecar_text}"
    );
    let (digest, name) = line
        .split_once("  ")
        .unwrap_or_else(|| panic!("sidecar line is not '<sha256>  <name>': {line:?}"));
    assert_eq!(
        name, "PHRASE_ALLOWLIST.txt",
        "the sidecar must name the allowlist relative to tests/compat-ledger/"
    );
    assert!(
        digest.len() == 64
            && digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "the digest must be 64 lowercase hex characters: {digest:?}"
    );

    let bytes = fs::read(&allowlist).expect("read the allowlist");
    let actual = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        actual, digest,
        "PHRASE_ALLOWLIST.txt has changed since it was frozen; the sidecar no longer \
         matches it"
    );

    // Entry format. Comments are free text; entries are not.
    let text = String::from_utf8(bytes).expect("the allowlist is UTF-8");
    let all_lines: Vec<&str> = text.lines().collect();
    assert!(
        all_lines.len() <= 20,
        "the allowlist is capped at 20 lines, found {}",
        all_lines.len()
    );
    for (index, line) in all_lines.iter().enumerate() {
        assert!(
            !line.trim().is_empty(),
            "line {} is blank; the allowlist has no blank lines",
            index + 1
        );
        if line.starts_with('#') {
            continue;
        }
        let tokens = allowlist_token_count(line);
        assert_eq!(
            tokens,
            8,
            "line {} is {tokens} tokens; the clean-room window is 8 tokens wide, so an \
             entry of any other length would never match and would fail silently: {line:?}",
            index + 1
        );
    }
}

/// The entry-format rule above is vacuous while the allowlist carries no
/// entries, which is its intended state. Exercise the rule itself so it cannot
/// rot before the first exception is ever granted.
#[test]
fn phrase_allowlist_token_rule_counts_normalised_tokens() {
    assert_eq!(
        allowlist_token_count("one two three four five six seven eight"),
        8
    );
    assert_eq!(
        allowlist_token_count("  One   Two\tthree four five six seven eight  "),
        8,
        "whitespace collapses and case folds, matching the gate's normalisation"
    );
    assert_eq!(
        allowlist_token_count("one two three four five six seven"),
        7
    );
    assert_eq!(
        allowlist_token_count("one two three four five six seven eight nine"),
        9
    );
    assert_eq!(allowlist_token_count(""), 0);
}
