//! Large-set regression for default rename detection.
//!
//! Layer: L1 (deterministic; tempdir only, no network).

use std::fs;

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

const LARGE_SET: usize = 1001;

#[test]
fn diff_large_set_warns_and_preserves_exact_renames() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    let exact_old = root.join("exact-old");
    let exact_new = root.join("exact-new");
    let inexact_old = root.join("inexact-old");
    let inexact_new = root.join("inexact-new");
    for dir in [&exact_old, &exact_new, &inexact_old, &inexact_new] {
        fs::create_dir_all(dir).expect("create large-set fixture directory");
    }

    for index in 0..LARGE_SET {
        fs::write(
            exact_old.join(format!("{index:04}.txt")),
            format!("exact-{index}\n"),
        )
        .expect("write exact source");
        fs::write(
            inexact_old.join(format!("{index:04}.txt")),
            format!("old-{index}\n"),
        )
        .expect("write inexact source");
    }
    assert_cli_success(&run_libra_command(&["add", "-A"], root), "stage base set");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "large base", "--no-verify"], root),
        "commit base set",
    );

    fs::remove_dir_all(&exact_old).expect("remove exact source directory");
    fs::remove_dir_all(&inexact_old).expect("remove inexact source directory");
    for index in 0..LARGE_SET {
        fs::write(
            exact_new.join(format!("{index:04}.txt")),
            format!("exact-{index}\n"),
        )
        .expect("write exact destination");
        fs::write(
            inexact_new.join(format!("{index:04}.txt")),
            format!("new-{index}\n"),
        )
        .expect("write inexact destination");
    }
    assert_cli_success(
        &run_libra_command(&["add", "-A"], root),
        "stage large rename set",
    );

    let output = run_libra_command(&["diff", "--staged", "--summary"], root);
    assert_cli_success(&output, "diff large rename set");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("skipped inexact rename detection"),
        "missing rename-limit warning: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches(" rename ").count(),
        LARGE_SET,
        "all exact renames must survive the limit"
    );
    assert!(stdout.contains("exact-old") && stdout.contains("exact-new"));
}
