//! P1-11 import/export fidelity and real-Git interoperability.
//!
//! L1 is deterministic and always runs. Real `git` checks are conditional so
//! environments without a system Git still exercise Libra's complete round trip.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use libra::utils::pager::LIBRA_TEST_ENV;
use tempfile::tempdir;

fn libra_command(cwd: &Path, args: &[&str]) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
    fs::create_dir_all(&config_home).expect("create isolated config home");
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(LIBRA_TEST_ENV, "1");
    command
}

fn run_libra(cwd: &Path, args: &[&str]) -> Output {
    libra_command(cwd, args)
        .output()
        .expect("spawn libra command")
}

fn run_libra_stdin(cwd: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = libra_command(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libra command with stdin");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .expect("write command stdin");
    child.wait_with_output().expect("collect libra output")
}

fn success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_libra(repo: &Path) {
    init_libra_with_format(repo, None);
}

fn init_libra_with_format(repo: &Path, object_format: Option<&str>) {
    fs::create_dir_all(repo).expect("create repo");
    let init = match object_format {
        Some(format) => run_libra(repo, &["init", "--object-format", format]),
        None => run_libra(repo, &["init"]),
    };
    success(&init, "libra init");
    success(
        &run_libra(repo, &["config", "user.name", "Round Trip"]),
        "configure user.name",
    );
    success(
        &run_libra(repo, &["config", "user.email", "roundtrip@example.com"]),
        "configure user.email",
    );
}

fn seed_libra_repo(repo: &Path) -> (String, String) {
    init_libra(repo);
    fs::write(repo.join("space name.txt"), "space\n").expect("write spaced path");
    fs::write(repo.join("tab\tname.txt"), "tab\n").expect("write tab path");
    success(
        &run_libra(repo, &["add", "space name.txt", "tab\tname.txt"]),
        "stage quoted paths",
    );
    success(
        &run_libra(repo, &["commit", "-m", "base", "--no-verify"]),
        "commit base",
    );
    let base = String::from_utf8_lossy(&run_libra(repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    fs::write(repo.join("space name.txt"), "space two\n").expect("update spaced path");
    success(
        &run_libra(repo, &["add", "space name.txt"]),
        "stage second commit",
    );
    success(
        &run_libra(repo, &["commit", "-m", "second", "--no-verify"]),
        "commit second",
    );
    let tip = String::from_utf8_lossy(&run_libra(repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    success(&run_libra(repo, &["branch", "topic"]), "create topic");
    success(
        &run_libra(repo, &["tag", "-m", "release message", "v1"]),
        "create annotated tag",
    );
    success(
        &run_libra(repo, &["notes", "add", "-m", "reviewed note"]),
        "create note",
    );
    (base, tip)
}

#[test]
fn libra_all_export_round_trips_refs_tags_notes_ranges_and_quoted_paths() {
    let source = tempdir().expect("source repo");
    let (base, tip) = seed_libra_repo(source.path());

    let export = run_libra(source.path(), &["fast-export", "--all"]);
    success(&export, "libra fast-export --all");
    let stream = String::from_utf8(export.stdout.clone()).expect("UTF-8 export stream");
    assert!(stream.contains("reset refs/heads/main"), "{stream}");
    assert!(stream.contains("reset refs/heads/topic"), "{stream}");
    assert!(stream.contains("tag v1\n"), "{stream}");
    assert!(stream.contains("commit refs/notes/commits"), "{stream}");
    assert!(stream.contains("N :"), "{stream}");
    assert!(stream.contains("\"space name.txt\""), "{stream}");
    assert!(stream.contains("\"tab\\tname.txt\""), "{stream}");

    let target = tempdir().expect("target repo");
    init_libra(target.path());
    let imported = run_libra_stdin(target.path(), &["fast-import", "--quiet"], &export.stdout);
    success(&imported, "Libra all-stream import");
    success(
        &run_libra(target.path(), &["rev-parse", "refs/heads/main"]),
        "imported main ref",
    );
    success(
        &run_libra(target.path(), &["rev-parse", "refs/heads/topic"]),
        "imported topic ref",
    );
    let imported_tag = run_libra(target.path(), &["cat-file", "-t", "refs/tags/v1"]);
    success(&imported_tag, "imported annotated tag");
    assert_eq!(String::from_utf8_lossy(&imported_tag.stdout).trim(), "tag");
    let note = run_libra(target.path(), &["notes", "show", "refs/heads/main"]);
    success(&note, "imported note");
    assert!(String::from_utf8_lossy(&note.stdout).contains("reviewed note"));
    let spaced = run_libra(
        target.path(),
        &["cat-file", "-p", "refs/heads/main:space name.txt"],
    );
    success(&spaced, "imported quoted path");
    assert_eq!(String::from_utf8_lossy(&spaced.stdout), "space two\n");

    let range = run_libra(source.path(), &["fast-export", &format!("{base}..{tip}")]);
    success(&range, "incremental range export");
    let range = String::from_utf8(range.stdout).expect("range stream UTF-8");
    assert_eq!(range.matches("commit ").count(), 1, "{range}");
    assert!(range.contains(&format!("from {base}")), "{range}");

    let tag_only = run_libra(source.path(), &["fast-export", "v1"]);
    success(&tag_only, "annotated-tag-only export");
    let tag_target = tempdir().expect("tag-only target");
    init_libra(tag_target.path());
    success(
        &run_libra_stdin(
            tag_target.path(),
            &["fast-import", "--quiet"],
            &tag_only.stdout,
        ),
        "annotated-tag-only import",
    );
    let imported_tag = run_libra(tag_target.path(), &["cat-file", "-t", "refs/tags/v1"]);
    success(&imported_tag, "tag-only imported annotated tag");
    assert_eq!(String::from_utf8_lossy(&imported_tag.stdout).trim(), "tag");
}

#[test]
fn fast_import_handles_inline_copy_rename_annotated_tag_and_note_modify() {
    let repo = tempdir().expect("import repo");
    init_libra(repo.path());
    let stream = b"commit refs/heads/imported
mark :1
committer Importer <importer@example.com> 1700000000 +0000
data 8
imported
M 100644 inline \"dir/a b.txt\"
data 7
payload
C \"dir/a b.txt\" \"copy\\tname.txt\"
R \"dir/a b.txt\" renamed.txt

tag v-import
from :1
tagger Importer <importer@example.com> 1700000001 +0000
data 7
release

commit refs/notes/commits
mark :2
committer Importer <importer@example.com> 1700000002 +0000
data 0
N inline :1
data 8
reviewed

done
";
    let imported = run_libra_stdin(repo.path(), &["fast-import", "--quiet"], stream);
    success(&imported, "manual fast-import stream");

    let renamed = run_libra(
        repo.path(),
        &["cat-file", "-p", "refs/heads/imported:renamed.txt"],
    );
    success(&renamed, "renamed path");
    assert_eq!(String::from_utf8_lossy(&renamed.stdout), "payload");
    let copied = run_libra(
        repo.path(),
        &["cat-file", "-p", "refs/heads/imported:copy\tname.txt"],
    );
    success(&copied, "copied tab path");
    assert_eq!(String::from_utf8_lossy(&copied.stdout), "payload");
    assert!(
        !run_libra(
            repo.path(),
            &["cat-file", "-e", "refs/heads/imported:dir/a b.txt"]
        )
        .status
        .success(),
        "rename source must be absent"
    );
    let tag = run_libra(repo.path(), &["cat-file", "-t", "refs/tags/v-import"]);
    success(&tag, "manual annotated tag");
    assert_eq!(String::from_utf8_lossy(&tag.stdout).trim(), "tag");
    let note = run_libra(repo.path(), &["notes", "show", "refs/heads/imported"]);
    success(&note, "manual note modify");
    assert!(String::from_utf8_lossy(&note.stdout).contains("reviewed"));

    let imported_oid = String::from_utf8_lossy(
        &run_libra(repo.path(), &["rev-parse", "refs/heads/imported"]).stdout,
    )
    .trim()
    .to_string();
    let rollback_stream = format!(
        "reset refs/tags/v-import\n\nreset refs/custom/unsupported\nfrom {imported_oid}\n\ndone\n"
    );
    let rollback = run_libra_stdin(
        repo.path(),
        &["fast-import", "--quiet"],
        rollback_stream.as_bytes(),
    );
    assert!(!rollback.status.success(), "unsupported ref must fail");
    success(
        &run_libra(repo.path(), &["cat-file", "-t", "refs/tags/v-import"]),
        "failed transaction must retain tag",
    );

    let invalid_mode_stream = b"blob
mark :10
data 4
blob
commit refs/heads/type-confusion
mark :11
committer Importer <importer@example.com> 1700000003 +0000
data 3
bad
M 160000 :10 submodule

done
";
    let invalid_mode = run_libra_stdin(
        repo.path(),
        &["fast-import", "--quiet"],
        invalid_mode_stream,
    );
    assert!(
        !invalid_mode.status.success(),
        "mode/object type confusion must fail"
    );
    assert!(
        !run_libra(repo.path(), &["rev-parse", "refs/heads/type-confusion"])
            .status
            .success(),
        "invalid mode stream must not publish its ref"
    );

    let delete_stream = b"reset refs/heads/imported

reset refs/tags/v-import

reset refs/notes/commits

done
";
    success(
        &run_libra_stdin(repo.path(), &["fast-import", "--quiet"], delete_stream),
        "reset deletion stream",
    );
    assert!(
        !run_libra(repo.path(), &["rev-parse", "refs/heads/imported"])
            .status
            .success(),
        "branch reset without from must delete"
    );
    assert!(
        !run_libra(repo.path(), &["rev-parse", "refs/tags/v-import"])
            .status
            .success(),
        "tag reset without from must delete"
    );
    assert!(
        !run_libra(repo.path(), &["notes", "show", &imported_oid])
            .status
            .success(),
        "notes reset without from must delete mappings"
    );

    success(
        &run_libra(
            repo.path(),
            &["config", "fastimport.maxInputSize", "invalid"],
        ),
        "set invalid import limit",
    );
    let invalid_limit = run_libra_stdin(repo.path(), &["fast-import", "--quiet"], b"done\n");
    assert!(
        !invalid_limit.status.success(),
        "invalid limit must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&invalid_limit.stderr).contains("invalid fastimport.maxInputSize"),
        "{}",
        String::from_utf8_lossy(&invalid_limit.stderr)
    );
}

#[test]
fn bundle_selectors_unbundle_and_real_git_interoperate() {
    let source = tempdir().expect("bundle source");
    seed_libra_repo(source.path());

    let branches_bundle = source.path().join("branches.bundle");
    success(
        &run_libra(
            source.path(),
            &[
                "bundle",
                "create",
                branches_bundle.to_str().expect("branches bundle path"),
                "--branches",
            ],
        ),
        "bundle create --branches",
    );
    let branch_heads = run_libra(
        source.path(),
        &[
            "bundle",
            "list-heads",
            branches_bundle.to_str().expect("branches bundle path"),
        ],
    );
    success(&branch_heads, "list branch-only bundle heads");
    let branch_heads = String::from_utf8_lossy(&branch_heads.stdout);
    assert!(branch_heads.contains(" refs/heads/main"), "{branch_heads}");
    assert!(branch_heads.contains(" refs/heads/topic"), "{branch_heads}");
    assert!(!branch_heads.contains(" refs/tags/"), "{branch_heads}");

    let tags_bundle = source.path().join("tags.bundle");
    success(
        &run_libra(
            source.path(),
            &[
                "bundle",
                "create",
                tags_bundle.to_str().expect("tags bundle path"),
                "--tags",
            ],
        ),
        "bundle create --tags",
    );
    let tag_heads = run_libra(
        source.path(),
        &[
            "bundle",
            "list-heads",
            tags_bundle.to_str().expect("tags bundle path"),
        ],
    );
    success(&tag_heads, "list tag-only bundle heads");
    let tag_heads = String::from_utf8_lossy(&tag_heads.stdout);
    assert!(tag_heads.contains(" refs/tags/v1"), "{tag_heads}");
    assert!(!tag_heads.contains(" refs/heads/"), "{tag_heads}");

    let bundle = source.path().join("all.bundle");
    let created = run_libra(
        source.path(),
        &[
            "bundle",
            "create",
            bundle.to_str().expect("bundle path"),
            "--all",
        ],
    );
    success(&created, "bundle create --all");
    success(
        &run_libra(
            source.path(),
            &["bundle", "verify", bundle.to_str().expect("bundle path")],
        ),
        "bundle verify",
    );
    let heads = run_libra(
        source.path(),
        &[
            "bundle",
            "list-heads",
            bundle.to_str().expect("bundle path"),
        ],
    );
    success(&heads, "bundle list-heads");
    let heads_text = String::from_utf8_lossy(&heads.stdout);
    assert!(heads_text.contains("refs/heads/main"), "{heads_text}");
    assert!(heads_text.contains("refs/heads/topic"), "{heads_text}");
    let tag_line = heads_text
        .lines()
        .find(|line| line.ends_with(" refs/tags/v1"))
        .expect("annotated tag head");
    let tag_oid = tag_line.split_whitespace().next().expect("tag oid");

    let target = tempdir().expect("unbundle target");
    init_libra(target.path());
    let unbundled = run_libra(
        target.path(),
        &["bundle", "unbundle", bundle.to_str().expect("bundle path")],
    );
    success(&unbundled, "libra bundle unbundle");
    success(
        &run_libra(
            target.path(),
            &["bundle", "unbundle", bundle.to_str().expect("bundle path")],
        ),
        "idempotent repeated unbundle",
    );
    let tag = run_libra(target.path(), &["cat-file", "-t", tag_oid]);
    success(&tag, "unbundled tag object");
    assert_eq!(String::from_utf8_lossy(&tag.stdout).trim(), "tag");

    if !git_available() {
        eprintln!("skipped real-Git bundle/import interoperability (git unavailable)");
        return;
    }
    let clone_parent = tempdir().expect("git clone parent");
    let clone_path = clone_parent.path().join("clone");
    let clone = Command::new("git")
        .args(["clone", bundle.to_str().expect("bundle path")])
        .arg(&clone_path)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("git clone bundle");
    success(&clone, "system Git clone from Libra bundle");
    let show_ref = Command::new("git")
        .args([
            "-C",
            clone_path.to_str().expect("clone path"),
            "show-ref",
            "--tags",
        ])
        .output()
        .expect("git show-ref tags");
    success(&show_ref, "system Git sees Libra annotated tag");
}

#[test]
fn import_export_and_bundle_are_hash_kind_neutral_in_sha256_repositories() {
    let source = tempdir().expect("sha256 source");
    init_libra_with_format(source.path(), Some("sha256"));
    fs::write(source.path().join("wide hash.txt"), "sha256 payload\n")
        .expect("write sha256 payload");
    success(
        &run_libra(source.path(), &["add", "wide hash.txt"]),
        "stage sha256 payload",
    );
    success(
        &run_libra(
            source.path(),
            &["commit", "-m", "sha256 base", "--no-verify"],
        ),
        "commit sha256 payload",
    );
    success(
        &run_libra(source.path(), &["tag", "-m", "sha256 tag", "sha-v1"]),
        "create sha256 annotated tag",
    );
    success(
        &run_libra(source.path(), &["notes", "add", "-m", "sha256 note"]),
        "create sha256 note",
    );
    let source_head = run_libra(source.path(), &["rev-parse", "HEAD"]);
    success(&source_head, "resolve sha256 source head");
    let source_head = String::from_utf8_lossy(&source_head.stdout)
        .trim()
        .to_string();
    assert_eq!(source_head.len(), 64);

    let export = run_libra(source.path(), &["fast-export", "--all"]);
    success(&export, "sha256 fast-export");
    let target = tempdir().expect("sha256 target");
    init_libra_with_format(target.path(), Some("sha256"));
    success(
        &run_libra_stdin(target.path(), &["fast-import", "--quiet"], &export.stdout),
        "sha256 fast-import",
    );
    let imported_head = run_libra(target.path(), &["rev-parse", "refs/heads/main"]);
    success(&imported_head, "resolve imported sha256 head");
    let imported_head_text = String::from_utf8_lossy(&imported_head.stdout)
        .trim()
        .to_string();
    assert_eq!(imported_head_text.len(), 64);
    let source_commit = run_libra(source.path(), &["cat-file", "-p", &source_head]);
    success(&source_commit, "read source sha256 commit");
    let imported_commit = run_libra(target.path(), &["cat-file", "-p", &imported_head_text]);
    success(&imported_commit, "read imported sha256 commit");
    assert_eq!(
        imported_commit.stdout, source_commit.stdout,
        "sha256 import must preserve the rendered commit body"
    );
    success(
        &run_libra(target.path(), &["notes", "show", "refs/heads/main"]),
        "imported sha256 note",
    );
    let imported_tag = run_libra(target.path(), &["cat-file", "-t", "refs/tags/sha-v1"]);
    success(&imported_tag, "imported sha256 tag");
    assert_eq!(String::from_utf8_lossy(&imported_tag.stdout).trim(), "tag");

    let bundle = source.path().join("sha256.bundle");
    success(
        &run_libra(
            source.path(),
            &[
                "bundle",
                "create",
                bundle.to_str().expect("bundle path"),
                "--all",
            ],
        ),
        "create sha256 bundle",
    );
    success(
        &run_libra(
            source.path(),
            &["bundle", "verify", bundle.to_str().expect("bundle path")],
        ),
        "verify sha256 bundle",
    );
    let unbundle_target = tempdir().expect("sha256 unbundle target");
    init_libra_with_format(unbundle_target.path(), Some("sha256"));
    success(
        &run_libra(
            unbundle_target.path(),
            &["bundle", "unbundle", bundle.to_str().expect("bundle path")],
        ),
        "unbundle sha256 objects",
    );
    success(
        &run_libra(unbundle_target.path(), &["cat-file", "-t", &source_head]),
        "read unbundled sha256 commit",
    );
}

#[test]
fn fast_streams_interoperate_bidirectionally_with_real_git() {
    if !git_available() {
        eprintln!("skipped real-Git fast-stream interoperability (git unavailable)");
        return;
    }

    let libra_source = tempdir().expect("Libra export source");
    seed_libra_repo(libra_source.path());
    let libra_export = run_libra(libra_source.path(), &["fast-export", "--all"]);
    success(&libra_export, "Libra export for system Git");

    let git_bare_parent = tempdir().expect("Git bare parent");
    let git_bare = git_bare_parent.path().join("imported.git");
    let git_init = Command::new("git")
        .args(["init", "--bare"])
        .arg(&git_bare)
        .output()
        .expect("git init --bare");
    success(&git_init, "initialize bare Git import target");
    let git_import = run_git_stdin(
        None,
        &[
            "--git-dir",
            git_bare.to_str().expect("bare Git path"),
            "fast-import",
            "--quiet",
        ],
        &libra_export.stdout,
    );
    success(&git_import, "system Git imports Libra stream");
    let git_tag_type = Command::new("git")
        .args([
            "--git-dir",
            git_bare.to_str().expect("bare Git path"),
            "cat-file",
            "-t",
            "refs/tags/v1",
        ])
        .output()
        .expect("inspect Git tag");
    success(&git_tag_type, "system Git imported annotated tag");
    assert_eq!(String::from_utf8_lossy(&git_tag_type.stdout).trim(), "tag");
    let git_note = Command::new("git")
        .args([
            "--git-dir",
            git_bare.to_str().expect("bare Git path"),
            "notes",
            "--ref=commits",
            "show",
            "refs/heads/main",
        ])
        .output()
        .expect("inspect Git note");
    success(&git_note, "system Git imported Libra note");
    assert!(String::from_utf8_lossy(&git_note.stdout).contains("reviewed note"));

    let tag_only_export = run_libra(libra_source.path(), &["fast-export", "v1"]);
    success(
        &tag_only_export,
        "Libra annotated-tag-only export for system Git",
    );
    let git_tag_bare = git_bare_parent.path().join("tag-only.git");
    let git_init = Command::new("git")
        .args(["init", "--bare"])
        .arg(&git_tag_bare)
        .output()
        .expect("git init tag-only bare target");
    success(&git_init, "initialize tag-only bare Git target");
    success(
        &run_git_stdin(
            None,
            &[
                "--git-dir",
                git_tag_bare.to_str().expect("tag-only bare Git path"),
                "fast-import",
                "--quiet",
            ],
            &tag_only_export.stdout,
        ),
        "system Git imports annotated-tag-only Libra stream",
    );
    let git_tag_type = Command::new("git")
        .args([
            "--git-dir",
            git_tag_bare.to_str().expect("tag-only bare Git path"),
            "cat-file",
            "-t",
            "refs/tags/v1",
        ])
        .output()
        .expect("inspect tag-only Git tag");
    success(&git_tag_type, "system Git imported tag-only annotated tag");
    assert_eq!(String::from_utf8_lossy(&git_tag_type.stdout).trim(), "tag");

    let git_source = tempdir().expect("Git export source");
    let git_init = Command::new("git")
        .arg("init")
        .arg(git_source.path())
        .output()
        .expect("git init source");
    success(&git_init, "initialize Git export source");
    success(
        &run_git(
            git_source.path(),
            &["symbolic-ref", "HEAD", "refs/heads/main"],
        ),
        "select Git main branch",
    );
    success(
        &run_git(git_source.path(), &["config", "user.name", "Git Exporter"]),
        "configure Git user.name",
    );
    success(
        &run_git(
            git_source.path(),
            &["config", "user.email", "git-exporter@example.com"],
        ),
        "configure Git user.email",
    );
    fs::write(git_source.path().join("Git space 雪.txt"), "from Git\n")
        .expect("write Git source file");
    success(&run_git(git_source.path(), &["add", "."]), "Git add");
    success(
        &run_git(git_source.path(), &["commit", "-m", "Git base"]),
        "Git commit",
    );
    success(
        &run_git(
            git_source.path(),
            &["tag", "-a", "git-v1", "-m", "Git release"],
        ),
        "Git annotated tag",
    );
    success(
        &run_git(git_source.path(), &["notes", "add", "-m", "note from Git"]),
        "Git note",
    );
    let git_export = run_git(git_source.path(), &["fast-export", "--all"]);
    success(&git_export, "system Git fast-export --all");

    let libra_target = tempdir().expect("Libra import target");
    init_libra(libra_target.path());
    let libra_import = run_libra_stdin(
        libra_target.path(),
        &["fast-import", "--quiet"],
        &git_export.stdout,
    );
    success(&libra_import, "Libra imports system Git stream");
    let git_main = run_git(git_source.path(), &["rev-parse", "refs/heads/main"]);
    success(&git_main, "resolve system Git main");
    let libra_main = run_libra(libra_target.path(), &["rev-parse", "refs/heads/main"]);
    success(&libra_main, "resolve imported Libra main");
    assert_eq!(
        String::from_utf8_lossy(&libra_main.stdout).trim(),
        String::from_utf8_lossy(&git_main.stdout).trim(),
        "fast-import must preserve Git commit identity; stream:\n{}",
        String::from_utf8_lossy(&git_export.stdout)
    );
    let imported_path = run_libra(
        libra_target.path(),
        &["cat-file", "-p", "refs/heads/main:Git space 雪.txt"],
    );
    success(&imported_path, "Libra imported Git quoted path");
    assert_eq!(String::from_utf8_lossy(&imported_path.stdout), "from Git\n");
    let imported_tag = run_libra(libra_target.path(), &["cat-file", "-t", "refs/tags/git-v1"]);
    success(&imported_tag, "Libra imported Git annotated tag");
    assert_eq!(String::from_utf8_lossy(&imported_tag.stdout).trim(), "tag");
    let imported_note = run_libra(libra_target.path(), &["notes", "show", "refs/heads/main"]);
    success(&imported_note, "Libra imported Git note");
    assert!(String::from_utf8_lossy(&imported_note.stdout).contains("note from Git"));
}

/// Hosts may enable `tag.gpgSign` / `commit.gpgSign` in the global
/// gitconfig, which would turn fixture tags into signed tags and make
/// `git fast-export --all` abort (default `--signed-tags=abort`). Inject
/// env-scoped config overrides so the interop fixture is independent of
/// the host's global Git configuration on every platform.
fn neutralize_host_git_signing(command: &mut Command) {
    command
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "tag.gpgSign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "commit.gpgSign")
        .env("GIT_CONFIG_VALUE_1", "false");
}

fn run_git(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    neutralize_host_git_signing(&mut command);
    command.output().expect("spawn system Git")
}

fn run_git_stdin(cwd: Option<&Path>, args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new("git");
    command
        .args(args)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    neutralize_host_git_signing(&mut command);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().expect("spawn system Git with stdin");
    child
        .stdin
        .take()
        .expect("piped Git stdin")
        .write_all(input)
        .expect("write Git stdin");
    child.wait_with_output().expect("collect system Git output")
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
