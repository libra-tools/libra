//! Guards release workflow rclone environment variables: rclone interprets
//! bare `RCLONE_*` variables as global CLI options, so release-owned constants
//! must use `LIBRA_RCLONE_*`. The configured R2 remote is the sole allowed
//! `RCLONE_CONFIG_<remote>_*` namespace in this workflow. YAML mappings, POSIX
//! shell assignments/exports, and PowerShell `$env:` assignments are guarded.

use std::fs;

use regex::Regex;

fn bare_rclone_variables(input: &str) -> Vec<&str> {
    let rclone_assignment =
        Regex::new(r#"(?m)(?:^|[^A-Za-z0-9_])(?P<name>RCLONE_[A-Z0-9_]+)["']?[ \t]*(?:=|:)"#)
            .expect("rclone environment-variable regex must compile");

    rclone_assignment
        .captures_iter(input)
        .filter_map(|captures| captures.name("name").map(|name| name.as_str()))
        .filter(|name| !name.starts_with("RCLONE_CONFIG_R2_"))
        .collect()
}

#[test]
fn release_workflow_uses_no_rclone_and_no_long_term_r2_credentials() {
    // plan-20260821 A1-09: uploads go through the Backend credential broker
    // (per-object presigned PUT URLs); rclone and the long-term secrets.R2_*
    // credentials are gone from the workflow entirely (GC-UP01-1 forbids
    // ever bringing the long-term credentials back).
    let workflow = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/release.yml"
    ))
    .expect("read release workflow");

    assert!(
        !workflow.to_lowercase().contains("rclone"),
        "release.yml must not reference rclone: uploads go through the broker"
    );
    assert!(
        !workflow.contains("secrets.R2_"),
        "release.yml must not reference long-term R2 credentials (GC-UP01-1)"
    );
    // If rclone ever returns, the bare-variable rule below applies again.
    let bare_rclone_variables = bare_rclone_variables(&workflow);
    assert!(
        bare_rclone_variables.is_empty(),
        "rclone treats bare RCLONE_* environment variables as CLI options: {bare_rclone_variables:?}"
    );
}

#[test]
fn rclone_environment_guard_catches_yaml_posix_and_powershell_assignments() {
    let assignments = r#"
RCLONE_VERSION: 1.75.0
export RCLONE_BWLIMIT=1M
RCLONE_TIMEOUT=30s rclone copy source target
(RCLONE_BUFFER_SIZE=1M)
$env:RCLONE_RETRIES = 2
$Env:RCLONE_LOW_LEVEL_RETRIES = 3
$ENV:RCLONE_TPSLIMIT=4
RCLONE_CONFIG_R2_TYPE: s3
flow: { RCLONE_TRANSFERS: 4, RCLONE_CONFIG_R2_PROVIDER: Cloudflare }
"RCLONE_CHECKERS": 8
'RCLONE_RETRIES': 3
"#;

    assert_eq!(
        bare_rclone_variables(assignments),
        [
            "RCLONE_VERSION",
            "RCLONE_BWLIMIT",
            "RCLONE_TIMEOUT",
            "RCLONE_BUFFER_SIZE",
            "RCLONE_RETRIES",
            "RCLONE_LOW_LEVEL_RETRIES",
            "RCLONE_TPSLIMIT",
            "RCLONE_TRANSFERS",
            "RCLONE_CHECKERS",
            "RCLONE_RETRIES",
        ]
    );
}
