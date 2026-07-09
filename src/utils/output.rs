//! Global output configuration for the Libra CLI.
//!
//! This module resolves the raw global CLI flags (`--json`, `--machine`,
//! `--color`, `--quiet`, `--no-pager`, `--progress`, `--exit-code-on-warning`)
//! into a single [`OutputConfig`] that every command handler receives.
//!
//! The design ensures that all commands share the same output-control surface
//! without duplicating flag definitions in per-command `Args` structs.

use std::{
    env,
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;

use crate::utils::error::{CliError, CliResult, StableErrorCode};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// JSON output layout selected by `--json[=FORMAT]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonFormat {
    /// Pretty-printed JSON (the default when `--json` is passed without a value).
    Pretty,
    /// Single-line compact JSON.
    Compact,
    /// Newline-delimited JSON — one JSON object per line.
    Ndjson,
}

/// Terminal color policy resolved from `--color`, `--no-color`, `NO_COLOR`,
/// and `TERM=dumb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    /// Let the `colored` crate auto-detect based on TTY.
    Auto,
    /// Never emit ANSI color codes.
    Never,
    /// Always emit ANSI color codes, even when piped.
    Always,
}

/// Progress reporting mode resolved from `--progress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// Emit NDJSON progress events to stderr.
    Json,
    /// Human-friendly `indicatif` progress bar on stderr.
    Text,
    /// Suppress all progress output.
    None,
}

/// Raw progress preference requested by the caller before auto-resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPreference {
    /// Resolve progress based on output mode and stderr TTY status.
    Auto,
    /// Request NDJSON progress events.
    Json,
    /// Request human-readable progress output.
    Text,
    /// Request no progress output.
    None,
}

// ---------------------------------------------------------------------------
// OutputConfig
// ---------------------------------------------------------------------------

/// Resolved output configuration, passed to every command handler.
///
/// Constructed once in `parse_async()` from the global CLI flags and
/// environment variables, then threaded through to each command's
/// `execute_safe(args, &output)`.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// `Some(format)` when JSON output was requested; `None` for human mode.
    pub json_format: Option<JsonFormat>,
    /// Resolved color policy.
    pub color: ColorChoice,
    /// Whether a pager is allowed (false when `--no-pager` or `--machine`).
    pub pager: bool,
    /// Suppress standard stdout output (keep warnings/errors on stderr).
    pub quiet: bool,
    /// Return exit code 9 when any warning is emitted.
    pub exit_code_on_warning: bool,
    /// How to report progress for long-running operations.
    pub progress: ProgressMode,
    /// Original progress preference before [`ProgressMode`] auto-resolution.
    pub progress_preference: ProgressPreference,
}

fn write_json_command_envelope<W: Write, T: Serialize>(
    writer: &mut W,
    command: &str,
    data: &T,
    format: JsonFormat,
) -> io::Result<()> {
    let envelope = serde_json::json!({
        "ok": true,
        "command": command,
        "data": data,
    });
    match format {
        JsonFormat::Pretty => serde_json::to_writer_pretty(&mut *writer, &envelope)?,
        JsonFormat::Compact | JsonFormat::Ndjson => serde_json::to_writer(&mut *writer, &envelope)?,
    }
    writeln!(writer)
}

