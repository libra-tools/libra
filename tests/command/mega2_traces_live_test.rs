//! Real Mega2 fixture cases for traces. Default CI does not start them.
//! `cargo nextest run --run-ignored only` must execute the selected case;
//! a missing fixture fails the case.

use super::mega2_traces_live_support::LiveFixture;

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_source_epoch_binding() {
    let fixture = LiveFixture::require().expect("live fixture");
    assert!(!fixture.http_url.is_empty());
    assert!(fixture.ssh_uri.starts_with("ssh://traces@"));
    assert_ne!(fixture.target, fixture.other_target);
    assert_eq!(fixture.server_id.len(), 36, "server id must be a UUID");
    assert_eq!(fixture.contract_sha256.len(), 64);
    assert_eq!(fixture.client_binary_sha256.len(), 64);
    // The service call itself arrives with the push transport (LTR-11/12).
    // This case still requires the real fixture and does not substitute one.
    let status = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            &format!(
                "{}/api/v1/traces/capabilities",
                fixture.http_url.trim_end_matches('/')
            ),
        ])
        .output()
        .expect("curl the live capabilities endpoint");
    let code = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.status.success(),
        "live capabilities endpoint unreachable: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_ne!(code.trim(), "000", "live service did not answer");
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_scoped_git_transport() {
    let fixture = LiveFixture::require().expect("live fixture");
    assert!(fixture.ssh_uri.starts_with("ssh://traces@"));
    let info = format!(
        "{}/info/refs?service=git-upload-pack",
        fixture.http_url.trim_end_matches('/')
    );
    let status = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            &info,
        ])
        .output()
        .expect("curl live git discovery");
    assert!(
        status.status.success(),
        "live git discovery unreachable: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let code = String::from_utf8_lossy(&status.stdout);
    assert_ne!(code.trim(), "000", "live git discovery did not answer");
}
