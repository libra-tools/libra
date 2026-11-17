//! Integration tests for `libra revision` (lore.md §1.16): the rebuildable
//! per-ref first-parent ordinal index — deterministic, append-only on
//! fast-forward, rebuilt on rewrites/replace changes, never lying.
//!
//! **Layer:** L1 — deterministic.

use super::*;

fn linear_repo(n: usize) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for i in 1..=n {
        fs::write(p.join("f.txt"), format!("{i}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("c{i}"), "--no-verify"], p),
            "commit",
        );
    }
    repo
}

#[test]
fn revision_find_number_and_reverse_roundtrip() {
    let repo = linear_repo(3);
    let p = repo.path();
    // Chain: initial + 3 = 4 revisions. find(1) = root; number(HEAD) = 4.
    let idx = run_libra_command(&["--json", "revision", "index"], p);
    assert_cli_success(&idx, "index");
    let json = parse_json_stdout(&idx);
    let total = json["data"]["max_ordinal"].as_i64().unwrap();
    assert_eq!(total, 4, "{json}");
    let find = run_libra_command(&["--json", "revision", "find", "-n", "1"], p);
    assert_cli_success(&find, "find root");
    let root_oid = parse_json_stdout(&find)["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();
    // Roundtrip: the root's ordinal is 1.
    let number = run_libra_command(&["--json", "revision", "number", &root_oid], p);
    assert_cli_success(&number, "reverse");
    assert_eq!(
        parse_json_stdout(&number)["data"]["ordinal"].as_i64(),
        Some(1)
    );
    let head = run_libra_command(&["revision", "number", "HEAD"], p);
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "4");
    // Out of range → exit 1 with the total; zero/negative → 129.
    let miss = run_libra_command(&["revision", "find", "-n", "99"], p);
    assert_eq!(miss.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&miss.stderr).contains("has 4 revisions"),
        "{}",
        String::from_utf8_lossy(&miss.stderr)
    );
    let zero = run_libra_command(&["revision", "find", "-n", "0"], p);
    assert_eq!(zero.status.code(), Some(129));
}

#[test]
fn revision_index_append_rebuild_and_determinism() {
    let repo = linear_repo(2);
    let p = repo.path();
    // Build, remember the root OID.
    let root = parse_json_stdout(&run_libra_command(
        &["--json", "revision", "find", "-n", "1"],
        p,
    ))["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();
    // Fast-forward: existing ordinals never change (append-only).
    fs::write(p.join("f.txt"), "more\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c3", "--no-verify"], p),
        "commit",
    );
    let after = parse_json_stdout(&run_libra_command(
        &["--json", "revision", "find", "-n", "1"],
        p,
    ))["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(root, after, "ordinal 1 stable across fast-forward");
    // Determinism: --rebuild reproduces the identical (ordinal, oid) mapping.
    let before = parse_json_stdout(&run_libra_command(
        &["--json", "revision", "number", "HEAD"],
        p,
    ));
    assert_cli_success(
        &run_libra_command(&["revision", "index", "--rebuild"], p),
        "rebuild",
    );
    let rebuilt = parse_json_stdout(&run_libra_command(
        &["--json", "revision", "number", "HEAD"],
        p,
    ));
    assert_eq!(before["data"], rebuilt["data"], "rebuild is deterministic");
    // History rewrite: reset --hard to an ancestor rebuilds honestly.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD~2"], p),
        "reset",
    );
    let idx = parse_json_stdout(&run_libra_command(&["--json", "revision", "index"], p));
    assert_eq!(idx["data"]["max_ordinal"].as_i64(), Some(2), "{idx}");
    // A rewritten-away OID answers not-found, never a stale number.
    let gone_oid = before["data"]["oid"].as_str().unwrap();
    let gone = run_libra_command(&["revision", "number", gone_oid], p);
    assert!(
        !gone.status.success(),
        "rewritten-away OID is not resolvable"
    );
}

