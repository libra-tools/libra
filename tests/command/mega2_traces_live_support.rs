//! Shared Mega2 traces live-fixture gate (LTR-10, later cards append).
//!
//! A selected live case fails when the fixture is absent. It does not skip
//! and it does not substitute a stub server. Secret values stay out of
//! assertions and logs.

#![cfg(unix)]

use std::{env, path::PathBuf};

/// Environment names required by GC-LTR-LIVE. Values are checked for
/// presence only.
pub const LIVE_KEYS: &[&str] = &[
    "LIBRA_TRACES_LIVE_ENABLED",
    "LIBRA_TRACES_LIVE_HTTP_URL",
    "LIBRA_TRACES_LIVE_SSH_URI",
    "LIBRA_TRACES_LIVE_TOKEN_FILE",
    "LIBRA_TRACES_LIVE_PEER_TOKEN_FILE",
    "LIBRA_TRACES_LIVE_SSH_KEY_FILE",
    "LIBRA_TRACES_LIVE_PEER_SSH_KEY_FILE",
    "LIBRA_TRACES_LIVE_TARGET",
    "LIBRA_TRACES_LIVE_OTHER_TARGET",
    "LIBRA_TRACES_LIVE_SERVER_ID",
    "LIBRA_TRACES_LIVE_CLIENT_BINARY_SHA256",
    "LIBRA_TRACES_LIVE_CONTRACT_SHA256",
];

#[derive(Debug)]
pub struct LiveFixture {
    pub http_url: String,
    pub ssh_uri: String,
    pub token_file: PathBuf,
    pub peer_token_file: PathBuf,
    pub ssh_key_file: PathBuf,
    pub peer_ssh_key_file: PathBuf,
    pub target: String,
    pub other_target: String,
    pub server_id: String,
    pub contract_sha256: String,
    pub client_binary_sha256: String,
}

impl LiveFixture {
    /// Load the real fixture. Missing or empty input is an error, never a skip.
    pub fn require() -> Result<Self, String> {
        let mut values = Vec::with_capacity(LIVE_KEYS.len());
        for key in LIVE_KEYS {
            match env::var(key) {
                Ok(value) if !value.is_empty() => values.push(value),
                _ => {
                    return Err(format!(
                        "{key} is missing or empty; this live case cannot skip or use a stub"
                    ));
                }
            }
        }
        if values[0] != "1" {
            return Err(
                "LIBRA_TRACES_LIVE_ENABLED must be 1; refusing to skip the live case".to_string(),
            );
        }
        let ssh_uri = values[2].clone();
        if !ssh_uri.starts_with("ssh://traces@") {
            return Err(
                "LIBRA_TRACES_LIVE_SSH_URI must use the reserved traces login (ssh://traces@...)"
                    .to_string(),
            );
        }
        Ok(Self {
            http_url: values[1].clone(),
            ssh_uri,
            token_file: PathBuf::from(&values[3]),
            peer_token_file: PathBuf::from(&values[4]),
            ssh_key_file: PathBuf::from(&values[5]),
            peer_ssh_key_file: PathBuf::from(&values[6]),
            target: values[7].clone(),
            other_target: values[8].clone(),
            server_id: values[9].clone(),
            client_binary_sha256: values[10].clone(),
            contract_sha256: values[11].clone(),
        })
    }
}
