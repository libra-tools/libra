//! Binary entry point for the `libra` CLI.
//!
//! Responsibilities, in order:
//! 1. Initialise the tracing subscriber (controlled by `LIBRA_LOG` / `RUST_LOG` and the
//!    optional `LIBRA_LOG_FILE` env var).
//! 2. Spawn a dedicated thread with a 32 MiB stack so deep call chains in the smart
//!    protocol code path do not overflow the much smaller default thread stack.
//! 3. Block on the CLI dispatcher and translate its result into a process exit code,
//!    rendering errors through the same [`OutputConfig`] machinery the dispatcher uses
//!    so that `--json` and friends keep behaving consistently when parsing itself fails.

use std::{
    any::Any,
    fs::OpenOptions,
    panic,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use libra::{
    cli,
    utils::{
        error::INTERNAL_ERROR_REPORT_HINT,
        log_config::{LogRotation, resolve_log_config},
        output::OutputConfig,
    },
};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

static STDOUT_BROKEN_PIPE_PANIC: AtomicBool = AtomicBool::new(false);

/// Process entry point.
///
/// Functional scope:
/// - Sets up logging, runs the CLI on a high-stack thread, and translates any error
///   into a non-zero exit code. The function intentionally does not return a
///   `Result` — exit codes are the only meaningful surface for a binary entry point.
///
/// Boundary conditions:
/// - If the CLI thread fails to spawn, exits with code `1` and a fatal message on
///   stderr (no JSON, since we never got far enough to know the user's preference)
///   plus the standard internal-error report hint.
/// - If the CLI thread panics, also exits `1` with a fixed message plus the same
///   hint; thread panics bypass the `CliError` rendering path.
/// - On a clean `Err(CliError)`, the exit code is sourced from
///   [`CliError::exit_code`] so each error class has a stable code.
fn main() {
    install_broken_pipe_panic_hook();
    init_tracing();

    const CLI_STACK_SIZE: usize = 32 * 1024 * 1024;
    let handle = std::thread::Builder::new()
        .name("libra-cli".to_string())
        .stack_size(CLI_STACK_SIZE)
        .spawn(|| cli::parse(None));

    let result = match handle {
        Ok(handle) => match handle.join() {
            Ok(result) => result,
            Err(payload) if panic_payload_is_stdout_broken_pipe(&*payload) => {
                flush_telemetry();
                return;
            }
            Err(_) if STDOUT_BROKEN_PIPE_PANIC.swap(false, Ordering::SeqCst) => {
                flush_telemetry();
                return;
            }
            Err(_) => {
                eprintln!("fatal: CLI thread panicked\n\nHint: {INTERNAL_ERROR_REPORT_HINT}");
                flush_telemetry();
                std::process::exit(1);
            }
        },
        Err(err) => {
            eprintln!(
                "fatal: failed to spawn CLI thread: {err}\n\nHint: {INTERNAL_ERROR_REPORT_HINT}"
            );
            flush_telemetry();
            std::process::exit(1);
        }
    };

    if let Err(err) = result {
        if err.is_stdout_broken_pipe() {
            flush_telemetry();
            return;
        }
        // Best-effort JSON rendering: resolve the output flags directly from argv so
        // parse-time failures follow the same precedence rules as successful dispatch.
        // We must read from `std::env::args()` (not the dispatcher's parsed `args`)
        // because the dispatcher returned an error before producing them.
        let argv: Vec<String> = std::env::args().collect();
        let output = OutputConfig::resolve_from_argv(&argv);
        err.print_for_output(&output);
        flush_telemetry();
        std::process::exit(err.exit_code());
    }
    flush_telemetry();
}

/// Suppress Rust's default panic report for the specific panic emitted by
/// `println!`/`print!` when stdout is a closed pipe. The CLI thread still unwinds;
/// `main` classifies the join payload and exits quietly.
fn install_broken_pipe_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if panic_payload_is_stdout_broken_pipe(info.payload()) {
            STDOUT_BROKEN_PIPE_PANIC.store(true, Ordering::SeqCst);
            return;
        }
        previous(info);
    }));
}

