//! Lock error classification does not turn filesystem failures into contention.

use std::{fs::TryLockError, io, path::Path};

use super::lock_error;

#[test]
fn contention_names_the_full_scope_key_path_and_retry_action() {
    // Given main's empty scope suffix, the repository alone is not the full key.
    let key = "repo-id:";
    let path = Path::new("private/info/operation-v2.lock");
    // When the lock API explicitly reports contention.
    let error = lock_error(TryLockError::WouldBlock, key, path);
    // Then its user-facing message preserves both identity coordinates and an action.
    assert_eq!(
        error.to_string(),
        "operation storage failed: operation scope lease is already held for repo-id: at 'private/info/operation-v2.lock'; wait for the other operation to finish, then retry"
    );
}

#[test]
fn genuine_lock_failures_keep_their_cause_and_are_not_reported_as_busy() {
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::Interrupted,
        io::ErrorKind::Unsupported,
    ] {
        // Given a real-error result rather than the API's WouldBlock variant.
        let error = TryLockError::Error(io::Error::new(kind, "filesystem declined lock"));
        // When the result is translated for the operation boundary.
        let message = lock_error(error, "repo-id:linked", Path::new("private/lease")).to_string();
        // Then the cause survives without misleading lock-held/retry guidance.
        assert_eq!(
            message,
            "operation storage failed: cannot lock for repo-id:linked operation scope lease 'private/lease': filesystem declined lock"
        );
        assert!(!message.contains("already held"));
    }
}
