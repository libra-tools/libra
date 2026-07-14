//! Cross-command Git object/revision compatibility for plan-20260708 P1-09.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("create temp directory");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("LIBRA_CONFIG_GLOBAL_DB", self.home.join(".libra/config.db"))
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile_file);
        }
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert!(
            output.status.success(),
            "{} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repo(&self, name: &str, object_format: Option<&str>) -> PathBuf {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo).expect("create repository directory");
        let repo_text = repo.to_str().expect("utf8 repo");
        match object_format {
            Some(format) => {
                self.success(&self.root, &["init", "--object-format", format, repo_text]);
            }
            None => {
                self.success(&self.root, &["init", repo_text]);
            }
        }
        self.success(&repo, &["config", "set", "user.name", "Revision Test"]);
        self.success(
            &repo,
            &["config", "set", "user.email", "revision@example.com"],
        );
        repo
    }

    fn commit_all(&self, repo: &Path, message: &str) {
        self.success(repo, &["add", "-A"]);
        self.success(repo, &["commit", "--no-gpg-sign", "-s", "-m", message]);
    }

    fn text(&self, repo: &Path, args: &[&str]) -> String {
        String::from_utf8(self.success(repo, args).stdout)
            .expect("command output is utf8")
            .trim()
            .to_string()
    }
}

#[test]
fn typed_peel_tree_paths_and_commit_tree_share_one_strict_resolver() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("typed-peel", None);
    fs::create_dir_all(repo.join("src")).expect("create source directory");
    fs::write(repo.join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n").expect("write source");
    fixture.commit_all(&repo, "base");

    let head = fixture.text(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(fixture.text(&repo, &["rev-parse", "HEAD^{commit}"]), head);

    let tree = fixture.text(&repo, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(fixture.text(&repo, &["cat-file", "-t", &tree]), "tree");
    assert_eq!(
        fixture.text(&repo, &["cat-file", "-t", "HEAD^{tree}"]),
        "tree",
        "cat-file must not treat ^{tree} as a parent suffix and ignore trailing text"
    );

    let blob = fixture.text(&repo, &["rev-parse", "HEAD:src/lib.rs"]);
    assert_eq!(fixture.text(&repo, &["cat-file", "-t", &blob]), "blob");
    assert_eq!(
        fixture.text(&repo, &["cat-file", "-p", "HEAD:src/lib.rs"]),
        "pub fn answer() -> u8 { 42 }"
    );

    let created = fixture.text(
        &repo,
        &["commit-tree", "HEAD^{tree}", "-m", "plumbing commit"],
    );
    assert_eq!(fixture.text(&repo, &["cat-file", "-t", &created]), "commit");
    assert_eq!(
        fixture.text(&repo, &["rev-parse", &format!("{created}^{{tree}}")]),
        tree
    );

    let malformed = fixture.run(&repo, &["cat-file", "-t", "HEAD^{tree}junk"]);
    assert_eq!(malformed.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&malformed.stderr).contains("Not a valid object name"),
        "unexpected malformed-spec error: {}",
        String::from_utf8_lossy(&malformed.stderr)
    );

    let missing_oid = "0000000000000000000000000000000000000000";
    assert_eq!(
        fixture.text(&repo, &["rev-parse", missing_oid]),
        missing_oid,
        "plain rev-parse may echo a syntactically complete object id"
    );
    let verify_missing = fixture.run(&repo, &["rev-parse", "--verify", missing_oid]);
    assert!(
        !verify_missing.status.success(),
        "--verify must prove that a full object id exists"
    );
    assert!(
        String::from_utf8_lossy(&verify_missing.stderr).contains(missing_oid),
        "unexpected missing-object error: {}",
        String::from_utf8_lossy(&verify_missing.stderr)
    );

    fs::remove_file(
        repo.join(".libra/objects")
            .join(&tree[..2])
            .join(&tree[2..]),
    )
    .expect("remove HEAD tree object");
    let corrupt_tree = fixture.run(&repo, &["read-tree", "HEAD"]);
    assert_eq!(corrupt_tree.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&corrupt_tree.stderr).contains("LBR-REPO-002"),
        "corrupt tree reference must retain its repository-corruption classification: {}",
        String::from_utf8_lossy(&corrupt_tree.stderr)
    );
}