fn panic_payload_is_stdout_broken_pipe(payload: &(dyn Any + Send)) -> bool {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return message_is_stdout_broken_pipe(message);
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message_is_stdout_broken_pipe(message);
    }
    false
}

fn message_is_stdout_broken_pipe(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("failed printing to stdout")
        && (lower.contains("broken pipe") || lower.contains("os error 32"))
}

/// Flush OTLP telemetry (feature-gated). MUST be called explicitly before
/// every `std::process::exit` in main — `process::exit` skips destructors,
/// so a scopeguard would silently miss exactly the error paths.
fn flush_telemetry() {
    #[cfg(feature = "otlp")]
    libra::utils::telemetry::shutdown();
}

/// Configure the global [`tracing`] subscriber.
///
/// Functional scope:
/// - Reads the filter directive from `LIBRA_LOG`, falling back to `RUST_LOG`, falling
///   back to `libra=debug` only when `LIBRA_LOG_FILE` is set (so the file is never
///   created with no useful content).
/// - When `LIBRA_LOG_FILE` is set, opens that file in append mode and routes events
///   there with ANSI escapes disabled. Otherwise emits to stderr with default
///   formatting.
///
/// Boundary conditions:
/// - When no env vars are set, returns silently without installing any subscriber so
///   that ordinary CLI use produces no log noise.
/// - Subscriber installation is best-effort: if the global subscriber is already
///   installed (e.g. because a library consumer set one up first) we print a warning
///   to stderr but never fail the process.
/// - If `LIBRA_LOG_FILE` cannot be opened, we warn on stderr and leave tracing
///   disabled — we never crash the CLI just because logging failed.
fn init_tracing() {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let config = resolve_log_config();

    // OTLP layer (lore.md 1.7): compiled only with the feature, active only
    // when the standard OTel endpoint env vars gate it on.
    #[cfg(feature = "otlp")]
    let otlp_layer = libra::utils::telemetry::try_build_layer();
    #[cfg(not(feature = "otlp"))]
    let otlp_layer: Option<tracing_subscriber::layer::Identity> = None;

    // Fmt layer: only when a filter directive is configured (preserving the
    // historical zero-subscriber fast path when neither logging nor
    // telemetry is requested).
    let fmt_layer = config
        .filter
        .as_deref()
        .and_then(|directive| build_fmt_layer(directive, config.file.as_deref(), config.rotation));

    // A Vec<Box<dyn Layer<Registry>>> implements Layer, letting the two
    // optional layers stack without type gymnastics.
    let mut layers: Vec<BoxedLayer> = Vec::new();
    if let Some(layer) = fmt_layer {
        layers.push(layer);
    }
    #[cfg(feature = "otlp")]
    if let Some(layer) = otlp_layer {
        layers.push(layer);
    }
    #[cfg(not(feature = "otlp"))]
    let _ = otlp_layer;

    if layers.is_empty() {
        return; // nothing to install — ordinary CLI use stays silent
    }

    if let Err(err) = tracing_subscriber::registry().with(layers).try_init() {
        eprintln!("warning: failed to initialize tracing subscriber: {err}");
    }
}

type BoxedLayer = Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;

/// Build the human-log fmt layer for the configured sink. The layer's
/// per-layer filter is the user's EnvFilter AND-ed with an exclusion of the
/// vetted `libra::telemetry` span target: that span exists for the OTLP
/// exporter only, and letting it through would prepend a span scope to every
/// dispatch-time log line — an observable format change for LIBRA_LOG users.
fn build_fmt_layer(
    directive: &str,
    file: Option<&Path>,
    rotation: LogRotation,
) -> Option<BoxedLayer> {
    use tracing_subscriber::layer::Layer;
    let env_filter = build_env_filter(directive);
    let not_telemetry =
        tracing_subscriber::filter::filter_fn(|metadata| metadata.target() != "libra::telemetry");
    let Some(path) = file else {
        let layer = tracing_subscriber::fmt::layer()
            .with_filter(env_filter)
            .with_filter(not_telemetry);
        return Some(Box::new(layer));
    };
    match rotation {
        // Default / pre-0.7 behaviour: one append-mode file at exactly `path`.
        LogRotation::Never => match OpenOptions::new().create(true).append(true).open(path) {
            Ok(log_file) => {
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(Mutex::new(log_file))
                    .with_filter(env_filter)
                    .with_filter(not_telemetry);
                Some(Box::new(layer))
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to open LIBRA_LOG_FILE {}; tracing disabled: {err}",
                    path.display()
                );
                None
            }
        },
        // lore.md §0.7: roll the file on the requested interval so no single log
        // file grows without limit. Rotation only SPLITS logs by time; it does
        // not delete old files, so total disk use needs external retention
        // (e.g. logrotate) or a dedicated log directory.
        rotation => build_rolling_fmt_layer(path, rotation, env_filter, not_telemetry),
    }
}

