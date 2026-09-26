//! Integration tests for `libra credential` (vault-backed Git credential helper).
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::process::Output;

use tempfile::{TempDir, tempdir};

use super::{
    run_libra_command, run_libra_command_with_stdin, run_libra_command_with_stdin_and_env,
};

fn init_repo() -> TempDir {
    let repo = tempdir().unwrap();
    assert!(
        run_libra_command(&["init"], repo.path()).status.success(),
        "init must succeed (and create the vault)"
    );
    repo
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

const STORE_INPUT: &str =
    "protocol=https\nhost=example.com\nusername=alice\npassword=s3cr3t-token\n\n";
const FILL_INPUT: &str = "protocol=https\nhost=example.com\n\n";

#[test]
fn credential_store_then_fill_round_trip() {
    let repo = init_repo();
    let store = run_libra_command_with_stdin(&["credential", "store"], repo.path(), STORE_INPUT);
    assert_eq!(
        store.status.code(),
        Some(0),
        "store failed: {}",
        stderr(&store)
    );

    let fill = run_libra_command_with_stdin(&["credential", "fill"], repo.path(), FILL_INPUT);
    assert_eq!(
        fill.status.code(),
        Some(0),
        "fill failed: {}",
        stderr(&fill)
    );
    let out = stdout(&fill);
    assert!(out.contains("username=alice"), "fill output: {out}");
    assert!(out.contains("password=s3cr3t-token"), "fill output: {out}");
}

#[test]
fn credential_fill_unknown_host_is_empty_and_exit_0() {
    let repo = init_repo();
    // No stored credential at all.
    let fill = run_libra_command_with_stdin(
        &["credential", "fill"],
        repo.path(),
        "protocol=https\nhost=nobody.example\n\n",
    );
    assert_eq!(
        fill.status.code(),
        Some(0),
        "a miss must exit 0 (no side channel): {}",
        stderr(&fill)
    );
    assert!(
        stdout(&fill).trim().is_empty(),
        "a miss must print nothing: {:?}",
        stdout(&fill)
    );
}

#[test]
fn credential_erase_removes_entry() {
    let repo = init_repo();
    run_libra_command_with_stdin(&["credential", "store"], repo.path(), STORE_INPUT);
    let erase = run_libra_command_with_stdin(&["credential", "erase"], repo.path(), FILL_INPUT);
    assert_eq!(erase.status.code(), Some(0));
    let fill = run_libra_command_with_stdin(&["credential", "fill"], repo.path(), FILL_INPUT);
    assert!(
        stdout(&fill).trim().is_empty(),
        "after erase, fill must be empty: {:?}",
        stdout(&fill)
    );
}

#[test]
fn credential_fill_username_mismatch_is_empty() {
    let repo = init_repo();
    run_libra_command_with_stdin(&["credential", "store"], repo.path(), STORE_INPUT);
    // Stored username is alice; ask for bob.
    let fill = run_libra_command_with_stdin(
        &["credential", "fill"],
        repo.path(),
        "protocol=https\nhost=example.com\nusername=bob\n\n",
    );
    assert_eq!(fill.status.code(), Some(0));
    assert!(
        stdout(&fill).trim().is_empty(),
        "a username mismatch must be a miss: {:?}",
        stdout(&fill)
    );
}

#[test]
fn credential_security_store_rejects_expired_timestamp() {
    let repo = init_repo();
    // password_expiry_utc in the distant past.
    let input = "protocol=https\nhost=example.com\nusername=alice\npassword=s3cr3t-token\npassword_expiry_utc=100\n\n";
    let store = run_libra_command_with_stdin(&["credential", "store"], repo.path(), input);
    assert_eq!(
        store.status.code(),
        Some(128),
        "storing an already-expired credential must fail"
    );
    // And the error must not echo the password.
    assert!(
        !stderr(&store).contains("s3cr3t-token"),
        "error must not leak the password: {}",
        stderr(&store)
    );
}

#[test]
fn credential_security_store_without_password_errors_without_leaking() {
    let repo = init_repo();
    let input = "protocol=https\nhost=example.com\nusername=alice\n\n";
    let store = run_libra_command_with_stdin(&["credential", "store"], repo.path(), input);
    assert_eq!(store.status.code(), Some(128));
    // The error references the host but never a secret.
    let err = stderr(&store);
    assert!(
        err.contains("example.com"),
        "error should name the host: {err}"
    );
}

#[test]
fn credential_security_no_password_in_debug_logs() {
    let repo = init_repo();
    // Store and fill with debug logging on; the password must never appear on
    // stderr (the log sink). On fill it is legitimately on stdout only.
    let store = run_libra_command_with_stdin_and_env(
        &["credential", "store"],
        repo.path(),
        STORE_INPUT,
        &[("RUST_LOG", "debug"), ("LIBRA_LOG", "debug")],
    );
    assert!(
        !stderr(&store).contains("s3cr3t-token"),
        "store must not log the password: {}",
        stderr(&store)
    );
    let fill = run_libra_command_with_stdin_and_env(
        &["credential", "fill"],
        repo.path(),
        FILL_INPUT,
        &[("RUST_LOG", "debug"), ("LIBRA_LOG", "debug")],
    );
    assert!(
        !stderr(&fill).contains("s3cr3t-token"),
        "fill must not log the password to the trace sink: {}",
        stderr(&fill)
    );
}

/// Replicate `credential::credential_key` so the test can target the stored
/// entry directly.
fn credential_config_key(protocol: &str, host: &str, path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"libra-credential-v1\0");
    hasher.update(protocol.as_bytes());
    hasher.update(b"\0");
    hasher.update(host.as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    format!("credential.{}", hex::encode(hasher.finalize()))
}

/// A credential that no longer decrypts (e.g. the vault unseal key was rotated)
/// must be a clean miss, not an error or a stale credential.
#[test]
fn credential_security_decrypt_failure_is_a_miss() {
    let repo = init_repo();
    run_libra_command_with_stdin(&["credential", "store"], repo.path(), STORE_INPUT);
    // Overwrite the stored (encrypted) value with undecryptable bytes.
    let key = credential_config_key("https", "example.com", "");
    let set = run_libra_command(&["config", &key, "00"], repo.path());
    assert!(set.status.success(), "config set failed: {}", stderr(&set));

    let fill = run_libra_command_with_stdin(&["credential", "fill"], repo.path(), FILL_INPUT);
    assert_eq!(
        fill.status.code(),
        Some(0),
        "an undecryptable entry must be a miss, not an error: {}",
        stderr(&fill)
    );
    assert!(
        stdout(&fill).trim().is_empty(),
        "an undecryptable entry must print nothing: {:?}",
        stdout(&fill)
    );
}

#[test]
fn credential_fill_outside_repository_is_empty() {
    // No repo / no vault: fill is still a clean miss, never an error.
    let dir = tempdir().unwrap();
    let fill = run_libra_command_with_stdin(&["credential", "fill"], dir.path(), FILL_INPUT);
    assert_eq!(
        fill.status.code(),
        Some(0),
        "fill without a vault must exit 0: {}",
        stderr(&fill)
    );
    assert!(stdout(&fill).trim().is_empty());
}

#[test]
fn credential_get_is_fill_alias_and_unknown_op_silent() {
    let repo = init_repo();

    // `get` is the Git helper protocol operation for a fill, aliased to `fill`.
    let store = run_libra_command_with_stdin(&["credential", "store"], repo.path(), STORE_INPUT);
    assert!(store.status.success(), "store: {}", stderr(&store));
    let get = run_libra_command_with_stdin(&["credential", "get"], repo.path(), FILL_INPUT);
    assert!(get.status.success(), "get: {}", stderr(&get));
    let out = stdout(&get);
    assert!(
        out.contains("username=alice"),
        "get should emit username: {out:?}"
    );
    assert!(
        out.contains("password=s3cr3t-token"),
        "get should emit password: {out:?}"
    );

    // An unknown operation is silently ignored (Git helper convention), exit 0.
    let unknown = run_libra_command_with_stdin(&["credential", "reject"], repo.path(), FILL_INPUT);
    assert!(
        unknown.status.success(),
        "unknown helper op must be silently ignored, got: {}",
        stderr(&unknown)
    );
    assert!(stdout(&unknown).is_empty(), "unknown op produces no output");
}

#[test]
fn credential_outside_repository_roundtrip() {
    // A non-repository working directory: the helper must still store/fill/erase
    // via the user-level (global) store (HP-14).
    let outside = tempdir().unwrap();

    let store = run_libra_command_with_stdin(&["credential", "store"], outside.path(), STORE_INPUT);
    assert!(
        store.status.success(),
        "store outside repo: {}",
        stderr(&store)
    );

    let get = run_libra_command_with_stdin(&["credential", "get"], outside.path(), FILL_INPUT);
    assert!(get.status.success(), "get outside repo: {}", stderr(&get));
    let out = stdout(&get);
    assert!(
        out.contains("username=alice"),
        "get outside repo emits username: {out:?}"
    );
    assert!(
        out.contains("password=s3cr3t-token"),
        "get outside repo emits password: {out:?}"
    );

    let erase = run_libra_command_with_stdin(&["credential", "erase"], outside.path(), FILL_INPUT);
    assert!(
        erase.status.success(),
        "erase outside repo: {}",
        stderr(&erase)
    );

    let get2 = run_libra_command_with_stdin(&["credential", "get"], outside.path(), FILL_INPUT);
    assert!(get2.status.success(), "get after erase: {}", stderr(&get2));
    assert!(
        !stdout(&get2).contains("username=alice"),
        "erased credential should no longer fill"
    );
}
