//! Resolution of Libra's tracing log-sink configuration from the environment.
//!
//! Shared between the binary's `init_tracing` (which builds the subscriber) and
//! the `libra logfile info` command (which reports the resolved config), so both
//! agree on exactly how `LIBRA_LOG`, `LIBRA_LOG_FILE`, and `LIBRA_LOG_ROTATION`
//! are interpreted (lore.md §0.7).

use std::path::PathBuf;

/// Rolling strategy for the `LIBRA_LOG_FILE` sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogRotation {
    /// Single append-mode file (the default; the pre-0.7 behaviour).
    Never,
    /// A new file per minute (`<file>.YYYY-MM-DD-HH-MM`).
    Minutely,
    /// A new file per hour (`<file>.YYYY-MM-DD-HH`).
    Hourly,
    /// A new file per day (`<file>.YYYY-MM-DD`).
    Daily,
}

impl LogRotation {
    /// Parse a `LIBRA_LOG_ROTATION` value (case-insensitive). Unknown values —
    /// like the unset case — resolve to [`LogRotation::Never`].
    pub fn parse(value: &str) -> LogRotation {
        match value.trim().to_ascii_lowercase().as_str() {
            "minutely" | "minute" => LogRotation::Minutely,
            "hourly" | "hour" => LogRotation::Hourly,
            "daily" | "day" => LogRotation::Daily,
            _ => LogRotation::Never,
        }
    }

    /// Stable lowercase name (also the accepted env value).
    pub fn as_str(&self) -> &'static str {
        match self {
            LogRotation::Never => "never",
            LogRotation::Minutely => "minutely",
            LogRotation::Hourly => "hourly",
            LogRotation::Daily => "daily",
        }
    }
}

/// The resolved tracing configuration for this process.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// The `LIBRA_LOG_FILE` sink path, if logging to a file.
    pub file: Option<PathBuf>,
    /// The rolling strategy (only meaningful when `file` is set).
    pub rotation: LogRotation,
    /// The resolved env-filter directive, if any (`LIBRA_LOG` / `RUST_LOG`, or
    /// the `libra=debug` fallback when only `LIBRA_LOG_FILE` is set).
    pub filter: Option<String>,
}

impl LogConfig {
    /// True when tracing is enabled at all (a filter was resolved).
    pub fn is_enabled(&self) -> bool {
        self.filter.is_some()
    }
}

/// Resolve the tracing log configuration from the environment, applying the same
/// precedence the subscriber uses:
/// - filter: `LIBRA_LOG` → `RUST_LOG` → `libra=debug` (only if `LIBRA_LOG_FILE` set)
/// - file: `LIBRA_LOG_FILE`
/// - rotation: `LIBRA_LOG_ROTATION` (default `never`)
pub fn resolve_log_config() -> LogConfig {
    let file = std::env::var_os("LIBRA_LOG_FILE").map(PathBuf::from);
    let rotation = std::env::var("LIBRA_LOG_ROTATION")
        .map(|value| LogRotation::parse(&value))
        .unwrap_or(LogRotation::Never);
    let filter = std::env::var("LIBRA_LOG")
        .ok()
        .or_else(|| std::env::var("RUST_LOG").ok())
        .or_else(|| file.as_ref().map(|_| "libra=debug".to_string()));

    LogConfig {
        file,
        rotation,
        filter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_parse_is_case_insensitive_and_defaults_to_never() {
        assert_eq!(LogRotation::parse("Daily"), LogRotation::Daily);
        assert_eq!(LogRotation::parse("  HOURLY "), LogRotation::Hourly);
        assert_eq!(LogRotation::parse("minutely"), LogRotation::Minutely);
        assert_eq!(LogRotation::parse("never"), LogRotation::Never);
        assert_eq!(LogRotation::parse("bogus"), LogRotation::Never);
        assert_eq!(LogRotation::parse(""), LogRotation::Never);
    }

    #[test]
    fn rotation_as_str_round_trips() {
        for rotation in [
            LogRotation::Never,
            LogRotation::Minutely,
            LogRotation::Hourly,
            LogRotation::Daily,
        ] {
            assert_eq!(LogRotation::parse(rotation.as_str()), rotation);
        }
    }
}
