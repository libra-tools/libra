//! P1-06 fetch/remote refspec and metadata compatibility guards.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

const PATH_ENV: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("create fixture root");
        let root = temp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).expect("create isolated home");
        Self {
            _temp: temp,
            root,
            home,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let config_home = self.home.join(".config");
        fs::create_dir_all(&config_home).expect("create config home");
        let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", PATH_ENV)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("LIBRA_CONFIG_GLOBAL_DB", self.home.join(".libra/config.db"))
            .env("LIBRA_CONFIG_SYSTEM_DB", self.home.join("system-config.db"))
            .env("LIBRA_TEST", "1")
            .env("LANG", "C")
            .env("LC_ALL", "C");
        command
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().expect("spawn libra")
    }

    fn success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.run(cwd, args);
        assert!(
            output.status.success(),
            "libra {args:?} failed (status {:?})\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repo(&self, name: &str) -> PathBuf {
        let repo = self.path(name);
        self.success(&self.root, &["init", "--vault", "false", path_str(&repo)]);
        self.success(&repo, &["config", "set", "user.name", "Refspec Test"]);
        self.success(
            &repo,
            &["config", "set", "user.email", "refspec@example.com"],
        );
        repo
    }

    fn commit(&self, repo: &Path, file: &str, body: &str, message: &str) {
        fs::write(repo.join(file), body).expect("write commit fixture");
        self.success(repo, &["add", file]);
        self.success(
            repo,
            &["commit", "--no-gpg-sign", "--no-verify", "-m", message],
        );
    }

    fn source_with_topic(&self, name: &str) -> PathBuf {
        let source = self.init_repo(name);
        self.commit(&source, "main.txt", "main\n", "main");
        self.success(&source, &["switch", "-c", "topic"]);
        self.commit(&source, "topic.txt", "topic\n", "topic");
        self.success(&source, &["switch", "main"]);
        source
    }

    fn add_remote(&self, repo: &Path, name: &str, source: &Path, extra: &[&str]) {
        let mut args = vec!["remote", "add"];
        args.extend_from_slice(extra);
        args.push(name);
        args.push(path_str(source));
        self.success(repo, &args);
    }

    fn has_ref(&self, repo: &Path, reference: &str) -> bool {
        self.run(repo, &["show-ref", "--verify", reference])
            .status
            .success()
    }

    fn ref_oid(&self, repo: &Path, reference: &str) -> String {
        let output = self.success(repo, &["show-ref", "--verify", reference]);
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .expect("show-ref output contains an object ID")
            .to_string()
    }
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("fixture path is UTF-8")
}

#[test]
fn explicit_source_destination_updates_only_the_requested_target() {
    let fixture = Fixture::new();
    let source = fixture.source_with_topic("source-explicit");
    let client = fixture.init_repo("client-explicit");
    fixture.add_remote(&client, "origin", &source, &[]);
    fixture.success(
        &client,
        &[
            "fetch",
            "origin",
            "refs/heads/topic:refs/remotes/origin/review",
        ],
    );
    assert!(fixture.has_ref(&client, "refs/remotes/origin/review"));
    assert!(!fixture.has_ref(&client, "refs/remotes/origin/topic"));
    assert!(!fixture.has_ref(&client, "refs/remotes/origin/main"));
}

#[test]
fn configured_track_and_set_branches_limit_subsequent_fetches() {
    let fixture = Fixture::new();
    let source = fixture.source_with_topic("source-configured");
    let client = fixture.init_repo("client-configured");
    fixture.add_remote(&client, "origin", &source, &["-t", "topic"]);
    fixture.success(&client, &["fetch", "origin"]);
    assert!(fixture.has_ref(&client, "refs/remotes/origin/topic"));
    assert!(!fixture.has_ref(&client, "refs/remotes/origin/main"));
    fixture.success(&client, &["remote", "set-branches", "origin", "main"]);
    fixture.success(&client, &["fetch", "origin"]);
    assert!(fixture.has_ref(&client, "refs/remotes/origin/main"));
}

