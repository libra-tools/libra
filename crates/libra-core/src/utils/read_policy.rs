//! Process-global object-read policy controlling where the tiered store fetches
//! objects from (lore.md §0.8). Selected by the global `--offline` flag (→
//! [`ReadPolicy::LocalOnly`], overriding the env) and the `LIBRA_READ_POLICY`
//! env var (`auto`/`offline`/`local`/`remote`). The `--local`/`--remote`
//! spellings are env-only because a global flag of those names would collide
//! with `config`/`clone`/`agent` options.
//!
//! Set once at CLI dispatch; consulted by
//! [`crate::utils::storage::tiered::TieredStorage`] on every read. Repos with no
//! configured durable tier (local-only backends) are unaffected — there is no
//! remote to prefer or forbid.

use std::sync::atomic::{AtomicU8, Ordering};

/// How the tiered object store resolves a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPolicy {
    /// Local first, then the durable tier on a miss (the default).
    Auto,
    /// Local only. A needed durable-tier object is a clear error rather than a
    /// network fetch (`--offline` / `LIBRA_READ_POLICY=offline|local`).
    LocalOnly,
    /// Prefer the durable tier — refetch and refresh the local cache even on a
    /// local hit; fall back to the local copy only when the object is absent
    /// remotely (`LIBRA_READ_POLICY=remote`).
    Remote,
}

impl ReadPolicy {
    /// Parse a `LIBRA_READ_POLICY` value (case-insensitive): `auto`/empty →
    /// Auto, `offline`/`local` → LocalOnly, `remote` → Remote. An unrecognized
    /// non-empty value is an **error** rather than a silent fall-through to Auto
    /// — a typo like `offilne` must not quietly re-enable durable-tier reads and
    /// defeat the user's offline intent.
    pub fn parse(value: &str) -> Result<ReadPolicy, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(ReadPolicy::Auto),
            "offline" | "local" => Ok(ReadPolicy::LocalOnly),
            "remote" => Ok(ReadPolicy::Remote),
            other => Err(format!(
                "unrecognized read policy '{other}' (expected one of: auto, offline, local, remote)"
            )),
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            ReadPolicy::Auto => 0,
            ReadPolicy::LocalOnly => 1,
            ReadPolicy::Remote => 2,
        }
    }

    fn from_u8(value: u8) -> ReadPolicy {
        match value {
            1 => ReadPolicy::LocalOnly,
            2 => ReadPolicy::Remote,
            _ => ReadPolicy::Auto,
        }
    }
}

static READ_POLICY: AtomicU8 = AtomicU8::new(0);

/// Set the process-wide read policy (called once from CLI dispatch).
pub fn set_read_policy(policy: ReadPolicy) {
    READ_POLICY.store(policy.to_u8(), Ordering::Relaxed);
}

/// The current process-wide read policy (defaults to [`ReadPolicy::Auto`]).
pub fn read_policy() -> ReadPolicy {
    ReadPolicy::from_u8(READ_POLICY.load(Ordering::Relaxed))
}

/// Resolve the read policy from the `LIBRA_READ_POLICY` environment variable
/// (`auto`/`offline`/`local`/`remote`; unset → [`ReadPolicy::Auto`]). This is
/// the env-driven baseline; the CLI `--offline` flag overrides it to
/// [`ReadPolicy::LocalOnly`].
///
/// # Errors
/// Returns the parse error for an unrecognized non-empty value, so the CLI can
/// fail fast rather than silently defaulting to Auto.
pub fn read_policy_from_env() -> Result<ReadPolicy, String> {
    match std::env::var("LIBRA_READ_POLICY") {
        Ok(value) => ReadPolicy::parse(&value),
        // Only a truly absent variable is Auto. A present-but-non-UTF-8 value is
        // an error, not silently Auto (which would re-enable remote reads).
        Err(std::env::VarError::NotPresent) => Ok(ReadPolicy::Auto),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("LIBRA_READ_POLICY contains invalid (non-UTF-8) bytes".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    // The read policy is a process-global; serialise every test that mutates it
    // (here and in cli/tiered tests) so they cannot observe each other's writes.
    #[test]
    #[serial]
    fn read_policy_round_trips() {
        let previous = read_policy();
        for policy in [ReadPolicy::Auto, ReadPolicy::LocalOnly, ReadPolicy::Remote] {
            set_read_policy(policy);
            assert_eq!(read_policy(), policy);
        }
        set_read_policy(previous);
    }

    #[test]
    fn parse_recognizes_values_and_rejects_typos() {
        assert_eq!(ReadPolicy::parse(""), Ok(ReadPolicy::Auto));
        assert_eq!(ReadPolicy::parse(" Auto "), Ok(ReadPolicy::Auto));
        assert_eq!(ReadPolicy::parse("offline"), Ok(ReadPolicy::LocalOnly));
        assert_eq!(ReadPolicy::parse("LOCAL"), Ok(ReadPolicy::LocalOnly));
        assert_eq!(ReadPolicy::parse("remote"), Ok(ReadPolicy::Remote));
        // A typo is a hard error, not a silent Auto (which would re-enable remote).
        assert!(ReadPolicy::parse("offilne").is_err());
        assert!(ReadPolicy::parse("bogus").is_err());
    }

    /// A present-but-non-UTF-8 `LIBRA_READ_POLICY` must error, not silently map
    /// to Auto (which would re-enable remote reads).
    #[cfg(unix)]
    #[test]
    #[serial(env)]
    fn read_policy_from_env_rejects_non_utf8_value() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let previous = std::env::var_os("LIBRA_READ_POLICY");
        // SAFETY: single-threaded under #[serial]; restored below.
        unsafe {
            std::env::set_var("LIBRA_READ_POLICY", OsStr::from_bytes(&[0xff, 0xfe]));
        }
        let result = read_policy_from_env();
        unsafe {
            match &previous {
                Some(value) => std::env::set_var("LIBRA_READ_POLICY", value),
                None => std::env::remove_var("LIBRA_READ_POLICY"),
            }
        }
        assert!(
            result.is_err(),
            "non-UTF-8 env value must error, not map to Auto"
        );
    }
}
