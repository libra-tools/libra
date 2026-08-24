//! Global resource-limit configuration (lore.md §0.9).
//!
//! Bounds fan-out so a large repository or CI run cannot exhaust host resources.
//! The concrete, wired limit today is the maximum number of concurrent remote
//! connections/requests (`--max-connections` / `LIBRA_MAX_CONNECTIONS`),
//! consumed by `crate::utils::storage::remote::RemoteStorage::exist_batch` (and
//! any future bounded remote fan-out). File-count/size, thread, and search
//! limits are documented follow-ons.
//!
//! Set once at CLI dispatch (flag > env > default). No-op for purely local
//! operations that never open a remote connection.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Default concurrent remote-connection cap when nothing is configured.
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;

static MAX_CONNECTIONS: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_CONNECTIONS);

/// Set the maximum number of concurrent remote connections. A value of `0` is
/// meaningless (no progress) and is clamped up to `1`.
pub fn set_max_connections(limit: usize) {
    MAX_CONNECTIONS.store(limit.max(1), Ordering::Relaxed);
}

/// The current maximum concurrent-remote-connection cap
/// (defaults to [`DEFAULT_MAX_CONNECTIONS`]).
pub fn max_connections() -> usize {
    MAX_CONNECTIONS.load(Ordering::Relaxed)
}

/// Resolve a `--max-connections` value from the `LIBRA_MAX_CONNECTIONS`
/// environment variable. `Ok(None)` when unset/empty (keep the current default);
/// `Ok(Some(n))` for a positive integer.
///
/// # Errors
/// A present-but-invalid value (non-positive, non-numeric, or non-UTF-8) is an
/// error, so the CLI can fail fast rather than silently ignoring a misconfigured
/// limit.
pub fn max_connections_from_env() -> Result<Option<usize>, String> {
    match std::env::var("LIBRA_MAX_CONNECTIONS") {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|limit| *limit > 0)
            .map(Some)
            .ok_or_else(|| {
                format!("invalid LIBRA_MAX_CONNECTIONS '{value}' (expected a positive integer)")
            }),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("LIBRA_MAX_CONNECTIONS contains invalid (non-UTF-8) bytes".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    #[serial]
    fn max_connections_round_trips_and_clamps_zero() {
        let previous = max_connections();
        set_max_connections(4);
        assert_eq!(max_connections(), 4);
        // 0 clamps to 1 (some concurrency is always needed to make progress).
        set_max_connections(0);
        assert_eq!(max_connections(), 1);
        set_max_connections(previous);
    }

    #[test]
    #[serial]
    fn env_parse_accepts_positive_and_rejects_invalid() {
        let previous = std::env::var_os("LIBRA_MAX_CONNECTIONS");
        // SAFETY: single-threaded under #[serial]; restored below.
        unsafe {
            std::env::set_var("LIBRA_MAX_CONNECTIONS", "8");
            assert_eq!(max_connections_from_env(), Ok(Some(8)));
            std::env::set_var("LIBRA_MAX_CONNECTIONS", "0");
            assert!(max_connections_from_env().is_err(), "0 is invalid");
            std::env::set_var("LIBRA_MAX_CONNECTIONS", "bogus");
            assert!(
                max_connections_from_env().is_err(),
                "non-numeric is invalid"
            );
            std::env::set_var("LIBRA_MAX_CONNECTIONS", "");
            assert_eq!(max_connections_from_env(), Ok(None), "empty → None");
            std::env::remove_var("LIBRA_MAX_CONNECTIONS");
            assert_eq!(max_connections_from_env(), Ok(None), "unset → None");
            match &previous {
                Some(value) => std::env::set_var("LIBRA_MAX_CONNECTIONS", value),
                None => std::env::remove_var("LIBRA_MAX_CONNECTIONS"),
            }
        }
    }
}
