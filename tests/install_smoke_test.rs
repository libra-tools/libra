//! CI carrier for the installer verification smoke (plan-20260821 A1-05).
//!
//! Drives `tests/data/install-smoke/run.sh`: twenty-four scenarios covering the
//! signed stable channel (verified install, tampered signature and tampered
//! payload, sha256 and size mismatches, expired / paused / revoked policy
//! branches, the stale-replay anti-downgrade floor, zero-size and
//! min_key_generation and key-validity-window policy rejections, the
//! non-canonical-serialization gate, the manifest-404 and
//! verifier-unavailable transition states, each with and without
//! `LIBRA_ALLOW_FALLBACK=1`). The harness
//! rewrites trust markers only in a COPY of the production installer and
//! asserts the production file stays byte-identical with zero runtime
//! key-override entry points.

use std::process::Command;

fn tool_available(tool: &str, arg: &str) -> bool {
    Command::new(tool)
        .arg(arg)
        .output()
        .map(|out| out.status.success())
        .is_ok_and(|ok| ok)
}

#[test]
fn install_sh_smoke_scenarios() {
    for (tool, arg) in [
        ("bash", "--version"),
        ("python3", "--version"),
        ("openssl", "version"),
    ] {
        if !tool_available(tool, arg) {
            eprintln!("skipped (install-smoke needs {tool} on PATH)");
            return;
        }
    }
    let harness = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/install-smoke/run.sh"
    );
    let output = Command::new("bash")
        .arg(harness)
        .output()
        .expect("install-smoke harness must spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "install-smoke harness failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("all 24 scenarios passed"),
        "harness did not report full coverage:\n{stdout}"
    );
}

#[test]
fn install_ps1_smoke_scenarios() {
    // The PowerShell smoke needs pwsh; environments without it skip with a
    // notice (the gap is registered in plan-20260821 A1-05).
    if !tool_available("pwsh", "-Version") {
        eprintln!("skipped (set up pwsh to run the install.ps1 smoke)");
        return;
    }
    let harness = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/install-smoke/run.ps1"
    );
    let output = Command::new("pwsh")
        .args(["-NoProfile", "-File", harness])
        .output()
        .expect("install.ps1 smoke harness must spawn");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "install.ps1 smoke failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}
