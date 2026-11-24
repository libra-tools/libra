//! Integration tests for `libra check-attr`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use tempfile::tempdir;

use super::{
    assert_cli_success, init_repo_via_cli, parse_json_stdout, run_libra_command,
    run_libra_command_with_env, run_libra_command_with_stdin,
};

/// Init a repo whose `.libra_attributes` runs `*.bin` through the LFS filter.
fn setup_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    let init = run_libra_command(&["init"], repo.path());
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        repo.path().join(".libra_attributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .expect("write .libra_attributes");
    repo
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn filter_on_tracked_path_reports_lfs() {
    let repo = setup_repo();
    let out = run_libra_command(&["check-attr", "filter", "a.bin"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "check-attr always exits 0 on success"
    );
    assert_eq!(stdout_of(&out), "a.bin: filter: lfs\n");
}

#[test]
fn filter_on_untracked_path_is_unspecified() {
    let repo = setup_repo();
    let out = run_libra_command(&["check-attr", "filter", "a.txt"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_of(&out), "a.txt: filter: unspecified\n");
}

#[test]
fn all_reports_only_set_attributes() {
    let repo = setup_repo();
    // Every explicitly assigned value/state is reported, including `-text`.
    let tracked = run_libra_command(&["check-attr", "--all", "a.bin"], repo.path());
    assert_eq!(tracked.status.code(), Some(0));
    assert_eq!(
        stdout_of(&tracked),
        "a.bin: diff: lfs\na.bin: merge: lfs\na.bin: text: unset\na.bin: filter: lfs\n"
    );
    // Untracked path: nothing is set.
    let untracked = run_libra_command(&["check-attr", "--all", "a.txt"], repo.path());
    assert_eq!(untracked.status.code(), Some(0));
    assert!(
        stdout_of(&untracked).is_empty(),
        "no attributes are set on an untracked path: {:?}",
        stdout_of(&untracked)
    );
}

#[test]
fn double_dash_separates_multiple_attributes() {
    let repo = setup_repo();
    let out = run_libra_command(
        &["check-attr", "filter", "text", "--", "a.bin"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("a.bin: filter: lfs"),
        "filter set: {stdout}"
    );
    assert!(
        stdout.contains("a.bin: text: unset"),
        "explicitly unset attr is reported as unset: {stdout}"
    );
}

#[test]
fn stdin_reads_pathnames() {
    let repo = setup_repo();
    let out = run_libra_command_with_stdin(
        &["check-attr", "filter", "--stdin"],
        repo.path(),
        "a.bin\na.txt\n",
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout_of(&out),
        "a.bin: filter: lfs\na.txt: filter: unspecified\n"
    );
}

#[test]
fn z_uses_nul_delimiters() {
    let repo = setup_repo();
    let out = run_libra_command(&["check-attr", "-z", "filter", "a.bin"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_of(&out), "a.bin\0filter\0lfs\0");
}

#[test]
fn json_output_reports_each_pair() {
    let repo = setup_repo();
    let out = run_libra_command(
        &["--json", "check-attr", "filter", "--", "a.bin", "a.txt"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(0));
    let json = parse_json_stdout(&out);
    let results = json["data"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);

    let bin = results
        .iter()
        .find(|r| r["path"] == "a.bin")
        .expect("a.bin");
    assert_eq!(bin["attr"], "filter");
    assert_eq!(bin["value"], "lfs");

    let txt = results
        .iter()
        .find(|r| r["path"] == "a.txt")
        .expect("a.txt");
    assert_eq!(txt["value"], "unspecified");
}

#[test]
fn missing_attribute_is_a_usage_error() {
    let repo = setup_repo();
    // A single positional with no `--`, `--all`, or `--stdin` cannot be split
    // into an attribute and a path -> fatal usage error (exit 128).
    let out = run_libra_command(&["check-attr", "filter"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "need an attribute and a pathname: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn outside_repository_is_an_error() {
    let dir = tempdir().expect("tempdir");
    let out = run_libra_command(&["check-attr", "filter", "a.bin"], dir.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "outside a repository is fatal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn path_outside_worktree_is_unspecified_not_a_panic() {
    let repo = setup_repo();
    // `../escape.bin` matches the `*.bin` rule lexically, but it escapes the
    // worktree — it must report `unspecified` (not `lfs`) and must NOT panic
    // (the LFS path relativization would otherwise trip).
    let out = run_libra_command(&["check-attr", "filter", "../escape.bin"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "out-of-worktree path is not fatal here"
    );
    assert_eq!(stdout_of(&out), "../escape.bin: filter: unspecified\n");
}

/// MX-11 (issues/481): `--cached` reads attribute files from the index and
/// `--source=<tree>` from a tree, while the default stays on the working tree.
#[test]
fn check_attr_cached_and_source_matrix() {
    let repo = super::create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join(".gitattributes"), "*.txt diff=csv\n").expect("write attributes");
    fs::write(root.join("a.txt"), "a\n").expect("write a.txt");
    super::assert_cli_success(
        &run_libra_command(&["add", ".gitattributes", "a.txt"], root),
        "stage attributes",
    );
    super::assert_cli_success(
        &run_libra_command(&["commit", "-m", "attrs", "--no-verify"], root),
        "commit attributes",
    );
    // Change the worktree copy without staging: the three sources now differ.
    fs::write(root.join(".gitattributes"), "*.txt diff=inline\n").expect("dirty attributes");

    let worktree = run_libra_command(&["check-attr", "diff", "a.txt"], root);
    assert_eq!(
        String::from_utf8_lossy(&worktree.stdout).trim(),
        "a.txt: diff: inline",
        "the default source is the working tree"
    );

    let cached = run_libra_command(&["check-attr", "--cached", "diff", "a.txt"], root);
    assert_eq!(
        String::from_utf8_lossy(&cached.stdout).trim(),
        "a.txt: diff: csv",
        "--cached reads the staged attribute file"
    );

    let source = run_libra_command(&["check-attr", "--source=HEAD", "diff", "a.txt"], root);
    assert_eq!(
        String::from_utf8_lossy(&source.stdout).trim(),
        "a.txt: diff: csv",
        "--source=HEAD reads the committed attribute file (tree-ish peels to its tree)"
    );

    // `--all` honours the same source.
    let all_cached = run_libra_command(&["check-attr", "--cached", "--all", "a.txt"], root);
    assert!(
        String::from_utf8_lossy(&all_cached.stdout).contains("a.txt: diff: csv"),
        "--all --cached must report the staged value: {}",
        String::from_utf8_lossy(&all_cached.stdout)
    );
}

/// PM-18: global attribute sources follow Git — XDG default, HOME fallback,
/// case-insensitive config keys, and explicit configuration beating XDG.
#[test]
fn check_attr_global_sources_follow_git() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    let home = p.join(".libra-test-home");
    let xdg_git = home.join(".config").join("git");
    std::fs::create_dir_all(&xdg_git).expect("mkdir xdg git");
    std::fs::write(p.join("f"), "x\n").expect("write f");

    // AC1: the XDG default file is read when nothing is configured.
    std::fs::write(xdg_git.join("attributes"), "f attr_f=set\n").expect("write xdg attrs");
    let out = run_libra_command(&["check-attr", "-a", "f"], p);
    assert_cli_success(&out, "AC1");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "f: attr_f: set\n",
        "AC1"
    );

    // AC2: an empty XDG_CONFIG_HOME falls back to $HOME/.config/git.
    std::fs::write(
        home.join(".config").join("git").join("attributes"),
        "f attr_f=home\n",
    )
    .expect("write home attrs");
    let out = run_libra_command_with_env(&["check-attr", "-a", "f"], p, &[("XDG_CONFIG_HOME", "")]);
    assert_cli_success(&out, "AC2");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "f: attr_f: home\n",
        "AC2"
    );

    // AC3: a lowercase `core.attributesfile` spelling is honored.
    std::fs::write(home.join("my_attrs"), "f attr_f=test\n").expect("write my_attrs");
    assert_cli_success(
        &run_libra_command(&["config", "core.attributesfile", "~/my_attrs"], p),
        "AC3 config",
    );
    let out = run_libra_command(&["check-attr", "-a", "f"], p);
    assert_cli_success(&out, "AC3");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "f: attr_f: test\n",
        "AC3"
    );

    // AC4: an explicitly configured (missing) file suppresses the XDG default.
    let out = run_libra_command(&["config", "core.attributesFile", "/nonexistent/attrs"], p);
    assert_cli_success(&out, "AC4 config");
    let out = run_libra_command(&["check-attr", "-a", "f"], p);
    assert_cli_success(&out, "AC4");
    assert!(
        out.stdout.is_empty(),
        "AC4: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_cli_success(
        &run_libra_command(&["config", "unset", "core.attributesFile"], p),
        "AC4 unset",
    );
    assert_cli_success(
        &run_libra_command(&["config", "unset", "core.attributesfile"], p),
        "AC4 unset lowercase",
    );

    // AC5: a repository `.gitattributes` overrides the global file.
    std::fs::write(p.join(".gitattributes"), "f -attr_f\n").expect("write local attrs");
    let out = run_libra_command(&["check-attr", "-a", "f"], p);
    assert_cli_success(&out, "AC5");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "f: attr_f: unset\n",
        "AC5"
    );
}

/// PM-27: attribute names are validated (Git's `attr_name_valid`) before any
/// path or stdin read, and `--all` conflicts with named attributes.
#[test]
fn check_attr_rejects_invalid_attribute_names() {
    use std::{io::Write, process::Stdio};

    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    std::fs::write(repo.path().join("x"), "x\n").expect("write x");
    std::fs::write(repo.path().join("x.dat"), "x\n").expect("write x.dat");

    for args in [
        vec!["check-attr", "", "x"],
        vec!["check-attr", "", "--", "x"],
        vec!["check-attr", "a b", "--", "x"],
        vec!["check-attr", "a!", "x"],
        vec!["check-attr", "--all", "diff", "--", "x.dat"],
    ] {
        let out = run_libra_command(&args, repo.path());
        assert!(!out.status.success(), "{args:?} must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
            "{args:?} code"
        );
    }

    // AC3: validation runs before stdin is read, so a producer that keeps stdin
    // open still gets an immediate usage error.
    let mut command = super::base_libra_command(&["check-attr", "--stdin", ""], repo.path());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn check-attr");
    let _stdin = child.stdin.take().expect("stdin");
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().expect("wait") {
            Some(status) => {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(10),
                    "AC3 timeout"
                );
                assert!(!status.success(), "AC3 exit");
                break;
            }
            None if start.elapsed() > std::time::Duration::from_secs(10) => {
                let _ = child.kill();
                panic!("AC3 must fail before reading stdin");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let mut stderr = String::new();
    use std::io::Read;
    if let Some(mut handle) = child.stderr.take() {
        handle.read_to_string(&mut stderr).expect("read stderr");
    }
    assert!(stderr.contains("LBR-CLI-002"), "AC3 code: {stderr}");
    let _ = std::io::stdout().flush();
}

/// PM-15: `[attr]` macros, the built-in `binary` macro, and Git's precedence.
#[test]
fn check_attr_expands_macros_with_git_precedence() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    std::fs::create_dir_all(p.join("sub")).expect("mkdir sub");
    std::fs::write(
        p.join(".gitattributes"),
        "[attr]mybin -diff -text\n*.dat mybin\n[attr]outer inner -merge\n[attr]inner foo=bar\n*.n outer\n*.b binary\n*.b2 mybin\n*.b2 diff\n*.e mybin\n*.e !mybin\n",
    )
    .expect("root gitattributes");
    std::fs::write(p.join("sub/.gitattributes"), "[attr]subm foo\n*.s subm\n").expect("sub");

    // AC1: a macro expands to its body.
    let out = run_libra_command(&["check-attr", "diff", "x.dat"], p);
    assert_cli_success(&out, "AC1");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "x.dat: diff: unset\n",
        "AC1"
    );

    // AC3: the built-in `binary` macro.
    let out = run_libra_command(&["check-attr", "-a", "f.b"], p);
    assert_cli_success(&out, "AC3");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "f.b: binary: set\nf.b: diff: unset\nf.b: merge: unset\nf.b: text: unset\n",
        "AC3"
    );

    // AC4: macros referencing macros.
    let out = run_libra_command(&["check-attr", "-a", "y.n"], p);
    assert_cli_success(&out, "AC4");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "y.n: merge: unset\ny.n: outer: set\ny.n: inner: set\ny.n: foo: bar\n",
        "AC4"
    );

    // AC5: a later explicit assignment wins over the macro body.
    let out = run_libra_command(&["check-attr", "-a", "x.b2"], p);
    assert_cli_success(&out, "AC5");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "x.b2: diff: set\nx.b2: text: unset\nx.b2: mybin: set\n",
        "AC5"
    );

    // AC6: `!macro` blocks the earlier expansion.
    let out = run_libra_command(&["check-attr", "-a", "x.e"], p);
    assert_cli_success(&out, "AC6");
    assert!(
        out.stdout.is_empty(),
        "AC6: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // AC7: macros defined in a subdirectory file are ignored.
    let out = run_libra_command(&["check-attr", "foo", "sub/x.s"], p);
    assert_cli_success(&out, "AC7");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "sub/x.s: foo: unspecified\n",
        "AC7"
    );

    // AC2: info/attributes macro wins; a macro defined only in the global file
    // is still usable.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    let global = p.join("global-attributes");
    std::fs::write(&global, "[attr]prio foo=global\n[attr]gm bar=g\n").expect("global");
    assert_cli_success(
        &run_libra_command(
            &["config", "core.attributesFile", global.to_str().unwrap()],
            p,
        ),
        "AC2 config",
    );
    std::fs::write(
        p.join(".gitattributes"),
        "[attr]prio foo=root\n*.p prio gm\n",
    )
    .expect("root");
    std::fs::create_dir_all(p.join(".libra/info")).expect("mkdir info");
    std::fs::write(p.join(".libra/info/attributes"), "[attr]prio foo=info\n").expect("info");
    let out = run_libra_command(&["check-attr", "foo", "bar", "--", "z.p"], p);
    assert_cli_success(&out, "AC2");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "z.p: foo: info\nz.p: bar: g\n",
        "AC2"
    );

    // AC8: a 3001-level macro chain is expanded iteratively.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    let mut rules = String::from("file a0\n");
    for index in 0..3000 {
        rules.push_str(&format!("[attr]a{index} a{}\n", index + 1));
    }
    rules.push_str("[attr]a3000 -text\n");
    std::fs::write(p.join(".gitattributes"), rules).expect("deep chain");
    let out = run_libra_command(&["check-attr", "text", "file"], p);
    assert_cli_success(&out, "AC8");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "file: text: unset\n",
        "AC8"
    );
}

