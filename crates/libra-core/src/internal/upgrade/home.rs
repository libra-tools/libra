//! Canonical `LIBRA_HOME` resolution (plan-20260714 §A.3).
//!
//! The upgrade subsystem persists its state under `{LIBRA_HOME}/upgrade/`.
//! [`resolve_libra_home`] is the single Rust-side implementation of the home
//! rules, kept in lockstep with `install.sh`'s
//! `LIBRA_HOME="${LIBRA_HOME:-$HOME/.libra}"` contract and with the
//! `LIBRA_CONFIG_GLOBAL_DB` test-isolation contract of the global config
//! database:
//!
//! 1. a non-empty `LIBRA_HOME` environment variable wins;
//! 2. otherwise, when `LIBRA_CONFIG_GLOBAL_DB` is set (the global-config
//!    isolation hook used by tests and sandboxes), the settings live next to
//!    that database. This rule exists ONLY for explicit isolation calls: the
//!    default layout is the XDG config directory since 2026-09-19 (GCX-01), so
//!    it is no longer a description of where per-user state normally lives;
//!    isolated environments must stay isolated instead of silently touching
//!    the real user's upgrade state;
//! 3. otherwise `$HOME/.libra` (falling back to the platform home directory
//!    on systems where `HOME` is not set, e.g. Windows).
//!
//! `install.sh` falls back to `/tmp/.libra` when `HOME` is unset; the Rust
//! side deliberately reports an actionable error instead — upgrade state
//! gates binary replacement, and writing it into a world-writable temp
//! directory would be unsafe (§A.5 ownership rules could never hold there).

use std::path::PathBuf;

/// Environment variable overriding the Libra home directory (see module docs).
pub const LIBRA_HOME_ENV: &str = "LIBRA_HOME";

/// Global config database override honored for upgrade-state isolation (rule 2).
const LIBRA_CONFIG_GLOBAL_DB_ENV: &str = "LIBRA_CONFIG_GLOBAL_DB";

/// Failure to determine the Libra home directory.
#[derive(Debug, thiserror::Error)]
pub enum LibraHomeError {
    /// Neither `LIBRA_HOME`, `HOME`, nor a platform home directory is available.
    #[error(
        "cannot determine the Libra home directory: neither the LIBRA_HOME environment variable \
         nor a home directory is available; set LIBRA_HOME to an absolute path \
         (the installer default is ~/.libra)"
    )]
    Unresolvable,
}

/// Resolve `{LIBRA_HOME}` — the per-user Libra state directory.
///
/// # Returns
/// The home directory per the module-level rules. Empty environment values
/// are treated as unset, matching `install.sh`'s `${LIBRA_HOME:-…}` semantics.
///
/// # Errors
/// [`LibraHomeError::Unresolvable`] when no home directory can be determined.
pub fn resolve_libra_home() -> Result<PathBuf, LibraHomeError> {
    if let Some(explicit) = std::env::var_os(LIBRA_HOME_ENV)
        && !explicit.is_empty()
    {
        return Ok(PathBuf::from(explicit));
    }
    if let Some(global_db) = std::env::var_os(LIBRA_CONFIG_GLOBAL_DB_ENV)
        && !global_db.is_empty()
    {
        let db_path = PathBuf::from(&global_db);
        // A bare relative override (`LIBRA_CONFIG_GLOBAL_DB=config.db`) has an
        // empty parent; it means "the current directory", exactly like the
        // database file itself — never fall through to the real home.
        return Ok(match db_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        });
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir);
    match home {
        Some(dir) => Ok(dir.join(".libra")),
        None => Err(LibraHomeError::Unresolvable),
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;
    use crate::utils::test::ScopedEnvVar;

    #[test]
    #[serial(env)]
    fn explicit_libra_home_wins() {
        let _env = ScopedEnvVar::set(LIBRA_HOME_ENV, "/custom/libra-home");
        let _db = ScopedEnvVar::set(LIBRA_CONFIG_GLOBAL_DB_ENV, "/isolated/config.db");
        assert_eq!(
            resolve_libra_home().expect("test fixture operation should succeed"),
            PathBuf::from("/custom/libra-home")
        );
    }

    #[test]
    #[serial(env)]
    fn global_db_isolation_hook_beats_home() {
        let _env = ScopedEnvVar::unset(LIBRA_HOME_ENV);
        let _db = ScopedEnvVar::set(LIBRA_CONFIG_GLOBAL_DB_ENV, "/isolated/store/config.db");
        let _home = ScopedEnvVar::set("HOME", "/tmp/should-not-be-used");
        assert_eq!(
            resolve_libra_home().expect("test fixture operation should succeed"),
            PathBuf::from("/isolated/store")
        );
    }

    #[test]
    #[serial(env)]
    fn bare_relative_global_db_override_stays_isolated() {
        let _env = ScopedEnvVar::unset(LIBRA_HOME_ENV);
        let _db = ScopedEnvVar::set(LIBRA_CONFIG_GLOBAL_DB_ENV, "config.db");
        let _home = ScopedEnvVar::set("HOME", "/tmp/should-not-be-used");
        assert_eq!(
            resolve_libra_home().expect("test fixture operation should succeed"),
            PathBuf::from(".")
        );
    }

    #[test]
    #[serial(env)]
    fn empty_libra_home_is_treated_as_unset() {
        let _env = ScopedEnvVar::set(LIBRA_HOME_ENV, "");
        let _db = ScopedEnvVar::unset(LIBRA_CONFIG_GLOBAL_DB_ENV);
        let _home = ScopedEnvVar::set("HOME", "/tmp/upgrade-home-test");
        assert_eq!(
            resolve_libra_home().expect("test fixture operation should succeed"),
            PathBuf::from("/tmp/upgrade-home-test/.libra")
        );
    }

    #[test]
    #[serial(env)]
    fn falls_back_to_home_dot_libra() {
        let _env = ScopedEnvVar::unset(LIBRA_HOME_ENV);
        let _db = ScopedEnvVar::unset(LIBRA_CONFIG_GLOBAL_DB_ENV);
        let _home = ScopedEnvVar::set("HOME", "/tmp/upgrade-home-test2");
        assert_eq!(
            resolve_libra_home().expect("test fixture operation should succeed"),
            PathBuf::from("/tmp/upgrade-home-test2/.libra")
        );
    }
}
