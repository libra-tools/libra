use super::*;

fn error_log_repo(fixture: &Fixture, name: &str) -> PathBuf {
    let repo = fixture.path(name);
    fixture.init_repo(&repo);
    fixture.commit_file(&repo, "tracked.txt", "content\n", "subject");
    repo
}

#[test]
fn invalid_log_defaults_fail_closed_before_output() {
    let fixture = Fixture::new();
    let repo = error_log_repo(&fixture, "log-config-invalid");

    for (key, value) in [
        ("format.pretty", ""),
        ("format.pretty", "not-a-pretty-format"),
        ("log.date", "not-a-date-mode"),
        ("log.follow", "sometimes"),
    ] {
        fixture.success(&repo, &["config", key, value]);
        let rejected = fixture.run(&repo, &["log", "-1"]);
        assert_eq!(rejected.status.code(), Some(129), "{key}");
        assert!(rejected.stdout.is_empty(), "{key}");
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{key}: {stderr}");
        assert!(stderr.contains(key), "{key}: {stderr}");
        fixture.success(&repo, &["config", "--unset", key]);
    }
}

#[test]
fn invalid_cli_date_modes_fail_closed_for_log_and_show() {
    let fixture = Fixture::new();
    let repo = error_log_repo(&fixture, "log-cli-date-invalid");

    for args in [
        &["log", "--date=not-a-date-mode", "-1"][..],
        &["show", "--date=not-a-date-mode", "--no-patch", "HEAD"][..],
    ] {
        let rejected = fixture.run(&repo, args);
        assert_eq!(rejected.status.code(), Some(129), "{args:?}");
        assert!(rejected.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains("LBR-CLI-002"), "{args:?}: {stderr}");
        assert!(stderr.contains("--date"), "{args:?}: {stderr}");
    }
}

#[test]
fn log_default_read_failure_is_io_error_before_output() {
    let fixture = Fixture::new();
    let repo = error_log_repo(&fixture, "log-config-read-failure");
    fs::create_dir_all(fixture.home.join(".libra").join("config.db"))
        .expect("replace global config database with a directory");

    let rejected = fixture.run(&repo, &["log", "-1"]);
    assert_eq!(rejected.status.code(), Some(128));
    assert!(rejected.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
    assert!(stderr.contains("format.pretty"), "{stderr}");
}