/// PM-28: `-a` follows Git's attribute registration order.
#[test]
fn check_attr_all_uses_registration_order() {
    // AC1: built-ins first, then names in file order; only set attributes.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(
        p.join(".gitattributes"),
        "[attr]mybin -diff -text\n*.dat mybin\n[attr]outer inner -merge\n[attr]inner foo=bar\n*.n outer\n",
    )
    .expect("root attributes");
    let out = run_libra_command(&["check-attr", "-a", "x.dat"], p);
    assert_cli_success(&out, "AC1");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "x.dat: diff: unset\nx.dat: text: unset\nx.dat: mybin: set\n",
        "AC1"
    );

    // AC3: an LFS-style `.libra_attributes` puts `filter` after the built-ins.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(
        p.join(".libra_attributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .expect("libra attributes");
    let out = run_libra_command(&["check-attr", "-a", "a.bin"], p);
    assert_cli_success(&out, "AC3");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a.bin: diff: lfs\na.bin: merge: lfs\na.bin: text: unset\na.bin: filter: lfs\n",
        "AC3"
    );

    // AC4: registration is global → root → info → subdirectory files.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    std::fs::create_dir_all(p.join("sub")).expect("mkdir sub");
    std::fs::create_dir_all(p.join(".libra/info")).expect("mkdir info");
    std::fs::write(p.join(".gitattributes"), "*.s rootattr\n").expect("root");
    std::fs::write(p.join(".libra/info/attributes"), "*.s infoattr\n").expect("info");
    std::fs::write(p.join("sub/.gitattributes"), "*.s subattr\n").expect("sub");
    let out = run_libra_command(&["check-attr", "-a", "sub/x.s"], p);
    assert_cli_success(&out, "AC4");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "sub/x.s: rootattr: set\nsub/x.s: infoattr: set\nsub/x.s: subattr: set\n",
        "AC4"
    );

    // AC2: a 3001-level macro chain registers `text` first, then a0..a3000.
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    let mut rules = String::from("file a0\n");
    for index in 0..3000 {
        rules.push_str(&format!("[attr]a{index} a{}\n", index + 1));
    }
    rules.push_str("[attr]a3000 -text\n");
    std::fs::write(p.join(".gitattributes"), rules).expect("deep chain");
    let out = run_libra_command(&["check-attr", "-a", "file"], p);
    assert_cli_success(&out, "AC2");
    let lines: Vec<&str> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| Box::leak(line.to_string().into_boxed_str()) as &str)
        .collect();
    assert_eq!(lines.len(), 3002, "AC2 line count");
    assert_eq!(lines[0], "file: text: unset", "AC2 first");
    assert_eq!(lines[1], "file: a0: set", "AC2 second");
    assert_eq!(lines[3001], "file: a3000: set", "AC2 last");
}
