//! IX-01 installer alias contract.
//!
//! The shell scenario drives the complete installer with an isolated HOME and
//! a deterministic downloader, so it covers both a fresh install and the
//! same-version early-return path without contacting the release service.

use std::{path::PathBuf, process::Command};

#[cfg(unix)]
#[test]
fn installer_creates_and_repairs_safe_optional_lba_alias() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = repo.join("tests/compat/install_alias_smoke.sh");
    let output = Command::new("sh")
        .arg(&script)
        .arg(&repo)
        .output()
        .expect("run POSIX install alias smoke script");

    assert!(
        output.status.success(),
        "install alias smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("install alias smoke: ok"),
        "smoke script did not print its completion marker\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(not(unix))]
#[test]
fn installer_alias_smoke_is_unix_only() {
    eprintln!("skipped: install.sh supports Linux and macOS");
}

/// plan-20260714 PD-10: the Windows installer ships the same optional `lba`
/// shorthand as the POSIX one, under the same safety contract.
///
/// The card's runtime criterion (`lba --version` == `libra --version` on
/// Windows) needs a Windows host, so it is not asserted here. What IS
/// asserted is the contract that governs whether the shim may be written at
/// all — the part that can silently clobber an unrelated `lba` on a user's
/// PATH — by reading the installer source. That is platform-independent and
/// so runs everywhere, rather than being skipped on the machines where the
/// installer is actually maintained.
#[test]
fn windows_installer_declares_the_safe_optional_lba_alias() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(repo.join("install.ps1"))
        .expect("install.ps1 is tracked in the repository");

    // Opt-out, both spellings, matching install.sh's --no-alias /
    // LIBRA_NO_ALIAS.
    assert!(
        script.contains("$NoAlias"),
        "install.ps1 must expose a -NoAlias switch"
    );
    assert!(
        script.contains("$env:LIBRA_NO_ALIAS"),
        "install.ps1 must honour LIBRA_NO_ALIAS like the POSIX installer"
    );
    assert!(
        script.contains("$env:LIBRA_NO_ALIAS -eq \"1\""),
        "install.ps1 must disable the alias only for LIBRA_NO_ALIAS=1, matching install.sh"
    );

    // A marker is what makes a reinstall idempotent AND makes a foreign file
    // recognisable; without it the installer cannot tell its own shim from
    // somebody else's `lba`.
    assert!(
        script.contains("$ShimMarker"),
        "install.ps1 must mark the shims it writes"
    );

    // Every spelling Windows would resolve instead of our `.cmd` has to be
    // checked, or the guard is decorative.
    for ext in ["\".exe\"", "\".bat\"", "\".ps1\""] {
        assert!(
            script.contains(ext),
            "install.ps1 must refuse to shadow an existing lba{ext}"
        );
    }
    assert!(
        script.contains("was not installed by libra"),
        "install.ps1 must decline, not overwrite, a foreign lba.*"
    );
    assert!(
        script.contains("lba.cmd"),
        "install.ps1 must create the lba.cmd shim"
    );
}