#[test]
fn annotated_tag_refs_peel_for_commit_consumers_and_show_ref_dereferences() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("tag-peel", None);
    fs::write(repo.join("release.txt"), "release\n").expect("write release file");
    fixture.commit_all(&repo, "release");
    fixture.success(&repo, &["branch", "topic"]);
    fixture.success(&repo, &["tag", "-m", "release v1", "v1.0"]);

    let head = fixture.text(&repo, &["rev-parse", "HEAD"]);
    let tag_object = fixture.text(&repo, &["rev-parse", "v1.0"]);
    assert_ne!(tag_object, head, "annotated tag resolves to its tag object");
    assert_eq!(
        fixture.text(&repo, &["rev-parse", "refs/tags/v1.0"]),
        tag_object
    );
    assert_eq!(
        fixture.text(&repo, &["rev-parse", "v1.0^{tag}"]),
        tag_object
    );
    assert_eq!(fixture.text(&repo, &["rev-parse", "v1.0^{}"]), head);
    assert_eq!(
        fixture.text(&repo, &["rev-parse", "refs/tags/v1.0^{commit}"]),
        head
    );

    let branches = fixture.text(
        &repo,
        &["branch", "--list", "--points-at", "refs/tags/v1.0"],
    );
    assert!(
        branches
            .lines()
            .any(|line| line.trim_end().ends_with("main"))
    );
    assert!(
        branches
            .lines()
            .any(|line| line.trim_end().ends_with("topic"))
    );

    let show_ref = fixture.text(&repo, &["show-ref", "--dereference", "--tags", "v1.0"]);
    let lines = show_ref.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "unexpected show-ref output: {show_ref}");
    assert_eq!(lines[0], format!("{tag_object} refs/tags/v1.0"));
    assert_eq!(lines[1], format!("{head} refs/tags/v1.0^{{}}"));
}

#[test]
fn at_shorthand_and_numeric_reflog_selectors_are_newest_first() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("reflog-selectors", None);
    fs::write(repo.join("state.txt"), "one\n").expect("write first state");
    fixture.commit_all(&repo, "one");
    let first = fixture.text(&repo, &["rev-parse", "HEAD"]);

    fs::write(repo.join("state.txt"), "two\n").expect("write second state");
    fixture.commit_all(&repo, "two");
    let second = fixture.text(&repo, &["rev-parse", "HEAD"]);

    assert_eq!(fixture.text(&repo, &["rev-parse", "@"]), second);
    assert_eq!(fixture.text(&repo, &["rev-parse", "main@{0}"]), second);
    assert_eq!(fixture.text(&repo, &["rev-parse", "@{0}"]), second);
    assert_eq!(fixture.text(&repo, &["rev-parse", "HEAD@{0}"]), second);
    assert_eq!(fixture.text(&repo, &["rev-parse", "main@{1}"]), first);
    assert_eq!(fixture.text(&repo, &["rev-parse", "HEAD@{1}"]), first);
    assert_eq!(
        fixture.text(&repo, &["cat-file", "-p", "@{1}:state.txt"]),
        "one"
    );

    let unsupported = fixture.run(&repo, &["rev-parse", "HEAD@{yesterday}"]);
    assert_eq!(unsupported.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("non-negative numeric index"));
}

#[test]
fn typed_peel_and_tree_path_are_hash_kind_neutral_in_sha256_repo() {
    let fixture = Fixture::new();
    let repo = fixture.init_repo("sha256", Some("sha256"));
    fs::write(repo.join("sha256.txt"), "wide hash\n").expect("write sha256 fixture");
    fixture.commit_all(&repo, "sha256 base");

    let head = fixture.text(&repo, &["rev-parse", "HEAD^{commit}"]);
    let tree = fixture.text(&repo, &["rev-parse", "HEAD^{tree}"]);
    let blob = fixture.text(&repo, &["rev-parse", "HEAD:sha256.txt"]);
    for object_id in [&head, &tree, &blob] {
        assert_eq!(
            object_id.len(),
            64,
            "expected SHA-256 object id: {object_id}"
        );
    }
    assert_eq!(fixture.text(&repo, &["cat-file", "-t", &tree]), "tree");
    assert_eq!(fixture.text(&repo, &["cat-file", "-t", &blob]), "blob");
}
