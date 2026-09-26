//! GC-HP-03 guard (issues/480 HP-07): a local push must not switch the process
//! working directory. The target is opened by path (path-addressed storage), so
//! pushing from any cwd leaves cwd untouched.

use std::process::Command;

fn libra(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(cwd)
        .args(args)
        .env("HOME", cwd.join(".h"))
        .env("XDG_CONFIG_HOME", cwd.join(".h/.config"))
        .env("LIBRA_TEST", "1")
        .output()
        .expect("spawn libra")
}

#[test]
fn local_push_does_not_switch_cwd() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&target).unwrap();

    assert!(libra(&source, &["init"]).status.success(), "init source");
    assert!(
        libra(&source, &["config", "user.name", "T"])
            .status
            .success(),
        "user.name"
    );
    assert!(
        libra(&source, &["config", "user.email", "t@t"])
            .status
            .success(),
        "user.email"
    );
    std::fs::write(source.join("f.txt"), "x").unwrap();
    assert!(libra(&source, &["add", "f.txt"]).status.success(), "add");
    assert!(
        libra(&source, &["commit", "-m", "c", "--no-verify"])
            .status
            .success(),
        "commit"
    );
    assert!(
        libra(&target, &["init", "--bare"]).status.success(),
        "init target"
    );

    let branch = String::from_utf8(libra(&source, &["branch", "--show-current"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let cwd_before = std::env::current_dir().unwrap();

    // Push to the target by path from the source cwd.
    assert!(
        libra(&source, &["push", target.to_str().unwrap(), &branch])
            .status
            .success(),
        "push to local target"
    );

    // GC-HP-03: cwd must be unchanged (no set_current_dir in the push path).
    assert_eq!(
        std::env::current_dir().unwrap(),
        cwd_before,
        "local push must not switch the process working directory"
    );
}