/// Convert stdout write failures into CLI errors. BrokenPipe is not an error for
/// Unix pipelines: it means the downstream consumer intentionally stopped early.
pub fn stdout_write_error(action: &str, error: io::Error) -> CliError {
    if error.kind() == io::ErrorKind::BrokenPipe {
        return CliError::silent_exit(0);
    }
    CliError::fatal(format!("failed to {action}: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

fn stdout_json_error(action: &str, error: serde_json::Error) -> CliError {
    if error.io_error_kind() == Some(io::ErrorKind::BrokenPipe) {
        return CliError::silent_exit(0);
    }
    CliError::internal(format!("failed to {action}: {error}"))
}

impl Default for OutputConfig {
    /// The default matches pre-existing behavior: human output, auto color,
    /// pager allowed, not quiet, no warning exit, text progress when TTY.
    fn default() -> Self {
        Self {
            json_format: None,
            color: ColorChoice::Auto,
            pager: true,
            quiet: false,
            exit_code_on_warning: false,
            progress: ProgressMode::Text,
            progress_preference: ProgressPreference::Auto,
        }
    }
}

impl OutputConfig {
    /// Resolve raw CLI flag values into a normalized [`OutputConfig`].
    ///
    /// The `color_raw` and `progress_raw` parameters are the string values
    /// of `--color` and `--progress` (e.g. `"auto"`, `"never"`, `"json"`).
    /// `--no-color` is normalized by the CLI parser to `color_raw = "never"`.
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn resolve(
        json: Option<&str>,
        machine: bool,
        no_pager: bool,
        color_raw: &str,
        quiet: bool,
        exit_code_on_warning: bool,
        progress_raw: &str,
    ) -> Self {
        // --machine implies --json=ndjson --no-pager --color=never --quiet
        let json_format = if machine {
            Some(JsonFormat::Ndjson)
        } else {
            json.map(|s| match s {
                "compact" => JsonFormat::Compact,
                "ndjson" => JsonFormat::Ndjson,
                _ => JsonFormat::Pretty,
            })
        };

        let quiet = quiet || machine;
        let pager = !no_pager && !machine && json_format.is_none();

        // Color: --machine and --no-color force never; otherwise parse the flag.
        // NO_COLOR or TERM=dumb force Never only while color remains auto.
        let explicit_color = if machine {
            ColorChoice::Never
        } else {
            match color_raw {
                "never" => ColorChoice::Never,
                "always" => ColorChoice::Always,
                _ => ColorChoice::Auto,
            }
        };

        let auto_color_disabled =
            env::var_os("NO_COLOR").is_some() || matches!(env::var("TERM").as_deref(), Ok("dumb"));
        let color = if explicit_color == ColorChoice::Auto && auto_color_disabled {
            ColorChoice::Never
        } else {
            explicit_color
        };

        // Progress: resolve "auto" based on context.
        let progress_preference = match progress_raw {
            "json" => ProgressPreference::Json,
            "text" => ProgressPreference::Text,
            "none" => ProgressPreference::None,
            _ => ProgressPreference::Auto,
        };

        let progress = if machine {
            ProgressMode::None
        } else {
            match progress_raw {
                "json" => ProgressMode::Json,
                "text" => ProgressMode::Text,
                "none" => ProgressMode::None,
                // "auto"
                _ => {
                    if json_format.is_some() {
                        ProgressMode::Json
                    } else if quiet {
                        ProgressMode::None
                    } else if io::stderr().is_terminal() {
                        ProgressMode::Text
                    } else {
                        ProgressMode::None
                    }
                }
            }
        };

        Self {
            json_format,
            color,
            pager,
            quiet,
            exit_code_on_warning,
            progress,
            progress_preference,
        }
    }

    /// Best-effort resolution of global output flags directly from argv.
    ///
    /// This is used during startup parse failures, before clap has produced a
    /// fully typed `Cli`, so error rendering still follows the same precedence
    /// rules as successful command execution.
    pub fn resolve_from_argv(argv: &[String]) -> Self {
        let mut json: Option<String> = None;
        let mut machine = false;
        let mut no_pager = false;
        let mut no_color = false;
        let mut color = String::from("auto");
        let mut quiet = false;
        let mut exit_code_on_warning = false;
        let mut progress = String::from("auto");

        let mut args = argv.iter().peekable();
        while let Some(arg) = args.next() {
            if let Some(value) = arg.strip_prefix("-J=") {
                json = Some(value.to_string());
                continue;
            }
            if let Some(value) = arg.strip_prefix("--json=") {
                json = Some(value.to_string());
                continue;
            }
            if let Some(value) = arg.strip_prefix("--color=") {
                color = value.to_string();
                continue;
            }
            if let Some(value) = arg.strip_prefix("--progress=") {
                progress = value.to_string();
                continue;
            }
            if arg == "--no-color" {
                no_color = true;
                continue;
            }
            if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 {
                let short_flags = &arg[1..];
                if short_flags.chars().all(|flag| matches!(flag, 'J' | 'q')) {
                    for flag in short_flags.chars() {
                        match flag {
                            'J' => json = Some(String::from("pretty")),
                            'q' => quiet = true,
                            _ => {}
                        }
                    }
                    continue;
                }
            }

            match arg.as_str() {
                "--json" | "-J" => json = Some(String::from("pretty")),
                "--machine" => machine = true,
                "--no-pager" => no_pager = true,
                "--no-color" => no_color = true,
                "--quiet" | "-q" => quiet = true,
                "--exit-code-on-warning" => exit_code_on_warning = true,
                "--color" => {
                    if let Some(value) = args.next() {
                        color = value.to_string();
                    }
                }
                "--progress" => {
                    if let Some(value) = args.next() {
                        progress = value.to_string();
                    }
                }
                _ => {}
            }
        }

        let color_raw = if no_color { "never" } else { &color };
        Self::resolve(
            json.as_deref(),
            machine,
            no_pager,
            color_raw,
            quiet,
            exit_code_on_warning,
            &progress,
        )
    }

    /// Returns `true` if any JSON output format was requested.
    pub fn is_json(&self) -> bool {
        self.json_format.is_some()
    }

    /// Build a child configuration for nested command execution.
    ///
    /// Nested commands must stay silent when the parent command owns the
    /// machine-readable output contract or has requested quiet mode.
    #[must_use]
    pub fn child_output_config(&self) -> Self {
        if self.is_json() || self.quiet {
            Self {
                json_format: None,
                quiet: true,
                progress: ProgressMode::None,
                progress_preference: ProgressPreference::None,
                ..self.clone()
            }
        } else {
            self.clone()
        }
    }

    /// Apply the resolved color choice to the `colored` crate's global override.
    ///
    /// Call this once, early in `parse_async()`, before any command runs.
    pub fn apply_color_override(&self) {
        match self.color {
            ColorChoice::Never => colored::control::set_override(false),
            ColorChoice::Always => colored::control::set_override(true),
            ColorChoice::Auto => colored::control::unset_override(),
        }
    }
}

// ---------------------------------------------------------------------------
// CommandOutput trait + emit helpers
// ---------------------------------------------------------------------------

/// Trait for command result types that support both human and JSON rendering.
///
/// Implementors must derive (or manually impl) `Serialize` for JSON mode and
/// provide a `render_human` method for the default text path.
pub trait CommandOutput: Serialize {
    /// Write a human-readable representation to `w`.
    fn render_human(&self, w: &mut dyn Write, config: &OutputConfig) -> io::Result<()>;
}

/// Emit a single value according to the active output mode.
///
/// - `Pretty` → pretty-printed JSON envelope `{"ok":true,"data":...}`
/// - `Compact` → single-line JSON envelope
/// - `Ndjson` → one JSON line (no envelope wrapper)
/// - `None` (human) → delegates to `CommandOutput::render_human`
pub fn emit<T: CommandOutput>(value: &T, config: &OutputConfig) -> CliResult<()> {
    let stdout = io::stdout();
    let mut w = stdout.lock();
    match config.json_format {
        Some(JsonFormat::Pretty) => {
            let envelope = serde_json::json!({"ok": true, "data": value});
            serde_json::to_writer_pretty(&mut w, &envelope)
                .map_err(|e| stdout_json_error("serialize JSON output", e))?;
            writeln!(w).map_err(|e| stdout_write_error("write JSON output", e))?;
        }
        Some(JsonFormat::Compact) => {
            let envelope = serde_json::json!({"ok": true, "data": value});
            serde_json::to_writer(&mut w, &envelope)
                .map_err(|e| stdout_json_error("serialize JSON output", e))?;
            writeln!(w).map_err(|e| stdout_write_error("write JSON output", e))?;
        }
        Some(JsonFormat::Ndjson) => {
            serde_json::to_writer(&mut w, value)
                .map_err(|e| stdout_json_error("serialize JSON output", e))?;
            writeln!(w).map_err(|e| stdout_write_error("write JSON output", e))?;
        }
        None => {
            value
                .render_human(&mut w, config)
                .map_err(|e| stdout_write_error("write output", e))?;
        }
    }
    Ok(())
}

/// Emit a JSON success envelope for commands that already have structured
/// machine-readable data but are not yet modeled as [`CommandOutput`].
pub fn emit_json_data<T: Serialize>(
    command: &str,
    data: &T,
    config: &OutputConfig,
) -> CliResult<()> {
    let format = config.json_format.ok_or_else(|| {
        CliError::internal("emit_json_data called without an active JSON output mode")
    })?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    write_json_command_envelope(&mut writer, command, data, format)
        .map_err(|e| stdout_write_error("write JSON output", e))
}

/// Emit each item in an iterator as a separate NDJSON line, or collect into a
/// JSON array envelope, or render human-readable text.
pub fn emit_list<T: CommandOutput>(items: &[T], config: &OutputConfig) -> CliResult<()> {
    let stdout = io::stdout();
    let mut w = stdout.lock();
    match config.json_format {
        Some(JsonFormat::Pretty) => {
            let envelope = serde_json::json!({"ok": true, "data": items});
            serde_json::to_writer_pretty(&mut w, &envelope)
                .map_err(|e| stdout_json_error("serialize JSON output", e))?;
            writeln!(w).map_err(|e| stdout_write_error("write JSON output", e))?;
        }
        Some(JsonFormat::Compact) => {
            let envelope = serde_json::json!({"ok": true, "data": items});
            serde_json::to_writer(&mut w, &envelope)
                .map_err(|e| stdout_json_error("serialize JSON output", e))?;
            writeln!(w).map_err(|e| stdout_write_error("write JSON output", e))?;
        }
        Some(JsonFormat::Ndjson) => {
            for item in items {
                serde_json::to_writer(&mut w, item)
                    .map_err(|e| stdout_json_error("serialize JSON output", e))?;
                writeln!(w).map_err(|e| stdout_write_error("write JSON output", e))?;
            }
        }
        None => {
            for item in items {
                item.render_human(&mut w, config)
                    .map_err(|e| stdout_write_error("write output", e))?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// info_println! macro
// ---------------------------------------------------------------------------

/// Print to stdout only when quiet mode is **not** active.
///
/// Usage mirrors `println!` but takes an `&OutputConfig` as the first argument:
/// ```ignore
/// info_println!(output, "Switched to branch '{}'", branch_name);
/// ```
#[macro_export]
macro_rules! info_println {
    ($config:expr, $($arg:tt)*) => {
        if !$config.quiet {
            println!($($arg)*);
        }
    };
}

// ---------------------------------------------------------------------------
// Warning tracker (for --exit-code-on-warning)
// ---------------------------------------------------------------------------

static WARNING_EMITTED: AtomicBool = AtomicBool::new(false);

/// Record that a warning was emitted during command execution.
///
/// Called from `emit_legacy_stderr()` when it detects a `"warning: "` prefix,
/// and may be called explicitly by commands that issue warnings.
pub fn record_warning() {
    WARNING_EMITTED.store(true, Ordering::Relaxed);
}

/// Returns `true` if [`record_warning`] was called at least once.
pub fn warning_was_emitted() -> bool {
    WARNING_EMITTED.load(Ordering::Relaxed)
}

/// Reset the warning tracker before each top-level CLI invocation.
pub fn reset_warning_tracker() {
    WARNING_EMITTED.store(false, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// ProgressReporter
// ---------------------------------------------------------------------------

/// Unified progress reporter that adapts to the resolved [`ProgressMode`].
///
/// - `Text` → wraps an `indicatif::ProgressBar` on stderr.
/// - `Json` → emits NDJSON progress events to stderr.
/// - `None` → all calls are no-ops.
pub struct ProgressReporter {
    mode: ProgressMode,
    task: String,
    bar: Option<ProgressBar>,
    total: Option<u64>,
}

impl ProgressReporter {
    /// Create a new reporter for the given task name.
    ///
    /// `total` is `Some(n)` for determinate progress or `None` for a spinner.
    pub fn new(task: &str, total: Option<u64>, config: &OutputConfig) -> Self {
        let mode = config.progress;
        let bar = match mode {
            ProgressMode::Text => {
                let pb = if let Some(len) = total {
                    let pb = ProgressBar::new(len);
                    let style = match ProgressStyle::default_bar().template(
                        "{spinner:.magenta} [{elapsed_precise}] [{bar:40.green/white}] {bytes}/{total_bytes} ({eta}) {bytes_per_sec}",
                    ) {
                        Ok(style) => style.progress_chars("=> "),
                        Err(err) => {
                            tracing::warn!("failed to build progress bar template: {err}");
                            ProgressStyle::default_bar()
                        }
                    };
                    pb.set_style(style);
                    pb
                } else {
                    ProgressBar::new_spinner()
                };
                Some(pb)
            }
            _ => None,
        };

        Self {
            mode,
            task: task.to_string(),
            bar,
            total,
        }
    }

    /// Update progress to `current` (out of the total set at construction).
    pub fn tick(&self, current: u64) {
        match self.mode {
            ProgressMode::Text => {
                if let Some(bar) = &self.bar {
                    bar.set_position(current);
                }
            }
            ProgressMode::Json => {
                let event = serde_json::json!({
                    "event": "progress",
                    "task": self.task,
                    "current": current,
                    "total": self.total,
                });
                // Progress goes to stderr to keep stdout clean for data.
                let _ = writeln!(io::stderr(), "{}", event);
            }
            ProgressMode::None => {}
        }
    }

    /// Mark the task as finished.
    pub fn finish(&self) {
        match self.mode {
            ProgressMode::Text => {
                if let Some(bar) = &self.bar {
                    bar.finish_and_clear();
                }
            }
            ProgressMode::Json => {
                let event = serde_json::json!({
                    "event": "progress_done",
                    "task": self.task,
                });
                let _ = writeln!(io::stderr(), "{}", event);
            }
            ProgressMode::None => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use colored::Colorize;
    use serial_test::serial;

    use super::*;

    struct ColorOverrideReset;

    impl Drop for ColorOverrideReset {
        fn drop(&mut self) {
            colored::control::unset_override();
        }
    }

    #[test]
    fn default_is_human_mode() {
        let config = OutputConfig::default();
        assert!(config.json_format.is_none());
        assert_eq!(config.color, ColorChoice::Auto);
        assert!(config.pager);
        assert!(!config.quiet);
        assert!(!config.exit_code_on_warning);
        assert_eq!(config.progress_preference, ProgressPreference::Auto);
    }

    #[test]
    fn resolve_machine_mode() {
        let config = OutputConfig::resolve(
            None,  // json
            true,  // machine
            false, // no_pager
            "auto", false, // quiet
            false, // exit_code_on_warning
            "auto",
        );
        assert_eq!(config.json_format, Some(JsonFormat::Ndjson));
        assert_eq!(config.color, ColorChoice::Never);
        assert!(!config.pager);
        assert!(config.quiet);
        assert_eq!(config.progress, ProgressMode::None);
        assert_eq!(config.progress_preference, ProgressPreference::Auto);
    }

    #[test]
    fn resolve_json_without_value() {
        let config =
            OutputConfig::resolve(Some("pretty"), false, false, "auto", false, false, "auto");
        assert_eq!(config.json_format, Some(JsonFormat::Pretty));
        // JSON mode disables pager
        assert!(!config.pager);
    }

    #[test]
    fn resolve_json_compact() {
        let config =
            OutputConfig::resolve(Some("compact"), false, false, "auto", false, false, "auto");
        assert_eq!(config.json_format, Some(JsonFormat::Compact));
    }

    #[test]
    fn resolve_json_ndjson() {
        let config =
            OutputConfig::resolve(Some("ndjson"), false, false, "auto", false, false, "auto");
        assert_eq!(config.json_format, Some(JsonFormat::Ndjson));
    }

    #[test]
    fn resolve_color_never() {
        let config = OutputConfig::resolve(None, false, false, "never", false, false, "auto");
        assert_eq!(config.color, ColorChoice::Never);
    }

    #[test]
    fn resolve_color_always() {
        let config = OutputConfig::resolve(None, false, false, "always", false, false, "auto");
        assert_eq!(config.color, ColorChoice::Always);
    }

    #[test]
    #[serial]
    fn resolve_term_dumb_disables_auto_color() {
        let _term = crate::utils::test::ScopedEnvVar::set("TERM", "dumb");

        let config = OutputConfig::resolve(None, false, false, "auto", false, false, "auto");
        assert_eq!(config.color, ColorChoice::Never);

        let explicit = OutputConfig::resolve(None, false, false, "always", false, false, "auto");
        assert_eq!(explicit.color, ColorChoice::Always);
    }

    #[test]
    fn resolve_no_pager() {
        let config = OutputConfig::resolve(None, false, true, "auto", false, false, "auto");
        assert!(!config.pager);
    }

    #[test]
    fn resolve_quiet_suppresses_progress() {
        let config = OutputConfig::resolve(None, false, false, "auto", true, false, "auto");
        assert!(config.quiet);
        assert_eq!(config.progress, ProgressMode::None);
    }

    #[test]
    fn resolve_explicit_progress_json() {
        let config = OutputConfig::resolve(None, false, false, "auto", false, false, "json");
        assert_eq!(config.progress, ProgressMode::Json);
        assert_eq!(config.progress_preference, ProgressPreference::Json);
    }

    #[test]
    fn resolve_explicit_progress_none() {
        let config = OutputConfig::resolve(None, false, false, "auto", false, false, "none");
        assert_eq!(config.progress, ProgressMode::None);
        assert_eq!(config.progress_preference, ProgressPreference::None);
    }

    #[test]
    fn resolve_exit_code_on_warning() {
        let config = OutputConfig::resolve(None, false, false, "auto", false, true, "auto");
        assert!(config.exit_code_on_warning);
    }

    #[test]
    fn warning_tracker() {
        reset_warning_tracker();
        assert!(!warning_was_emitted());
        record_warning();
        assert!(warning_was_emitted());
        reset_warning_tracker();
        assert!(!warning_was_emitted());
    }

    #[test]
    fn resolve_from_argv_machine_overrides_json_pretty() {
        let argv = vec![
            "libra".to_string(),
            "--machine".to_string(),
            "--json".to_string(),
            "status".to_string(),
        ];

        let config = OutputConfig::resolve_from_argv(&argv);

        assert_eq!(config.json_format, Some(JsonFormat::Ndjson));
        assert!(config.quiet);
    }

    #[test]
    fn resolve_from_argv_supports_clustered_short_flags() {
        let argv = vec!["libra".to_string(), "-qJ".to_string(), "status".to_string()];

        let config = OutputConfig::resolve_from_argv(&argv);

        assert_eq!(config.json_format, Some(JsonFormat::Pretty));
        assert!(config.quiet);
    }

    #[test]
    fn resolve_from_argv_no_color_sets_never() {
        let argv = vec![
            "libra".to_string(),
            "--color=always".to_string(),
            "--no-color".to_string(),
            "status".to_string(),
        ];

        let config = OutputConfig::resolve_from_argv(&argv);

        assert_eq!(config.color, ColorChoice::Never);
    }

    #[test]
    #[serial]
    fn apply_color_override_auto_clears_previous_override() {
        let _guard = ColorOverrideReset;

        // Force colors on, verify the override takes effect.
        colored::control::set_override(true);
        assert!(
            "x".red().to_string().contains("\u{1b}["),
            "forced override should enable ANSI colors"
        );

        // Switch to "never" and verify colors are disabled.
        OutputConfig::resolve(None, false, false, "never", false, false, "auto")
            .apply_color_override();
        assert!(
            !"x".red().to_string().contains("\u{1b}["),
            "never mode should disable ANSI colors"
        );

        // Switch to "auto" — the override should be cleared, so the
        // library falls back to terminal detection.  We only assert that
        // the transition did not panic; the actual coloring depends on
        // whether stdout is a TTY.
        OutputConfig::default().apply_color_override();
    }
}