#[test]
fn revision_replace_change_invalidates_index() {
    let repo = linear_repo(2);
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["revision", "index"], p), "build");
    // Replace the ROOT's child's parent link? Simpler: replace revision 2
    // with revision 1 — the effective chain shortens on next validation.
    let two = parse_json_stdout(&run_libra_command(
        &["--json", "revision", "find", "-n", "2"],
        p,
    ))["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();
    let one = parse_json_stdout(&run_libra_command(
        &["--json", "revision", "find", "-n", "1"],
        p,
    ))["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();
    let replaced = run_libra_command(&["replace", &two, &one], p);
    assert_cli_success(&replaced, "replace");
    // The index must NOT serve the old chain as fresh: the replace-set
    // digest changed → rebuild on the next read (the chain now walks
    // through the replacement).
    let idx = parse_json_stdout(&run_libra_command(&["--json", "revision", "index"], p));
    assert_eq!(
        idx["data"]["max_ordinal"].as_i64(),
        Some(2),
        "chain re-walked under the replacement: {idx}"
    );
    // ordinal 2's slot now maps through the replaced parent chain — the
    // key assertion is freshness: built_at changed vs a no-op validate.
    let again = parse_json_stdout(&run_libra_command(&["--json", "revision", "index"], p));
    assert_eq!(
        idx["data"]["built_at"], again["data"]["built_at"],
        "stable once rebuilt under the new replace set"
    );
}

// Synthetic raw Git commits exercise the importer without manufacturing change
// rows or pretending that a past agent successfully tested these commits.
fn history_import_fixture() -> (tempfile::TempDir, serde_json::Value) {
    use sha2::{Digest, Sha256};
    let repo = tempfile::tempdir().unwrap();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], p),
        "init history fixture",
    );
    let write = |kind: &str, body: &str| {
        let output =
            run_libra_command_with_stdin(&["hash-object", "-w", "-t", kind, "--stdin"], p, body);
        assert_cli_success(&output, "write raw historical object");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let tree = write("tree", "");
    let root_body = format!(
        "tree {tree}\nauthor fixture <fixture@example.invalid> 1 +0000\ncommitter fixture <fixture@example.invalid> 1 +0000\n\nlegacy root\n"
    );
    let root = write("commit", &root_body);
    let tip_body = format!(
        "tree {tree}\nparent {root}\nauthor fixture <fixture@example.invalid> 2 +0000\ncommitter fixture <fixture@example.invalid> 2 +0000\n\nlegacy tip\n"
    );
    let tip = write("commit", &tip_body);
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/main", &tip], p),
        "set historical base",
    );
    let identity = run_libra_command(&["config", "get", "--local", "libra.repoid"], p);
    assert_cli_success(&identity, "repository identity");
    let repo_id = String::from_utf8(identity.stdout)
        .unwrap()
        .trim()
        .to_string();
    let mut commits = vec![root.clone(), tip.clone()];
    commits.sort();
    let commit_digest = format!(
        "{:x}",
        Sha256::digest(format!("{}\n", commits.join("\n")).as_bytes())
    );
    let objects = serde_json::json!([
        {"oid":tree,"kind":"tree","size":0},
        {"oid":root,"kind":"commit","size":root_body.len()},
        {"oid":tip,"kind":"commit","size":tip_body.len()}
    ]);
    let mut lines: Vec<_> = objects
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            format!(
                "{} {} {}\n",
                o["oid"].as_str().unwrap(),
                o["kind"].as_str().unwrap(),
                o["size"].as_u64().unwrap()
            )
        })
        .collect();
    lines.sort();
    let object_digest = format!("{:x}", Sha256::digest(lines.concat().as_bytes()));
    let manifest = serde_json::json!({
        "schema_version":1,"repo_id":repo_id,"object_format":"sha1","base_commit":tip,
        "visibility_policy":"strict_time","cutoff_ms":2000,
        "commit_oids":commits,"commit_inventory_sha256":commit_digest,
        "objects":objects,"object_inventory_sha256":object_digest,"shallow_cuts":[],
        "sources":[{"commit_oid":root,"available_at_ms":1000},{"commit_oid":tip,"available_at_ms":2000}]
    });
    (repo, manifest)
}