#[test]
fn remote_update_without_arguments_honors_remotes_default() {
    let fixture = Fixture::new();
    let first = fixture.source_with_topic("source-first");
    let second = fixture.source_with_topic("source-second");
    let client = fixture.init_repo("client-update");
    fixture.add_remote(&client, "first", &first, &[]);
    fixture.add_remote(&client, "second", &second, &[]);
    fixture.success(&client, &["config", "set", "remotes.default", "second"]);
    fixture.success(&client, &["remote", "update"]);
    assert!(!fixture.has_ref(&client, "refs/remotes/first/main"));
    assert!(fixture.has_ref(&client, "refs/remotes/second/main"));
}

#[test]
fn remote_rename_moves_tracking_refs_and_rewrites_fetch_destinations() {
    let fixture = Fixture::new();
    let source = fixture.source_with_topic("source-rename");
    let client = fixture.init_repo("client-rename");
    fixture.add_remote(&client, "origin", &source, &["-t", "topic"]);
    fixture.success(&client, &["fetch", "origin"]);
    assert!(fixture.has_ref(&client, "refs/remotes/origin/topic"));
    fixture.success(&client, &["remote", "rename", "origin", "upstream"]);
    assert!(!fixture.has_ref(&client, "refs/remotes/origin/topic"));
    assert!(fixture.has_ref(&client, "refs/remotes/upstream/topic"));
    let config = fixture.success(&client, &["config", "--get-all", "remote.upstream.fetch"]);
    assert_eq!(
        String::from_utf8_lossy(&config.stdout).trim(),
        "+refs/heads/topic:refs/remotes/upstream/topic"
    );
}

#[test]
fn up_to_date_fetch_keeps_fetch_head_symref_and_orig_head_contracts() {
    let fixture = Fixture::new();
    let source = fixture.source_with_topic("source-metadata");
    let client = fixture.init_repo("client-metadata");
    fixture.add_remote(&client, "origin", &source, &[]);
    fixture.success(&client, &["fetch", "origin", "main"]);
    fixture.success(&client, &["fetch", "origin", "main"]);
    let fetch_head = fs::read_to_string(client.join(".libra/FETCH_HEAD"))
        .expect("up-to-date fetch still writes FETCH_HEAD");
    assert!(fetch_head.contains("branch 'main'"), "{fetch_head:?}");
    assert!(!client.join(".libra/ORIG_HEAD").exists());
    let symref = fixture.success(&client, &["ls-remote", "--symref", "origin", "HEAD"]);
    let stdout = String::from_utf8_lossy(&symref.stdout);
    assert!(
        stdout.contains("ref: refs/heads/main\tHEAD"),
        "missing symbolic HEAD line: {stdout}"
    );
    assert!(fixture.has_ref(&client, "refs/remotes/origin/HEAD"));
}

#[test]
fn multi_ref_non_fast_forward_failure_rolls_back_earlier_updates() {
    let fixture = Fixture::new();
    let source = fixture.source_with_topic("source-atomic");
    let client = fixture.init_repo("client-atomic");
    fixture.add_remote(&client, "origin", &source, &[]);
    fixture.success(&client, &["fetch", "origin"]);

    let main_before = fixture.ref_oid(&client, "refs/remotes/origin/main");
    let topic_before = fixture.ref_oid(&client, "refs/remotes/origin/topic");

    fixture.success(&source, &["switch", "main"]);
    fixture.commit(&source, "main-2.txt", "main 2\n", "advance main");
    let advanced_main = fixture.ref_oid(&source, "refs/heads/main");
    fixture.success(&source, &["switch", "topic"]);
    fixture.success(&source, &["reset", "--hard", &advanced_main]);

    fixture.success(
        &client,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "refs/heads/main:refs/remotes/origin/main",
        ],
    );
    fixture.success(
        &client,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "refs/heads/topic:refs/remotes/origin/topic",
        ],
    );

    let failed = fixture.run(&client, &["fetch", "origin"]);
    assert!(!failed.status.success(), "non-fast-forward fetch must fail");
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("non-fast-forward"),
        "unexpected fetch error: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(
        fixture.ref_oid(&client, "refs/remotes/origin/main"),
        main_before,
        "the earlier fast-forward update must roll back"
    );
    assert_eq!(
        fixture.ref_oid(&client, "refs/remotes/origin/topic"),
        topic_before,
        "the rejected ref must stay unchanged"
    );
}
