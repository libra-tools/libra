//! `tests/compat/installer_https_examples_guard.rs` — installer surface contract
//! ensuring every user-facing `curl ... install.sh` example in `install.sh`
//! uses the canonical HTTPS URL.

use std::{fs, path::PathBuf};

fn installer_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh")
}

#[test]
fn installer_examples_use_canonical_https_url() {
    let path = installer_path();
    let body = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read installer {}: {err}", path.display()));

    let install_lines: Vec<&str> = body
        .lines()
        .filter(|line| line.contains("curl -fsSL") && line.contains("install.sh"))
        .collect();

    assert!(
        !install_lines.is_empty(),
        "install.sh should keep at least one canonical curl example so the guard has coverage"
    );

    for line in install_lines {
        assert!(
            line.contains("https://libra.tools/install.sh"),
            "installer curl examples must use the canonical HTTPS URL; offending line: {line}"
        );
    }
}
