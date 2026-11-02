//! Table-driven exit-code contract suite for the Git-compatible plumbing
//! commands (EC-01 in `docs/development/plan/issues/479.md`).
//!
//! Pins the documented exit-code contract from ADR-PL-02 without touching any
//! production code:
//!
//! - valid input exits `0`;
//! - Git-defined non-error non-zero cases (`cat-file -e` on a well-formed but
//!   absent object, and `diff-files` with working-tree differences) exit `1`;
//! - runtime fatals exit `128`;
//! - usage / invalid-target errors exit `129` with stable code `LBR-CLI-002` /
//!   `LBR-CLI-003` (Libra's documented divergence from Git's `128`; see
//!   `docs/error-codes.md`).
//!
//! Every row is asserted three times for identical exit codes, and the `--json`
//! variant is asserted to share the human-mode exit code. The suite never
//! shells out to the system `git` (repo convention); the `/usr/bin/git` 2.56
//! truth for each row was captured separately for the plan's manual evidence.
//!
//! **Layer:** L1 — deterministic, subprocess-based, no external dependencies.

use std::{fs, path::Path};

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

/// The eight plumbing command families covered by the EC-01 contract table.
const COMMANDS: [&str; 8] = [
    "cat-file",
    "hash-object",
    "ls-tree",
    "rev-parse",
    "update-ref",
    "symbolic-ref",
    "verify-pack",
    "diff-files",
];

/// A well-formed 40-hex SHA-1 object id that does not exist in any fixture.
const ABSENT_SHA1: &str = "1111111111111111111111111111111111111111";

/// The four input quadrants of the EC-01 contract. [`Quadrant::Differences`] is
/// a fifth, plumbing-diff-specific label for the "working tree differs"
/// outcome — exit 0 by default, 1 under `--exit-code`/`--quiet`
/// (ADR-PL-02 point 3 / ADR-PL-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quadrant {
    /// Well-formed input that succeeds.
    Valid,
    /// Well-formed but absent object / missing on-disk file.
    MissingObject,
    /// A name/ref/revision that cannot be resolved.
    UnresolvableName,
    /// CLI usage error (unknown flag, missing or conflicting arguments).
    UsageError,
    /// `diff-files` with working-tree differences (exit 1 by Git design).
    Differences,
}

/// The four core quadrants checked for every command by the coverage test.
const CORE_QUADRANTS: [Quadrant; 4] = [
    Quadrant::Valid,
    Quadrant::MissingObject,
    Quadrant::UnresolvableName,
    Quadrant::UsageError,
];

/// Non-applicable quadrant cells, with the reason they are not exercised.
///
/// The coverage test asserts each command is either covered by a table row or
/// justified here, never both and never neither.
const NA_QUADRANTS: &[(&str, Quadrant, &str)] = &[
    (
        "update-ref",
        Quadrant::MissingObject,
        "`update-ref -d <missing-ref>` is a successful no-op when no CAS value is supplied; deletion does not resolve an object",
    ),
    (
        "hash-object",
        Quadrant::MissingObject,
        "hash-object resolves no existing objects; it only hashes input content \
         (malformed content surfaces as UsageError LBR-CLI-002/129)",
    ),
    (
        "hash-object",
        Quadrant::UnresolvableName,
        "hash-object accepts no revision or ref arguments",
    ),
    (
        "rev-parse",
        Quadrant::UsageError,
        "rev-parse consumes positional tokens as revisions, so an unknown token is \
         an unresolvable name (LBR-CLI-003/129), not a clap usage error",
    ),
    (
        "symbolic-ref",
        Quadrant::MissingObject,
        "symbolic-ref resolves no objects; it only reads/writes HEAD",
    ),
    (
        "verify-pack",
        Quadrant::UnresolvableName,
        "verify-pack takes .idx file paths, not object names; a missing file is the \
         MissingObject row (LBR-IO-001/128)",
    ),
    (
        "diff-files",
        Quadrant::MissingObject,
        "diff-files compares the index against the working tree and resolves no object names",
    ),
    (
        "diff-files",
        Quadrant::UnresolvableName,
        "diff-files accepts no revision arguments",
    ),
];