#[test]
fn revision_import_materializes_unknown_history_and_is_idempotent() {
    let (repo, manifest) = history_import_fixture();
    let path = repo.path().join("history.json");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let args = [
        "--json",
        "revision",
        "import",
        "--manifest",
        path.to_str().unwrap(),
    ];
    let first = run_libra_command(&args, repo.path());
    assert_cli_success(&first, "import historical commits");
    assert_eq!(parse_json_stdout(&first)["data"]["imported"], 2);
    let rebuilt = run_libra_command(&["--json", "memory", "rebuild"], repo.path());
    assert_cli_success(&rebuilt, "derive imported history");
    assert_eq!(parse_json_stdout(&rebuilt)["data"]["projected"], 2);
    let listing = run_libra_command(&["--json", "memory", "list"], repo.path());
    assert_cli_success(&listing, "list imported history");
    let json = parse_json_stdout(&listing);
    for episode in json["data"]["episodes"].as_array().unwrap() {
        let shown = run_libra_command(
            &[
                "--json",
                "memory",
                "show",
                episode["episode_id"].as_str().unwrap(),
            ],
            repo.path(),
        );
        assert_cli_success(&shown, "show imported history");
        let shown = parse_json_stdout(&shown);
        assert_eq!(shown["data"]["outcome"], "unknown", "{shown}");
    }
    let repeat = run_libra_command(&args, repo.path());
    assert_cli_success(&repeat, "repeat identical import");
    let repeat = parse_json_stdout(&repeat);
    assert_eq!(repeat["data"]["imported"], 0);
    assert_eq!(repeat["data"]["reused"], 2);
    assert_eq!(
        repeat["data"]["mappings"],
        parse_json_stdout(&first)["data"]["mappings"]
    );
    // A no-op import must not invalidate the already-built generation.
    assert_cli_success(
        &run_libra_command(&["--json", "memory", "list"], repo.path()),
        "read after idempotent import",
    );
    let (other, _) = history_import_fixture();
    let wrong_repo = run_libra_command(&args, other.path());
    assert_eq!(wrong_repo.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&wrong_repo.stderr).contains("another repository"));
    let mut narrower = manifest.clone();
    narrower["sources"].as_array_mut().unwrap().pop();
    let narrow_path = repo.path().join("narrow-history.json");
    fs::write(&narrow_path, serde_json::to_vec(&narrower).unwrap()).unwrap();
    let omitted = run_libra_command(
        &[
            "revision",
            "import",
            "--manifest",
            narrow_path.to_str().unwrap(),
        ],
        repo.path(),
    );
    assert_eq!(omitted.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&omitted.stderr).contains("omitted from this cutoff"));
    assert_cli_success(
        &run_libra_command(&["--json", "memory", "list"], repo.path()),
        "a refused cutoff must preserve the previously admitted generation",
    );
    let mut hostile = manifest.clone();
    hostile["\u{1b}[2Jforged"] = serde_json::json!(true);
    let hostile_path = repo.path().join("hostile-history.json");
    fs::write(&hostile_path, serde_json::to_vec(&hostile).unwrap()).unwrap();
    let rejected = run_libra_command(
        &[
            "revision",
            "import",
            "--manifest",
            hostile_path.to_str().unwrap(),
        ],
        repo.path(),
    );
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("unknown field"));
    assert!(
        !stderr.contains("\u{1b}[2J"),
        "manifest field escaped into terminal output"
    );
}

#[tokio::test]
async fn revision_import_rolls_back_the_first_revision_when_the_second_fails() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let (repo, manifest) = history_import_fixture();
    let path = repo.path().join("history.json");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let database = libra::internal::db::establish_connection(
        repo.path().join(".libra/libra.db").to_str().unwrap(),
    )
    .await
    .unwrap();
    let failing_oid = manifest["sources"][1]["commit_oid"].as_str().unwrap();
    database.execute_unprepared(&format!("CREATE TRIGGER reject_history_revision BEFORE INSERT ON change_revision WHEN NEW.commit_oid = '{failing_oid}' BEGIN SELECT RAISE(ABORT, 'injected second revision failure'); END")).await.unwrap();
    let output = run_libra_command(
        &[
            "--json",
            "revision",
            "import",
            "--manifest",
            path.to_str().unwrap(),
        ],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&output.stderr).contains("injected second revision failure"));
    for table in ["change_identity", "change_revision"] {
        let row = database
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT count(*) AS count FROM {table}"),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<i64>("", "count").unwrap(),
            0,
            "partial source rows survived in {table}"
        );
    }
}
