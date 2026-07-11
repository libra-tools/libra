use super::*;

fn follow_subjects(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn renamed_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "old.txt", "same blob\n", "add old path");
    fs::rename(repo.join("old.txt"), repo.join("new.txt")).expect("rename fixture path");
    fixture.success(&repo, &["add", "-A"]);
    fixture.success(
        &repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "rename path",
        ],
    );
    repo
}

#[test]
fn log_follow_config_and_explicit_flag_carry_historical_paths() {
    let fixture = Fixture::new();
    let repo = renamed_repo(&fixture, "log-follow-default");

    let without_follow =
        follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "--no-follow", "new.txt"]));
    assert_eq!(without_follow, ["rename path"]);

    let explicit =
        follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "--follow", "new.txt"]));
    assert_eq!(explicit, ["rename path", "add old path"]);
    let reversed = follow_subjects(&fixture.success(
        &repo,
        &["log", "--format=%s", "--reverse", "--follow", "new.txt"],
    ));
    assert_eq!(reversed, ["add old path", "rename path"]);

    fixture.success(&repo, &["config", "--global", "log.follow", "true"]);
    fixture.success(&repo, &["config", "log.follow", "false"]);
    let local_false = follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "new.txt"]));
    assert_eq!(local_false, without_follow);
    fixture.success(&repo, &["config", "--unset", "log.follow"]);

    let configured = follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "new.txt"]));
    assert_eq!(configured, ["rename path", "add old path"]);

    let names = String::from_utf8_lossy(
        &fixture
            .success(&repo, &["log", "--format=%s", "--name-status", "new.txt"])
            .stdout,
    )
    .into_owned();
    assert!(names.contains("A\tnew.txt"), "{names}");
    assert!(names.contains("D\told.txt"), "{names}");
    assert!(names.matches("A\told.txt").count() == 1, "{names}");

    let stats = String::from_utf8_lossy(
        &fixture
            .success(&repo, &["log", "--format=%s", "--stat", "new.txt"])
            .stdout,
    )
    .into_owned();
    assert!(stats.contains("old.txt"), "{stats}");

    let patch = String::from_utf8_lossy(
        &fixture
            .success(&repo, &["log", "--format=%s", "--patch", "new.txt"])
            .stdout,
    )
    .into_owned();
    assert!(patch.contains("diff --git a/old.txt b/old.txt"), "{patch}");

    let quiet = fixture.success(&repo, &["--quiet", "log", "--stat", "--follow", "new.txt"]);
    assert!(quiet.stdout.is_empty(), "quiet stdout: {quiet:?}");

    let json = fixture.success(&repo, &["--json", "log", "new.txt"]);
    let payload: Value = serde_json::from_slice(&json.stdout).expect("parse log JSON");
    let commits = payload["data"]["commits"].as_array().expect("JSON commits");
    assert_eq!(commits[0]["subject"], "rename path");
    assert_eq!(commits[1]["subject"], "add old path");
    assert!(
        commits[1]["files"]
            .as_array()
            .expect("historical files")
            .iter()
            .any(|file| file["path"] == "old.txt" && file["status"] == "added"),
        "{}",
        commits[1]
    );

    let disabled =
        follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "--no-follow", "new.txt"]));
    assert_eq!(disabled, without_follow);

    fixture.commit_file(&repo, "other.txt", "other\n", "add other path");
    let multiple =
        follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "new.txt", "other.txt"]));
    assert_eq!(multiple, ["add other path", "rename path"]);
}

#[test]
fn log_follow_config_normalizes_a_subdirectory_path_to_the_repo_root() {
    let fixture = Fixture::new();
    let repo = fixture.path("log-follow-subdirectory");
    fixture.init_repo(&repo);
    fs::create_dir_all(repo.join("sub")).expect("create fixture subdirectory");
    fixture.commit_file(&repo, "sub/old.txt", "same blob\n", "add nested old path");
    fs::rename(repo.join("sub/old.txt"), repo.join("sub/new.txt"))
        .expect("rename nested fixture path");
    fixture.success(&repo, &["add", "-A"]);
    fixture.success(
        &repo,
        &[
            "commit",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "rename nested path",
        ],
    );
    fixture.success(&repo, &["config", "log.follow", "true"]);

    let output =
        follow_subjects(&fixture.success(&repo.join("sub"), &["log", "--format=%s", "new.txt"]));
    assert_eq!(output, ["rename nested path", "add nested old path"]);
}

#[test]
fn log_follow_does_not_treat_an_unchanged_copy_source_as_a_rename() {
    let fixture = Fixture::new();
    let repo = fixture.path("log-follow-copy");
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "old.txt", "same blob\n", "add copy source");
    fs::copy(repo.join("old.txt"), repo.join("new.txt")).expect("copy fixture path");
    fixture.success(&repo, &["add", "new.txt"]);
    fixture.success(
        &repo,
        &["commit", "--no-gpg-sign", "--no-verify", "-m", "copy path"],
    );
    fixture.success(&repo, &["config", "log.follow", "true"]);

    let output = follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "new.txt"]));
    assert_eq!(output, ["copy path"]);
}

#[test]
fn log_follow_config_keeps_a_single_directory_as_a_directory_filter() {
    let fixture = Fixture::new();
    let repo = fixture.path("log-follow-directory");
    fixture.init_repo(&repo);
    fs::create_dir_all(repo.join("src")).expect("create fixture directory");
    fixture.commit_file(&repo, "src/one.txt", "one\n", "add first source file");
    fixture.commit_file(&repo, "src/two.txt", "two\n", "add second source file");
    fixture.success(&repo, &["config", "log.follow", "true"]);

    let output = follow_subjects(&fixture.success(&repo, &["log", "--format=%s", "src"]));
    assert_eq!(output, ["add second source file", "add first source file"]);
}