/// A single exit-contract table row.
struct Case {
    /// Human-readable label used in assertion diagnostics.
    name: &'static str,
    /// Plumbing command family (the subcommand name; also `args[0]`).
    command: &'static str,
    /// The input quadrant this row exercises.
    quadrant: Quadrant,
    /// The argv for the subprocess (no global `--json` prefix).
    args: Vec<String>,
    /// The pinned exit code.
    exit_code: i32,
    /// Expected `Error-Code:` stable code in human stderr, for error rows.
    error_code: Option<&'static str>,
    /// Whether the row must produce an empty stderr (e.g. `cat-file -e` probe).
    silent: bool,
}

impl Case {
    fn new(
        name: &'static str,
        command: &'static str,
        quadrant: Quadrant,
        exit_code: i32,
        args: &[&str],
    ) -> Self {
        Self {
            name,
            command,
            quadrant,
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            exit_code,
            error_code: None,
            silent: false,
        }
    }

    fn with_error_code(mut self, code: &'static str) -> Self {
        self.error_code = Some(code);
        self
    }

    fn silent(mut self) -> Self {
        self.silent = true;
        self
    }
}

/// Build the full contract table. `valid_idx` is the path (relative to the
/// fixture root) of a real, verify-pack-acceptable `.idx` produced by `repack`.
fn build_cases(valid_idx: &str) -> Vec<Case> {
    let missing_idx = ".libra/objects/pack/does-not-exist.idx";
    vec![
        // ---- cat-file ----
        Case::new(
            "cat-file -e HEAD",
            "cat-file",
            Quadrant::Valid,
            0,
            &["cat-file", "-e", "HEAD"],
        ),
        Case::new(
            "cat-file -t HEAD",
            "cat-file",
            Quadrant::Valid,
            0,
            &["cat-file", "-t", "HEAD"],
        ),
        Case::new(
            "cat-file -s HEAD",
            "cat-file",
            Quadrant::Valid,
            0,
            &["cat-file", "-s", "HEAD"],
        ),
        Case::new(
            "cat-file -p HEAD",
            "cat-file",
            Quadrant::Valid,
            0,
            &["cat-file", "-p", "HEAD"],
        ),
        Case::new(
            "cat-file -e absent object",
            "cat-file",
            Quadrant::MissingObject,
            1,
            &["cat-file", "-e", ABSENT_SHA1],
        )
        .silent(),
        Case::new(
            "cat-file -t absent object",
            "cat-file",
            Quadrant::MissingObject,
            129,
            &["cat-file", "-t", ABSENT_SHA1],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "cat-file -e unresolvable name",
            "cat-file",
            Quadrant::UnresolvableName,
            129,
            &["cat-file", "-e", "not-a-ref"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "cat-file usage error",
            "cat-file",
            Quadrant::UsageError,
            129,
            &["cat-file", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        Case::new(
            "cat-file -e missing object arg",
            "cat-file",
            Quadrant::UsageError,
            129,
            &["cat-file", "-e"],
        )
        .with_error_code("LBR-CLI-002"),
        // ---- hash-object ----
        Case::new(
            "hash-object -t blob",
            "hash-object",
            Quadrant::Valid,
            0,
            &["hash-object", "-t", "blob", "blob.txt"],
        ),
        Case::new(
            "hash-object -t tree --literally",
            "hash-object",
            Quadrant::Valid,
            0,
            &["hash-object", "-t", "tree", "--literally", "tree.txt"],
        ),
        Case::new(
            "hash-object -t tree invalid content",
            "hash-object",
            Quadrant::UsageError,
            129,
            &["hash-object", "-t", "tree", "tree.txt"],
        )
        .with_error_code("LBR-CLI-002"),
        Case::new(
            "hash-object -t unsupported type",
            "hash-object",
            Quadrant::UsageError,
            129,
            &["hash-object", "-t", "tre", "blob.txt"],
        )
        .with_error_code("LBR-CLI-002"),
        Case::new(
            "hash-object usage error",
            "hash-object",
            Quadrant::UsageError,
            129,
            &["hash-object", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        // ---- ls-tree ----
        Case::new(
            "ls-tree HEAD",
            "ls-tree",
            Quadrant::Valid,
            0,
            &["ls-tree", "HEAD"],
        ),
        Case::new(
            "ls-tree absent object",
            "ls-tree",
            Quadrant::MissingObject,
            129,
            &["ls-tree", ABSENT_SHA1],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "ls-tree unresolvable name",
            "ls-tree",
            Quadrant::UnresolvableName,
            129,
            &["ls-tree", "not-a-ref"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "ls-tree usage error",
            "ls-tree",
            Quadrant::UsageError,
            129,
            &["ls-tree", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        // ---- rev-parse ----
        Case::new(
            "rev-parse HEAD",
            "rev-parse",
            Quadrant::Valid,
            0,
            &["rev-parse", "HEAD"],
        ),
        Case::new(
            "rev-parse --verify HEAD",
            "rev-parse",
            Quadrant::Valid,
            0,
            &["rev-parse", "--verify", "HEAD"],
        ),
        Case::new(
            "rev-parse --verify absent object",
            "rev-parse",
            Quadrant::MissingObject,
            128,
            &["rev-parse", "--verify", ABSENT_SHA1],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "rev-parse unresolvable name",
            "rev-parse",
            Quadrant::UnresolvableName,
            129,
            &["rev-parse", "not-a-ref"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "rev-parse --verify unresolvable name",
            "rev-parse",
            Quadrant::UnresolvableName,
            128,
            &["rev-parse", "--verify", "not-a-ref"],
        )
        .with_error_code("LBR-CLI-003"),
        // RV-01 index-path error rows (I7): a path absent from the index.
        // The I8 row (stage 0 missing with higher stages present) needs a
        // stage-only index entry, which would flip every `diff-files` row in
        // the shared fixture to "differences"; it lives in its own
        // deterministic sub-fixture below
        // (`plumbing_exit_contract_index_stage0_missing_row`).
        Case::new(
            "rev-parse :path not in index",
            "rev-parse",
            Quadrant::UnresolvableName,
            129,
            &["rev-parse", ":not-in-index.txt"],
        )
        .with_error_code("LBR-CLI-003"),
        // RV-02 message-search error rows (M5, M6, M7): bare leading `!`,
        // invalid regex, and no matching commit.
        Case::new(
            "rev-parse :/ bare ! message search",
            "rev-parse",
            Quadrant::UnresolvableName,
            129,
            &["rev-parse", ":/!x"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "rev-parse :/ invalid message-search regex",
            "rev-parse",
            Quadrant::UnresolvableName,
            129,
            &["rev-parse", ":/(unclosed"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "rev-parse :/ message search no match",
            "rev-parse",
            Quadrant::UnresolvableName,
            129,
            &["rev-parse", ":/no-such-message-anywhere"],
        )
        .with_error_code("LBR-CLI-003"),
        // ---- update-ref ----
        Case::new(
            "update-ref create ref",
            "update-ref",
            Quadrant::Valid,
            0,
            &["update-ref", "refs/heads/extra", "HEAD"],
        ),
        Case::new(
            "update-ref -d missing ref is a no-op",
            "update-ref",
            Quadrant::Valid,
            0,
            &["update-ref", "-d", "refs/heads/does-not-exist"],
        ),
        Case::new(
            "update-ref unresolvable new value",
            "update-ref",
            Quadrant::UnresolvableName,
            128,
            &["update-ref", "refs/heads/x", "not-a-ref"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "update-ref usage error",
            "update-ref",
            Quadrant::UsageError,
            129,
            &["update-ref", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        // UR-01 rows (B2/B3/out-of-scope): stdin batch failures leave every
        // ref untouched. The stdin body is injected by the dedicated tests in
        // update_ref_test; these rows pin the argv-level --no-deref no-op and
        // the flag surface.
        Case::new(
            "update-ref --no-deref accepted",
            "update-ref",
            Quadrant::Valid,
            0,
            &["update-ref", "--no-deref", "refs/heads/extra", "HEAD"],
        ),
        // ---- symbolic-ref ----
        Case::new(
            "symbolic-ref HEAD",
            "symbolic-ref",
            Quadrant::Valid,
            0,
            &["symbolic-ref", "HEAD"],
        ),
        Case::new(
            "symbolic-ref non-HEAD",
            "symbolic-ref",
            Quadrant::UnresolvableName,
            129,
            &["symbolic-ref", "FOO"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "symbolic-ref usage error",
            "symbolic-ref",
            Quadrant::UsageError,
            129,
            &["symbolic-ref", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        // SR-01 delete rows (S1, S2, S3): `-d` is accepted for the git t1401
        // contract and never deletes anything.
        Case::new(
            "symbolic-ref -d HEAD",
            "symbolic-ref",
            Quadrant::UsageError,
            128,
            &["symbolic-ref", "-d", "HEAD"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "symbolic-ref --delete other name",
            "symbolic-ref",
            Quadrant::UsageError,
            128,
            &["symbolic-ref", "--delete", "FOO"],
        )
        .with_error_code("LBR-CLI-003"),
        Case::new(
            "symbolic-ref -d branch ref",
            "symbolic-ref",
            Quadrant::UsageError,
            128,
            &["symbolic-ref", "-d", "refs/heads/main"],
        )
        .with_error_code("LBR-CLI-003"),
        // ---- verify-pack ----
        Case::new(
            "verify-pack valid index",
            "verify-pack",
            Quadrant::Valid,
            0,
            &["verify-pack", valid_idx],
        ),
        Case::new(
            "verify-pack missing index",
            "verify-pack",
            Quadrant::MissingObject,
            128,
            &["verify-pack", missing_idx],
        )
        .with_error_code("LBR-IO-001"),
        Case::new(
            "verify-pack usage error",
            "verify-pack",
            Quadrant::UsageError,
            129,
            &["verify-pack", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        Case::new(
            "verify-pack no index args",
            "verify-pack",
            Quadrant::UsageError,
            129,
            &["verify-pack"],
        )
        .with_error_code("LBR-CLI-002"),
        // ---- diff-files ----
        Case::new(
            "diff-files clean",
            "diff-files",
            Quadrant::Valid,
            0,
            &["diff-files"],
        ),
        Case::new(
            "diff-files usage error",
            "diff-files",
            Quadrant::UsageError,
            129,
            &["diff-files", "--bad"],
        )
        .with_error_code("LBR-CLI-002"),
        Case::new(
            "diff-files with differences",
            "diff-files",
            Quadrant::Differences,
            0,
            &["diff-files"],
        ),
        Case::new(
            "diff-files --name-only with differences",
            "diff-files",
            Quadrant::Differences,
            0,
            &["diff-files", "--name-only"],
        ),
        Case::new(
            "diff-files --exit-code with differences",
            "diff-files",
            Quadrant::Differences,
            1,
            &["diff-files", "--exit-code"],
        ),
        Case::new(
            "diff-files --quiet with differences",
            "diff-files",
            Quadrant::Differences,
            1,
            &["diff-files", "--quiet"],
        ),
        // ---- update-index (UI-01/UI-02/UI-03 appended rows) ----
        Case::new(
            "update-index --refresh modified worktree",
            "update-index",
            Quadrant::Differences,
            1,
            &["update-index", "--refresh"],
        ),
        Case::new(
            "update-index -q --refresh modified worktree",
            "update-index",
            Quadrant::Differences,
            0,
            &["update-index", "-q", "--refresh"],
        ),
        Case::new(
            "update-index --chmod non-regular path",
            "update-index",
            Quadrant::UsageError,
            128,
            &["update-index", "--chmod=+x", "subdir"],
        )
        .with_error_code("LBR-CLI-002"),
    ]
}

/// Build the shared fixture: a committed repository (tracked `tracked.txt`),
/// the `blob.txt` / `tree.txt` hash-object inputs, and a `repack`ed pack so the
/// `verify-pack` valid row has a real `.idx`. Returns the repo and the path
/// (repo-relative) of the generated `.idx`.
fn build_fixture() -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("blob.txt"), b"hello world\n").expect("write blob.txt fixture");
    fs::write(repo.path().join("tree.txt"), b"not a tree").expect("write tree.txt fixture");
    // The update-index --chmod non-regular row needs a directory target.
    fs::create_dir(repo.path().join("subdir")).expect("create subdir fixture");

    let repack = run_libra_command(&["repack"], repo.path());
    assert_cli_success(&repack, "repack fixture for verify-pack valid row");

    let pack_dir = repo.path().join(".libra").join("objects").join("pack");
    let idx = fs::read_dir(&pack_dir)
        .expect("read pack directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
        .expect("repack should produce an idx");
    let valid_idx = format!(
        ".libra/objects/pack/{}",
        idx.file_name()
            .expect("idx path should have a file name")
            .to_string_lossy()
    );
    (repo, valid_idx)
}

/// Run one row and assert its pinned exit code, stable error code, and
/// (for the `cat-file -e` probe) its silent stderr.
fn assert_case(repo: &Path, case: &Case) {
    let args: Vec<&str> = case.args.iter().map(String::as_str).collect();
    let output = run_libra_command(&args, repo);
    assert_eq!(
        output.status.code(),
        Some(case.exit_code),
        "{}: expected exit {} but got {:?}\nstdout: {}\nstderr: {}",
        case.name,
        case.exit_code,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(code) = case.error_code {
        assert!(
            stderr.contains(&format!("Error-Code: {code}")),
            "{}: expected stable error code {code} in stderr, got: {stderr}",
            case.name,
        );
    }
    if case.silent {
        assert!(
            output.stderr.is_empty(),
            "{}: expected silent stderr, got: {stderr}",
            case.name,
        );
    }
}

/// Run every row that needs a clean working tree, then dirty the tree, run the
/// `diff-files` differences rows, and restore the tree for the next pass.
fn run_contract_pass(repo: &Path, cases: &[Case]) {
    let (clean, dirty): (Vec<_>, Vec<_>) = cases
        .iter()
        .partition(|case| case.quadrant != Quadrant::Differences);
    for case in &clean {
        assert_case(repo, case);
    }
    fs::write(repo.join("tracked.txt"), b"changed\n").expect("dirty tracked.txt");
    for case in &dirty {
        assert_case(repo, case);
    }
    fs::write(repo.join("tracked.txt"), b"tracked\n").expect("restore tracked.txt");
}

/// Assert `--json` and human output share the same exit code for every row,
/// including the clean/dirty `diff-files` split.
fn run_json_human_pass(repo: &Path, cases: &[Case]) {
    let (clean, dirty): (Vec<_>, Vec<_>) = cases
        .iter()
        .partition(|case| case.quadrant != Quadrant::Differences);
    for case in &clean {
        assert_json_matches_human(repo, case);
    }
    fs::write(repo.join("tracked.txt"), b"changed\n").expect("dirty tracked.txt");
    for case in &dirty {
        assert_json_matches_human(repo, case);
    }
    fs::write(repo.join("tracked.txt"), b"tracked\n").expect("restore tracked.txt");
}

fn assert_json_matches_human(repo: &Path, case: &Case) {
    let human_args: Vec<&str> = case.args.iter().map(String::as_str).collect();
    let mut json_args = vec!["--json"];
    json_args.extend(case.args.iter().map(String::as_str));

    let human = run_libra_command(&human_args, repo);
    let json = run_libra_command(&json_args, repo);
    assert_eq!(
        human.status.code(),
        json.status.code(),
        "{}: human and --json exit codes differ\nhuman stderr: {}\njson stderr: {}",
        case.name,
        String::from_utf8_lossy(&human.stderr),
        String::from_utf8_lossy(&json.stderr),
    );
}

#[test]
fn plumbing_exit_codes_match_documented_contract() {
    let (repo, valid_idx) = build_fixture();
    let cases = build_cases(&valid_idx);
    run_contract_pass(repo.path(), &cases);
}

#[test]
fn plumbing_exit_codes_are_deterministic() {
    let (repo, valid_idx) = build_fixture();
    let cases = build_cases(&valid_idx);
    // Three full passes: every row must reproduce its pinned exit code and the
    // `diff-files` clean/dirty split must return the tree to clean each time.
    for _ in 0..3 {
        run_contract_pass(repo.path(), &cases);
    }
}

#[test]
fn plumbing_exit_codes_json_matches_human() {
    let (repo, valid_idx) = build_fixture();
    let cases = build_cases(&valid_idx);
    run_json_human_pass(repo.path(), &cases);
}

/// RV-01 I8 contract row: `rev-parse :<path>` where the path has higher
/// stages but no stage-0 entry must exit 129 with `LBR-CLI-003` (stage-0
/// explanation asserted in `rev_parse_test::test_rev_parse_index_path_matrix`).
///
/// The stage-only index entry would flip every `diff-files` row in the shared
/// fixture to "differences" (Libra's `diff-files` always injects `--exit-code`),
/// so this row gets a private fixture; it still honours the EC-01 guarantees:
/// three identical runs plus a `--json`-vs-human exit-code check.
#[test]
fn plumbing_exit_contract_index_stage0_missing_row() {
    let repo = create_committed_repo_via_cli();
    inject_stage_only_entry(repo.path());

    let case = Case::new(
        "rev-parse :path stage-0 missing",
        "rev-parse",
        Quadrant::UnresolvableName,
        129,
        &["rev-parse", ":conflict.txt"],
    )
    .with_error_code("LBR-CLI-003");
    for _ in 0..3 {
        assert_case(repo.path(), &case);
    }
    assert_json_matches_human(repo.path(), &case);
}

/// Write a stage-1-only `conflict.txt` entry into the repo index. The object
/// id need not exist: `rev-parse :conflict.txt` errors on the stage check
/// before reading any object.
fn inject_stage_only_entry(repo: &Path) {
    use git_internal::{
        hash::{HashKind, ObjectHash},
        internal::index::{Index, IndexEntry},
    };
    let index_path = repo.join(".libra").join("index");
    let mut index =
        Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load index fixture");
    let hash: ObjectHash = ABSENT_SHA1.parse().expect("parse stage blob oid");
    let mut entry = IndexEntry::new_from_blob("conflict.txt".to_string(), hash, 1);
    entry.flags.stage = 1;
    index.add(entry);
    index
        .save_with_hash_kind(HashKind::Sha1, &index_path)
        .expect("save stage-only index fixture");
}

#[test]
fn plumbing_exit_contract_covers_every_command_and_quadrant() {
    let cases = build_cases("<idx>");
    for command in COMMANDS {
        for quadrant in CORE_QUADRANTS {
            let has_case = cases
                .iter()
                .any(|case| case.command == command && case.quadrant == quadrant);
            let is_na = NA_QUADRANTS
                .iter()
                .any(|(cmd, q, _)| *cmd == command && *q == quadrant);
            assert!(
                has_case || is_na,
                "command {command} quadrant {quadrant:?} is neither covered nor marked N/A",
            );
            assert!(
                !(has_case && is_na),
                "command {command} quadrant {quadrant:?} is both covered and marked N/A",
            );
        }
    }
}