/// Route tracing to a time-rolled file: `<dir>/<name>.<date-suffix>` where the
/// suffix granularity follows `rotation`. Blocking writer (no worker guard), so
/// no log lines are lost when the short-lived CLI process exits.
fn build_rolling_fmt_layer<F>(
    path: &Path,
    rotation: LogRotation,
    env_filter: EnvFilter,
    not_telemetry: tracing_subscriber::filter::FilterFn<F>,
) -> Option<BoxedLayer>
where
    F: Fn(&tracing::Metadata<'_>) -> bool + Send + Sync + 'static,
{
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // A bare filename rolls in the current directory.
        _ => PathBuf::from("."),
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        eprintln!(
            "warning: LIBRA_LOG_FILE {} has no valid UTF-8 file name; tracing disabled",
            path.display()
        );
        return None;
    };

    // Create the log directory up front; the builder errors (rather than
    // creating it) if it is missing.
    if let Err(err) = std::fs::create_dir_all(&directory) {
        eprintln!(
            "warning: failed to create log directory {}; tracing disabled: {err}",
            directory.display()
        );
        return None;
    }

    let rotation_kind = match rotation {
        LogRotation::Minutely => Rotation::MINUTELY,
        LogRotation::Hourly => Rotation::HOURLY,
        LogRotation::Daily => Rotation::DAILY,
        LogRotation::Never => Rotation::NEVER,
    };

    // Use the fallible builder (not `RollingFileAppender::new`, which panics) so
    // an init failure only disables logging, never crashes the CLI. We do NOT
    // enable `max_log_files` pruning: it deletes by filename prefix and would
    // risk removing unrelated `<file>.*` files in the log directory.
    match RollingFileAppender::builder()
        .rotation(rotation_kind)
        .filename_prefix(file_name)
        .build(&directory)
    {
        Ok(appender) => {
            use tracing_subscriber::layer::Layer;
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(appender)
                .with_filter(env_filter)
                .with_filter(not_telemetry);
            Some(Box::new(layer))
        }
        Err(err) => {
            eprintln!(
                "warning: failed to open rolling LIBRA_LOG_FILE {}; tracing disabled: {err}",
                path.display()
            );
            None
        }
    }
}

/// Build the [`EnvFilter`] that drives the global tracing subscriber.
///
/// Functional scope:
/// - Parses `directives` (the resolved value of `LIBRA_LOG`/`RUST_LOG`/the
///   `libra=debug` fallback) and, when the user did not say anything about
///   the `rfuse3` target, pins `rfuse3::raw::session=error` so the spammy
///   `"The data is not 4096 bytes aligned"` warning that fires for every
///   sub-page write to the worktree FUSE mount stays out of normal logs.
///
/// Boundary conditions:
/// - If the user opts in by mentioning `rfuse3` anywhere in their filter
///   string (e.g. `LIBRA_LOG=rfuse3=warn`), we skip the suppression so the
///   user's directive wins outright.
/// - The added directive is a static literal whose parse cannot fail in any
///   supported `tracing-subscriber` version; the `expect` is a hard
///   invariant, not a runtime fallback.
fn build_env_filter(directives: &str) -> EnvFilter {
    let env_filter = EnvFilter::new(directives);
    if directives.contains("rfuse3") {
        return env_filter;
    }
    env_filter.add_directive(
        "rfuse3::raw::session=error"
            .parse()
            .expect("static rfuse3 directive must parse"),
    )
}
